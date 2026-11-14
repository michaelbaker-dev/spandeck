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
    /// Signs calls to other stations with the fleet key.
    pub trust: Arc<crate::trust::TrustState>,
    pub station: String,
}

impl FleetState {
    pub fn new(cfg: &Config, trust: Arc<crate::trust::TrustState>) -> FleetState {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_millis(1500))
            .build()
            .unwrap_or_default();
        FleetState {
            http,
            peers: Mutex::new(BTreeMap::new()),
            whois: Mutex::new(HashMap::new()),
            me: Mutex::new(None),
            trust,
            station: cfg.station.clone(),
        }
    }

    pub fn peer(&self, station: &str) -> Option<Peer> {
        self.peers.lock().unwrap().get(station).cloned()
    }

    pub(crate) fn forget(&self, station: &str) {
        self.peers.lock().unwrap().remove(station);
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
        self.post(addr, method, params, true).await
    }

    /// A call any device under the owner's login may make without the fleet
    /// key: who are you, and asking to join.
    pub async fn call_open(
        &self,
        addr: &str,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, String> {
        self.post(addr, method, params, false).await
    }

    async fn post(
        &self,
        addr: &str,
        method: &str,
        params: Value,
        signed: bool,
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
        if signed {
            // The signature covers the exact bytes sent.
            req = req.header("x-spandeck-sig", self.trust.sign("rpc", &body));
        }
        let resp = req
            .body(body)
            .send()
            .await
            .map_err(|e| format!("unreachable: {e}"))?;
        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| format!("bad reply: {e}"))?;
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
        let mut candidates: Vec<String> = self.cfg.peers.clone();
        if self.cfg.discovery {
            let fleet = &self.fleet;
            let me = tokio::task::block_in_place(|| fleet.tailnet_self());
            if let (Some(me), Some(status)) = (me, tokio::task::block_in_place(tailscale_status)) {
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
                    if !same_user || !online || tagged || matches!(os, "iOS" | "tvOS" | "android") {
                        continue;
                    }
                    if let Some(ip) = peer
                        .get("TailscaleIPs")
                        .and_then(|a| a.as_array())
                        .and_then(|a| a.first())
                        .and_then(|i| i.as_str())
                    {
                        candidates.push(format!("{ip}:{}", self.cfg.web_port));
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
                let r = tokio::time::timeout(Duration::from_millis(2500), async {
                    let hello = self
                        .fleet
                        .call_open(&addr, "station.hello", json!({}))
                        .await;
                    match hello {
                        // Same fleet: ask for the full picture, signed.
                        Ok(h)
                            if h.get("fleet").and_then(|f| f.as_str())
                                == Some(self.trust.fleet_id().as_str()) =>
                        {
                            self.fleet.call_addr(&addr, "station.info", json!({})).await
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
                (addr, r.ok().and_then(|r| r.ok()))
            }
        });
        let results = futures_join_all(probes).await;
        let mut changed = false;
        let now = now_ms();
        {
            let mut peers = self.fleet.peers.lock().unwrap();
            let mut seen_addrs = vec![];
            for (addr, info) in results {
                let Some(info) = info else { continue };
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
        if changed {
            self.emit("fleet.updated", None, None, json!({}));
        }
        self.setup_sync().await;
        self.deck_sync().await;
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
                if p.state != "online" {
                    // Never imply anything about a station we cannot see.
                    v["working"] = Value::Null;
                    v["needs_you"] = Value::Null;
                }
                v
            })
            .collect();
        json!({"station": self.station, "version": VERSION, "stations": std::iter::once(me).chain(peers).collect::<Vec<_>>(), "stops": self.stops(20)})
    }

    // ----------------------------------------------------------------- stop-all

    /// Interrupt every agent turn that was running when the stop was issued.
    /// Processes stay (Guarantees: Stop).
    pub fn apply_stop(&self, stop_id: &str, issued_ms: u64, by: &str) -> Value {
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
        let mut targets: Vec<String> = match scope {
            "fleet" => std::iter::once(self.station.clone())
                .chain(self.fleet.peers().into_iter().map(|p| p.station))
                .collect(),
            "station" | "" => vec![self.station.clone()],
            other => vec![other.to_string()],
        };
        targets.dedup();
        let mut rows = vec![];
        for station in targets {
            let id = new_id("x");
            let local = station == self.station;
            if !local && self.fleet.peer(&station).is_none() {
                return Err(Error::NotFound(format!("station {station}")));
            }
            self.db.with(|c| {
                c.execute(
                    "INSERT INTO stops(id,station,scope,issued,issued_by,status,updated) VALUES(?1,?2,?3,?4,?5,'requested',?4)",
                    rusqlite::params![id, station, scope, issued as i64, by],
                )
            })?;
            if local {
                let result = self.apply_stop(&id, issued, by);
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
                json!({"id": id, "issued_ms": issued, "by": by}),
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
            if self
                .fleet
                .peer(&station)
                .is_some_and(|p| p.state == "online")
            {
                self.stop_try(&id, &station, issued as u64, by.as_deref().unwrap_or("you"))
                    .await;
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
