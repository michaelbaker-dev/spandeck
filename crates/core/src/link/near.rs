//! The near side of a link: the station the owner is at. One task per
//! `[[link]]` keeps its carrier running (the owner's `ssh`, or whatever
//! `cmd` names), and one port on 127.0.0.1 gives the browser on this
//! machine the far station's own deck. Each connection the browser makes
//! is one `web` stream.
//!
//! This side opens streams and takes none: its multiplexer refuses every
//! stream the far end tries to open (`Mux::near`). It runs nothing the far
//! end sends, installs and updates nothing there, and stores no password,
//! key or token: only the port it chose, whether the owner is holding the
//! link closed, and the id of the browser entry the far station made for
//! it, in a file only this user can read.

use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

use super::conn::Conn;
use super::mux::{clean, kind, Mux};
use super::{carrier_argv, is_ssh, plain_name, LinkConfig};
use crate::daemon::{invalid, Daemon, Result};
use crate::paths::now_ms;

/// The heartbeat: a ping this often, and "not answering" when none has
/// been answered for this long.
const PING_EVERY: Duration = Duration::from_secs(2);
const PING_DEAD: Duration = Duration::from_secs(6);
/// How long a carrier is given to reach the far side and say hello.
const HELLO_WAIT: Duration = Duration::from_secs(30);
/// Trying again: at once, then 100 ms doubling to this, each a little
/// more or less so that several links do not come back in step.
const RETRY_MAX: Duration = Duration::from_secs(5);
/// When trying again cannot help until the owner has done something.
const RETRY_NEEDS_YOU: Duration = Duration::from_secs(300);
/// The first port tried for a link that names none.
const FIRST_PORT: u16 = 17400;

/// Said when the far side ends the heartbeat's stream and not the link.
const GONE: &str = "the far side stopped answering pings";

pub const UP: &str = "up";
pub const CONNECTING: &str = "connecting";
pub const NOT_ANSWERING: &str = "not answering";
pub const NEEDS_YOU: &str = "needs you";
pub const HELD: &str = "held";
pub const NOT_STARTED: &str = "not started";

#[derive(Clone)]
struct Status {
    state: &'static str,
    /// When `state` became what it is.
    since_ms: u64,
    /// The far station, as it last said: its name, version and system.
    station: String,
    version: String,
    os: String,
    rtt_ms: Option<u64>,
    /// The carrier's process, while there is one.
    pid: Option<u32>,
    /// What to tell the owner, when it is not up.
    said: String,
    /// Why the local port is not open, when it is not.
    port_trouble: Option<String>,
}

pub struct Near {
    pub name: String,
    pub cmd: Vec<String>,
    pub port: u16,
    status: Mutex<Status>,
    conn: Mutex<Option<Conn>>,
    /// Told when the owner holds or lets go of the link, or says "now".
    kick: Notify,
    held: AtomicBool,
    /// A carrier of this link is running, or being ended.
    running: AtomicBool,
    file: std::path::PathBuf,
}

/// What is kept of a link between runs. No secret is in it.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Saved {
    port: u16,
    held: bool,
    /// The far station's entry for this machine's browser.
    device: Option<String>,
}

fn load(file: &std::path::Path) -> Saved {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn jitter(d: Duration) -> Duration {
    let mut b = [0u8; 1];
    let _ = spandeck_terminal::sys::random_bytes(&mut b);
    // 0.7 to 1.3 of it.
    d.mul_f64(0.7 + 0.6 * f64::from(b[0]) / 255.0)
}

/// How long to wait before try number `attempt` (from 0).
fn retry_in(attempt: u32) -> Duration {
    match attempt {
        0 => Duration::ZERO,
        n => jitter(
            Duration::from_millis(100)
                .saturating_mul(1 << (n - 1).min(10))
                .min(RETRY_MAX),
        ),
    }
}

/// The carrier's command as the owner would type it.
fn typed(cmd: &[String]) -> String {
    cmd.iter()
        .map(|a| {
            if a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./@:=+,".contains(c))
                && !a.is_empty()
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a carrier that never said hello means: the sentence for the
/// owner, and whether trying again by itself can help.
fn why_not(
    name: &str,
    cmd: &[String],
    why: &str,
    code: Option<i32>,
    stderr: &[String],
) -> (String, bool) {
    let all = format!("{}\n{why}", stderr.join("\n")).to_lowercase();
    let last = stderr.last().cloned().unwrap_or_default();
    let has = |words: &[&str]| words.iter().any(|w| all.contains(w));
    let then = format!("then `spandeck link up {name}`");
    if has(&[
        "host key verification failed",
        "remote host identification has changed",
        "host key is known",
        "no matching host key",
    ]) {
        return (
            format!(
                "ssh does not know this host's key, or the key has changed ({last}). spandeck accepts no host key for you. In a terminal run `{} true`, answer what it asks, {then}",
                typed(cmd)
            ),
            true,
        );
    }
    if has(&[
        "permission denied",
        "too many authentication failures",
        "passphrase",
        "password",
        "verification code",
        "authentication failed",
    ]) {
        return (
            format!(
                "ssh wants you to sign in ({last}), and spandeck never answers that for you. In a terminal run `{} true`, sign in, {then}",
                typed(cmd)
            ),
            true,
        );
    }
    if has(&["unrecognized subcommand", "unexpected argument"]) {
        return (
            format!(
                "{name} has a spandeck from before links. A link never updates anything: update spandeck there, {then}"
            ),
            true,
        );
    }
    if code == Some(127)
        || has(&[
            "command not found",
            "spandeck: not found",
            "no such file or directory",
            "is not recognized",
        ])
    {
        return (
            format!(
                "{name} has no spandeck where a link looks for it (the PATH there, and ~/.local/bin). A link never installs anything: install spandeck there, {then}"
            ),
            true,
        );
    }
    if all.contains("speaks version") {
        return (format!("{why}, {then}"), true);
    }
    // The helper ran, and the station there would not have it.
    if all.contains("would not take the link") {
        return (format!("{last}, {then}"), true);
    }
    // The carrier ended and the far side never spoke: what the carrier
    // itself said is the reason (a host that cannot be reached, say).
    let program = cmd.first().map(String::as_str).unwrap_or("the carrier");
    let said = match (last.is_empty(), why.contains("said nothing")) {
        (true, _) => why.to_string(),
        (false, true) => format!("`{program}` ended before the far side answered: {last}"),
        (false, false) => format!("{why} ({last})"),
    };
    (said, false)
}

impl Near {
    fn set(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.status.lock().unwrap());
    }

    fn state(&self, state: &'static str, said: &str) {
        self.set(|s| {
            if s.state != state {
                s.state = state;
                s.since_ms = now_ms();
            }
            s.said = said.to_string();
            if state != UP {
                s.rtt_ms = None;
                s.pid = None;
            }
        });
    }

    /// No carrier is to run now: the owner holds the link closed, or this
    /// program image is about to be replaced.
    fn parked(&self, d: &Daemon) -> bool {
        self.held.load(Ordering::SeqCst) || d.link.stopping.load(Ordering::SeqCst)
    }

    fn save(&self, f: impl FnOnce(&mut Saved)) {
        let mut saved = load(&self.file);
        f(&mut saved);
        saved.port = self.port;
        let text = serde_json::to_string(&saved).unwrap_or_default();
        if let Err(e) = spandeck_terminal::sys::write_private(&self.file, text.as_bytes()) {
            tracing::warn!(link = %self.name, error = %e, "could not keep a link's file");
        }
    }

    /// One row of `link.list`.
    pub fn view(&self) -> Value {
        let s = self.status.lock().unwrap().clone();
        json!({
            "name": self.name,
            "port": self.port,
            "carrier": typed(&self.cmd),
            "state": s.state,
            "since_ms": s.since_ms,
            "station": s.station,
            "version": s.version,
            "os": s.os,
            "rtt_ms": s.rtt_ms,
            "pid": s.pid,
            "said": s.said,
            "port_trouble": s.port_trouble,
            "now_ms": now_ms(),
        })
    }

    pub(crate) fn hold(&self, d: &Daemon, held: bool) {
        self.held.store(held, Ordering::SeqCst);
        self.save(|s| s.held = held);
        d.record(
            if held { "link.held" } else { "link.released" },
            None,
            None,
            None,
            Some("you"),
            json!({"link": self.name}),
        );
        // A permit is kept, so the task hears this wherever it is.
        self.kick.notify_one();
    }

    /// Ask the far station, through the link, to sign this machine's
    /// browser in. What comes back is good once and for two minutes, and
    /// is not a token: the page exchanges it with its own station.
    pub(crate) async fn open(&self, d: &Daemon) -> Result<Value> {
        let s = self.status.lock().unwrap().clone();
        let conn = self.conn.lock().unwrap().clone();
        let Some(conn) = conn else {
            return Err(invalid(match s.said.is_empty() {
                true => format!("{} is not connected ({})", self.name, s.state),
                false => format!("{} is not connected: {}", self.name, s.said),
            }));
        };
        let asked = async {
            let (mut read, mut write) = conn.open(kind::SIGN)?.split();
            let ask = json!({
                "name": format!("{} (over a link)", crate::paths::hostname()),
                "device": load(&self.file).device,
            });
            write
                .write_all(ask.to_string().as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            write.finish();
            let answer = read.read_all(4096).await.map_err(|e| e.to_string())?;
            serde_json::from_slice::<Value>(&answer).map_err(|e| e.to_string())
        };
        let answer = match tokio::time::timeout(Duration::from_secs(15), asked).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(invalid(format!("{} did not answer: {e}", self.name))),
            Err(_) => {
                return Err(invalid(format!(
                    "{} did not answer within 15 seconds",
                    self.name
                )))
            }
        };
        if let Some(e) = answer["error"].as_str() {
            return Err(invalid(format!("{} refused: {}", self.name, clean(e, 200))));
        }
        // What the far side sent is put in an address: only what an id and
        // a code are made of is taken.
        let plain = |v: &Value| {
            v.as_str()
                .filter(|t| {
                    (8..=80).contains(&t.len())
                        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                })
                .map(String::from)
        };
        let (Some(id), Some(collect)) = (plain(&answer["id"]), plain(&answer["collect"])) else {
            return Err(invalid(format!(
                "{} answered with something that is not a sign-in",
                self.name
            )));
        };
        self.save(|s| s.device = Some(id.clone()));
        d.record(
            "link.signin",
            None,
            None,
            None,
            Some("you"),
            json!({"link": self.name, "station": s.station, "device": id,
                "again": answer["reused"] == json!(true)}),
        );
        Ok(json!({
            "name": self.name,
            "station": s.station,
            "url": format!("http://localhost:{}/#signin={id}.{collect}", self.port),
            "again": answer["reused"] == json!(true),
        }))
    }
}

impl Daemon {
    /// Start a task for each `[[link]]`. Called only when there is one.
    pub(crate) fn link_start(&self) {
        let dir = self.paths.state_dir.join("links");
        let _ = std::fs::create_dir_all(&dir);
        let _ = spandeck_terminal::sys::private_dir(&dir);
        let mut links: Vec<Arc<Near>> = vec![];
        for cfg in &self.cfg.link {
            let trouble = self.link_trouble(cfg, &links);
            let file = dir.join(format!(
                "{}.json",
                if plain_name(&cfg.name) {
                    cfg.name.as_str()
                } else {
                    "unnamed"
                }
            ));
            let saved = load(&file);
            let taken: Vec<u16> = links.iter().map(|l| l.port).collect();
            let port = match (trouble.is_some(), cfg.port, saved.port) {
                (true, _, _) => 0,
                (_, Some(p), _) => p,
                (_, None, p) if p != 0 && !taken.contains(&p) => p,
                _ => (FIRST_PORT..FIRST_PORT + 200)
                    .find(|p| {
                        !taken.contains(p)
                            && *p != self.cfg.web_port
                            && std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, *p)).is_ok()
                    })
                    .unwrap_or(0),
            };
            let trouble = trouble
                .or_else(|| (port == 0).then(|| "no port on 127.0.0.1 is free for it".to_string()));
            let link = Arc::new(Near {
                name: clean(&cfg.name, 32),
                cmd: cfg.cmd.clone(),
                port,
                status: Mutex::new(Status {
                    state: if trouble.is_some() {
                        NOT_STARTED
                    } else {
                        CONNECTING
                    },
                    since_ms: now_ms(),
                    station: String::new(),
                    version: String::new(),
                    os: String::new(),
                    rtt_ms: None,
                    pid: None,
                    said: trouble.clone().unwrap_or_default(),
                    port_trouble: None,
                }),
                conn: Mutex::new(None),
                kick: Notify::new(),
                held: AtomicBool::new(saved.held),
                running: AtomicBool::new(false),
                file,
            });
            links.push(link.clone());
            match trouble {
                Some(why) => tracing::warn!(link = %link.name, %why, "a [[link]] was not started"),
                None => {
                    link.save(|_| {});
                    tokio::spawn(listen(link.clone()));
                    tokio::spawn(run(self.clone(), link));
                }
            }
        }
        *self.link.near.lock().unwrap() = links;
    }

    /// Why a `[[link]]` cannot be started as written, if it cannot.
    fn link_trouble(&self, cfg: &LinkConfig, before: &[Arc<Near>]) -> Option<String> {
        if !plain_name(&cfg.name) {
            return Some("its name must be 1 to 32 letters, digits, `-`, `_` or `.`".into());
        }
        if before.iter().any(|l| l.name == cfg.name) {
            return Some("another [[link]] has the same name".into());
        }
        if cfg.cmd.is_empty() || cfg.cmd[0].is_empty() {
            return Some("it has no cmd: the command that reaches the far side".into());
        }
        if cfg.port == Some(0) || cfg.port == Some(self.cfg.web_port) {
            return Some("its port is this station's own, or 0".into());
        }
        if cfg.port.is_some() && before.iter().any(|l| Some(l.port) == cfg.port) {
            return Some("another [[link]] has the same port".into());
        }
        None
    }

    /// End every carrier and wait until each is gone (two seconds at
    /// most), before this program image is replaced: the next image starts
    /// its own, and none is left behind as a process nobody collects.
    pub fn link_stop_all(&self) {
        let links = self.link.near.lock().unwrap().clone();
        if links.is_empty() {
            return;
        }
        self.link.stopping.store(true, Ordering::SeqCst);
        for link in &links {
            link.kick.notify_one();
        }
        let began = Instant::now();
        while began.elapsed() < Duration::from_secs(2)
            && links.iter().any(|l| l.running.load(Ordering::SeqCst))
        {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The image was not replaced after all: the links are opened again.
    pub fn link_resume(&self) {
        if self.link.stopping.swap(false, Ordering::SeqCst) {
            for link in self.link.near.lock().unwrap().iter() {
                link.kick.notify_one();
            }
        }
    }
}

/// The local port: every connection is one stream to the far station's
/// web listener, or, while the link is down, a short answer that says so.
async fn listen(link: Arc<Near>) {
    let listener = loop {
        match tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, link.port)).await {
            Ok(l) => break l,
            Err(e) => {
                let why = format!(
                    "port {} on this machine could not be opened ({e}); it is tried again by itself",
                    link.port
                );
                link.set(|s| s.port_trouble = Some(why));
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };
    link.set(|s| s.port_trouble = None);
    loop {
        let tcp = match listener.accept().await {
            Ok((tcp, _)) => tcp,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let _ = tcp.set_nodelay(true);
        let conn = link.conn.lock().unwrap().clone();
        let link = link.clone();
        tokio::spawn(async move {
            match conn.map(|c| c.open(kind::WEB)) {
                Some(Ok(stream)) => super::conn::splice(tcp, stream).await,
                _ => answer_down(tcp, &link).await,
            }
        });
    }
}

/// What a browser is told while the link is down: at once, never a hang.
/// Plain text, and nothing in it came from the far side.
async fn answer_down(mut tcp: tokio::net::TcpStream, link: &Near) {
    // The request is read first (its head, briefly): a connection closed
    // with a request unread is reset, and the browser shows nothing.
    let mut head = vec![0u8; 8192];
    let mut have = 0;
    let _ = tokio::time::timeout(Duration::from_millis(500), async {
        while have < head.len() && !head[..have].windows(4).any(|w| w == b"\r\n\r\n") {
            match tcp.read(&mut head[have..]).await {
                Ok(n) if n > 0 => have += n,
                _ => break,
            }
        }
    })
    .await;
    let s = link.status.lock().unwrap().clone();
    let since = crate::restore::clock_utc(s.since_ms);
    let body = match s.state {
        HELD => format!(
            "The link to {} is held closed since {since}.\n`spandeck link up {}` on this machine opens it again.\n",
            link.name, link.name
        ),
        NEEDS_YOU => format!(
            "{} is not connected since {since}, and it needs you.\n`spandeck link list` on this machine says what to do.\n",
            link.name
        ),
        _ => format!(
            "{} is not answering over its link since {since}.\nspandeck keeps trying by itself, and this page asks again every few seconds.\n`spandeck link list` on this machine says more.\n",
            link.name
        ),
    };
    let answer = format!(
        "HTTP/1.1 503 Service Unavailable\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\ncache-control: no-store\r\nx-content-type-options: nosniff\r\nretry-after: 3\r\nrefresh: 3\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tcp.write_all(answer.as_bytes()).await;
    let _ = tcp.shutdown().await;
}

enum Ended {
    /// It was up, and then it was not.
    WasUp {
        for_ms: u64,
    },
    /// It never said hello. `needs_you`: trying again cannot help.
    Never {
        needs_you: bool,
    },
    Held,
}

/// Keep one link's carrier running, for as long as the station runs.
async fn run(d: Daemon, link: Arc<Near>) {
    let mut attempt: u32 = 0;
    loop {
        if link.parked(&d) {
            if link.held.load(Ordering::SeqCst) {
                link.state(HELD, "");
            }
            link.kick.notified().await;
            attempt = 0;
            continue;
        }
        link.running.store(true, Ordering::SeqCst);
        let ended = once(&d, &link).await;
        link.running.store(false, Ordering::SeqCst);
        let wait = match ended {
            Ended::Held => continue,
            // A link that held for a while is tried again at once; one
            // that comes up and falls straight down is not hammered.
            Ended::WasUp { for_ms } if for_ms >= 10_000 => {
                attempt = 1;
                Duration::ZERO
            }
            Ended::WasUp { .. } | Ended::Never { needs_you: false } => {
                attempt += 1;
                retry_in(attempt - 1)
            }
            Ended::Never { needs_you: true } => {
                attempt = 0;
                RETRY_NEEDS_YOU
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = link.kick.notified() => attempt = 0,
        }
    }
}

/// What a carrier's standard error said last: lines for a person, kept
/// short, and never anything but text.
fn stderr_lines(
    stderr: tokio::process::ChildStderr,
) -> (Arc<Mutex<VecDeque<String>>>, tokio::task::JoinHandle<()>) {
    let lines: Arc<Mutex<VecDeque<String>>> = Arc::default();
    let into = lines.clone();
    let task = tokio::spawn(async move {
        let mut read = tokio::io::BufReader::new(stderr.take(64 * 1024)).lines();
        while let Ok(Some(line)) = read.next_line().await {
            let line = clean(&line, 200);
            if line.is_empty() {
                continue;
            }
            let mut lines = into.lock().unwrap();
            if lines.len() >= 20 {
                lines.pop_front();
            }
            lines.push_back(line);
        }
    });
    (lines, task)
}

/// One run of the carrier, from starting it to its end.
async fn once(d: &Daemon, link: &Arc<Near>) -> Ended {
    let argv = carrier_argv(&link.cmd);
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    // The carrier is not this station's program: it is told nothing of
    // this station's folders, its token or a pane.
    for name in [
        "SPANDECK_STATE_DIR",
        "SPANDECK_CONFIG_DIR",
        "SPANDECK_TOKEN",
        "SPANDECK_PANE",
        "SPANDECK_SOCKET",
    ] {
        cmd.env_remove(name);
    }
    spandeck_terminal::sys::no_window(&mut cmd);
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let failed = |link: &Near, said: String, needs_you: bool| {
        link.state(if needs_you { NEEDS_YOU } else { NOT_ANSWERING }, &said);
        Ended::Never { needs_you }
    };
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let said = format!("`{}` could not be started on this machine: {e}", argv[0]);
            return failed(link, said, e.kind() == std::io::ErrorKind::NotFound);
        }
    };
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return failed(link, "the carrier gave no pipes".into(), false);
    };
    let (said_lines, said_task) = stderr_lines(stderr);
    let (conn, _never) = Conn::start(Mux::near(), stdout, stdin);
    conn.say_hello(json!({
        "station": d.station,
        "version": crate::VERSION,
        "port": link.port,
    }));
    // Wait for the far side's hello; the owner may hold the link meanwhile.
    let deadline = tokio::time::Instant::now() + HELLO_WAIT;
    let hello = loop {
        tokio::select! {
            h = conn.hello() => break h,
            _ = tokio::time::sleep_until(deadline) => {
                break Err(format!("no answer in {} seconds", HELLO_WAIT.as_secs()))
            }
            _ = link.kick.notified() => {
                if link.parked(d) {
                    conn.close("held");
                    let _ = child.kill().await;
                    return Ended::Held;
                }
            }
        }
    };
    let hello = match hello {
        Ok(h) => h,
        Err(why) => {
            conn.close(&why);
            // It has usually ended by itself: how, and what it said.
            let code = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(status)) => status.code(),
                _ => {
                    let _ = child.kill().await;
                    None
                }
            };
            let _ = tokio::time::timeout(Duration::from_millis(500), said_task).await;
            let lines: Vec<String> = said_lines.lock().unwrap().iter().cloned().collect();
            let (said, needs_you) = why_not(&link.name, &link.cmd, &why, code, &lines);
            // Said once in the log, not at every try.
            if link.status.lock().unwrap().said != said {
                tracing::warn!(link = %link.name, %said, "a link did not come up");
            }
            return failed(link, said, needs_you);
        }
    };
    // Up. What the far side says of itself is text, cleaned and short.
    let text = |key: &str| clean(hello[key].as_str().unwrap_or(""), 64);
    let (station, version) = (text("station"), text("version"));
    let began = Instant::now();
    *link.conn.lock().unwrap() = Some(conn.clone());
    link.set(|s| {
        s.state = UP;
        s.since_ms = now_ms();
        s.said.clear();
        s.station = station.clone();
        s.version = version.clone();
        s.os = text("os");
        s.pid = child.id();
    });
    let carrier = typed(&link.cmd);
    d.record(
        "link.up",
        None,
        None,
        None,
        None,
        json!({"link": link.name, "carrier": carrier, "over": if is_ssh(&argv[0]) { "ssh" } else { "exec" },
            "station": station, "version": version, "port": link.port}),
    );
    let why = beat(d, link, &conn, &mut child).await;
    *link.conn.lock().unwrap() = None;
    let held = link.parked(d);
    conn.close(&why);
    let _ = child.kill().await;
    said_task.abort();
    let last = said_lines.lock().unwrap().back().cloned();
    let said = match last {
        Some(last) if !held => format!("{why} ({last})"),
        _ => why,
    };
    link.state(
        if link.held.load(Ordering::SeqCst) {
            HELD
        } else {
            NOT_ANSWERING
        },
        &said,
    );
    d.record(
        "link.down",
        None,
        None,
        None,
        None,
        json!({"link": link.name, "carrier": carrier, "station": station, "version": version,
            "why": said, "refused": conn.refused()}),
    );
    match held {
        true => Ended::Held,
        false => Ended::WasUp {
            for_ms: began.elapsed().as_millis() as u64,
        },
    }
}

/// While the link is up: a ping every two seconds on a stream of its own,
/// and the round trip of each. Returns why the link is no longer up.
async fn beat(
    d: &Daemon,
    link: &Arc<Near>,
    conn: &Conn,
    child: &mut tokio::process::Child,
) -> String {
    let Ok(stream) = conn.open(kind::PING) else {
        return "the link ended as it began".into();
    };
    let (mut read, mut write) = stream.split();
    let mut tick = tokio::time::interval(PING_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut seq: u64 = 0;
    let mut sent: VecDeque<(u64, Instant)> = VecDeque::new();
    let mut answered = Instant::now();
    let mut partial: Vec<u8> = vec![];
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until((answered + PING_DEAD).into()) => {
                return format!("no answer to a ping for {} seconds", PING_DEAD.as_secs());
            }
            _ = tick.tick() => {
                seq += 1;
                sent.push_back((seq, Instant::now()));
                if sent.len() > 8 {
                    sent.pop_front();
                }
                // Eight bytes into a window of a quarter of a million:
                // this does not wait unless the far side takes nothing,
                // and then the limit above ends the link.
                let wrote = tokio::time::timeout(PING_EVERY, write.write_all(&seq.to_be_bytes())).await;
                if matches!(wrote, Ok(Err(_))) {
                    return conn.ended_now().unwrap_or_else(|| GONE.into());
                }
            }
            got = read.read() => {
                let Ok(Some(bytes)) = got else {
                    return conn.ended_now().unwrap_or_else(|| GONE.into());
                };
                read.used(bytes.len());
                partial.extend(bytes);
                while partial.len() >= 8 {
                    let back = u64::from_be_bytes(partial[..8].try_into().unwrap_or([0; 8]));
                    partial.drain(..8);
                    if let Some((_, at)) = sent.iter().find(|(n, _)| *n == back) {
                        answered = Instant::now();
                        let rtt = at.elapsed().as_millis() as u64;
                        link.set(|s| s.rtt_ms = Some(rtt));
                    }
                }
            }
            why = conn.ended() => return why,
            status = child.wait() => {
                return match status {
                    Ok(s) => format!("the carrier ended ({s})"),
                    Err(e) => format!("the carrier ended ({e})"),
                };
            }
            _ = link.kick.notified() => {
                if link.held.load(Ordering::SeqCst) {
                    return "held closed by you".into();
                }
                if link.parked(d) {
                    return "this station is being replaced by a newer spandeck".into();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "When the carrier fails because SSH wants the owner, stop
    /// hammering ... and say exactly what to run", and "if the far side
    /// has no spandeck or an older one without link, say so plainly". The
    /// lines are OpenSSH's, a shell's and clap's own.
    #[test]
    fn what_ssh_and_the_far_side_say_becomes_what_to_do() {
        let cmd = vec!["ssh".to_string(), "me@devbox".to_string()];
        let said = |code, lines: &[&str]| {
            let lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            why_not(
                "devbox",
                &cmd,
                "the other end said nothing a link begins with",
                code,
                &lines,
            )
        };
        let (text, needs_you) = said(Some(255), &["me@devbox: Permission denied (publickey)."]);
        assert!(needs_you);
        assert!(
            text.contains(
                "In a terminal run `ssh me@devbox true`, sign in, then `spandeck link up devbox`"
            ),
            "{text}"
        );
        let (text, needs_you) = said(Some(255), &["Host key verification failed."]);
        assert!(
            needs_you && text.contains("spandeck accepts no host key for you"),
            "{text}"
        );
        let (text, needs_you) = said(Some(127), &["sh: 1: exec: spandeck: not found"]);
        assert!(
            needs_you && text.contains("A link never installs anything"),
            "{text}"
        );
        let (text, needs_you) = said(Some(2), &["error: unrecognized subcommand 'link'"]);
        assert!(needs_you && text.contains("from before links"), "{text}");
        // A network that is down is tried again by itself.
        let (text, needs_you) = said(
            Some(255),
            &["ssh: connect to host devbox port 22: Connection refused"],
        );
        assert!(!needs_you, "{text}");
        assert_eq!(
            text,
            "`ssh` ended before the far side answered: ssh: connect to host devbox port 22: Connection refused"
        );
    }

    /// "Reconnect: at once, then 100 ms doubling to 5 s with jitter."
    #[test]
    fn trying_again_is_at_once_then_doubles_to_five_seconds() {
        assert_eq!(retry_in(0), Duration::ZERO);
        let within = |n, ms: u64| {
            let d = retry_in(n).as_millis() as u64;
            assert!(d >= ms * 7 / 10 && d <= ms * 13 / 10 + 1, "{n}: {d} ms");
        };
        within(1, 100);
        within(2, 200);
        within(5, 1600);
        within(7, 5000);
        within(40, 5000);
    }
}
