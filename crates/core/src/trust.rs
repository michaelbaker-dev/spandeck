//! Who may act as the owner from another machine.
//!
//! Tailscale says which device is connecting, not which program on it. So a
//! device under the owner's login is not enough: a station must hold the
//! fleet key, and a browser must hold a token the owner approved. Both are
//! handed out once, on the owner's approval at a station that already has
//! the key.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};

use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::daemon::{invalid, Daemon, Error, Result};
use crate::paths::{now_ms, random_token, Paths};

/// How far apart two stations' clocks may be for a signed call to count.
const SKEW_MS: u64 = 120_000;
/// How long a request waits for the owner.
const REQUEST_MS: u64 = 10 * 60 * 1000;
const MAX_PENDING: usize = 20;

#[derive(Clone, PartialEq)]
enum Decision {
    Waiting,
    Approved,
    Denied,
}

#[derive(Clone)]
struct Request {
    id: String,
    /// "station" asks for the fleet key; "device" asks for a browser token.
    kind: &'static str,
    name: String,
    from: String,
    /// Shown on both sides so the owner can see they match.
    code: String,
    collect_hash: [u8; 32],
    decision: Decision,
    expires_ms: u64,
}

#[derive(Clone)]
struct Outgoing {
    station: String,
    addr: String,
    id: String,
    collect: String,
    code: String,
    expires_ms: u64,
    problem: Option<String>,
}

pub struct TrustState {
    key: RwLock<Vec<u8>>,
    file: PathBuf,
    requests: Mutex<BTreeMap<String, Request>>,
    outgoing: Mutex<Option<Outgoing>>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn mac(key: &[u8], parts: &[&[u8]]) -> Hmac<Sha256> {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("any key length works");
    for p in parts {
        // Length-prefixed, so two different lists of parts never read the same.
        m.update(&(p.len() as u64).to_le_bytes());
        m.update(p);
    }
    m
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("new");
    spandeck_terminal::sys::write_private(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, path)
}

impl TrustState {
    /// Read this station's fleet key, or start a fleet of one.
    pub fn load(paths: &Paths) -> std::io::Result<TrustState> {
        let file = paths.state_dir.join("fleet.key");
        let key = match std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| unhex(t.trim()))
            .filter(|k| k.len() == 32)
        {
            Some(k) => k,
            None => {
                let t = random_token();
                write_private(&file, &t)?;
                unhex(&t).expect("a random token is hex")
            }
        };
        Ok(TrustState {
            key: RwLock::new(key),
            file,
            requests: Mutex::new(BTreeMap::new()),
            outgoing: Mutex::new(None),
        })
    }

    fn key(&self) -> Vec<u8> {
        self.key.read().unwrap().clone()
    }

    /// A public name for the fleet: stations with the same key show the same
    /// id, and the id says nothing about the key.
    pub fn fleet_id(&self) -> String {
        hex(&mac(&self.key(), &[b"fleet-id"]).finalize().into_bytes()[..8])
    }

    /// Sign one call to another station: `<time>.<nonce>.<mac>`.
    pub fn sign(&self, purpose: &str, body: &[u8]) -> String {
        let ts = now_ms().to_string();
        let nonce = &random_token()[..16];
        let tag = mac(
            &self.key(),
            &[purpose.as_bytes(), ts.as_bytes(), nonce.as_bytes(), body],
        )
        .finalize()
        .into_bytes();
        format!("{ts}.{nonce}.{}", hex(&tag))
    }

    /// Whether `sig` was made with this fleet's key for exactly this call.
    pub fn verify(&self, purpose: &str, body: &[u8], sig: &str) -> bool {
        let mut it = sig.split('.');
        let (Some(ts), Some(nonce), Some(tag), None) = (it.next(), it.next(), it.next(), it.next())
        else {
            return false;
        };
        let Ok(at) = ts.parse::<u64>() else {
            return false;
        };
        if now_ms().abs_diff(at) > SKEW_MS {
            return false;
        }
        let Some(tag) = unhex(tag) else {
            return false;
        };
        mac(
            &self.key(),
            &[purpose.as_bytes(), ts.as_bytes(), nonce.as_bytes(), body],
        )
        .verify_slice(&tag)
        .is_ok()
    }

    fn device_token(&self, id: &str) -> String {
        let tag = mac(&self.key(), &[b"device", id.as_bytes()])
            .finalize()
            .into_bytes();
        format!("dev.{id}.{}", hex(&tag))
    }

    /// The device id, if this is a token this fleet issued.
    fn device_of(&self, token: &str) -> Option<String> {
        let rest = token.strip_prefix("dev.")?;
        let (id, tag) = rest.split_once('.')?;
        let tag = unhex(tag)?;
        mac(&self.key(), &[b"device", id.as_bytes()])
            .verify_slice(&tag)
            .ok()
            .map(|_| id.to_string())
    }
}

fn short_code() -> String {
    // Letters that are hard to confuse when read off another screen.
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    unhex(&random_token()[..12])
        .unwrap_or_default()
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

fn clean_name(name: &str) -> String {
    let n: String = name
        .chars()
        .filter(|c| {
            c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')' | '\'')
        })
        .take(60)
        .collect();
    if n.trim().is_empty() {
        "unnamed".into()
    } else {
        n.trim().to_string()
    }
}

impl Daemon {
    /// A token the owner approved for a browser on another device.
    pub fn device_for_token(&self, token: &str) -> Option<String> {
        let id = self.trust.device_of(token)?;
        (!self.revoked_devices().contains(&id)).then_some(id)
    }

    fn revoked_devices(&self) -> Vec<String> {
        self.db
            .kv_get("trust.revoked")
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    fn known_devices(&self) -> Vec<Value> {
        self.db
            .kv_get("trust.devices")
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    /// What the owner sees: the fleet, requests waiting here, and this
    /// station's own request to join another fleet.
    pub fn trust_view(&self) -> Value {
        let now = now_ms();
        let requests: Vec<Value> = {
            let mut map = self.trust.requests.lock().unwrap();
            map.retain(|_, r| r.expires_ms > now);
            map.values()
                .filter(|r| r.decision == Decision::Waiting)
                .map(|r| json!({"id": r.id, "kind": r.kind, "name": r.name, "from": r.from, "code": r.code, "expires_ms": r.expires_ms}))
                .collect()
        };
        let outgoing = self.trust.outgoing.lock().unwrap().clone().map(|o| {
            json!({"station": o.station, "code": o.code, "expires_ms": o.expires_ms, "problem": o.problem})
        });
        let revoked = self.revoked_devices();
        let devices: Vec<Value> = self
            .known_devices()
            .into_iter()
            .filter(|d| {
                d["id"]
                    .as_str()
                    .is_some_and(|id| !revoked.iter().any(|r| r == id))
            })
            .collect();
        json!({
            "station": self.station,
            "fleet": self.trust.fleet_id(),
            "requests": requests,
            "outgoing": outgoing,
            "devices": devices,
        })
    }

    /// Someone on the owner's tailnet asks to be let in. Nothing is granted
    /// until the owner approves it here. Returns what the asker needs to
    /// collect the answer.
    pub fn trust_request(&self, kind: &str, name: &str, from: &str) -> Result<Value> {
        let kind: &'static str = match kind {
            "station" => "station",
            "device" => "device",
            _ => return Err(invalid("kind is station or device")),
        };
        let now = now_ms();
        let name = clean_name(name);
        let collect = random_token();
        let req = Request {
            id: crate::paths::new_id("q"),
            kind,
            name: name.clone(),
            from: from.to_string(),
            code: short_code(),
            collect_hash: Sha256::digest(collect.as_bytes()).into(),
            decision: Decision::Waiting,
            expires_ms: now + REQUEST_MS,
        };
        {
            let mut map = self.trust.requests.lock().unwrap();
            map.retain(|_, r| r.expires_ms > now);
            // One live request per asker, so a retry replaces the old one.
            map.retain(|_, r| !(r.kind == kind && r.from == from && r.name == name));
            if map.len() >= MAX_PENDING {
                return Err(Error::Denied(
                    "too many requests are waiting; try again later".into(),
                ));
            }
            map.insert(req.id.clone(), req.clone());
        }
        let (title, body) = match kind {
            "station" => (
                format!("Station {name} asks to join this fleet"),
                format!("From {from}. Approve only if it shows the code {}. It will be able to control every station.", req.code),
            ),
            _ => (
                format!("A browser asks to sign in: {name}"),
                format!("From {from}. Approve only if it shows the code {}. It will be able to control every station.", req.code),
            ),
        };
        self.notify("trust.request", None, None, &title, &body);
        self.record(
            "trust.request",
            None,
            None,
            None,
            None,
            json!({"kind": kind, "name": name, "from": from}),
        );
        self.emit("trust.updated", None, None, json!({}));
        Ok(
            json!({"id": req.id, "collect": collect, "code": req.code, "expires_ms": req.expires_ms, "station": self.station}),
        )
    }

    /// The asker polls this. The key or token is handed over once.
    pub fn trust_collect(&self, id: &str, collect: &str, from: &str) -> Result<Value> {
        let mut map = self.trust.requests.lock().unwrap();
        let now = now_ms();
        map.retain(|_, r| r.expires_ms > now);
        let Some(r) = map.get(id) else {
            return Ok(json!({"status": "gone"}));
        };
        let given: [u8; 32] = Sha256::digest(collect.as_bytes()).into();
        if r.from != from || !crate::rpc::constant_eq(&hex(&given), &hex(&r.collect_hash)) {
            return Err(Error::Denied("not the one that asked".into()));
        }
        match r.decision {
            Decision::Waiting => Ok(json!({"status": "waiting"})),
            Decision::Denied => {
                map.remove(id);
                Ok(json!({"status": "denied"}))
            }
            Decision::Approved => {
                let r = map.remove(id).expect("checked above");
                Ok(match r.kind {
                    "station" => json!({"status": "approved", "key": hex(&self.trust.key())}),
                    _ => json!({"status": "approved", "token": self.trust.device_token(&r.id)}),
                })
            }
        }
    }

    /// A station's request that is waiting here and shows this code: its id,
    /// the station's name, and when it lapses.
    pub(crate) fn trust_station_request(&self, code: &str) -> Option<(String, String, u64)> {
        let now = now_ms();
        self.trust
            .requests
            .lock()
            .unwrap()
            .values()
            .find(|r| {
                r.kind == "station"
                    && r.decision == Decision::Waiting
                    && r.expires_ms > now
                    && r.code == code
            })
            .map(|r| (r.id.clone(), r.name.clone(), r.expires_ms))
    }

    /// The owner's answer. `which` is a request id or the code shown.
    pub fn trust_decide(&self, which: &str, approve: bool, by: &str) -> Result<Value> {
        let found = {
            let mut map = self.trust.requests.lock().unwrap();
            let now = now_ms();
            map.retain(|_, r| r.expires_ms > now);
            let id = map
                .values()
                .find(|r| {
                    r.decision == Decision::Waiting
                        && (r.id == which || r.code.eq_ignore_ascii_case(which.trim()))
                })
                .map(|r| r.id.clone());
            id.and_then(|id| {
                let r = map.get_mut(&id)?;
                r.decision = if approve {
                    Decision::Approved
                } else {
                    Decision::Denied
                };
                Some(r.clone())
            })
        };
        let Some(r) = found else {
            return Err(Error::NotFound(
                "no waiting request with that id or code".into(),
            ));
        };
        if approve && r.kind == "device" {
            let mut devices = self.known_devices();
            devices
                .push(json!({"id": r.id, "name": r.name, "from": r.from, "approved_ms": now_ms()}));
            self.db
                .kv_set("trust.devices", &Value::Array(devices).to_string());
        }
        self.record(
            if approve {
                "trust.approved"
            } else {
                "trust.denied"
            },
            None,
            None,
            None,
            None,
            json!({"kind": r.kind, "name": r.name, "from": r.from, "by": by}),
        );
        self.emit("trust.updated", None, None, json!({}));
        if r.kind == "station" {
            // An add from the Control Deck may be waiting on this answer.
            self.add_decided(&r.code, approve);
        }
        Ok(json!({"kind": r.kind, "name": r.name, "approved": approve}))
    }

    /// Stop a signed-in browser from being trusted here.
    pub fn device_revoke(&self, id: &str, by: &str) -> Result<Value> {
        let mut revoked = self.revoked_devices();
        if !revoked.iter().any(|r| r == id) {
            revoked.push(id.to_string());
            self.db.kv_set("trust.revoked", &json!(revoked).to_string());
        }
        self.record(
            "trust.revoked",
            None,
            None,
            None,
            None,
            json!({"device": id, "by": by}),
        );
        self.emit("trust.updated", None, None, json!({}));
        Ok(json!({"revoked": id}))
    }

    /// Another station tells this one about a revoked browser.
    pub(crate) fn device_revoked_elsewhere(&self, id: &str) {
        let mut revoked = self.revoked_devices();
        if !revoked.iter().any(|r| r == id) && id.len() <= 64 {
            revoked.push(id.to_string());
            self.db.kv_set("trust.revoked", &json!(revoked).to_string());
        }
    }

    /// Ask another station to let this one into its fleet. The owner approves
    /// there; this station then takes that fleet's key in place of its own.
    pub async fn fleet_join(&self, station: &str) -> Result<Value> {
        let peer = self
            .fleet
            .peer(station)
            .ok_or_else(|| invalid(format!("no station called {station} was found")))?;
        let out = self
            .fleet
            .call_open(
                &peer.addr,
                "fleet.join.request",
                json!({"station": self.station}),
            )
            .await
            .map_err(|e| invalid(format!("{station}: {e}")))?;
        let get = |k: &str| out.get(k).and_then(|v| v.as_str()).map(String::from);
        let (Some(id), Some(collect), Some(code)) = (get("id"), get("collect"), get("code")) else {
            return Err(invalid(format!("{station} sent an unexpected reply")));
        };
        let expires_ms = out
            .get("expires_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(now_ms() + REQUEST_MS);
        *self.trust.outgoing.lock().unwrap() = Some(Outgoing {
            station: station.to_string(),
            addr: peer.addr.clone(),
            id,
            collect,
            code: code.clone(),
            expires_ms,
            problem: None,
        });
        self.emit("trust.updated", None, None, json!({}));
        let me = self.clone();
        tokio::spawn(async move { me.fleet_join_wait().await });
        Ok(
            json!({"station": station, "code": code, "expires_ms": expires_ms,
            "next": format!("On {station}, approve the request that shows the code {code}: in its Control Deck, or with `spandeck approve {code}`.")}),
        )
    }

    async fn fleet_join_wait(&self) {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            let Some(o) = self.trust.outgoing.lock().unwrap().clone() else {
                return;
            };
            let ended = |problem: Option<String>| {
                let mut out = self.trust.outgoing.lock().unwrap();
                match problem {
                    Some(p) => {
                        if let Some(o) = out.as_mut() {
                            o.problem = Some(p);
                            o.expires_ms = 0;
                        }
                    }
                    None => *out = None,
                }
                self.emit("trust.updated", None, None, json!({}));
            };
            if o.problem.is_some() {
                return;
            }
            if o.expires_ms < now_ms() {
                return ended(Some("nobody approved it in time".into()));
            }
            let Ok(v) = self
                .fleet
                .call_open(
                    &o.addr,
                    "fleet.join.collect",
                    json!({"id": o.id, "collect": o.collect}),
                )
                .await
            else {
                continue;
            };
            match v.get("status").and_then(|s| s.as_str()) {
                Some("waiting") => continue,
                Some("approved") => {
                    let key = v
                        .get("key")
                        .and_then(|k| k.as_str())
                        .and_then(unhex)
                        .filter(|k| k.len() == 32);
                    let Some(key) = key else {
                        return ended(Some(format!("{} sent an unusable key", o.station)));
                    };
                    if let Err(e) = write_private(&self.trust.file, &hex(&key)) {
                        return ended(Some(format!("could not save the fleet key: {e}")));
                    }
                    *self.trust.key.write().unwrap() = key;
                    self.record(
                        "trust.joined",
                        None,
                        None,
                        None,
                        None,
                        json!({"station": o.station, "fleet": self.trust.fleet_id()}),
                    );
                    self.notify(
                        "trust.joined",
                        None,
                        None,
                        &format!("This station joined {}'s fleet", o.station),
                        "Browsers that were signed in to this station need to sign in again.",
                    );
                    ended(None);
                    self.discover_peers().await;
                    return;
                }
                Some("denied") => return ended(Some(format!("{} said no", o.station))),
                _ => return ended(Some(format!("{} forgot the request; ask again", o.station))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(key: u8) -> TrustState {
        TrustState {
            key: RwLock::new(vec![key; 32]),
            file: PathBuf::new(),
            requests: Mutex::new(BTreeMap::new()),
            outgoing: Mutex::new(None),
        }
    }

    // Guarantee (Trust): only a station holding the fleet key speaks for the
    // owner, and only for the exact call it signed.
    #[test]
    fn a_signature_covers_the_exact_call_and_only_this_fleets_key_makes_one() {
        let (a, other) = (state(1), state(2));
        let sig = a.sign("rpc", b"{\"method\":\"stop.apply\"}");
        assert!(a.verify("rpc", b"{\"method\":\"stop.apply\"}", &sig));
        assert!(!a.verify("rpc", b"{\"method\":\"pane.spawn\"}", &sig));
        assert!(!a.verify("ws", b"{\"method\":\"stop.apply\"}", &sig));
        assert!(!other.verify("rpc", b"{\"method\":\"stop.apply\"}", &sig));
        // An old signature is refused, so a recorded call cannot be replayed later.
        let old = now_ms() - SKEW_MS - 1000;
        let tag = mac(&a.key(), &[b"rpc", old.to_string().as_bytes(), b"n", b"x"])
            .finalize()
            .into_bytes();
        assert!(!a.verify("rpc", b"x", &format!("{old}.n.{}", hex(&tag))));
        assert!(!a.verify("rpc", b"x", "junk"));
    }

    #[test]
    fn a_device_token_is_only_good_in_the_fleet_that_issued_it() {
        let (a, other) = (state(1), state(2));
        let token = a.device_token("q-123");
        assert_eq!(a.device_of(&token).as_deref(), Some("q-123"));
        assert_eq!(other.device_of(&token), None);
        assert_eq!(a.device_of(&token.replace("q-123", "q-124")), None);
        assert_ne!(a.fleet_id(), other.fleet_id());
    }
}
