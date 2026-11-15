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
use crate::stops;

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

/// Who is on the other end of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remote {
    /// This machine. Needs a token.
    Loopback,
    /// A device under the user's own Tailscale login: the user, or their station.
    Owner { login: String, node: String },
    /// Anything else. Never trusted automatically.
    Stranger(String),
}

pub struct FleetState {
    pub http: reqwest::Client,
    peers: Mutex<BTreeMap<String, Peer>>,
    whois: Mutex<HashMap<IpAddr, (u64, Remote)>>,
    me: Mutex<Option<(u64, Option<TailnetSelf>)>>,
    /// Stations dropped from this station's list since it last looked at
    /// the fleet: the next look says so to the roster (manage.rs).
    dropped: Mutex<Vec<String>>,
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
        FleetState {
            http,
            peers: Mutex::new(BTreeMap::new()),
            whois: Mutex::new(HashMap::new()),
            me: Mutex::new(None),
            dropped: Mutex::new(vec![]),
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
                if now_ms().saturating_sub(*at) < 60_000 {
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
        let remote = match (self.tailnet_self(), tailscale_whois(ip)) {
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
        let body =
            json!({"method": method, "params": params, "from": self.station, "protocol": PROTOCOL})
                .to_string()
                .into_bytes();
        let mut req = self
            .http
            .post(format!("http://{addr}/api/peer/rpc"))
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
            self.perf.link_call(addr, began.elapsed(), Some(why));
            self.note_answer(addr, false);
            // No connection was made: nothing was sent.
            let sent = match e.is_connect() {
                true => Sent::No,
                false => Sent::Maybe,
            };
            (sent, format!("unreachable: {e}"))
        })?;
        self.note_answer(addr, true);
        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| {
            self.perf
                .link_call(addr, began.elapsed(), Some("bad reply"));
            (Sent::Maybe, format!("bad reply: {e}"))
        })?;
        let refused = match body.pointer("/error/code").and_then(|c| c.as_str()) {
            Some("clock") => Some("clock"),
            _ if status == reqwest::StatusCode::UNAUTHORIZED => Some("refused"),
            _ => None,
        };
        self.perf.link_call(addr, began.elapsed(), refused);
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
fn tailscale_bin() -> Option<String> {
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
        std::process::Command::new(p)
            .arg("version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

fn tailscale_json(args: &[&str]) -> Option<Value> {
    static BIN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let bin = BIN.get_or_init(tailscale_bin).as_ref()?;
    let out = std::process::Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

pub fn tailscale_status() -> Option<Value> {
    tailscale_json(&["status", "--json"])
}

fn tailscale_whois(ip: IpAddr) -> Option<Value> {
    tailscale_json(&["whois", "--json", &ip.to_string()])
}

impl Daemon {
    /// Find stations: every online device under the same login is asked whether
    /// it runs spandeck. Only those holding the same fleet key are trusted;
    /// the rest are listed as not linked until the owner joins them.
    pub(crate) async fn discover_peers(&self) {
        self.look_at_fleet(false).await;
    }

    /// One look at a time. `only_if_stale`: a caller that only needs the
    /// fleet to have been looked at lately, and waited for a look that was
    /// under way, does not look again.
    pub(crate) async fn look_at_fleet(&self, only_if_stale: bool) {
        let _one = self.sync.looking.lock().await;
        if only_if_stale && !self.sync_look_is_stale() {
            return;
        }
        let began = std::time::Instant::now();
        self.discover_once().await;
        // A station that was away when the fleet's key was replaced, and is
        // back: handed the key, it is in the fleet at the next look.
        if self.rekey_stragglers().await {
            tokio::time::sleep(Duration::from_millis(300)).await;
            self.discover_once().await;
        }
        // A newer copy of a synced document is taken before this station's
        // is offered: see sync.rs.
        self.sync_pull().await;
        self.setup_sync().await;
        self.deck_sync().await;
        self.look_sync().await;
        self.prefs_sync().await;
        self.sync_looked();
        // A station that was away when a request arrived here hears of it.
        self.trust_relay_send(false).await;
        self.trust_sync().await;
        // Tailscale key expiry: what each station says of its own key, and
        // the warnings that are due (tailnet.rs). Nothing else reads these.
        self.tailnet_ask().await;
        self.tailnet_watch();
        self.perf.look.record(began.elapsed());
    }

    async fn discover_once(&self) {
        let mut candidates: Vec<String> = self.cfg.peers.clone();
        // Where a station of this fleet can be: the addresses in this
        // station's config, and the owner's own devices, on or off. An
        // address another station names for a member is taken only if it
        // is one of these (manage.rs, the roster).
        let mut places: Vec<String> = self.cfg.peers.clone();
        if self.cfg.discovery {
            let fleet = &self.fleet;
            let me = tokio::task::block_in_place(|| fleet.tailnet_self());
            let status = tokio::task::block_in_place(tailscale_status);
            // The same output says when each device's key expires.
            self.tailnet_refresh(status.as_ref());
            // And which addresses this machine has now: the web server
            // listens on a new one and lets go of one that is gone. When
            // the client does not answer, what it last said stands.
            if let Some(status) = &status {
                let now = Some(OwnAddrs::of(status));
                fleet.own_addrs.send_if_modified(|held| {
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
                        places.push(format!("{ip}:{}", self.cfg.web_port));
                        if online {
                            candidates.push(format!("{ip}:{}", self.cfg.web_port));
                        }
                    }
                }
            }
        }
        // Stations seen before are asked too, and stay listed while they are off.
        let known = self.known_stations();
        {
            let mut peers = self.fleet.peers.lock().unwrap();
            for k in &known {
                candidates.push(k.addr.clone());
                peers.entry(k.station.clone()).or_insert_with(|| {
                    let mut p = k.clone();
                    p.state = "unreachable".into();
                    p
                });
            }
        }
        candidates.sort();
        candidates.dedup();
        let probes = candidates.iter().map(|addr| {
            let addr = addr.clone();
            async move {
                // The unsigned answer is timed, and says that station's
                // clock: the link's round trip, and how far the clocks are
                // apart, for the Performance page.
                let reading = std::sync::Mutex::new(None);
                let r = tokio::time::timeout(Duration::from_millis(2500), async {
                    let (sent, began) = (now_ms(), std::time::Instant::now());
                    let hello = self
                        .fleet
                        .call_open(&addr, "station.hello", json!({}))
                        .await;
                    let read = hello.as_ref().ok().map(|h| {
                        (began.elapsed(), sent, h.get("now_ms").and_then(|n| n.as_u64()))
                    });
                    *reading.lock().unwrap() = read;
                    match hello {
                        // Same fleet: ask for the full picture, signed.
                        Ok(h)
                            if h.get("fleet").and_then(|f| f.as_str())
                                == Some(self.trust.fleet_id().as_str()) =>
                        {
                            let info = self.fleet.call_addr(&addr, "station.info", json!({})).await;
                            // It holds this fleet's key and still refuses the
                            // signed call: when its clock is surely more than
                            // two minutes from this one's, that is why. It
                            // is listed, as a station that cannot be reached,
                            // and the reason is said (audit, section 10).
                            let apart = read.and_then(|(took, sent, theirs)| {
                                let rtt = took.as_millis() as u64;
                                let diff = theirs? as i64 - (sent + rtt / 2) as i64;
                                (diff.unsigned_abs()
                                    > crate::perf::CLOCK_LIMIT_MS as u64 + rtt / 2 + 1)
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
                        Err(_) => self.fleet.call_addr(&addr, "station.info", json!({})).await,
                    }
                })
                .await;
                let reading = reading.into_inner().unwrap();
                (addr, r.ok().and_then(|r| r.ok()), reading)
            }
        });
        let results = futures_join_all(probes).await;
        let mut changed = false;
        let now = now_ms();
        {
            let mut peers = self.fleet.peers.lock().unwrap();
            let mut seen_addrs = vec![];
            for (addr, info, _) in &results {
                let (addr, Some(info)) = (addr.clone(), info.clone()) else {
                    continue;
                };
                let Some(station) = info
                    .get("station")
                    .and_then(|s| s.as_str())
                    .map(String::from)
                else {
                    continue;
                };
                if station == self.station {
                    continue;
                }
                let protocol = info.get("protocol").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
                if info.get("clock_refused").is_some() {
                    // Not reached: what was last known of it is kept.
                    let p = peers.entry(station.clone()).or_insert_with(|| Peer {
                        station,
                        addr,
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
                    continue;
                }
                let state = if info.get("not_linked").is_some() {
                    "not_linked"
                } else if protocol == PROTOCOL {
                    "online"
                } else {
                    "update_needed"
                };
                let prev = peers.get(&station).map(|p| p.state.clone());
                if prev.as_deref() != Some(state) {
                    changed = true;
                }
                seen_addrs.push(addr.clone());
                peers.insert(
                    station.clone(),
                    Peer {
                        station,
                        addr,
                        version: info
                            .get("version")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        protocol,
                        state: state.to_string(),
                        last_seen_ms: now,
                        info,
                    },
                );
            }
            // A machine that is not in this fleet and no longer answers is
            // not one of its stations that is off: it leaves the list. (A
            // station removed from the fleet and then stopped would otherwise
            // stay, shown as unreachable, with offers to wake it.) A station
            // of this fleet that is away is listed again from `known` above.
            let before = peers.len();
            peers.retain(|_, p| seen_addrs.contains(&p.addr) || p.state != "not_linked");
            changed |= peers.len() != before;
            for p in peers.values_mut() {
                if !seen_addrs.contains(&p.addr) && p.state != "unreachable" {
                    p.state = "unreachable".into();
                    changed = true;
                }
            }
        }
        let online: Vec<Peer> = self
            .fleet
            .peers()
            .into_iter()
            .filter(|p| p.state == "online")
            .collect();
        self.remember_stations(&online);
        // The roster of the fleet's members is the same on every station.
        changed |= self.roster_merge(&online, &places);
        // The links the Performance page shows are to the stations listed.
        let listed: Vec<(String, String, String)> = self
            .fleet
            .peers()
            .into_iter()
            .map(|p| (p.addr, p.station, p.state))
            .collect();
        self.perf.links_follow(&listed);
        for (addr, _, reading) in &results {
            if let Some((took, sent, theirs)) = reading {
                self.perf.link_hello(addr, *took, *sent, *theirs);
            }
        }
        if changed {
            self.emit("fleet.updated", None, None, json!({}));
        }
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
        json!({"station": self.station, "version": VERSION, "stations": std::iter::once(me).chain(peers).collect::<Vec<_>>(), "stops": self.stops(20)})
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
