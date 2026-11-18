//! What the synced documents share: the deck document, the look document,
//! the agent setup, the owner's preferences and the tools document. Each is one document, kept
//! whole on every station, and the newer copy wins. This file is what keeps
//! "newer" honest:
//!
//!   - a station looks at the fleet before the owner's change is taken, when
//!     it has not looked lately (it has just started, or the machine slept),
//!     and takes a newer copy first, so a change is never made to a stale
//!     copy and then stamped the newest;
//!   - a copy is stamped later than every stamp this station has seen in the
//!     fleet, so a station whose clock is slow does not lose;
//!   - a station asks for a newer copy (`sync.pull`) as well as offering its
//!     own, so it does not wait for the other side to look;
//!   - each copy says, per station, the stamp of the last change made there
//!     that it holds (`changes`). A station whose copy is replaced by one
//!     that lacks a change it held tells the owner which station's change
//!     was lost, instead of losing it in silence.
//!
//! It is still one whole document and the last writer wins: two changes to
//! the same document at two stations, neither having the other's, leave one.
//! DECISIONS.md ("The same view from every station") says when that can be.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::daemon::{invalid, Daemon, Result};
use crate::paths::now_ms;

/// The documents, by the name each has in `station.info`.
pub(crate) const DOCS: [&str; 5] = ["deck", "look", "setup", "prefs", "tools"];

/// A station that looked at the fleet longer ago than this looks again
/// before a change is taken. A little over the time between two looks.
const LOOK_STALE_MS: u64 = 12_000;

#[derive(Default)]
pub(crate) struct SyncState {
    /// The newest stamp seen anywhere in the fleet, per document.
    seen: Mutex<HashMap<&'static str, u64>>,
    /// When this station last finished looking at the fleet. 0: never.
    looked_ms: AtomicU64,
    /// One look at a time.
    pub(crate) looking: tokio::sync::Mutex<()>,
}

fn doc_name(doc: &str) -> Option<&'static str> {
    DOCS.iter().copied().find(|d| *d == doc)
}

/// Per station, the stamp of the last change made there that a copy holds.
pub type Changes = BTreeMap<String, u64>;

/// A record that arrived from another station: bounded, or not kept.
pub(crate) fn changes_ok(changes: &Changes) -> bool {
    changes.len() <= 256 && changes.keys().all(|k| !k.is_empty() && k.len() <= 64)
}

/// What a document is called when the owner is told about it.
fn spoken(doc: &str) -> &'static str {
    match doc {
        "deck" => "the global layouts and pins",
        "look" => "the themes and looks",
        "setup" => "the agent setup",
        "tools" => "the tools for agents",
        _ => "your preferences",
    }
}

impl Daemon {
    fn sync_stamp_here(&self, doc: &str) -> u64 {
        match doc {
            "deck" => self.deck().updated_ms,
            "look" => self.look().updated_ms,
            "setup" => self.setup().updated_ms,
            "tools" => self.tools_doc().updated_ms,
            _ => self.prefs().updated_ms,
        }
    }

    /// A stamp was seen: another station's copy, or one offered to this one.
    pub(crate) fn sync_saw(&self, doc: &'static str, stamp: u64) {
        let mut seen = self.sync.seen.lock().unwrap();
        let s = seen.entry(doc).or_insert(0);
        *s = (*s).max(stamp);
    }

    /// The stamp for a change made here: later than the copy it replaces and
    /// than every copy seen in the fleet, whatever this machine's clock says.
    pub(crate) fn sync_stamp(&self, doc: &'static str, replaces: u64) -> u64 {
        let seen = self.sync.seen.lock().unwrap().get(doc).copied();
        let stamp = now_ms().max(replaces + 1).max(seen.unwrap_or(0) + 1);
        self.sync_saw(doc, stamp);
        stamp
    }

    /// Record in a copy that it now holds a change made here, stamped
    /// `stamp`, made to the copy stamped `base`. A copy with no record (from
    /// before there was one, or one that passed through a station that
    /// keeps none) is taken to hold every change up to its own stamp: `*`.
    pub(crate) fn sync_mark(&self, changes: &mut Changes, base: u64, stamp: u64) {
        if changes.is_empty() && base > 0 {
            changes.insert("*".into(), base);
        }
        changes.insert(self.station.clone(), stamp);
    }

    /// This station's copy is being replaced by a newer one. A change this
    /// copy holds that the newer one does not is lost: the owner is told
    /// which station it was made at. Every station that held it says the
    /// same words, which the inbox shows once. A newer copy with no record
    /// tells nothing, and nothing is said.
    pub(crate) fn sync_replaced(&self, doc: &'static str, mine: &Changes, theirs: &Changes) {
        if theirs.is_empty() {
            return;
        }
        let floor = theirs.get("*").copied().unwrap_or(0);
        let lost: Vec<&str> = mine
            .iter()
            .filter(|(station, at)| {
                station.as_str() != "*"
                    && theirs.get(*station).copied().unwrap_or(0).max(floor) < **at
            })
            .map(|(station, _)| station.as_str())
            .collect();
        if !lost.is_empty() {
            self.notify(
                "sync.lost",
                None,
                None,
                &format!("A change to {} made at {} was not kept", spoken(doc), lost.join(", ")),
                "The same thing was changed at another station that did not have that change yet, and the later of the two replaced the other. If something you changed is missing, change it again.",
            );
        }
    }

    /// The station answering `sync.pull`: its copy, for a station that asks.
    pub(crate) fn sync_copy(&self, doc: &str) -> Result<Value> {
        let doc = doc_name(doc).ok_or_else(|| invalid("no such document"))?;
        let copy = match doc {
            "deck" => json!(self.deck()),
            "look" => json!(self.look()),
            "setup" => json!(self.setup()),
            "tools" => json!(self.tools_doc()),
            _ => json!(self.prefs()),
        };
        Ok(json!({"doc": doc, "copy": copy}))
    }

    /// Ask `station` for its copy of a document and take it if it is newer.
    pub(crate) async fn sync_pull_from(&self, station: &str, doc: &'static str) {
        let copy = match self
            .fleet
            .call(station, "sync.pull", json!({"doc": doc}))
            .await
        {
            Ok(v) => v["copy"].clone(),
            // A station from before `sync.pull` (0.4.0) has no such call. Its
            // copy is read with the call a Control Deck opened here would
            // make of it; it has no preferences document and no tools one.
            Err(e) if e.contains("not available") && !matches!(doc, "prefs" | "tools") => {
                let got = self
                    .fleet
                    .call(
                        station,
                        "rpc.forward",
                        json!({"method": format!("{doc}.get"), "params": {}}),
                    )
                    .await;
                match (got, doc) {
                    (Ok(v), "setup") => v["setup"].clone(),
                    (Ok(v), _) => v,
                    (Err(_), _) => return,
                }
            }
            Err(_) => return,
        };
        let took = match doc {
            "deck" => serde_json::from_value(copy).map(|c| self.deck_push(c, station)),
            "look" => serde_json::from_value(copy).map(|c| self.look_push(c, station)),
            "setup" => serde_json::from_value(copy).map(|c| self.setup_push(c, station)),
            "tools" => serde_json::from_value(copy).map(|c| self.tools_push(c, station)),
            _ => serde_json::from_value(copy).map(|c| self.prefs_push(c, station)),
        };
        if let Ok(Err(e)) = took {
            tracing::warn!(station, doc, error = %e, "a newer copy could not be taken");
        }
    }

    /// In a station's round: note the stamps it said, and take its copy of
    /// each document of which it has the newest in reach. A newer copy
    /// that a third station said it has is left to that station's round,
    /// so a copy is not taken only to be replaced a moment later.
    pub(crate) async fn sync_pull(&self, peer: &crate::fleet::Peer) {
        for doc in DOCS {
            let at = format!("/{doc}/updated_ms");
            let stamp = |p: &crate::fleet::Peer| p.info.pointer(&at).and_then(|v| v.as_u64());
            let Some(theirs) = stamp(peer) else {
                continue;
            };
            self.sync_saw(doc, theirs);
            let newer_elsewhere = self.fleet.peers().iter().any(|p| {
                p.state == "online" && p.station != peer.station && stamp(p) > Some(theirs)
            });
            if theirs > self.sync_stamp_here(doc) && !newer_elsewhere {
                self.sync_pull_from(&peer.station, doc).await;
            }
        }
    }

    pub(crate) fn sync_looked(&self) {
        self.sync.looked_ms.store(now_ms(), Ordering::SeqCst);
    }

    pub(crate) fn sync_look_is_stale(&self) -> bool {
        let (at, now) = (self.sync.looked_ms.load(Ordering::SeqCst), now_ms());
        at == 0 || now < at || now - at > LOOK_STALE_MS
    }

    /// Before the owner's change to a synced document is taken: a station
    /// that has not looked at the fleet lately (just started, or the machine
    /// slept) looks now, and takes a newer copy if there is one.
    pub(crate) async fn sync_catch_up(&self) {
        if self.sync_look_is_stale() {
            self.look_at_fleet(true).await;
        }
    }

    /// What `station` answered when it was offered this station's copy
    /// (stamped `stamp`). If it kept its own because that is newer, this
    /// station takes that one now, and the owner is told if a change made
    /// here went with it.
    pub(crate) async fn sync_sent(
        &self,
        doc: &'static str,
        stamp: u64,
        station: &str,
        reply: std::result::Result<Value, String>,
    ) {
        let Ok(v) = reply else { return };
        if v.get("kept").is_none() {
            return;
        }
        let theirs = v["updated_ms"].as_u64().unwrap_or(0);
        self.sync_saw(doc, theirs);
        if theirs > stamp {
            self.sync_pull_from(station, doc).await;
        }
    }
}
