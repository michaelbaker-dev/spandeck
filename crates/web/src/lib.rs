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
use std::sync::{Arc, Mutex, RwLock};

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Response, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::serve::ListenerExt;
use axum::{Json, Router};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use spandeck_core::daemon::{Authority, Caller, Daemon};
use spandeck_core::fleet::Remote;
use spandeck_core::perf::Kind;
use spandeck_core::rpc::AtRead;
use spandeck_core::server::error_json;
use spandeck_core::trust::Signed;
use spandeck_terminal::PaneEvent;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite;

mod lanes;
use lanes::{Lane, Lines, Scope};

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

/// The addresses the web server listens on, kept in step with what is
/// wanted: an address that could not be bound is tried again, and with no
/// `web_bind` the machine's Tailscale addresses are followed as they change.
struct Listening {
    web: Web,
    app: Router,
    port: u16,
    /// Always wanted: localhost, or what `web_bind` names.
    fixed: Vec<IpAddr>,
    /// With no `web_bind`: this machine's Tailscale addresses, and its name
    /// there, as last read.
    tailnet: Vec<IpAddr>,
    dns: Option<String>,
    serving: Vec<(IpAddr, SocketAddr, tokio::task::JoinHandle<()>)>,
}

impl Listening {
    /// Listen on every wanted address that is not being served, and stop
    /// serving a Tailscale address the machine no longer has.
    async fn bring_in_line(&mut self) {
        let wanted: Vec<IpAddr> = self
            .fixed
            .iter()
            .chain(self.tailnet.iter())
            .copied()
            .collect();
        let mut changed = false;
        // Gone: stop accepting there first, then take it off the allowlist.
        let (kept, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.serving)
            .into_iter()
            .partition(|(ip, _, _)| wanted.contains(ip));
        self.serving = kept;
        for (ip, _, task) in gone {
            task.abort();
            tracing::info!(%ip, "no longer listening: the address is gone");
            changed = true;
        }
        // New, or not bound before: bind, learn the port, fill in the Host
        // allowlist, and only then start answering. A request must never
        // race the allowlist.
        let mut fresh = vec![];
        for ip in wanted {
            if self.serving.iter().any(|(have, _, _)| *have == ip)
                || fresh.iter().any(|(have, _, _)| *have == ip)
            {
                continue;
            }
            match tokio::net::TcpListener::bind(SocketAddr::new(ip, self.port)).await {
                Ok(listener) => match listener.local_addr() {
                    Ok(addr) => fresh.push((ip, addr, listener)),
                    Err(e) => tracing::warn!(%ip, error = %e, "could not listen"),
                },
                // Tried again at the next round, ten seconds on.
                Err(e) => tracing::warn!(%ip, port = self.port, error = %e, "could not listen"),
            }
        }
        if !changed && fresh.is_empty() && self.hosts() == *self.web.hosts.read().unwrap() {
            return;
        }
        let mut hosts = self.hosts();
        for (_, addr, _) in &fresh {
            hosts.extend(self.hosts_of(addr));
        }
        hosts.sort();
        hosts.dedup();
        *self.web.hosts.write().unwrap() = hosts;
        for (ip, addr, listener) in fresh {
            let app = self.app.clone();
            let daemon = self.web.daemon.clone();
            let listener = listener.tap_io(move |tcp| no_delay(&daemon, tcp));
            let task = tokio::spawn(async move {
                let _ = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await;
            });
            tracing::info!(%addr, "Control Deck listening");
            self.serving.push((ip, addr, task));
        }
        *self.web.daemon.web_addrs.lock().unwrap() =
            self.serving.iter().map(|(_, a, _)| a.to_string()).collect();
    }

    /// The Host values answered to for what is being served.
    fn hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .serving
            .iter()
            .flat_map(|(_, addr, _)| self.hosts_of(addr))
            .collect();
        hosts.sort();
        hosts.dedup();
        hosts
    }

    fn hosts_of(&self, addr: &SocketAddr) -> Vec<String> {
        let p = addr.port();
        let mut hosts = vec![format!("{}:{p}", addr.ip())];
        for name in [
            "localhost".to_string(),
            self.web.daemon.station.to_lowercase(),
            spandeck_core::paths::hostname().to_lowercase(),
        ] {
            hosts.push(format!("{name}:{p}"));
        }
        if let Some(dns) = &self.dns {
            hosts.push(format!("{dns}:{p}"));
            // Behind `tailscale serve`, the browser sees the bare HTTPS name,
            // or the name with whichever port serve was given. The name
            // belongs to this machine alone, so any port of it is ours.
            hosts.push(dns.clone());
            hosts.push(format!("{dns}:*"));
        }
        hosts
    }
}

/// The switch for sending what is written at once (`[flags] nodelay`, on
/// unless it is set to false).
const NODELAY: &str = "nodelay";

/// Send what is written to this connection at once (`TCP_NODELAY`), where
/// the system's default is to hold a small piece back until the other end
/// has acknowledged the piece before it. Measured on 0.4.4: four answers to
/// a deck that asked four things together took 42 ms, because the second
/// waited for an acknowledgement the browser's system gives only after
/// 40 ms when it has nothing of its own to send; under 1 ms when it
/// acknowledged at once. Every connection the station accepts: a deck's
/// socket, another station's calls and its relays through this one.
fn no_delay(daemon: &Daemon, tcp: &tokio::net::TcpStream) {
    if daemon.flag_on(NODELAY, true) {
        if let Err(e) = tcp.set_nodelay(true) {
            tracing::debug!(error = %e, "could not set TCP_NODELAY on a connection");
        }
    }
}

/// Bind the listeners and start serving. Returns the bound addresses.
///
/// What could not be bound is tried again every ten seconds, and (with no
/// `web_bind`) the machine's Tailscale addresses are followed: before this,
/// a station that started before Tailscale was up, or whose address
/// changed, was never served there until spandeck was restarted.
pub async fn start(daemon: Daemon) -> anyhow::Result<Vec<SocketAddr>> {
    let port = daemon.cfg.web_port;
    let follow = daemon.cfg.web_bind.is_empty();
    let mut fixed: Vec<IpAddr> = vec![];
    let mut tailnet: Vec<IpAddr> = vec![];
    if follow {
        fixed.push(IpAddr::from([127, 0, 0, 1]));
        let fleet_daemon = daemon.clone();
        let me = tokio::task::spawn_blocking(move || fleet_daemon.fleet.tailnet_self())
            .await
            .ok()
            .flatten();
        if let Some(me) = me {
            tailnet.extend(
                me.ips
                    .iter()
                    .filter_map(|i| i.parse::<IpAddr>().ok())
                    .filter(|i| i.is_ipv4()),
            );
        }
    } else {
        for b in &daemon.cfg.web_bind {
            fixed.push(
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

    let dns = tokio::task::spawn_blocking(|| {
        spandeck_core::fleet::tailscale_status()
            .and_then(|s| spandeck_core::fleet::OwnAddrs::of(&s).dns)
    })
    .await
    .ok()
    .flatten();
    let mut listening = Listening {
        web,
        app,
        port,
        fixed,
        tailnet,
        dns,
        serving: vec![],
    };
    listening.bring_in_line().await;
    let bound: Vec<SocketAddr> = listening.serving.iter().map(|(_, a, _)| *a).collect();

    // From here on: every ten seconds, and at once when a look at the fleet
    // finds this machine's Tailscale addresses changed.
    let mut own = daemon.fleet.own_addrs.subscribe();
    tokio::spawn(async move {
        let mut watching = true;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
                changed = own.changed(), if watching => watching = changed.is_ok(),
            }
            let now = own.borrow_and_update().clone();
            if let Some(now) = now {
                // The machine's name there is answered to under either
                // setting; its addresses are listened on only when no
                // `web_bind` says where to listen.
                listening.dns = now.dns.or(listening.dns.take());
                if follow {
                    listening.tailnet = now.ips.into_iter().filter(|i| i.is_ipv4()).collect();
                }
            }
            listening.bring_in_line().await;
        }
    });
    if bound.is_empty() {
        anyhow::bail!("could not listen on port {port} on any address");
    }
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
        /// Whose pane it is, and its id there: what typed into this
        /// channel keeps its order with (lanes).
        station: String,
        pane: String,
        task: tokio::task::JoinHandle<()>,
    },
    /// A conversation being watched: items go down, nothing comes up.
    Watch { task: tokio::task::JoinHandle<()> },
}

impl Chan {
    fn close(self) {
        match self {
            Chan::Local { task, .. } | Chan::Remote { task, .. } | Chan::Watch { task } => {
                task.abort()
            }
        }
    }
}

/// The channel numbers of one connection: its pane attachments, watched
/// conversations and relays to other stations.
///
/// A number names a channel on the connection that gave it and nowhere
/// else. Every connection used to count from 1, so a page that came back
/// after a drop still held numbers the new connection was handing out
/// again: a key sent with one pane's old number was written to whichever
/// pane had that number now (measured on 0.4.2). Each connection now starts
/// counting at a place of its own, and what is sent with a number this
/// connection did not give is refused.
struct Channels<T> {
    live: HashMap<u32, T>,
    next: u32,
}

/// A connection's first number is below this, which leaves a connection
/// three thousand million numbers before it comes round.
const CHAN_FIRST_BELOW: u32 = 1 << 30;

impl<T> Channels<T> {
    /// `seed`: any number, from the system's random source for a real
    /// connection. The first number is in `1..CHAN_FIRST_BELOW`.
    fn starting_from(seed: u32) -> Self {
        Channels {
            live: HashMap::new(),
            next: 1 + seed % (CHAN_FIRST_BELOW - 1),
        }
    }

    /// A number for a new channel: never 0 (which an answer without a
    /// number reads as), and never one in use on this connection, however
    /// long it has counted.
    fn fresh(&mut self) -> u32 {
        loop {
            let chan = self.next;
            self.next = if chan >= u32::MAX - 1 { 1 } else { chan + 1 };
            if !self.live.contains_key(&chan) {
                return chan;
            }
        }
    }

    /// The number a request names, if it is one: a whole number that fits
    /// the frame's four bytes. Anything else names no channel (it used to
    /// be cut to its low 32 bits, and so could name one).
    fn named(params: &Value) -> Option<u32> {
        params
            .get("chan")
            .and_then(|c| c.as_u64())
            .and_then(|c| u32::try_from(c).ok())
    }

    fn open(&mut self, chan: u32, what: T) {
        self.live.insert(chan, what);
    }

    fn get(&self, params: &Value) -> Option<&T> {
        Self::named(params).and_then(|c| self.live.get(&c))
    }

    fn close(&mut self, params: &Value) -> Option<T> {
        Self::named(params).and_then(|c| self.live.remove(&c))
    }
}

/// Where a new connection starts counting its channels.
fn chan_seed() -> u32 {
    u32::from_str_radix(&spandeck_core::paths::random_token()[..8], 16)
        .expect("a random token is hex")
}

fn frame(kind: u8, chan: u32, payload: &[u8]) -> Message {
    let mut buf = Vec::with_capacity(payload.len() + 5);
    buf.push(kind);
    buf.extend_from_slice(&chan.to_le_bytes());
    buf.extend_from_slice(payload);
    Message::Binary(buf.into())
}

/// A connection's channels, shared by the loop that reads its requests and
/// the tasks that attach: an attach is answered after its channel is in
/// here, and one that finishes after the connection ended is closed again.
struct Open {
    chans: Channels<Chan>,
    ended: bool,
}

/// What every task that runs one of a connection's requests needs of it.
#[derive(Clone)]
struct Conn {
    web: Web,
    who: Arc<Caller>,
    out: mpsc::Sender<Message>,
    open: Arc<Mutex<Open>>,
}

/// One request as it was read.
struct Req {
    arrived: std::time::Instant,
    id: Value,
    method: String,
    params: Value,
}

/// Where what is typed into a channel goes.
enum Typed {
    /// Not taken, and why.
    No(Value),
    /// A pane of this station.
    Here(String),
    /// A pane of another station, down the connection made for it.
    There {
        tx: mpsc::Sender<String>,
        text: String,
        station: String,
        pane: String,
    },
}

fn denied(message: &str) -> Value {
    json!({"code": "denied", "message": message})
}

impl Conn {
    fn daemon(&self) -> &Daemon {
        &self.web.daemon
    }

    /// `pane.attach` and `pane.conversation.watch`: the channel is in the
    /// connection's list before the answer that names it is given.
    async fn attach(&self, method: &str, params: &Value) -> Result<Value, Value> {
        let (daemon, who) = (self.daemon(), &self.who);
        let watch = method == "pane.conversation.watch";
        if !who.is_owner() {
            return Err(denied(if watch {
                "a conversation is for the user's clients"
            } else {
                "attaching is for the user's clients"
            }));
        }
        let chan = self.open.lock().unwrap().chans.fresh();
        let station = params
            .get("station")
            .and_then(|s| s.as_str())
            .filter(|s| *s != daemon.station)
            .map(String::from);
        // The conversation view's live stream: the first page as the
        // reply, then every change on this channel, the pane's file
        // looked at five times a second for as long as someone watches.
        // The owner's clients only, as attaching is; a pane on another
        // station is watched there and passed through, and nothing of
        // it is kept here.
        if watch {
            let (result, task) = match station {
                None => watch_local(daemon, params, chan, self.out.clone())
                    .await
                    .map_err(|e| error_json(&e))?,
                Some(station) => {
                    watch_remote(
                        daemon,
                        &station,
                        params,
                        &who.client,
                        chan,
                        self.out.clone(),
                    )
                    .await?
                }
            };
            self.opened(chan, Chan::Watch { task })?;
            return Ok(result);
        }
        let pane = params
            .get("pane")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        let control = params.get("mode").and_then(|m| m.as_str()) != Some("watch");
        match station {
            None => {
                let (result, task) =
                    attach_local(daemon, &pane, &who.client, chan, self.out.clone())
                        .map_err(|e| error_json(&e))?;
                let pane = result["pane"].as_str().unwrap_or(&pane).to_string();
                daemon.note_attach(&pane, &who.who, &who.client, control);
                self.opened(
                    chan,
                    Chan::Local {
                        pane,
                        control,
                        task,
                    },
                )?;
                Ok(result)
            }
            Some(station) => {
                let (result, tx, peer_chan, task) = attach_remote(
                    daemon,
                    &station,
                    &pane,
                    control,
                    &who.client,
                    chan,
                    self.out.clone(),
                )
                .await
                .map_err(|e| json!({"code": "unreachable", "message": e}))?;
                let pane = result["pane"].as_str().unwrap_or(&pane).to_string();
                self.opened(
                    chan,
                    Chan::Remote {
                        tx,
                        peer_chan,
                        control,
                        station,
                        pane,
                        task,
                    },
                )?;
                Ok(result)
            }
        }
    }

    /// Put a new channel in the connection's list; if the connection ended
    /// while it was being made, close it instead.
    fn opened(&self, chan: u32, what: Chan) -> Result<(), Value> {
        let mut open = self.open.lock().unwrap();
        if open.ended {
            what.close();
            return Err(json!({"code": "internal", "message": "the connection ended"}));
        }
        open.chans.open(chan, what);
        Ok(())
    }

    fn detach(&self, params: &Value) -> Value {
        let gone = self.open.lock().unwrap().chans.close(params);
        if let Some(c) = gone {
            c.close();
        }
        json!({"ok": true})
    }

    /// Typing and resizing name the channel they are for. A number this
    /// connection did not give names nothing, whatever pane a connection
    /// before this one had under it.
    fn typed(&self, method: &str, params: &Value) -> Typed {
        let watching = || Typed::No(denied("watching only: take control to type"));
        let open = self.open.lock().unwrap();
        match open.chans.get(params) {
            Some(Chan::Watch { .. }) => Typed::No(denied("a watched conversation takes no typing")),
            Some(Chan::Local { control, .. }) | Some(Chan::Remote { control, .. }) if !control => {
                watching()
            }
            Some(Chan::Local { pane, .. }) => Typed::Here(pane.clone()),
            Some(Chan::Remote {
                tx,
                peer_chan,
                station,
                pane,
                ..
            }) => {
                let mut p = params.clone();
                p["chan"] = json!(peer_chan);
                Typed::There {
                    tx: tx.clone(),
                    text: json!({"id": null, "method": method, "params": p}).to_string(),
                    station: station.clone(),
                    pane: pane.clone(),
                }
            }
            None => Typed::No(json!({"code": "not_found", "message": "no such channel"})),
        }
    }

    async fn dispatch(&self, method: &str, params: Value, at: AtRead) -> Result<Value, Value> {
        self.daemon()
            .dispatch_at(&self.who, method, params, at)
            .await
            .map_err(|e| error_json(&e))
    }

    /// A request was handled: count it, and give its answer if it asked
    /// for one (a keystroke does not).
    fn done(
        &self,
        req: &Req,
        lane: Option<Lane>,
        began: std::time::Instant,
        reply: Result<Value, Value>,
    ) -> Option<Message> {
        let gateway = req.method == "peer.call";
        let waited = began.duration_since(req.arrived);
        let perf = &self.daemon().perf;
        perf.request(
            Kind::Deck,
            &req.method,
            gateway.then(|| req.params["method"].as_str()).flatten(),
            gateway.then(|| req.params["station"].as_str()).flatten(),
            began.elapsed(),
            Some(waited),
        );
        if let Some(lane) = lane {
            perf.lane_waited(lane, waited);
        }
        answer(&req.id, reply)
    }

    /// `done`, from a task: the answer waits for room on the way out.
    async fn finish(
        &self,
        req: &Req,
        lane: Lane,
        began: std::time::Instant,
        reply: Result<Value, Value>,
    ) {
        if let Some(msg) = self.done(req, Some(lane), began, reply) {
            let _ = self.out.send(msg).await;
        }
    }

    /// Give an answer from the loop that reads requests, which waits for
    /// nothing: when the way out is full, a task of its own waits.
    fn say(&self, msg: Option<Message>) {
        if let Some(msg) = msg {
            if let Err(mpsc::error::TrySendError::Full(msg)) = self.out.try_send(msg) {
                let out = self.out.clone();
                tokio::spawn(async move {
                    let _ = out.send(msg).await;
                });
            }
        }
    }

    /// What is typed for a pane of another station could not be handed to
    /// the connection made for it: said on the pane's channel, since a key
    /// asks for no answer.
    fn input_refused(&self, params: &Value, station: &str) {
        if let Some(chan) = Channels::<Chan>::named(params) {
            let short: String = station.chars().take(64).collect();
            self.say(Some(Message::Text(
                json!({"chan": chan, "input_refused": 1, "why": "busy", "station": short})
                    .to_string()
                    .into(),
            )));
        }
    }
}

fn answer(id: &Value, reply: Result<Value, Value>) -> Option<Message> {
    if id.is_null() {
        return None; // fire-and-forget (keystrokes)
    }
    let out = match reply {
        Ok(result) => json!({"id": id, "result": result}),
        Err(error) => json!({"id": id, "error": error}),
    };
    Some(Message::Text(out.to_string().into()))
}

/// The requests of one connection that are running: its lines, its limits,
/// and the tasks.
struct Running {
    conn: Conn,
    lines: Lines,
    counts: lanes::Counts,
    reads: Arc<Semaphore>,
    reads_to: HashMap<String, Arc<Semaphore>>,
    attaches: Arc<Semaphore>,
    /// Ended with the connection: reads, attaches, waits.
    ending: JoinSet<()>,
    /// Run to their end whatever becomes of the connection: everything
    /// that changes something, as before.
    lasting: JoinSet<()>,
    /// Did the page say it knows `busy`?
    page_knows_busy: bool,
}

impl Running {
    fn new(conn: Conn, page_knows_busy: bool) -> Running {
        Running {
            conn,
            lines: Lines::default(),
            counts: lanes::Counts::default(),
            reads: Arc::new(Semaphore::new(lanes::READS)),
            reads_to: HashMap::new(),
            attaches: Arc::new(Semaphore::new(lanes::ATTACHES)),
            ending: JoinSet::new(),
            lasting: JoinSet::new(),
            page_knows_busy,
        }
    }

    /// Refuse a request at once: too much is waiting where it would wait.
    fn busy(&self, req: &Req, busy: &lanes::Busy) {
        self.conn.daemon().perf.busy.add();
        self.conn
            .say(answer(&req.id, Err(busy.error(self.page_knows_busy))));
    }

    /// A pane of this station by the name a request gave: its id, so that
    /// two names for one pane are one pane's order.
    fn scope_here(&self, scope: Option<Scope>) -> Option<Scope> {
        match scope {
            Some(Scope::Pane(name)) => Some(Scope::Pane(
                self.conn
                    .daemon()
                    .pane(&name)
                    .map(|p| p.id.clone())
                    .unwrap_or(name),
            )),
            other => other,
        }
    }

    /// Take one request: put it in its lane. Waits for nothing (what is
    /// typed into a pane of this station is handed to the pane's own queue
    /// here, which never waits).
    async fn take(&mut self, req: Req) {
        let conn = self.conn.clone();
        let method = req.method.as_str();
        let by_chan = req.params.get("chan").is_some_and(|c| !c.is_null());
        match method {
            "pane.detach" | "pane.conversation.unwatch" => {
                let reply = Ok(conn.detach(&req.params));
                return conn.say(conn.done(&req, None, std::time::Instant::now(), reply));
            }
            "pane.input" | "pane.resize" if by_chan => return self.typed(req).await,
            _ => {}
        }
        let route = lanes::route(&conn.daemon().station, method, &req.params);
        let station = route.station.clone();
        let line = station.clone().unwrap_or_default();
        let lane = route.lane;
        let scope = match &station {
            None => self.scope_here(route.scope.clone()),
            Some(_) => route.scope.clone(),
        };
        // What is typed into a pane of this station that nothing read
        // before it is still being done for: straight to the pane.
        if lane == Lane::Input
            && station.is_none()
            && scope.as_ref().is_none_or(|s| self.lines.free("", s))
        {
            let began = std::time::Instant::now();
            let reply = conn
                .dispatch(method, req.params.clone(), AtRead::default())
                .await;
            return conn.say(conn.done(&req, Some(lane), began, reply));
        }
        let counted = match self.counts.admit(station.as_deref(), lane) {
            Ok(c) => c,
            Err(busy) => return self.busy(&req, &busy),
        };
        // A stop's place in a pane's input is where it is read: here,
        // unless something typed for that pane before it is still waiting
        // its turn, when the stop takes its place at its own turn.
        let mut at = AtRead::default();
        let mut place = None;
        let stop_all = station.is_none() && route.method == "stop.all";
        if station.is_none() && lane == Lane::Urgent && conn.who.is_owner() {
            let daemon = conn.daemon();
            match (route.method.as_str(), &scope) {
                ("pane.stop", Some(Scope::Pane(id))) if !self.lines.input_waits("", id) => {
                    if let Ok(pane) = daemon.pane(id) {
                        at.marks.insert(pane.id.clone(), pane.term.mark_stop());
                    }
                }
                ("stop.all", _) => {
                    let whom = route.stop_scope.as_deref().unwrap_or("station");
                    let mut legs: Vec<(String, String)> = vec![];
                    if daemon.stop_is_for_here(whom) {
                        daemon
                            .stop_marks_unless(&mut at.marks, |id| self.lines.input_waits("", id));
                        legs.push((daemon.station.clone(), String::new()));
                    }
                    match whom {
                        "fleet" => legs.extend(
                            daemon
                                .fleet
                                .peers()
                                .iter()
                                .map(|p| (p.station.clone(), p.station.clone())),
                        ),
                        other if !daemon.stop_is_for_here(other) => {
                            legs.push((other.to_string(), other.to_string()))
                        }
                        _ => {}
                    }
                    // The stop's place among what is for each station's
                    // panes; each station's part waits for its own turn.
                    for (name, line) in legs {
                        let leg = self.lines.enter(&line, Scope::AllPanes, false);
                        at.legs.insert(name, Box::new(leg));
                    }
                }
                _ => {}
            }
        }
        if !stop_all || at.legs.is_empty() {
            place = scope.map(|s| self.lines.enter(&line, s, lane == Lane::Input));
        }
        // A read for another station has that station's places and no
        // others, so stations that do not answer hold up no read of this
        // station or of a station that does. (A name that is no station of
        // the fleet is refused at once, wherever it waits.)
        let places = match lane {
            Lane::Read => Some(
                match station
                    .as_ref()
                    .filter(|s| conn.daemon().fleet.peer(s).is_some())
                {
                    Some(s) => self
                        .reads_to
                        .entry(s.clone())
                        .or_insert_with(|| Arc::new(Semaphore::new(lanes::READS_PER_STATION)))
                        .clone(),
                    None => self.reads.clone(),
                },
            ),
            Lane::Attach => Some(self.attaches.clone()),
            _ => None,
        };
        let task = async move {
            let _counted = counted;
            let _held = match places {
                Some(places) => places.acquire_owned().await.ok(),
                None => None,
            };
            if let Some(place) = &place {
                place.turn().await;
            }
            let began = std::time::Instant::now();
            let reply = match lane {
                Lane::Attach => conn.attach(&req.method, &req.params).await,
                _ => conn.dispatch(&req.method, req.params.clone(), at).await,
            };
            drop(place);
            conn.finish(&req, lane, began, reply).await;
        };
        if route.ends_with_connection {
            self.ending.spawn(task);
        } else {
            self.lasting.spawn(task);
        }
    }

    /// `pane.input` and `pane.resize` for a channel.
    async fn typed(&mut self, mut req: Req) {
        let conn = self.conn.clone();
        let began = std::time::Instant::now();
        match conn.typed(&req.method, &req.params) {
            Typed::No(why) => conn.say(conn.done(&req, Some(Lane::Input), began, Err(why))),
            Typed::Here(pane) => {
                let scope = Scope::Pane(pane.clone());
                req.params["pane"] = json!(pane);
                if self.lines.free("", &scope) {
                    let reply = conn
                        .dispatch(&req.method, req.params.clone(), AtRead::default())
                        .await;
                    return conn.say(conn.done(&req, Some(Lane::Input), began, reply));
                }
                // Something for this pane read before it is still being
                // done (a Stop, a Pause): it follows.
                let counted = match self.counts.admit(None, Lane::Input) {
                    Ok(c) => c,
                    Err(busy) => return self.busy(&req, &busy),
                };
                let place = self.lines.enter("", scope, true);
                self.lasting.spawn(async move {
                    let _counted = counted;
                    place.turn().await;
                    let began = std::time::Instant::now();
                    let reply = conn
                        .dispatch(&req.method, req.params.clone(), AtRead::default())
                        .await;
                    drop(place);
                    conn.finish(&req, Lane::Input, began, reply).await;
                });
            }
            Typed::There {
                tx,
                text,
                station,
                pane,
            } => {
                // Handed to the connection made for that pane, never
                // waited for: when that station takes nothing and 256
                // things already wait, this one is refused and the page
                // told, where it used to stop every request of this page.
                let scope = Scope::Pane(pane);
                let (to, knows) = (station.clone(), self.page_knows_busy);
                let hand = move |conn: &Conn, req: &Req| match tx.try_send(text) {
                    Ok(()) => Ok(json!({"ok": true})),
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        conn.daemon().perf.busy.add();
                        conn.input_refused(&req.params, &to);
                        Err(
                            json!({"code": if knows { lanes::CAP_BUSY } else { "internal" }, "message": format!("{to} is not taking what is typed: this was not sent")}),
                        )
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => Err(
                        json!({"code": "unreachable", "message": "the connection to that station ended"}),
                    ),
                };
                if self.lines.free(&station, &scope) {
                    let reply = hand(&conn, &req);
                    return conn.say(conn.done(&req, Some(Lane::Input), began, reply));
                }
                // A paste or a Stop for that pane, sent through a call of
                // its own, has not been answered yet: this follows it.
                let counted = match self.counts.admit(Some(&station), Lane::Input) {
                    Ok(c) => c,
                    Err(busy) => {
                        conn.input_refused(&req.params, &station);
                        return self.busy(&req, &busy);
                    }
                };
                let place = self.lines.enter(&station, scope, true);
                self.lasting.spawn(async move {
                    let _counted = counted;
                    place.turn().await;
                    let began = std::time::Instant::now();
                    let reply = hand(&conn, &req);
                    drop(place);
                    conn.finish(&req, Lane::Input, began, reply).await;
                });
            }
        }
    }

    /// One request, as before lanes (`ws_lanes = false`): waited for here,
    /// so the next is not read until it is done.
    async fn one_at_a_time(&mut self, req: Req) {
        let conn = self.conn.clone();
        let began = std::time::Instant::now();
        let by_chan = req.params.get("chan").is_some_and(|c| !c.is_null());
        let reply = match req.method.as_str() {
            "pane.attach" | "pane.conversation.watch" => {
                conn.attach(&req.method, &req.params).await
            }
            "pane.detach" | "pane.conversation.unwatch" => Ok(conn.detach(&req.params)),
            "pane.input" | "pane.resize" if by_chan => match conn.typed(&req.method, &req.params) {
                Typed::No(why) => Err(why),
                Typed::Here(pane) => {
                    let mut p = req.params.clone();
                    p["pane"] = json!(pane);
                    conn.dispatch(&req.method, p, AtRead::default()).await
                }
                Typed::There { tx, text, .. } => {
                    let _ = tx.send(text).await;
                    Ok(json!({"ok": true}))
                }
            },
            _ => {
                conn.dispatch(&req.method, req.params.clone(), AtRead::default())
                    .await
            }
        };
        if let Some(msg) = conn.done(&req, None, began, reply) {
            let _ = conn.out.send(msg).await;
        }
    }
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
    // What the page says it understands that a page from before does not.
    let page_knows_busy = auth["caps"]
        .as_array()
        .is_some_and(|caps| caps.iter().any(|c| c == lanes::CAP_BUSY));
    // Which of the owner's devices this is, as Tailscale names it: the deck
    // warns when the key of the device in the owner's hand is about to
    // expire. A device that opens a deck is one the fleet warns about.
    let device = if who.is_owner() && !who.who.starts_with("you (via ") {
        web.daemon.note_deck_device(addr.ip());
        web.daemon.tailnet_device_at(addr.ip())
    } else {
        None
    };
    // Read once, here: a connection keeps the way it began.
    let in_lanes = web.daemon.flag_on(lanes::FLAG, true);
    let hello = json!({"auth": {"ok": true, "station": web.daemon.station, "version": spandeck_core::VERSION, "build": assets_build(), "client": who.client, "full": who.is_owner(), "device": device, "lanes": in_lanes}});
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

    let conn = Conn {
        web: web.clone(),
        who: Arc::new(who),
        out: out_tx.clone(),
        open: Arc::new(Mutex::new(Open {
            chans: Channels::starting_from(chan_seed()),
            ended: false,
        })),
    };
    let who = conn.who.clone();
    let mut running = Running::new(conn.clone(), page_knows_busy);

    // Each request is stamped as it arrives, so that how long it then stood
    // before it was begun is known (the Performance page). They are read in
    // the order they came; up to 64 wait here, and past that the socket is
    // not read, as before. Reading one never waits for the one before it
    // (lanes.rs), unless the switch `ws_lanes` is off.
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
            // Tasks that are done are let go of as they finish.
            Some(_) = running.ending.join_next(), if !running.ending.is_empty() => continue,
            Some(_) = running.lasting.join_next(), if !running.lasting.is_empty() => continue,
        };
        let Some((arrived, msg)) = msg else { break };
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(req) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let req = Req {
            arrived,
            id: req.get("id").cloned().unwrap_or(Value::Null),
            method: req
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
            params: req.get("params").cloned().unwrap_or(json!({})),
        };
        if req.method == "events.subscribe" {
            let reply = if !who.is_owner() {
                Err(denied("events are for the user's clients"))
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
            };
            conn.say(conn.done(&req, None, std::time::Instant::now(), reply));
        } else if in_lanes {
            running.take(req).await;
        } else {
            running.one_at_a_time(req).await;
        }
    }

    // The connection is over. What only it would have read the answer to
    // is ended; what changes something runs to its end, as it always did.
    let chans = {
        let mut open = conn.open.lock().unwrap();
        open.ended = true;
        std::mem::take(&mut open.chans.live)
    };
    for (_, c) in chans {
        c.close();
    }
    if let Some(t) = events_task {
        t.abort();
    }
    reader.abort();
    running.ending.shutdown().await;
    if key_replaced {
        // The reply to the call that replaced the key is still on its way
        // out: let that call finish, and its answer go, before the
        // connection is closed.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while running.lasting.join_next().await.is_some() {}
        })
        .await;
        running.lasting.detach_all();
        drop(running);
        drop(conn);
        drop(out_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), writer).await;
        return;
    }
    running.lasting.detach_all();
    writer.abort();
}

type AttachResult = (Value, tokio::task::JoinHandle<()>);

/// A stop threw away something this client had sent to the pane and that
/// had not reached its program: said on the client's channel for the pane,
/// as how many things, never what they were.
fn discarded_for(
    ev: &spandeck_core::BusEvent,
    pane: &str,
    client: &str,
    chan: u32,
) -> Option<Value> {
    if ev.kind != "pane.input_discarded" || ev.pane.as_deref() != Some(pane) {
        return None;
    }
    let n = ev.data["clients"][client].as_u64().filter(|n| *n > 0)?;
    Some(json!({"chan": chan, "input_discarded": n, "why": "stopped"}))
}

/// Attach to a pane on this station: a snapshot now, then the live stream.
/// `client`: who attaches, told on this channel when a stop throws away
/// what it had sent.
fn attach_local(
    daemon: &Daemon,
    pane: &str,
    client: &str,
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
    let mut bus = daemon.bus.subscribe();
    let client = client.to_string();
    let task = tokio::spawn(async move {
        loop {
            let ev = tokio::select! {
                ev = rx.recv() => ev,
                said = bus.recv() => {
                    // Only one kind of event is looked for; one missed
                    // because this client fell behind is not made up for.
                    if let Some(msg) = said.ok().and_then(|ev| discarded_for(&ev, &pane_id, &client, chan)) {
                        if out.send(Message::Text(msg.to_string().into())).await.is_err() {
                            return;
                        }
                    }
                    continue;
                }
            };
            match ev {
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

type PeerSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A connection to another station of the fleet for one browser tab, signed
/// with the fleet key: how a deck served here attaches to a pane there, or
/// watches its conversation. The station there takes it as the owner's own
/// request passed on, and as a client of its own (`gateway_client`).
async fn peer_socket(
    daemon: &Daemon,
    station: &str,
    client: &str,
) -> Result<
    (
        futures_util::stream::SplitSink<PeerSocket, tungstenite::Message>,
        futures_util::stream::SplitStream<PeerSocket>,
    ),
    String,
> {
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
        // The same towards the other station: a key typed through this
        // one is a small piece too.
        tokio_tungstenite::connect_async_with_config(req, None, daemon.flag_on(NODELAY, true)),
    )
    .await
    .map_err(|_| format!("{station} did not answer"))?
    .map_err(|e| format!("{station} unreachable: {e}"))?;
    let (mut peer_sink, peer_stream) = ws.split();
    peer_sink
        .send(tungstenite::Message::Text(
            json!({"auth": {"client": gateway_client(&daemon.station, client)}})
                .to_string()
                .into(),
        ))
        .await
        .map_err(|e| e.to_string())?;
    Ok((peer_sink, peer_stream))
}

/// Why another station refused this one's connection.
fn refused_by(daemon: &Daemon, station: &str) -> String {
    // When the clocks are too far apart, that is why.
    match daemon.perf.clock_of(station).filter(|c| c.refused()) {
        Some(c) => format!(
            "{station} refused this station: its clock and this station's differ by {}",
            spandeck_core::perf::clock_words(c.diff_ms)
        ),
        None => format!("{station} refused this station"),
    }
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
    let (mut peer_sink, mut peer_stream) = peer_socket(daemon, station, client).await?;
    let send = |v: Value| tungstenite::Message::Text(v.to_string().into());
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
                    return Err(refused_by(daemon, station));
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

// ------------------------------------------------------- conversation watch

type Watched = (Value, tokio::task::JoinHandle<()>);

/// How often a watched pane's session file is looked at. The archive's own
/// look is once a second; nobody waits that long for a reply they are
/// watching for.
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_millis(200);

/// `pane.conversation`, off the async runtime: it reads a file and the
/// databases.
async fn conversation_of(
    daemon: &Daemon,
    pane: &str,
    params: Value,
) -> Result<Value, spandeck_core::Error> {
    let (d, pane) = (daemon.clone(), pane.to_string());
    tokio::task::spawn_blocking(move || d.conversation(&pane, &params))
        .await
        .map_err(|e| spandeck_core::Error::Invalid(e.to_string()))?
}

/// What of an answer, beside its items, is worth telling a watcher when it
/// changes: everything but the station's clock.
fn steady(answer: &Value) -> Value {
    let mut live = answer["live"].clone();
    if let Some(live) = live.as_object_mut() {
        live.remove("now_ms");
    }
    json!([
        answer["available"],
        answer["reason"],
        answer["session"],
        answer["title"],
        answer["not_understood"],
        live
    ])
}

/// Watch a pane's conversation on this station. The reply is what
/// `pane.conversation` gives for the same parameters, so a client that
/// comes back with its cursor is caught up in the reply; after it, every
/// change is a `{"chan", "conversation": {…}}` message of that same shape,
/// asked for `after` the cursor of the one before.
async fn watch_local(
    daemon: &Daemon,
    params: &Value,
    chan: u32,
    out: mpsc::Sender<Message>,
) -> Result<Watched, spandeck_core::Error> {
    let pane = daemon
        .pane(params.get("pane").and_then(|p| p.as_str()).unwrap_or(""))?
        .id
        .clone();
    let mut first = conversation_of(daemon, &pane, params.clone()).await?;
    first["chan"] = json!(chan);
    let mut was = steady(&first);
    let mut session = first["session"].as_str().unwrap_or("").to_string();
    let mut cursor = first["cursor"].as_u64().unwrap_or(0);
    let d = daemon.clone();
    let task = tokio::spawn(async move {
        let text = |v: Value| Message::Text(v.to_string().into());
        let mut tick = tokio::time::interval(WATCH_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tick.tick().await;
        loop {
            tick.tick().await;
            // An empty session name is never the pane's: when the pane gets
            // a session, the answer starts again from its newest items.
            let ask = json!({"pane": pane, "session": session, "after": cursor, "limit": 200});
            let next = match conversation_of(&d, &pane, ask).await {
                Ok(next) => next,
                Err(e) => {
                    // The pane was closed.
                    let _ = out
                        .send(text(
                            json!({"chan": chan, "conversation_end": {"message": e.to_string()}}),
                        ))
                        .await;
                    return;
                }
            };
            let now = steady(&next);
            let news = |key: &str| next[key].as_array().is_some_and(|a| !a.is_empty());
            let changed =
                news("items") || news("updated") || next["reset"] == json!(true) || now != was;
            was = now;
            session = next["session"].as_str().unwrap_or("").to_string();
            cursor = next["cursor"].as_u64().unwrap_or(0);
            if changed
                && out
                    .send(text(json!({"chan": chan, "conversation": next})))
                    .await
                    .is_err()
            {
                return;
            }
        }
    });
    Ok((first, task))
}

/// Gateway: watch a pane's conversation on another station and pass each
/// message through, so the browser keeps one connection to the station it
/// opened. What passes is the owner's own history on its way to the owner's
/// browser: nothing of it is kept, logged or read here.
async fn watch_remote(
    daemon: &Daemon,
    station: &str,
    params: &Value,
    client: &str,
    chan: u32,
    out: mpsc::Sender<Message>,
) -> Result<Watched, Value> {
    let unreachable = |message: String| json!({"code": "unreachable", "message": message});
    let (mut peer_sink, mut peer_stream) = peer_socket(daemon, station, client)
        .await
        .map_err(unreachable)?;
    let mut ask = params.clone();
    if let Some(ask) = ask.as_object_mut() {
        ask.remove("station");
    }
    peer_sink
        .send(tungstenite::Message::Text(
            json!({"id": 1, "method": "pane.conversation.watch", "params": ask})
                .to_string()
                .into(),
        ))
        .await
        .map_err(|e| unreachable(e.to_string()))?;
    let mut result = loop {
        match tokio::time::timeout(std::time::Duration::from_secs(10), peer_stream.next()).await {
            Ok(Some(Ok(tungstenite::Message::Text(t)))) => {
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                if v.get("auth").is_some_and(|a| a["ok"] == json!(false)) {
                    return Err(unreachable(refused_by(daemon, station)));
                }
                if v.get("id") == Some(&json!(1)) {
                    if let Some(e) = v.get("error") {
                        // Another station's words: bounded, and passed as text.
                        let short = |v: &Value, max: usize| -> String {
                            v.as_str().unwrap_or("").chars().take(max).collect()
                        };
                        return Err(json!({
                            "code": short(&e["code"], 32),
                            "message": format!("{station}: {}", short(&e["message"], 300)),
                        }));
                    }
                    break v["result"].clone();
                }
            }
            Ok(Some(Ok(_))) => {}
            _ => return Err(unreachable(format!("{station} did not answer"))),
        }
    };
    result["chan"] = json!(chan);
    let task = tokio::spawn(async move {
        // Held so the connection stays open for as long as it is watched.
        let _peer_sink = peer_sink;
        loop {
            match peer_stream.next().await {
                Some(Ok(tungstenite::Message::Text(t))) => {
                    let Ok(mut v) = serde_json::from_str::<Value>(&t) else {
                        continue;
                    };
                    if v.get("chan").is_some() {
                        v["chan"] = json!(chan);
                        if out.send(Message::Text(v.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                }
                Some(Ok(_)) => {}
                _ => {
                    let _ = out
                        .send(Message::Text(
                            json!({"chan": chan, "unreachable": true})
                                .to_string()
                                .into(),
                        ))
                        .await;
                    break;
                }
            }
        }
    });
    Ok((result, task))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The audit's gap 13, measured on 0.4.2: every connection counted its
    /// channels from 1, so a key carrying a pane's number from before a
    /// reconnect was written to the pane that had the number now.
    #[test]
    fn a_number_from_another_connection_names_nothing() {
        // Two connections, each starting where its own seed puts it.
        let mut before = Channels::starting_from(0x1234_5678);
        let mut after = Channels::starting_from(0x9abc_def0);
        let (b, c) = (before.fresh(), after.fresh());
        before.open(b, "pane B");
        after.open(c, "pane C");
        assert_ne!(b, c);
        assert_eq!(after.get(&json!({"chan": b})), None, "B's old number");
        assert_eq!(after.get(&json!({"chan": c})), Some(&"pane C"));
        // A number is not cut down to one that fits: 2^32 more than C's
        // number is not C's.
        let wide = c as u64 + (1 << 32);
        assert_eq!(after.get(&json!({"chan": wide})), None);
        assert_eq!(after.close(&json!({"chan": wide})), None);
        assert_eq!(after.get(&json!({"chan": "1"})), None);
        // Whatever the seed, the first number is in 1..2^30.
        for seed in [0, 1, CHAN_FIRST_BELOW - 2, CHAN_FIRST_BELOW - 1, u32::MAX] {
            let first = Channels::<()>::starting_from(seed).fresh();
            assert!((1..CHAN_FIRST_BELOW).contains(&first), "{seed}: {first}");
        }
    }

    /// The frame's channel field is four bytes and 0 is read as "no
    /// number": a connection that counts all the way round passes over 0
    /// and over every number it still has open.
    #[test]
    fn a_connection_that_counts_round_reuses_no_live_number() {
        let mut chans = Channels::starting_from(0);
        chans.next = u32::MAX - 2;
        chans.open(1, "still open");
        chans.open(2, "still open");
        let given: Vec<u32> = (0..3).map(|_| chans.fresh()).collect();
        assert_eq!(given, [u32::MAX - 2, u32::MAX - 1, 3]);
    }
}
