//! PTY spawn and control (macOS and Linux).
//!
//! The master descriptor is a plain fd so it can survive an exec-in-place
//! upgrade: the new daemon image is the same process, still the parent of every
//! pane's child, and adopts the descriptors by number.

use std::ffi::OsString;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use nix::fcntl::{fcntl, FcntlArg, FdFlag};
use nix::pty::{openpty, Winsize};
use nix::sys::signal::{killpg, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpawnSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Added to the daemon's environment.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

impl SpawnSpec {
    pub fn shell(cols: u16, rows: u16) -> Self {
        let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        SpawnSpec { program, args: vec![], cwd: None, env: vec![], cols, rows }
    }
}

/// How a pane's process ended.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExitInfo {
    Code(i32),
    Signal(i32),
    /// The process is gone but its status could not be collected.
    Unknown,
}

#[derive(Debug)]
pub struct Pty {
    master: OwnedFd,
    pid: i32,
}

fn winsize(cols: u16, rows: u16) -> Winsize {
    Winsize { ws_row: rows.max(1), ws_col: cols.max(2), ws_xpixel: 0, ws_ypixel: 0 }
}

nix::ioctl_write_ptr_bad!(tiocswinsz, libc::TIOCSWINSZ, Winsize);

fn set_cloexec(fd: BorrowedFd<'_>, on: bool) -> io::Result<()> {
    let flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD)?);
    let flags = if on { flags | FdFlag::FD_CLOEXEC } else { flags & !FdFlag::FD_CLOEXEC };
    fcntl(fd, FcntlArg::F_SETFD(flags))?;
    Ok(())
}

impl Pty {
    pub fn spawn(spec: &SpawnSpec) -> io::Result<Pty> {
        let ws = winsize(spec.cols, spec.rows);
        let pair = openpty(Some(&ws), None)?;
        set_cloexec(pair.master.as_fd(), true)?;

        let slave_fd = pair.slave.as_raw_fd();
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in &spec.env {
            cmd.env(OsString::from(k), OsString::from(v));
        }
        cmd.stdin(Stdio::from(pair.slave.try_clone()?));
        cmd.stdout(Stdio::from(pair.slave.try_clone()?));
        cmd.stderr(Stdio::from(pair.slave.try_clone()?));
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                // fd 0 is the slave by now; make it the controlling terminal.
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                let _ = slave_fd;
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        let pid = child.id() as i32;
        // We reap by pid ourselves (it must keep working after an exec-in-place
        // upgrade, when no `Child` value exists any more).
        drop(child);
        drop(pair.slave);
        Ok(Pty { master: pair.master, pid })
    }

    /// Adopt a PTY that this process already owns (after an exec-in-place upgrade).
    ///
    /// # Safety
    /// `fd` must be an open PTY master owned by nobody else.
    pub unsafe fn adopt(fd: RawFd, pid: i32) -> io::Result<Pty> {
        let master = OwnedFd::from_raw_fd(fd);
        // Fails with EBADF if the descriptor did not survive.
        set_cloexec(master.as_fd(), true)?;
        Ok(Pty { master, pid })
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn master_fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    pub fn try_clone_master(&self) -> io::Result<OwnedFd> {
        let fd = self.master.try_clone()?;
        set_cloexec(fd.as_fd(), true)?;
        Ok(fd)
    }

    /// Let the master survive an exec (upgrade handoff) or not.
    pub fn set_inheritable(&self, inheritable: bool) -> io::Result<()> {
        set_cloexec(self.master.as_fd(), !inheritable)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let ws = winsize(cols, rows);
        // SAFETY: valid fd and a valid Winsize pointer.
        unsafe { tiocswinsz(self.master.as_raw_fd(), &ws) }?;
        Ok(())
    }

    /// Signal the pane's whole process group.
    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        killpg(Pid::from_raw(self.pid), sig).map_err(io::Error::from)
    }

    /// Non-blocking: has the child exited?
    pub fn try_wait(&self) -> Option<ExitInfo> {
        wait_pid(self.pid, false)
    }

    /// Blocking wait for the child.
    pub fn wait(&self) -> ExitInfo {
        wait_pid(self.pid, true).unwrap_or(ExitInfo::Unknown)
    }
}

fn wait_pid(pid: i32, block: bool) -> Option<ExitInfo> {
    let flags = if block { None } else { Some(WaitPidFlag::WNOHANG) };
    loop {
        return match waitpid(Pid::from_raw(pid), flags) {
            Ok(WaitStatus::Exited(_, code)) => Some(ExitInfo::Code(code)),
            Ok(WaitStatus::Signaled(_, sig, _)) => Some(ExitInfo::Signal(sig as i32)),
            Ok(WaitStatus::StillAlive) => None,
            Ok(_) => continue,
            Err(nix::errno::Errno::EINTR) => continue,
            // Not our child (or already reaped): gone, status unknown.
            Err(_) => Some(ExitInfo::Unknown),
        };
    }
}

/// Is a process with this pid still alive?
pub fn pid_alive(pid: i32) -> bool {
    nix::sys::signal::kill(Pid::from_raw(pid), None).is_ok()
}
