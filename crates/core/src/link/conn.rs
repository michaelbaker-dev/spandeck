//! A link that is running: the multiplexer (`mux.rs`) with something to
//! read from and something to write to, and its streams as things a task
//! can read and write. Both ends use it: the station the owner is at
//! (`near.rs`), over a carrier's pipes, and the helper on the far side
//! (`helper.rs`), over its own standard input and output.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch, Notify};

use super::mux::{Event, Mux};

enum In {
    Data(Vec<u8>),
    Fin,
    Reset,
}

struct Pipe {
    tx: mpsc::UnboundedSender<In>,
    /// Told when more may be sent, and when the stream is gone.
    writable: Notify,
    gone: AtomicBool,
}

struct Inner {
    mux: Mutex<Mux>,
    /// Told when the multiplexer has something to write.
    output: Notify,
    pipes: Mutex<HashMap<u32, Arc<Pipe>>>,
    /// The other end's hello, once it has come.
    hello: watch::Sender<Option<Value>>,
    /// Why the link ended, once it has.
    ended: watch::Sender<Option<String>>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Inner {
    fn end(&self, why: String) {
        self.ended.send_if_modified(|e| {
            if e.is_none() {
                *e = Some(why);
                true
            } else {
                false
            }
        });
        for (_, pipe) in self.pipes.lock().unwrap().drain() {
            pipe.gone.store(true, Ordering::SeqCst);
            let _ = pipe.tx.send(In::Reset);
            pipe.writable.notify_one();
        }
        self.output.notify_one();
    }

    fn is_ended(&self) -> bool {
        self.ended.borrow().is_some()
    }
}

/// One end of a link.
#[derive(Clone)]
pub struct Conn {
    inner: Arc<Inner>,
}

/// A stream the other end opened.
pub struct Incoming {
    pub kind: u8,
    pub stream: Stream,
}

impl Conn {
    /// Start reading and writing. Nothing is said to the other end until
    /// `say_hello`. Streams the other end opens (the far end only: the
    /// near one's multiplexer refuses them all) arrive on the receiver.
    pub fn start<R, W>(mux: Mux, read: R, write: W) -> (Conn, mpsc::UnboundedReceiver<Incoming>)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let inner = Arc::new(Inner {
            mux: Mutex::new(mux),
            output: Notify::new(),
            pipes: Mutex::new(HashMap::new()),
            hello: watch::channel(None).0,
            ended: watch::channel(None).0,
            tasks: Mutex::new(vec![]),
        });
        let (opened_tx, opened_rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(read_loop(inner.clone(), read, opened_tx));
        let writer = tokio::spawn(write_loop(inner.clone(), write));
        *inner.tasks.lock().unwrap() = vec![reader, writer];
        (Conn { inner }, opened_rx)
    }

    /// Begin the link from this end (`Mux::hello`).
    pub fn say_hello(&self, more: Value) {
        self.inner.mux.lock().unwrap().hello(more);
        self.inner.output.notify_one();
    }

    /// The other end's hello. An error: the link ended first, and why.
    pub async fn hello(&self) -> Result<Value, String> {
        let mut hello = self.inner.hello.subscribe();
        let mut ended = self.inner.ended.subscribe();
        loop {
            if let Some(v) = hello.borrow_and_update().clone() {
                return Ok(v);
            }
            if let Some(why) = ended.borrow_and_update().clone() {
                return Err(why);
            }
            tokio::select! {
                _ = hello.changed() => {}
                _ = ended.changed() => {}
            }
        }
    }

    /// Waits until the link has ended, and says why.
    pub async fn ended(&self) -> String {
        let mut ended = self.inner.ended.subscribe();
        loop {
            if let Some(why) = ended.borrow_and_update().clone() {
                return why;
            }
            if ended.changed().await.is_err() {
                return "the link is gone".into();
            }
        }
    }

    /// Why the link ended, if it has.
    pub fn ended_now(&self) -> Option<String> {
        self.inner.ended.borrow().clone()
    }

    /// End the link from this end. Every stream ends with it.
    pub fn close(&self, why: &str) {
        self.inner.end(why.to_string());
        for task in self.inner.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }

    /// How many streams the other end tried to open and was refused.
    pub fn refused(&self) -> u64 {
        self.inner.mux.lock().unwrap().refused()
    }

    /// Open a stream to the other end.
    pub fn open(&self, kind: u8) -> Result<Stream, String> {
        if let Some(why) = self.inner.ended.borrow().clone() {
            return Err(why);
        }
        let id = self
            .inner
            .mux
            .lock()
            .unwrap()
            .open(kind)
            .ok_or("the link is carrying as many connections as it may")?;
        let stream = new_stream(&self.inner, id);
        self.inner.output.notify_one();
        Ok(stream)
    }
}

fn new_stream(inner: &Arc<Inner>, id: u32) -> Stream {
    let (tx, rx) = mpsc::unbounded_channel();
    let pipe = Arc::new(Pipe {
        tx,
        writable: Notify::new(),
        gone: AtomicBool::new(false),
    });
    inner.pipes.lock().unwrap().insert(id, pipe.clone());
    let held = Arc::new(Held {
        id,
        inner: inner.clone(),
    });
    Stream {
        read: StreamRead {
            id,
            rx,
            held: held.clone(),
        },
        write: StreamWrite { id, pipe, held },
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    inner: Arc<Inner>,
    mut read: R,
    opened: mpsc::UnboundedSender<Incoming>,
) {
    let mut buf = vec![0u8; 64 * 1024];
    let why = loop {
        let n = match read.read(&mut buf).await {
            Ok(0) => {
                break match inner.mux.lock().unwrap().eof() {
                    Ok(()) => "the other end closed the link".to_string(),
                    Err(e) => e.to_string(),
                }
            }
            Ok(n) => n,
            Err(e) => break format!("the link could not be read: {e}"),
        };
        let (fed, events) = {
            let mut mux = inner.mux.lock().unwrap();
            let fed = mux.feed(&buf[..n]);
            let events: Vec<Event> = std::iter::from_fn(|| mux.event()).collect();
            (fed, events)
        };
        for ev in events {
            match ev {
                Event::Hello(v) => {
                    inner.hello.send_replace(Some(v));
                }
                Event::Opened { id, kind } => {
                    let stream = new_stream(&inner, id);
                    let _ = opened.send(Incoming { kind, stream });
                }
                Event::Refused { kind } => {
                    tracing::warn!(
                        kind = super::mux::kind::name(kind),
                        "the far end of a link tried to open a stream and was refused"
                    );
                }
                Event::Data { id, bytes } => {
                    let pipe = inner.pipes.lock().unwrap().get(&id).cloned();
                    if let Some(pipe) = pipe {
                        let _ = pipe.tx.send(In::Data(bytes));
                    }
                }
                Event::Fin { id } => {
                    let pipe = inner.pipes.lock().unwrap().get(&id).cloned();
                    if let Some(pipe) = pipe {
                        let _ = pipe.tx.send(In::Fin);
                    }
                }
                Event::Reset { id } => {
                    let pipe = inner.pipes.lock().unwrap().remove(&id);
                    if let Some(pipe) = pipe {
                        pipe.gone.store(true, Ordering::SeqCst);
                        let _ = pipe.tx.send(In::Reset);
                        pipe.writable.notify_one();
                    }
                }
                Event::Writable { id } => {
                    let pipe = inner.pipes.lock().unwrap().get(&id).cloned();
                    if let Some(pipe) = pipe {
                        pipe.writable.notify_one();
                    }
                }
            }
        }
        // A refusal or a credit may be waiting to be written.
        inner.output.notify_one();
        if let Err(e) = fed {
            break e.to_string();
        }
    };
    inner.end(why);
}

async fn write_loop<W: AsyncWrite + Unpin>(inner: Arc<Inner>, mut write: W) {
    loop {
        let bytes = inner.mux.lock().unwrap().output(64 * 1024);
        if bytes.is_empty() {
            if inner.is_ended() {
                return;
            }
            inner.output.notified().await;
            continue;
        }
        let wrote = async {
            write.write_all(&bytes).await?;
            write.flush().await
        };
        if let Err(e) = wrote.await {
            inner.end(format!("the link could not be written: {e}"));
            return;
        }
    }
}

/// Lets go of the stream when both halves are dropped: if it was not
/// finished by both ends, the other end is told it is gone.
struct Held {
    id: u32,
    inner: Arc<Inner>,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.inner.pipes.lock().unwrap().remove(&self.id);
        self.inner.mux.lock().unwrap().release(self.id);
        self.inner.output.notify_one();
    }
}

pub struct Stream {
    read: StreamRead,
    write: StreamWrite,
}

impl Stream {
    pub fn split(self) -> (StreamRead, StreamWrite) {
        (self.read, self.write)
    }
}

pub struct StreamRead {
    id: u32,
    rx: mpsc::UnboundedReceiver<In>,
    held: Arc<Held>,
}

fn gone() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::ConnectionReset, "the stream is gone")
}

impl StreamRead {
    /// The next bytes. `None`: the other end has finished sending. After
    /// the bytes are used, say so with `used`, or no more will come.
    pub async fn read(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        match self.rx.recv().await {
            Some(In::Data(bytes)) => Ok(Some(bytes)),
            Some(In::Fin) => Ok(None),
            Some(In::Reset) | None => Err(gone()),
        }
    }

    pub fn used(&self, n: usize) {
        self.held.inner.mux.lock().unwrap().consumed(self.id, n);
        self.held.inner.output.notify_one();
    }

    /// Everything the other end sends until it finishes, if that is no
    /// more than `max` bytes.
    pub async fn read_all(&mut self, max: usize) -> std::io::Result<Vec<u8>> {
        let mut all = vec![];
        while let Some(bytes) = self.read().await? {
            self.used(bytes.len());
            all.extend(bytes);
            if all.len() > max {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "more was sent than is taken",
                ));
            }
        }
        Ok(all)
    }
}

pub struct StreamWrite {
    id: u32,
    pipe: Arc<Pipe>,
    held: Arc<Held>,
}

impl StreamWrite {
    /// Send all of `bytes`, waiting while the stream's window is full.
    pub async fn write_all(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        let inner = &self.held.inner;
        while !bytes.is_empty() {
            if self.pipe.gone.load(Ordering::SeqCst) || inner.is_ended() {
                return Err(gone());
            }
            let n = inner.mux.lock().unwrap().send(self.id, bytes);
            bytes = &bytes[n..];
            if n > 0 {
                inner.output.notify_one();
            }
            if !bytes.is_empty() {
                if !inner.mux.lock().unwrap().has(self.id) {
                    return Err(gone());
                }
                self.pipe.writable.notified().await;
            }
        }
        Ok(())
    }

    /// This end will send no more.
    pub fn finish(&mut self) {
        self.held.inner.mux.lock().unwrap().fin(self.id);
        self.held.inner.output.notify_one();
    }
}

/// Carry one TCP connection over one stream, both ways, until both have
/// finished or either has failed. What arrives from the link is allowed
/// again only once the socket has taken it, and the socket is read only as
/// fast as the link's window lets it be sent: a browser that stops reading
/// stops its own stream and no other.
pub async fn splice(tcp: tokio::net::TcpStream, stream: Stream) {
    let (mut from_tcp, mut to_tcp) = tcp.into_split();
    let (mut from_link, mut to_link) = stream.split();
    let up = async {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = from_tcp.read(&mut buf).await?;
            if n == 0 {
                to_link.finish();
                return Ok::<(), std::io::Error>(());
            }
            to_link.write_all(&buf[..n]).await?;
        }
    };
    let down = async {
        while let Some(bytes) = from_link.read().await? {
            to_tcp.write_all(&bytes).await?;
            from_link.used(bytes.len());
        }
        to_tcp.shutdown().await
    };
    let _ = tokio::try_join!(up, down);
}

#[cfg(test)]
mod tests {
    use super::super::mux::{kind, WINDOW};
    use super::*;
    use serde_json::json;

    fn link() -> (Conn, Conn, mpsc::UnboundedReceiver<Incoming>) {
        let (a, b) = tokio::io::duplex(8 * 1024);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let (near, _) = Conn::start(Mux::near(), ar, aw);
        let (far, opened) = Conn::start(Mux::far(), br, bw);
        near.say_hello(json!({"station": "laptop"}));
        far.say_hello(json!({"station": "devbox"}));
        (near, far, opened)
    }

    /// The owner's rule, with real tasks and a real pipe between the two
    /// ends: megabytes are being written to a stream nobody reads, and a
    /// second stream's question is still answered at once. Then the first
    /// is read, and every byte of it arrives.
    #[tokio::test]
    async fn megabytes_nobody_reads_do_not_hold_up_another_stream() {
        let (near, _far, mut opened) = link();
        assert_eq!(near.hello().await.unwrap()["station"], json!("devbox"));
        let (_stalled_read, mut stalled_write) = near.open(kind::WEB).unwrap().split();
        let flood = tokio::spawn(async move {
            let megabytes = vec![7u8; 4 * WINDOW as usize];
            stalled_write.write_all(&megabytes).await.unwrap();
            stalled_write.finish();
        });
        let (mut unread, _unread_write) = opened.recv().await.unwrap().stream.split();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !flood.is_finished(),
            "four windows were taken with none read"
        );
        // Another stream, meanwhile.
        let (mut ask_read, mut ask_write) = near.open(kind::PING).unwrap().split();
        let (mut theirs_read, mut theirs_write) = opened.recv().await.unwrap().stream.split();
        let answered = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            ask_write.write_all(b"ping").await.unwrap();
            let got = theirs_read.read().await.unwrap().unwrap();
            theirs_write.write_all(&got).await.unwrap();
            ask_read.read().await.unwrap().unwrap()
        })
        .await
        .expect("the second stream waited behind the first");
        assert_eq!(answered, b"ping");
        // The first is read at last.
        let mut total = 0;
        while let Some(bytes) = unread.read().await.unwrap() {
            assert!(bytes.iter().all(|b| *b == 7));
            total += bytes.len();
            unread.used(bytes.len());
        }
        assert_eq!(total, 4 * WINDOW as usize);
        flood.await.unwrap();
    }

    /// A carrier that dies takes every stream with it at once: a reader
    /// and a writer that were waiting are both told, and neither hangs.
    #[tokio::test]
    async fn every_stream_ends_when_the_link_does() {
        let (near, far, mut opened) = link();
        let (mut read, mut write) = near.open(kind::WEB).unwrap().split();
        let _theirs = opened.recv().await.unwrap();
        let writer =
            tokio::spawn(async move { write.write_all(&vec![0u8; 2 * WINDOW as usize]).await });
        let reader = tokio::spawn(async move { read.read().await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        far.close("the test cut it");
        let ended = tokio::time::timeout(std::time::Duration::from_secs(2), near.ended())
            .await
            .unwrap();
        assert_eq!(ended, "the other end closed the link");
        assert!(writer.await.unwrap().is_err());
        assert!(reader.await.unwrap().is_err());
        assert!(near.open(kind::WEB).is_err());
    }
}
