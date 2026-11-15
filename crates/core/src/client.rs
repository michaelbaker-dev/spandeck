//! A small blocking client for the local socket. Used by the CLI, by hooks
//! (which must start fast), by an agent's tools (`spandeck mcp`) and by
//! tests.
//!
//! A client made with `waiting` does not fail because the daemon is being
//! upgraded or started again: it tries again within its budget. It makes a
//! random request id once per call and sends it with every try, so that a
//! change whose answer was lost is not carried out a second time
//! (rids.rs): a second try of a change is only ever made by that id, and
//! only to a daemon that says it keeps ids.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spandeck_terminal::sys::local::{self, Conn};

use crate::paths::Paths;

pub struct Client {
    conn: Option<(BufReader<Conn>, Conn)>,
    socket: PathBuf,
    token: String,
    next_id: u64,
    timeout: Option<Duration>,
    wait: Option<Waiting>,
}

/// How long a call goes on trying while the daemon is away, and what is
/// said meanwhile.
struct Waiting {
    budget: Duration,
    notice: Option<(Duration, String)>,
}

#[derive(Debug)]
pub struct ClientError {
    pub code: String,
    pub message: String,
}

impl ClientError {
    /// The daemon could not be reached and nothing was sent to it: the
    /// call was not carried out.
    pub fn not_sent(&self) -> bool {
        self.code == "not_running"
    }

    /// A change was sent and no answer came, or the station could not
    /// tell: it may or may not have been carried out, and was not sent a
    /// second time.
    pub fn outcome_unknown(&self) -> bool {
        self.code == "outcome_unknown"
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ClientError {}

fn io_err(e: std::io::Error) -> ClientError {
    ClientError {
        code: "io".into(),
        message: e.to_string(),
    }
}

fn closed() -> ClientError {
    ClientError {
        code: "closed".into(),
        message: "spandeckd closed the connection".into(),
    }
}

fn not_running(socket: &Path, e: &dyn std::fmt::Display) -> ClientError {
    ClientError {
        code: "not_running".into(),
        message: format!(
            "spandeckd is not running ({}: {e}). Start it with `spandeck daemon`.",
            socket.display()
        ),
    }
}

/// Nothing listens, or the socket is not there yet: the daemon is away.
fn away(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
    )
}

/// How one try ended, when it did not end with a result.
enum Failed {
    /// The request could not be written: the daemon did not get it.
    NotSent(String),
    /// Written whole, and the connection ended before the answer.
    Lost(String),
    /// The daemon answered with an error, or something that is not a
    /// connection ending: not tried again.
    Told(ClientError),
}

/// How long an agent's hook waits for a daemon that is not there before it
/// gives up, silently as before.
pub const HOOK_WAIT: Duration = Duration::from_secs(3);
/// How long a command waits for a daemon that is away (`--no-wait`: not at
/// all), and after how long it says that it is waiting.
pub const CLI_WAIT: Duration = Duration::from_secs(5);
pub const CLI_NOTICE: Duration = Duration::from_secs(1);
/// How long an agent's tool call waits.
pub const TOOL_WAIT: Duration = Duration::from_secs(15);

impl Client {
    pub fn connect(socket: &Path, token: &str) -> Result<Client, ClientError> {
        Client::connect_waiting(socket, token, Duration::ZERO)
    }

    /// As `connect`, for an agent's hook: when the daemon is not there to
    /// connect to (nothing listens, or its socket is not there yet), try
    /// again after 25, 50, 100, 200, 400 and 800 ms and then every 500 ms,
    /// for `limit` at most. A daemon that starts again restores its panes
    /// before it listens, so a restored agent's first hook used to find
    /// nobody (49 of 50, measured) though the daemon answers within 0.2 s.
    /// Any other failure is not tried again.
    pub fn connect_waiting(
        socket: &Path,
        token: &str,
        limit: Duration,
    ) -> Result<Client, ClientError> {
        let began = Instant::now();
        let mut pause = Duration::from_millis(25);
        let mut client = Client::unconnected(socket, token);
        loop {
            let e = match client.open() {
                Ok(()) => return Ok(client),
                Err(e) => e,
            };
            let left = limit.saturating_sub(began.elapsed());
            if !away(&e) || left.is_zero() {
                return Err(not_running(socket, &e));
            }
            std::thread::sleep(pause.min(left));
            pause = if pause >= Duration::from_millis(800) {
                Duration::from_millis(500)
            } else {
                pause * 2
            };
        }
    }

    fn unconnected(socket: &Path, token: &str) -> Client {
        Client {
            conn: None,
            socket: socket.to_path_buf(),
            token: token.to_string(),
            next_id: 0,
            timeout: None,
            wait: None,
        }
    }

    /// A client whose calls wait for a daemon that is away, for `budget`
    /// at most per call, and are not carried out twice (see the top of
    /// this file). It connects at its first call.
    pub fn waiting(socket: &Path, token: &str, budget: Duration) -> Client {
        let mut client = Client::unconnected(socket, token);
        client.wait = Some(Waiting {
            budget,
            notice: None,
        });
        client
    }

    /// Say `text` on standard error, once per call, when a call has waited
    /// this long for the daemon.
    pub fn notice(mut self, after: Duration, text: &str) -> Client {
        if let Some(w) = self.wait.as_mut() {
            w.notice = Some((after, text.to_string()));
        }
        self
    }

    fn env_token(paths: &Paths) -> Result<String, ClientError> {
        match std::env::var("SPANDECK_TOKEN") {
            Ok(t) if !t.is_empty() => Ok(t),
            _ => std::fs::read_to_string(paths.token_file())
                .map(|t| t.trim().to_string())
                .map_err(|_| ClientError { code: "not_running".into(), message: "spandeckd has not run on this station yet. Start it with `spandeck daemon`.".into() }),
        }
    }

    /// Connect as whoever the environment says we are: a pane (its own token)
    /// or the user (the client token file).
    pub fn from_env(paths: &Paths) -> Result<Client, ClientError> {
        Client::from_env_waiting(paths, Duration::ZERO)
    }

    /// `from_env`, waiting up to `limit` for a daemon that is not there
    /// (see `connect_waiting`).
    pub fn from_env_waiting(paths: &Paths, limit: Duration) -> Result<Client, ClientError> {
        Client::connect_waiting(&paths.socket(), &Client::env_token(paths)?, limit)
    }

    /// `waiting`, as whoever the environment says we are.
    pub fn from_env_retrying(paths: &Paths, budget: Duration) -> Result<Client, ClientError> {
        Ok(Client::waiting(
            &paths.socket(),
            &Client::env_token(paths)?,
            budget,
        ))
    }

    pub fn set_timeout(&mut self, d: Option<Duration>) {
        self.timeout = d;
        if let Some((reader, _)) = &self.conn {
            let _ = reader.get_ref().set_read_timeout(d);
        }
    }

    fn open(&mut self) -> std::io::Result<()> {
        let stream = local::connect(&self.socket)?;
        let writer = stream.try_clone()?;
        let _ = stream.set_read_timeout(self.timeout);
        self.conn = Some((BufReader::new(stream), writer));
        Ok(())
    }

    /// One request on the connection, and its answer.
    fn send(
        &mut self,
        method: &str,
        params: Value,
        rid: Option<&str>,
        again_ms: Option<u64>,
    ) -> Result<Value, Failed> {
        self.next_id += 1;
        let mut req =
            json!({"id": self.next_id, "token": self.token, "method": method, "params": params});
        if let Some(rid) = rid {
            req["rid"] = json!(rid);
            if let Some(ms) = again_ms {
                req["again_ms"] = json!(ms);
            }
        }
        let Some((reader, writer)) = self.conn.as_mut() else {
            return Err(Failed::NotSent("not connected".into()));
        };
        // A request is one line, and is read as one: a write that failed
        // part of the way left no line for the daemon to act on.
        if let Err(e) = writer.write_all(format!("{req}\n").as_bytes()) {
            self.conn = None;
            return Err(Failed::NotSent(e.to_string()));
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                self.conn = None;
                return Err(Failed::Lost("spandeckd closed the connection".into()));
            }
            Ok(_) => {}
            // This client's own limit on waiting for an answer.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                self.conn = None;
                return Err(Failed::Told(io_err(e)));
            }
            Err(e) => {
                self.conn = None;
                return Err(Failed::Lost(e.to_string()));
            }
        }
        let resp: Value = serde_json::from_str(&line).map_err(|e| {
            Failed::Told(ClientError {
                code: "protocol".into(),
                message: e.to_string(),
            })
        })?;
        if let Some(err) = resp.get("error") {
            let text = |key: &str| {
                err.get(key)
                    .and_then(|c| c.as_str())
                    .unwrap_or("error")
                    .to_string()
            };
            return Err(Failed::Told(ClientError {
                code: text("code"),
                message: text("message"),
            }));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// One try, as this client always made it: for a client that does not
    /// wait.
    fn call_once(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        if self.conn.is_none() {
            return Err(closed());
        }
        self.send(method, params, None, None).map_err(|f| match f {
            Failed::NotSent(e) => ClientError {
                code: "io".into(),
                message: e,
            },
            Failed::Lost(_) => closed(),
            Failed::Told(e) => e,
        })
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        if self.wait.is_none() {
            return self.call_once(method, params);
        }
        self.call_with(method, &mut || params.clone())
    }

    /// `call`, with the parameters made anew for each try (a wait that is
    /// asked again asks for the time it has left).
    ///
    /// When the daemon is away (nothing listens, there is no socket, or
    /// the connection ends before the answer) the call is made again after
    /// 25, 50, 100, 200, 400 and 800 ms and then every second, for this
    /// client's budget of waiting at most, with the same request id.
    ///
    /// - A request that was never written is simply sent when the daemon
    ///   is back.
    /// - One that was written and not answered: a call that only reads is
    ///   sent again. A change is sent again only by its id, and only to a
    ///   daemon that says it keeps ids (`caps` has `rid`), which carries
    ///   it out once and gives the first answer again, or says it cannot
    ///   tell. To a daemon that keeps none, a change is never sent twice.
    ///
    /// Errors: `not_running` when nothing was ever sent, `outcome_unknown`
    /// when a change was sent and may or may not have been carried out.
    pub fn call_with(
        &mut self,
        method: &str,
        params: &mut dyn FnMut() -> Value,
    ) -> Result<Value, ClientError> {
        let Some(wait) = &self.wait else {
            return self.call_once(method, params());
        };
        let (budget, notice) = (wait.budget, wait.notice.clone());
        let rid = crate::rids::new_rid();
        let began = Instant::now();
        let mut left = budget;
        let mut pause = Duration::from_millis(25);
        let mut told = false;
        // When the request was first written whole, if it ever was.
        let mut written: Option<Instant> = None;
        loop {
            let args = params();
            let change = !crate::rpc::repeatable(method, &args);
            let unknown = |why: &str| {
                ClientError {
                code: "outcome_unknown".into(),
                message: format!(
                    "{method}: spandeckd went away before it answered ({why}). This may or may not have been done, and was not sent again: check before repeating it."
                ),
            }
            };
            let why = 'attempt: {
                if self.conn.is_none() {
                    match self.open() {
                        Ok(()) => {}
                        Err(e) if away(&e) => break 'attempt e.to_string(),
                        Err(e) => return Err(not_running(&self.socket, &e)),
                    }
                }
                let again_ms = written.map(|t| t.elapsed().as_millis() as u64);
                if change && again_ms.is_some() {
                    // A second try of a change: only to a daemon that
                    // keeps ids, asked on the connection the try is made on.
                    match self.send("station.info", json!({}), None, None) {
                        Ok(info)
                            if info["caps"]
                                .as_array()
                                .is_some_and(|c| c.iter().any(|c| c == crate::rids::CAP)) => {}
                        Ok(_) => return Err(unknown("this spandeckd does not keep request ids")),
                        Err(Failed::NotSent(e) | Failed::Lost(e)) => break 'attempt e,
                        Err(Failed::Told(e)) => return Err(unknown(&e.message)),
                    }
                }
                let wrote = Instant::now();
                match self.send(method, args, Some(&rid), again_ms) {
                    Ok(v) => return Ok(v),
                    Err(Failed::Told(e)) => return Err(e),
                    Err(Failed::NotSent(e)) => e,
                    Err(Failed::Lost(e)) => {
                        written.get_or_insert(wrote);
                        e
                    }
                }
            };
            self.conn = None;
            if left.is_zero() {
                return Err(match (written, change) {
                    (Some(_), true) => unknown(&format!(
                        "and did not answer again within {} s",
                        budget.as_secs()
                    )),
                    _ => not_running(&self.socket, &why),
                });
            }
            let step = pause.min(left);
            match &notice {
                Some((after, text)) if !told && began.elapsed() + step >= *after => {
                    let before = after.saturating_sub(began.elapsed()).min(step);
                    std::thread::sleep(before);
                    eprintln!("{text}");
                    told = true;
                    std::thread::sleep(step - before);
                }
                _ => std::thread::sleep(step),
            }
            left -= step;
            pause = (pause * 2).min(Duration::from_secs(1));
        }
    }

    /// Subscribe to events; then call `next_event` repeatedly.
    pub fn subscribe(&mut self, kind_prefix: Option<&str>) -> Result<(), ClientError> {
        if self.conn.is_none() {
            self.open().map_err(|e| not_running(&self.socket, &e))?;
        }
        self.call_once("events.subscribe", json!({"kind": kind_prefix}))
            .map(|_| ())
    }

    pub fn next_event(&mut self) -> Result<Value, ClientError> {
        let Some((reader, _)) = self.conn.as_mut() else {
            return Err(closed());
        };
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(io_err)?;
        if n == 0 {
            return Err(closed());
        }
        serde_json::from_str(&line).map_err(|e| ClientError {
            code: "protocol".into(),
            message: e.to_string(),
        })
    }
}
