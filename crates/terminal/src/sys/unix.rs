//! macOS and Linux.
//!
//! The PTY master is a plain fd so it can survive an exec-in-place upgrade:
//! the new daemon image is the same process, still the parent of every pane's
//! child, and adopts the descriptors by number.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use nix::fcntl::{fcntl, FcntlArg, FdFlag};
use nix::pty::{openpty, Winsize};
use nix::sys::signal::killpg;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;

use crate::pty::{ExitInfo, Signal, SpawnSpec};

/// A terminal in this process survives an upgrade: the daemon's program is
/// replaced in place (exec) and the descriptors stay open.
pub const LOCAL_HANDOFF: bool = true;

/// Panes live in the daemon's own process. (Set `SPANDECK_PANE_HOST=1` to
/// try the pane host on Unix; Windows always uses it.)
pub const PANE_HOST: bool = false;

// ---------------------------------------------------------------- terminals

#[derive(Debug)]
pub struct Pty {
    master: OwnedFd,
    pid: i32,
}

fn winsize(cols: u16, rows: u16) -> Winsize {
    Winsize {
        ws_row: rows.max(1),
        ws_col: cols.max(2),
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

nix::ioctl_write_ptr_bad!(tiocswinsz, libc::TIOCSWINSZ, Winsize);

fn set_cloexec(fd: BorrowedFd<'_>, on: bool) -> io::Result<()> {
    let flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD)?);
    let flags = if on {
        flags | FdFlag::FD_CLOEXEC
    } else {
        flags & !FdFlag::FD_CLOEXEC
    };
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
        for k in &spec.env_remove {
            cmd.env_remove(k);
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
        Ok(Pty {
            master: pair.master,
            pid,
        })
    }

    /// Adopt a PTY that this process already owns (after an exec-in-place upgrade).
    ///
    /// # Safety
    /// `fd` must be an open PTY master owned by nobody else.
    pub unsafe fn adopt(fd: i32, pid: i32) -> io::Result<Pty> {
        let master = OwnedFd::from_raw_fd(fd as RawFd);
        // Fails with EBADF if the descriptor did not survive.
        set_cloexec(master.as_fd(), true)?;
        Ok(Pty { master, pid })
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    fn try_clone_master(&self) -> io::Result<OwnedFd> {
        let fd = self.master.try_clone()?;
        set_cloexec(fd.as_fd(), true)?;
        Ok(fd)
    }

    /// The end the pane's output is read from.
    pub fn reader(&self) -> io::Result<PtyReader> {
        Ok(PtyReader {
            file: File::from(self.try_clone_master()?),
            pid: self.pid,
        })
    }

    /// The end keystrokes are written to.
    pub fn writer(&self) -> io::Result<PtyWriter> {
        Ok(PtyWriter(File::from(self.try_clone_master()?)))
    }

    /// A descriptor for this terminal that survives exec, for the next
    /// process image to adopt by number.
    pub fn handoff_fd(&self) -> io::Result<i32> {
        // `dup` gives a descriptor without CLOEXEC: exactly what must cross exec.
        let fd = nix::unistd::dup(self.master.as_fd())?;
        Ok(fd.into_raw_fd())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let ws = winsize(cols, rows);
        // SAFETY: valid fd and a valid Winsize pointer.
        unsafe { tiocswinsz(self.master.as_raw_fd(), &ws) }?;
        Ok(())
    }

    /// Signal the pane's whole process group.
    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        let sig = match sig {
            Signal::Hangup => nix::sys::signal::Signal::SIGHUP,
            Signal::Terminate => nix::sys::signal::Signal::SIGTERM,
            Signal::Kill => nix::sys::signal::Signal::SIGKILL,
        };
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

pub struct PtyReader {
    file: File,
    pid: i32,
}

impl PtyReader {
    /// Wait a moment for output. `false`: none yet, ask again. The pause is
    /// what lets a pane's reader notice it has been asked to stop.
    pub fn ready(&mut self) -> io::Result<bool> {
        let mut pfd = [nix::poll::PollFd::new(
            self.file.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        match nix::poll::poll(&mut pfd, 200u16) {
            Ok(0) => Ok(false),
            Err(nix::errno::Errno::EINTR) => Ok(false),
            Err(e) => Err(e.into()),
            Ok(_) => Ok(true),
        }
    }

    /// Read output. `Ok(0)` or an error: every program on the terminal has
    /// closed it (EIO on Unix), and the pane is done.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }

    /// After the terminal closed: how the pane's program ended.
    pub fn exit_status(&self) -> ExitInfo {
        // The PTY closed, so the child is exiting; give it a moment to be reapable.
        for _ in 0..200 {
            match waitpid(Pid::from_raw(self.pid), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(_, code)) => return ExitInfo::Code(code),
                Ok(WaitStatus::Signaled(_, sig, _)) => return ExitInfo::Signal(sig as i32),
                Ok(WaitStatus::StillAlive) => std::thread::sleep(Duration::from_millis(10)),
                Ok(_) => {}
                Err(nix::errno::Errno::EINTR) => {}
                Err(_) => return ExitInfo::Unknown,
            }
        }
        ExitInfo::Unknown
    }
}

pub struct PtyWriter(File);

impl Write for PtyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn wait_pid(pid: i32, block: bool) -> Option<ExitInfo> {
    let flags = if block {
        None
    } else {
        Some(WaitPidFlag::WNOHANG)
    };
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

// ------------------------------------------------------------- local socket

/// The socket the CLI, hooks and `spandeck mcp` reach the daemon through: a
/// Unix socket only its owner can open.
pub mod local {
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    /// One accepted connection. Split it with `tokio::io::split`.
    pub type Stream = tokio::net::UnixStream;
    /// A blocking connection, for the CLI and hooks.
    pub type Conn = std::os::unix::net::UnixStream;

    pub struct Listener(tokio::net::UnixListener);

    impl Listener {
        /// Listen at `path`, replacing whatever a previous run left there.
        pub fn bind(path: &Path) -> io::Result<Listener> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // A stale socket file from a previous run (or the previous image).
            let _ = std::fs::remove_file(path);
            let listener = tokio::net::UnixListener::bind(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Listener(listener))
        }

        pub async fn accept(&mut self) -> io::Result<Stream> {
            self.0.accept().await.map(|(stream, _)| stream)
        }
    }

    pub fn connect(path: &Path) -> io::Result<Conn> {
        Conn::connect(path)
    }

    /// Where the socket is when the system keeps such things outside the
    /// file system. Unix sockets are files: there is no such place.
    pub fn pipe_name(_tag: &str) -> Option<PathBuf> {
        None
    }
}

// -------------------------------------------------------------------- files

/// Write a file only this user can read (a token, a key).
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents)?;
    f.sync_all()
}

/// Make a folder only this user can enter.
pub fn private_dir(path: &Path) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// Is this a file the system would run?
pub fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Mark a file as a program (tests write small scripts and run them).
pub fn set_executable(path: &Path) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

/// A file's permission bits, where the system has them.
pub fn file_mode(path: &Path) -> Option<u32> {
    std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

/// The file names a command name can stand for. On Unix, itself.
pub fn executable_names(name: &str) -> Vec<String> {
    vec![name.to_string()]
}

/// Where state and config are kept when the environment does not say.
pub fn default_dirs(home: &Path) -> (PathBuf, PathBuf) {
    (
        home.join(".local/state/spandeck"),
        home.join(".config/spandeck"),
    )
}

/// Put `new` where `to` is, although `to` may be a program that is running.
pub fn replace_file(new: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(new, to)
}

// ---------------------------------------------------------------- processes

/// Become `cmd`: same process, same open terminals. Returns only if that
/// could not be done.
pub fn replace_process(cmd: &mut Command) -> io::Error {
    cmd.exec()
}

/// Start a program that goes on after this one ends, with no terminal.
pub fn spawn_detached(cmd: &mut Command) -> io::Result<()> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    // Collected when it ends, so it does not stay a zombie of this process.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The upgraded daemon is the same process (exec in place).
pub const UPGRADE_KEEPS_PID: bool = true;

/// End a pane's program left over from an upgrade that did not complete, if
/// it is still this process's child.
pub fn end_leftover_child(pid: i32) {
    let ours = matches!(
        waitpid(Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)),
        Ok(WaitStatus::StillAlive)
    );
    if ours {
        let _ = killpg(Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL);
        let _ = waitpid(Pid::from_raw(pid), None);
    }
}

/// Close descriptors inherited from a previous image whose handoff is not
/// being adopted, so orphaned terminals do not stay half-open. Call before
/// this process opens anything.
pub fn close_inherited() {
    for fd in 3..1024 {
        // SAFETY: nothing has opened a descriptor yet in this process image.
        unsafe { libc::close(fd) };
    }
}

/// Wait for another process to end. Never needed on Unix, where an upgrade
/// stays in one process.
pub fn wait_for_exit(_pid: u32, _limit: Duration) {}

/// Resolves when the daemon is asked to stop (SIGTERM, SIGINT).
pub async fn shutdown_signal() -> io::Result<()> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    Ok(())
}

/// Start `cmd` without opening a console window. Only Windows opens one.
pub fn no_window(_cmd: &mut Command) {}

/// Let Ctrl-C reach the programs this one starts without ending this one.
/// On Unix the shell script that needs this uses `trap`; nothing calls it.
pub fn ignore_interrupt() {}

/// Was Ctrl-C pressed since `ignore_interrupt`? Never, on Unix.
pub fn interrupted() -> bool {
    false
}

// -------------------------------------------------------------- the machine

pub fn hostname() -> Option<String> {
    nix::unistd::gethostname()
        .ok()
        .and_then(|h| h.into_string().ok())
}

pub fn load_average() -> Option<f64> {
    let mut avg = [0f64; 1];
    // SAFETY: valid pointer to one f64.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 1) };
    (n == 1).then_some(avg[0])
}

/// Fill `buf` from the system's random source.
pub fn random_bytes(buf: &mut [u8]) -> io::Result<()> {
    File::open("/dev/urandom")?.read_exact(buf)
}

/// The command that holds the machine awake while it runs.
pub fn keep_awake_cmd(_exe: &Path) -> Vec<String> {
    let v: &[&str] = if cfg!(target_os = "macos") {
        &["caffeinate", "-i"]
    } else {
        &[
            "systemd-inhibit",
            "--what=sleep:idle",
            "--who=spandeck",
            "--why=agents are working",
            "sleep",
            "infinity",
        ]
    };
    v.iter().map(|s| s.to_string()).collect()
}

/// Hold the machine awake until the process `while_pid` ends. On Unix the
/// system's own tools do this (`keep_awake_cmd`).
pub fn hold_awake(_while_pid: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this system keeps awake with caffeinate or systemd-inhibit",
    ))
}

// ------------------------------------------------------------------- shells

/// The shell a plain pane runs.
pub fn default_shell() -> (String, Vec<String>) {
    (
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
        vec![],
    )
}

/// The command that runs one shell line.
pub fn shell_line(line: &str) -> Vec<String> {
    vec!["sh".to_string(), "-c".to_string(), line.to_string()]
}

// ---------------------------------------------------------------- a console

/// Standard input as it was before `stdin_raw`; restores it when dropped.
pub struct RawInput(Option<libc::termios>);

/// Read standard input a key at a time, with no echo and Ctrl-C as a byte.
pub fn stdin_raw() -> RawInput {
    // SAFETY: plain termios calls on fd 0 with a valid pointer.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) != 0 {
            return RawInput(None);
        }
        let saved = t;
        t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        libc::tcsetattr(0, libc::TCSANOW, &t);
        RawInput(Some(saved))
    }
}

impl RawInput {
    pub fn restore(&mut self) {
        if let Some(t) = self.0.take() {
            // SAFETY: restoring settings read from the same descriptor.
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
        }
    }
}

impl Drop for RawInput {
    fn drop(&mut self) {
        self.restore();
    }
}
