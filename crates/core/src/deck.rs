//! The deck document: the layouts that belong to no project ("global"), and
//! what the owner pinned. One document, kept identical on every station, so
//! the Control Deck's left-hand list is the same wherever it is opened.
//!
//! Each station keeps its own copy (`deck.json`). Only the owner changes it;
//! the station where they did pushes it to the others, and a station keeps
//! whichever copy is newer. A station that was away is brought up to date
//! when it is seen again.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::{invalid, write_private, Daemon, Error, Result};
use crate::project::{valid_name, Layout};

const MAX_LAYOUTS: usize = 48;
const MAX_PINS: usize = 64;

/// Something the owner pinned. A layout with a project is that project's
/// layout on that station; without, a global one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Pin {
    Pane {
        station: String,
        id: String,
    },
    Layout {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        station: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        name: String,
    },
}

impl Pin {
    fn validate(&self) -> std::result::Result<(), String> {
        let ok = match self {
            Pin::Pane { station, id } => valid_name(station) && valid_name(id),
            Pin::Layout {
                station: Some(station),
                project: Some(project),
                name,
            } => valid_name(station) && valid_name(project) && valid_name(name),
            Pin::Layout {
                station: None,
                project: None,
                name,
            } => valid_name(name),
            Pin::Layout { .. } => {
                return Err(
                    "a pinned project layout names its station and its project; a global one names neither"
                        .into(),
                )
            }
        };
        if ok {
            Ok(())
        } else {
            Err("a pin's names use letters, digits, - _ . only".into())
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Deck {
    pub updated_ms: u64,
    /// Global layouts. Each leaf names its station: the Control Deck saves
    /// it so, and gives one saved without (through this API, or by a deck
    /// of 0.4.0 or earlier) the station its pane is found on. Until then such a
    /// leaf is on no station; it is never read as "the station being viewed".
    pub layouts: Vec<Layout>,
    pub pins: Vec<Pin>,
    /// Which stations' changes this copy holds (sync.rs). A station from
    /// before this passes it over and writes the document without it.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub changes: crate::sync::Changes,
}

impl Deck {
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.layouts.len() > MAX_LAYOUTS {
            return Err(format!("there are at most {MAX_LAYOUTS} global layouts"));
        }
        if self.pins.len() > MAX_PINS {
            return Err(format!("at most {MAX_PINS} things can be pinned"));
        }
        if !crate::sync::changes_ok(&self.changes) {
            return Err("the record of changes is not one".into());
        }
        self.layouts.iter().try_for_each(Layout::validate)?;
        self.pins.iter().try_for_each(Pin::validate)
    }
}

impl Daemon {
    fn deck_file(&self) -> PathBuf {
        self.paths.config_dir.join("deck.json")
    }

    pub fn deck(&self) -> Deck {
        std::fs::read(self.deck_file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn deck_write(&self, deck: &Deck) -> Result<()> {
        std::fs::create_dir_all(&self.paths.config_dir)?;
        let tmp = self.deck_file().with_extension("json.tmp");
        write_private(
            &tmp,
            &serde_json::to_string_pretty(deck).map_err(|e| invalid(e.to_string()))?,
        )?;
        std::fs::rename(&tmp, self.deck_file())?;
        Ok(())
    }

    /// The owner's change path, and the only one that writes what was not
    /// already another station's copy. `change` returns what to put in the
    /// ship's log, or nothing when the document is already as asked.
    pub(crate) fn deck_change(
        &self,
        by: &str,
        change: impl FnOnce(&mut Deck) -> Result<Option<Value>>,
    ) -> Result<Deck> {
        let deck = {
            let _held = self.deck_lock.lock().unwrap();
            let mut deck = self.deck();
            let Some(what) = change(&mut deck)? else {
                return Ok(deck);
            };
            deck.validate().map_err(invalid)?;
            // Later than the copy it replaces and than any seen in the fleet.
            let base = deck.updated_ms;
            deck.updated_ms = self.sync_stamp("deck", base);
            self.sync_mark(&mut deck.changes, base, deck.updated_ms);
            self.deck_write(&deck)?;
            self.record("deck.changed", None, None, None, Some(by), what);
            deck
        };
        self.emit(
            "deck.updated",
            None,
            None,
            json!({"updated_ms": deck.updated_ms}),
        );
        // The other stations are told without making the owner wait for them.
        // One that cannot be reached now gets the document when it is seen.
        let (d, pushed) = (self.clone(), deck.clone());
        tokio::spawn(async move { d.deck_send(&pushed, 0).await });
        Ok(deck)
    }

    /// Offer a copy to every online peer, or (when `only_if_older_than` is
    /// set) to those that last said theirs was older than that.
    async fn deck_send(&self, deck: &Deck, only_if_older_than: u64) {
        for peer in self.fleet.peers() {
            let theirs = peer
                .info
                .pointer("/deck/updated_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if peer.state != "online" || (only_if_older_than > 0 && theirs >= only_if_older_than) {
                continue;
            }
            let r = self
                .fleet
                .call(&peer.station, "deck.push", json!({"deck": deck}))
                .await;
            self.sync_sent("deck", deck.updated_ms, &peer.station, r)
                .await;
        }
    }

    pub fn deck_layout_save(&self, layout: Layout, by: &str) -> Result<Value> {
        layout.validate().map_err(invalid)?;
        let name = layout.name.clone();
        let mut changed = true;
        self.deck_change(by, |deck| {
            match deck.layouts.iter().position(|l| l.name == layout.name) {
                Some(at) if deck.layouts[at] == layout => {
                    changed = false;
                    return Ok(None);
                }
                Some(at) => deck.layouts[at] = layout.clone(),
                None if deck.layouts.len() >= MAX_LAYOUTS => {
                    return Err(invalid(format!(
                        "there are at most {MAX_LAYOUTS} global layouts"
                    )))
                }
                None => deck.layouts.push(layout.clone()),
            }
            Ok(Some(json!({"layout": layout.name, "saved": true})))
        })?;
        Ok(json!({"layout": name, "changed": changed}))
    }

    pub fn deck_layout_remove(&self, name: &str, by: &str) -> Result<Value> {
        self.deck_change(by, |deck| {
            let before = deck.layouts.len();
            deck.layouts.retain(|l| l.name != name);
            if deck.layouts.len() == before {
                return Err(Error::NotFound(format!("global layout {name}")));
            }
            deck.pins.retain(
                |p| !matches!(p, Pin::Layout { project: None, name: pinned, .. } if pinned == name),
            );
            Ok(Some(json!({"layout": name, "removed": true})))
        })?;
        self.look_forget_layout(&format!("global:{name}"), by);
        Ok(json!({"removed": name}))
    }

    /// Pin or unpin. Doing either twice changes nothing.
    pub fn deck_pin(&self, pin: Pin, pinned: bool, by: &str) -> Result<Value> {
        pin.validate().map_err(invalid)?;
        let deck = self.deck_change(by, |deck| {
            let at = deck.pins.iter().position(|p| *p == pin);
            match (at, pinned) {
                (Some(_), true) | (None, false) => return Ok(None),
                (Some(at), false) => {
                    deck.pins.remove(at);
                }
                (None, true) if deck.pins.len() >= MAX_PINS => {
                    return Err(invalid(format!(
                        "at most {MAX_PINS} things can be pinned; unpin one first"
                    )))
                }
                (None, true) => deck.pins.push(pin.clone()),
            }
            Ok(Some(json!({"pin": pin, "pinned": pinned})))
        })?;
        Ok(json!(deck.pins))
    }

    /// Another station offering its copy. Kept only if it is newer.
    pub fn deck_push(&self, deck: Deck, from: &str) -> Result<Value> {
        deck.validate().map_err(invalid)?;
        self.sync_saw("deck", deck.updated_ms);
        {
            let _held = self.deck_lock.lock().unwrap();
            let mine = self.deck();
            if deck.updated_ms <= mine.updated_ms {
                return Ok(
                    json!({"station": self.station, "ok": true, "kept": "this station's copy is as new or newer", "updated_ms": mine.updated_ms}),
                );
            }
            self.sync_replaced("deck", &mine.changes, &deck.changes);
            self.deck_write(&deck)?;
        }
        self.record(
            "deck.received",
            None,
            None,
            None,
            Some(from),
            json!({"updated_ms": deck.updated_ms, "layouts": deck.layouts.len(), "pins": deck.pins.len()}),
        );
        self.emit(
            "deck.updated",
            None,
            None,
            json!({"updated_ms": deck.updated_ms}),
        );
        Ok(json!({"station": self.station, "ok": true, "updated_ms": deck.updated_ms}))
    }

    /// Called after discovery: bring peers with an older copy up to date.
    pub(crate) async fn deck_sync(&self) {
        let mine = self.deck();
        if mine.updated_ms > 0 {
            self.deck_send(&mine, mine.updated_ms).await;
        }
    }
}
