//! A small blocking client for the local socket. Used by the CLI, by hooks
//! (which must start fast) and by tests.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use spandeck_terminal::sys::local::{self, Conn};

use crate::paths::Paths;

pub struct Client {
    reader: BufReader<Conn>,
    writer: Conn,
    token: String,
    next_id: u64,
}

#[derive(Debug)]
pub struct ClientError {
    pub code: String,
    pub message: String,
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

/// How long an agent's hook waits for a daemon that is not there before it
/// gives up, silently as before.
pub const HOOK_WAIT: Duration = Duration::from_secs(3);

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
        let began = std::time::Instant::now();
        let mut pause = Duration::from_millis(25);
        let stream = loop {
            let e = match local::connect(socket) {
                Ok(stream) => break stream,
                Err(e) => e,
            };
            let away = matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            );
            let left = limit.saturating_sub(began.elapsed());
            if !away || left.is_zero() {
                return Err(ClientError {
                    code: "not_running".into(),
                    message: format!(
                        "spandeckd is not running ({}: {e}). Start it with `spandeck daemon`.",
                        socket.display()
                    ),
                });
            }
            std::thread::sleep(pause.min(left));
            pause = if pause >= Duration::from_millis(800) {
                Duration::from_millis(500)
            } else {
                pause * 2
            };
        };
        let writer = stream.try_clone().map_err(io_err)?;
        Ok(Client {
            reader: BufReader::new(stream),
            writer,
            token: token.to_string(),
            next_id: 0,
        })
    }

    /// Connect as whoever the environment says we are: a pane (its own token)
    /// or the user (the client token file).
    pub fn from_env(paths: &Paths) -> Result<Client, ClientError> {
        Client::from_env_waiting(paths, Duration::ZERO)
    }

    /// `from_env`, waiting up to `limit` for a daemon that is not there
    /// (see `connect_waiting`).
    pub fn from_env_waiting(paths: &Paths, limit: Duration) -> Result<Client, ClientError> {
        let token = match std::env::var("SPANDECK_TOKEN") {
            Ok(t) if !t.is_empty() => t,
            _ => std::fs::read_to_string(paths.token_file())
                .map(|t| t.trim().to_string())
                .map_err(|_| ClientError { code: "not_running".into(), message: "spandeckd has not run on this station yet. Start it with `spandeck daemon`.".into() })?,
        };
        Client::connect_waiting(&paths.socket(), &token, limit)
    }

    pub fn set_timeout(&mut self, d: Option<Duration>) {
        let _ = self.reader.get_ref().set_read_timeout(d);
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        self.next_id += 1;
        let req =
            json!({"id": self.next_id, "token": self.token, "method": method, "params": params});
        self.writer
            .write_all(format!("{req}\n").as_bytes())
            .map_err(io_err)?;
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).map_err(io_err)?;
        if n == 0 {
            return Err(ClientError {
                code: "closed".into(),
                message: "spandeckd closed the connection".into(),
            });
        }
        let resp: Value = serde_json::from_str(&line).map_err(|e| ClientError {
            code: "protocol".into(),
            message: e.to_string(),
        })?;
        if let Some(err) = resp.get("error") {
            return Err(ClientError {
                code: err
                    .get("code")
                    .and_then(|c| c.as_str())
                    .unwrap_or("error")
                    .to_string(),
                message: err
                    .get("message")
                    .and_then(|c| c.as_str())
                    .unwrap_or("error")
                    .to_string(),
            });
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Subscribe to events; then call `next_event` repeatedly.
    pub fn subscribe(&mut self, kind_prefix: Option<&str>) -> Result<(), ClientError> {
        self.call("events.subscribe", json!({"kind": kind_prefix}))
            .map(|_| ())
    }

    pub fn next_event(&mut self) -> Result<Value, ClientError> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).map_err(io_err)?;
        if n == 0 {
            return Err(ClientError {
                code: "closed".into(),
                message: "spandeckd closed the connection".into(),
            });
        }
        serde_json::from_str(&line).map_err(|e| ClientError {
            code: "protocol".into(),
            message: e.to_string(),
        })
    }
}
