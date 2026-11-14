//! The pane host: a small process that owns the terminals and their
//! programs, so they outlive the daemon (DECISIONS.md, "Windows").
//!
//! On Windows a pseudoconsole belongs to the process that made it and cannot
//! be handed on, so the daemon cannot keep panes through its own upgrade the
//! way it does on Unix (exec in place). The host is what stays: the daemon
//! is its client, and a new daemon attaches to the panes the old one left.
//!
//! The host is deliberately small and knows nothing about agents, projects
//! or screens. Per pane it keeps the terminal, the program's exit status,
//! and the last `RING` bytes the program wrote, each byte numbered from the
//! pane's start. The screen stays in the daemon; a daemon that attaches says
//! from which byte it wants the output (an upgrade: the byte its predecessor
//! stopped at, with that daemon's snapshot of the screen; after a crash: the
//! whole ring, replayed into a fresh screen).
//!
//! Two kinds of connection on one pipe, told apart by the first line:
//!
//!   control   {"op":"hello"} … one JSON line each way per request
//!   events    {"events":true}, then binary frames from the host only
//!
//! They are separate because a Windows pipe opened for plain reads and
//! writes does one thing at a time: a thread blocked reading would hold up
//! every write on the same connection.
//!
//! The protocol only grows. `PROTOCOL` is what this host speaks; a daemon
//! speaks every protocol back to 1, and never asks an older host for
//! something it does not have.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::pty::{ExitInfo, Pty, PtyWriter, Signal, SpawnSpec};
use crate::sys::local;

/// The protocol this host speaks. Raised when a request or frame is added.
pub const PROTOCOL: u32 = 1;

/// How much of each pane's output the host keeps for a daemon that attaches.
pub const RING: usize = 8 * 1024 * 1024;

/// Frame kinds on an events connection.
pub const FRAME_DATA: u8 = 1;
pub const FRAME_EXIT: u8 = 2;

/// One frame for the events connection:
/// kind, id length, id, offset (u64 LE), payload length (u32 LE), payload.
pub fn frame(kind: u8, id: &str, offset: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(14 + id.len() + payload.len());
    out.push(kind);
    out.push(id.len() as u8);
    out.extend_from_slice(id.as_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

struct Ring {
    /// The number of the first byte in `buf`.
    start: u64,
    buf: VecDeque<u8>,
    /// Which events connection this pane's output goes to (0: none).
    attached: u64,
}

impl Ring {
    fn end(&self) -> u64 {
        self.start + self.buf.len() as u64
    }

    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes);
        if self.buf.len() > RING {
            let mut drop = self.buf.len() - RING;
            // Cut at a line end where one is near, so a replay starts cleanly.
            if let Some(nl) = self
                .buf
                .iter()
                .skip(drop)
                .take(4096)
                .position(|b| *b == b'\n')
            {
                drop += nl + 1;
            }
            self.buf.drain(..drop);
            self.start += drop as u64;
        }
    }
}

struct HostPane {
    pty: Pty,
    writer: Mutex<PtyWriter>,
    ring: Mutex<Ring>,
    exit: Mutex<Option<ExitInfo>>,
    size: Mutex<(u16, u16)>,
}

struct Host {
    panes: Mutex<HashMap<String, Arc<HostPane>>>,
    /// The current events connection and its number. A new one replaces it.
    events: Mutex<(u64, Option<UnboundedSender<Vec<u8>>>)>,
    next_events: AtomicU64,
    /// Open control connections: a daemon is here while this is not zero.
    daemons: AtomicUsize,
    /// When the last daemon left.
    alone_since: Mutex<Instant>,
    started: Instant,
}

impl Host {
    fn send(&self, generation: u64, bytes: Vec<u8>) {
        let events = self.events.lock().unwrap();
        if events.0 == generation {
            if let Some(tx) = &events.1 {
                let _ = tx.send(bytes);
            }
        }
    }

    fn pane(&self, req: &Value) -> Result<Arc<HostPane>, String> {
        let id = req["id"].as_str().unwrap_or("");
        self.panes
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no pane {id}"))
    }

    fn spawn(self: &Arc<Self>, id: &str, spec: &SpawnSpec) -> Result<Value, String> {
        if id.is_empty() || id.len() > 200 {
            return Err("a pane needs an id".into());
        }
        if self.panes.lock().unwrap().contains_key(id) {
            return Err(format!("pane {id} already exists"));
        }
        let pty = Pty::spawn(spec).map_err(|e| e.to_string())?;
        let mut reader = pty.reader().map_err(|e| e.to_string())?;
        let writer = pty.writer().map_err(|e| e.to_string())?;
        let pid = pty.pid();
        let pane = Arc::new(HostPane {
            pty,
            writer: Mutex::new(writer),
            // A pane belongs to the daemon that started it until another attaches.
            ring: Mutex::new(Ring {
                start: 0,
                buf: VecDeque::new(),
                attached: self.events.lock().unwrap().0,
            }),
            exit: Mutex::new(None),
            size: Mutex::new((spec.cols, spec.rows)),
        });
        self.panes
            .lock()
            .unwrap()
            .insert(id.to_string(), pane.clone());
        let (host, id) = (self.clone(), id.to_string());
        std::thread::Builder::new()
            .name(format!("host-pane-{pid}"))
            .spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match reader.ready() {
                        Ok(false) => continue,
                        Ok(true) => {}
                        Err(_) => break,
                    }
                    let n = match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    };
                    // Kept and sent under one lock, so a daemon that attaches
                    // gets every byte once: from the ring, or as a frame.
                    let mut ring = pane.ring.lock().unwrap();
                    let at = ring.end();
                    ring.push(&buf[..n]);
                    host.send(ring.attached, frame(FRAME_DATA, &id, at, &buf[..n]));
                }
                let info = reader.exit_status();
                let ring = pane.ring.lock().unwrap();
                *pane.exit.lock().unwrap() = Some(info);
                let said = serde_json::to_vec(&info).unwrap_or_default();
                host.send(ring.attached, frame(FRAME_EXIT, &id, ring.end(), &said));
            })
            .map_err(|e| e.to_string())?;
        Ok(json!({"pid": pid}))
    }

    /// Send a pane's output to the current events connection from byte
    /// `from` on (or from as far back as the ring goes), then as it comes.
    fn attach(&self, id: &str, pane: &HostPane, from: Option<u64>) -> Value {
        let generation = self.events.lock().unwrap().0;
        let mut ring = pane.ring.lock().unwrap();
        let first = from.unwrap_or(ring.start).clamp(ring.start, ring.end());
        ring.attached = generation;
        let skip = (first - ring.start) as usize;
        let backlog: Vec<u8> = ring.buf.iter().skip(skip).copied().collect();
        let mut at = first;
        for chunk in backlog.chunks(64 * 1024) {
            self.send(generation, frame(FRAME_DATA, id, at, chunk));
            at += chunk.len() as u64;
        }
        let exit = *pane.exit.lock().unwrap();
        if let Some(info) = exit {
            let said = serde_json::to_vec(&info).unwrap_or_default();
            self.send(generation, frame(FRAME_EXIT, id, ring.end(), &said));
        }
        let (cols, rows) = *pane.size.lock().unwrap();
        json!({
            "pid": pane.pty.pid(), "cols": cols, "rows": rows, "exit": exit,
            // Bytes before `first` that the daemon asked for are gone.
            "first": first, "end": ring.end(), "lost": from.is_some_and(|f| f < first),
        })
    }

    fn request(self: &Arc<Self>, req: &Value) -> Result<Value, String> {
        let op = req["op"].as_str().unwrap_or("");
        match op {
            "hello" => Ok(json!({
                "protocol": PROTOCOL,
                "version": env!("CARGO_PKG_VERSION"),
                "pid": std::process::id(),
                "panes": self.panes.lock().unwrap().len(),
                "up_secs": self.started.elapsed().as_secs(),
            })),
            "spawn" => {
                let spec: SpawnSpec =
                    serde_json::from_value(req["spec"].clone()).map_err(|e| e.to_string())?;
                self.spawn(req["id"].as_str().unwrap_or(""), &spec)
            }
            "attach" => {
                let pane = self.pane(req)?;
                Ok(self.attach(
                    req["id"].as_str().unwrap_or(""),
                    &pane,
                    req["from"].as_u64(),
                ))
            }
            "write" => {
                let pane = self.pane(req)?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(req["data"].as_str().unwrap_or(""))
                    .map_err(|e| e.to_string())?;
                let mut w = pane.writer.lock().unwrap();
                w.write_all(&bytes).map_err(|e| e.to_string())?;
                Ok(json!({}))
            }
            "resize" => {
                let pane = self.pane(req)?;
                let cols = req["cols"].as_u64().unwrap_or(80) as u16;
                let rows = req["rows"].as_u64().unwrap_or(24) as u16;
                pane.pty.resize(cols, rows).map_err(|e| e.to_string())?;
                *pane.size.lock().unwrap() = (cols, rows);
                Ok(json!({}))
            }
            "signal" => {
                let pane = self.pane(req)?;
                let sig = match req["signal"].as_str().unwrap_or("") {
                    "hangup" => Signal::Hangup,
                    "terminate" => Signal::Terminate,
                    "kill" => Signal::Kill,
                    other => return Err(format!("no signal {other}")),
                };
                pane.pty.signal(sig).map_err(|e| e.to_string())?;
                Ok(json!({}))
            }
            "list" => {
                let panes = self.panes.lock().unwrap();
                let rows: Vec<Value> = panes
                    .iter()
                    .map(|(id, p)| {
                        let ring = p.ring.lock().unwrap();
                        let (cols, rows) = *p.size.lock().unwrap();
                        json!({
                            "id": id, "pid": p.pty.pid(), "cols": cols, "rows": rows,
                            "exit": *p.exit.lock().unwrap(), "first": ring.start, "end": ring.end(),
                        })
                    })
                    .collect();
                Ok(json!(rows))
            }
            // The daemon is done with a pane: end what is left of it.
            "forget" => {
                let id = req["id"].as_str().unwrap_or("");
                if let Some(pane) = self.panes.lock().unwrap().remove(id) {
                    if pane.exit.lock().unwrap().is_none() {
                        let _ = pane.pty.signal(Signal::Kill);
                    }
                }
                Ok(json!({}))
            }
            // The station is stopping: every pane ends, and so does the host.
            "shutdown" => {
                self.end_all();
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_millis(200));
                    std::process::exit(0);
                });
                Ok(json!({}))
            }
            other => Err(format!(
                "this pane host (protocol {PROTOCOL}) does not know {other:?}"
            )),
        }
    }

    fn end_all(&self) {
        for pane in self.panes.lock().unwrap().values() {
            if pane.exit.lock().unwrap().is_none() {
                let _ = pane.pty.signal(Signal::Hangup);
                let _ = pane.pty.signal(Signal::Kill);
            }
        }
    }
}

async fn connection(host: Arc<Host>, stream: local::Stream) -> io::Result<()> {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = tokio::io::BufReader::new(read).lines();
    let Some(first) = lines.next_line().await? else {
        return Ok(());
    };
    let first: Value = serde_json::from_str(&first).unwrap_or(Value::Null);
    if first["events"] == json!(true) {
        // This connection now carries every attached pane's output.
        let (tx, mut rx) = unbounded_channel::<Vec<u8>>();
        let generation = host.next_events.fetch_add(1, Ordering::SeqCst) + 1;
        *host.events.lock().unwrap() = (generation, Some(tx));
        write
            .write_all(format!("{}\n", json!({"events": generation})).as_bytes())
            .await?;
        while let Some(bytes) = rx.recv().await {
            if write.write_all(&bytes).await.is_err() {
                break;
            }
        }
        let mut events = host.events.lock().unwrap();
        if events.0 == generation {
            events.1 = None;
        }
        return Ok(());
    }
    host.daemons.fetch_add(1, Ordering::SeqCst);
    let mut next = Some(first);
    let result = async {
        loop {
            let req = match next.take() {
                Some(req) => req,
                None => match lines.next_line().await? {
                    Some(line) => serde_json::from_str(&line).unwrap_or(Value::Null),
                    None => return Ok(()),
                },
            };
            // A request can wait on a program (a write to one that is not
            // reading): it must not hold up the other connections.
            let h = host.clone();
            let out = tokio::task::spawn_blocking(move || match h.request(&req) {
                Ok(v) => json!({"ok": v}),
                Err(e) => json!({"error": e}),
            })
            .await
            .unwrap_or_else(|e| json!({"error": e.to_string()}));
            write.write_all(format!("{out}\n").as_bytes()).await?;
        }
    }
    .await;
    host.daemons.fetch_sub(1, Ordering::SeqCst);
    *host.alone_since.lock().unwrap() = Instant::now();
    result
}

/// Run a pane host on `pipe` until it is told to shut down, or it has been
/// left alone too long: with no pane, a few seconds; with panes, `linger`
/// (a daemon that crashed is expected back; panes nobody can reach are not
/// kept for ever). `None`: wait for a daemon as long as it takes.
pub fn serve(pipe: &Path, linger: Option<Duration>) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let mut listener = local::Listener::bind(pipe)?;
        let host = Arc::new(Host {
            panes: Mutex::new(HashMap::new()),
            events: Mutex::new((0, None)),
            next_events: AtomicU64::new(0),
            daemons: AtomicUsize::new(0),
            alone_since: Mutex::new(Instant::now()),
            started: Instant::now(),
        });
        {
            let host = host.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    if host.daemons.load(Ordering::SeqCst) > 0 {
                        continue;
                    }
                    let alone = host.alone_since.lock().unwrap().elapsed();
                    let live = host
                        .panes
                        .lock()
                        .unwrap()
                        .values()
                        .any(|p| p.exit.lock().unwrap().is_none());
                    let over = if live {
                        linger.is_some_and(|l| alone > l)
                    } else {
                        alone > Duration::from_secs(5)
                    };
                    if over {
                        host.end_all();
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        std::process::exit(0);
                    }
                }
            });
        }
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    let host = host.clone();
                    tokio::spawn(async move {
                        let _ = connection(host, stream).await;
                    });
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
}

/// How long a host with running panes waits for a daemon before it ends
/// them, when `SPANDECK_HOST_LINGER` (seconds; 0: for ever) does not say.
/// The login service starts a daemon that failed again within a minute or two.
pub const LINGER: Duration = Duration::from_secs(600);

/// The pane host program: `spandeck-pane-host <pipe>`.
pub fn main() -> i32 {
    let Some(pipe) = std::env::args().nth(1) else {
        eprintln!("usage: spandeck-pane-host <pipe>   (started by spandeckd, not by hand)");
        return 64;
    };
    let linger = match std::env::var("SPANDECK_HOST_LINGER")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        Some(0) => None,
        Some(secs) => Some(Duration::from_secs(secs)),
        None => Some(LINGER),
    };
    match serve(Path::new(&pipe), linger) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("spandeck-pane-host: {e}");
            1
        }
    }
}

/// The host listening on `pipe`: the one already there, else a new one
/// started from `program` (and waited for).
pub fn connect_or_start(pipe: &Path, program: &Path) -> io::Result<Arc<crate::hosted::HostClient>> {
    if let Ok(client) = crate::hosted::HostClient::connect(pipe) {
        return Ok(client);
    }
    let mut cmd = std::process::Command::new(program);
    cmd.arg(pipe);
    crate::sys::spawn_detached(&mut cmd)?;
    let start = Instant::now();
    loop {
        match crate::hosted::HostClient::connect(pipe) {
            Ok(client) => return Ok(client),
            Err(e) if start.elapsed() > Duration::from_secs(10) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("the pane host ({}) did not start: {e}", program.display()),
                ))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(30)),
        }
    }
}
