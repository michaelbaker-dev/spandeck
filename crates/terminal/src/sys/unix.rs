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

    /// Signal the pane's whole process group. Kill reaches further: every
    /// process in the pane's session, so a job that a shell put in a group
    /// of its own (`sleep 300 &`) ends with the pane.
    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        let kill = matches!(sig, Signal::Kill);
        let sig = match sig {
            Signal::Hangup => nix::sys::signal::Signal::SIGHUP,
            Signal::Terminate => nix::sys::signal::Signal::SIGTERM,
            Signal::Kill => nix::sys::signal::Signal::SIGKILL,
        };
        let sent = killpg(Pid::from_raw(self.pid), sig).map_err(io::Error::from);
        if kill {
            for pid in self.in_this_session() {
                let _ = nix::sys::signal::kill(Pid::from_raw(pid), sig);
            }
        }
        sent
    }

    /// Every process in the pane's session. The pane's program started the
    /// session (`setsid` in `spawn`), so the session's id is its pid, and it
    /// stays that after the program itself has ended. The terminal cannot be
    /// asked instead: a session whose first program ended has none.
    fn in_this_session(&self) -> Vec<i32> {
        let Ok(out) = Command::new("ps").args(["-axo", "pid="]).output() else {
            return vec![];
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<i32>().ok())
            // SAFETY: getsid only reads; a pid that is gone gives -1.
            .filter(|pid| *pid > 1 && unsafe { libc::getsid(*pid) } == self.pid)
            .collect()
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

/// A process, as `processes` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: i32,
    /// The session it is in. A pane's program starts a session, so this is
    /// that program's pid for everything the pane started.
    pub sid: i32,
    /// How long ago it started, in seconds.
    pub age_secs: u64,
    /// Its program's name, without the folder.
    pub name: String,
}

/// Every process on this machine with its session and its age, from one run
/// of `ps` (the same on macOS and Linux). Empty when `ps` cannot be run.
pub fn processes() -> Vec<Proc> {
    let Ok(out) = Command::new("ps")
        .args(["-axo", "pid=,etime=,comm="])
        .output()
    else {
        return vec![];
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse::<i32>().ok().filter(|p| *p > 1)?;
            let age_secs = elapsed_secs(parts.next()?)?;
            let comm = parts.collect::<Vec<_>>().join(" ");
            // SAFETY: getsid only reads; a pid that is gone gives -1.
            let sid = unsafe { libc::getsid(pid) };
            (sid > 1).then(|| Proc {
                pid,
                sid,
                age_secs,
                name: comm.rsplit('/').next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// `ps`'s elapsed time, `[[days-]hours:]minutes:seconds`, as seconds.
fn elapsed_secs(etime: &str) -> Option<u64> {
    let (days, clock) = match etime.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, etime),
    };
    let mut secs = 0u64;
    for part in clock.split(':') {
        secs = secs * 60 + part.parse::<u64>().ok()?;
    }
    Some(days * 86_400 + secs)
}

/// End these processes now. For what a pane's program left running when the
/// daemon that held the pane ended: they are in no pane any more and are not
/// this process's children, so nothing else would end them.
pub fn end_processes(pids: &[i32]) {
    for pid in pids.iter().filter(|p| **p > 1) {
        let _ = nix::sys::signal::kill(Pid::from_raw(*pid), nix::sys::signal::Signal::SIGKILL);
    }
}

/// What one pane's programs have used and hold now (`tree_usage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TreeUse {
    /// Processor time used so far by the programs that are alive, in
    /// milliseconds. A program that ended takes its time with it.
    pub cpu_ms: u64,
    /// Resident memory, each program's added up (what two of them share
    /// is counted twice).
    pub rss_bytes: u64,
    /// How many programs were counted: 0 when the pane's program is gone.
    pub count: u32,
}

/// One process, as `tree_usage` reads it.
struct ProcNow {
    pid: i32,
    ppid: i32,
    sid: i32,
    cpu_ms: u64,
    rss_bytes: u64,
}

/// For each pane's program (`roots`): what it and everything it started
/// are using. A pane's program leads a session, so what it started is
/// every process of that session, and every process that descends from
/// it by its parents (one that began a session of its own). One pass over
/// the system's processes, whatever the number of panes. `None` where
/// this system has no sound way to read it: nothing is then shown.
pub fn tree_usage(roots: &[i32]) -> Option<Vec<TreeUse>> {
    Some(sum_trees(&all_procs()?, roots))
}

fn sum_trees(procs: &[ProcNow], roots: &[i32]) -> Vec<TreeUse> {
    let parent: std::collections::HashMap<i32, i32> =
        procs.iter().map(|p| (p.pid, p.ppid)).collect();
    let place: std::collections::HashMap<i32, usize> = roots
        .iter()
        .enumerate()
        .filter(|(_, pid)| **pid > 1)
        .map(|(i, pid)| (*pid, i))
        .collect();
    let mut out = vec![TreeUse::default(); roots.len()];
    for p in procs {
        let mut at = place.get(&p.sid).copied();
        // Up through its parents, a bounded way: a chain is never this long.
        let (mut pid, mut steps) = (p.pid, 0);
        while at.is_none() && pid > 1 && steps < 64 {
            at = place.get(&pid).copied();
            pid = parent.get(&pid).copied().unwrap_or(0);
            steps += 1;
        }
        if let Some(i) = at {
            out[i].cpu_ms += p.cpu_ms;
            out[i].rss_bytes += p.rss_bytes;
            out[i].count += 1;
        }
    }
    out
}

/// Linux: every process's `/proc/<pid>/stat`, one small read each.
#[cfg(target_os = "linux")]
fn all_procs() -> Option<Vec<ProcNow>> {
    // SAFETY: sysconf only reads.
    let (ticks, page) = unsafe {
        (
            libc::sysconf(libc::_SC_CLK_TCK),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    if ticks <= 0 || page <= 0 {
        return None;
    }
    let mut out = vec![];
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        // A process that ended meanwhile is simply not there.
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        if let Some(p) = parse_stat(pid, &stat, ticks as u64, page as u64) {
            out.push(p);
        }
    }
    Some(out)
}

/// One line of `/proc/<pid>/stat` (proc(5)). The program's name is in
/// brackets and may hold spaces and brackets, so the fields are counted
/// from the last ")": state, parent, group, session, ..., user and system
/// time in clock ticks (fields 14 and 15), ..., resident pages (field 24).
#[cfg(target_os = "linux")]
fn parse_stat(pid: i32, stat: &str, ticks: u64, page: u64) -> Option<ProcNow> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_ascii_whitespace().collect();
    let n = |i: usize| f.get(i)?.parse::<u64>().ok();
    Some(ProcNow {
        pid,
        ppid: f.get(1)?.parse().ok()?,
        sid: f.get(3)?.parse().ok()?,
        cpu_ms: (n(11)? + n(12)?) * 1000 / ticks,
        rss_bytes: f.get(21)?.parse::<i64>().ok()?.max(0) as u64 * page,
    })
}

/// macOS: libproc, one call for each process. A process of another user
/// is refused and left out, which is right: a pane's programs are the
/// owner's. NOT COMPILED OR RUN where this was written (a Linux machine):
/// it uses only what the `libc` crate declares for macOS.
#[cfg(target_os = "macos")]
fn all_procs() -> Option<Vec<ProcNow>> {
    use std::mem::size_of;
    // Processor times are in the machine's own clock units (not
    // nanoseconds on Apple silicon): the time base converts them.
    #[allow(deprecated)]
    let (numer, denom) = {
        // SAFETY: two plain numbers, for which all zeroes is a value.
        let mut base: libc::mach_timebase_info = unsafe { std::mem::zeroed() };
        // SAFETY: a valid pointer to the struct it fills.
        if unsafe { libc::mach_timebase_info(&mut base) } != 0 || base.denom == 0 {
            return None;
        }
        (base.numer as u128, base.denom as u128)
    };
    // SAFETY: with no buffer it only says how many there are.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return None;
    }
    let mut pids: Vec<libc::pid_t> = vec![0; n as usize + 64];
    // SAFETY: the buffer is `pids`, and its size in bytes is given.
    let n = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr() as *mut libc::c_void,
            (pids.len() * size_of::<libc::pid_t>()) as libc::c_int,
        )
    };
    if n <= 0 {
        return None;
    }
    pids.truncate((n as usize).min(pids.len()));
    let mut out = vec![];
    for pid in pids.into_iter().filter(|p| *p > 1) {
        // SAFETY: plain data, for which all zeroes is a value.
        let mut info: libc::proc_taskallinfo = unsafe { std::mem::zeroed() };
        let size = size_of::<libc::proc_taskallinfo>() as libc::c_int;
        // SAFETY: the buffer is `info`, and its size is given.
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKALLINFO,
                0,
                &mut info as *mut libc::proc_taskallinfo as *mut libc::c_void,
                size,
            )
        };
        if got != size {
            continue;
        }
        let units = info.ptinfo.pti_total_user as u128 + info.ptinfo.pti_total_system as u128;
        out.push(ProcNow {
            pid,
            ppid: info.pbsd.pbi_ppid as i32,
            // SAFETY: getsid only reads; a pid that is gone gives -1.
            sid: unsafe { libc::getsid(pid) },
            cpu_ms: (units * numer / denom / 1_000_000) as u64,
            rss_bytes: info.ptinfo.pti_resident_size,
        });
    }
    Some(out)
}

/// Any other Unix: not read, so nothing is shown.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn all_procs() -> Option<Vec<ProcNow>> {
    None
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

/// Resolves when the daemon is asked to stop (SIGTERM, SIGINT). Which
/// station it is matters only on Windows, where the request has a name.
pub async fn shutdown_signal(_station: &Path) -> io::Result<()> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    Ok(())
}

/// Windows only: there the daemon cannot be sent a signal. Here the login
/// service stops it (`systemctl --user stop`, `launchctl bootout`).
pub fn request_stop(_station: &Path) -> io::Result<bool> {
    Ok(false)
}

/// Nothing to record: an upgrade stays in one process, so the pid file
/// never names another.
pub fn record_successor_in(_pid_file: &Path) {}

// ------------------------------------------------ supervising the station
//
// Windows only (DECISIONS.md, "Windows"). Here systemd and launchd restart
// a daemon that failed, and follow it through an upgrade because it stays
// the same process; `spandeckd` is not installed. These exist so the code
// that uses them is compiled and checked on every system.

/// Never made here.
pub struct Supervisor(());

/// What `Supervisor::watch` saw.
#[derive(Debug, PartialEq, Eq)]
pub enum Watched {
    Ended(i32),
    Gone,
    Leave,
}

/// Not here: the login service supervises.
pub fn supervise(_station: &Path, _take_over: Duration) -> Option<Supervisor> {
    None
}

/// Not by a supervisor of ours.
pub fn supervised(_station: &Path) -> bool {
    false
}

impl Supervisor {
    pub fn watch(&self, _pid: u32) -> Watched {
        Watched::Gone
    }

    pub fn leave_within(&self, _wait: Duration) -> bool {
        true
    }
}

/// Not asked for here.
pub fn process_started(_pid: u32) -> Option<std::time::SystemTime> {
    None
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

/// The command that holds the machine awake while it runs. It ends by itself
/// when this daemon is gone, so one that the daemon could not end (a crash)
/// does not hold the machine awake for ever.
pub fn keep_awake_cmd(_exe: &Path) -> Vec<String> {
    let pid = std::process::id().to_string();
    let v: Vec<&str> = if cfg!(target_os = "macos") {
        vec!["caffeinate", "-i", "-w", &pid]
    } else {
        vec![
            "systemd-inhibit",
            "--what=sleep:idle",
            "--who=spandeck",
            "--why=agents are working",
            "sh",
            "-c",
            "while kill -0 \"$0\" 2>/dev/null; do sleep 5; done",
            &pid,
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

// ------------------------------------------------------- who, and whose file

/// The name of the user this process runs as, from the system's own list,
/// never from the environment (a pane's program can set `USER`).
pub fn user_name() -> Option<String> {
    nix::unistd::User::from_uid(nix::unistd::getuid())
        .ok()
        .flatten()
        .map(|u| u.name)
}

/// Is this a program of the system's: a file owned by root that nobody but
/// root may write? Asked before something is run with an administrator's
/// right, since an agent on this machine runs as the owner's user and could
/// put a program of its own under a name that is looked for.
pub fn system_program(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.uid() == 0 && m.mode() & 0o022 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: i32, ppid: i32, sid: i32, cpu_ms: u64, rss_bytes: u64) -> ProcNow {
        ProcNow {
            pid,
            ppid,
            sid,
            cpu_ms,
            rss_bytes,
        }
    }

    /// The reason: "CPU and memory of a pane's programs (the process tree
    /// under the pane's PTY)". What a pane started is counted for that pane
    /// and no other: everything in its session, a program that began a
    /// session of its own under it, and nothing of the station's own or of
    /// the pane beside it.
    #[test]
    fn a_panes_programs_are_its_session_and_what_descends_from_it() {
        let procs = [
            p(50, 1, 50, 900, 9000),    // the station itself
            p(100, 50, 100, 10, 1000),  // pane A's shell: leads a session
            p(101, 100, 100, 20, 2000), // what it runs
            p(102, 1, 100, 5, 500),     // left in the background: its parent is gone
            p(110, 101, 110, 40, 4000), // began a session of its own
            p(111, 110, 110, 1, 100),   // and its child
            p(200, 50, 200, 7, 700),    // pane B's shell
            p(300, 1, 300, 99, 9900),   // somebody else's
        ];
        let used = sum_trees(&procs, &[100, 200, 999, 0]);
        assert_eq!(
            used[0],
            TreeUse {
                cpu_ms: 76,
                rss_bytes: 7600,
                count: 5
            }
        );
        assert_eq!(
            used[1],
            TreeUse {
                cpu_ms: 7,
                rss_bytes: 700,
                count: 1
            }
        );
        // A program that is gone, and a pane with none: nothing, and no
        // other process is taken for theirs.
        assert_eq!((used[2], used[3]), Default::default());
    }

    /// The reason: a program may be named anything, brackets and spaces
    /// included, and its name is in the middle of the line that is read.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_line_is_read_whatever_the_program_is_called() {
        let line = "4242 (tmux: a) b (c) R 100 4242 77 34816 4242 4194304 1 0 0 0 250 50 0 0 20 0 3 0 100 1000000 300 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";
        let read = parse_stat(4242, line, 100, 4096).unwrap();
        assert_eq!(
            (read.ppid, read.sid, read.cpu_ms, read.rss_bytes),
            (100, 77, 3000, 300 * 4096)
        );
        assert!(parse_stat(1, "1 (short", 100, 4096).is_none());
    }
}
