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
use std::sync::atomic::{AtomicU64, Ordering};
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
use spandeck_core::beat::{self, Beat, Verdict};
use spandeck_core::daemon::{Authority, Caller, Daemon};
use spandeck_core::fleet::Remote;
use spandeck_core::perf::Kind;
use spandeck_core::rids::Retry;
use spandeck_core::rpc::AtRead;
use spandeck_core::server::error_json;
use spandeck_core::trust::Signed;
use spandeck_terminal::PaneEvent;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite;

mod fence;
mod files;
mod lanes;
mod tls;
use lanes::{Lane, Lines, Scope};
use spandeck_core::typed;

#[derive(RustEmbed)]
#[folder = "assets"]
struct Assets;

#[derive(Clone)]
pub struct Web {
    daemon: Daemon,
    /// Host header values this server answers to.
    hosts: Arc<RwLock<Vec<String>>>,
    /// The connections that asked to be fenced (fence.rs): a page that
    /// comes back closes its own earlier connection.
    fences: Arc<Mutex<fence::Table<Arc<Fenced>>>>,
    /// How each other station's clock stands to this one's, from the pings
    /// on the connections it holds here for its pages' panes (typed.rs).
    /// One per station, not per connection: a station's clock is one clock.
    clocks: Arc<Mutex<HashMap<String, Clock>>>,
}

/// A sender's clock against this station's: written by the task that reads
/// its pings, read when something it typed is judged.
type Clock = Arc<Mutex<typed::Baseline>>;

/// How a connection is told that the page it belonged to has come back on
/// a newer one: it ends, and what it still had waiting to be typed is
/// thrown away.
#[derive(Default)]
struct Fenced {
    off: std::sync::atomic::AtomicBool,
    told: tokio::sync::Notify,
}

impl Fenced {
    fn close(&self) {
        self.off.store(true, Ordering::SeqCst);
        self.told.notify_one();
    }

    fn is_off(&self) -> bool {
        self.off.load(Ordering::SeqCst)
    }
}

/// A connection's place in the fence table, given up when it ends.
struct Entered {
    fences: Arc<Mutex<fence::Table<Arc<Fenced>>>>,
    key: fence::Key,
    ticket: u64,
}

impl Drop for Entered {
    fn drop(&mut self) {
        self.fences.lock().unwrap().leave(&self.key, self.ticket);
    }
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
    /// The same addresses over TLS, while a certificate is held (tls.rs).
    tls_port: u16,
    certs: Arc<tls::Certs>,
    acceptor: Option<tokio_rustls::TlsAcceptor>,
    tls_serving: Vec<Served>,
    /// Always wanted: localhost, or what `web_bind` names.
    fixed: Vec<IpAddr>,
    /// With no `web_bind`: this machine's Tailscale addresses, and its name
    /// there, as last read.
    tailnet: Vec<IpAddr>,
    dns: Option<String>,
    serving: Vec<Served>,
}

type Served = (IpAddr, SocketAddr, tokio::task::JoinHandle<()>);

/// Stop serving what is no longer wanted. Says whether anything went.
fn drop_gone(serving: &mut Vec<Served>, wanted: &[IpAddr], what: &str) -> bool {
    let (kept, gone): (Vec<_>, Vec<_>) = std::mem::take(serving)
        .into_iter()
        .partition(|(ip, _, _)| wanted.contains(ip));
    *serving = kept;
    for (ip, _, task) in &gone {
        task.abort();
        tracing::info!(%ip, "no longer listening{what}");
    }
    !gone.is_empty()
}

/// Bind what is wanted and not served yet.
async fn bind_new(
    serving: &[Served],
    wanted: &[IpAddr],
    port: u16,
) -> Vec<(IpAddr, SocketAddr, tokio::net::TcpListener)> {
    let mut fresh: Vec<(IpAddr, SocketAddr, tokio::net::TcpListener)> = vec![];
    for ip in wanted {
        if serving.iter().any(|(have, _, _)| have == ip)
            || fresh.iter().any(|(have, _, _)| have == ip)
        {
            continue;
        }
        match tokio::net::TcpListener::bind(SocketAddr::new(*ip, port)).await {
            Ok(listener) => match listener.local_addr() {
                Ok(addr) => fresh.push((*ip, addr, listener)),
                Err(e) => tracing::warn!(%ip, error = %e, "could not listen"),
            },
            // Tried again at the next round, ten seconds on.
            Err(e) => tracing::warn!(%ip, port, error = %e, "could not listen"),
        }
    }
    fresh
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
        // TLS on the same addresses, while there is a certificate to show.
        let wanted_tls = match (&self.acceptor, self.certs.held()) {
            (Some(_), true) => wanted.clone(),
            _ => vec![],
        };
        // Gone: stop accepting there first, then take it off the allowlist.
        let mut changed = drop_gone(&mut self.serving, &wanted, ": the address is gone");
        changed |= drop_gone(&mut self.tls_serving, &wanted_tls, " for HTTPS");
        // New, or not bound before: bind, learn the port, fill in the Host
        // allowlist, and only then start answering. A request must never
        // race the allowlist.
        let fresh = bind_new(&self.serving, &wanted, self.port).await;
        let fresh_tls = bind_new(&self.tls_serving, &wanted_tls, self.tls_port).await;
        self.web
            .daemon
            .https
            .set_serving(!self.tls_serving.is_empty() || !fresh_tls.is_empty());
        if !changed
            && fresh.is_empty()
            && fresh_tls.is_empty()
            && self.hosts() == *self.web.hosts.read().unwrap()
        {
            return;
        }
        let mut hosts = self.hosts();
        for (_, addr, _) in fresh.iter().chain(&fresh_tls) {
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
        for (ip, addr, listener) in fresh_tls {
            let Some(acceptor) = self.acceptor.clone() else {
                continue;
            };
            let daemon = self.web.daemon.clone();
            let task = tokio::spawn(tls::serve(
                listener,
                acceptor,
                self.app.clone(),
                move |tcp| no_delay(&daemon, tcp),
            ));
            tracing::info!(%addr, "Control Deck listening (HTTPS)");
            self.tls_serving.push((ip, addr, task));
        }
        *self.web.daemon.web_addrs.lock().unwrap() =
            self.serving.iter().map(|(_, a, _)| a.to_string()).collect();
    }

    /// The Host values answered to for what is being served.
    fn hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .serving
            .iter()
            .chain(&self.tls_serving)
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
        fences: Default::default(),
        clocks: Default::default(),
    };
    let app = Router::new()
        .route("/api/rpc", post(http_rpc))
        .route("/api/peer/rpc", post(peer_rpc))
        .route("/api/device/request", post(device_request))
        .route("/api/device/collect", post(device_collect))
        .route("/api/ws", get(ws_upgrade))
        .route("/api/ping", get(ping_upgrade))
        .route("/api/font/{id}", get(font))
        .route("/api/files/get", get(files::get))
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
    // HTTPS: what is held is asked for and renewed by a task of its own
    // (https.rs); this one serves it. A station that is given no
    // certificate serves everything over HTTP exactly as before.
    let certs = Arc::new(tls::Certs::default());
    let acceptor = tls::acceptor(certs.clone())
        .map_err(|e| tracing::warn!(error = %e, "HTTPS cannot be served"))
        .ok();
    let asking = daemon.clone();
    tokio::spawn(async move {
        let mut force = false;
        loop {
            let d = asking.clone();
            let _ = tokio::task::spawn_blocking(move || d.https_refresh(force)).await;
            // Until it is on (or off for good), asked again every minute:
            // Tailscale may not be up yet, or the owner is giving the
            // right to ask. After that only the machine's name is looked
            // at, and the certificate is asked for twice a day.
            let settled = matches!(asking.https_view()["state"].as_str(), Some("on" | "off"));
            let wait = std::time::Duration::from_secs(if settled { 600 } else { 60 });
            force = tokio::select! {
                _ = tokio::time::sleep(wait) => false,
                _ = asking.https.kick.notified() => true,
            };
        }
    });
    let mut pem = daemon.https.pem.subscribe();
    let mut listening = Listening {
        web,
        app,
        port,
        tls_port: daemon.cfg.web.https_port,
        certs,
        acceptor,
        tls_serving: vec![],
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
            let mut renewed = false;
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
                changed = own.changed(), if watching => watching = changed.is_ok(),
                got = pem.changed() => renewed = got.is_ok(),
            }
            // A certificate that is new (or gone) is the next handshake's;
            // the connections that are open are not touched.
            if renewed || !listening.certs.held() {
                let held = pem.borrow_and_update().clone();
                let cert = held.as_ref().map(|p| tls::certified(&p.cert, p.key()));
                match cert {
                    Some(Ok(cert)) => listening.certs.set(Some(cert)),
                    Some(Err(e)) => {
                        tracing::warn!(error = %e, "the HTTPS certificate cannot be used");
                        listening.certs.set(None);
                    }
                    None => listening.certs.set(None),
                }
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

/// Whether a request arrived over TLS (tls.rs).
struct Via(bool);

impl<S: Sync> axum::extract::FromRequestParts<S> for Via {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Via(parts.extensions.get::<tls::Secure>().is_some()))
    }
}

/// The page a browser's request came from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Page {
    /// This station's own deck, or no browser at all.
    Own,
    /// The deck of another member of this station's fleet, by its name:
    /// a page that lost its own station and carries on through this one.
    Deck(String),
}

/// Host must be one of ours; Origin, when a browser sends one, must match
/// it, and be `https` on the TLS port. `decks`: the one other case, for
/// the endpoints that allow it: the Origin is exactly the deck of a member
/// of this station's own roster (`Daemon::member_decks`).
fn origin_rule(
    hosts: &[String],
    host: Option<&str>,
    origin: Option<&str>,
    secure: bool,
    decks: Option<&[(String, String)]>,
) -> Result<Page, (StatusCode, &'static str)> {
    let Some(host) = host.map(str::to_lowercase) else {
        return Err((StatusCode::BAD_REQUEST, "missing Host"));
    };
    let any_port = format!(
        "{}:*",
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(&host)
    );
    if !hosts.contains(&host) && !hosts.contains(&any_port) {
        return Err((StatusCode::FORBIDDEN, "this Host is not served here"));
    }
    let Some(origin) = origin.map(str::to_lowercase) else {
        return Ok(Page::Own);
    };
    let (scheme, origin_host) = origin.split_once("://").unwrap_or(("", ""));
    if origin_host == host && (!secure || scheme == "https") {
        return Ok(Page::Own);
    }
    match decks.and_then(|d| d.iter().find(|(deck, _)| *deck == origin)) {
        Some((_, station)) => Ok(Page::Deck(station.clone())),
        None => Err((StatusCode::FORBIDDEN, "cross-origin requests are refused")),
    }
}

/// `decks`: this endpoint may be reached from another member's deck
/// (`/api/ws` and `/api/ping` only), while `page_failover` is on.
fn origin_ok(
    web: &Web,
    headers: &HeaderMap,
    secure: bool,
    decks: bool,
) -> Result<Page, (StatusCode, &'static str)> {
    let text = |name| {
        headers
            .get(name)
            .and_then(|h: &HeaderValue| h.to_str().ok())
    };
    let (host, origin) = (text(header::HOST), text(header::ORIGIN));
    let decks =
        (decks && origin.is_some() && web.daemon.flag_on(spandeck_core::https::VIA_FLAG, true))
            .then(|| web.daemon.member_decks());
    let mut hosts = web.hosts.read().unwrap().clone();
    // While a link's helper is attached: `localhost` and `127.0.0.1` with
    // the port the near side's browser uses (core: link/mod.rs). With no
    // link there is none, and nothing is added.
    hosts.extend(web.daemon.link_hosts());
    origin_rule(&hosts, host, origin, secure, decks.as_deref())
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
    page: &Page,
) -> Result<Caller, (StatusCode, &'static str)> {
    let client = client_id(headers, addr);
    match identify(web, addr.ip()).await {
        // Tailscale says which device is connecting, not which program on
        // it. So every caller also proves itself: a station in this fleet
        // with a signature, a browser with a token the owner approved.
        Remote::Loopback | Remote::Owner { .. } => {
            // A page loaded from another member's deck: only a browser's
            // own token, which every station of the fleet already takes.
            // Never this station's local token, a pane's, or a signature.
            if let Page::Deck(_) = page {
                let device = token
                    .filter(|t| t.starts_with("dev."))
                    .and_then(|t| web.daemon.device_for_token(&t))
                    .ok_or((StatusCode::UNAUTHORIZED, FROM_A_DECK))?;
                return Ok(Caller {
                    authority: Authority::Owner,
                    pane: None,
                    client,
                    who: format!("you (device {device})"),
                });
            }
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
        Remote::NoTailscale => Err((StatusCode::SERVICE_UNAVAILABLE, NO_TAILSCALE)),
    }
}

/// Said to whoever connects from another address while the local Tailscale
/// client does not say who they are. It is not "you are a stranger": the
/// page keeps trying, and is let in when Tailscale answers.
const NO_TAILSCALE: &str =
    "Tailscale is not answering here, so this station cannot tell who is connecting. It is asked again by itself.";

/// Said to a page loaded from another station's deck that shows anything
/// but a signed-in browser's token.
const FROM_A_DECK: &str =
    "sign in: from another station's deck only a signed-in browser's own token is accepted here";

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
    Via(secure): Via,
    Json(body): Json<Value>,
) -> axum::response::Response {
    if let Err(e) = origin_ok(&web, &headers, secure, false) {
        return refuse(e);
    }
    let who = match caller(&web, &addr, &headers, None, &Page::Own).await {
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
    Via(secure): Via,
    Json(body): Json<Value>,
) -> axum::response::Response {
    device_call(&web, &addr, &headers, secure, |ip| {
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
    Via(secure): Via,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let get = |k: &str| body.get(k).and_then(|v| v.as_str()).unwrap_or("");
    device_call(&web, &addr, &headers, secure, |ip| {
        web.daemon.trust_collect(get("id"), get("collect"), ip)
    })
    .await
}

async fn device_call(
    web: &Web,
    addr: &SocketAddr,
    headers: &HeaderMap,
    secure: bool,
    f: impl FnOnce(&str) -> spandeck_core::daemon::Result<Value>,
) -> axum::response::Response {
    if let Err(e) = origin_ok(web, headers, secure, false) {
        return refuse(e);
    }
    let remote = identify(web, addr.ip()).await;
    if remote == Remote::NoTailscale {
        return refuse((StatusCode::SERVICE_UNAVAILABLE, NO_TAILSCALE));
    }
    if matches!(remote, Remote::Stranger(_)) {
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
    let remote = identify(&web, addr.ip()).await;
    if remote == Remote::NoTailscale {
        return refuse((StatusCode::SERVICE_UNAVAILABLE, NO_TAILSCALE));
    }
    if matches!(remote, Remote::Stranger(_)) {
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
    // Something the owner typed at a deck another station serves, passed
    // on by that station with its own clock's time: refused when it is late
    // by the clock this station reads from that station's pings (typed.rs).
    // With no such reading (that station holds no connection here for a
    // pane, or is from before) it is taken, as before.
    if let Some(why) = forwarded_late(&web, &from, method, &params) {
        return Json(json!({"error": why})).into_response();
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

/// `rpc.forward` of something typed (`pane.input`, `pane.paste`,
/// `pane.send_text`) that the forwarding station stamped: why it is not
/// taken, when it is late.
fn forwarded_late(web: &Web, from: &str, method: &str, params: &Value) -> Option<Value> {
    let daemon = &web.daemon;
    if method != "rpc.forward"
        || !daemon.flag_on(typed::INPUT_FLAG, true)
        || !daemon.flag_on(beat::FLAG, true)
    {
        return None;
    }
    let kind = typed::Kind::of(params.get("method")?.as_str()?)?;
    let inner = params.get("params")?;
    let stamp = typed::Stamp::of(inner)?;
    let now = beat::mono_ms();
    let clock = web.clocks.lock().unwrap().get(from).cloned()?;
    let reading = clock
        .lock()
        .unwrap()
        .reading_for(inner.get("boot").and_then(|b| b.as_str()), now)?;
    let fresh_ms = typed::fresh_ms(daemon.flag(typed::FRESH_FLAG).as_ref());
    match typed::judge(Some(reading), stamp, now, kind, fresh_ms) {
        typed::Verdict::Late { age_ms, limit_ms } => {
            daemon.perf.typed_late.add();
            Some(late(&daemon.station, age_ms, limit_ms))
        }
        _ => None,
    }
}

/// A font the owner uploaded, by its id, for the owner's browsers. The id is
/// looked up in the look document; the bytes are a file a browser asked to be
/// kept, so they go out as a font and as nothing a browser might guess.
async fn font(
    State(web): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Via(secure): Via,
    Path(id): Path<String>,
) -> axum::response::Response {
    if let Err(e) = origin_ok(&web, &headers, secure, false) {
        return refuse(e);
    }
    match caller(&web, &addr, &headers, None, &Page::Own).await {
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

/// What the page's worker is given in place of the two marks in `sw.js`:
/// the build, and every file of it. The worker is the build: a station
/// that serves other files serves another worker.
fn shell_files() -> Vec<String> {
    let mut files: Vec<String> = Assets::iter()
        .map(|f| f.to_string())
        .filter(|f| f != "sw.js")
        .collect();
    files.sort();
    files
}

async fn asset(
    State(web): State<Web>,
    Via(secure): Via,
    headers: HeaderMap,
    uri: Uri,
) -> Response<Body> {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    // `[web] redirect = true`: a browser that asks the plain port for the
    // page at this machine's Tailscale name or address is sent to the
    // HTTPS deck. Only the page, only while HTTPS is served, never
    // localhost (the deck `spandeck cc` opens).
    if !secure && path == "index.html" && web.daemon.cfg.web.redirect {
        let https = web.daemon.https_view();
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let local =
            host.starts_with("localhost") || host.starts_with("127.") || host.starts_with('[');
        if let (true, false, Some(name)) =
            (https["on"] == json!(true), local, https["name"].as_str())
        {
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::TEMPORARY_REDIRECT;
            if let Ok(to) = HeaderValue::from_str(&format!("https://{name}:{}/", https["port"])) {
                resp.headers_mut().insert(header::LOCATION, to);
                resp.headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                return resp;
            }
        }
    }
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
                    let text = if path == "sw.js" {
                        // `[flags] app_shell = false`: the worker that
                        // takes the saved copy away.
                        let keep = web.daemon.flag_on("app_shell", true);
                        text.replace("__BUILD__", if keep { v } else { "" })
                            .replace(
                                "\"__FILES__\"",
                                &serde_json::to_string(&shell_files()).unwrap_or_default(),
                            )
                    } else if path.ends_with(".html") {
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
                        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self' ws: wss:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
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
    Via(secure): Via,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    let page = match origin_ok(&web, &headers, secure, true) {
        Ok(page) => page,
        Err(e) => return refuse(e),
    };
    if matches!(identify(&web, addr.ip()).await, Remote::Stranger(_)) {
        return refuse((
            StatusCode::FORBIDDEN,
            "this device is not under your Tailscale login",
        ));
    }
    ws.max_message_size(100 << 20)
        .on_upgrade(move |socket| ws_session(web, addr, headers, page, socket))
}

/// How far away this station is, for a page choosing which station to
/// work through: a socket that says who answers and then sends back what
/// it is sent, a few times. No token: it says only what `station.hello`
/// says to any device under the owner's login. A socket and not a plain
/// request, so that the page's content policy stays as it is (`connect-src
/// 'self' ws: wss:`) and no answer of this station is ever readable by
/// another origin's `fetch`.
async fn ping_upgrade(
    State(web): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Via(secure): Via,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if let Err(e) = origin_ok(&web, &headers, secure, true) {
        return refuse(e);
    }
    match identify(&web, addr.ip()).await {
        Remote::Stranger(_) => {
            return refuse((
                StatusCode::FORBIDDEN,
                "this device is not under your Tailscale login",
            ))
        }
        Remote::NoTailscale => return refuse((StatusCode::SERVICE_UNAVAILABLE, NO_TAILSCALE)),
        Remote::Loopback | Remote::Owner { .. } => {}
    }
    ws.max_message_size(256).on_upgrade(move |mut socket| async move {
        let hello = json!({"station": web.daemon.station, "version": spandeck_core::VERSION, "build": assets_build(), "caps": web.daemon.caps()});
        if socket
            .send(Message::Text(hello.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
        // Sixteen at most, within ten seconds: then it is closed.
        let end = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        for _ in 0..16 {
            match tokio::time::timeout_at(end, socket.next()).await {
                Ok(Some(Ok(Message::Text(t)))) if t.len() <= 64 => {
                    if socket.send(Message::Text(t)).await.is_err() {
                        return;
                    }
                }
                _ => return,
            }
        }
    })
}

enum Chan {
    Local {
        pane: String,
        control: bool,
        /// The number of the last thing typed on this attachment that was
        /// taken: a number is taken once (typed.rs).
        iseq: u64,
        task: tokio::task::JoinHandle<()>,
        /// In control: the pane counts this client among those that have
        /// its terminal open, until the channel is closed.
        _held: Option<spandeck_core::daemon::Holding>,
    },
    Remote {
        tx: mpsc::Sender<ToPeer>,
        peer_chan: u64,
        control: bool,
        iseq: u64,
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
    /// Names a page gave to attachments that are still being made.
    claimed: std::collections::HashSet<u32>,
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
            claimed: Default::default(),
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
            if !self.live.contains_key(&chan) && !self.claimed.contains(&chan) {
                return chan;
            }
        }
    }

    /// Take the name a page chose for a new attachment. Refused when it
    /// is 0 (which an answer without a number reads as) or names a channel
    /// this connection has open or is opening: a name is one attachment's.
    fn claim(&mut self, chan: u32) -> bool {
        chan != 0 && !self.live.contains_key(&chan) && self.claimed.insert(chan)
    }

    /// The attachment a name was taken for was not made.
    fn unclaim(&mut self, chan: u32) {
        self.claimed.remove(&chan);
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
        self.claimed.remove(&chan);
        self.live.insert(chan, what);
    }

    #[cfg(test)]
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
    /// Attachments are named by the page (the switch `deck_names`).
    names: bool,
    /// The page said it names every pane it attaches (`auth.caps`): one
    /// it does not name is refused, so that no pane of this connection
    /// has a number the page did not choose.
    page_names: bool,
    /// The page's instance (fence.rs), passed on to a gateway's fence.
    instance: Option<String>,
    /// Set when the page has come back on a newer connection.
    fenced: Arc<Fenced>,
    /// What is typed with a stamp is judged (the switch `deck_input`), and
    /// how late a key may be before the round trip is added.
    judged: bool,
    fresh_ms: u64,
}

/// One request as it was read.
struct Req {
    arrived: std::time::Instant,
    id: Value,
    method: String,
    params: Value,
    /// The request's id, and on a second try how long ago the first was
    /// written (rids.rs).
    retry: Option<Retry>,
    /// How the sender's clock stood to this station's when this was read
    /// off the connection (typed.rs).
    clock: Option<typed::Reading>,
}

impl Req {
    /// Is this a second try of a request written before?
    fn again(&self) -> bool {
        self.retry.as_ref().is_some_and(|r| r.again_ms.is_some())
    }
}

/// What is typed is never sent a second time: a second try of it is told
/// that what became of the first is not known.
fn typed_twice() -> Value {
    json!({"code": "outcome_unknown", "message": "could not confirm whether what was typed arrived. Nothing was sent again: look at the pane before typing it again."})
}

/// Where what is typed into a channel goes.
enum Typed {
    /// Not taken, and why.
    No(Value),
    /// A pane of this station.
    Here(String),
    /// A pane of another station, down the connection made for it:
    /// the request as that station is to read it (its own channel number).
    There {
        tx: mpsc::Sender<ToPeer>,
        method: String,
        params: Value,
        station: String,
        pane: String,
    },
    /// Numbered input whose number this attachment has taken already.
    Twice,
}

/// What goes down the connection a station holds to another for one pane.
enum ToPeer {
    /// Passed on as it is (a resize; input that was not stamped).
    Text(String),
    /// Something typed that was stamped: when (by this station's clock),
    /// the round trip so far, and how late it may be.
    Typed {
        method: String,
        params: Value,
        typed_ms: f64,
        rtt_ms: u64,
        kind: typed::Kind,
    },
}

/// Seconds as they are said: "5.2 s".
fn secs_words(ms: u64) -> String {
    format!("{}.{} s", ms / 1000, (ms % 1000) / 100)
}

/// What was typed and stamped was not taken: why, as an error and as what
/// is said on the pane's channel. Never what was typed.
fn late(station: &str, age_ms: u64, limit_ms: u64) -> Value {
    json!({"code": "late", "age_ms": age_ms, "limit_ms": limit_ms, "station": station, "message": format!(
        "not sent: this reached {station} {} after it was typed, and nothing typed is delivered more than {} late",
        secs_words(age_ms), secs_words(limit_ms)
    )})
}

fn not_ready(station: &str) -> Value {
    json!({"code": "not_ready", "station": station, "message": format!(
        "not sent: {station} has not yet read this connection's clock, so it cannot tell how old this is"
    )})
}

/// What was typed on a connection whose page has since come back on a
/// newer one, and had not been handed to its pane: it is thrown away.
fn replaced() -> Value {
    json!({"code": "replaced", "message": "this page has made a newer connection: what was still waiting on this one was not sent"})
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
        let who = &self.who;
        let watch = method == "pane.conversation.watch";
        if !who.is_owner() {
            return Err(denied(if watch {
                "a conversation is for the user's clients"
            } else {
                "attaching is for the user's clients"
            }));
        }
        let chan = self.name_for(watch, params)?;
        let made = self.attach_as(chan, watch, params).await;
        if made.is_err() {
            self.open.lock().unwrap().chans.unclaim(chan);
        }
        made
    }

    /// The number a new channel goes by: the one the page chose, when it
    /// chose one that is free on this connection; else the next of this
    /// connection's own.
    fn name_for(&self, watch: bool, params: &Value) -> Result<u32, Value> {
        let invalid = |message: String| json!({"code": "invalid", "message": message});
        let mut open = self.open.lock().unwrap();
        let asked = params.get("chan").filter(|c| !c.is_null());
        match asked {
            Some(asked) if self.names => {
                let chan = Channels::<Chan>::named(params)
                    .ok_or_else(|| invalid(format!("{asked} is not a channel number")))?;
                if !open.chans.claim(chan) {
                    return Err(invalid(format!(
                        "channel {chan} is already attached on this connection"
                    )));
                }
                Ok(chan)
            }
            // A pane's channel takes what is typed, so on a connection
            // whose page names them none is given a number the page did
            // not choose. (A watched conversation takes no typing.)
            None if self.names && self.page_names && !watch => Err(invalid(
                "this page names its attachments: pane.attach needs chan".into(),
            )),
            _ => Ok(open.chans.fresh()),
        }
    }

    async fn attach_as(&self, chan: u32, watch: bool, params: &Value) -> Result<Value, Value> {
        let (daemon, who) = (self.daemon(), &self.who);
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
                let held = match daemon.pane(&pane) {
                    Ok(entry) if control => Some(entry.hold(&who.client)),
                    _ => None,
                };
                self.opened(
                    chan,
                    Chan::Local {
                        pane,
                        control,
                        iseq: 0,
                        task,
                        _held: held,
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
                    Tab {
                        client: &who.client,
                        instance: self.instance.as_deref().filter(|_| self.names),
                    },
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
                        iseq: 0,
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
        let mut open = self.open.lock().unwrap();
        // Numbered input is taken once on its attachment.
        let numbered = (self.judged && method == "pane.input")
            .then(|| params.get("iseq").and_then(|n| n.as_u64()))
            .flatten();
        let named = Channels::<Chan>::named(params);
        let Some(chan) = named.and_then(|c| open.chans.live.get_mut(&c)) else {
            return Typed::No(json!({"code": "not_found", "message": "no such channel"}));
        };
        let (control, iseq) = match chan {
            Chan::Watch { .. } => {
                return Typed::No(denied("a watched conversation takes no typing"))
            }
            Chan::Local { control, iseq, .. } | Chan::Remote { control, iseq, .. } => {
                (*control, iseq)
            }
        };
        if !control {
            return watching();
        }
        if !typed::take_once(iseq, numbered) {
            return Typed::Twice;
        }
        match chan {
            Chan::Local { pane, .. } => Typed::Here(pane.clone()),
            Chan::Remote {
                tx,
                peer_chan,
                station,
                pane,
                ..
            } => {
                let mut p = params.clone();
                p["chan"] = json!(peer_chan);
                Typed::There {
                    tx: tx.clone(),
                    method: method.to_string(),
                    params: p,
                    station: station.clone(),
                    pane: pane.clone(),
                }
            }
            Chan::Watch { .. } => Typed::No(denied("a watched conversation takes no typing")),
        }
    }

    /// Is something typed still on time (typed.rs)? `Ok(None)`: it was not
    /// stamped, or the switch is off: taken as before. `Ok(Some(..))`: on
    /// time; when it was typed by this station's clock, and the round trip
    /// that counted. `Err`: refused, and why. Judged at the moment it
    /// would be handed on, by the clock as it stood when it was read.
    fn on_time(
        &self,
        clock: Option<typed::Reading>,
        method: &str,
        params: &Value,
    ) -> Result<Option<(f64, u64, typed::Kind)>, Value> {
        let (Some(kind), true) = (typed::Kind::of(method), self.judged) else {
            return Ok(None);
        };
        let Some(stamp) = typed::Stamp::of(params) else {
            return Ok(None);
        };
        let station = &self.daemon().station;
        match typed::judge(clock, stamp, beat::mono_ms(), kind, self.fresh_ms) {
            typed::Verdict::Take { typed_ms, rtt_ms } => Ok(Some((typed_ms, rtt_ms, kind))),
            typed::Verdict::NotReady => {
                self.daemon().perf.typed_refused.add();
                Err(not_ready(station))
            }
            typed::Verdict::Late { age_ms, limit_ms } => {
                self.daemon().perf.typed_late.add();
                Err(late(station, age_ms, limit_ms))
            }
        }
    }

    /// Something typed into a channel was not taken: said on that channel,
    /// since a key asks for no answer. Which one (its number), why, and how
    /// late; never what it was.
    fn input_dropped(&self, params: &Value, why: &Value) {
        if let Some(chan) = Channels::<Chan>::named(params) {
            let mut said = json!({"chan": chan, "input_dropped": 1, "why": why["code"]});
            for k in ["age_ms", "limit_ms", "station"] {
                if let Some(v) = why.get(k) {
                    said[k] = v.clone();
                }
            }
            if let Some(n) = params.get("iseq").and_then(|n| n.as_u64()) {
                said["iseq"] = json!(n);
            }
            self.say(Some(Message::Text(said.to_string().into())));
        }
    }

    /// Carry a request out. `retry`: its id, when it came with one, so
    /// that a change is done once whichever lane it runs in (rids.rs).
    async fn dispatch(
        &self,
        method: &str,
        params: Value,
        retry: Option<Retry>,
        at: AtRead,
    ) -> Result<Value, Value> {
        self.daemon()
            .dispatch_read(&self.who, method, params, retry, at)
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
    attaches_to: HashMap<String, Arc<Semaphore>>,
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
            attaches_to: HashMap::new(),
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
            let reply = match conn.on_time(req.clock, method, &req.params) {
                Err(why) => Err(why),
                Ok(_) => {
                    conn.dispatch(
                        method,
                        req.params.clone(),
                        req.retry.clone(),
                        AtRead::default(),
                    )
                    .await
                }
            };
            return conn.say(conn.done(&req, Some(lane), began, reply));
        }
        // A station that has stopped answering: what is asked of it is
        // refused at once, where each call would wait out its own five
        // seconds, but for one call every two seconds that finds out
        // whether it is back. A Stop, a Pause, a Kill or an answer for it
        // is always sent.
        let mut finds_out = false;
        if let Some(to) = station.as_deref() {
            let fleet = &conn.daemon().fleet;
            let silent = fleet.not_answering(to);
            if let Some(since) = lanes::held_back(lane, silent, || fleet.probe(to)) {
                conn.daemon().perf.busy.add();
                let for_ms = spandeck_core::paths::now_ms().saturating_sub(since);
                let no = lanes::not_answering(to, for_ms, self.page_knows_busy);
                return conn.say(answer(&req.id, Err(no)));
            }
            // Let through to find out: not asked again at its turn.
            finds_out = silent.is_some();
        }
        // Attaching to a pane of another station is counted for that
        // station and has that station's places: it waits for that station
        // (four seconds to connect, five for its answer) and must hold up
        // no attach here or anywhere else.
        let attach_to = (lane == Lane::Attach)
            .then(|| req.params.get("station").and_then(|s| s.as_str()))
            .flatten()
            .filter(|s| *s != conn.daemon().station)
            .map(String::from);
        // Not asked of a station that is not answering (it would wait out
        // four seconds to connect), but for the one that finds out.
        if let Some(to) = attach_to.as_deref() {
            let fleet = &conn.daemon().fleet;
            let silent = fleet.not_answering(to);
            if let Some(since) = lanes::held_back(lane, silent, || fleet.probe(to)) {
                conn.daemon().perf.busy.add();
                let for_ms = spandeck_core::paths::now_ms().saturating_sub(since);
                let no = lanes::not_answering(to, for_ms, self.page_knows_busy);
                return conn.say(answer(&req.id, Err(no)));
            }
        }
        let counted = match self
            .counts
            .admit(station.as_deref().or(attach_to.as_deref()), lane)
        {
            Ok(c) => c,
            Err(busy) => return self.busy(&req, &busy),
        };
        // A stop's place in a pane's input is where it is read: here,
        // unless something typed for that pane before it is still waiting
        // its turn, when the stop takes its place at its own turn.
        let mut at = AtRead::default();
        let mut place = None;
        let stop_all = station.is_none() && route.method == "stop.all";
        // A second try of a stop takes nothing here: it is answered from
        // the record of the first, or, with none, stops as any stop does
        // and takes its places when it runs.
        let again = req.again();
        if station.is_none() && lane == Lane::Urgent && conn.who.is_owner() && !again {
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
        if !stop_all || (at.legs.is_empty() && !again) {
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
            Lane::Attach => Some(
                match attach_to.filter(|s| conn.daemon().fleet.peer(s).is_some()) {
                    Some(s) => self
                        .attaches_to
                        .entry(s)
                        .or_insert_with(|| Arc::new(Semaphore::new(lanes::READS_PER_STATION)))
                        .clone(),
                    None => self.attaches.clone(),
                },
            ),
            _ => None,
        };
        let knows = self.page_knows_busy;
        // The parameters of the call that acts, for what is said if a
        // station gives no answer.
        let route_params = match req.method.as_str() {
            "peer.call" => req.params.get("params").cloned().unwrap_or(Value::Null),
            _ => req.params.clone(),
        };
        let ends_with_connection = route.ends_with_connection;
        let task = async move {
            let _counted = counted;
            // Its place among the reads, and its turn in its line. For
            // another station that wait has an end: a call that has stood
            // four seconds behind others for that station is not sent, and
            // is told so (nothing was changed), so what it costs its caller
            // is bounded whatever that station does.
            let waits = async {
                let held = match places {
                    Some(places) => places.acquire_owned().await.ok(),
                    None => None,
                };
                if let Some(place) = &place {
                    place.turn().await;
                }
                held
            };
            let began;
            let reply = match &station {
                None => {
                    let _held = waits.await;
                    began = std::time::Instant::now();
                    match lane {
                        Lane::Attach => conn.attach(&req.method, &req.params).await,
                        Lane::Input if conn.fenced.is_off() => Err(replaced()),
                        // Judged now, after its wait, not when it was read.
                        _ => match conn.on_time(req.clock, &route.method, &req.params) {
                            Err(why) => Err(why),
                            Ok(_) => {
                                conn.dispatch(
                                    &req.method,
                                    req.params.clone(),
                                    req.retry.clone(),
                                    at,
                                )
                                .await
                            }
                        },
                    }
                }
                Some(to) => {
                    let held = tokio::time::timeout(lanes::STATION_WAIT, waits).await;
                    began = std::time::Instant::now();
                    let fleet = &conn.daemon().fleet;
                    // While it waited the station may have stopped
                    // answering: then it is not sent either (a Stop is).
                    let silent = lanes::held_back(lane, fleet.not_answering(to), || {
                        finds_out || fleet.probe(to)
                    });
                    match (held, silent) {
                        (Err(_), _) => Err(lanes::not_sent(to, knows)),
                        (Ok(_), _) if lane == Lane::Input && conn.fenced.is_off() => {
                            Err(replaced())
                        }
                        (Ok(_), Some(since)) => {
                            let for_ms = spandeck_core::paths::now_ms().saturating_sub(since);
                            Err(lanes::not_answering(to, for_ms, knows))
                        }
                        // Given up here after the deadline, whatever the
                        // call underneath is doing.
                        (Ok(_held), None) => {
                            // Something typed for a pane there, by the
                            // pane's name (a phone typing into a pane it
                            // watches, the message box): judged here for
                            // the way from the page, then stamped with
                            // this station's clock for the way on.
                            match conn.on_time(req.clock, &route.method, &route_params) {
                                Err(why) => Err(why),
                                Ok(stamped) => {
                                    let mut params = req.params.clone();
                                    if let (Some((typed_ms, rtt_ms, _)), "peer.call") =
                                        (stamped, req.method.as_str())
                                    {
                                        let gone = rtt_ms
                                            + conn
                                                .daemon()
                                                .perf
                                                .rtt_ms_of(to)
                                                .unwrap_or(0)
                                                .min(typed::RTT_MAX_MS);
                                        params["params"]["t"] = json!(typed_ms);
                                        params["params"]["rtt"] = json!(gone);
                                        params["params"]["boot"] = json!(typed::boot());
                                    }
                                    let call =
                                        conn.dispatch(&req.method, params, req.retry.clone(), at);
                                    match tokio::time::timeout(lanes::STATION_DEADLINE, call).await
                                    {
                                        Ok(reply) => reply,
                                        Err(_) => Err(lanes::no_answer(
                                            to,
                                            &route.method,
                                            spandeck_core::rpc::repeatable(
                                                &route.method,
                                                &route_params,
                                            ),
                                            knows,
                                        )),
                                    }
                                }
                            }
                        }
                    }
                }
            };
            drop(place);
            conn.finish(&req, lane, began, reply).await;
        };
        if ends_with_connection {
            self.ending.spawn(task);
        } else {
            self.lasting.spawn(task);
        }
    }

    /// `pane.input` and `pane.resize` for a channel.
    async fn typed(&mut self, mut req: Req) {
        let conn = self.conn.clone();
        let began = std::time::Instant::now();
        // What was typed and not taken is said on its channel.
        let dropped = |conn: &Conn, req: &Req, why: Value| {
            conn.input_dropped(&req.params, &why);
            Err(why)
        };
        match conn.typed(&req.method, &req.params) {
            Typed::No(why) => conn.say(conn.done(&req, Some(Lane::Input), began, Err(why))),
            Typed::Twice => {
                conn.daemon().perf.typed_twice.add();
                let no = Err(typed_twice());
                conn.say(conn.done(&req, Some(Lane::Input), began, no))
            }
            Typed::Here(pane) => {
                let scope = Scope::Pane(pane.clone());
                req.params["pane"] = json!(pane);
                if self.lines.free("", &scope) {
                    let reply = match conn.on_time(req.clock, &req.method, &req.params) {
                        Err(why) => dropped(&conn, &req, why),
                        Ok(_) => {
                            conn.dispatch(
                                &req.method,
                                req.params.clone(),
                                req.retry.clone(),
                                AtRead::default(),
                            )
                            .await
                        }
                    };
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
                    // Judged now, after its wait: a key that stood behind
                    // a stop for longer than its limit is not written.
                    let reply = if conn.fenced.is_off() {
                        Err(replaced())
                    } else {
                        match conn.on_time(req.clock, &req.method, &req.params) {
                            Err(why) => dropped(&conn, &req, why),
                            Ok(_) => {
                                conn.dispatch(
                                    &req.method,
                                    req.params.clone(),
                                    req.retry.clone(),
                                    AtRead::default(),
                                )
                                .await
                            }
                        }
                    };
                    drop(place);
                    conn.finish(&req, Lane::Input, began, reply).await;
                });
            }
            Typed::There {
                tx,
                method,
                params,
                station,
                pane,
            } => {
                // Handed to the connection made for that pane, never
                // waited for: when that station takes nothing and 256
                // things already wait, this one is refused and the page
                // told, where it used to stop every request of this page.
                if req.again() {
                    let no = Err(typed_twice());
                    return conn.say(conn.done(&req, Some(Lane::Input), began, no));
                }
                let scope = Scope::Pane(pane);
                let (to, knows) = (station.clone(), self.page_knows_busy);
                // Judged here for the way from the page, at the moment it
                // is handed on; the connection for that pane judges the
                // way on (`attach_remote`), and that station the whole.
                let hand = move |conn: &Conn, req: &Req| {
                    let item = match conn.on_time(req.clock, &method, &req.params) {
                        Err(why) => return dropped(conn, req, why),
                        Ok(None) => ToPeer::Text(
                            json!({"id": null, "method": method, "params": params}).to_string(),
                        ),
                        Ok(Some((typed_ms, rtt_ms, kind))) => ToPeer::Typed {
                            method,
                            params,
                            typed_ms,
                            rtt_ms,
                            kind,
                        },
                    };
                    match tx.try_send(item) {
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
                    }
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
                    // Not kept for later than the four seconds any call
                    // for another station may wait its turn.
                    let turn = tokio::time::timeout(lanes::STATION_WAIT, place.turn()).await;
                    let began = std::time::Instant::now();
                    let reply = match turn {
                        Ok(()) if conn.fenced.is_off() => Err(replaced()),
                        Ok(()) => hand(&conn, &req),
                        Err(_) => {
                            conn.input_refused(&req.params, &station);
                            Err(lanes::not_sent(&station, knows))
                        }
                    };
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
                Typed::Twice => {
                    conn.daemon().perf.typed_twice.add();
                    Err(typed_twice())
                }
                Typed::Here(pane) => {
                    let mut p = req.params.clone();
                    p["pane"] = json!(pane);
                    match conn.on_time(req.clock, &req.method, &p) {
                        Err(why) => {
                            conn.input_dropped(&req.params, &why);
                            Err(why)
                        }
                        Ok(_) => {
                            conn.dispatch(&req.method, p, req.retry.clone(), AtRead::default())
                                .await
                        }
                    }
                }
                Typed::There { .. } if req.again() => Err(typed_twice()),
                Typed::There {
                    tx, method, params, ..
                } => match conn.on_time(req.clock, &method, &req.params) {
                    Err(why) => {
                        conn.input_dropped(&req.params, &why);
                        Err(why)
                    }
                    Ok(stamped) => {
                        let item = match stamped {
                            None => ToPeer::Text(
                                json!({"id": null, "method": method, "params": params}).to_string(),
                            ),
                            Some((typed_ms, rtt_ms, kind)) => ToPeer::Typed {
                                method,
                                params,
                                typed_ms,
                                rtt_ms,
                                kind,
                            },
                        };
                        let _ = tx.send(item).await;
                        Ok(json!({"ok": true}))
                    }
                },
            },
            // Something typed by the pane's name: for this station, judged
            // here; for another, by the station it is passed to.
            m => match conn.on_time(req.clock, m, &req.params) {
                Err(why) => Err(why),
                Ok(_) => {
                    conn.dispatch(
                        &req.method,
                        req.params.clone(),
                        req.retry.clone(),
                        AtRead::default(),
                    )
                    .await
                }
            },
        };
        if let Some(msg) = conn.done(&req, None, began, reply) {
            let _ = conn.out.send(msg).await;
        }
    }
}

async fn ws_session(web: Web, addr: SocketAddr, headers: HeaderMap, page: Page, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    // The token travels in the first message, never in a cookie or the URL.
    let first = match tokio::time::timeout(std::time::Duration::from_secs(10), stream.next()).await
    {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Value>(&t).unwrap_or(Value::Null),
        _ => return,
    };
    let auth = first.get("auth").cloned().unwrap_or(Value::Null);
    let token = auth.get("token").and_then(|t| t.as_str()).map(String::from);
    let mut who = match caller(&web, &addr, &headers, token, &page).await {
        Ok(c) => c,
        Err((_, msg)) => {
            let _ = sink
                .send(Message::Text(
                    // `again`: nothing is wrong with this browser; the
                    // page keeps trying by itself.
                    json!({"auth": {"ok": false, "message": msg, "again": msg == NO_TAILSCALE}})
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
    let names = web.daemon.flag_on(typed::NAMES_FLAG, true);
    let page_names = auth["caps"]
        .as_array()
        .is_some_and(|caps| caps.iter().any(|c| c == typed::NAMES_CAP));
    // The same page come back (or the same page's same pane, through a
    // gateway) closes the connection it had before, here, before this one
    // is told it is signed in: nothing the older one still holds is done
    // after anything this one sends. The owner's connections only, and
    // only one that asks (fence.rs).
    let from_station = who.who.starts_with("you (via ");
    let fenced = Arc::new(Fenced::default());
    let _entered = (names && who.is_owner())
        .then(|| fence::Key::from_auth(&who.who, &who.client, from_station, &auth))
        .flatten()
        .map(|key| {
            let (ticket, older) = web
                .fences
                .lock()
                .unwrap()
                .enter(key.clone(), fenced.clone());
            if let Some(older) = older {
                web.daemon.perf.fenced.add();
                older.close();
            }
            Entered {
                fences: web.fences.clone(),
                key,
                ticket,
            }
        });
    let instance = fence::instance_of(auth.get("instance")).map(String::from);
    // What is typed with a stamp is judged against this connection's
    // pings (typed.rs). Another station's connection for one pane shares
    // that station's one clock with its others.
    let beats = web.daemon.flag_on(beat::FLAG, true);
    let judged = beats && web.daemon.flag_on(typed::INPUT_FLAG, true);
    let clock: Clock = match who.who.strip_prefix("you (via ") {
        Some(rest) if from_station => web
            .clocks
            .lock()
            .unwrap()
            .entry(rest.trim_end_matches(')').to_string())
            .or_default()
            .clone(),
        _ => Default::default(),
    };
    // A page that carries on through this station after losing its own:
    // said in the ship's log, and to the page.
    let via = match &page {
        Page::Deck(station) => {
            web.daemon.record(
                "deck.via",
                None,
                None,
                None,
                Some(&who.who),
                json!({"from": station, "client": who.client}),
            );
            Some(station.clone())
        }
        Page::Own => None,
    };
    let hello = json!({"auth": {"ok": true, "station": web.daemon.station, "version": spandeck_core::VERSION, "build": assets_build(), "client": who.client, "full": who.is_owner(), "device": device, "lanes": in_lanes, "caps": web.daemon.caps(), "via": via}});
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
        names,
        page_names,
        instance,
        fenced: fenced.clone(),
        judged,
        fresh_ms: typed::fresh_ms(web.daemon.flag(typed::FRESH_FLAG).as_ref()),
    };
    let who = conn.who.clone();
    let mut running = Running::new(conn.clone(), page_knows_busy);

    // Each request is stamped as it arrives, so that how long it then stood
    // before it was begun is known (the Performance page). They are read in
    // the order they came; up to 64 wait here, and past that the socket is
    // not read, as before. Reading one never waits for the one before it
    // (lanes.rs), unless the switch `ws_lanes` is off.
    type Read = (std::time::Instant, Option<typed::Reading>, Message);
    let (in_tx, mut in_rx) = mpsc::channel::<Read>(64);
    // The heartbeat (beat.rs). When anything at all was last read from the
    // other end is kept by the task that reads, so that it is right while
    // requests wait to be taken. A page's ping is answered there too, by
    // nothing that can wait: its answer says the link and this station
    // are alive, whatever the requests before it are waiting for.
    let conn_began = std::time::Instant::now();
    let heard_ms = Arc::new(AtomicU64::new(0));
    let (heard, pong_tx) = (heard_ms.clone(), out_tx.clone());
    let reader = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            let now = std::time::Instant::now();
            heard.store(
                now.duration_since(conn_began).as_millis() as u64,
                Ordering::Relaxed,
            );
            // A ping is also how the sender's clock is read (typed.rs),
            // here and only here: what is typed never sets it. Everything
            // else goes on with the clock as it stood when it was read, so
            // that a sender which then says it was away does not make what
            // it sent before look fresh.
            if beats {
                if let Some((ping, pong)) = pong_for(&msg) {
                    if judged {
                        clock.lock().unwrap().ping(beat::mono_ms(), &ping);
                    }
                    let _ = pong_tx.try_send(pong);
                    continue;
                }
            }
            let reading = clock.lock().unwrap().reading();
            if in_tx.send((now, reading, msg)).await.is_err() {
                break;
            }
        }
    });
    // Another station's connection for one pane is judged in seconds (the
    // owner is looking at that pane); a deck's in half a minute, which is
    // only ever a page that is gone.
    let mut pulse = Beat::new(
        if from_station {
            beat::STATION
        } else {
            beat::DECK
        },
        conn_began,
    );

    loop {
        let msg = tokio::select! {
            m = in_rx.recv() => m,
            // The page has come back on a newer connection: this one is
            // over, with whatever it had not yet read.
            _ = fenced.told.notified() => break,
            _ = rekeyed.changed(), if rests_on_key => {
                key_replaced = true;
                break;
            }
            _ = tokio::time::sleep_until(pulse.due().into()), if beats => {
                pulse.heard(conn_began + std::time::Duration::from_millis(heard_ms.load(Ordering::Relaxed)));
                match pulse.tick(std::time::Instant::now()) {
                    // Never waited for: with the way out full, the frames
                    // already in it are what the other end hears.
                    Verdict::Ping => {
                        let _ = out_tx.try_send(Message::Ping(Default::default()));
                        continue;
                    }
                    Verdict::Wait => continue,
                    Verdict::Dead => {
                        let perf = &web.daemon.perf;
                        if from_station { perf.silent_gateways.add() } else { perf.silent_decks.add() }
                        break;
                    }
                }
            }
            // Tasks that are done are let go of as they finish.
            Some(_) = running.ending.join_next(), if !running.ending.is_empty() => continue,
            Some(_) = running.lasting.join_next(), if !running.lasting.is_empty() => continue,
        };
        let Some((arrived, clock, msg)) = msg else {
            break;
        };
        if fenced.is_off() {
            break;
        }
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(req) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let retry = match Retry::parse(req.get("rid"), req.get("again_ms")) {
            Ok(retry) => retry,
            Err(why) => {
                conn.say(answer(&id, Err(json!({"code": "invalid", "message": why}))));
                continue;
            }
        };
        let req = Req {
            arrived,
            clock,
            retry,
            id,
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

/// A page's ping (`{"ping": n, "t": …}`) and its answer: the number and
/// the page's own time back, and this station's forward-only clock (`s`).
/// It is a frame, not a request: it names no method, is on no method list,
/// and is answered whoever is signed in on the connection.
fn pong_for(msg: &Message) -> Option<(Value, Message)> {
    let Message::Text(text) = msg else {
        return None;
    };
    if text.len() > 256 || !text.as_str().starts_with("{\"ping\"") {
        return None;
    }
    let ping: Value = serde_json::from_str(text.as_str()).ok()?;
    let mut pong = json!({"pong": ping.get("ping")?.as_u64()?, "s": beat::mono_ms()});
    if let Some(t) = ping.get("t").and_then(|t| t.as_f64()) {
        pong["t"] = json!(t);
    }
    Some((ping, Message::Text(pong.to_string().into())))
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
    mpsc::Sender<ToPeer>,
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
    fence: Option<Value>,
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
    let made = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        // The same towards the other station: a key typed through this
        // one is a small piece too.
        tokio_tungstenite::connect_async_with_config(req, None, daemon.flag_on(NODELAY, true)),
    )
    .await;
    // Never the transport's own words: they name the address.
    let (ws, _) = match made {
        Ok(Ok(made)) => made,
        // It answered, and would not have this station.
        Ok(Err(tungstenite::Error::Http(_))) => return Err(refused_by(daemon, station)),
        Ok(Err(e)) => {
            daemon.fleet.connection_tried(station, false);
            let io = match &e {
                tungstenite::Error::Io(io) => Some(io.kind()),
                _ => None,
            };
            use spandeck_core::fleet::NoAnswer;
            return Err(match NoAnswer::from(true, false, io) {
                NoAnswer::NoConnection => format!("{station} could not be reached"),
                why => format!("{station} {}", why.words()),
            });
        }
        Err(_) => {
            daemon.fleet.connection_tried(station, false);
            return Err(format!("{station} could not be reached within 4 s"));
        }
    };
    let (mut peer_sink, peer_stream) = ws.split();
    // `fence`: this is one tab's connection for one pane, and replaces
    // that tab's earlier one for that pane (fence.rs). A station from
    // before passes over it.
    let mut auth = json!({"client": gateway_client(&daemon.station, client)});
    if let Some(fence) = fence {
        auth["fence"] = fence;
    }
    peer_sink
        .send(tungstenite::Message::Text(
            json!({"auth": auth}).to_string().into(),
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

type PeerSink = futures_util::stream::SplitSink<PeerSocket, tungstenite::Message>;

/// What became of something sent to the other station.
enum Sending {
    Sent,
    /// The connection is closed.
    Closed,
    /// It could not even be written for a ping's interval: the other end
    /// has taken nothing for that long. Silent for this many ms.
    Stuck(u64),
}

/// The heartbeat of a connection this station holds to another for one
/// pane (beat.rs: a ping every 2 s, given up after 6 s with nothing heard),
/// and what it tells the rest of the station: the same "not answering"
/// state that the calls to that station feed (`FleetState::not_answering`).
struct Pulse {
    daemon: Daemon,
    station: String,
    on: bool,
    beat: Beat,
    /// The pings that carry this station's clock (typed.rs): the last one's
    /// number, the round trip the last answer showed, and whether this
    /// station was itself away since the last one.
    ping_n: u64,
    rtt_ms: u64,
    was_away: bool,
}

impl Pulse {
    fn new(daemon: &Daemon, station: &str) -> Pulse {
        let beat = Beat::new(beat::STATION, std::time::Instant::now());
        Pulse {
            daemon: daemon.clone(),
            station: station.to_string(),
            // Read once: a connection keeps the way it began.
            on: daemon.flag_on(beat::FLAG, true),
            beat,
            ping_n: 0,
            rtt_ms: 0,
            was_away: false,
        }
    }

    /// This station's clock, for the other one to read: a text ping, as a
    /// page sends one. `boot` names this run of the station; `fresh` says
    /// this station was not running for a while (it slept, or was frozen),
    /// so its clock may have stood still and the readings from before prove
    /// nothing. A station from before passes it over.
    fn clock_ping(&mut self) -> tungstenite::Message {
        self.ping_n += 1;
        // Written out, with `ping` first: that is how the reader at the
        // other end knows a ping without parsing every request twice.
        let fresh = if std::mem::take(&mut self.was_away) {
            r#","fresh":true"#
        } else {
            ""
        };
        let ping = format!(
            r#"{{"ping":{},"t":{},"rtt":{},"boot":"{}"{fresh}}}"#,
            self.ping_n,
            beat::mono_ms(),
            self.rtt_ms,
            typed::boot()
        );
        tungstenite::Message::Text(ping.into())
    }

    /// The answer to a clock ping: the round trip of this hop.
    fn clock_pong(&mut self, pong: &Value) {
        if let Some(t) = pong.get("t").and_then(|t| t.as_f64()) {
            self.rtt_ms = ((beat::mono_ms() as f64 - t).max(0.0) as u64).min(typed::RTT_MAX_MS);
        }
    }

    /// Has the other station said nothing for so long that what is typed
    /// is not passed on to it? For how long, then.
    fn silent_ms(&self) -> Option<u64> {
        let quiet = self.beat.quiet(std::time::Instant::now()).as_millis() as u64;
        let after = typed::silent_after_ms(beat::STATION.0.as_millis() as u64, self.rtt_ms);
        (self.on && quiet > after).then_some(quiet)
    }

    /// Something came from the other station (or its connection ended).
    fn heard<E>(&mut self, from_peer: &Option<Result<tungstenite::Message, E>>) {
        let Some(Ok(msg)) = from_peer else {
            return;
        };
        self.beat.heard(std::time::Instant::now());
        // A station taken to be silent has answered a ping: it is back,
        // and what is asked of it is sent again from now, not at the next
        // call that is let through to find out.
        if matches!(msg, tungstenite::Message::Pong(_))
            && self.daemon.fleet.not_answering(&self.station).is_some()
        {
            self.daemon.fleet.heartbeat(&self.station, None);
        }
    }

    /// Send, without ever waiting longer than a ping's interval: a write
    /// that waits is the other end taking nothing, and must not stop this
    /// connection's own clock.
    async fn send(&mut self, sink: &mut PeerSink, msg: tungstenite::Message) -> Sending {
        match tokio::time::timeout(beat::STATION.0, sink.send(msg)).await {
            Ok(Ok(())) => Sending::Sent,
            Ok(Err(_)) => Sending::Closed,
            Err(_) => Sending::Stuck(self.lost()),
        }
    }

    /// The next beat: a ping goes out. `Some(ms)` when the other station
    /// has said nothing for six seconds: for how long.
    async fn tick(&mut self, sink: &mut PeerSink) -> Option<u64> {
        if !self.on {
            return std::future::pending().await;
        }
        let due = self.beat.due();
        tokio::time::sleep_until(due.into()).await;
        let now = std::time::Instant::now();
        // More than a whole interval late: this station was not running.
        self.was_away |= now.saturating_duration_since(due) > beat::STATION.0;
        match self.beat.tick(now) {
            Verdict::Dead => Some(self.lost()),
            Verdict::Wait => None,
            Verdict::Ping => {
                let clock = self.clock_ping();
                let _ = self.send(sink, clock).await;
                match self
                    .send(sink, tungstenite::Message::Ping(Default::default()))
                    .await
                {
                    Sending::Stuck(quiet_ms) => Some(quiet_ms),
                    // A closed connection is found by the read.
                    _ => None,
                }
            }
        }
    }

    /// Given up for silence: counted, and the station is not answering
    /// from when it was last heard.
    fn lost(&self) -> u64 {
        let quiet_ms = self.beat.quiet(std::time::Instant::now()).as_millis() as u64;
        let last_heard = spandeck_core::paths::now_ms().saturating_sub(quiet_ms);
        self.daemon.fleet.heartbeat(&self.station, Some(last_heard));
        self.daemon.perf.silent_panes.add();
        quiet_ms
    }

    /// What the page is told on the pane's channel. `unreachable` is the
    /// message a page from before knows; `station` and `quiet_ms` (when it
    /// was silence, not a closed connection) are for one that says more.
    fn gone(&self, chan: u32, quiet_ms: Option<u64>) -> Message {
        let station: String = self.station.chars().take(64).collect();
        let mut said = json!({"chan": chan, "unreachable": true, "station": station});
        if let Some(ms) = quiet_ms {
            said["quiet_ms"] = json!(ms);
        }
        Message::Text(said.to_string().into())
    }
}

/// The browser tab a gateway attaches for: its client id here, and its
/// page's instance when the page gave one (and the switch `deck_names` is
/// on here).
struct Tab<'a> {
    client: &'a str,
    instance: Option<&'a str>,
}

/// Gateway: attach to a pane on another station and relay its stream, so the
/// browser keeps a single connection to the station it opened. `client` is
/// the browser tab the attach is for.
async fn attach_remote(
    daemon: &Daemon,
    station: &str,
    pane: &str,
    control: bool,
    tab: Tab<'_>,
    chan: u32,
    out: mpsc::Sender<Message>,
) -> Result<RemoteAttach, String> {
    // The tab's page and the pane: the same page attaching this pane
    // again closes this connection at the pane's station.
    let fence = tab.instance.map(|i| json!({"pane": pane, "instance": i}));
    let (mut peer_sink, mut peer_stream) = peer_socket(daemon, station, tab.client, fence).await?;
    let send = |v: Value| tungstenite::Message::Text(v.to_string().into());
    let mut pulse = Pulse::new(daemon, station);
    // This station's clock first, so that the pane's station can tell how
    // old the first key is.
    if pulse.on {
        let clock = pulse.clock_ping();
        peer_sink.send(clock).await.map_err(|e| e.to_string())?;
    }
    // Does that station judge what is typed itself (`caps`, in its hello)?
    let mut peer_judges = false;
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
                if let Some(caps) = v["auth"]["caps"].as_array() {
                    peer_judges = caps.iter().any(|c| c == typed::INPUT_CAP);
                }
                if v.get("pong").is_some() {
                    pulse.clock_pong(&v);
                }
                if v.get("id") == Some(&json!(1)) {
                    if let Some(e) = v.get("error") {
                        return Err(e["message"].as_str().unwrap_or("attach failed").to_string());
                    }
                    break v["result"].clone();
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(_) => return Err(format!("{station} closed the connection")),
            Err(_) => {
                daemon.fleet.connection_tried(station, false);
                return Err(format!("{station} did not answer within 5 s"));
            }
        }
    };
    daemon.fleet.connection_tried(station, true);
    let peer_chan = result["chan"].as_u64().unwrap_or(0);
    result["chan"] = json!(chan);
    result["station"] = json!(station);
    // Who this tab is there: what a `resized` message names when it was us.
    result["client"] = me;
    let pane_id = result["pane"].as_str().unwrap_or(pane).to_string();

    let (tx, mut rx) = mpsc::channel::<ToPeer>(256);
    let fresh_ms = typed::fresh_ms(daemon.flag(typed::FRESH_FLAG).as_ref());
    let perf = daemon.perf.clone();
    let to = station.chars().take(64).collect::<String>();
    let task = tokio::spawn(async move {
        // Why the loop ended, when it is the other station's doing: for
        // how long it had said nothing, or that the connection closed.
        let ended: Option<Option<u64>> = loop {
            tokio::select! {
                dead = pulse.tick(&mut peer_sink) => if let Some(quiet_ms) = dead { break Some(Some(quiet_ms)) },
                to_peer = rx.recv() => {
                    let text = match to_peer {
                        Some(ToPeer::Text(text)) => text,
                        // Something typed, with its time. Not written into
                        // a connection whose other end has said nothing
                        // (it would be delivered whenever that ends), nor
                        // when it is already late; and that station judges
                        // the whole way again, when it is one that does.
                        Some(ToPeer::Typed { method, mut params, typed_ms, rtt_ms, kind }) => {
                            let way = rtt_ms + pulse.rtt_ms;
                            let (age, limit) = ((beat::mono_ms() as f64 - typed_ms).max(0.0) as u64, kind.limit_ms(fresh_ms, way));
                            let why = if let Some(quiet_ms) = pulse.silent_ms() {
                                perf.typed_refused.add();
                                Some(json!({"why": "silent", "quiet_ms": quiet_ms}))
                            } else if age > limit {
                                perf.typed_late.add();
                                Some(json!({"why": "late", "age_ms": age, "limit_ms": limit}))
                            } else {
                                None
                            };
                            if let Some(mut said) = why {
                                said["chan"] = json!(chan);
                                said["input_dropped"] = json!(1);
                                said["station"] = json!(to);
                                if let Some(n) = params.get("iseq") { said["iseq"] = n.clone(); }
                                if out.send(Message::Text(said.to_string().into())).await.is_err() { break None; }
                                continue;
                            }
                            if peer_judges {
                                params["t"] = json!(typed_ms);
                                params["rtt"] = json!(way);
                            } else if let Some(p) = params.as_object_mut() {
                                p.remove("t");
                            }
                            json!({"id": null, "method": method, "params": params}).to_string()
                        }
                        None => break None,
                    };
                    match pulse.send(&mut peer_sink, tungstenite::Message::Text(text.into())).await {
                        Sending::Sent => {}
                        Sending::Closed => break Some(None),
                        Sending::Stuck(quiet_ms) => break Some(Some(quiet_ms)),
                    }
                },
                from_peer = peer_stream.next() => {
                    pulse.heard(&from_peer);
                    match from_peer {
                        Some(Ok(tungstenite::Message::Binary(b))) if b.len() >= 5 => {
                            if out.send(frame(b[0], chan, &b[5..])).await.is_err() { break None; }
                        }
                        Some(Ok(tungstenite::Message::Text(t))) => {
                            let Ok(mut v) = serde_json::from_str::<Value>(&t) else { continue };
                            if v.get("pong").is_some() {
                                pulse.clock_pong(&v);
                            } else if v.get("chan").is_some() {
                                v["chan"] = json!(chan);
                                if out.send(Message::Text(v.to_string().into())).await.is_err() { break None; }
                            } else if let Some(msg) = v.get("event").and_then(|ev| resized_for(ev, &pane_id, chan)) {
                                if out.send(Message::Text(msg.to_string().into())).await.is_err() { break None; }
                            }
                        }
                        Some(Ok(_)) => {}
                        _ => break Some(None),
                    }
                },
            }
        };
        if let Some(quiet_ms) = ended {
            let _ = out.send(pulse.gone(chan, quiet_ms)).await;
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
        answer["source"],
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
    let screen = params.get("screen").and_then(|s| s.as_bool()) == Some(true);
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
            let mut ask = json!({"pane": pane, "session": session, "after": cursor, "limit": 200});
            // A deck that shows the screen as a conversation said so.
            if screen {
                ask["screen"] = json!(true);
            }
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
    let (mut peer_sink, mut peer_stream) = peer_socket(daemon, station, client, None)
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
            Ok(_) => return Err(unreachable(format!("{station} closed the connection"))),
            Err(_) => {
                daemon.fleet.connection_tried(station, false);
                return Err(unreachable(format!("{station} did not answer within 10 s")));
            }
        }
    };
    daemon.fleet.connection_tried(station, true);
    result["chan"] = json!(chan);
    let mut pulse = Pulse::new(daemon, station);
    let task = tokio::spawn(async move {
        // The connection stays open for as long as it is watched.
        let ended: Option<Option<u64>> = loop {
            tokio::select! {
                dead = pulse.tick(&mut peer_sink) => if let Some(quiet_ms) = dead { break Some(Some(quiet_ms)) },
                from_peer = peer_stream.next() => {
                    pulse.heard(&from_peer);
                    match from_peer {
                        Some(Ok(tungstenite::Message::Text(t))) => {
                            let Ok(mut v) = serde_json::from_str::<Value>(&t) else {
                                continue;
                            };
                            if v.get("chan").is_some() {
                                v["chan"] = json!(chan);
                                if out.send(Message::Text(v.to_string().into())).await.is_err() {
                                    break None;
                                }
                            }
                        }
                        Some(Ok(_)) => {}
                        _ => break Some(None),
                    }
                }
            }
        };
        if let Some(quiet_ms) = ended {
            let _ = out.send(pulse.gone(chan, quiet_ms)).await;
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
        // Debug builds only, for the test of a new build of the deck
        // replacing the one a browser has saved.
        if cfg!(debug_assertions) {
            if let Ok(other) = std::env::var("SPANDECK_TEST_BUILD") {
                other.hash(&mut h);
            }
        }
        format!("{:016x}", h.finish())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reason: DECISIONS.md, "What 'working through another station' is"
    /// and "What was reviewed" (Host, Origin): the one other origin a
    /// station accepts is exactly the deck of a member of its own roster,
    /// and only where the endpoint allows it; everything else refuses
    /// other origins as before.
    #[test]
    fn only_a_members_deck_is_another_origin_that_is_accepted() {
        let hosts: Vec<String> = [
            "100.64.0.1:7391",
            "100.64.0.1:7392",
            "spark2.tail0.ts.net:7392",
            "spark2.tail0.ts.net:*",
        ]
        .map(String::from)
        .to_vec();
        let decks = vec![
            ("http://100.64.0.2:7391".to_string(), "m4".to_string()),
            ("https://m4.tail0.ts.net:7392".to_string(), "m4".to_string()),
        ];
        let refused = Err((StatusCode::FORBIDDEN, "cross-origin requests are refused"));
        let at =
            |host: &str, origin: Option<&str>, secure: bool, decks: Option<&[(String, String)]>| {
                origin_rule(&hosts, Some(host), origin, secure, decks)
            };
        // This station's own deck, and a caller that is no browser.
        assert_eq!(at("100.64.0.1:7391", None, false, None), Ok(Page::Own));
        assert_eq!(
            at(
                "100.64.0.1:7391",
                Some("http://100.64.0.1:7391"),
                false,
                None
            ),
            Ok(Page::Own)
        );
        assert_eq!(
            at(
                "Spark2.tail0.ts.net:7392",
                Some("https://spark2.tail0.ts.net:7392"),
                true,
                None
            ),
            Ok(Page::Own)
        );
        // Behind `tailscale serve` the browser's origin is https and this
        // station sees the plain port: as before.
        assert_eq!(
            at(
                "spark2.tail0.ts.net",
                Some("https://spark2.tail0.ts.net"),
                false,
                None
            ),
            Ok(Page::Own)
        );
        // On the TLS port a page that is not https is not this deck.
        assert_eq!(
            at(
                "spark2.tail0.ts.net:7392",
                Some("http://spark2.tail0.ts.net:7392"),
                true,
                None
            ),
            refused
        );
        // A member's deck, by address and by name, where the endpoint
        // allows it (`/api/ws`, `/api/ping`), on either port.
        for secure in [false, true] {
            let host = if secure {
                "spark2.tail0.ts.net:7392"
            } else {
                "100.64.0.1:7391"
            };
            assert_eq!(
                at(host, Some("http://100.64.0.2:7391"), secure, Some(&decks)),
                Ok(Page::Deck("m4".into()))
            );
            assert_eq!(
                at(
                    host,
                    Some("https://M4.tail0.ts.net:7392"),
                    secure,
                    Some(&decks)
                ),
                Ok(Page::Deck("m4".into()))
            );
        }
        // Where it does not (`/api/rpc`, `/api/device/*`, `/api/font`,
        // every file), or with the switch off: refused.
        assert_eq!(
            at(
                "100.64.0.1:7391",
                Some("http://100.64.0.2:7391"),
                false,
                None
            ),
            refused
        );
        // Not a member's deck: another site, another port or scheme of a
        // member's machine, a name that only starts like one.
        for other in [
            "https://example.com",
            "http://100.64.0.2:8080",
            "https://100.64.0.2:7391",
            "http://m4.tail0.ts.net:7392",
            "https://m4.tail0.ts.net:7392.example.com",
            "https://m4.tail0.ts.net",
            "null",
        ] {
            assert_eq!(
                at("100.64.0.1:7391", Some(other), false, Some(&decks)),
                refused,
                "{other}"
            );
        }
        // A Host that is not this station's is refused whatever the origin.
        assert_eq!(
            at(
                "evil.example:7391",
                Some("http://100.64.0.2:7391"),
                false,
                Some(&decks)
            ),
            Err((StatusCode::FORBIDDEN, "this Host is not served here"))
        );
        assert_eq!(
            origin_rule(&hosts, None, None, false, None),
            Err((StatusCode::BAD_REQUEST, "missing Host"))
        );
    }

    /// Reason: "A new certificate replaces the old one for new connections
    /// only: connections that are open carry on."
    #[tokio::test]
    async fn a_renewed_certificate_is_the_next_connections() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
        use tokio_rustls::rustls::{ClientConfig, RootCertStore};

        let make = || {
            let made = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
            (
                made.cert.pem(),
                made.signing_key.serialize_pem(),
                made.cert.der().clone(),
            )
        };
        let (first, second) = (make(), make());
        let certs = Arc::new(tls::Certs::default());
        certs.set(Some(tls::certified(&first.0, &first.1).unwrap()));
        // A key that is not the certificate's is not taken.
        assert!(tls::certified(&first.0, &second.1).is_err());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/hello", get(|| async { "hello" }));
        tokio::spawn(tls::serve(
            listener,
            tls::acceptor(certs.clone()).unwrap(),
            app,
            |_| {},
        ));

        let mut roots = RootCertStore::empty();
        roots.add(first.2.clone()).unwrap();
        roots.add(second.2.clone()).unwrap();
        let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        // A browser that resumes a session is not shown a certificate
        // again: here every connection is a whole handshake.
        config.resumption = tokio_rustls::rustls::client::Resumption::disabled();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let connect = || async {
            let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
            let name = ServerName::try_from("localhost").unwrap();
            let tls = connector.connect(name, tcp).await.unwrap();
            let shown: CertificateDer<'static> =
                tls.get_ref().1.peer_certificates().unwrap()[0].clone();
            (tls, shown)
        };
        async fn ask(tls: &mut tokio_rustls::client::TlsStream<tokio::net::TcpStream>) -> String {
            tls.write_all(b"GET /hello HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await
                .unwrap();
            let mut got = vec![];
            let mut buf = [0u8; 1024];
            while !got.ends_with(b"hello") {
                let n = tls.read(&mut buf).await.unwrap();
                assert!(n > 0, "the connection was closed");
                got.extend_from_slice(&buf[..n]);
            }
            String::from_utf8_lossy(&got).into_owned()
        }
        let (mut open, shown) = connect().await;
        assert_eq!(shown, first.2);
        assert!(ask(&mut open).await.starts_with("HTTP/1.1 200"));
        // Renewed: the next connection is shown the new one, and the one
        // that was open still answers.
        certs.set(Some(tls::certified(&second.0, &second.1).unwrap()));
        let (mut next, shown) = connect().await;
        assert_eq!(shown, second.2);
        assert!(ask(&mut next).await.starts_with("HTTP/1.1 200"));
        assert!(ask(&mut open).await.starts_with("HTTP/1.1 200"));
        // With none held, a handshake fails and nothing is served.
        certs.set(None);
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        assert!(connector.connect(name, tcp).await.is_err());
    }

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

    /// Unit 03b closes what 02a left ("unlikely, not impossible"): the page
    /// names each attachment with a number it never uses again, so a
    /// number left over from before a reconnect is one the new connection
    /// was never given. The station's part: a name is one attachment's.
    #[test]
    fn a_name_the_page_chose_is_one_attachments() {
        let mut chans = Channels::starting_from(7);
        // Taken as given; a second attachment under the same name is
        // refused while the first is being made, and while it is open.
        assert!(chans.claim(900));
        assert!(!chans.claim(900));
        chans.open(900, "pane B");
        assert!(!chans.claim(900));
        assert_eq!(chans.get(&json!({"chan": 900})), Some(&"pane B"));
        // 0 is what an answer without a number reads as.
        assert!(!chans.claim(0));
        // An attachment that was not made gives its name back; one that
        // was closed leaves it free (the page does not use it again; a
        // station need not remember every name it ever saw).
        assert!(chans.claim(901));
        chans.unclaim(901);
        assert!(chans.claim(901));
        // The connection's own numbers pass over a name the page holds.
        let mut chans = Channels::starting_from(0);
        assert!(chans.claim(1));
        chans.open(2, "open");
        assert_eq!(chans.fresh(), 3);
        // A pane on a new connection, under a new name: the old name
        // names nothing there.
        let mut after = Channels::starting_from(7);
        assert!(after.claim(902));
        after.open(902, "pane C");
        assert_eq!(after.get(&json!({"chan": 900})), None);
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
