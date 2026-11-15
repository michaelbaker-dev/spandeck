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
use crate::paths::{new_id, now_ms, Config};
use crate::relay::{BoxFuture, Envelope, RelayAck, Transport};

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
}

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
        }
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

    async fn post(
        &self,
        addr: &str,
        method: &str,
        params: Value,
        key: Option<Vec<u8>>,
    ) -> std::result::Result<Value, String> {
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
            format!("unreachable: {e}")
        })?;
        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| {
            self.perf
                .link_call(addr, began.elapsed(), Some("bad reply"));
            format!("bad reply: {e}")
        })?;
        let refused = match body.pointer("/error/code").and_then(|c| c.as_str()) {
            Some("clock") => Some("clock"),
            _ if status == reqwest::StatusCode::UNAUTHORIZED => Some("refused"),
            _ => None,
        };
        self.perf.link_call(addr, began.elapsed(), refused);
        if let Some(err) = body.get("error") {
            return Err(err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("error")
                .to_string());
        }
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
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
    /// Processes stay (Guarantees: Stop).
    ///
    /// "When it was issued" is worked out on this station's own clock: the
    /// station that issued it says how long ago that was (`age_ms`, a length
    /// of time, which two clocks that disagree still agree on), and this one
    /// counts back from now. A station from before this sends only its own
    /// clock's time (`issued_ms`), which is then used as it was.
    pub fn apply_stop(
        &self,
        stop_id: &str,
        issued_ms: u64,
        age_ms: Option<u64>,
        by: &str,
    ) -> Value {
        let issued_ms = match age_ms {
            Some(age) => now_ms().saturating_sub(age),
            None => issued_ms,
        };
        let key = format!("stop:{stop_id}");
        if let Some(prev) = self.db.kv_get(&key) {
            // Delivered twice (a retry after a lost reply): same answer, no second stop.
            return serde_json::from_str(&prev).unwrap_or(json!({"stopped": []}));
        }
        let mut stopped = vec![];
        for p in self.list_panes() {
            let hit = {
                let m = p.meta.lock().unwrap();
                m.exit.is_none()
                    && m.agent.is_agent()
                    && matches!(m.state, AgentState::Working | AgentState::NeedsYou)
                    && m.created_ms <= issued_ms
                    && m.state_since_ms <= issued_ms.max(m.turn.started_ms)
                    && m.turn.started_ms <= issued_ms
            };
            if hit && self.stop(&p.id, by).is_ok() {
                stopped.push(p.id.clone());
            }
        }
        let result = json!({"stopped": stopped, "station": self.station, "applied_ms": now_ms()});
        self.db.kv_set(&key, &result.to_string());
        self.record(
            "station.stop_all",
            None,
            None,
            None,
            Some(by),
            json!({"id": stop_id, "issued_ms": issued_ms, "stopped": stopped.len()}),
        );
        result
    }

    /// Stop-all for this station, one other station, or the whole fleet.
    /// Each target gets its own row: requested, confirmed or unreachable.
    pub async fn stop_all(&self, scope: &str, by: &str) -> Result<Value> {
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
        let mut rows = vec![];
        for station in targets {
            let id = new_id("x");
            let local = station == self.station;
            match self.fleet.peer(&station) {
                _ if local => {}
                None => return Err(Error::NotFound(format!("station {station}"))),
                Some(p) if !member(&p) => {
                    return Err(invalid(format!(
                        "{station} is not in this fleet, so it cannot be stopped from here"
                    )))
                }
                Some(_) => {}
            }
            self.db.with(|c| {
                c.execute(
                    "INSERT INTO stops(id,station,scope,issued,issued_by,status,updated) VALUES(?1,?2,?3,?4,?5,'requested',?4)",
                    rusqlite::params![id, station, scope, issued as i64, by],
                )
            })?;
            if local {
                let result = self.apply_stop(&id, issued, None, by);
                self.stop_mark(&id, "confirmed", &result);
            } else {
                self.stop_try(&id, &station, issued, by).await;
            }
            rows.push(id);
        }
        self.emit("stop.updated", None, None, json!({"ids": rows}));
        Ok(
            json!({"issued_ms": issued, "stops": self.stops(20).into_iter().filter(|s| rows.iter().any(|r| s["id"] == json!(r))).collect::<Vec<_>>()}),
        )
    }

    fn stop_mark(&self, id: &str, status: &str, result: &Value) {
        let _ = self.db.with(|c| {
            c.execute(
                "UPDATE stops SET status=?2, updated=?3, result=?4 WHERE id=?1",
                rusqlite::params![id, status, now_ms() as i64, result.to_string()],
            )
        });
    }

    async fn stop_try(&self, id: &str, station: &str, issued: u64, by: &str) {
        match self
            .fleet
            .call(
                station,
                "stop.apply",
                // How long ago, for the station's own clock; and when by
                // this one's, for a station from before it read the first.
                json!({"id": id, "issued_ms": issued, "age_ms": now_ms().saturating_sub(issued), "by": by}),
            )
            .await
        {
            Ok(result) => self.stop_mark(id, "confirmed", &result),
            // Unreachable is its own state. It is never shown as stopped.
            Err(e) => self.stop_mark(id, "unreachable", &json!({"error": e})),
        }
        self.emit("stop.updated", None, None, json!({"ids": [id]}));
    }

    /// A stop for an unreachable station is delivered when it comes back.
    pub(crate) async fn pump_stops(&self) {
        let waiting: Vec<(String, String, i64, Option<String>)> = self
            .db
            .with(|c| {
                let mut stmt = c.prepare("SELECT id, station, issued, issued_by FROM stops WHERE status IN ('requested','unreachable') AND station != ?1")?;
                let rows = stmt.query_map([&self.station], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                rows.collect()
            })
            .unwrap_or_default();
        for (id, station, issued, by) in waiting {
            match self.fleet.peer(&station).map(|p| p.state) {
                // A member that answers, on any version: it is tried.
                Some(state) if state == "online" || state == "update_needed" => {
                    self.stop_try(&id, &station, issued as u64, by.as_deref().unwrap_or("you"))
                        .await;
                }
                // It is no longer (or never was) in this fleet: the stop
                // cannot be delivered, and is not left saying it will be.
                Some(state) if state == "not_linked" => {
                    let _ = self
                        .db
                        .with(|c| c.execute("DELETE FROM stops WHERE id=?1", [&id]));
                    self.record(
                        "stop.dropped",
                        None,
                        None,
                        None,
                        None,
                        json!({"id": id, "station": station, "why": "not in this fleet"}),
                    );
                    self.emit("stop.updated", None, None, json!({"ids": [id]}));
                }
                _ => {}
            }
        }
    }

    pub fn stops(&self, limit: u32) -> Vec<Value> {
        self.db
            .with(|c| {
                let mut stmt = c.prepare("SELECT id,station,scope,issued,issued_by,status,updated,result FROM stops ORDER BY issued DESC, rowid DESC LIMIT ?1")?;
                let rows = stmt.query_map([limit], |r| {
                    let result: String = r.get(7)?;
                    let status: String = r.get(5)?;
                    Ok(json!({
                        "id": r.get::<_, String>(0)?, "station": r.get::<_, String>(1)?, "scope": r.get::<_, String>(2)?,
                        // The station that issued it, which keeps the row and delivers it,
                        // and how long ago that was by its own clock.
                        "from": self.station,
                        "age_ms": now_ms().saturating_sub(r.get::<_, i64>(3)? as u64),
                        "issued_ms": r.get::<_, i64>(3)?, "by": r.get::<_, Option<String>>(4)?,
                        "label": match status.as_str() { "confirmed" => "stop confirmed", "unreachable" => "station unreachable", _ => "stop requested" },
                        "status": status, "updated_ms": r.get::<_, i64>(6)?,
                        "result": serde_json::from_str::<Value>(&result).unwrap_or(Value::Null),
                    }))
                })?;
                rows.collect()
            })
            .unwrap_or_default()
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
