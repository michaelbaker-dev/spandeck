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

use crate::daemon::{invalid, Authority, Caller, Daemon, Error, Result};
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
    /// The fleet-mates a station says it brings with it. Names only, for the
    /// owner to read: what joins is whoever holds that fleet's key.
    with: Vec<String>,
    collect_hash: [u8; 32],
    decision: Decision,
    expires_ms: u64,
}

/// A request that waits at another station of this fleet. That station told
/// this one, so the owner can see and answer it here. It holds no secret:
/// the answer is passed to the station that was asked, which decides.
#[derive(Clone, PartialEq)]
struct Relayed {
    /// The station that was asked, and that holds the request.
    at: String,
    id: String,
    kind: &'static str,
    name: String,
    from: String,
    code: String,
    with: Vec<String>,
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
    /// Who asked for the join, for the ship's log.
    by: String,
    /// The fleet-mates that come along if it is approved.
    with: Vec<String>,
}

pub struct TrustState {
    key: RwLock<Vec<u8>>,
    file: PathBuf,
    requests: Mutex<BTreeMap<String, Request>>,
    /// Requests waiting at other stations of this fleet, by id.
    relayed: Mutex<BTreeMap<String, Relayed>>,
    outgoing: Mutex<Option<Outgoing>>,
    /// Goes up each time the key is replaced: a station left the fleet, or
    /// the fleet joined another. A connection that was let in with the old
    /// key ends then.
    epoch: tokio::sync::watch::Sender<u64>,
    /// Held while the list of retired keys is read, changed and written.
    retired_lock: Mutex<()>,
}

/// A station that was away when the fleet's key was replaced.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Straggler {
    pub station: String,
    /// Where it was last seen. The new key is sent to this address only.
    pub addr: String,
}

/// A key the fleet stopped using: a station was removed, or the fleet joined
/// another one. Nothing is accepted under it. It is kept only to hand the
/// new key to the stations that still hold it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Retired {
    pub key: String,
    /// The station that was removed; empty when the fleet joined another.
    pub removed: String,
    /// The station whose fleet this one joined; empty for a removal.
    #[serde(default)]
    pub joined: String,
    /// Who asked for it.
    #[serde(default)]
    pub by: String,
    pub since_ms: u64,
    pub stations: Vec<Straggler>,
}

/// How long a station that was away is still handed the new key when it
/// comes back. After that it rejoins on the owner's approval.
pub const RETIRED_KEPT_MS: u64 = 30 * 24 * 3600 * 1000;

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn unhex(s: &str) -> Option<Vec<u8>> {
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
            relayed: Mutex::new(BTreeMap::new()),
            outgoing: Mutex::new(None),
            epoch: tokio::sync::watch::Sender::new(0),
            retired_lock: Mutex::new(()),
        })
    }

    pub(crate) fn key(&self) -> Vec<u8> {
        self.key.read().unwrap().clone()
    }

    /// A public name for the fleet: stations with the same key show the same
    /// id, and the id says nothing about the key.
    pub fn fleet_id(&self) -> String {
        fleet_id_of(&self.key())
    }

    /// Sign one call to another station: `<time>.<nonce>.<mac>`.
    pub fn sign(&self, purpose: &str, body: &[u8]) -> String {
        sign_with(&self.key(), purpose, body)
    }

    /// Stop using this station's key and use `key` from now on: saved first,
    /// so a restart never brings the old one back. Every signature, device
    /// token and open connection that rested on the old key ends here.
    pub(crate) fn replace_key(&self, key: Vec<u8>) -> std::io::Result<()> {
        write_private(&self.file, &hex(&key))?;
        *self.key.write().unwrap() = key;
        // Requests that were waiting were made to the old fleet.
        self.requests.lock().unwrap().clear();
        self.relayed.lock().unwrap().clear();
        *self.outgoing.lock().unwrap() = None;
        self.epoch.send_modify(|n| *n += 1);
        Ok(())
    }

    /// Changes when the key is replaced: see `replace_key`.
    pub fn rekeyed(&self) -> tokio::sync::watch::Receiver<u64> {
        self.epoch.subscribe()
    }

    fn retired_file(&self) -> PathBuf {
        self.file.with_file_name("fleet.retired")
    }

    /// Keys this fleet stopped using, with the stations still to be told.
    pub(crate) fn retired(&self) -> Vec<Retired> {
        std::fs::read(self.retired_file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Read the list, change it and write it back, one caller at a time.
    pub(crate) fn retired_change(
        &self,
        change: impl FnOnce(&mut Vec<Retired>),
    ) -> std::io::Result<()> {
        let _held = self.retired_lock.lock().unwrap();
        let mut list = self.retired();
        change(&mut list);
        self.set_retired(list)
    }

    fn set_retired(&self, mut list: Vec<Retired>) -> std::io::Result<()> {
        let now = now_ms();
        list.retain(|r| !r.stations.is_empty() && now.saturating_sub(r.since_ms) < RETIRED_KEPT_MS);
        if list.is_empty() {
            return match std::fs::remove_file(self.retired_file()) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        write_private(
            &self.retired_file(),
            &serde_json::to_string(&list).unwrap_or_default(),
        )
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

/// The public name of the fleet that holds `key`.
pub(crate) fn fleet_id_of(key: &[u8]) -> String {
    hex(&mac(key, &[b"fleet-id"]).finalize().into_bytes()[..8])
}

/// Sign one call with a given key: `<time>.<nonce>.<mac>`.
pub(crate) fn sign_with(key: &[u8], purpose: &str, body: &[u8]) -> String {
    let ts = now_ms().to_string();
    let nonce = &random_token()[..16];
    let tag = mac(
        key,
        &[purpose.as_bytes(), ts.as_bytes(), nonce.as_bytes(), body],
    )
    .finalize()
    .into_bytes();
    format!("{ts}.{nonce}.{}", hex(&tag))
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

/// Up to twenty names another machine sent, each cleaned like any name.
fn clean_names(names: Option<&Value>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for n in names.and_then(|v| v.as_array()).into_iter().flatten() {
        let Some(n) = n.as_str().map(clean_name) else {
            continue;
        };
        if n != "unnamed" && !out.contains(&n) && out.len() < 20 {
            out.push(n);
        }
    }
    out
}

/// "Station spark2", or "Station spark2 and 2 other stations (a, b)".
fn asker(kind: &str, name: &str, with: &[String]) -> String {
    match (kind, with.len()) {
        ("station", 0) => format!("Station {name}"),
        ("station", 1) => format!("Station {name} and 1 other station ({})", with[0]),
        ("station", n) => format!(
            "Station {name} and {n} other stations ({})",
            with.join(", ")
        ),
        _ => name.to_string(),
    }
}

/// The notice for a request, as (title, body). `at` is the station that was
/// asked, when that is not this one.
fn request_notice(
    kind: &str,
    name: &str,
    with: &[String],
    from: &str,
    code: &str,
    at: Option<&str>,
) -> (String, String) {
    let who = asker(kind, name, with);
    let title = match (kind, with.is_empty()) {
        ("station", true) => format!("{who} asks to join this fleet"),
        ("station", false) => format!("{who} ask to join this fleet"),
        _ => format!("A browser asks to sign in: {who}"),
    };
    let mut body = match at {
        Some(at) => format!("Asked at {at}, from {from}. "),
        None => format!("From {from}. "),
    };
    body.push_str(&format!(
        "Approve only if it shows the code {code}. It will be able to control every station."
    ));
    if !with.is_empty() {
        body.push_str(
            " Every station that holds its fleet's key comes with it, whatever names it gives.",
        );
    }
    (title, body)
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
        if self.revoked_devices().contains(&id) {
            return None;
        }
        self.device_seen(&id);
        Some(id)
    }

    /// A browser just showed a token this fleet's key made. The list must
    /// have it, or it could not be signed out: one this station never heard
    /// of is listed from now on, and one listed from before entries said
    /// which key made them is now known to be signed in.
    fn device_seen(&self, id: &str) {
        let fleet = self.trust.fleet_id();
        let mut all = self.stored_devices();
        match all.iter_mut().find(|d| d["id"].as_str() == Some(id)) {
            Some(d) if d["fleet"].as_str() == Some(fleet.as_str()) => return,
            Some(d) => d["fleet"] = json!(fleet),
            None => all.push(json!({"id": id, "name": "a browser", "from": "",
                "at": self.station, "approved_ms": 0, "fleet": fleet})),
        }
        self.db
            .kv_set("trust.devices", &Value::Array(all).to_string());
        self.emit("trust.updated", None, None, json!({}));
        self.trust_sync_soon();
    }

    /// A new token for the browser that made this call, when it is one the
    /// owner signed in on another device. Given once, in the reply to the
    /// call that replaced the fleet's key, so the owner is not signed out of
    /// the Control Deck they are using.
    pub(crate) fn device_token_again(&self, who: &str) -> Option<(String, String)> {
        let id = who.strip_prefix("you (device ")?.strip_suffix(')')?;
        (!self.revoked_devices().iter().any(|r| r == id))
            .then(|| (id.to_string(), self.trust.device_token(id)))
    }

    /// Every browser signed in from another device was signed in under the
    /// old key. Their tokens no longer work, so they are not listed. The
    /// one kept is the browser that was given a token made with the new key.
    pub(crate) fn devices_forget(&self, keep: Option<&str>) {
        let fleet = self.trust.fleet_id();
        let kept: Vec<Value> = self
            .stored_devices()
            .into_iter()
            .filter(|d| keep.is_some() && d["id"].as_str() == keep)
            .map(|mut d| {
                d["fleet"] = json!(fleet);
                d
            })
            .collect();
        self.db
            .kv_set("trust.devices", &Value::Array(kept).to_string());
    }

    /// This station's record of the browser that made a call, if it is one
    /// signed in from another device.
    pub(crate) fn device_entry(&self, who: &str) -> Option<Value> {
        let (id, _) = self.device_token_again(who)?;
        self.known_devices()
            .into_iter()
            .find(|d| d["id"].as_str() == Some(id.as_str()))
    }

    pub(crate) fn devices_keep(&self, mut entry: Value) {
        entry["fleet"] = json!(self.trust.fleet_id());
        let mut devices = self.stored_devices();
        devices.retain(|d| d["id"] != entry["id"]);
        devices.push(entry);
        self.db
            .kv_set("trust.devices", &Value::Array(devices).to_string());
    }

    /// The browsers signed out in this fleet. The list belongs to the key
    /// that made their tokens, named beside it: once this station holds
    /// another key, those tokens are refused for that reason alone, and the
    /// list is empty. While the key is in use nothing is ever taken off it.
    pub(crate) fn revoked_devices(&self) -> Vec<String> {
        if self
            .db
            .kv_get("trust.revoked.fleet")
            .is_some_and(|f| f != self.trust.fleet_id())
        {
            return vec![];
        }
        self.db
            .kv_get("trust.revoked")
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    /// Add to the browsers signed out in this fleet. Returns whether any
    /// was new here.
    fn revoked_add<'a>(&self, ids: impl IntoIterator<Item = &'a str>) -> bool {
        let mut revoked = self.revoked_devices();
        let before = revoked.len();
        for id in ids {
            let plain = !id.is_empty() && id.len() <= 64 && !id.chars().any(char::is_control);
            if plain && !revoked.iter().any(|r| r == id) {
                revoked.push(id.to_string());
            }
        }
        if revoked.len() == before {
            return false;
        }
        self.db.kv_set("trust.revoked", &json!(revoked).to_string());
        self.db
            .kv_set("trust.revoked.fleet", &self.trust.fleet_id());
        true
    }

    /// Every entry this station has kept, whatever key it was made under.
    fn stored_devices(&self) -> Vec<Value> {
        self.db
            .kv_get("trust.devices")
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    /// The browsers signed in to this fleet: approved under the key this
    /// station holds, and not signed out. An entry from before entries said
    /// which key made them is kept, marked as not known to be signed in,
    /// until its token is seen or the key changes.
    fn known_devices(&self) -> Vec<Value> {
        let fleet = self.trust.fleet_id();
        let revoked = self.revoked_devices();
        let mut devices: Vec<Value> = self
            .stored_devices()
            .into_iter()
            .filter(|d| {
                d["fleet"].as_str().is_none_or(|f| f == fleet)
                    && d["id"]
                        .as_str()
                        .is_some_and(|id| !revoked.iter().any(|r| r == id))
            })
            .collect();
        devices.sort_by_key(|d| {
            (
                d["approved_ms"].as_u64().unwrap_or(0),
                d["id"].as_str().unwrap_or_default().to_string(),
            )
        });
        devices
    }

    /// What every station of the fleet must hold alike: the browsers signed
    /// in and the browsers signed out, under the key named.
    fn trust_doc(&self) -> Value {
        let fleet = self.trust.fleet_id();
        let devices: Vec<Value> = self
            .known_devices()
            .into_iter()
            .filter(|d| d["fleet"].as_str() == Some(fleet.as_str()))
            .collect();
        json!({"fleet": fleet, "devices": devices, "revoked": self.revoked_devices()})
    }

    /// Take in another station's copy. A sign-out always wins: the lists of
    /// signed-out browsers are joined, and a browser on it is listed
    /// nowhere. A browser one station knows and the other does not is
    /// added. Text from another station: cleaned and capped, only shown.
    /// Returns whether anything here changed.
    fn trust_merge(&self, doc: &Value) -> bool {
        let fleet = self.trust.fleet_id();
        if doc["fleet"].as_str() != Some(fleet.as_str()) {
            return false;
        }
        let mut changed = self.revoked_add(
            doc["revoked"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str()),
        );
        let revoked = self.revoked_devices();
        let mut all = self.stored_devices();
        let text = |v: &Value, max: usize| -> String {
            v.as_str()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control())
                .take(max)
                .collect()
        };
        for d in doc["devices"].as_array().into_iter().flatten().take(500) {
            let id = text(&d["id"], 64);
            if id.is_empty() || revoked.contains(&id) {
                continue;
            }
            let theirs = json!({
                "id": id, "name": clean_name(&text(&d["name"], 60)), "from": text(&d["from"], 64),
                "at": clean_name(&text(&d["at"], 60)),
                "approved_ms": d["approved_ms"].as_u64().unwrap_or(0), "fleet": fleet,
            });
            match all
                .iter_mut()
                .find(|m| m["id"].as_str() == Some(id.as_str()))
            {
                // The one that says when it was approved is the fuller one.
                Some(mine) => {
                    let known = mine["fleet"].as_str() == Some(fleet.as_str());
                    if !known
                        || mine["approved_ms"].as_u64().unwrap_or(0)
                            < theirs["approved_ms"].as_u64().unwrap_or(0)
                    {
                        *mine = theirs;
                        changed = true;
                    }
                }
                None => {
                    all.push(theirs);
                    changed = true;
                }
            }
        }
        // What is signed out, or was made under a key that is gone, goes.
        let before = all.len();
        all.retain(|d| {
            d["fleet"].as_str().is_none_or(|f| f == fleet)
                && d["id"]
                    .as_str()
                    .is_some_and(|id| !revoked.iter().any(|r| r == id))
        });
        changed |= all.len() != before;
        if changed {
            self.db
                .kv_set("trust.devices", &Value::Array(all).to_string());
            self.emit("trust.updated", None, None, json!({}));
        }
        changed
    }

    /// Another station of this fleet sends its copy and gets this one's.
    pub(crate) fn trust_synced(&self, caller: &Caller, params: &Value) -> Result<Value> {
        if caller.authority != Authority::Peer {
            return Err(Error::Denied(
                "trust.sync is for another station of this fleet".into(),
            ));
        }
        self.trust_merge(params);
        Ok(self.trust_doc())
    }

    /// Make the lists of browsers the same on every station of the fleet
    /// that answers: each is sent this station's copy and replies with its
    /// own. Run whenever stations are looked for, so one that was off when
    /// a browser was signed in or out has it within a look of coming back,
    /// and at once after a change here. A station too old to know the call
    /// is skipped.
    pub(crate) async fn trust_sync(&self) {
        let mates: Vec<String> = self
            .fleet
            .peers()
            .into_iter()
            .filter(|p| p.state == "online" && p.info["trust_relay"] == json!(true))
            .map(|p| p.addr)
            .collect();
        if mates.is_empty() {
            return;
        }
        let doc = self.trust_doc();
        let calls = mates
            .iter()
            .map(|addr| self.fleet.call_addr(addr, "trust.sync", doc.clone()));
        for theirs in crate::fleet::futures_join_all(calls)
            .await
            .into_iter()
            .flatten()
        {
            self.trust_merge(&theirs);
        }
    }

    fn trust_sync_soon(&self) {
        // Not from a test of this file alone, which has no runtime.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let d = self.clone();
            rt.spawn(async move { d.trust_sync().await });
        }
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
                .map(|r| json!({"id": r.id, "kind": r.kind, "name": r.name, "with": r.with, "from": r.from, "code": r.code, "expires_ms": r.expires_ms, "at": self.station}))
                .collect()
        };
        // And those waiting at the other stations of the fleet.
        let requests: Vec<Value> = {
            let mut map = self.trust.relayed.lock().unwrap();
            map.retain(|_, r| r.expires_ms > now);
            requests
                .into_iter()
                .chain(map.values().map(|r| json!({"id": r.id, "kind": r.kind, "name": r.name, "with": r.with, "from": r.from, "code": r.code, "expires_ms": r.expires_ms, "at": r.at})))
                .collect()
        };
        let outgoing = self.trust.outgoing.lock().unwrap().clone().map(|o| {
            json!({"station": o.station, "code": o.code, "expires_ms": o.expires_ms, "problem": o.problem, "with": o.with})
        });
        // One list for the fleet: see `trust_sync`.
        let fleet = self.trust.fleet_id();
        let devices: Vec<Value> = self
            .known_devices()
            .into_iter()
            .map(|mut d| {
                if d["fleet"].as_str() != Some(fleet.as_str()) {
                    d["unconfirmed"] = json!(true);
                }
                d
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
    /// until the owner approves it, here or at another station of this
    /// fleet. Returns what the asker needs to collect the answer. `with` is
    /// the fleet-mates a station says come with it: text from another
    /// machine, cleaned and shown, never acted on.
    pub fn trust_request(
        &self,
        kind: &str,
        name: &str,
        with: Option<&Value>,
        from: &str,
    ) -> Result<Value> {
        let kind: &'static str = match kind {
            "station" => "station",
            "device" => "device",
            _ => return Err(invalid("kind is station or device")),
        };
        let now = now_ms();
        let name = clean_name(name);
        let mut with = match kind {
            "station" => clean_names(with),
            _ => vec![],
        };
        with.retain(|n| *n != name);
        let collect = random_token();
        let req = Request {
            id: crate::paths::new_id("q"),
            kind,
            name: name.clone(),
            from: from.to_string(),
            code: short_code(),
            with: with.clone(),
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
        let (title, body) = request_notice(kind, &name, &with, from, &req.code, None);
        self.notify("trust.request", None, None, &title, &body);
        self.record(
            "trust.request",
            None,
            None,
            None,
            None,
            json!({"kind": kind, "name": name, "from": from, "with": with}),
        );
        self.emit("trust.updated", None, None, json!({}));
        self.trust_relay_soon();
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
                    // With the key go the browsers signed out in this fleet:
                    // their tokens were made with this key, and a station that
                    // arrives must refuse them too.
                    "station" => json!({"status": "approved", "key": hex(&self.trust.key()),
                        "revoked": self.revoked_devices()}),
                    _ => json!({"status": "approved", "token": self.trust.device_token(&r.id)}),
                })
            }
        }
    }

    /// A station's request that is waiting here and shows this code: its id,
    /// the station's name, the fleet-mates it says it brings, and when it
    /// lapses.
    pub(crate) fn trust_station_request(
        &self,
        code: &str,
    ) -> Option<(String, String, Vec<String>, u64)> {
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
            .map(|r| (r.id.clone(), r.name.clone(), r.with.clone(), r.expires_ms))
    }

    /// The requests waiting at this station, as its fleet-mates are told of
    /// them: who asks and the code to match. Never the value the asker
    /// collects with, and never the key.
    fn trust_waiting(&self) -> Vec<Value> {
        let now = now_ms();
        self.trust
            .requests
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.decision == Decision::Waiting && r.expires_ms > now)
            .map(|r| json!({"id": r.id, "kind": r.kind, "name": r.name, "with": r.with, "from": r.from, "code": r.code, "expires_ms": r.expires_ms}))
            .collect()
    }

    /// Tell the other stations of the fleet which requests wait here, so the
    /// owner can answer at whichever one they are using. The whole list is
    /// sent each time: a request that was answered is no longer in it. A
    /// station that is away, or too old to know the call, is skipped.
    pub(crate) async fn trust_relay_send(&self, even_if_none: bool) {
        let requests = self.trust_waiting();
        if requests.is_empty() && !even_if_none {
            return;
        }
        let params = json!({"requests": requests});
        let mates: Vec<String> = self
            .fleet
            .peers()
            .into_iter()
            .filter(|p| p.state == "online" && p.info["trust_relay"] == json!(true))
            .map(|p| p.addr)
            .collect();
        let calls = mates
            .iter()
            .map(|addr| self.fleet.call_addr(addr, "trust.relay", params.clone()));
        crate::fleet::futures_join_all(calls).await;
    }

    fn trust_relay_soon(&self) {
        let d = self.clone();
        tokio::spawn(async move { d.trust_relay_send(true).await });
    }

    /// Another station of this fleet says which requests wait there. They
    /// are listed here beside this station's own and raise the same notice.
    /// Everything in the call is text from another machine: cleaned, capped,
    /// and only ever shown.
    pub(crate) fn trust_relayed(&self, caller: &Caller, params: &Value) -> Result<Value> {
        if caller.authority != Authority::Peer {
            return Err(Error::Denied(
                "trust.relay is for another station of this fleet".into(),
            ));
        }
        // The name it is known by here, so the answer can be sent to it.
        let at = match self.fleet.peer(&caller.who) {
            Some(p) => p.station,
            None => clean_name(&caller.who),
        };
        let now = now_ms();
        let plain =
            |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        let mut theirs: Vec<Relayed> = vec![];
        for r in params["requests"].as_array().into_iter().flatten() {
            let text = |k: &str, max: usize| -> String {
                r[k].as_str()
                    .unwrap_or_default()
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(max)
                    .collect()
            };
            let kind: &'static str = match r["kind"].as_str() {
                Some("station") => "station",
                Some("device") => "device",
                _ => continue,
            };
            let (id, code) = (text("id", 64), text("code", 12));
            if !plain(&id) || !plain(&code) || theirs.len() >= MAX_PENDING {
                continue;
            }
            theirs.push(Relayed {
                at: at.clone(),
                id,
                kind,
                name: clean_name(&text("name", 60)),
                from: text("from", 64),
                code,
                with: clean_names(r.get("with")),
                // Never kept longer than a request made here would be.
                expires_ms: r["expires_ms"].as_u64().unwrap_or(0).min(now + REQUEST_MS),
            });
        }
        theirs.retain(|r| r.expires_ms > now);
        let mut fresh = vec![];
        let changed = {
            let mut map = self.trust.relayed.lock().unwrap();
            map.retain(|_, r| r.expires_ms > now);
            let before: Vec<Relayed> = map.values().filter(|r| r.at == at).cloned().collect();
            map.retain(|_, r| r.at != at);
            for r in theirs {
                // An id another station already uses is that station's.
                if map.contains_key(&r.id) {
                    continue;
                }
                if !before.iter().any(|b| b.id == r.id) {
                    fresh.push(r.clone());
                }
                map.insert(r.id.clone(), r);
            }
            let after: Vec<Relayed> = map.values().filter(|r| r.at == at).cloned().collect();
            before != after
        };
        for r in &fresh {
            let (title, body) =
                request_notice(r.kind, &r.name, &r.with, &r.from, &r.code, Some(&r.at));
            self.notify("trust.request", None, None, &title, &body);
        }
        if changed {
            self.emit("trust.updated", None, None, json!({}));
        }
        Ok(json!({"ok": true}))
    }

    /// The owner's answer, given at this station. A request that waits here
    /// is decided here. One that waits at another station of the fleet is
    /// passed to that station as the owner's request (`rpc.forward`), and it
    /// decides: the first answer to reach it stands. `here_only` is set on a
    /// request that was passed on, so it is never passed on twice.
    pub async fn trust_answer(
        &self,
        which: &str,
        approve: bool,
        by: &str,
        here_only: bool,
    ) -> Result<Value> {
        match self.trust_decide(which, approve, by) {
            Err(Error::NotFound(_)) if !here_only => {}
            answered => return answered,
        }
        let now = now_ms();
        let found = self
            .trust
            .relayed
            .lock()
            .unwrap()
            .values()
            .find(|r| {
                r.expires_ms > now && (r.id == which || r.code.eq_ignore_ascii_case(which.trim()))
            })
            .cloned();
        let Some(r) = found else {
            return Err(Error::NotFound(
                "no waiting request with that id or code".into(),
            ));
        };
        let method = if approve {
            "trust.approve"
        } else {
            "trust.deny"
        };
        let sent = self
            .fleet
            .call(
                &r.at,
                "rpc.forward",
                json!({"method": method, "params": {"request": r.id, "here": true}}),
            )
            .await;
        let forget = || {
            self.trust.relayed.lock().unwrap().remove(&r.id);
            self.emit("trust.updated", None, None, json!({}));
        };
        match sent {
            Ok(mut v) => {
                forget();
                self.record(
                    "trust.passed_on",
                    None,
                    None,
                    None,
                    Some(by),
                    json!({"kind": r.kind, "name": r.name, "from": r.from, "at": r.at, "approved": approve}),
                );
                v["at"] = json!(r.at);
                Ok(v)
            }
            Err(e) if e.contains("no waiting request") => {
                forget();
                Err(Error::NotFound(format!(
                    "{} no longer has that request: it was answered already, or it lapsed",
                    r.at
                )))
            }
            Err(e) => Err(invalid(format!("{}: {e}", r.at))),
        }
    }

    /// The owner's answer to a request that waits at this station. `which`
    /// is a request id or the code shown.
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
            self.devices_keep(json!({"id": r.id, "name": r.name, "from": r.from,
                "at": self.station, "approved_ms": now_ms()}));
            self.trust_sync_soon();
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
            json!({"kind": r.kind, "name": r.name, "from": r.from, "with": r.with, "by": by}),
        );
        self.emit("trust.updated", None, None, json!({}));
        // The other stations of the fleet stop listing it.
        self.trust_relay_soon();
        if r.kind == "station" {
            // An add from the Control Deck may be waiting on this answer.
            self.add_decided(&r.code, approve);
        }
        Ok(json!({"kind": r.kind, "name": r.name, "with": r.with, "approved": approve}))
    }

    /// Sign a browser out of the fleet. It is refused here at once, and by
    /// every station that answers as soon as it is told; one that is off
    /// is told when it is next seen (`trust_sync`).
    pub async fn device_revoke(&self, id: &str, by: &str) -> Result<Value> {
        self.revoked_add([id]);
        self.record(
            "trust.revoked",
            None,
            None,
            None,
            None,
            json!({"device": id, "by": by}),
        );
        self.emit("trust.updated", None, None, json!({}));
        self.trust_sync().await;
        // A station from before the lists were shared takes one at a time,
        // and only while it is on.
        for peer in self.fleet.peers() {
            if peer.state == "online" && peer.info["trust_relay"] != json!(true) {
                let _ = self
                    .fleet
                    .call(&peer.station, "trust.revoked", json!({"device": id}))
                    .await;
            }
        }
        Ok(json!({"revoked": id}))
    }

    /// The browsers signed out in a fleet, as sent with its key (at a join,
    /// and with `fleet.rekey`): each is refused here from now on. Ids from
    /// another station: nothing else is done with them.
    pub(crate) fn devices_revoked_with_key(&self, list: Option<&Value>) {
        self.revoked_add(
            list.and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str()),
        );
    }

    /// Another station tells this one about a revoked browser.
    pub(crate) fn device_revoked_elsewhere(&self, id: &str) {
        if self.revoked_add([id]) {
            self.emit("trust.updated", None, None, json!({}));
        }
    }

    /// Ask another station to let this one into its fleet. The owner approves
    /// at any station of that fleet; this station then takes that fleet's key
    /// in place of its own, and hands it to the stations of the fleet it was
    /// in, so they come with it: a join is a merge, never a split.
    pub async fn fleet_join(&self, station: &str, by: &str) -> Result<Value> {
        let peer = self
            .fleet
            .peer(station)
            .ok_or_else(|| invalid(format!("no station called {station} was found")))?;
        let with: Vec<String> = self
            .fleet_members()
            .into_iter()
            .map(|p| p.station)
            .filter(|s| s != station)
            .collect();
        let out = self
            .fleet
            .call_open(
                &peer.addr,
                "fleet.join.request",
                json!({"station": self.station, "with": with}),
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
            by: by.to_string(),
            with: with.clone(),
        });
        self.record(
            "trust.join_asked",
            None,
            None,
            None,
            Some(by),
            json!({"station": station, "with": with}),
        );
        self.emit("trust.updated", None, None, json!({}));
        let me = self.clone();
        tokio::spawn(async move { me.fleet_join_wait().await });
        let stations = with.len() + 1;
        let what = match with.len() {
            0 => format!("This station then joins {station}'s fleet. Browsers signed in to this station from other devices sign in again."),
            n => format!(
                "This station and the {} of its fleet ({}) then move into one fleet with {station}. Browsers signed in to those {stations} stations from other devices sign in again.",
                if n == 1 { "other station".to_string() } else { format!("{n} other stations") },
                with.join(", ")
            ),
        };
        Ok(
            json!({"station": station, "code": code, "expires_ms": expires_ms, "with": with,
            "next": format!("At {station}, or at any station of its fleet, approve the request that shows the code {code}: in the Control Deck, or with `spandeck approve {code}`. {what}")}),
        )
    }

    async fn fleet_join_wait(&self) {
        let mut asked = 0u32;
        loop {
            // Often at first, when the owner is likely at the other screen.
            asked += 1;
            let wait = if asked <= 40 { 400 } else { 1500 };
            tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
            let Some(o) = self.trust.outgoing.lock().unwrap().clone() else {
                return;
            };
            let ended = |problem: Option<String>| {
                {
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
                    // The key is taken, and the stations of the fleet this
                    // one was in are owed it: see `fleet_joined`.
                    let mates = match self.fleet_joined(key, &o.station, &o.by) {
                        Ok(mates) => mates,
                        Err(e) => return ended(Some(e.to_string())),
                    };
                    // Browsers signed out in that fleet are signed out here,
                    // before the mates are handed the key and the same list.
                    self.devices_revoked_with_key(v.get("revoked"));
                    self.record(
                        "trust.joined",
                        None,
                        None,
                        None,
                        Some(&o.by),
                        json!({"station": o.station, "fleet": self.trust.fleet_id(), "with": mates}),
                    );
                    let stations = mates.len() + 1;
                    let body = if mates.is_empty() {
                        "Browsers that were signed in to this station from other devices need to sign in again.".to_string()
                    } else {
                        format!(
                            "The stations of the fleet it was in come with it ({}): each is handed the new key now if it is on, or when it is next seen. Browsers that were signed in to those {stations} stations from other devices need to sign in again.",
                            mates.join(", ")
                        )
                    };
                    self.notify(
                        "trust.joined",
                        None,
                        None,
                        &format!("This station joined {}'s fleet", o.station),
                        &body,
                    );
                    ended(None);
                    self.emit("fleet.updated", None, None, json!({}));
                    // Looking again is what hands the mates the key.
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
            relayed: Mutex::new(BTreeMap::new()),
            outgoing: Mutex::new(None),
            epoch: tokio::sync::watch::Sender::new(0),
            retired_lock: Mutex::new(()),
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
