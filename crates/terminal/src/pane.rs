//! A running pane: a PTY, its server-side screen, and one reader thread.
//!
//! Backpressure rule: the reader thread always drains the PTY and never waits
//! for a client. The sink it calls must not block; slow clients are the
//! caller's problem (they are dropped and resynced from a snapshot).

use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use nix::sys::signal::Signal;

use crate::pty::{ExitInfo, Pty, SpawnSpec};
use crate::screen::{Screen, ScreenEvent, ScreenInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvent {
    Output(Arc<[u8]>),
    Title(String),
    Bell,
    Exited(ExitInfo),
}

pub type Sink = Box<dyn FnMut(PaneEvent) + Send + 'static>;

/// State needed to rebuild a pane in a new process image after an upgrade.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaneHandoff {
    pub master_fd: i32,
    pub pid: i32,
    pub cols: u16,
    pub rows: u16,
    /// `Screen::snapshot_ansi` output, replayed into the new screen.
    pub snapshot: Vec<u8>,
}

pub struct Pane {
    pty: Pty,
    screen: Arc<Mutex<Screen>>,
    writer: Mutex<File>,
    exited: Arc<Mutex<Option<ExitInfo>>>,
    /// Set to stop the reader thread without closing the PTY (handoff).
    detach: Arc<AtomicBool>,
    bytes_out: Arc<AtomicU64>,
    last_output_ms: Arc<AtomicU64>,
    reader: Mutex<Option<thread::JoinHandle<()>>>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Pane {
    pub fn spawn(spec: &SpawnSpec, sink: Sink) -> io::Result<Pane> {
        let pty = Pty::spawn(spec)?;
        let screen = Screen::new(spec.cols, spec.rows);
        Pane::start(pty, screen, sink)
    }

    /// Rebuild a pane from handoff state inside the new process image.
    ///
    /// # Safety
    /// `h.master_fd` must be an inherited PTY master owned by nobody else.
    pub unsafe fn adopt(h: &PaneHandoff, sink: Sink) -> io::Result<Pane> {
        let pty = Pty::adopt(h.master_fd, h.pid)?;
        let mut screen = Screen::new(h.cols, h.rows);
        screen.feed(&h.snapshot);
        Pane::start(pty, screen, sink)
    }

    fn start(pty: Pty, screen: Screen, mut sink: Sink) -> io::Result<Pane> {
        let screen = Arc::new(Mutex::new(screen));
        let exited = Arc::new(Mutex::new(None));
        let detach = Arc::new(AtomicBool::new(false));
        let bytes_out = Arc::new(AtomicU64::new(0));
        let last_output_ms = Arc::new(AtomicU64::new(now_ms()));
        let mut reader = File::from(pty.try_clone_master()?);
        let writer = File::from(pty.try_clone_master()?);
        let mut reply_writer = File::from(pty.try_clone_master()?);
        let pid = pty.pid();

        let handle = {
            let screen = screen.clone();
            let exited = exited.clone();
            let detach = detach.clone();
            let bytes_out = bytes_out.clone();
            let last_output_ms = last_output_ms.clone();
            thread::Builder::new().name(format!("pane-{pid}")).spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    // Poll so a detach request is noticed without output arriving.
                    let mut pfd = [nix::poll::PollFd::new(
                        std::os::fd::AsFd::as_fd(&reader),
                        nix::poll::PollFlags::POLLIN,
                    )];
                    match nix::poll::poll(&mut pfd, 200u16) {
                        Ok(0) => {
                            if detach.load(Ordering::SeqCst) {
                                return;
                            }
                            continue;
                        }
                        Err(nix::errno::Errno::EINTR) => continue,
                        Err(_) => break,
                        Ok(_) => {}
                    }
                    if detach.load(Ordering::SeqCst) {
                        return;
                    }
                    let n = match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        // EIO: every slave descriptor is closed, the pane is done.
                        Err(_) => break,
                    };
                    let chunk = &buf[..n];
                    // Feed and publish under one lock: a late joiner that holds the
                    // screen lock gets a snapshot and a stream cut at the same point.
                    let events = {
                        let mut guard = screen.lock().unwrap();
                        let events = guard.feed(chunk);
                        sink(PaneEvent::Output(Arc::from(chunk)));
                        events
                    };
                    bytes_out.fetch_add(n as u64, Ordering::Relaxed);
                    last_output_ms.store(now_ms(), Ordering::Relaxed);
                    for ev in events {
                        match ev {
                            ScreenEvent::Reply(bytes) => {
                                let _ = reply_writer.write_all(&bytes);
                            }
                            ScreenEvent::Title(t) => sink(PaneEvent::Title(t)),
                            ScreenEvent::Bell => sink(PaneEvent::Bell),
                        }
                    }
                }
                let info = wait_exit(pid);
                *exited.lock().unwrap() = Some(info);
                sink(PaneEvent::Exited(info));
            })?
        };

        Ok(Pane {
            pty,
            screen,
            writer: Mutex::new(writer),
            exited,
            detach,
            bytes_out,
            last_output_ms,
            reader: Mutex::new(Some(handle)),
        })
    }

    pub fn pid(&self) -> i32 {
        self.pty.pid()
    }

    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.writer.lock().unwrap().write_all(bytes)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let mut screen = self.screen.lock().unwrap();
        self.pty.resize(cols, rows)?;
        screen.resize(cols, rows);
        Ok(())
    }

    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        self.pty.signal(sig)
    }

    pub fn exit_info(&self) -> Option<ExitInfo> {
        *self.exited.lock().unwrap()
    }

    pub fn info(&self) -> ScreenInfo {
        self.screen.lock().unwrap().info()
    }

    pub fn text(&self) -> String {
        self.screen.lock().unwrap().text()
    }

    pub fn lines(&self) -> Vec<String> {
        self.screen.lock().unwrap().lines()
    }

    pub fn full_text(&self, max_history: usize) -> Vec<String> {
        self.screen.lock().unwrap().full_text(max_history)
    }

    /// Run `f` with the screen locked. Output cannot interleave while it runs,
    /// which is how a late joiner gets a snapshot and a clean stream cut point.
    pub fn with_screen<R>(&self, f: impl FnOnce(&Screen) -> R) -> R {
        f(&self.screen.lock().unwrap())
    }

    pub fn bytes_out(&self) -> u64 {
        self.bytes_out.load(Ordering::Relaxed)
    }

    pub fn last_output_ms(&self) -> u64 {
        self.last_output_ms.load(Ordering::Relaxed)
    }

    /// Stop the reader and describe this pane for the next process image.
    /// The PTY stays open and is marked to survive exec.
    pub fn prepare_handoff(&self, max_history: usize) -> io::Result<PaneHandoff> {
        self.detach.store(true, Ordering::SeqCst);
        if let Some(h) = self.reader.lock().unwrap().take() {
            let _ = h.join();
        }
        let screen = self.screen.lock().unwrap();
        let fd = {
            let w = self.writer.lock().unwrap();
            nix::unistd::dup(std::os::fd::AsFd::as_fd(&*w))?
        };
        // `dup` gives a descriptor without CLOEXEC: exactly what must cross exec.
        let raw = std::os::fd::IntoRawFd::into_raw_fd(fd);
        Ok(PaneHandoff {
            master_fd: raw,
            pid: self.pty.pid(),
            cols: screen.cols(),
            rows: screen.rows(),
            snapshot: screen.snapshot_ansi(max_history),
        })
    }
}

fn wait_exit(pid: i32) -> ExitInfo {
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
    use nix::unistd::Pid;
    // The PTY closed, so the child is exiting; give it a moment to be reapable.
    for _ in 0..200 {
        match waitpid(Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => return ExitInfo::Code(code),
            Ok(WaitStatus::Signaled(_, sig, _)) => return ExitInfo::Signal(sig as i32),
            Ok(WaitStatus::StillAlive) => thread::sleep(std::time::Duration::from_millis(10)),
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => {}
            Err(_) => return ExitInfo::Unknown,
        }
    }
    ExitInfo::Unknown
}
