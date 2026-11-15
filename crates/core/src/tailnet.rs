//! Tailscale key expiry: a machine whose key lapses drops off the tailnet,
//! and spandeck with it. This reads what the local Tailscale client already
//! says (the status output discovery fetches), shows the days left, and
//! warns before it happens. It renews nothing and changes nothing.
//!
//! Who may see it: the owner and the stations of the fleet (`perf.get`,
//! `tailnet.view`, `tailnet.report`). It is a list of the owner's devices,
//! so none of it is in `station.info` or any call an agent has.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::Daemon;
use crate::fleet::Peer;
use crate::paths::now_ms;

const DAY_MS: u64 = 24 * 3_600_000;
/// A notice is raised when a key has this many days left or fewer, and
/// when it has expired (0).
pub const LEVELS: [u32; 5] = [14, 7, 3, 1, 0];
const MAX_DEVICES: usize = 64;
/// A device counts as one the owner uses the deck from for this long.
const DECK_DEVICE_MS: u64 = 30 * DAY_MS;
const MAX_DECK_DEVICES: usize = 32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    /// The name Tailscale's admin console lists it under.
    pub name: String,
    pub os: String,
    pub ips: Vec<String>,
    /// None: the key does not expire.
    pub expires_ms: Option<u64>,
    pub expired: bool,
    pub online: bool,
    pub me: bool,
    /// "direct" or "relayed", when this station is talking to it.
    pub path: Option<String>,
    pub relay: Option<String>,
}

/// What the local Tailscale client said, at one look.
#[derive(Debug, Clone, PartialEq)]
pub struct Tailnet {
    pub checked_ms: u64,
    /// Tailscale's own word ("Running", "NeedsLogin", "Stopped"), or
    /// "not_running" when the client did not answer.
    pub state: String,
    /// This device first, then the owner's others: soonest expiry first.
    pub devices: Vec<Device>,
}

/// What a station says of its own key to the stations of its fleet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub expires_ms: Option<u64>,
    #[serde(default)]
    pub expired: bool,
    /// When it was heard, by this station's clock.
    #[serde(default)]
    pub at_ms: u64,
    /// Devices that used a Control Deck there: Tailscale address, and when.
    #[serde(default)]
    pub decks: BTreeMap<String, u64>,
}

#[derive(Default)]
pub struct TailnetState {
    now: Mutex<Option<Tailnet>>,
    /// When each station was last asked for its own key.
    asked: Mutex<BTreeMap<String, u64>>,
    /// When each device's deck use was last written down (not every time).
    noted: Mutex<BTreeMap<String, u64>>,
}

impl TailnetState {
    pub fn get(&self) -> Option<Tailnet> {
        self.now.lock().unwrap().clone()
    }
}

/// Text from somewhere else: no control characters, and bounded.
fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Days since 1970-01-01 of a date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "2027-05-13T18:35:55.123Z" or with "+02:00", as milliseconds since 1970.
/// Go's zero time (year 1), which Tailscale writes for "no expiry", and
/// anything else before 1971 is None.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |from: usize, to: usize| s.get(from..to)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let rest = s.get(19..)?;
    let zone = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset_secs = match zone {
        "Z" | "z" => 0,
        z if z.len() == 6 && (z.starts_with('+') || z.starts_with('-')) => {
            let secs =
                z.get(1..3)?.parse::<i64>().ok()? * 3600 + z.get(4..6)?.parse::<i64>().ok()? * 60;
            if z.starts_with('-') {
                -secs
            } else {
                secs
            }
        }
        _ => return None,
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset_secs;
    (secs > 365 * 86_400).then(|| secs as u64 * 1000)
}

/// A time as Tailscale writes one: "2026-11-21T00:00:00Z".
pub fn rfc3339(ms: u64) -> String {
    let (y, m, d) = civil(ms);
    let s = ms / 1000 % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// "21 Nov 2026", in UTC: the same words on every station, whatever its
/// time zone, so that one notice raised by several reads as one.
pub fn utc_date(ms: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, m, d) = civil(ms);
    format!("{d} {} {y}", MONTHS[(m - 1) as usize])
}

/// The year, month and day (UTC) of a time.
fn civil(ms: u64) -> (i64, i64, i64) {
    let z = (ms / DAY_MS) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

fn device(v: &Value, me: bool) -> Device {
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default();
    let dns = text("DNSName").split('.').next().unwrap_or_default();
    let name = if dns.is_empty() {
        text("HostName")
    } else {
        dns
    };
    let expires_ms = v
        .get("KeyExpiry")
        .and_then(|x| x.as_str())
        .and_then(parse_rfc3339);
    let active = v.get("Active").and_then(|x| x.as_bool()).unwrap_or(false);
    let direct = !text("CurAddr").is_empty();
    Device {
        id: clean(text("ID"), 64),
        name: clean(name, 64),
        os: clean(text("OS"), 16),
        ips: v
            .get("TailscaleIPs")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|i| i.as_str())
                    .filter(|i| i.parse::<std::net::IpAddr>().is_ok())
                    .take(4)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        expires_ms,
        expired: v.get("Expired").and_then(|x| x.as_bool()).unwrap_or(false),
        online: me || v.get("Online").and_then(|x| x.as_bool()).unwrap_or(false),
        me,
        path: (!me && (direct || active))
            .then(|| if direct { "direct" } else { "relayed" }.to_string()),
        relay: (!me && !direct && active && !text("Relay").is_empty())
            .then(|| clean(text("Relay"), 16)),
    }
}

/// Read `tailscale status --json`: this device, and the others under the
/// same login. Tagged and shared devices are not the owner's own and are
/// left out (a tagged device's key does not expire anyway).
pub fn parse_status(status: Option<&Value>, now: u64) -> Tailnet {
    let Some(status) = status else {
        return Tailnet {
            checked_ms: now,
            state: "not_running".into(),
            devices: vec![],
        };
    };
    let state = clean(
        status
            .get("BackendState")
            .and_then(|s| s.as_str())
            .unwrap_or("Running"),
        24,
    );
    let mut devices = vec![];
    let me = status.get("Self");
    let user = me.and_then(|m| m.get("UserID")).and_then(|u| u.as_i64());
    let mut others: Vec<Device> = status
        .get("Peer")
        .and_then(|p| p.as_object())
        .into_iter()
        .flat_map(|m| m.values())
        .filter(|p| {
            let tagged = p
                .get("Tags")
                .and_then(|t| t.as_array())
                .is_some_and(|t| !t.is_empty());
            user.is_some() && p.get("UserID").and_then(|u| u.as_i64()) == user && !tagged
        })
        .map(|p| device(p, false))
        .collect();
    others.sort_by(|a, b| {
        (a.expires_ms.unwrap_or(u64::MAX), &a.name)
            .cmp(&(b.expires_ms.unwrap_or(u64::MAX), &b.name))
    });
    if let Some(me) = me {
        devices.push(device(me, true));
    }
    devices.extend(others.into_iter().take(MAX_DEVICES - 1));
    Tailnet {
        checked_ms: now,
        state,
        devices,
    }
}

/// Whole days left, rounded down; negative once it has passed.
pub fn days_left(expires_ms: u64, now: u64) -> i64 {
    (expires_ms as i64 - now as i64).div_euclid(DAY_MS as i64)
}

/// Which notice a key is due: the smallest of 14, 7, 3, 1 it is within,
/// 0 when it has expired, none while it has more than 14 days.
pub fn level(expires_ms: u64, expired: bool, now: u64) -> Option<u32> {
    if expired || expires_ms <= now {
        return Some(0);
    }
    let left = expires_ms - now;
    LEVELS
        .iter()
        .rev()
        .skip(1)
        .find(|days| left <= **days as u64 * DAY_MS)
        .copied()
}

/// Every station raises the same notice in its own inbox, and the Control
/// Deck shows them as one. Only one sends it to the phone: the station
/// whose name sorts first among those that are on and can tell. Each
/// decides that alone; a fleet split in two may push twice.
pub fn pushes(me: &str, others_on: &[String]) -> bool {
    others_on.iter().all(|o| me <= o.as_str())
}

/// What a device was last warned of: which expiry, and at which level.
fn due(mark: Option<&str>, expires_ms: u64, level: Option<u32>) -> Option<u32> {
    let level = level?;
    let said = mark.and_then(|m| {
        let (exp, lvl) = m.split_once(':')?;
        (exp.parse::<u64>().ok()? == expires_ms).then(|| lvl.parse::<u32>().ok())?
    });
    said.is_none_or(|s| level < s).then_some(level)
}

/// The words of a notice. They hold no "this station" and no local time,
/// so every station writes the same ones.
pub fn notice(name: &str, station: bool, expires_ms: u64, level: u32) -> (String, String) {
    let on = utc_date(expires_ms);
    let agents = if station {
        " spandeck cannot reach it while it is off; its agents keep running, unseen."
    } else {
        " The Control Deck cannot be opened from it while it is off."
    };
    if level == 0 {
        return (
            format!("Tailscale key of {name} has expired"),
            format!("{name} dropped off your Tailscale network on {on} (UTC) and cannot be reached over it until that is put right.{agents} What to do: {}", recovery(name)),
        );
    }
    (
        format!(
            "Tailscale key of {name} expires within {level} day{}",
            if level == 1 { "" } else { "s" }
        ),
        format!("{name} drops off your Tailscale network on {on} (UTC) unless its key is renewed.{agents} Before then, either: in Tailscale's admin console (login.tailscale.com/admin/machines), open {name}'s menu and choose \"Disable key expiry\"; or at the machine, open Tailscale and sign in again, which lasts until the next expiry."),
    )
}

/// What to do about a machine whose key has expired, from any device.
pub fn recovery(name: &str) -> String {
    format!("1. On any device, open Tailscale's admin console (login.tailscale.com/admin/machines), find {name}, and in its menu choose \"Temporarily extend key\": it is back for 30 minutes. 2. Within that time, in the same menu choose \"Disable key expiry\", or go to the machine and sign in to Tailscale again. 3. Or, at the machine itself at any time: open Tailscale and sign in (on Linux: sudo tailscale up --force-reauth, then open the address it prints). When it is back, spandeck finds it by itself and delivers what was waiting.")
}

impl Daemon {
    /// After a look: what the local client says now.
    pub(crate) fn tailnet_refresh(&self, status: Option<&Value>) {
        *self.tailnet.now.lock().unwrap() = Some(parse_status(status, now_ms()));
    }

    fn tailnet_reports(&self) -> BTreeMap<String, Report> {
        self.db
            .kv_get("tailnet.members")
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    /// Devices that used this station's Control Deck: address, and when.
    fn deck_devices(&self) -> BTreeMap<String, u64> {
        let now = now_ms();
        let mut seen: BTreeMap<String, u64> = self
            .db
            .kv_get("tailnet.decks")
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        seen.retain(|_, at| now.saturating_sub(*at) < DECK_DEVICE_MS);
        seen
    }

    /// A Control Deck was opened from this Tailscale address. Written down
    /// at most once an hour per device.
    pub fn note_deck_device(&self, ip: std::net::IpAddr) {
        if ip.is_loopback() {
            return;
        }
        let (ip, now) = (ip.to_string(), now_ms());
        {
            let mut noted = self.tailnet.noted.lock().unwrap();
            if noted
                .get(&ip)
                .is_some_and(|at| now.saturating_sub(*at) < 3_600_000)
            {
                return;
            }
            if noted.len() >= MAX_DECK_DEVICES {
                noted.clear();
            }
            noted.insert(ip.clone(), now);
        }
        let mut seen = self.deck_devices();
        seen.insert(ip, now);
        while seen.len() > MAX_DECK_DEVICES {
            let Some(oldest) = seen
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            seen.remove(&oldest);
        }
        self.db.kv_set(
            "tailnet.decks",
            &serde_json::to_string(&seen).unwrap_or_default(),
        );
    }

    /// `tailnet.report`: this station's own key, for the stations of its
    /// fleet. A station's signed call, or the owner's.
    pub fn tailnet_report(&self) -> Value {
        let t = self.tailnet.get();
        let me = t.as_ref().and_then(|t| t.devices.iter().find(|d| d.me));
        json!({
            "read": t.is_some(),
            "id": me.map(|d| d.id.clone()).unwrap_or_default(),
            "device": me.map(|d| d.name.clone()).unwrap_or_default(),
            "state": t.as_ref().map(|t| t.state.clone()).unwrap_or_default(),
            "expires_ms": me.and_then(|d| d.expires_ms),
            "expired": me.is_some_and(|d| d.expired),
            "decks": self.deck_devices(),
        })
    }

    /// Ask the stations that answer for theirs: when one is first seen, and
    /// then once a minute. What each says is kept for when it is off.
    pub(crate) async fn tailnet_ask(&self) {
        let now = now_ms();
        let asked: Vec<Peer> = {
            let mut last = self.tailnet.asked.lock().unwrap();
            let due: Vec<Peer> = self
                .fleet
                .peers()
                .into_iter()
                .filter(|p| p.state == "online" && p.info.get("perf").is_some())
                .filter(|p| {
                    last.get(&p.station)
                        .is_none_or(|at| now.saturating_sub(*at) >= 60_000)
                })
                .collect();
            last.retain(|s, _| self.fleet.peer(s).is_some_and(|p| p.state == "online"));
            for p in &due {
                last.insert(p.station.clone(), now);
            }
            due
        };
        if asked.is_empty() {
            return;
        }
        let calls = asked.iter().map(|p| async move {
            let said = self
                .fleet
                .call(&p.station, "tailnet.report", json!({}))
                .await;
            (p.station.clone(), said.ok())
        });
        let answers = crate::fleet::futures_join_all(calls).await;
        let mut reports = self.tailnet_reports();
        let before = serde_json::to_string(&reports).unwrap_or_default();
        for (station, said) in answers {
            let Some(said) = said.filter(|s| s["read"] == json!(true)) else {
                continue;
            };
            // What another station says is data: bounded, and only these.
            let text = |k: &str, max| clean(said[k].as_str().unwrap_or_default(), max);
            let decks = said["decks"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(ip, _)| ip.parse::<std::net::IpAddr>().is_ok())
                .filter_map(|(ip, at)| Some((ip.clone(), at.as_u64()?.min(now))))
                .take(MAX_DECK_DEVICES)
                .collect();
            reports.insert(
                station,
                Report {
                    id: text("id", 64),
                    device: text("device", 64),
                    state: text("state", 24),
                    expires_ms: said["expires_ms"].as_u64(),
                    expired: said["expired"] == json!(true),
                    at_ms: now,
                    decks,
                },
            );
        }
        // Only the fleet's members are remembered.
        let members: Vec<String> = self.fleet.peers().into_iter().map(|p| p.station).collect();
        reports.retain(|s, _| members.contains(s));
        // The time it was heard changes at every asking: it is written
        // down only when what was said changed, or once an hour.
        let stale = |r: &BTreeMap<String, Report>| -> String {
            serde_json::to_string(
                &r.iter()
                    .map(|(k, v)| {
                        let mut v = v.clone();
                        v.at_ms /= 3_600_000;
                        (k, v)
                    })
                    .collect::<BTreeMap<_, _>>(),
            )
            .unwrap_or_default()
        };
        let old: BTreeMap<String, Report> = serde_json::from_str(&before).unwrap_or_default();
        if stale(&reports) != stale(&old) {
            self.db.kv_set(
                "tailnet.members",
                &serde_json::to_string(&reports).unwrap_or_default(),
            );
        }
    }

    /// The Tailscale device a station is, as the local client lists it: by
    /// the id the station gave, else by its address when one device has it.
    pub(crate) fn tailnet_device_of(&self, peer: &Peer, t: Option<&Tailnet>) -> Option<Device> {
        let t = t?;
        let reports = self.tailnet_reports();
        if let Some(r) = reports.get(&peer.station).filter(|r| !r.id.is_empty()) {
            if let Some(d) = t.devices.iter().find(|d| d.id == r.id) {
                return Some(d.clone());
            }
        }
        let host = peer
            .addr
            .rsplit_once(':')
            .map_or(peer.addr.as_str(), |(h, _)| h);
        if host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
        {
            return None;
        }
        let mut found = t.devices.iter().filter(|d| d.ips.iter().any(|i| i == host));
        match (found.next(), found.next()) {
            (Some(d), None) => Some(d.clone()),
            _ => None,
        }
    }

    /// A station's key: from the local Tailscale client when it lists the
    /// device (which it does while the device is off), else what the
    /// station last said of itself, which is remembered.
    fn station_key(&self, peer: Option<&Peer>, t: Option<&Tailnet>) -> Value {
        let key = |d: &Device, source: &str, as_of: u64| json!({"known": true, "id": d.id, "device": d.name, "expires_ms": d.expires_ms, "expired": d.expired, "source": source, "as_of_ms": as_of});
        let Some(peer) = peer else {
            return match t.and_then(|t| t.devices.iter().find(|d| d.me).map(|d| (t, d))) {
                Some((t, d)) => {
                    let mut v = key(d, "tailscale", t.checked_ms);
                    v["state"] = json!(t.state);
                    v
                }
                None => json!({"known": false, "state": t.map(|t| t.state.clone())}),
            };
        };
        if let Some(d) = self.tailnet_device_of(peer, t) {
            return key(&d, "tailscale", t.map_or(0, |t| t.checked_ms));
        }
        match self.tailnet_reports().get(&peer.station) {
            Some(r) => {
                json!({"known": true, "id": r.id, "device": r.device, "expires_ms": r.expires_ms, "expired": r.expired,
                "state": r.state, "source": if peer.state == "online" { "station" } else { "remembered" }, "as_of_ms": r.at_ms})
            }
            None => json!({"known": false}),
        }
    }

    /// The device a connection from this address is, if the local client
    /// lists it: for the deck's "this device's key expires" line.
    pub fn tailnet_device_at(&self, ip: std::net::IpAddr) -> Option<String> {
        let t = self.tailnet.get()?;
        let ip = ip.to_string();
        t.devices
            .iter()
            .find(|d| (ip_is_loopback(&ip) && d.me) || d.ips.contains(&ip))
            .map(|d| d.id.clone())
            .filter(|id| !id.is_empty())
    }

    /// Every device a warning is owed for: the stations of the fleet, and
    /// the owner's devices that used a Control Deck in the last 30 days.
    fn tailnet_watched(&self, t: Option<&Tailnet>) -> Vec<Value> {
        let mut out: Vec<Value> = vec![];
        let label = |station: &str, device: &str| {
            if device.is_empty() || device.eq_ignore_ascii_case(station) {
                station.to_string()
            } else {
                format!("{station} ({device})")
            }
        };
        let mut add = |station: Option<&str>, mut key: Value| {
            if key["known"] != json!(true) {
                return;
            }
            let device = key["device"].as_str().unwrap_or_default().to_string();
            key["name"] = json!(match station {
                Some(s) => label(s, &device),
                None => device,
            });
            key["station"] = json!(station);
            out.push(key);
        };
        add(Some(&self.station), self.station_key(None, t));
        for p in self
            .fleet
            .peers()
            .iter()
            .filter(|p| p.state != "not_linked")
        {
            add(Some(&p.station), self.station_key(Some(p), t));
        }
        let mut decks = self.deck_devices();
        let now = now_ms();
        for r in self.tailnet_reports().values() {
            for (ip, at) in &r.decks {
                if now.saturating_sub(*at) < DECK_DEVICE_MS {
                    let mine = decks.entry(ip.clone()).or_insert(0);
                    *mine = (*mine).max(*at);
                }
            }
        }
        let known: Vec<String> = out
            .iter()
            .filter_map(|k| k["id"].as_str().map(String::from))
            .collect();
        for d in t.into_iter().flat_map(|t| t.devices.iter()) {
            if !known.contains(&d.id) && d.ips.iter().any(|ip| decks.contains_key(ip)) {
                out.push(json!({"known": true, "id": d.id, "device": d.name, "name": d.name, "station": null, "os": d.os,
                    "expires_ms": d.expires_ms, "expired": d.expired, "source": "tailscale", "as_of_ms": t.map_or(0, |t| t.checked_ms), "deck": true}));
            }
        }
        out
    }

    /// Raise the notices that are due. Each station does this alone, with
    /// the same words; what was said is remembered in the database, so a
    /// restart does not say it again.
    pub(crate) fn tailnet_watch(&self) {
        let t = self.tailnet.get();
        let now = now_ms();
        let on: Vec<String> = self
            .fleet
            .peers()
            .into_iter()
            .filter(|p| p.state == "online" && p.info.get("perf").is_some())
            .map(|p| p.station)
            .collect();
        for w in self.tailnet_watched(t.as_ref()) {
            let (Some(expires), Some(name)) = (w["expires_ms"].as_u64(), w["name"].as_str()) else {
                continue;
            };
            let id = match w["id"].as_str().filter(|i| !i.is_empty()) {
                Some(id) => id.to_string(),
                None => format!("station:{}", w["station"].as_str().unwrap_or_default()),
            };
            let mark_key = format!("tailnet.warned:{id}");
            let mark = self.db.kv_get(&mark_key);
            let Some(level) = due(
                mark.as_deref(),
                expires,
                level(expires, w["expired"] == json!(true), now),
            ) else {
                continue;
            };
            self.db.kv_set(&mark_key, &format!("{expires}:{level}"));
            // A device that is not a station and expired long ago is not news.
            if w["station"].is_null() && now.saturating_sub(expires) > 30 * DAY_MS {
                continue;
            }
            let (title, body) = notice(name, !w["station"].is_null(), expires, level);
            self.notify_with("tailnet", &title, &body, pushes(&self.station, &on));
            self.record(
                "tailnet.warned",
                None,
                None,
                None,
                None,
                json!({"device": name, "level": level, "expires_ms": expires}),
            );
        }
    }

    /// `tailnet.view`, and the same inside `perf.get`: this station's key,
    /// each station's, and the owner's other devices that are near expiry.
    /// The owner's and a station's only.
    pub fn tailnet_view(&self) -> Value {
        let t = self.tailnet.get();
        let now = now_ms();
        let watched = self.tailnet_watched(t.as_ref());
        let stations: serde_json::Map<String, Value> = watched
            .iter()
            .filter_map(|w| Some((w["station"].as_str()?.to_string(), w.clone())))
            .collect();
        let station_ids: Vec<&str> = stations.values().filter_map(|s| s["id"].as_str()).collect();
        // Not noise: only what expires within 30 days or did in the last 30.
        let near = |e: u64| e < now + 30 * DAY_MS && now < e + 30 * DAY_MS;
        let devices: Vec<Value> = t
            .iter()
            .flat_map(|t| t.devices.iter())
            .filter(|d| !station_ids.contains(&d.id.as_str()) && d.expires_ms.is_some_and(near))
            .map(|d| json!({"id": d.id, "name": d.name, "os": d.os, "expires_ms": d.expires_ms, "expired": d.expired, "online": d.online,
                "deck": watched.iter().any(|w| w["id"] == json!(d.id))}))
            .collect();
        json!({
            "read": t.is_some(),
            "discovery": self.cfg.discovery,
            "state": t.as_ref().map(|t| t.state.clone()),
            "checked_ms": t.as_ref().map(|t| t.checked_ms),
            "now_ms": now,
            "stations": stations,
            "devices": devices,
        })
    }
}

fn ip_is_loopback(ip: &str) -> bool {
    ip.parse::<std::net::IpAddr>()
        .is_ok_and(|i| i.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOV15: u64 = 1_794_700_800_000; // 15 Nov 2026 00:00 UTC

    /// The owner, 15 Nov 2026: "a Tailscale token expiring and lost access
    /// to the machine, how can we expose this". The status output is read,
    /// the days are counted and a notice is raised at 14, 7, 3, 1 and at
    /// expiry, once each, by every station in the same words, pushed by one.
    #[test]
    fn key_expiry_is_read_counted_and_warned_of_once_per_level() {
        assert_eq!(parse_rfc3339("2026-11-15T00:00:00Z"), Some(NOV15));
        assert_eq!(
            parse_rfc3339("2026-11-15T02:00:00.123456789+02:00"),
            Some(NOV15)
        );
        assert_eq!(parse_rfc3339("2026-11-14T19:00:00-05:00"), Some(NOV15));
        // Go's zero time is "no expiry"; junk is nothing.
        assert_eq!(parse_rfc3339("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339("soon"), None);
        assert_eq!(parse_rfc3339("2026-13-40T00:00:00Z"), None);
        assert_eq!(utc_date(NOV15), "15 Nov 2026");
        assert_eq!(
            parse_rfc3339(&rfc3339(NOV15 + 86_399_000)),
            Some(NOV15 + 86_399_000)
        );
        assert_eq!(
            utc_date(parse_rfc3339("2028-02-29T23:59:59Z").unwrap()),
            "29 Feb 2028"
        );
        assert_eq!(
            utc_date(parse_rfc3339("2027-01-01T00:00:00Z").unwrap()),
            "1 Jan 2027"
        );

        let status = json!({
            "BackendState": "Running",
            "Self": {"ID": "n-me", "HostName": "Spark2", "DNSName": "spark2.tail0.ts.net.", "OS": "linux", "UserID": 7, "TailscaleIPs": ["100.64.0.1"]},
            "Peer": {
                "k1": {"ID": "n-phone", "HostName": "iPhone\u{7}<b>", "DNSName": "", "OS": "iOS", "UserID": 7, "Online": true,
                    "TailscaleIPs": ["100.64.0.9", "not an address"], "KeyExpiry": "2026-11-21T00:00:00Z"},
                "k2": {"ID": "n-old", "DNSName": "old-box.tail0.ts.net.", "OS": "linux", "UserID": 7, "Online": false,
                    "KeyExpiry": "2026-06-21T00:00:00Z", "Expired": true},
                "k3": {"ID": "n-m4", "DNSName": "m4-macbook.tail0.ts.net.", "OS": "macOS", "UserID": 7, "Online": true,
                    "TailscaleIPs": ["100.64.0.3"], "CurAddr": "192.168.1.9:41641", "Active": true, "Relay": "nyc"},
                "k4": {"ID": "n-far", "DNSName": "far.tail0.ts.net.", "OS": "windows", "UserID": 7, "Online": true, "Active": true, "Relay": "fra",
                    "KeyExpiry": "2027-01-02T00:00:00Z"},
                "k5": {"ID": "n-ci", "DNSName": "ci.tail0.ts.net.", "UserID": 7, "Tags": ["tag:ci"]},
                "k6": {"ID": "n-guest", "DNSName": "guest.tail0.ts.net.", "UserID": 8, "KeyExpiry": "2026-11-16T00:00:00Z"},
            }
        });
        let t = parse_status(Some(&status), NOV15);
        assert_eq!(t.state, "Running");
        let names: Vec<&str> = t.devices.iter().map(|d| d.name.as_str()).collect();
        // This device, then soonest expiry first; no tagged or shared device.
        assert_eq!(
            names,
            ["spark2", "old-box", "iPhone<b>", "far", "m4-macbook"]
        );
        assert!(t.devices[0].me && t.devices[0].expires_ms.is_none());
        let phone = &t.devices[2];
        assert_eq!((phone.ips.len(), phone.expired), (1, false));
        assert_eq!(days_left(phone.expires_ms.unwrap(), NOV15), 6);
        assert!(t.devices[1].expired);
        assert_eq!(days_left(t.devices[1].expires_ms.unwrap(), NOV15), -147);
        assert_eq!(t.devices[4].path.as_deref(), Some("direct"));
        assert_eq!(
            (t.devices[3].path.as_deref(), t.devices[3].relay.as_deref()),
            (Some("relayed"), Some("fra"))
        );
        // The client does not answer: that is said, not "never expires".
        assert_eq!(parse_status(None, NOV15).state, "not_running");
        assert_eq!(
            parse_status(Some(&json!({"BackendState": "NeedsLogin"})), 0).state,
            "NeedsLogin"
        );

        // Levels: nothing at 40 and 20 days; 14, 7, 3, 1; expired.
        let d = |days: u64| NOV15 + days * DAY_MS;
        assert_eq!(level(d(40), false, NOV15), None);
        assert_eq!(level(d(20), false, NOV15), None);
        assert_eq!(level(d(14), false, NOV15), Some(14));
        assert_eq!(level(d(6), false, NOV15), Some(7));
        assert_eq!(level(d(3), false, NOV15), Some(3));
        assert_eq!(level(d(1) - 1, false, NOV15), Some(1));
        assert_eq!(level(NOV15 - 1, false, NOV15), Some(0));
        assert_eq!(level(d(5), true, NOV15), Some(0));

        // Time moved through the levels: one notice at each, none repeated
        // at the next look or after a restart (the mark is in the database).
        let exp = d(20);
        let mut mark: Option<String> = None;
        let mut raised = vec![];
        for day in 0..=21 {
            for _look in 0..3 {
                let now = NOV15 + day * DAY_MS;
                if let Some(l) = due(mark.as_deref(), exp, level(exp, false, now)) {
                    raised.push((day, l));
                    mark = Some(format!("{exp}:{l}"));
                }
            }
        }
        assert_eq!(raised, [(6, 14), (13, 7), (17, 3), (19, 1), (20, 0)]);
        // A station that was off until two days before says the level it
        // finds, once. A renewed key (a new date) starts again.
        assert_eq!(due(None, exp, level(exp, false, d(18))), Some(3));
        assert_eq!(
            due(Some(&format!("{exp}:3")), exp, level(exp, false, d(18))),
            None
        );
        assert_eq!(
            due(
                Some(&format!("{exp}:0")),
                d(200),
                level(d(200), false, d(190))
            ),
            Some(14)
        );

        // The same words on every station, so the deck shows one entry.
        let (title, body) = notice("M4-Macbook (m4-macbook)", true, d(6), 7);
        assert_eq!(
            title,
            "Tailscale key of M4-Macbook (m4-macbook) expires within 7 days"
        );
        assert!(
            body.contains("on 21 Nov 2026 (UTC)") && body.contains("Disable key expiry"),
            "{body}"
        );
        let (title, body) = notice("iPhone", false, d(0), 0);
        assert_eq!(title, "Tailscale key of iPhone has expired");
        assert!(
            body.contains("Temporarily extend key") && body.contains("Disable key expiry"),
            "{body}"
        );

        // One push for the fleet: the first by name among those that are on.
        let on = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(pushes("alpha", &on(&["bravo", "charlie"])));
        assert!(!pushes("bravo", &on(&["alpha", "charlie"])));
        assert!(
            pushes("bravo", &on(&["charlie"])),
            "alpha is off: the next one does"
        );
        assert!(pushes("solo", &[]));
    }
}
