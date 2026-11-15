//! The local API: newline-delimited JSON over the local socket (a Unix
//! socket; a named pipe on Windows).
//!
//!   -> {"id":1,"token":"…","method":"pane.list","params":{}}
//!   <- {"id":1,"result":[…]}            or  {"id":1,"error":{"code":"…","message":"…"}}
//!
//! `events.subscribe` turns the connection into a stream of {"event":{…}}.
//! The stream is not durable: a slow subscriber gets {"events_lost":n} and
//! should re-read whatever state it shows.

use serde_json::{json, Value};
use spandeck_terminal::sys::local::{Listener, Stream};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast::error::RecvError;

use crate::daemon::{Daemon, Error};

pub fn error_json(e: &Error) -> Value {
    json!({"code": e.code(), "message": e.to_string()})
}

/// Open the local socket. Only this user can connect to it.
pub fn bind(daemon: &Daemon) -> std::io::Result<Listener> {
    Listener::bind(&daemon.paths.socket())
}

pub async fn serve(daemon: Daemon, mut listener: Listener) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let d = daemon.clone();
                tokio::spawn(async move {
                    if let Err(e) = connection(d, stream).await {
                        tracing::debug!(error = %e, "socket connection ended");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "socket accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn connection(daemon: Daemon, stream: Stream) -> std::io::Result<()> {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    let conn_id = crate::paths::new_id("c");
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let out = json!({"id": null, "error": {"code": "invalid", "message": format!("not JSON: {e}")}});
                write.write_all(format!("{out}\n").as_bytes()).await?;
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let token = req.get("token").and_then(|t| t.as_str()).unwrap_or("");
        let Some(caller) = daemon.caller_for_token(token, &conn_id) else {
            let out =
                json!({"id": id, "error": {"code": "denied", "message": "missing or wrong token"}});
            write.write_all(format!("{out}\n").as_bytes()).await?;
            continue;
        };
        if method == "events.subscribe" {
            if !caller.is_owner() {
                let out = json!({"id": id, "error": {"code": "denied", "message": "events are for the user's clients"}});
                write.write_all(format!("{out}\n").as_bytes()).await?;
                continue;
            }
            let mut rx = daemon.bus.subscribe();
            write
                .write_all(
                    format!("{}\n", json!({"id": id, "result": {"subscribed": true}})).as_bytes(),
                )
                .await?;
            let prefix = params
                .get("kind")
                .and_then(|k| k.as_str())
                .map(String::from);
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if prefix
                            .as_ref()
                            .is_some_and(|p| !ev.kind.starts_with(p.as_str()))
                        {
                            continue;
                        }
                        write
                            .write_all(format!("{}\n", json!({"event": ev})).as_bytes())
                            .await?;
                    }
                    Err(RecvError::Lagged(n)) => {
                        write
                            .write_all(format!("{}\n", json!({"events_lost": n})).as_bytes())
                            .await?;
                    }
                    Err(RecvError::Closed) => return Ok(()),
                }
            }
        }
        let out = match daemon
            .dispatch_timed(crate::perf::Kind::Local, None, &caller, &method, params)
            .await
        {
            Ok(result) => json!({"id": id, "result": result}),
            Err(e) => json!({"id": id, "error": error_json(&e)}),
        };
        write.write_all(format!("{out}\n").as_bytes()).await?;
    }
    Ok(())
}
