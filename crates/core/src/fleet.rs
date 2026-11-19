//! The fleet: finding other stations over Tailscale, trusting the ones under
//! the same login, calling them, and stop-all with honest states.
//!
//! No central server and no replication. Each station answers for itself;
//! the fleet view is a live union with an explicit unreachable state.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spandeck_agents::AgentState;

use crate::daemon::{invalid, Daemon, Error, Result, PROTOCOL, VERSION};
use crate::paths::{now_ms, Config};
use crate::relay::{BoxFuture, Envelope, RelayAck, Transport};
use crate::rounds::{Kind, Listed, Outcome, Select};
use crate::stops;

/// `station.hello` and `station.info` are given this long together.
const PROBE_DEADLINE: Duration = Duration::from_millis(2500);
/// A round with one station is cut off after this long. Each call in it
/// has the HTTP client's own five seconds; this bounds them all.
pub const ROUND_DEADLINE: Duration = Duration::from_secs(8);
/// A caller that waits for the fleet to be looked at waits this long at
/// most, however many stations do not answer.
pub const LOOK_WAIT: Duration = Duration::from_secs(5);

/// A timed `station.hello`: how long it took, this station's clock when it
/// was sent, and the clock the station said.
type Reading = (Duration, u64, Option<u64>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub station: String,
    /// host:port of its web/API listener.
    pub addr: String,
    pub version: String,
    pub protocol: u32,
    /// online | unreachable | update_needed | not_linked
    pub state: String,
    pub last_seen_ms: u64,
    /// Its own `station.info`, as of last contact.
    pub info: Value,
}

#[derive(Debug, Clone)]
pub struct TailnetSelf {
    pub user_id: i64,
    pub login: String,
    pub ips: Vec<String>,
}

/// What became of a call to another station that did not give a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// No connection was made: the station was sent nothing.
    No,
    /// The request went out and no answer, or no readable one, came back:
    /// it may have been carried out.
    Maybe,
    /// The station answered, with an error.
    Answered,
}

/// Why a call to another station got no answer at all, as far as this end
/// can tell. The transport's own error is never shown to the owner: it
/// named a URL and said nothing a person could act on (the audit: "golf:
/// unreachable: error sending request for url (…)").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoAnswer {
    /// The machine answered that nothing listens at the station's port.
    Refused,
    /// No connection within the 1.5 s given to make one.
    NoConnection,
    /// This machine has no way to that address at all.
    NoRoute,
    /// The request went out and nothing came back in the call's 5 s.
    Timeout,
    /// The connection ended before an answer came.
    Broke,
}

impl NoAnswer {
    /// From what is known of the failure: whether a connection was made,
    /// whether time ran out, and the operating system's own error if there
    /// was one.
    pub fn from(connect: bool, timeout: bool, io: Option<std::io::ErrorKind>) -> NoAnswer {
        use std::io::ErrorKind as K;
        match (connect, timeout, io) {
            (true, _, Some(K::ConnectionRefused)) => NoAnswer::Refused,
            (true, _, Some(K::NetworkUnreachable | K::HostUnreachable | K::AddrNotAvailable)) => {
                NoAnswer::NoRoute
            }
            (true, ..) => NoAnswer::NoConnection,
            (false, true, _) => NoAnswer::Timeout,
            (false, false, _) => NoAnswer::Broke,
        }
    }

    fn of(e: &reqwest::Error) -> NoAnswer {
        let mut io = None;
        let mut source = std::error::Error::source(e);
        while let Some(err) = source {
            if let Some(found) = err.downcast_ref::<std::io::Error>() {
                io = Some(found.kind());
            }
            source = err.source();
        }
        NoAnswer::from(e.is_connect(), e.is_timeout(), io)
    }

    /// What is said of the station, to follow its name: "golf did not
    /// answer within 5 s". No address, no URL.
    pub fn words(self) -> &'static str {
        match self {
            NoAnswer::Refused => "refused the connection (spandeck may be stopped there)",
            NoAnswer::NoConnection => {
                "could not be reached within 1.5 s (it may be asleep or off, or Tailscale may not be connected there)"
            }
            NoAnswer::NoRoute => {
                "cannot be reached from this station (Tailscale may not be connected here)"
            }
            NoAnswer::Timeout => "did not answer within 5 s",
            NoAnswer::Broke => "ended the connection before it answered",
        }
    }

    /// Is this text one of the above? For a caller that puts the
    /// station's name in front of it.
    pub fn said(text: &str) -> bool {
        use NoAnswer::*;
        [Refused, NoConnection, NoRoute, Timeout, Broke]
            .iter()
            .any(|n| n.words() == text)
    }
}

/// Who is on the other end of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remote {
    /// This machine. Needs a token.
    Loopback,
    /// A device under the user's own Tailscale login: the user, or their station.
    Owner { login: String, node: String },
    /// Anything else. Never trusted automatically.
    Stranger(String),
    /// The local Tailscale client did not say who it is: nobody is let in
    /// on its word, and nothing is concluded about the device either. It
    /// is asked again at the next connection.
    NoTailscale,
}

pub struct FleetState {
    pub http: reqwest::Client,
    /// For one large thing at a time (a release, in pieces): connections
    /// of its own, so that nothing else said between two stations waits
    /// behind it on a connection, and a time limit given with each call.
    bulk: reqwest::Client,
    peers: Mutex<BTreeMap<String, Peer>>,
    whois: Mutex<HashMap<IpAddr, (u64, Remote)>>,
    me: Mutex<Option<(u64, Option<TailnetSelf>)>>,
    /// Stations dropped from this station's list since it last looked at
    /// the fleet: the next look says so to the roster (manage.rs).
    dropped: Mutex<Vec<String>>,
    /// Where a station of this fleet can be, as of the last look.
    pub(crate) places: Mutex<Vec<String>>,
    /// The name the local Tailscale client gives the owner's device at each
    /// address, as of the last look (https.rs: whose deck an origin is).
    pub(crate) names: Mutex<HashMap<String, String>>,
    /// Held while the roster is read, merged and written back.
    pub(crate) roster: Mutex<()>,
    /// Signs calls to other stations with the fleet key.
    pub trust: Arc<crate::trust::TrustState>,
    pub station: String,
    /// Round trips and failures of calls to other stations are counted here.
    pub perf: Arc<crate::perf::Perf>,
    /// This machine's own Tailscale addresses and name, as the local client
    /// said at the last look at the fleet. The web server listens on what
    /// this says. `None` until a look has read them.
    pub own_addrs: tokio::sync::watch::Sender<Option<OwnAddrs>>,
    /// Addresses whose last calls got no answer at all: see `not_answering`.
    silent: Mutex<Silence>,
}

/// Calls in a row to one address that got no answer (no connection, or
/// nothing within the call's five seconds).
#[derive(Default, Clone, Copy)]
struct Silent {
    fails: u32,
    since_ms: u64,
    probed: bool,
    probe_ms: u64,
}

/// Which addresses are not answering: the whole of the rule, apart from
/// the calls that feed it and the clock.
#[derive(Default)]
pub struct Silence {
    by_addr: HashMap<String, Silent>,
}

impl Silence {
    /// A call to `addr` was answered (whatever the answer), or got no
    /// answer at all.
    pub fn note(&mut self, addr: &str, answered: bool, now_ms: u64) {
        if answered {
            self.by_addr.remove(addr);
            return;
        }
        if self.by_addr.len() >= 256 && !self.by_addr.contains_key(addr) {
            return;
        }
        let s = self.by_addr.entry(addr.to_string()).or_default();
        if s.fails == 0 {
            s.since_ms = now_ms;
        }
        s.fails = s.fails.saturating_add(1);
    }

    /// A connection to `addr` was given up after its pings went unanswered
    /// (`beat::STATION`: three of them, six seconds): that is more than two
    /// calls with no answer, so it is not answering from when it was last
    /// heard, unless it already was from before.
    pub fn lost(&mut self, addr: &str, last_heard_ms: u64) {
        if self.by_addr.len() >= 256 && !self.by_addr.contains_key(addr) {
            return;
        }
        let s = self.by_addr.entry(addr.to_string()).or_default();
        if s.fails == 0 {
            s.since_ms = last_heard_ms;
        }
        s.fails = s.fails.max(SILENT_AFTER);
    }

    /// Not answering, and since when: from the second call in a row with
    /// no answer until the first answer.
    pub fn since(&self, addr: &str) -> Option<u64> {
        self.by_addr
            .get(addr)
            .filter(|s| s.fails >= SILENT_AFTER)
            .map(|s| s.since_ms)
    }

    /// May a call go to `addr` to find out whether it is back? Once in
    /// `SILENT_PROBE_MS`; always for an address that is answering.
    pub fn probe(&mut self, addr: &str, now_ms: u64) -> bool {
        match self.by_addr.get_mut(addr) {
            Some(s) if s.probed && now_ms.saturating_sub(s.probe_ms) < SILENT_PROBE_MS => false,
            Some(s) => {
                (s.probed, s.probe_ms) = (true, now_ms);
                true
            }
            None => true,
        }
    }
}

/// After this many calls in a row with no answer a station is taken to be
/// not answering.
pub const SILENT_AFTER: u32 = 2;
/// While a station is not answering, one of a Control Deck's calls to it is
/// let through this often, to find out whether it is back.
pub const SILENT_PROBE_MS: u64 = 2000;

/// See `FleetState::own_addrs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnAddrs {
    pub ips: Vec<IpAddr>,
    /// The machine's name on the tailnet, lower case, no final dot.
    pub dns: Option<String>,
}

impl OwnAddrs {
    /// From `tailscale status --json`.
    pub fn of(status: &Value) -> OwnAddrs {
        OwnAddrs {
            ips: status
                .pointer("/Self/TailscaleIPs")
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
                .filter_map(|i| i.as_str()?.parse().ok())
                .collect(),
            dns: status
                .pointer("/Self/DNSName")
                .and_then(|d| d.as_str())
                .map(|d| d.trim_end_matches('.').to_lowercase())
                .filter(|d| !d.is_empty()),
        }
    }
}

impl FleetState {
    pub fn new(
        cfg: &Config,
        trust: Arc<crate::trust::TrustState>,
        perf: Arc<crate::perf::Perf>,
    ) -> FleetState {
        let http = crate::http::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_millis(1500))
            .build()
            .unwrap_or_default();
        let bulk = crate::http::builder()
            .connect_timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_default();
        FleetState {
            http,
            bulk,
            peers: Mutex::new(BTreeMap::new()),
            whois: Mutex::new(HashMap::new()),
            me: Mutex::new(None),
            dropped: Mutex::new(vec![]),
            places: Mutex::new(vec![]),
            names: Mutex::new(HashMap::new()),
            roster: Mutex::new(()),
            trust,
            station: cfg.station.clone(),
            perf,
            own_addrs: tokio::sync::watch::Sender::new(None),
            silent: Mutex::new(Silence::default()),
        }
    }

    /// A call to this address was answered (whatever the answer), or was
    /// not answered at all.
    fn note_answer(&self, addr: &str, answered: bool) {
        self.silent.lock().unwrap().note(addr, answered, now_ms());
    }

    /// Is this station not answering: did its last calls, two or more in a
    /// row, get no answer? Since when (this station's clock). It is so
    /// from the second such call and until the first answer to any call,
    /// the look at the fleet every ten seconds included. Nothing a station
    /// does by itself reads this (a stop is always sent): it is for a
    /// Control Deck's connection, which then refuses a call to that
    /// station at once where each would wait out its five seconds.
    pub fn not_answering(&self, station: &str) -> Option<u64> {
        let addr = self.peer(station)?.addr;
        self.silent.lock().unwrap().since(&addr)
    }

    /// What a connection held to `station` for one pane found (the
    /// gateway's socket, `beat::STATION`), fed into the same state the
    /// calls feed. `Some(last_heard_ms)`: its pings went unanswered and it
    /// was given up. `None`: that station was heard on such a connection,
    /// which ends a silence as an answered call does. (It is only told so
    /// when the station was taken to be silent: a pong every two seconds on
    /// every pane's connection is not otherwise worth a lock.)
    pub fn heartbeat(&self, station: &str, lost: Option<u64>) {
        let Some(addr) = self.addr_of(station) else {
            return;
        };
        let mut silent = self.silent.lock().unwrap();
        match lost {
            Some(last_heard_ms) => silent.lost(&addr, last_heard_ms),
            None => silent.note(&addr, true, now_ms()),
        }
    }

    /// A connection for a pane of `station` was tried: made and answered,
    /// or not at all. Counted as a call to it is.
    pub fn connection_tried(&self, station: &str, answered: bool) {
        if let Some(addr) = self.addr_of(station) {
            self.note_answer(&addr, answered);
        }
    }

    /// May one call be let through to a station that is not answering, to
    /// find out whether it is back? Once every two seconds, for whoever
    /// asks first.
    pub fn probe(&self, station: &str) -> bool {
        let Some(addr) = self.peer(station).map(|p| p.addr) else {
            return true;
        };
        self.silent.lock().unwrap().probe(&addr, now_ms())
    }

    pub fn peer(&self, station: &str) -> Option<Peer> {
        self.peers.lock().unwrap().get(station).cloned()
    }

    fn addr_of(&self, station: &str) -> Option<String> {
        let peers = self.peers.lock().unwrap();
        peers.get(station).map(|p| p.addr.clone())
    }

    pub(crate) fn forget(&self, station: &str) {
        self.peers.lock().unwrap().remove(station);
        self.dropped.lock().unwrap().push(station.to_string());
    }

    /// This station left its fleet: it has no list, and nothing to say to
    /// a roster about the stations it was with.
    pub(crate) fn forget_all(&self) {
        self.peers.lock().unwrap().clear();
        self.dropped.lock().unwrap().clear();
    }

    pub(crate) fn take_dropped(&self) -> Vec<String> {
        std::mem::take(&mut *self.dropped.lock().unwrap())
    }

    /// Bring the list in line with the roster: a member that is not on it is
    /// listed as unreachable, and one the fleet forgot leaves it unless it
    /// answers.
    pub(crate) fn follow_roster(&self, known: &[Peer], gone: impl Fn(&Peer) -> bool) -> bool {
        let mut peers = self.peers.lock().unwrap();
        let before = peers.len();
        peers.retain(|_, p| p.state != "unreachable" || !gone(p));
        let mut changed = peers.len() != before;
        for k in known {
            if !peers.contains_key(&k.station) {
                let mut p = k.clone();
                p.state = "unreachable".into();
                peers.insert(k.station.clone(), p);
                changed = true;
            }
        }
        changed
    }

    pub fn peers(&self) -> Vec<Peer> {
        self.peers.lock().unwrap().values().cloned().collect()
    }

    /// This device's Tailscale identity, from the local client. Cached briefly.
    pub fn tailnet_self(&self) -> Option<TailnetSelf> {
        {
            let me = self.me.lock().unwrap();
            if let Some((at, v)) = &*me {
                // An answer is kept a minute; that there was none, two
                // seconds, so that it is asked again almost at once.
                let keep = if v.is_some() { 60_000 } else { 2_000 };
                if now_ms().saturating_sub(*at) < keep {
                    return v.clone();
                }
            }
        }
        let v = tailscale_status().and_then(|s| {
            let me = s.get("Self")?;
            let user_id = me.get("UserID")?.as_i64()?;
            let login = s
                .pointer(&format!("/User/{user_id}/LoginName"))
                .and_then(|l| l.as_str())
                .unwrap_or_default()
                .to_string();
            let ips = me
                .get("TailscaleIPs")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|i| i.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Some(TailnetSelf {
                user_id,
                login,
                ips,
            })
        });
        *self.me.lock().unwrap() = Some((now_ms(), v.clone()));
        v
    }

    /// Identify a connection by its real remote address. Serve identity headers
    /// are never consulted: they can be forged from localhost.
    pub fn identify(&self, ip: IpAddr) -> Remote {
        if ip.is_loopback() {
            return Remote::Loopback;
        }
        if let Some((at, r)) = self.whois.lock().unwrap().get(&ip) {
            if now_ms().saturating_sub(*at) < 120_000 {
                return r.clone();
            }
        }
        // A connection from this machine's own Tailscale address is a local
        // process, not the owner at another device: it needs a token, like
        // localhost. Otherwise any local program could act as the owner.
        if self
            .tailnet_self()
            .is_some_and(|me| me.ips.iter().any(|i| i.parse::<IpAddr>().ok() == Some(ip)))
        {
            return Remote::Loopback;
        }
        let who = match tailscale_whois(ip) {
            Whois::Is(who) => Some(who),
            Whois::Unknown => None,
            // Not kept: the next connection asks again, so the owner is
            // let in as soon as Tailscale answers.
            Whois::Failed => return Remote::NoTailscale,
        };
        let remote = match (self.tailnet_self(), who) {
            (Some(me), Some(who)) => {
                let uid = who.pointer("/UserProfile/ID").and_then(|v| v.as_i64());
                let login = who
                    .pointer("/UserProfile/LoginName")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let node = who
                    .pointer("/Node/ComputedName")
                    .or_else(|| who.pointer("/Node/Name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let tagged = who
                    .pointer("/Node/Tags")
                    .and_then(|t| t.as_array())
                    .is_some_and(|t| !t.is_empty());
                if uid == Some(me.user_id) && !tagged {
                    Remote::Owner { login, node }
                } else {
                    Remote::Stranger(if tagged {
                        format!("tagged device {node}")
                    } else {
                        login
                    })
                }
            }
            // Tailscale knows the device and did not say who this machine
            // is: the same failure, and not kept either.
            (None, Some(_)) => return Remote::NoTailscale,
            _ => Remote::Stranger(ip.to_string()),
        };
        self.whois
            .lock()
            .unwrap()
            .insert(ip, (now_ms(), remote.clone()));
        remote
    }

    /// Call another station.
    pub async fn call(
        &self,
        station: &str,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, String> {
        let peer = self
            .peer(station)
            .ok_or_else(|| format!("station {station} is not in the fleet"))?;
        self.call_addr(&peer.addr, method, params).await
    }

    pub async fn call_addr(
        &self,
        addr: &str,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, String> {
        self.post(addr, method, params, Some(self.trust.key()))
            .await
    }

    /// A call signed with a key this fleet no longer uses: the only way to
    /// reach a station that was away when the key was replaced.
    pub(crate) async fn call_with_key(
        &self,
        addr: &str,
        method: &str,
        params: Value,
        key: &[u8],
    ) -> std::result::Result<Value, String> {
        self.post(addr, method, params, Some(key.to_vec())).await
    }

    /// A call any device under the owner's login may make without the fleet
    /// key: who are you, and asking to join.
    pub async fn call_open(
        &self,
        addr: &str,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, String> {
        self.post(addr, method, params, None).await
    }

    /// `call`, for a caller that must know whether a call that failed may
    /// have arrived: the error says which it was.
    pub async fn call_sent(
        &self,
        station: &str,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, (Sent, String)> {
        let peer = self
            .peer(station)
            .ok_or_else(|| (Sent::No, format!("station {station} is not in the fleet")))?;
        self.post_sent(&peer.addr, method, params, Some(self.trust.key()))
            .await
    }

    /// A signed call that may take long or carry a piece of a file: made
    /// on connections of its own, with the time it may take. It is not
    /// counted among the link's calls (the Performance page's round trips
    /// are of ordinary calls), and its failing does not mark the station
    /// as silent: the ordinary calls say that.
    pub async fn call_bulk(
        &self,
        station: &str,
        method: &str,
        params: Value,
        limit: Duration,
    ) -> std::result::Result<Value, String> {
        let peer = self
            .peer(station)
            .ok_or_else(|| format!("station {station} is not in the fleet"))?;
        self.post_on(
            &self.bulk,
            Some(limit),
            &peer.addr,
            method,
            params,
            Some(self.trust.key()),
        )
        .await
        .map_err(|(_, why)| why)
    }

    async fn post(
        &self,
        addr: &str,
        method: &str,
        params: Value,
        key: Option<Vec<u8>>,
    ) -> std::result::Result<Value, String> {
        self.post_sent(addr, method, params, key)
            .await
            .map_err(|(_, why)| why)
    }

    async fn post_sent(
        &self,
        addr: &str,
        method: &str,
        params: Value,
        key: Option<Vec<u8>>,
    ) -> std::result::Result<Value, (Sent, String)> {
        self.post_on(&self.http, None, addr, method, params, key)
            .await
    }

    /// `limit`: the call is a bulk one (see `call_bulk`).
    async fn post_on(
        &self,
        http: &reqwest::Client,
        limit: Option<Duration>,
        addr: &str,
        method: &str,
        params: Value,
        key: Option<Vec<u8>>,
    ) -> std::result::Result<Value, (Sent, String)> {
        let counted = limit.is_none();
        let body =
            json!({"method": method, "params": params, "from": self.station, "protocol": PROTOCOL})
                .to_string()
                .into_bytes();
        let mut req = http.post(format!("http://{addr}/api/peer/rpc"));
        if let Some(limit) = limit {
            req = req.timeout(limit);
        }
        let mut req = req
            .header("x-spandeck-station", &self.station)
            .header("content-type", "application/json");
        if let Some(key) = key {
            // The signature covers the exact bytes sent.
            req = req.header(
                "x-spandeck-sig",
                crate::trust::sign_with(&key, "rpc", &body),
            );
        }
        // Counted for the Performance page: how long the call took, or how
        // it failed. An answer that is an error is still an answer.
        let began = std::time::Instant::now();
        let resp = req.body(body).send().await.map_err(|e| {
            let why = if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "no connection"
            } else {
                "no answer"
            };
            if counted {
                self.perf.link_call(addr, began.elapsed(), Some(why));
                self.note_answer(addr, false);
            }
            // No connection was made: nothing was sent.
            let sent = match e.is_connect() {
                true => Sent::No,
                false => Sent::Maybe,
            };
            (sent, NoAnswer::of(&e).words().to_string())
        })?;
        if counted {
            self.note_answer(addr, true);
        }
        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| {
            if counted {
                self.perf
                    .link_call(addr, began.elapsed(), Some("bad reply"));
            }
            (Sent::Maybe, format!("bad reply: {e}"))
        })?;
        let refused = match body.pointer("/error/code").and_then(|c| c.as_str()) {
            Some("clock") => Some("clock"),
            _ if status == reqwest::StatusCode::UNAUTHORIZED => Some("refused"),
            _ => None,
        };
        if counted {
            self.perf.link_call(addr, began.elapsed(), refused);
        }
        if let Some(err) = body.get("error") {
            return Err((
                Sent::Answered,
                err.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("error")
                    .to_string(),
            ));
        }
        if !status.is_success() {
            return Err((Sent::Answered, format!("HTTP {status}")));
        }
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// The production station link for the message relay.
impl Transport for FleetState {
    fn deliver<'a>(
        &'a self,
        station: &'a str,
        env: &'a Envelope,
    ) -> BoxFuture<'a, std::result::Result<RelayAck, String>> {
        Box::pin(async move {
            let v = self
                .call(station, "relay.accept", json!({"envelope": env}))
                .await?;
            serde_json::from_value(v).map_err(|e| e.to_string())
        })
    }
}

/// Tailscale's command-line program: on PATH, else where its installer
/// puts it (macOS: inside the app; Windows: `%ProgramFiles%\Tailscale`).
pub(crate) fn tailscale_bin() -> Option<String> {
    let mut places = vec![
        "tailscale".to_string(),
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale".to_string(),
        "/usr/local/bin/tailscale".to_string(),
    ];
    if spandeck_terminal::sys::WINDOWS {
        let programs = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        places = vec![
            "tailscale.exe".to_string(),
            format!(r"{programs}\Tailscale\tailscale.exe"),
        ];
    }
    places.into_iter().find(|p| {
        tailscale_cmd(p)
            .arg("version")
            .stderr(std::process::Stdio::null())
            .output()
            .is_ok_and(|o| {
                o.status.success() && says_a_version(&String::from_utf8_lossy(&o.stdout))
            })
    })
}

/// Tailscale's program, to be run as a command. On a Mac the program inside
/// the app is also the app: started with no terminal, as a station that
/// starts at login is, it tries to open its window, prints "The Tailscale
/// GUI failed to start" and ends with success. `TAILSCALE_BE_CLI` tells it
/// which it is to be. (Found on the owner's Mac "monitoring", 17 Nov 2026:
/// its station saw no machine of the fleet, and said Tailscale was not
/// running, which it was.)
pub(crate) fn tailscale_cmd(bin: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(bin);
    cmd.env("TAILSCALE_BE_CLI", "1");
    cmd
}

/// Is this what `tailscale version` prints: a version number first, and not
/// words about a window that would not open.
fn says_a_version(out: &str) -> bool {
    out.trim_start()
        .split(|c: char| c.is_whitespace() || c == '-')
        .next()
        .is_some_and(|v| {
            let mut parts = v.split('.');
            parts.clone().count() >= 2
                && parts.all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn tailscale_json(args: &[&str]) -> Option<Value> {
    static BIN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let bin = BIN.get_or_init(tailscale_bin).as_ref()?;
    let out = tailscale_cmd(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

pub fn tailscale_status() -> Option<Value> {
    tailscale_json(&["status", "--json"])
}

/// What the local Tailscale client said of an address.
enum Whois {
    Is(Value),
    /// It answered, and knows no such device.
    Unknown,
    /// It did not answer: the program could not be run, or says it cannot
    /// reach its own service.
    Failed,
}

/// Does what `tailscale` printed say that the client itself is not
/// answering (as opposed to: it answered that it knows no such peer)?
fn tailscale_is_down(stderr: &str) -> bool {
    let said = stderr.to_lowercase();
    [
        "failed to connect",
        "appear to be running",
        "is stopped",
        "not running",
    ]
    .iter()
    .any(|w| said.contains(w))
}

fn tailscale_whois(ip: IpAddr) -> Whois {
    static BIN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let Some(bin) = BIN.get_or_init(tailscale_bin).as_ref() else {
        // No Tailscale on this machine at all: as before, nobody from
        // another address is the owner.
        return Whois::Unknown;
    };
    let out = tailscale_cmd(bin)
        .args(["whois", "--json", &ip.to_string()])
        .output();
    match out {
        Ok(out) if out.status.success() => match serde_json::from_slice(&out.stdout) {
            Ok(who) => Whois::Is(who),
            Err(_) => Whois::Failed,
        },
        Ok(out) if !tailscale_is_down(&String::from_utf8_lossy(&out.stderr)) => Whois::Unknown,
        _ => Whois::Failed,
    }
}

impl Daemon {
    // Finding stations: every online device under the same login is asked
    // whether it runs spandeck. Only those holding the same fleet key are
    // trusted; the rest are listed as not linked until the owner joins them.

    /// Look at the whole fleet and wait for it, for a caller that needs
    /// the list to be current: `fleet.refresh`, a join, a machine added.
    /// Every address is asked now, whatever its schedule.
    pub(crate) async fn discover_peers(&self) {
        self.look_at_fleet(false).await;
    }

    /// One look that is waited for at a time. `only_if_stale`: a caller
    /// that only needs the fleet to have been looked at lately (the owner's
    /// change to a synced document), and waited for a look that was under
    /// way, does not look again.
    ///
    /// The lock is held only by callers that chose to wait for the fleet.
    /// No round and no periodic loop takes it, so it cannot make one
    /// station wait for another: what it serializes is the waiting. The
    /// wait is bounded (`LOOK_WAIT`) however many stations do not answer;
    /// a round still running then goes on by itself.
    pub(crate) async fn look_at_fleet(&self, only_if_stale: bool) {
        let _one = self.sync.looking.lock().await;
        if only_if_stale && !self.sync_look_is_stale() {
            return;
        }
        let began = std::time::Instant::now();
        let select = if only_if_stale {
            Select::Members
        } else {
            Select::All
        };
        self.look_start(select).await;
        self.rounds.settle(only_if_stale, LOOK_WAIT).await;
        self.looked(began);
    }

    /// The periodic look (the `fleet` loop, every ten seconds): start the
    /// rounds that are due and return. A watcher of its own notes when the
    /// members asked have all answered or run out of time; nothing else
    /// waits for that.
    pub(crate) async fn look_begin(&self) {
        let began = std::time::Instant::now();
        self.look_start(Select::Due).await;
        let d = self.clone();
        tokio::spawn(async move {
            d.rounds.settle(true, LOOK_WAIT).await;
            d.looked(began);
        });
    }

    /// A machine is being chosen to add: the machines that have not joined
    /// are all asked now. Nothing is waited for.
    pub(crate) fn look_soon(&self) {
        let d = self.clone();
        tokio::spawn(async move { d.look_start(Select::All).await });
    }

    /// The station `station` made a signed call here, so it is there: if
    /// it was listed as away, or was being asked less often, its round
    /// begins now.
    pub(crate) fn heard_from(&self, station: &str) {
        // What waits for it is sent now, not when its wait is over.
        self.relay.back(station);
        if let Some(addr) = self.fleet.addr_of(station) {
            if self.rounds.heard(&addr) {
                self.rounds.kick(self, Select::Due);
            }
        }
    }

    /// Work out where stations can be, and start a round for each address
    /// that `select` names. This asks the local Tailscale client and reads
    /// this station's own lists; it waits for no other station.
    async fn look_start(&self, select: Select) {
        let mut listed: BTreeMap<String, Listed> = BTreeMap::new();
        let mut list = |addr: &str, kind: Kind, online: Option<bool>| {
            let l = listed.entry(addr.to_string()).or_insert(Listed {
                addr: addr.to_string(),
                kind,
                online,
            });
            if kind == Kind::Member {
                l.kind = kind;
            }
            l.online = l.online.or(online);
        };
        // An address the config names is asked like a member's.
        for addr in &self.cfg.peers {
            list(addr, Kind::Member, None);
        }
        // Where a station of this fleet can be: the addresses in this
        // station's config, and the owner's own devices, on or off. An
        // address another station names for a member is taken only if it
        // is one of these (manage.rs, the roster).
        let mut places: Vec<String> = self.cfg.peers.clone();
        let mut online_at: HashMap<String, bool> = HashMap::new();
        let mut names: HashMap<String, String> = HashMap::new();
        if self.cfg.discovery {
            // The local client is given four seconds: one that hangs holds
            // up neither the rounds that are due nor anything else.
            let d = self.clone();
            let asked = tokio::time::timeout(
                Duration::from_secs(4),
                tokio::task::spawn_blocking(move || (d.fleet.tailnet_self(), tailscale_status())),
            )
            .await;
            let (me, status) = match asked {
                Ok(Ok(said)) => said,
                _ => (None, None),
            };
            // The same output says when each device's key expires.
            self.tailnet_refresh(status.as_ref());
            // And which addresses this machine has now: the web server
            // listens on a new one and lets go of one that is gone. When
            // the client does not answer, what it last said stands.
            if let Some(status) = &status {
                let now = Some(OwnAddrs::of(status));
                self.fleet.own_addrs.send_if_modified(|held| {
                    let changed = *held != now;
                    *held = now;
                    changed
                });
            }
            if let (Some(me), Some(status)) = (me, status) {
                for peer in status
                    .get("Peer")
                    .and_then(|p| p.as_object())
                    .into_iter()
                    .flat_map(|m| m.values())
                {
                    let same_user = peer.get("UserID").and_then(|u| u.as_i64()) == Some(me.user_id);
                    let online = peer
                        .get("Online")
                        .and_then(|o| o.as_bool())
                        .unwrap_or(false);
                    let tagged = peer
                        .get("Tags")
                        .and_then(|t| t.as_array())
                        .is_some_and(|t| !t.is_empty());
                    let os = peer.get("OS").and_then(|o| o.as_str()).unwrap_or("");
                    if !same_user || tagged || matches!(os, "iOS" | "tvOS" | "android") {
                        continue;
                    }
                    if let Some(ip) = peer
                        .get("TailscaleIPs")
                        .and_then(|a| a.as_array())
                        .and_then(|a| a.first())
                        .and_then(|i| i.as_str())
                    {
                        let dns = peer.get("DNSName").and_then(|d| d.as_str()).unwrap_or("");
                        let dns = dns.trim_end_matches('.').to_lowercase();
                        if !dns.is_empty() {
                            names.insert(ip.to_string(), dns);
                        }
                        let addr = format!("{ip}:{}", self.cfg.web_port);
                        places.push(addr.clone());
                        online_at.insert(addr.clone(), online);
                        if online {
                            list(&addr, Kind::Other, Some(true));
                        }
                    }
                }
            }
        }
        // Stations seen before are asked too, and stay listed while they
        // are off. So is whatever is listed now as a station of this fleet.
        let known = self.known_stations();
        let mut changed = false;
        {
            let mut peers = self.fleet.peers.lock().unwrap();
            for k in &known {
                list(&k.addr, Kind::Member, online_at.get(&k.addr).copied());
                peers.entry(k.station.clone()).or_insert_with(|| {
                    let mut p = k.clone();
                    p.state = "unreachable".into();
                    changed = true;
                    p
                });
            }
            for p in peers.values().filter(|p| p.state != "not_linked") {
                list(&p.addr, Kind::Member, online_at.get(&p.addr).copied());
            }
        }
        *self.fleet.places.lock().unwrap() = places;
        if self.cfg.discovery && !names.is_empty() {
            *self.fleet.names.lock().unwrap() = names;
        }
        // A key this fleet stopped using is kept thirty days for the
        // stations still owed the new one: this drops what is older.
        if !self.trust.retired().is_empty() {
            let _ = self.trust.retired_change(|_| {});
        }
        // What this station itself dropped or was told to forget is put
        // into the roster whether or not anyone answers.
        changed |= self.roster_take(&[]);
        self.listed_now(changed);
        self.rounds
            .follow(&listed.into_values().collect::<Vec<_>>());
        self.rounds.kick(self, select);
    }

    /// The members asked at a look have answered, or run out of time.
    fn looked(&self, began: std::time::Instant) {
        self.sync_looked();
        // Tailscale key expiry: the warnings that are due (tailnet.rs).
        // After the answers, since which station sends a warning on
        // depends on which stations are there.
        self.tailnet_watch();
        self.perf.look.record(began.elapsed());
    }

    /// The roster of the fleet's members, brought up to date with what the
    /// stations in `online` say. One at a time: rounds end together.
    fn roster_take(&self, online: &[Peer]) -> bool {
        let _one = self.fleet.roster.lock().unwrap();
        self.remember_stations(online);
        let places = self.fleet.places.lock().unwrap().clone();
        self.roster_merge(online, &places)
    }

    /// The list of stations may have changed: the links the Performance
    /// page shows follow it, and the decks are told when it did.
    fn listed_now(&self, changed: bool) {
        let listed: Vec<(String, String, String)> = self
            .fleet
            .peers()
            .into_iter()
            .map(|p| (p.addr, p.station, p.state))
            .collect();
        self.perf.links_follow(&listed);
        if changed {
            self.emit("fleet.updated", None, None, json!({}));
        }
    }

    /// Ask the machine at `addr` whether it runs spandeck, and for the full
    /// picture if it is of this fleet. Two and a half seconds at most.
    async fn probe(&self, addr: &str) -> (Option<Value>, Option<Reading>) {
        // The unsigned answer is timed, and says that station's clock: the
        // link's round trip, and how far the clocks are apart, for the
        // Performance page.
        let reading = std::sync::Mutex::new(None);
        let r = tokio::time::timeout(PROBE_DEADLINE, async {
            let (sent, began) = (now_ms(), std::time::Instant::now());
            let hello = self.fleet.call_open(addr, "station.hello", json!({})).await;
            let read = hello.as_ref().ok().map(|h| {
                (
                    began.elapsed(),
                    sent,
                    h.get("now_ms").and_then(|n| n.as_u64()),
                )
            });
            *reading.lock().unwrap() = read;
            match hello {
                // Same fleet: ask for the full picture, signed.
                Ok(h)
                    if h.get("fleet").and_then(|f| f.as_str())
                        == Some(self.trust.fleet_id().as_str()) =>
                {
                    let info = self.fleet.call_addr(addr, "station.info", json!({})).await;
                    // It holds this fleet's key and still refuses the
                    // signed call: when its clock is surely more than
                    // two minutes from this one's, that is why. It
                    // is listed, as a station that cannot be reached,
                    // and the reason is said (audit, section 10).
                    let apart = read.and_then(|(took, sent, theirs)| {
                        let rtt = took.as_millis() as u64;
                        let diff = theirs? as i64 - (sent + rtt / 2) as i64;
                        (diff.unsigned_abs() > crate::perf::CLOCK_LIMIT_MS as u64 + rtt / 2 + 1)
                            .then_some(diff)
                    });
                    match (info, apart) {
                        (Err(_), Some(diff)) => Ok(json!({
                            "station": h.get("station"), "version": h.get("version"),
                            "protocol": h.get("protocol"), "os": h.get("os"), "clock_refused": diff,
                        })),
                        (info, _) => info,
                    }
                }
                Ok(mut h) => {
                    h["not_linked"] = json!(true);
                    Ok(h)
                }
                // A station from before fleets existed still answers this.
                Err(_) => self.fleet.call_addr(addr, "station.info", json!({})).await,
            }
        })
        .await;
        let reading = reading.into_inner().unwrap();
        (r.ok().and_then(|r| r.ok()), reading)
    }

    /// Put what the machine at `addr` answered (or that it did not) into
    /// the list. Only entries at that address are touched: what another
    /// address answered, or has not answered yet, is not this round's.
    /// Returns the station, when it is one of this fleet that this station
    /// can work with.
    fn probe_taken(
        &self,
        addr: &str,
        info: Option<Value>,
        reading: Option<Reading>,
    ) -> Option<Peer> {
        let now = now_ms();
        let mut changed = false;
        let mut member = None;
        let answered = info.and_then(|info| {
            let station = info.get("station")?.as_str()?.to_string();
            (station != self.station).then_some((station, info))
        });
        {
            let mut peers = self.fleet.peers.lock().unwrap();
            match answered {
                // A machine that is not in this fleet and no longer answers
                // is not one of its stations that is off: it leaves the
                // list. (A station removed from the fleet and then stopped
                // would otherwise stay, shown as unreachable, with offers
                // to wake it.) A station of this fleet that is away stays.
                None => {
                    let before = peers.len();
                    peers.retain(|_, p| p.addr != addr || p.state != "not_linked");
                    changed |= peers.len() != before;
                    for p in peers.values_mut().filter(|p| p.addr == addr) {
                        if p.state != "unreachable" {
                            p.state = "unreachable".into();
                            changed = true;
                        }
                    }
                }
                Some((station, info)) if info.get("clock_refused").is_some() => {
                    let protocol =
                        info.get("protocol").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
                    // Not reached: what was last known of it is kept.
                    let p = peers.entry(station.clone()).or_insert_with(|| Peer {
                        station,
                        addr: addr.to_string(),
                        version: info["version"].as_str().unwrap_or("?").to_string(),
                        protocol,
                        state: String::new(),
                        last_seen_ms: 0,
                        info: json!({"os": info.get("os")}),
                    });
                    if p.state != "unreachable" {
                        p.state = "unreachable".into();
                        changed = true;
                    }
                }
                Some((station, info)) => {
                    let protocol =
                        info.get("protocol").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
                    let state = if info.get("not_linked").is_some() {
                        "not_linked"
                    } else if protocol == PROTOCOL {
                        "online"
                    } else {
                        "update_needed"
                    };
                    // A station that answers at two addresses is listed at
                    // one of them, always the same one (the later in
                    // order, as when every address was asked together).
                    let elsewhere = peers.get(&station).is_some_and(|p| {
                        p.addr.as_str() > addr
                            && p.state == state
                            && now.saturating_sub(p.last_seen_ms) < 25_000
                    });
                    if !elsewhere {
                        changed |= peers.get(&station).map(|p| p.state.as_str()) != Some(state);
                        let peer = Peer {
                            station: station.clone(),
                            addr: addr.to_string(),
                            version: info
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                                .to_string(),
                            protocol,
                            state: state.to_string(),
                            last_seen_ms: now,
                            info,
                        };
                        member = (state == "online").then(|| peer.clone());
                        peers.insert(station, peer);
                    }
                }
            }
        }
        // It answers: messages waiting for it are sent now, not when the
        // wait after the last failed try is over (relay.rs).
        if let Some(peer) = &member {
            self.relay.back(&peer.station);
        }
        // The roster of the fleet's members is the same on every station.
        if let Some(peer) = &member {
            changed |= self.roster_take(std::slice::from_ref(peer));
        }
        self.listed_now(changed);
        if let Some((took, sent, theirs)) = reading {
            self.perf.link_hello(addr, took, sent, theirs);
        }
        member
    }

    /// Everything this station does with the machine at `addr`, once: is a
    /// station there, and if it is one of this fleet, what the two owe each
    /// other. It runs in a task of its own (rounds.rs) and is cut off at
    /// `ROUND_DEADLINE`; nothing waits for it but a caller that asked to.
    async fn station_round(&self, addr: &str) -> Outcome {
        let (info, reading) = self.probe(addr).await;
        let answered = info.is_some();
        let mut member = self.probe_taken(addr, info, reading);
        // A station that was away when the fleet's key was replaced, and is
        // back: handed the key, it is in the fleet when asked again.
        if answered && self.rekey_stragglers(addr).await {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let (info, reading) = self.probe(addr).await;
            member = self.probe_taken(addr, info, reading);
        }
        if let Some(peer) = member {
            self.station_sync(&peer).await;
        }
        if answered {
            Outcome::Answered
        } else {
            Outcome::Silent
        }
    }

    /// What this station and one station of its fleet owe each other at
    /// every look. Each call is to that station alone.
    async fn station_sync(&self, peer: &Peer) {
        // A newer copy of a synced document is taken before this station's
        // is offered: see sync.rs.
        self.sync_pull(peer).await;
        self.setup_offer(peer, &self.setup()).await;
        self.deck_offer(peer, &self.deck(), true).await;
        self.look_offer(peer, &self.look(), true).await;
        self.prefs_offer(peer, &self.prefs(), true).await;
        self.tools_offer(peer, &self.tools_doc(), true).await;
        self.roles_offer(peer, &self.roles_doc(), true).await;
        // A station that was away when a request arrived here hears of it.
        self.trust_relay_to(peer).await;
        self.trust_sync_with(peer).await;
        // Tailscale key expiry: what the station says of its own key.
        self.tailnet_ask(peer).await;
        self.look_fonts_sync(None, Some(peer)).await;
    }

    pub fn fleet_view(&self) -> Value {
        let mut me = self.info();
        me["state"] = json!("online");
        me["self"] = json!(true);
        me["agents"] = json!(self
            .pane_views()
            .into_iter()
            .filter(|p| p["exited"] == json!(false))
            .collect::<Vec<_>>());
        me["projects"] = json!(self
            .projects()
            .iter()
            .map(|p| json!({"name": p["name"], "open": p["open"]}))
            .collect::<Vec<_>>());
        let names = self.fleet.names.lock().unwrap().clone();
        let peers: Vec<Value> = self
            .fleet
            .peers()
            .into_iter()
            .map(|p| {
                let mut v = p.info.clone();
                v["station"] = json!(p.station);
                v["state"] = json!(p.state);
                v["addr"] = json!(p.addr);
                v["last_seen_ms"] = json!(p.last_seen_ms);
                v["self"] = json!(false);
                // Where its deck is over HTTPS is passed to a page only
                // when this station's own Tailscale client gives that
                // name for its address: a page that loses this station
                // connects there and shows its token (https.rs).
                let said = crate::https::brief(&p.info);
                let host = p.addr.rsplit_once(':').map_or(p.addr.as_str(), |(h, _)| h);
                let known = names.get(host).map(String::as_str);
                if !said.is_null() && crate::https::deck_origins(&p.addr, &said, known).len() < 2 {
                    v["https"] = json!({"on": false, "state": "unconfirmed", "port": said["port"],
                        "why": "this station's own Tailscale does not give that name for its address"});
                }
                // A station that is off: the version it last had.
                if v.get("version").is_none() {
                    v["version"] = json!(p.version);
                }
                if p.state != "online" {
                    // Never imply anything about a station we cannot see.
                    v["working"] = Value::Null;
                    v["needs_you"] = Value::Null;
                }
                // It answers, holds this fleet's key, and its clock is more
                // than two minutes from this station's: said as such.
                if let Some(c) = self.perf.clock_of(&p.station) {
                    if p.state == "unreachable" && c.refused() {
                        v["why"] = json!("clock");
                    }
                    v["clock_diff_ms"] = json!(c.diff_ms);
                }
                v
            })
            .collect();
        let mut stations: Vec<Value> = std::iter::once(me).chain(peers).collect();
        // Is a newer spandeck published? The newest any station learned,
        // and beside each station what that means for it (newrelease.rs).
        let update = self.fleet_update(&mut stations);
        json!({"station": self.station, "version": VERSION, "stations": stations, "update": update, "stops": self.stops(20)})
    }

    // ----------------------------------------------------------------- stop-all

    /// Interrupt every agent turn that was running when the stop was issued.
    /// Processes stay (Guarantees: Stop). Applied once, however often the
    /// same stop arrives.
    ///
    /// "When it was issued" is on this station's own clock. A station that
    /// announces `stop_cutoff` sends that moment already converted
    /// (`cutoff_ms`), and a stop that arrives late is still aimed at it. One
    /// from before says how long ago the stop was issued when it sent it
    /// (`age_ms`), and this station counts back from now; older still, only
    /// its own clock's time (`issued_ms`), used as it is. See stops.rs.
    ///
    /// `marks`: the stop's place in each pane's input, taken when the stop
    /// was read (`Daemon::stop_marks`). Each pane is stopped on a thread of
    /// its own, since a program that is not reading takes two seconds to
    /// be found out; such a pane is listed under `not_reading`, never under
    /// `stopped`. Waits, so it is called off the request threads.
    pub fn apply_stop(
        &self,
        stop_id: &str,
        issued_ms: u64,
        age_ms: Option<u64>,
        cutoff_ms: Option<u64>,
        by: &str,
        mut marks: crate::holds::StopMarks,
    ) -> Value {
        let cutoff_ms = cutoff_ms.filter(|_| self.flag_on(stops::CAP, true));
        let aim = stops::aim(now_ms(), issued_ms, age_ms, cutoff_ms);
        let (result, first) = self.stops.applier.apply(stop_id, || {
            let mut hit = vec![];
            for p in self.list_panes() {
                let work = {
                    let m = p.meta.lock().unwrap();
                    stops::Work {
                        busy: m.exit.is_none()
                            && m.agent.is_agent()
                            && matches!(m.state, AgentState::Working | AgentState::NeedsYou),
                        created_ms: m.created_ms,
                        state_since_ms: m.state_since_ms,
                        turn_started_ms: m.turn.started_ms,
                        busy_since_ms: m.busy_since_ms,
                    }
                };
                if stops::hit(&work, &aim) {
                    let mark = marks.remove(&p.id).unwrap_or_else(|| p.term.mark_stop());
                    hit.push((p.id.clone(), mark));
                }
            }
            // The panes this stop is not for are held up no longer.
            marks.clear();
            let runtime = tokio::runtime::Handle::try_current().ok();
            let results: Vec<(String, Option<bool>)> = std::thread::scope(|scope| {
                let jobs: Vec<_> = hit
                    .into_iter()
                    .map(|(id, mark)| {
                        let runtime = runtime.clone();
                        scope.spawn(move || {
                            let _in = runtime.as_ref().map(|r| r.enter());
                            let done = self.stop_at(&id, by, mark, false);
                            (id, done.ok().map(|s| s.delivered))
                        })
                    })
                    .collect();
                jobs.into_iter().filter_map(|j| j.join().ok()).collect()
            });
            let ids = |want: bool| -> Vec<&String> {
                results
                    .iter()
                    .filter(|(_, delivered)| *delivered == Some(want))
                    .map(|(id, _)| id)
                    .collect()
            };
            json!({
                "stopped": ids(true), "not_reading": ids(false),
                "station": self.station, "applied_ms": now_ms(),
            })
        });
        if first {
            self.record(
                "station.stop_all",
                None,
                None,
                None,
                Some(by),
                json!({
                    "id": stop_id, "issued_ms": aim.ms(),
                    "aimed_by": if matches!(aim, stops::Aim::Cutoff(_)) { "cutoff" } else { "age" },
                    "stopped": result["stopped"].as_array().map_or(0, |s| s.len()),
                    "not_reading": result["not_reading"].as_array().map_or(0, |s| s.len()),
                }),
            );
        }
        result
    }

    /// `apply_stop` for a stop that arrives in a request: each pane's place
    /// for it is taken now, as the request is read, and the stopping (which
    /// waits for a program that is not reading) runs off the request
    /// threads.
    pub async fn apply_stop_off_thread(
        &self,
        stop_id: &str,
        issued_ms: u64,
        age_ms: Option<u64>,
        cutoff_ms: Option<u64>,
        by: &str,
    ) -> Result<Value> {
        let marks = self.stop_marks();
        let (d, id, by) = (self.clone(), stop_id.to_string(), by.to_string());
        tokio::task::spawn_blocking(move || {
            d.apply_stop(&id, issued_ms, age_ms, cutoff_ms, &by, marks)
        })
        .await
        .map_err(|e| invalid(e.to_string()))
    }

    /// Stop-all for this station, one other station, or the whole fleet.
    /// Each target gets its own row: requested, confirmed or unreachable.
    ///
    /// Nothing waits for anything else: this station's agents are stopped,
    /// the rows are written, and every other station is sent its stop, all
    /// at the same time and each station within its own five seconds. The
    /// database is given one second; a row it has not taken by then is held
    /// in memory, said to be not saved, and written by the stops task.
    ///
    /// `marks`: the stop's place in the input of this station's panes,
    /// taken when the request was read.
    pub async fn stop_all(
        &self,
        scope: &str,
        by: &str,
        marks: crate::holds::StopMarks,
        mut legs: crate::rpc::StopLegs,
        rid_key: Option<&str>,
    ) -> Result<Value> {
        let issued = now_ms();
        // The fleet is its members: a machine that has not joined cannot
        // be sent a stop (it does not take this fleet's calls), and a row
        // for it would say "unreachable" for ever.
        let member = |p: &Peer| p.state != "not_linked";
        let mut targets: Vec<String> = match scope {
            "fleet" => std::iter::once(self.station.clone())
                .chain(
                    self.fleet
                        .peers()
                        .iter()
                        .filter(|p| member(p))
                        .map(|p| p.station.clone()),
                )
                .collect(),
            "station" | "" => vec![self.station.clone()],
            other => vec![other.to_string()],
        };
        targets.dedup();
        // Refused before anything is stopped.
        for station in targets.iter().filter(|s| **s != self.station) {
            match self.fleet.peer(station) {
                None => return Err(Error::NotFound(format!("station {station}"))),
                Some(p) if !member(&p) => {
                    return Err(invalid(format!(
                        "{station} is not in this fleet, so it cannot be stopped from here"
                    )))
                }
                Some(_) => {}
            }
        }
        let book = &self.stops.book;
        let ids = book.issue(&targets, scope, by, issued);
        // Asked with a request id: a second try of the same request is
        // given these stops, and issues none. Not waited for (rids.rs).
        if let Some(key) = rid_key {
            self.rids.stop_issued(key, &ids, issued);
        }
        let (here, there): (Vec<_>, Vec<_>) = ids
            .iter()
            .zip(&targets)
            .partition(|(_, station)| **station == self.station);
        // A station's leg (the stop's place in the order of the connection
        // that read it) is let go of when that station's part is done, and
        // the legs of stations this stop is not for at once.
        legs.retain(|station, _| targets.contains(station));
        let leg = legs.remove(&self.station);
        // This station: on a thread of its own, since stopping a pane also
        // writes the ship's log, and a database that is slow to take that
        // must not hold up the stops for the other stations.
        let local = here.first().map(|(id, _)| {
            let (d, id, by) = (self.clone(), id.to_string(), by.to_string());
            async move {
                if let Some(leg) = &leg {
                    leg.turn().await;
                }
                tokio::task::spawn_blocking(move || {
                    let result = d.apply_stop(&id, issued, None, None, &by, marks);
                    d.stops.book.mark(&id, "confirmed", result, now_ms());
                    drop(leg);
                })
                .await
            }
        });
        let cutoff = self.flag_on(stops::CAP, true);
        let sends = futures_join_all(there.iter().map(|(id, station)| {
            let leg = legs.remove(*station);
            async move {
                if let Some(leg) = &leg {
                    leg.turn().await;
                }
                let sent = stops::send(book, self, id, cutoff).await;
                drop(leg);
                sent
            }
        }));
        let (wrote, _, _) = tokio::join!(
            self.stops_sync(false, Duration::from_secs(1)),
            async {
                if let Some(local) = local {
                    let _ = local.await;
                }
            },
            sends
        );
        // What came of each, written too. A database that did not take the
        // rows in its second is not given another: the writer has them,
        // and the answer says "not yet saved".
        let again = if wrote {
            Duration::from_secs(1)
        } else {
            Duration::ZERO
        };
        self.stops_sync(false, again).await;
        self.emit("stop.updated", None, None, json!({"ids": ids}));
        Ok(
            json!({"issued_ms": issued, "stops": self.stops(50).into_iter().filter(|s| ids.iter().any(|r| s["id"] == json!(r))).collect::<Vec<_>>()}),
        )
    }

    /// Write the stops' rows, off the request threads, waiting at most a
    /// second: a database that is locked or full delays no stop. `load`:
    /// first take in the rows that wait in the database (after a restart).
    /// True when every row is written.
    async fn stops_sync(&self, load: bool, wait: Duration) -> bool {
        let book = self.stops.book.clone();
        let job = tokio::task::spawn_blocking(move || {
            if load {
                book.load_waiting();
            }
            book.save_within(wait)
        });
        matches!(
            tokio::time::timeout(wait + Duration::from_millis(500), job).await,
            Ok(Ok(true))
        )
    }

    /// A stop for an unreachable station is delivered when it comes back.
    /// Run by its own task (daemon.rs): every two seconds, and at once when
    /// the list of stations changes.
    pub(crate) async fn pump_stops(&self) {
        let book = &self.stops.book;
        let unsaved = book.unsaved();
        let saved = self.stops_sync(true, Duration::from_secs(1)).await;
        let round = stops::deliver_waiting(book, self, self.flag_on(stops::CAP, true)).await;
        for (id, station) in &round.dropped {
            self.record(
                "stop.dropped",
                None,
                None,
                None,
                None,
                json!({"id": id, "station": station, "why": "not in this fleet"}),
            );
        }
        let mut ids: Vec<&String> = round.tried.iter().collect();
        ids.extend(round.dropped.iter().map(|(id, _)| id));
        if !round.tried.is_empty() {
            self.stops_sync(false, Duration::from_secs(1)).await;
        }
        if !ids.is_empty() || (unsaved > 0 && saved) {
            self.emit("stop.updated", None, None, json!({"ids": ids}));
        }
    }

    pub fn stops(&self, limit: u32) -> Vec<Value> {
        self.stops.book.list(limit, now_ms())
    }

    /// Recall across stations: each station answers from its own scrubbed copy.
    pub async fn recall_fleet(&self, query: &str, project: &str, limit: u32) -> Vec<Value> {
        let peers: Vec<String> = self
            .fleet
            .peers()
            .into_iter()
            .filter(|p| p.state == "online")
            .map(|p| p.station)
            .collect();
        let calls = peers.iter().map(|station| async move {
            self.fleet
                .call(
                    station,
                    "recall.local",
                    json!({"query": query, "project": project, "limit": limit}),
                )
                .await
                .ok()
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
        });
        futures_join_all(calls)
            .await
            .into_iter()
            .flatten()
            .collect()
    }

    pub fn require_peer(&self, station: &str) -> Result<Peer> {
        self.fleet
            .peer(station)
            .ok_or_else(|| invalid(format!("station {station} is not in the fleet")))
    }
}

/// Sending a stop from a running station: its clock, what it has read of
/// the other station's, and the calls over the fleet's HTTP client (so a
/// clock reading and the stop that follows it share a connection).
impl stops::StopLink for Daemon {
    fn now_ms(&self) -> u64 {
        now_ms()
    }

    fn samples(&self, station: &str) -> (Vec<crate::perf::ClockSample>, u64) {
        (self.perf.clock_samples(station), crate::perf::secs())
    }

    fn peer(&self, station: &str) -> Option<(String, bool)> {
        self.fleet.peer(station).map(|p| {
            let cutoff = p.info["caps"]
                .as_array()
                .is_some_and(|caps| caps.iter().any(|c| c == stops::CAP));
            (p.state, cutoff)
        })
    }

    fn hello<'a>(
        &'a self,
        station: &'a str,
    ) -> BoxFuture<'a, std::result::Result<stops::Hello, String>> {
        Box::pin(async move {
            let peer = self
                .fleet
                .peer(station)
                .ok_or_else(|| format!("station {station} is not in the fleet"))?;
            let (sent, began) = (now_ms(), std::time::Instant::now());
            let h = self
                .fleet
                .call_open(&peer.addr, "station.hello", json!({}))
                .await?;
            let theirs = h.get("now_ms").and_then(|n| n.as_u64());
            // The reading is kept with the looks' own, for the next stop
            // and for the Performance page.
            self.perf
                .link_hello(&peer.addr, began.elapsed(), sent, theirs);
            Ok(stops::Hello {
                station: h.get("station").and_then(|s| s.as_str()).map(String::from),
                now_ms: theirs,
                cutoff: h["caps"]
                    .as_array()
                    .is_some_and(|caps| caps.iter().any(|c| c == stops::CAP)),
            })
        })
    }

    fn apply<'a>(
        &'a self,
        station: &'a str,
        params: Value,
    ) -> BoxFuture<'a, std::result::Result<Value, String>> {
        Box::pin(self.fleet.call(station, "stop.apply", params))
    }
}

/// Run futures concurrently and collect their outputs (no extra crate).
pub async fn futures_join_all<F: std::future::Future>(
    futs: impl IntoIterator<Item = F>,
) -> Vec<F::Output> {
    let mut pinned: Vec<_> = futs.into_iter().map(|f| Box::pin(f)).collect();
    let mut out: Vec<Option<F::Output>> = (0..pinned.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (i, f) in pinned.iter_mut().enumerate() {
            if out[i].is_none() {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => out[i] = Some(v),
                    std::task::Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await;
    out.into_iter().flatten().collect()
}

/// A round with a real station (rounds.rs).
impl crate::rounds::Link for Daemon {
    fn round<'a>(&'a self, addr: &'a str) -> BoxFuture<'a, Outcome> {
        Box::pin(self.station_round(addr))
    }

    fn ended(&self, addr: &str, took: Duration, outcome: Outcome) {
        self.perf
            .link_round(addr, took, outcome == Outcome::Answered);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's Mac "monitoring" (17 Nov 2026): Tailscale's program,
    /// started with no terminal, printed that its window failed to start and
    /// ended with success, and the station took that for Tailscale.
    #[test]
    fn what_tailscale_prints_for_its_version_is_told_from_a_window_that_would_not_open() {
        assert!(says_a_version("1.102.3\n  tailscale commit: 9329c36\n"));
        assert!(says_a_version("1.104.5-t0123abcd\n"));
        assert!(!says_a_version(
            "The Tailscale GUI failed to start: The operation couldn\u{2019}t be completed. (Tailscale.CLIError error 3.)"
        ));
        assert!(!says_a_version(""));
        let cmd = tailscale_cmd("tailscale");
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "TAILSCALE_BE_CLI" && v == Some(std::ffi::OsStr::new("1"))));
    }
    use std::io::ErrorKind as K;

    /// The audit (FAILURES section 4): the owner was shown "golf:
    /// unreachable: error sending request for url (http://100.…/api/peer/rpc)",
    /// the transport's own words, which name an address and say nothing of
    /// what happened. Each way a call can get no answer has its own plain
    /// words, which follow the station's name and hold no address.
    #[test]
    fn a_call_with_no_answer_is_said_in_plain_words() {
        let said = |connect, timeout, io| NoAnswer::from(connect, timeout, io).words();
        assert_eq!(
            said(false, true, None),
            "did not answer within 5 s",
            "sent, and the call's five seconds ran out"
        );
        assert_eq!(
            said(true, false, Some(K::ConnectionRefused)),
            "refused the connection (spandeck may be stopped there)"
        );
        // No connection in the second and a half given to make one: the
        // machine or its Tailscale, which this end cannot tell apart.
        assert!(said(true, true, None).starts_with("could not be reached within 1.5 s"));
        assert!(said(true, true, Some(K::TimedOut)).starts_with("could not be reached"));
        // No way out of this machine: that is this station's own network.
        for io in [K::NetworkUnreachable, K::HostUnreachable] {
            assert!(said(true, false, Some(io)).contains("Tailscale may not be connected here"));
        }
        assert_eq!(
            said(false, false, Some(K::ConnectionReset)),
            "ended the connection before it answered"
        );
        for why in [
            NoAnswer::Refused,
            NoAnswer::NoConnection,
            NoAnswer::NoRoute,
            NoAnswer::Timeout,
            NoAnswer::Broke,
        ] {
            let words = why.words();
            assert!(NoAnswer::said(words));
            assert!(
                !words.contains("http") && !words.contains("url") && !words.contains("error"),
                "{words}"
            );
        }
        // An answer that is an error is the station's own words, and is
        // not taken for one of these.
        assert!(!NoAnswer::said("no such pane"));
    }
}
