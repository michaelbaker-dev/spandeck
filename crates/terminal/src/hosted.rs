//! The daemon's side of the pane host (`host.rs`): one control connection
//! for requests, one events connection whose frames are handed to each
//! pane's reader.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};

use crate::host::{FRAME_DATA, FRAME_EXIT};
use crate::pty::{ExitInfo, Signal, SpawnSpec};
use crate::sys::local::{self, Conn};

enum Event {
    Data(Vec<u8>),
    Exit(ExitInfo),
}

type Routes = Arc<Mutex<HashMap<String, Sender<Event>>>>;

/// What a host says about itself.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HostInfo {
    pub protocol: u32,
    pub version: String,
    pub pid: u32,
}

/// A pane the host holds.
#[derive(Debug, Clone)]
pub struct HostedPane {
    pub id: String,
    pub pid: i32,
    pub cols: u16,
    pub rows: u16,
    pub exit: Option<ExitInfo>,
}

pub struct HostClient {
    pipe: PathBuf,
    control: Mutex<(BufReader<Conn>, Conn)>,
    routes: Routes,
    alive: Arc<AtomicBool>,
    pub info: HostInfo,
}

fn other(e: impl ToString) -> io::Error {
    io::Error::other(e.to_string())
}

impl HostClient {
    /// Connect to the host listening on `pipe`.
    pub fn connect(pipe: &Path) -> io::Result<Arc<HostClient>> {
        let conn = local::connect(pipe)?;
        let control = (BufReader::new(conn.try_clone()?), conn);
        // The events connection: one line each way, then frames only.
        let mut events = local::connect(pipe)?;
        events.write_all(b"{\"events\":true}\n")?;
        let mut events = BufReader::new(events);
        let mut line = String::new();
        events.read_line(&mut line)?;
        let routes: Routes = Default::default();
        let alive = Arc::new(AtomicBool::new(true));
        {
            let (routes, alive) = (routes.clone(), alive.clone());
            std::thread::Builder::new()
                .name("pane-host-events".into())
                .spawn(move || {
                    let _ = demux(&mut events, &routes);
                    // The host is gone, and every pane with it.
                    alive.store(false, Ordering::SeqCst);
                    for (_, tx) in routes.lock().unwrap().drain() {
                        let _ = tx.send(Event::Exit(ExitInfo::Unknown));
                    }
                })?;
        }
        let mut client = HostClient {
            pipe: pipe.to_path_buf(),
            control: Mutex::new(control),
            routes,
            alive,
            info: HostInfo {
                protocol: 0,
                version: String::new(),
                pid: 0,
            },
        };
        let hello = client.call(json!({"op": "hello"}))?;
        client.info = HostInfo {
            protocol: hello["protocol"].as_u64().unwrap_or(0) as u32,
            version: hello["version"].as_str().unwrap_or("").to_string(),
            pid: hello["pid"].as_u64().unwrap_or(0) as u32,
        };
        if client.info.protocol == 0 {
            return Err(other(
                "what answered on the pane host's pipe is not a pane host",
            ));
        }
        Ok(Arc::new(client))
    }

    pub fn pipe(&self) -> &Path {
        &self.pipe
    }

    /// False once the host has gone away (and its panes with it).
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn call(&self, req: Value) -> io::Result<Value> {
        let mut control = self.control.lock().unwrap();
        control.1.write_all(format!("{req}\n").as_bytes())?;
        let mut line = String::new();
        if control.0.read_line(&mut line)? == 0 {
            self.alive.store(false, Ordering::SeqCst);
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the pane host closed the connection",
            ));
        }
        let reply: Value = serde_json::from_str(&line).map_err(other)?;
        match reply.get("error").and_then(|e| e.as_str()) {
            Some(e) => Err(other(e)),
            None => Ok(reply["ok"].clone()),
        }
    }

    /// The panes the host holds, running or ended.
    pub fn list(&self) -> io::Result<Vec<HostedPane>> {
        let rows = self.call(json!({"op": "list"}))?;
        Ok(rows
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| HostedPane {
                id: r["id"].as_str().unwrap_or("").to_string(),
                pid: r["pid"].as_i64().unwrap_or(0) as i32,
                cols: r["cols"].as_u64().unwrap_or(80) as u16,
                rows: r["rows"].as_u64().unwrap_or(24) as u16,
                exit: serde_json::from_value(r["exit"].clone()).ok().flatten(),
            })
            .collect())
    }

    /// End a pane's programs if any are left, and drop what the host kept of it.
    pub fn forget(&self, id: &str) -> io::Result<()> {
        self.routes.lock().unwrap().remove(id);
        self.call(json!({"op": "forget", "id": id})).map(|_| ())
    }

    /// End every pane and the host itself (the station is stopping).
    pub fn shutdown(&self) -> io::Result<()> {
        self.call(json!({"op": "shutdown"})).map(|_| ())
    }

    /// Start a program in a new pane the host will hold under `id`.
    pub(crate) fn spawn(self: &Arc<Self>, id: &str, spec: &SpawnSpec) -> io::Result<Parts> {
        // The route is there before the program can write anything.
        let (tx, rx) = channel();
        self.routes.lock().unwrap().insert(id.to_string(), tx);
        let reply = match self.call(json!({"op": "spawn", "id": id, "spec": spec})) {
            Ok(r) => r,
            Err(e) => {
                self.routes.lock().unwrap().remove(id);
                return Err(e);
            }
        };
        Ok(self.parts(id, rx, reply["pid"].as_i64().unwrap_or(0) as i32, 0))
    }

    /// Take over a pane the host already holds. `from`: the byte to go on
    /// from (the daemon before this one stopped there), or `None` for
    /// everything the host kept. Returns the pane's size and the byte up to
    /// which what arrives is old output, not new.
    pub(crate) fn attach(
        self: &Arc<Self>,
        id: &str,
        from: Option<u64>,
    ) -> io::Result<(Parts, Attached)> {
        let (tx, rx) = channel();
        self.routes.lock().unwrap().insert(id.to_string(), tx);
        let reply = match self.call(json!({"op": "attach", "id": id, "from": from})) {
            Ok(r) => r,
            Err(e) => {
                self.routes.lock().unwrap().remove(id);
                return Err(e);
            }
        };
        let first = reply["first"].as_u64().unwrap_or(0);
        let attached = Attached {
            cols: reply["cols"].as_u64().unwrap_or(80) as u16,
            rows: reply["rows"].as_u64().unwrap_or(24) as u16,
            end: reply["end"].as_u64().unwrap_or(first),
            lost: reply["lost"].as_bool().unwrap_or(false),
        };
        let pid = reply["pid"].as_i64().unwrap_or(0) as i32;
        Ok((self.parts(id, rx, pid, first), attached))
    }

    fn parts(self: &Arc<Self>, id: &str, rx: Receiver<Event>, pid: i32, at: u64) -> Parts {
        let consumed = Arc::new(AtomicU64::new(at));
        let handle = Handle {
            host: self.clone(),
            id: id.to_string(),
            pid,
            consumed: consumed.clone(),
        };
        Parts {
            source: Source {
                rx,
                pending: Vec::new(),
                taken: 0,
                exit: None,
                consumed,
            },
            writer: HostWriter(handle.clone()),
            handle,
        }
    }
}

fn demux(events: &mut BufReader<Conn>, routes: &Routes) -> io::Result<()> {
    loop {
        let mut head = [0u8; 2];
        events.read_exact(&mut head)?;
        let mut id = vec![0u8; head[1] as usize];
        events.read_exact(&mut id)?;
        let mut nums = [0u8; 12];
        events.read_exact(&mut nums)?;
        let len = u32::from_le_bytes(nums[8..12].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; len];
        events.read_exact(&mut payload)?;
        let id = String::from_utf8_lossy(&id).into_owned();
        let event = match head[0] {
            FRAME_DATA => Event::Data(payload),
            FRAME_EXIT => {
                Event::Exit(serde_json::from_slice(&payload).unwrap_or(ExitInfo::Unknown))
            }
            // A newer host's frame this daemon does not know: skipped.
            _ => continue,
        };
        if let Some(tx) = routes.lock().unwrap().get(&id) {
            let _ = tx.send(event);
        }
    }
}

/// What `attach` learned about the pane.
pub(crate) struct Attached {
    pub cols: u16,
    pub rows: u16,
    /// Output up to this byte was written before this daemon attached.
    pub end: u64,
    /// The host no longer had all the output that was asked for.
    pub lost: bool,
}

pub(crate) struct Parts {
    pub source: Source,
    pub writer: HostWriter,
    pub handle: Handle,
}

/// A hosted pane's output, as its reader thread takes it.
pub(crate) struct Source {
    rx: Receiver<Event>,
    pending: Vec<u8>,
    taken: usize,
    exit: Option<ExitInfo>,
    /// The number of the next byte `read` will return.
    consumed: Arc<AtomicU64>,
}

impl Source {
    pub fn ready(&mut self) -> io::Result<bool> {
        if self.taken < self.pending.len() || self.exit.is_some() {
            return Ok(true);
        }
        match self.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Data(bytes)) => {
                self.pending = bytes;
                self.taken = 0;
                Ok(true)
            }
            Ok(Event::Exit(info)) => {
                self.exit = Some(info);
                Ok(true)
            }
            Err(RecvTimeoutError::Timeout) => Ok(false),
            Err(RecvTimeoutError::Disconnected) => {
                self.exit = Some(ExitInfo::Unknown);
                Ok(true)
            }
        }
    }

    /// `Ok(0)`: the pane's program has ended and its output is all read.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = &self.pending[self.taken..];
        let n = left.len().min(buf.len());
        buf[..n].copy_from_slice(&left[..n]);
        self.taken += n;
        self.consumed.fetch_add(n as u64, Ordering::SeqCst);
        Ok(n)
    }

    pub fn exit_status(&self) -> ExitInfo {
        self.exit.unwrap_or(ExitInfo::Unknown)
    }

    pub fn offset(&self) -> u64 {
        self.consumed.load(Ordering::SeqCst)
    }
}

/// Everything else a pane asks of its terminal, asked of the host.
#[derive(Clone)]
pub(crate) struct Handle {
    host: Arc<HostClient>,
    id: String,
    pid: i32,
    consumed: Arc<AtomicU64>,
}

impl Handle {
    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn offset(&self) -> u64 {
        self.consumed.load(Ordering::SeqCst)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        self.host
            .call(json!({"op": "resize", "id": self.id, "cols": cols, "rows": rows}))
            .map(|_| ())
    }

    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        let name = match sig {
            Signal::Hangup => "hangup",
            Signal::Terminate => "terminate",
            Signal::Kill => "kill",
        };
        self.host
            .call(json!({"op": "signal", "id": self.id, "signal": name}))
            .map(|_| ())
    }

    /// The pane is dropped by the daemon for good.
    pub fn forget(&self) {
        let _ = self.host.forget(&self.id);
    }
}

pub(crate) struct HostWriter(Handle);

impl HostWriter {
    pub fn clone_of(other: &HostWriter) -> HostWriter {
        HostWriter(other.0.clone())
    }
}

impl Write for HostWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let data = base64::engine::general_purpose::STANDARD.encode(buf);
        self.0
            .host
            .call(json!({"op": "write", "id": self.0.id, "data": data}))?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
