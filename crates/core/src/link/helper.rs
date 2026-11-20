//! `spandeck link --stdio`: the far side of a link. A short-lived program
//! the carrier runs (over the owner's `ssh`, say). It speaks the link on
//! its standard input and output, and writes anything meant for a person
//! on its standard error.
//!
//! It holds no authority of its own. It finds the station on this machine
//! as every `spandeck` command does, through the station's private local
//! socket and with the token only this user can read, and asks it two
//! things no agent, no other station and no browser may ask: where its
//! web listener really is on this machine, and to make a browser entry
//! for the near side. Each `web` stream is then one TCP connection to
//! that listener, bytes copied both ways.
//!
//! If no station is running here it starts one and says so: the account's
//! login service where there is one, else a station of its own, detached.
//! It never installs, updates or replaces anything.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::oneshot;

use super::conn::{splice, Conn, Incoming, Stream};
use super::mux::{clean, kind, Mux};
use crate::client::Client;
use crate::paths::Paths;

/// How long a station this helper started is given to answer.
const START_WAIT: Duration = Duration::from_secs(30);
/// How often the station is told this helper is still here, so that one
/// that was restarted takes the near side's address again.
const ATTACH_EVERY: Duration = Duration::from_secs(2);

type Ask = (String, Value, oneshot::Sender<Result<Value, String>>);

/// The station on this machine, asked from the helper's tasks. One
/// connection, kept for as long as the helper lives: when it closes, the
/// station forgets the near side's address.
#[derive(Clone)]
struct Station {
    tx: std::sync::mpsc::Sender<Ask>,
}

impl Station {
    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send((method.to_string(), params, tx))
            .map_err(|_| "the station here is gone".to_string())?;
        rx.await
            .unwrap_or_else(|_| Err("the station here is gone".into()))
    }
}

fn own_token(paths: &Paths) -> Option<String> {
    // The station's own token, from its file: never what the environment
    // says (in a pane that is the pane's).
    std::fs::read_to_string(paths.token_file())
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn connect(paths: &Paths) -> Option<Client> {
    let mut c = Client::connect(&paths.socket(), &own_token(paths)?).ok()?;
    c.set_timeout(Some(Duration::from_secs(20)));
    Some(c)
}

/// Where this account's station has a login service (`spandeck service
/// --install`: a systemd user unit, a launchd agent), that is what is
/// started: a station it keeps running is better than one nobody
/// watches. `false`: there is none, it is not this station's, or it would
/// not start; the helper then starts the station itself, as before.
fn start_login_service(paths: &Paths) -> bool {
    if spandeck_terminal::sys::WINDOWS || !paths.is_the_station(&crate::paths::home_dir()) {
        return false;
    }
    // The command `spandeck start` has another station run, less its last
    // resort.
    let line = "systemctl --user start spandeckd 2>/dev/null || launchctl kickstart gui/$(id -u)/dev.spandeck.spandeckd 2>/dev/null";
    let Ok(mut child) = std::process::Command::new("sh")
        .args(["-c", line])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    let began = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if began.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The station on this machine: the one running, or one started now.
fn station(paths: &Paths) -> Result<(Client, Value), String> {
    let answering = |paths: &Paths| {
        let mut c = connect(paths)?;
        let info = c.call("station.info", json!({})).ok()?;
        // Its web listener is up too.
        info["web"]
            .as_array()
            .is_some_and(|w| !w.is_empty())
            .then_some((c, info))
    };
    if let Some(found) = answering(paths) {
        return Ok(found);
    }
    // Nothing answers on the socket: is nothing running, or is a station
    // only just starting? One that has its socket open is waited for.
    if connect(paths).is_none() {
        if start_login_service(paths) {
            eprintln!(
                "spandeck link: no station was running on {}, so its login service was started.",
                crate::paths::hostname()
            );
        } else {
            let exe =
                std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("daemon");
            spandeck_terminal::sys::spawn_detached(&mut cmd).map_err(|e| {
                format!("no station is running here, and one could not be started: {e}")
            })?;
            eprintln!(
                "spandeck link: no station was running on {}, so one was started. It goes on running after this link closes.",
                crate::paths::hostname()
            );
        }
    }
    let began = Instant::now();
    while began.elapsed() < START_WAIT {
        if let Some(found) = answering(paths) {
            return Ok(found);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "the station here did not answer within {} seconds; its log is {}",
        START_WAIT.as_secs(),
        paths.log_file().display()
    ))
}

/// Carry the helper's questions to the station, one at a time, on one
/// connection. A station that went away (an upgrade, a restart) is waited
/// for and told of this helper again; one that does not come back ends
/// the helper, and with it the link.
fn talk(
    paths: Paths,
    mut client: Client,
    rx: std::sync::mpsc::Receiver<Ask>,
    web: Arc<Mutex<String>>,
) {
    let mut port: Option<Value> = None;
    let ask = |client: &mut Client, port: &Option<Value>, method: &str, params: Value| {
        for again in [false, true] {
            match client.call(method, params.clone()) {
                Ok(v) => return Ok(v),
                Err(e) if !again && matches!(e.code.as_str(), "closed" | "io" | "not_running") => {
                    let began = Instant::now();
                    let back = loop {
                        if let Some(c) = connect(&paths) {
                            break Some(c);
                        }
                        if began.elapsed() > START_WAIT {
                            break None;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    };
                    let Some(back) = back else {
                        eprintln!("spandeck link: the station here stopped answering");
                        std::process::exit(1);
                    };
                    *client = back;
                    if let Some(port) = port {
                        let _ = client.call("link.attach", json!({"port": port}));
                    }
                }
                Err(e) => return Err(e.message),
            }
        }
        Err("the station here did not answer".to_string())
    };
    loop {
        match rx.recv_timeout(ATTACH_EVERY) {
            Ok((method, params, reply)) => {
                let answer = ask(&mut client, &port, &method, params.clone());
                if method == "link.attach" && answer.is_ok() {
                    port = Some(params["port"].clone());
                }
                let _ = reply.send(answer);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if port.is_some() {
                    let params = json!({"port": port});
                    if let Ok(v) = ask(&mut client, &port, "link.attach", params) {
                        if let Some(addr) = v["web"].as_str() {
                            *web.lock().unwrap() = addr.to_string();
                        }
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The program. Returns its exit code.
pub fn main(paths: &Paths) -> i32 {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("spandeck link: {e}");
            return 1;
        }
    };
    let code = match runtime.block_on(run(paths.clone())) {
        Ok(()) => 0,
        Err(why) => {
            eprintln!("spandeck link: {why}");
            1
        }
    };
    // A read of standard input may still be waiting: it is not waited for.
    runtime.shutdown_background();
    code
}

async fn run(paths: Paths) -> Result<(), String> {
    let found = {
        let paths = paths.clone();
        tokio::task::spawn_blocking(move || station(&paths))
            .await
            .map_err(|e| e.to_string())??
    };
    let (client, info) = found;
    let web: Arc<Mutex<String>> = Arc::default();
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let (paths, web) = (paths.clone(), web.clone());
        std::thread::spawn(move || talk(paths, client, rx, web));
    }
    let station = Station { tx };

    let (conn, mut opened) = Conn::start(Mux::far(), tokio::io::stdin(), tokio::io::stdout());
    let near = tokio::time::timeout(Duration::from_secs(30), conn.hello())
        .await
        .map_err(|_| "the other end said nothing for 30 seconds".to_string())??;
    let port = near["port"]
        .as_u64()
        .filter(|p| (1..=65535).contains(p))
        .ok_or("the other end did not say which port its browser uses")?;
    let attached = station
        .call("link.attach", json!({"port": port}))
        .await
        .map_err(|e| {
            format!(
                "the station running here would not take the link ({}). If it is older than this spandeck, restart it",
                clean(&e, 200)
            )
        })?;
    *web.lock().unwrap() = attached["web"].as_str().unwrap_or_default().to_string();
    // Only now: the near side opens streams when it has this hello, and
    // by then the station answers to the near side's address.
    conn.say_hello(json!({
        "station": info["station"],
        "version": info["version"],
        "os": std::env::consts::OS,
    }));
    loop {
        tokio::select! {
            incoming = opened.recv() => {
                let Some(Incoming { kind, stream }) = incoming else {
                    return ended(conn.ended().await);
                };
                match kind {
                    kind::WEB => {
                        let addr = web.lock().unwrap().clone();
                        tokio::spawn(async move {
                            // A stream dropped here is ended for the near
                            // side: its browser's connection closes.
                            if let Ok(tcp) = tokio::net::TcpStream::connect(&addr).await {
                                let _ = tcp.set_nodelay(true);
                                splice(tcp, stream).await;
                            }
                        });
                    }
                    kind::PING => {
                        tokio::spawn(echo(stream));
                    }
                    kind::SIGN => {
                        tokio::spawn(sign(station.clone(), stream));
                    }
                    // The multiplexer takes no other kind.
                    _ => {}
                }
            }
            why = conn.ended() => return ended(why),
        }
    }
}

/// The carrier closing its end is how a link ordinarily ends.
fn ended(why: String) -> Result<(), String> {
    match why.as_str() {
        "the other end closed the link" => Ok(()),
        _ => Err(why),
    }
}

/// The heartbeat: what arrives is sent back.
async fn echo(stream: Stream) {
    let (mut read, mut write) = stream.split();
    while let Ok(Some(bytes)) = read.read().await {
        read.used(bytes.len());
        if write.write_all(&bytes).await.is_err() {
            return;
        }
    }
    write.finish();
}

/// One question about signing the near side's browser in, passed to the
/// station as this user, and its answer.
async fn sign(station: Station, stream: Stream) {
    let (mut read, mut write) = stream.split();
    let answer = match read.read_all(4096).await {
        Ok(ask) => match serde_json::from_slice::<Value>(&ask) {
            Ok(ask) => station
                .call(
                    "link.signin",
                    json!({"name": ask["name"], "device": ask["device"]}),
                )
                .await
                .unwrap_or_else(|e| json!({"error": e})),
            Err(_) => json!({"error": "the question could not be read"}),
        },
        Err(_) => return,
    };
    let _ = write.write_all(answer.to_string().as_bytes()).await;
    write.finish();
}
