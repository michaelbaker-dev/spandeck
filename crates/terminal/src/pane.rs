//! A running pane: a PTY, its server-side screen, and one reader thread.
//!
//! Backpressure rule: the reader thread always drains the PTY and never waits
//! for a client. The sink it calls must not block; slow clients are the
//! caller's problem (they are dropped and resynced from a snapshot).

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::hosted::{self, HostClient};
use crate::pty::{ExitInfo, Pty, PtyReader, Signal, SpawnSpec};
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
    /// Unix: the terminal's descriptor, inherited across exec. -1 for a
    /// pane the pane host holds.
    pub master_fd: i32,
    /// A pane the pane host holds: its id there, and the number of the
    /// first output byte the previous daemon did not take.
    #[serde(default)]
    pub host_id: Option<String>,
    #[serde(default)]
    pub offset: u64,
    pub pid: i32,
    pub cols: u16,
    pub rows: u16,
    /// `Screen::snapshot_ansi` output, replayed into the new screen.
    pub snapshot: Vec<u8>,
}

/// Where a pane's terminal is: in this process, or in the pane host.
enum Terminal {
    Local(Pty),
    Hosted(hosted::Handle),
}

enum Source {
    Local(PtyReader),
    Hosted(hosted::Source),
}

impl Source {
    fn ready(&mut self) -> io::Result<bool> {
        match self {
            Source::Local(r) => r.ready(),
            Source::Hosted(s) => s.ready(),
        }
    }
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Source::Local(r) => r.read(buf),
            Source::Hosted(s) => s.read(buf),
        }
    }
    fn exit_status(&self) -> ExitInfo {
        match self {
            Source::Local(r) => r.exit_status(),
            Source::Hosted(s) => s.exit_status(),
        }
    }
    /// The number of the next output byte (hosted panes only; else 0).
    fn offset(&self) -> u64 {
        match self {
            Source::Local(_) => 0,
            Source::Hosted(s) => s.offset(),
        }
    }
}

type Writer = Box<dyn Write + Send>;

/// How much input may wait for a program that is not reading before more is
/// refused.
const INPUT_WAITING_MAX: usize = 8 << 20;

/// What is typed or pasted into a pane, on its way to the program. A program
/// that is not reading (`sleep 600`, one stopped in a debugger) lets the
/// terminal fill up, and a write then waits for as long as that lasts. That
/// wait used to be taken by whoever sent the input, holding the pane's lock:
/// a 20 KB paste into such a pane stopped the whole station answering, Kill
/// included (found on 15 Nov 2026). So input is handed to a thread of the
/// pane's own, in order, and the sender never waits.
struct Input {
    tx: Mutex<std::sync::mpsc::Sender<Vec<u8>>>,
    waiting: Arc<std::sync::atomic::AtomicUsize>,
}

impl Input {
    fn start(mut writer: Writer) -> io::Result<Input> {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let waiting = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let left = waiting.clone();
        thread::Builder::new()
            .name("pane-input".into())
            .spawn(move || {
                // Ends when the pane is dropped, or its terminal is gone.
                while let Ok(bytes) = rx.recv() {
                    let done = writer.write_all(&bytes);
                    left.fetch_sub(bytes.len(), Ordering::SeqCst);
                    if done.is_err() {
                        break;
                    }
                }
            })?;
        Ok(Input {
            tx: Mutex::new(tx),
            waiting,
        })
    }

    fn send(&self, bytes: &[u8]) -> io::Result<()> {
        if self.waiting.load(Ordering::SeqCst) + bytes.len() > INPUT_WAITING_MAX {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the program in this pane is not reading what is sent to it; nothing more was sent",
            ));
        }
        self.waiting.fetch_add(bytes.len(), Ordering::SeqCst);
        self.tx.lock().unwrap().send(bytes.to_vec()).map_err(|_| {
            io::Error::new(io::ErrorKind::BrokenPipe, "this pane's terminal is closed")
        })
    }
}

pub struct Pane {
    pty: Terminal,
    screen: Arc<Mutex<Screen>>,
    input: Input,
    exited: Arc<Mutex<Option<ExitInfo>>>,
    /// Set to stop the reader thread without closing the PTY (handoff).
    detach: Arc<AtomicBool>,
    bytes_out: Arc<AtomicU64>,
    last_output_ms: Arc<AtomicU64>,
    reader: Mutex<Option<thread::JoinHandle<()>>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Pane {
    pub fn spawn(spec: &SpawnSpec, sink: Sink) -> io::Result<Pane> {
        let pty = Pty::spawn(spec)?;
        let screen = Screen::new(spec.cols, spec.rows);
        let source = Source::Local(pty.reader()?);
        let (writer, replies) = (Box::new(pty.writer()?), Box::new(pty.writer()?));
        Pane::start(
            Terminal::Local(pty),
            source,
            writer,
            replies,
            screen,
            sink,
            0,
        )
    }

    /// Start a program in a pane the pane host holds under `id`, so it
    /// outlives this process.
    pub fn spawn_on(
        host: &Arc<HostClient>,
        id: &str,
        spec: &SpawnSpec,
        sink: Sink,
    ) -> io::Result<Pane> {
        let parts = host.spawn(id, spec)?;
        let screen = Screen::new(spec.cols, spec.rows);
        let replies = Box::new(hosted::HostWriter::clone_of(&parts.writer));
        Pane::start(
            Terminal::Hosted(parts.handle),
            Source::Hosted(parts.source),
            Box::new(parts.writer),
            replies,
            screen,
            sink,
            0,
        )
    }

    /// Take over a pane the pane host holds. With `handoff` (an upgrade):
    /// the previous daemon's picture of the screen, and output from the
    /// byte it stopped at. Without (the previous daemon crashed): the
    /// screen is rebuilt from the output the host kept.
    pub fn adopt_on(
        host: &Arc<HostClient>,
        id: &str,
        handoff: Option<&PaneHandoff>,
        sink: Sink,
    ) -> io::Result<Pane> {
        let (parts, attached) = host.attach(id, handoff.map(|h| h.offset))?;
        let screen = match handoff {
            // The host no longer has everything since the snapshot: the
            // snapshot plus what is left is still the best picture there is.
            Some(h) => {
                let mut screen = Screen::new(h.cols, h.rows);
                screen.feed(&h.snapshot);
                if attached.lost {
                    tracing::warn!(pane = id, "some output was not kept across the handoff");
                }
                screen
            }
            None => Screen::new(attached.cols, attached.rows),
        };
        let replies = Box::new(hosted::HostWriter::clone_of(&parts.writer));
        Pane::start(
            Terminal::Hosted(parts.handle),
            Source::Hosted(parts.source),
            Box::new(parts.writer),
            replies,
            screen,
            sink,
            // Old output is replayed into the screen; the questions in it
            // (cursor position and the like) were answered long ago.
            attached.end,
        )
    }

    /// Rebuild a pane from handoff state inside the new process image.
    ///
    /// # Safety
    /// `h.master_fd` must be an inherited PTY master owned by nobody else.
    pub unsafe fn adopt(h: &PaneHandoff, sink: Sink) -> io::Result<Pane> {
        let pty = Pty::adopt(h.master_fd, h.pid)?;
        let mut screen = Screen::new(h.cols, h.rows);
        screen.feed(&h.snapshot);
        let source = Source::Local(pty.reader()?);
        let (writer, replies) = (Box::new(pty.writer()?), Box::new(pty.writer()?));
        Pane::start(
            Terminal::Local(pty),
            source,
            writer,
            replies,
            screen,
            sink,
            0,
        )
    }

    /// `quiet_until`: output before this byte is a replay, and gets no replies.
    fn start(
        pty: Terminal,
        mut reader: Source,
        writer: Writer,
        mut reply_writer: Writer,
        screen: Screen,
        mut sink: Sink,
        quiet_until: u64,
    ) -> io::Result<Pane> {
        let screen = Arc::new(Mutex::new(screen));
        let exited = Arc::new(Mutex::new(None));
        let detach = Arc::new(AtomicBool::new(false));
        let bytes_out = Arc::new(AtomicU64::new(0));
        let last_output_ms = Arc::new(AtomicU64::new(now_ms()));
        let pid = match &pty {
            Terminal::Local(p) => p.pid(),
            Terminal::Hosted(h) => h.pid(),
        };

        let handle = {
            let screen = screen.clone();
            let exited = exited.clone();
            let detach = detach.clone();
            let bytes_out = bytes_out.clone();
            let last_output_ms = last_output_ms.clone();
            thread::Builder::new()
                .name(format!("pane-{pid}"))
                .spawn(move || {
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        // Wait in short steps so a detach request is noticed
                        // without output arriving.
                        match reader.ready() {
                            Ok(false) => {
                                if detach.load(Ordering::SeqCst) {
                                    return;
                                }
                                continue;
                            }
                            Err(_) => break,
                            Ok(true) => {}
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
                        let replay = reader.offset() <= quiet_until && quiet_until > 0;
                        for ev in events {
                            match ev {
                                ScreenEvent::Reply(_) if replay => {}
                                ScreenEvent::Bell if replay => {}
                                ScreenEvent::Reply(bytes) => {
                                    let _ = reply_writer.write_all(&bytes);
                                }
                                ScreenEvent::Title(t) => sink(PaneEvent::Title(t)),
                                ScreenEvent::Bell => sink(PaneEvent::Bell),
                            }
                        }
                    }
                    let info = reader.exit_status();
                    *exited.lock().unwrap() = Some(info);
                    sink(PaneEvent::Exited(info));
                })?
        };

        Ok(Pane {
            pty,
            screen,
            input: Input::start(writer)?,
            exited,
            detach,
            bytes_out,
            last_output_ms,
            reader: Mutex::new(Some(handle)),
        })
    }

    pub fn pid(&self) -> i32 {
        match &self.pty {
            Terminal::Local(p) => p.pid(),
            Terminal::Hosted(h) => h.pid(),
        }
    }

    /// The daemon is done with this pane for good. A pane in this process
    /// ends when it is dropped; one in the pane host has to be told, since
    /// dropping it is also what a daemon does when it is being replaced.
    pub fn forget(&self) {
        if let Terminal::Hosted(h) = &self.pty {
            h.forget();
        }
    }

    /// Send input to the program. It is queued and this returns at once; see
    /// `Input`. Refused when too much is already waiting.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.input.send(bytes)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let mut screen = self.screen.lock().unwrap();
        match &self.pty {
            Terminal::Local(p) => p.resize(cols, rows)?,
            Terminal::Hosted(h) => h.resize(cols, rows)?,
        }
        screen.resize(cols, rows);
        Ok(())
    }

    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        match &self.pty {
            Terminal::Local(p) => p.signal(sig),
            Terminal::Hosted(h) => h.signal(sig),
        }
    }

    pub fn exit_info(&self) -> Option<ExitInfo> {
        *self.exited.lock().unwrap()
    }

    pub fn info(&self) -> ScreenInfo {
        self.screen.lock().unwrap().info()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.screen.lock().unwrap().bracketed_paste()
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
    pub fn with_screen<R>(&self, f: impl FnOnce(&mut Screen) -> R) -> R {
        f(&mut self.screen.lock().unwrap())
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
        if matches!(self.pty, Terminal::Local(_)) && !crate::sys::LOCAL_HANDOFF {
            // The reader is left running: the pane stays in service.
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a terminal in this process cannot be handed to another on this system",
            ));
        }
        self.detach.store(true, Ordering::SeqCst);
        if let Some(h) = self.reader.lock().unwrap().take() {
            let _ = h.join();
        }
        let mut screen = self.screen.lock().unwrap();
        let (raw, host_id, offset) = match &self.pty {
            Terminal::Local(p) => (p.handoff_fd()?, None, 0),
            // The reader has stopped: this is the first byte it did not take.
            Terminal::Hosted(h) => (-1, Some(h.id().to_string()), h.offset()),
        };
        Ok(PaneHandoff {
            master_fd: raw,
            host_id,
            offset,
            pid: self.pid(),
            cols: screen.cols(),
            rows: screen.rows(),
            snapshot: screen.snapshot_ansi(max_history),
        })
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // The reader thread holds the terminal (and, for a hosted pane, the
        // connection to the host) open: let it go.
        self.detach.store(true, Ordering::SeqCst);
    }
}
