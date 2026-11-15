//! The web side of spandeckd: the Control Deck's assets, the HTTP and
//! WebSocket API, and the gateway to other stations.
//!
//! Who is calling is decided by the connection's real remote address:
//!   - loopback: needs a token (an agent on this machine can reach loopback);
//!   - a tailnet address that Tailscale says belongs to the same login: the user;
//!   - anything else: refused.
//!
//! Tailscale Serve identity headers are never read.
//!
//! Every API request also passes an Origin and Host check, because browsers
//! do not apply same-origin rules to WebSockets.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Response, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use spandeck_core::daemon::{Authority, Caller, Daemon};
use spandeck_core::fleet::Remote;
use spandeck_core::perf::Kind;
use spandeck_core::server::error_json;
use spandeck_core::trust::Signed;
use spandeck_terminal::PaneEvent;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;

#[derive(RustEmbed)]
#[folder = "assets"]
struct Assets;

#[derive(Clone)]
pub struct Web {
    daemon: Daemon,
    /// Host header values this server answers to.
    hosts: Arc<RwLock<Vec<String>>>,
}

const FRAME_OUTPUT: u8 = 1;
const FRAME_RESYNC: u8 = 2;

/// Bind the listeners and start serving. Returns the bound addresses.
pub async fn start(daemon: Daemon) -> anyhow::Result<Vec<SocketAddr>> {
    let port = daemon.cfg.web_port;
    let mut ips: Vec<IpAddr> = vec![];
    if daemon.cfg.web_bind.is_empty() {
        ips.push(IpAddr::from([127, 0, 0, 1]));
        let fleet_daemon = daemon.clone();
        let me = tokio::task::spawn_blocking(move || fleet_daemon.fleet.tailnet_self())
            .await
            .ok()
            .flatten();
        if let Some(me) = me {
            ips.extend(
                me.ips
                    .iter()
                    .filter_map(|i| i.parse::<IpAddr>().ok())
                    .filter(|i| i.is_ipv4()),
            );
        }
    } else {
        for b in &daemon.cfg.web_bind {
            ips.push(
                b.parse()
                    .map_err(|_| anyhow::anyhow!("web_bind: {b} is not an IP address"))?,
            );
        }
    }

    let web = Web {
        daemon: daemon.clone(),
        hosts: Arc::new(RwLock::new(vec![])),
    };
    let app = Router::new()
        .route("/api/rpc", post(http_rpc))
        .route("/api/peer/rpc", post(peer_rpc))
        .route("/api/device/request", post(device_request))
        .route("/api/device/collect", post(device_collect))
        .route("/api/ws", get(ws_upgrade))
        .route("/api/font/{id}", get(font))
        .fallback(get(asset))
        .layer(DefaultBodyLimit::max(100 << 20))
        .with_state(web.clone());

    // Bind first, learn the ports, fill in the Host allowlist, and only then
    // start answering: a request must never race the allowlist.
    let mut listeners = vec![];
    for ip in ips {
        match tokio::net::TcpListener::bind(SocketAddr::new(ip, port)).await {
            Ok(listener) => listeners.push(listener),
            Err(e) => tracing::warn!(%ip, port, error = %e, "could not listen"),
        }
    }
    if listeners.is_empty() {
        anyhow::bail!("could not listen on port {port} on any address");
    }
    let bound: Vec<SocketAddr> = listeners
        .iter()
        .filter_map(|l| l.local_addr().ok())
        .collect();

    let mut hosts = vec![];
    let dns = tokio::task::spawn_blocking(|| {
        spandeck_core::fleet::tailscale_status().and_then(|s| {
            s.pointer("/Self/DNSName")
                .and_then(|d| d.as_str())
                .map(|d| d.trim_end_matches('.').to_lowercase())
        })
    })
    .await
    .ok()
    .flatten();
    for addr in &bound {
        let p = addr.port();
        hosts.push(format!("{}:{p}", addr.ip()));
        for name in [
            "localhost".to_string(),
            daemon.station.to_lowercase(),
            spandeck_core::paths::hostname().to_lowercase(),
        ] {
            hosts.push(format!("{name}:{p}"));
        }
        if let Some(dns) = &dns {
            hosts.push(format!("{dns}:{p}"));
            // Behind `tailscale serve`, the browser sees the bare HTTPS name,
            // or the name with whichever port serve was given. The name
            // belongs to this machine alone, so any port of it is ours.
            hosts.push(dns.clone());
            hosts.push(format!("{dns}:*"));
        }
    }
    hosts.sort();
    hosts.dedup();
    *web.hosts.write().unwrap() = hosts;
    for listener in listeners {
        let app = app.clone();
        tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        });
    }
    *daemon.web_addrs.lock().unwrap() = bound.iter().map(|a| a.to_string()).collect();
    Ok(bound)
}

// ------------------------------------------------------------------ checks

/// Host must be one of ours; Origin, when a browser sends one, must match it.
fn origin_ok(web: &Web, headers: &HeaderMap) -> Result<(), (StatusCode, &'static str)> {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.to_lowercase());
    let Some(host) = host else {
        return Err((StatusCode::BAD_REQUEST, "missing Host"));
    };
    let any_port = format!(
        "{}:*",
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(&host)
    );
    let ours = {
        let hosts = web.hosts.read().unwrap();
        hosts.contains(&host) || hosts.contains(&any_port)
    };
    if !ours {
        return Err((StatusCode::FORBIDDEN, "this Host is not served here"));
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        let origin_host = origin.split("://").nth(1).unwrap_or("").to_lowercase();
        if origin_host != host {
            return Err((StatusCode::FORBIDDEN, "cross-origin requests are refused"));
        }
    }
    Ok(())
}

async fn identify(web: &Web, ip: IpAddr) -> Remote {
    if ip.is_loopback() {
        return Remote::Loopback;
    }
    let d = web.daemon.clone();
    tokio::task::spawn_blocking(move || d.fleet.identify(ip))
        .await
        .unwrap_or(Remote::Stranger(ip.to_string()))
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string())
}

fn client_id(headers: &HeaderMap, addr: &SocketAddr) -> String {
    headers
        .get("x-spandeck-client")
        .and_then(|h| h.to_str().ok())
        .filter(|c| {
            !c.is_empty()
                && c.len() <= 64
                && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        })
        .map(|c| c.to_string())
        .unwrap_or_else(|| format!("web-{}", addr.ip()))
}

/// The user (or an agent with its pane token) calling the API.
async fn caller(
    web: &Web,
    addr: &SocketAddr,
    headers: &HeaderMap,
    token: Option<String>,
) -> Result<Caller, (StatusCode, &'static str)> {
    let client = client_id(headers, addr);
    match identify(web, addr.ip()).await {
        // Tailscale says which device is connecting, not which program on
        // it. So every caller also proves itself: a station in this fleet
        // with a signature, a browser with a token the owner approved.
        Remote::Loopback | Remote::Owner { .. } => {
            if let Some(station) = gateway_station(web, headers) {
                return Ok(Caller {
                    authority: Authority::Owner,
                    pane: None,
                    client,
                    who: format!("you (via {station})"),
                });
            }
            let token = token.or_else(|| bearer(headers)).ok_or((
                StatusCode::UNAUTHORIZED,
                "sign in: this browser has no token for this station",
            ))?;
            web.daemon.caller_for_token(&token, &client).ok_or((
                StatusCode::UNAUTHORIZED,
                "sign in: this browser's token is not accepted here",
            ))
        }
        Remote::Stranger(_) => Err((
            StatusCode::FORBIDDEN,
            "this device is not under your Tailscale login",
        )),
    }
}

/// A station in this fleet relaying its own user's attach: it signs its name.
fn gateway_station(web: &Web, headers: &HeaderMap) -> Option<String> {
    let station = headers.get("x-spandeck-station")?.to_str().ok()?;
    let sig = headers.get("x-spandeck-sig")?.to_str().ok()?;
    web.daemon
        .trust
        .verify("ws", station.as_bytes(), sig)
        .then(|| station.to_string())
}

fn refuse(e: (StatusCode, &'static str)) -> axum::response::Response {
    (
        e.0,
        Json(json!({"error": {"code": "denied", "message": e.1}})),
    )
        .into_response()
}

// -------------------------------------------------------------------- HTTP

async fn http_rpc(
    State(web): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    if let Err(e) = origin_ok(&web, &headers) {
        return refuse(e);
    }
    let who = match caller(&web, &addr, &headers, None).await {
        Ok(c) => c,
        Err(e) => return refuse(e),
    };
    let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = body.get("params").cloned().unwrap_or(json!({}));
    match web
        .daemon
        .dispatch_timed(Kind::Http, None, &who, method, params)
        .await
    {
        Ok(result) => Json(json!({"result": result})).into_response(),
        Err(e) => {
            let status = match e.code() {
                "denied" => StatusCode::FORBIDDEN,
                "not_found" => StatusCode::NOT_FOUND,
                "invalid" => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({"error": error_json(&e)}))).into_response()
        }
    }
}

/// A browser on one of the owner's devices asks to be signed in. Nothing is
/// granted until the owner approves it at a station.
async fn device_request(
    State(web): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    device_call(&web, &addr, &headers, |ip| {
        web.daemon.trust_request(
            "device",
            body.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("a browser"),
            None,
            ip,
        )
    })
    .await
}

async fn device_collect(
    State(web): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let get = |k: &str| body.get(k).and_then(|v| v.as_str()).unwrap_or("");
    device_call(&web, &addr, &headers, |ip| {
        web.daemon.trust_collect(get("id"), get("collect"), ip)
    })
    .await
}

async fn device_call(
    web: &Web,
    addr: &SocketAddr,
    headers: &HeaderMap,
    f: impl FnOnce(&str) -> spandeck_core::daemon::Result<Value>,
) -> axum::response::Response {
    if let Err(e) = origin_ok(web, headers) {
        return refuse(e);
    }
    if matches!(identify(web, addr.ip()).await, Remote::Stranger(_)) {
        return refuse((
            StatusCode::FORBIDDEN,
            "this device is not under your Tailscale login",
        ));
    }
    match f(&addr.ip().to_string()) {
        Ok(result) => Json(json!({"result": result})).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": error_json(&e)})),
        )
            .into_response(),
    }
}

/// Another station calling. Only a device under the same login is a station.
async fn peer_rpc(
    State(web): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    raw: axum::body::Bytes,
) -> axum::response::Response {
    // A browser must never reach this endpoint.
    if headers.contains_key(header::ORIGIN) {
        return refuse((StatusCode::FORBIDDEN, "not for browsers"));
    }
    let Ok(body) = serde_json::from_slice::<Value>(&raw) else {
        return refuse((StatusCode::BAD_REQUEST, "not JSON"));
    };
    let from = body
        .get("from")
        .and_then(|f| f.as_str())
        .unwrap_or("?")
        .to_string();
    if matches!(identify(&web, addr.ip()).await, Remote::Stranger(_)) {
        return refuse((StatusCode::FORBIDDEN, "not a station under this login"));
    }
    let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = body.get("params").cloned().unwrap_or(json!({}));
    // What any device under the owner's login may ask without the fleet key:
    // who are you, and to be let in. Nothing is granted until the owner approves.
    let asker = addr.ip().to_string();
    let open = match method {
        "station.hello" => Some(Ok(json!({
            "station": web.daemon.station,
            "version": spandeck_core::VERSION,
            "protocol": spandeck_core::PROTOCOL,
            "fleet": web.daemon.trust.fleet_id(),
            // This station's clock, so that two stations whose clocks are
            // too far apart for signed calls can say that is why.
            "now_ms": spandeck_core::paths::now_ms(),
            "caps": web.daemon.caps(),
            // What it runs, for a station that is asked to connect to it.
            "os": std::env::consts::OS,
            // When it joins a fleet it brings the stations of its own.
            "merge": true,
        }))),
        "fleet.join.request" => Some(
            web.daemon.trust_request(
                "station",
                params
                    .get("station")
                    .and_then(|s| s.as_str())
                    .unwrap_or(&from),
                params.get("with"),
                &asker,
            ),
        ),
        "fleet.join.collect" => Some(web.daemon.trust_collect(
            params.get("id").and_then(|s| s.as_str()).unwrap_or(""),
            params.get("collect").and_then(|s| s.as_str()).unwrap_or(""),
            &asker,
        )),
        _ => None,
    };
    if let Some(out) = open {
        return match out {
            Ok(result) => Json(json!({"result": result})).into_response(),
            Err(e) => Json(json!({"error": error_json(&e)})).into_response(),
        };
    }
    // Everything else needs the fleet key, over the exact bytes received.
    let signed = headers
        .get("x-spandeck-sig")
        .and_then(|s| s.to_str().ok())
        .map_or(Signed::No, |sig| web.daemon.trust.check("rpc", &raw, sig));
    match signed {
        Signed::Yes => {}
        // Refused as before; the reason is said, to a holder of the key.
        Signed::Clock { ahead_ms } => {
            let words = spandeck_core::perf::clock_words(ahead_ms);
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": {"code": "clock", "clock_diff_ms": ahead_ms, "message": format!(
                    "the clocks of {from} and {} differ by {words}: a station's calls are taken only within 2 minutes. Set both clocks from the network; it then works again by itself",
                    web.daemon.station
                )}})),
            )
                .into_response();
        }
        Signed::No => {
            return refuse((
                StatusCode::UNAUTHORIZED,
                "not in this fleet: join it first, and approve the request on this station",
            ))
        }
    }
    let who = Caller {
        authority: Authority::Peer,
        pane: None,
        client: format!("station:{from}"),
        who: from,
    };
    match web
        .daemon
        .dispatch_timed(Kind::Station, None, &who, method, params)
        .await
    {
        Ok(result) => Json(json!({"result": result})).into_response(),
        Err(e) => Json(json!({"error": error_json(&e)})).into_response(),
    }
}

/// A font the owner uploaded, by its id, for the owner's browsers. The id is
/// looked up in the look document; the bytes are a file a browser asked to be
/// kept, so they go out as a font and as nothing a browser might guess.
async fn font(
    State(web): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> axum::response::Response {
    if let Err(e) = origin_ok(&web, &headers) {
        return refuse(e);
    }
    match caller(&web, &addr, &headers, None).await {
        Ok(who) if who.is_owner() => {}
        Ok(_) => return refuse((StatusCode::FORBIDDEN, "fonts are for the user's clients")),
        Err(e) => return refuse(e),
    }
    let (mime, bytes) = match web.daemon.look_font_file(&id) {
        Ok(found) => found,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": error_json(&e)})),
            )
                .into_response()
        }
    };
    let mut resp = Response::new(Body::from(bytes));
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    // The id is the file's own hash: what it names never changes.
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    h.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    resp
}

async fn asset(uri: Uri) -> Response<Body> {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            // Phones hold on to scripts hard. Each of our files is asked for
            // under a name that changes with every build, so a new build is
            // always fetched: the page names app.js?v=…, and each script
            // names the ones it imports the same way.
            let mut data = file.data.into_owned();
            let ours = !path.starts_with("vendor/");
            if ours && (path.ends_with(".html") || path.ends_with(".js")) {
                if let Ok(text) = String::from_utf8(data.clone()) {
                    let v = assets_build();
                    let text = if path.ends_with(".html") {
                        text.replace("src=\"app.js\"", &format!("src=\"app.js?v={v}\""))
                            .replace("href=\"app.css\"", &format!("href=\"app.css?v={v}\""))
                    } else {
                        text.lines()
                            .map(|l| {
                                if l.starts_with("import ") && l.trim_end().ends_with(".js';") {
                                    l.trim_end().trim_end_matches(".js';").to_string()
                                        + &format!(".js?v={v}';")
                                } else {
                                    l.to_string()
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    data = text.into_bytes();
                }
            }
            let mut resp = Response::new(Body::from(data));
            let h = resp.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime.as_ref())
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            );
            // The bundled symbols font is large and is asked for under a
            // name that has its version in it: a browser keeps it. Nothing
            // else is kept.
            h.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(if path.ends_with(".woff2") {
                    "public, max-age=31536000, immutable"
                } else {
                    "no-store, must-revalidate"
                }),
            );
            h.insert(
                "x-content-type-options",
                HeaderValue::from_static("nosniff"),
            );
            h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
            if path.ends_with(".html") {
                h.insert(
                    "content-security-policy",
                    HeaderValue::from_static(
                        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' ws: wss:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
                    ),
                );
            }
            resp
        }
        None => {
            let mut resp = Response::new(Body::from("not found"));
            *resp.status_mut() = StatusCode::NOT_FOUND;
            resp
        }
    }
}

// --------------------------------------------------------------- WebSocket

async fn ws_upgrade(
    State(web): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if let Err(e) = origin_ok(&web, &headers) {
        return refuse(e);
    }
    if matches!(identify(&web, addr.ip()).await, Remote::Stranger(_)) {
        return refuse((
            StatusCode::FORBIDDEN,
            "this device is not under your Tailscale login",
        ));
    }
    ws.max_message_size(100 << 20)
        .on_upgrade(move |socket| ws_session(web, addr, headers, socket))
}

enum Chan {
    Local {
        pane: String,
        control: bool,
        task: tokio::task::JoinHandle<()>,
    },
    Remote {
        tx: mpsc::Sender<String>,
        peer_chan: u64,
        control: bool,
        task: tokio::task::JoinHandle<()>,
    },
}

fn frame(kind: u8, chan: u32, payload: &[u8]) -> Message {
    let mut buf = Vec::with_capacity(payload.len() + 5);
    buf.push(kind);
    buf.extend_from_slice(&chan.to_le_bytes());
    buf.extend_from_slice(payload);
    Message::Binary(buf.into())
}

async fn ws_session(web: Web, addr: SocketAddr, headers: HeaderMap, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    // The token travels in the first message, never in a cookie or the URL.
    let first = match tokio::time::timeout(std::time::Duration::from_secs(10), stream.next()).await
    {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Value>(&t).unwrap_or(Value::Null),
        _ => return,
    };
    let auth = first.get("auth").cloned().unwrap_or(Value::Null);
    let token = auth.get("token").and_then(|t| t.as_str()).map(String::from);
    let mut who = match caller(&web, &addr, &headers, token).await {
        Ok(c) => c,
        Err((_, msg)) => {
            let _ = sink
                .send(Message::Text(
                    json!({"auth": {"ok": false, "message": msg}})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };
    if let Some(c) = auth
        .get("client")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty() && c.len() <= 64)
    {
        who.client = c
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
            .collect();
    }
    // Which of the owner's devices this is, as Tailscale names it: the deck
    // warns when the key of the device in the owner's hand is about to
    // expire. A device that opens a deck is one the fleet warns about.
    let device = if who.is_owner() && !who.who.starts_with("you (via ") {
        web.daemon.note_deck_device(addr.ip());
        web.daemon.tailnet_device_at(addr.ip())
    } else {
        None
    };
    let hello = json!({"auth": {"ok": true, "station": web.daemon.station, "version": spandeck_core::VERSION, "build": assets_build(), "client": who.client, "full": who.is_owner(), "device": device}});
    if sink
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Everything going to the browser funnels through one bounded queue. A
    // slow browser backs this up; pane streams then lag and resync from a
    // snapshot. The PTY never waits.
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(256);
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut chans: HashMap<u32, Chan> = HashMap::new();
    let mut next_chan: u32 = 1;
    let mut events_task: Option<tokio::task::JoinHandle<()>> = None;

    // A browser signed in from another device, or another station relaying
    // its user, was let in on the strength of the fleet key. When a station
    // is removed, or this station's fleet joins another one, the key is
    // replaced, and such a connection ends: what it
    // was let in with no longer counts. The token this station made for
    // its own Control Deck, and a pane's token, do not rest on the key.
    let rests_on_key = who.is_owner() && who.who != "you";
    let mut rekeyed = web.daemon.trust.rekeyed();
    let mut key_replaced = false;

    // Each request is stamped as it arrives, so that how long it then stood
    // behind the ones before it is known (the Performance page). They are
    // taken one at a time and in the order they came, as before; up to 64
    // wait here, and past that the socket is not read, as before.
    let (in_tx, mut in_rx) = mpsc::channel::<(std::time::Instant, Message)>(64);
    let reader = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            if in_tx.send((std::time::Instant::now(), msg)).await.is_err() {
                break;
            }
        }
    });

    loop {
        let msg = tokio::select! {
            m = in_rx.recv() => m,
            _ = rekeyed.changed(), if rests_on_key => {
                key_replaced = true;
                break;
            }
        };
        let Some((arrived, msg)) = msg else { break };
        let (waited, began) = (arrived.elapsed(), std::time::Instant::now());
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(req) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let chan_of = |p: &Value| p.get("chan").and_then(|c| c.as_u64()).map(|c| c as u32);

        let reply: Result<Value, Value> = match method.as_str() {
            "pane.attach" => {
                if !who.is_owner() {
                    Err(json!({"code": "denied", "message": "attaching is for the user's clients"}))
                } else {
                    let chan = next_chan;
                    next_chan += 1;
                    let pane = params
                        .get("pane")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string();
                    let control = params.get("mode").and_then(|m| m.as_str()) != Some("watch");
                    let station = params
                        .get("station")
                        .and_then(|s| s.as_str())
                        .filter(|s| *s != web.daemon.station)
                        .map(String::from);
                    match station {
                        None => match attach_local(&web.daemon, &pane, chan, out_tx.clone()) {
                            Ok((result, task)) => {
                                web.daemon.note_attach(
                                    result["pane"].as_str().unwrap_or(&pane),
                                    &who.who,
                                    &who.client,
                                    control,
                                );
                                chans.insert(
                                    chan,
                                    Chan::Local {
                                        pane: result["pane"].as_str().unwrap_or(&pane).to_string(),
                                        control,
                                        task,
                                    },
                                );
                                Ok(result)
                            }
                            Err(e) => Err(error_json(&e)),
                        },
                        Some(station) => match attach_remote(
                            &web.daemon,
                            &station,
                            &pane,
                            control,
                            &who.client,
                            chan,
                            out_tx.clone(),
                        )
                        .await
                        {
                            Ok((result, tx, peer_chan, task)) => {
                                chans.insert(
                                    chan,
                                    Chan::Remote {
                                        tx,
                                        peer_chan,
                                        control,
                                        task,
                                    },
                                );
                                Ok(result)
                            }
                            Err(e) => Err(json!({"code": "unreachable", "message": e})),
                        },
                    }
                }
            }
            "pane.detach" => {
                if let Some(c) = chan_of(&params).and_then(|c| chans.remove(&c)) {
                    match c {
                        Chan::Local { task, .. } | Chan::Remote { task, .. } => task.abort(),
                    }
                }
                Ok(json!({"ok": true}))
            }
            "pane.input" | "pane.resize" if chan_of(&params).is_some() => {
                match chan_of(&params).and_then(|c| chans.get(&c)) {
                    Some(Chan::Local { pane, control, .. }) => {
                        if !control {
                            Err(
                                json!({"code": "denied", "message": "watching only: take control to type"}),
                            )
                        } else {
                            let mut p = params.clone();
                            p["pane"] = json!(pane);
                            web.daemon
                                .dispatch(&who, &method, p)
                                .await
                                .map_err(|e| error_json(&e))
                        }
                    }
                    Some(Chan::Remote {
                        tx,
                        peer_chan,
                        control,
                        ..
                    }) => {
                        if !control {
                            Err(
                                json!({"code": "denied", "message": "watching only: take control to type"}),
                            )
                        } else {
                            let mut p = params.clone();
                            p["chan"] = json!(peer_chan);
                            let _ = tx
                                .send(
                                    json!({"id": null, "method": method, "params": p}).to_string(),
                                )
                                .await;
                            Ok(json!({"ok": true}))
                        }
                    }
                    None => Err(json!({"code": "not_found", "message": "no such channel"})),
                }
            }
            "events.subscribe" => {
                if !who.is_owner() {
                    Err(json!({"code": "denied", "message": "events are for the user's clients"}))
                } else {
                    if let Some(t) = events_task.take() {
                        t.abort();
                    }
                    let mut rx = web.daemon.bus.subscribe();
                    let tx = out_tx.clone();
                    events_task = Some(tokio::spawn(async move {
                        loop {
                            let msg = match rx.recv().await {
                                Ok(ev) => json!({"event": ev}),
                                Err(RecvError::Lagged(n)) => json!({"events_lost": n}),
                                Err(RecvError::Closed) => break,
                            };
                            if tx
                                .send(Message::Text(msg.to_string().into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }));
                    Ok(json!({"subscribed": true}))
                }
            }
            _ => web
                .daemon
                .dispatch(&who, &method, params)
                .await
                .map_err(|e| error_json(&e)),
        };
        let gateway = method == "peer.call";
        web.daemon.perf.request(
            Kind::Deck,
            &method,
            gateway.then(|| req["params"]["method"].as_str()).flatten(),
            gateway.then(|| req["params"]["station"].as_str()).flatten(),
            began.elapsed(),
            Some(waited),
        );
        if id.is_null() {
            continue; // fire-and-forget (keystrokes)
        }
        let out = match reply {
            Ok(result) => json!({"id": id, "result": result}),
            Err(error) => json!({"id": id, "error": error}),
        };
        if out_tx
            .send(Message::Text(out.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }

    for (_, c) in chans {
        match c {
            Chan::Local { task, .. } | Chan::Remote { task, .. } => task.abort(),
        }
    }
    if let Some(t) = events_task {
        t.abort();
    }
    reader.abort();
    if key_replaced {
        // The reply to the call that replaced the key is still on its way
        // out: let it go before the connection is closed.
        drop(out_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), writer).await;
        return;
    }
    writer.abort();
}

type AttachResult = (Value, tokio::task::JoinHandle<()>);

/// Attach to a pane on this station: a snapshot now, then the live stream.
fn attach_local(
    daemon: &Daemon,
    pane: &str,
    chan: u32,
    out: mpsc::Sender<Message>,
) -> Result<AttachResult, spandeck_core::Error> {
    let entry = daemon.pane(pane)?;
    let pane_id = entry.id.clone();
    let (snapshot, info, mut rx) = daemon.attach(&pane_id)?;
    let result = json!({
        "chan": chan, "pane": pane_id, "info": info,
        "snapshot_b64": base64::engine::general_purpose::STANDARD.encode(&snapshot),
        "exited": entry.meta().exit,
    });
    let d = daemon.clone();
    let task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(PaneEvent::Output(bytes)) => {
                    if out.send(frame(FRAME_OUTPUT, chan, &bytes)).await.is_err() {
                        return;
                    }
                }
                Ok(PaneEvent::Exited(info)) => {
                    let _ = out
                        .send(Message::Text(
                            json!({"chan": chan, "exited": info}).to_string().into(),
                        ))
                        .await;
                }
                Ok(_) => {}
                // This client fell behind: repaint it from the server's screen.
                Err(RecvError::Lagged(_)) => match d.attach(&pane_id) {
                    Ok((snapshot, _, fresh)) => {
                        d.perf.resyncs.add();
                        rx = fresh;
                        if out
                            .send(frame(FRAME_RESYNC, chan, &snapshot))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                Err(RecvError::Closed) => return,
            }
        }
    });
    Ok((result, task))
}

type RemoteAttach = (
    Value,
    mpsc::Sender<String>,
    u64,
    tokio::task::JoinHandle<()>,
);

/// Who a browser tab is at a pane's station when it comes through this one.
/// Each tab is a client of its own there, as it is here, so the sizing rule
/// (one size per pane, set by whoever has control) holds through a gateway:
/// before, every tab came as `gw-<station>` and they fought over the size.
/// Both parts are cut to what any station takes as a client id (letters,
/// digits and `-`, 64 in all); a longer one would be passed over there.
fn gateway_client(station: &str, client: &str) -> String {
    let clean = |s: &str, max: usize| -> String {
        s.chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
            .take(max)
            .collect()
    };
    format!("gw-{}-{}", clean(station, 24), clean(client, 36))
}

/// A `pane.resized` event of the pane's station, as a message on the
/// browser's channel for that pane: the size, and which client set it.
/// It is text from another station: numbers within a terminal's range and a
/// bounded name, or nothing.
fn resized_for(ev: &Value, pane: &str, chan: u32) -> Option<Value> {
    if ev["kind"] != json!("pane.resized") || ev["pane"] != json!(pane) {
        return None;
    }
    let cols = ev["data"]["cols"]
        .as_u64()
        .filter(|c| (2..=500).contains(c))?;
    let rows = ev["data"]["rows"]
        .as_u64()
        .filter(|r| (1..=300).contains(r))?;
    let controller: String = ev["data"]["controller"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(128)
        .collect();
    Some(json!({"chan": chan, "resized": {"cols": cols, "rows": rows, "controller": controller}}))
}

/// Gateway: attach to a pane on another station and relay its stream, so the
/// browser keeps a single connection to the station it opened. `client` is
/// the browser tab the attach is for.
async fn attach_remote(
    daemon: &Daemon,
    station: &str,
    pane: &str,
    control: bool,
    client: &str,
    chan: u32,
    out: mpsc::Sender<Message>,
) -> Result<RemoteAttach, String> {
    use tungstenite::client::IntoClientRequest;
    let peer = daemon
        .fleet
        .peer(station)
        .ok_or_else(|| format!("station {station} is not in the fleet"))?;
    let mut req = format!("ws://{}/api/ws", peer.addr)
        .into_client_request()
        .map_err(|e| e.to_string())?;
    req.headers_mut().insert(
        "x-spandeck-station",
        HeaderValue::from_str(&daemon.station).map_err(|e| e.to_string())?,
    );
    req.headers_mut().insert(
        "x-spandeck-sig",
        HeaderValue::from_str(&daemon.trust.sign("ws", daemon.station.as_bytes()))
            .map_err(|e| e.to_string())?,
    );
    let (ws, _) = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        tokio_tungstenite::connect_async(req),
    )
    .await
    .map_err(|_| format!("{station} did not answer"))?
    .map_err(|e| format!("{station} unreachable: {e}"))?;
    let (mut peer_sink, mut peer_stream) = ws.split();
    let send = |v: Value| tungstenite::Message::Text(v.to_string().into());
    peer_sink
        .send(send(
            json!({"auth": {"client": gateway_client(&daemon.station, client)}}),
        ))
        .await
        .map_err(|e| e.to_string())?;
    // The station's events come down this connection too, for one thing:
    // who resized the pane and to what.
    peer_sink
        .send(send(
            json!({"id": 2, "method": "events.subscribe", "params": {}}),
        ))
        .await
        .map_err(|e| e.to_string())?;
    peer_sink
        .send(send(json!({"id": 1, "method": "pane.attach", "params": {"pane": pane, "mode": if control { "control" } else { "watch" }}})))
        .await
        .map_err(|e| e.to_string())?;
    // Wait for the attach reply (the auth hello comes first). The hello says
    // which client that station took this connection for.
    let mut me = Value::Null;
    let mut result = loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), peer_stream.next()).await {
            Ok(Some(Ok(tungstenite::Message::Text(t)))) => {
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                if v.get("auth").is_some_and(|a| a["ok"] == json!(false)) {
                    // When the clocks are too far apart, that is why.
                    return Err(match daemon.perf.clock_of(station).filter(|c| c.refused()) {
                        Some(c) => format!(
                            "{station} refused this station: its clock and this station's differ by {}",
                            spandeck_core::perf::clock_words(c.diff_ms)
                        ),
                        None => format!("{station} refused this station"),
                    });
                }
                if let Some(c) = v["auth"]["client"].as_str() {
                    me = json!(c.chars().take(128).collect::<String>());
                }
                if v.get("id") == Some(&json!(1)) {
                    if let Some(e) = v.get("error") {
                        return Err(e["message"].as_str().unwrap_or("attach failed").to_string());
                    }
                    break v["result"].clone();
                }
            }
            Ok(Some(Ok(_))) => {}
            _ => return Err(format!("{station} closed the connection")),
        }
    };
    let peer_chan = result["chan"].as_u64().unwrap_or(0);
    result["chan"] = json!(chan);
    result["station"] = json!(station);
    // Who this tab is there: what a `resized` message names when it was us.
    result["client"] = me;
    let pane_id = result["pane"].as_str().unwrap_or(pane).to_string();

    let (tx, mut rx) = mpsc::channel::<String>(256);
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                to_peer = rx.recv() => match to_peer {
                    Some(text) => {
                        if peer_sink.send(tungstenite::Message::Text(text.into())).await.is_err() { break; }
                    }
                    None => break,
                },
                from_peer = peer_stream.next() => match from_peer {
                    Some(Ok(tungstenite::Message::Binary(b))) if b.len() >= 5 => {
                        if out.send(frame(b[0], chan, &b[5..])).await.is_err() { break; }
                    }
                    Some(Ok(tungstenite::Message::Text(t))) => {
                        let Ok(mut v) = serde_json::from_str::<Value>(&t) else { continue };
                        if v.get("chan").is_some() {
                            v["chan"] = json!(chan);
                            if out.send(Message::Text(v.to_string().into())).await.is_err() { break; }
                        } else if let Some(msg) = v.get("event").and_then(|ev| resized_for(ev, &pane_id, chan)) {
                            if out.send(Message::Text(msg.to_string().into())).await.is_err() { break; }
                        }
                    }
                    Some(Ok(_)) => {}
                    _ => {
                        let _ = out.send(Message::Text(json!({"chan": chan, "unreachable": true}).to_string().into())).await;
                        break;
                    }
                },
            }
        }
    });
    Ok((result, tx, peer_chan, task))
}

/// A short name for exactly this set of Control Deck files. A page that was
/// loaded from an older set reloads itself when it sees a different one.
fn assets_build() -> &'static str {
    static BUILD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BUILD.get_or_init(|| {
        use std::hash::{Hash, Hasher};
        let mut names: Vec<_> = Assets::iter().collect();
        names.sort();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for n in names {
            n.hash(&mut h);
            if let Some(f) = Assets::get(&n) {
                f.data.hash(&mut h);
            }
        }
        format!("{:016x}", h.finish())
    })
}
