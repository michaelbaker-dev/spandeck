//! A release got from another station of the fleet (the owner's decision of
//! 17 Nov 2026: a station that cannot download its update asks the closest
//! station that can to serve it).
//!
//! Every station answers the same three questions from a station of its
//! fleet, over the signed station-to-station calls and nothing else:
//!
//! - `release.offer`: can you supply this release for this platform? It
//!   answers what it has (`kept`: downloaded before, and it still matches
//!   the SHA-256 it was checked against), what it could get (`fetch`: its
//!   release host answers and publishes that file, with the SHA-256 it
//!   publishes), `own` (it runs that platform and that version itself), or
//!   `none`. Nothing is downloaded to answer.
//! - `release.prepare`: have it ready. The archive is downloaded if need
//!   be and checked against its published SHA-256 (supply.rs, the check
//!   `spandeck update` makes); the answer is its size and SHA-256.
//! - `release.chunk`: so many bytes of it from an offset.
//!
//! The station that asks takes the pieces one at a time on connections of
//! their own (`FleetState::call_bulk`), so that a message, a stop or a
//! terminal between the two stations never waits behind a piece, rests
//! between pieces, tries a piece again from where it was, and goes to the
//! next station that offered when one stops answering.
//!
//! No agent and no read-only caller has these calls; a browser has no way
//! to them. What the asking station believes, and why that is no new
//! power, is in SECURITY.md.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};

use crate::daemon::{invalid, Daemon, Error, Result, VERSION};
use crate::manage::release_target;
use crate::supply::{
    archive_ext, older, sha256_hex, size_words, valid_tag, ReleaseHost, Source, Supplied, TARGETS,
};

/// What a station says of itself when it answers these calls (`caps`).
pub const CAP: &str = "release_supply";

/// The size of a piece: begun at, and kept between. A piece that took more
/// than two seconds halves the next one; one that took less than half a
/// second doubles it. So a piece never holds a slow link for long.
const PIECE_START: u64 = 128 * 1024;
const PIECE_MIN: u64 = 16 * 1024;
const PIECE_MAX: u64 = 1024 * 1024;
/// How long one piece may take, and how often it is asked for again from
/// the same place before the next station is tried.
const PIECE_LIMIT: Duration = Duration::from_secs(30);
const PIECE_TRIES: u32 = 3;
/// How long a station has to have the release ready (its own download).
const PREPARE_LIMIT: Duration = Duration::from_secs(330);
/// How long the taking may last in all.
const TAKE_LIMIT: Duration = Duration::from_secs(3000);

/// What a station offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Have {
    /// It holds the archive, checked against its published SHA-256.
    Kept,
    /// Its release host publishes it; it would download and check it.
    Fetch,
    /// Its own running program, of that version and platform.
    Own,
}

#[derive(Debug, Clone)]
pub struct Offer {
    pub station: String,
    pub have: Have,
    pub tag: String,
    /// The published SHA-256 (`kept`, `fetch`). An own copy has none until
    /// it is put together.
    pub sha256: Option<String>,
    /// The latest round trip of a call to that station (the Performance
    /// page's figure), when there is one.
    pub rtt_ms: Option<u64>,
}

/// Why the fleet gave nothing.
#[derive(Debug)]
pub enum NoFleet {
    /// No station of the fleet that answers now can supply releases.
    NoSupplier { online: Vec<String> },
    /// Those asked have nothing for that platform (and version).
    Nothing { asked: Vec<String> },
    /// Two stations give different SHA-256s for the same release.
    Disagree { tag: String, a: String, b: String },
    /// The newest any of them has is what this station runs.
    Current { tag: String },
    /// Each that offered was tried, and failed.
    Failed { tried: Vec<(String, String)> },
}

impl NoFleet {
    pub fn words(&self) -> String {
        match self {
            NoFleet::NoSupplier { online } if online.is_empty() => {
                "no other station of your fleet is answering now".to_string()
            }
            NoFleet::NoSupplier { online } => format!(
                "no station of your fleet can supply releases yet ({} run a spandeck from before stations could: update one of them first)",
                online.join(", ")
            ),
            NoFleet::Nothing { asked } => format!(
                "none of the stations asked ({}) has it or can reach the release host",
                asked.join(", ")
            ),
            NoFleet::Disagree { tag, a, b } => format!(
                "{a} and {b} give different SHA-256s for {tag}, so one of them holds something that is not that release, and nothing was taken from either"
            ),
            NoFleet::Current { tag } => {
                format!("the newest release the fleet can get is {tag}, which this station runs")
            }
            NoFleet::Failed { tried } => tried
                .iter()
                .map(|(s, why)| format!("{s}: {why}"))
                .collect::<Vec<_>>()
                .join("; "),
        }
    }
}

/// Which release to take, and from whom, in order.
///
/// `tag`: the version asked for by name, if one was. Otherwise the newest
/// that a station holds or can fetch; never one older than `floor` (this
/// station's own version), and for an update (`newer_only`) only one that
/// is newer: a station adding a machine wants the release it runs itself
/// as much as a newer one. A station's own copy is taken only when no
/// station holds or can fetch a release (`allow_own`), and then the
/// newest of those.
///
/// The order: one that holds it before one that would have to fetch it;
/// among equals the shortest round trip, a station never timed last.
///
/// Stations that hold or can fetch the same release must give the same
/// SHA-256 for it: when two do not, nothing is taken.
pub fn choose(
    offers: &[Offer],
    tag: Option<&str>,
    allow_own: bool,
    newer_only: bool,
    floor: &str,
) -> std::result::Result<(String, Vec<Offer>), NoFleet> {
    let asked: Vec<String> = offers.iter().map(|o| o.station.clone()).collect();
    let fits = |o: &&Offer| match tag {
        Some(t) => o.tag == t,
        None => !older(&o.tag, floor),
    };
    let newest = |list: &[&Offer]| {
        list.iter()
            .map(|o| o.tag.clone())
            .reduce(|a, b| if older(&a, &b) { b } else { a })
    };
    let published: Vec<&Offer> = offers
        .iter()
        .filter(fits)
        .filter(|o| o.have != Have::Own)
        .collect();
    let own: Vec<&Offer> = offers
        .iter()
        .filter(fits)
        .filter(|o| o.have == Have::Own && allow_own)
        .collect();
    let (pool, chosen) = match (newest(&published), newest(&own)) {
        (Some(t), _) => (published, t),
        (None, Some(t)) => (own, t),
        (None, None) => return Err(NoFleet::Nothing { asked }),
    };
    if tag.is_none() && newer_only && !older(floor, &chosen) {
        return Err(NoFleet::Current { tag: chosen });
    }
    let mut from: Vec<Offer> = pool
        .into_iter()
        .filter(|o| o.tag == chosen)
        .cloned()
        .collect();
    // The same release from two stations is the same bytes.
    let mut sums = from.iter().filter_map(|o| Some((o, o.sha256.as_ref()?)));
    if let Some((first, sum)) = sums.next() {
        if let Some((other, _)) = sums.find(|(_, s)| *s != sum) {
            return Err(NoFleet::Disagree {
                tag: chosen,
                a: first.station.clone(),
                b: other.station.clone(),
            });
        }
    }
    from.sort_by_key(|o| {
        (
            o.have,
            o.rtt_ms.is_none(),
            o.rtt_ms.unwrap_or(0),
            o.station.clone(),
        )
    });
    Ok((chosen, from))
}

/// This station's own copy, put together for a station that is taking it.
#[derive(Default)]
pub struct Held {
    own: std::sync::Mutex<Option<std::sync::Arc<Supplied>>>,
    /// An update through the fleet is under way.
    pub(crate) updating: std::sync::atomic::AtomicBool,
}

impl Daemon {
    // ------------------------------------------------ the station asked

    /// `release.offer`.
    pub(crate) async fn release_offer(&self, tag: Option<&str>, target: &str) -> Value {
        let none = json!({"have": "none"});
        if !TARGETS.contains(&target) || tag.is_some_and(|t| !valid_tag(t)) {
            return none;
        }
        let own_tag = format!("v{VERSION}");
        let host = ReleaseHost::new(self.cfg.update.url.as_deref(), Duration::from_secs(4)).ok();
        // The version: the one named, else what the release host calls its
        // newest (never older than this station), else what is kept.
        let mut reached = false;
        let want = match (tag, &host) {
            (Some(t), _) => Some(t.to_string()),
            (None, Some(host)) => match host.latest().await {
                Ok(latest) => {
                    reached = true;
                    Some(if older(&latest, VERSION) {
                        own_tag.clone()
                    } else {
                        latest
                    })
                }
                Err(_) => None,
            },
            _ => None,
        };
        let kept = self.kept_of(target, want.as_deref());
        if let Some(kept) = kept {
            return json!({"have": "kept", "tag": kept.tag, "size": kept.size, "sha256": kept.sha256});
        }
        if let (Some(want), Some(host)) = (&want, &host) {
            if tag.is_none() || reached || host.latest().await.is_ok() {
                if let Ok(sum) = host.published_sum(want, target).await {
                    return json!({"have": "fetch", "tag": want, "sha256": sum});
                }
            }
        }
        // Its own running program: for a station of its own kind, and for
        // this version only.
        if release_target().as_deref() == Some(target) && tag.is_none_or(|t| t == own_tag) {
            return json!({"have": "own", "tag": own_tag});
        }
        none
    }

    /// `release.prepare`: the archive, ready to be taken in pieces.
    pub(crate) async fn release_prepare(
        &self,
        tag: &str,
        target: &str,
        own: bool,
    ) -> Result<Value> {
        if !TARGETS.contains(&target) || !valid_tag(tag) {
            return Err(invalid("that is not a release"));
        }
        let ready = |s: &Supplied, source: &str| json!({"tag": s.tag, "target": s.target, "size": s.size, "sha256": s.sha256, "source": source});
        if own {
            if release_target().as_deref() != Some(target) || tag != format!("v{VERSION}") {
                return Err(invalid(
                    "this station does not run that version on that kind of machine",
                ));
            }
            let made = self.own_copy(target).await.map_err(invalid)?;
            let answer = ready(&made, "own");
            *self.held.own.lock().unwrap() = Some(std::sync::Arc::new(made));
            self.prune_leftovers();
            return Ok(answer);
        }
        let host = ReleaseHost::new(self.cfg.update.url.as_deref(), Duration::from_secs(300))
            .map_err(invalid)?;
        // Checked against the SHA-256 published now; when the release host
        // does not answer, a kept copy that matches the one it was checked
        // against when it was downloaded. Nothing else is ever handed on.
        match self.published(&host, tag, target).await {
            Ok(s) => Ok(ready(&s, "published")),
            Err(crate::supply::Fetch::Checksum) => Err(invalid(format!(
                "{} downloaded {tag} and it does not match its published checksum, so it supplies nothing",
                self.station
            ))),
            Err(_) => match self.kept_of(target, Some(tag)) {
                Some(s) => Ok(ready(&s, "kept")),
                None => Err(invalid(format!(
                    "{} cannot reach the release host and holds no copy of {tag}",
                    self.station
                ))),
            },
        }
    }

    /// `release.chunk`: `len` bytes from `offset` of the archive whose
    /// SHA-256 is `sha256` (a kept one, or this station's own copy made by
    /// `release.prepare`).
    pub(crate) fn release_chunk(
        &self,
        tag: &str,
        target: &str,
        sha256: &str,
        offset: u64,
        len: u64,
    ) -> Result<Value> {
        use std::io::{Read, Seek, SeekFrom};
        if !TARGETS.contains(&target) || !valid_tag(tag) {
            return Err(invalid("that is not a release"));
        }
        let own = self
            .held
            .own
            .lock()
            .unwrap()
            .clone()
            .filter(|s| s.sha256 == sha256 && s.tag == tag && s.target == target);
        let (path, size): (PathBuf, u64) = match &own {
            Some(s) => (s.path.clone(), s.size),
            None => {
                let dir = self.supply_dir().join(tag);
                let stored = std::fs::read_to_string(dir.join(format!("{target}.sha256")))
                    .unwrap_or_default();
                if stored.trim() != sha256 || sha256.len() != 64 {
                    return Err(invalid("this station does not hold that release"));
                }
                let path = dir.join(format!("{target}.{}", archive_ext(target)));
                let size = std::fs::metadata(&path)?.len();
                (path, size)
            }
        };
        let len = len.min(PIECE_MAX).min(size.saturating_sub(offset));
        let mut bytes = vec![0u8; len as usize];
        let mut file = std::fs::File::open(&path)?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut bytes)?;
        // The last piece of its own copy: it is not kept.
        if own.is_some() && offset + len >= size {
            *self.held.own.lock().unwrap() = None;
        }
        Ok(json!({
            "data_b64": base64::engine::general_purpose::STANDARD.encode(&bytes),
            "size": size,
        }))
    }

    // ------------------------------------------------ the station asking

    /// The stations of this fleet that answer now and say they supply
    /// releases; and, when there is none, those that answer.
    fn suppliers(&self) -> std::result::Result<Vec<String>, NoFleet> {
        let online: Vec<crate::fleet::Peer> = self
            .fleet_members()
            .into_iter()
            .filter(|p| p.state == "online")
            .collect();
        let can: Vec<String> = online
            .iter()
            .filter(|p| {
                p.info["caps"]
                    .as_array()
                    .is_some_and(|c| c.iter().any(|x| x == CAP))
            })
            .map(|p| p.station.clone())
            .collect();
        if can.is_empty() {
            return Err(NoFleet::NoSupplier {
                online: online.into_iter().map(|p| p.station).collect(),
            });
        }
        Ok(can)
    }

    /// Who could supply releases now, for `spandeck doctor`.
    pub(crate) fn supplier_names(&self) -> std::result::Result<Vec<String>, String> {
        self.suppliers().map_err(|e| e.words())
    }

    /// Ask every station that supplies releases what it has, all at once.
    async fn fleet_offers(
        &self,
        tag: Option<&str>,
        target: &str,
    ) -> std::result::Result<(Vec<String>, Vec<Offer>), NoFleet> {
        let stations = self.suppliers()?;
        let mut asks = tokio::task::JoinSet::new();
        for station in stations.clone() {
            let (me, tag, target) = (self.clone(), tag.map(String::from), target.to_string());
            asks.spawn(async move {
                // It may ask its own release host first: more than a call's 5 s.
                let said = me
                    .fleet
                    .call_bulk(
                        &station,
                        "release.offer",
                        json!({"tag": tag, "target": target}),
                        Duration::from_secs(12),
                    )
                    .await
                    .ok()?;
                let have = match said["have"].as_str()? {
                    "kept" => Have::Kept,
                    "fetch" => Have::Fetch,
                    "own" => Have::Own,
                    _ => return None,
                };
                let offered = said["tag"].as_str().filter(|t| valid_tag(t))?.to_string();
                let sha256 = said["sha256"]
                    .as_str()
                    .filter(|s| s.len() == 64)
                    .map(str::to_lowercase);
                if have != Have::Own && sha256.is_none() {
                    return None;
                }
                Some(Offer {
                    rtt_ms: me.perf.rtt_ms_of(&station),
                    station,
                    have,
                    tag: offered,
                    sha256,
                })
            });
        }
        let mut offers = vec![];
        while let Some(done) = asks.join_next().await {
            offers.extend(done.ok().flatten());
        }
        Ok((stations, offers))
    }

    /// Take one release from one station: have it made ready, then piece by
    /// piece into a file of this station's, and check it. `said` is told
    /// how far it is, in words.
    async fn take_from(
        &self,
        offer: &Offer,
        target: &str,
        said: &(impl Fn(String) + Sync),
    ) -> std::result::Result<Supplied, String> {
        let station = &offer.station;
        let ready = self
            .fleet
            .call_bulk(
                station,
                "release.prepare",
                json!({"tag": offer.tag, "target": target, "own": offer.have == Have::Own}),
                PREPARE_LIMIT,
            )
            .await?;
        let size = ready["size"].as_u64().unwrap_or(0);
        let sum = ready["sha256"].as_str().unwrap_or_default().to_lowercase();
        // What it made ready is what was asked for and what it offered.
        if ready["tag"] != json!(offer.tag) || ready["target"] != json!(target) {
            return Err(format!(
                "it made ready {} for {}, which is not what was asked for ({} for {target})",
                ready["tag"].as_str().unwrap_or("?"),
                ready["target"].as_str().unwrap_or("?"),
                offer.tag
            ));
        }
        if size == 0 || size > 1 << 30 || sum.len() != 64 {
            return Err("it did not say the size and SHA-256 of what it made ready".into());
        }
        if offer.sha256.as_ref().is_some_and(|offered| *offered != sum) {
            return Err(
                "what it made ready does not have the SHA-256 it offered, so nothing was taken"
                    .into(),
            );
        }
        let own = offer.have == Have::Own;
        said(format!(
            "getting {} from {station} ({}){}…",
            offer.tag,
            size_words(size),
            if own {
                ": that station's own copy, since no station of the fleet can reach the release host"
            } else {
                ""
            }
        ));
        let dir = self
            .supply_dir()
            .join(format!("in-{}", &crate::paths::random_token()[..8]));
        let path = dir.join(format!("{target}.{}", archive_ext(target)));
        let tidy = |why: String| {
            let _ = std::fs::remove_dir_all(&dir);
            why
        };
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| tidy(e.to_string()))?;
        let (began, mut at, mut piece, mut tries, mut tenth) =
            (Instant::now(), 0u64, PIECE_START, 0u32, 0u64);
        while at < size {
            if began.elapsed() > TAKE_LIMIT {
                return Err(tidy("it took more than fifty minutes".into()));
            }
            let asked = Instant::now();
            let got = self
                .fleet
                .call_bulk(
                    station,
                    "release.chunk",
                    json!({"tag": offer.tag, "target": target, "sha256": sum, "offset": at, "len": piece}),
                    PIECE_LIMIT,
                )
                .await
                .and_then(|v| {
                    base64::engine::general_purpose::STANDARD
                        .decode(v["data_b64"].as_str().unwrap_or_default())
                        .map_err(|e| format!("bad piece: {e}"))
                })
                .and_then(|bytes| match bytes.len() as u64 {
                    0 => Err("an empty piece".to_string()),
                    n if n > piece || at + n > size => Err("a piece that is too long".to_string()),
                    _ => Ok(bytes),
                });
            let bytes = match got {
                Ok(bytes) => bytes,
                // The same piece again, from the same place, smaller.
                Err(why) => {
                    tries += 1;
                    if tries >= PIECE_TRIES {
                        return Err(tidy(format!(
                            "it stopped after {} of {}: {why}",
                            size_words(at),
                            size_words(size)
                        )));
                    }
                    piece = (piece / 2).max(PIECE_MIN);
                    tokio::time::sleep(Duration::from_millis(400 * tries as u64)).await;
                    continue;
                }
            };
            tries = 0;
            use tokio::io::AsyncWriteExt;
            file.write_all(&bytes)
                .await
                .map_err(|e| tidy(e.to_string()))?;
            at += bytes.len() as u64;
            let took = asked.elapsed();
            if took > Duration::from_secs(2) {
                piece = (piece / 2).max(PIECE_MIN);
            } else if took < Duration::from_millis(500) {
                piece = (piece * 2).min(PIECE_MAX);
            }
            if at * 10 / size > tenth && at < size {
                tenth = at * 10 / size;
                said(format!(
                    "getting {} from {station}: {} of {}…",
                    offer.tag,
                    size_words(at),
                    size_words(size)
                ));
            }
            // The link is left a quarter of the time for everything else.
            tokio::time::sleep((took / 4).min(Duration::from_millis(500))).await;
        }
        {
            use tokio::io::AsyncWriteExt;
            file.flush().await.map_err(|e| tidy(e.to_string()))?;
        }
        drop(file);
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| tidy(e.to_string()))?;
        if bytes.len() as u64 != size || !crate::rpc::constant_eq(&sha256_hex(&bytes), &sum) {
            return Err(tidy(
                "what arrived does not have the SHA-256 it was offered with, so it was thrown away"
                    .into(),
            ));
        }
        Ok(Supplied {
            tag: offer.tag.clone(),
            target: target.to_string(),
            path,
            size,
            sha256: sum,
            source: if own {
                Source::FleetOwnCopy
            } else {
                Source::Fleet
            },
            via: Some(station.clone()),
            temporary: Some(dir),
        })
    }

    /// A release for `target` from the fleet: every station that supplies
    /// releases is asked what it has, the best is taken from (`choose`),
    /// and the next when that fails.
    ///
    /// `tag`: a version asked for by name. `explicit`: the owner named it,
    /// so it may be older than this station. `update`: it is for this
    /// station itself, so a station's own running program may be taken
    /// when none holds or can fetch a release, and the release this
    /// station already runs is no use.
    pub async fn release_from_fleet(
        &self,
        tag: Option<&str>,
        target: &str,
        update: bool,
        explicit: bool,
        said: impl Fn(String) + Sync,
    ) -> std::result::Result<Supplied, NoFleet> {
        self.prune_leftovers();
        let (asked, offers) = self.fleet_offers(tag, target).await?;
        if offers.is_empty() {
            return Err(NoFleet::Nothing { asked });
        }
        let named = tag.filter(|_| explicit);
        let (chosen, from) = choose(&offers, named.or(tag), update, update, VERSION)?;
        // A version that was not the owner's to name is never older than
        // this station (`choose` saw to it when none was named).
        if !explicit && older(&chosen, VERSION) {
            return Err(NoFleet::Current { tag: chosen });
        }
        let mut tried = vec![];
        for offer in &from {
            match self.take_from(offer, target, &said).await {
                Ok(supplied) => return Ok(supplied),
                Err(why) => {
                    tracing::warn!(station = %offer.station, %why, "a release could not be taken from a station");
                    // Kept in the ship's log: the next station is asked,
                    // and why this one was passed over should not be lost.
                    self.record(
                        "release.supply",
                        None,
                        None,
                        None,
                        None,
                        json!({"outcome": "not taken", "from": offer.station, "release": offer.tag, "why": why}),
                    );
                    said(format!(
                        "{} could not supply {} ({why}); asking the next station…",
                        offer.station, offer.tag
                    ));
                    tried.push((offer.station.clone(), why));
                }
            }
        }
        Err(NoFleet::Failed { tried })
    }
}

/// Only another station of the fleet has these calls: the owner has no use
/// for them, and no agent and no page may have them.
pub(crate) fn peers_only(caller: &crate::daemon::Caller) -> Result<()> {
    if caller.authority != crate::daemon::Authority::Peer {
        return Err(Error::Denied(
            "a release is supplied to a station of the fleet, at its own request".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(station: &str, have: Have, tag: &str, sum: Option<&str>, rtt: Option<u64>) -> Offer {
        Offer {
            station: station.into(),
            have,
            tag: tag.into(),
            sha256: sum.map(|s| s.repeat(64)),
            rtt_ms: rtt,
        }
    }

    /// The owner's decision of 17 Nov 2026: "it should ask the closest
    /// server to serve it up". Which station that is, from what each
    /// offered: one that holds the release before one that must fetch it,
    /// then the shortest round trip; a station's own copy only when none
    /// holds or can fetch a release; never a release older than the
    /// station unless that version was asked for by name; and two stations
    /// that give different SHA-256s for one release are both refused.
    #[test]
    fn the_closest_station_that_has_the_release_is_chosen() {
        let names = |r: std::result::Result<(String, Vec<Offer>), NoFleet>| {
            let (tag, from) = r.unwrap();
            (tag, from.into_iter().map(|o| o.station).collect::<Vec<_>>())
        };
        let all = [
            offer("far-kept", Have::Kept, "v0.5.0", Some("a"), Some(90)),
            offer("near-fetch", Have::Fetch, "v0.5.0", Some("a"), Some(3)),
            offer("near-kept", Have::Kept, "v0.5.0", Some("a"), Some(4)),
            offer("untimed-kept", Have::Kept, "v0.5.0", Some("a"), None),
            offer("mid-fetch", Have::Fetch, "v0.5.0", Some("a"), Some(20)),
            offer("old", Have::Kept, "v0.4.12", Some("b"), Some(1)),
            offer("own", Have::Own, "v0.6.0", None, Some(1)),
        ];
        assert_eq!(
            names(choose(&all, None, true, true, "0.4.12")),
            (
                "v0.5.0".to_string(),
                vec![
                    "near-kept".to_string(),
                    "far-kept".into(),
                    "untimed-kept".into(),
                    "near-fetch".into(),
                    "mid-fetch".into()
                ]
            )
        );
        // Named by the owner: that version, also an older one.
        assert_eq!(
            names(choose(&all, Some("v0.4.12"), true, true, "0.5.0")),
            ("v0.4.12".to_string(), vec!["old".to_string()])
        );
        // Not named: nothing older than the station, and nothing when the
        // newest is what it runs.
        assert!(matches!(
            choose(&all[5..6], None, true, true, "0.5.0"),
            Err(NoFleet::Nothing { .. })
        ));
        assert!(matches!(
            choose(&all[..5], None, true, true, "0.5.0"),
            Err(NoFleet::Current { .. })
        ));
        // A station adding a machine wants the release it runs itself.
        assert_eq!(
            names(choose(&all[..5], None, false, false, "0.5.0")).0,
            "v0.5.0"
        );
        // A station's own copy: only when no station holds or can fetch
        // one, and only where it is allowed (an update, not Add a machine).
        assert_eq!(
            names(choose(&all[6..], None, true, true, "0.5.0")),
            ("v0.6.0".to_string(), vec!["own".to_string()])
        );
        assert!(matches!(
            choose(&all[6..], None, false, false, "0.5.0"),
            Err(NoFleet::Nothing { .. })
        ));
        // Two stations, two SHA-256s for one release: neither.
        let two = [
            offer("spark2", Have::Kept, "v0.5.0", Some("a"), Some(2)),
            offer("m4", Have::Fetch, "v0.5.0", Some("c"), Some(9)),
        ];
        let no = choose(&two, None, true, true, "0.4.12").unwrap_err();
        assert_eq!(
            no.words(),
            "spark2 and m4 give different SHA-256s for v0.5.0, so one of them holds something that is not that release, and nothing was taken from either"
        );
    }
}
