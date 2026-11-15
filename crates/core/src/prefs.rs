//! The owner's preferences: small choices about how the Control Deck behaves
//! that should follow the owner from station to station, not stay with one
//! browser on one station (each station is its own web address, so what a
//! browser keeps for one it does not have for another).
//!
//! One document, `prefs.json`, kept the same on every station by the rules
//! of the deck and look documents (sync.rs). It is a document of its own for
//! the reason the look document is: a station from before it (0.4.0) reads
//! the deck document into the fields it knows and writes only those back, so
//! a preference kept there would be dropped fleet-wide at that station's
//! next change. Such a station never holds this one: it is offered only to a
//! station that says it keeps it. It has a format number for the same reason
//! the look document has.
//!
//! What is in it is data, checked here: a preference this version does not
//! know is dropped, and each value is of its kind and bounded.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::{invalid, write_private, Daemon, Result};

pub const FORMAT: u32 = 1;

/// Every preference there is, and whether a value is one it can take.
fn value_ok(key: &str, v: &Value) -> Option<bool> {
    Some(match key {
        // "Continue the last conversation" in the New pane dialog.
        "newPane.resumeLast" => v.is_boolean(),
        // The parts of the list on the left that are folded shut.
        "side.folded" => v.as_array().is_some_and(|list| {
            list.len() <= 256
                && list.iter().all(|k| {
                    k.as_str()
                        .is_some_and(|k| k.len() <= 200 && !k.chars().any(char::is_control))
                })
        }),
        _ => return None,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub format: u32,
    pub updated_ms: u64,
    pub values: BTreeMap<String, Value>,
    /// Which stations' changes this copy holds (sync.rs).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub changes: crate::sync::Changes,
}

impl Prefs {
    /// The document with what this version knows, or why it is refused.
    pub fn clean(mut self) -> std::result::Result<Prefs, String> {
        let mut values = BTreeMap::new();
        for (key, value) in std::mem::take(&mut self.values) {
            match value_ok(&key, &value) {
                None => continue,
                Some(true) => values.insert(key, value),
                Some(false) => return Err(format!("{key} does not take that value")),
            };
        }
        self.values = values;
        if !crate::sync::changes_ok(&self.changes) {
            self.changes.clear();
        }
        Ok(self)
    }
}

impl Daemon {
    fn prefs_file(&self) -> PathBuf {
        self.paths.config_dir.join("prefs.json")
    }

    pub fn prefs(&self) -> Prefs {
        std::fs::read(self.prefs_file())
            .ok()
            .and_then(|b| serde_json::from_slice::<Prefs>(&b).ok())
            .and_then(|p| p.clean().ok())
            .unwrap_or_default()
    }

    fn prefs_write(&self, prefs: &Prefs) -> Result<()> {
        std::fs::create_dir_all(&self.paths.config_dir)?;
        let tmp = self.prefs_file().with_extension("json.tmp");
        write_private(
            &tmp,
            &serde_json::to_string_pretty(prefs).map_err(|e| invalid(e.to_string()))?,
        )?;
        std::fs::rename(&tmp, self.prefs_file())?;
        Ok(())
    }

    /// The owner sets one preference; `null` puts it back to its default.
    pub fn prefs_set(&self, key: &str, value: Value, by: &str) -> Result<Value> {
        match value_ok(key, &value) {
            None => return Err(invalid(format!("there is no preference called {key}"))),
            Some(false) if !value.is_null() => {
                return Err(invalid(format!("{key} does not take that value")))
            }
            _ => {}
        }
        let prefs = {
            let _held = self.prefs_lock.lock().unwrap();
            let mut prefs = self.prefs();
            if prefs.format > FORMAT {
                return Err(invalid(
                    "the preferences saved here are from a newer spandeck than this station runs; update this station to change them",
                ));
            }
            let before = prefs.values.clone();
            if value.is_null() {
                prefs.values.remove(key);
            } else {
                prefs.values.insert(key.to_string(), value);
            }
            if prefs.values == before {
                return Ok(json!(prefs));
            }
            prefs.format = FORMAT;
            let base = prefs.updated_ms;
            prefs.updated_ms = self.sync_stamp("prefs", base);
            self.sync_mark(&mut prefs.changes, base, prefs.updated_ms);
            self.prefs_write(&prefs)?;
            self.record(
                "prefs.changed",
                None,
                None,
                None,
                Some(by),
                json!({"key": key}),
            );
            prefs
        };
        self.emit(
            "prefs.updated",
            None,
            None,
            json!({"updated_ms": prefs.updated_ms}),
        );
        let (d, pushed) = (self.clone(), prefs.clone());
        tokio::spawn(async move { d.prefs_send(&pushed).await });
        Ok(json!(prefs))
    }

    /// Offer a copy to the online stations that keep this document, each in
    /// a call of its own and all at the same time: one that is slow to
    /// answer holds up none of the others.
    async fn prefs_send(&self, prefs: &Prefs) {
        let peers = self.fleet.peers();
        crate::fleet::futures_join_all(peers.iter().map(|p| self.prefs_offer(p, prefs, false)))
            .await;
    }

    /// Offer a copy to one online station. `if_older` (its round, at every
    /// look): only when it last said its copy was older.
    pub(crate) async fn prefs_offer(
        &self,
        peer: &crate::fleet::Peer,
        prefs: &Prefs,
        if_older: bool,
    ) {
        let Some(theirs) = peer.info.pointer("/prefs/updated_ms") else {
            return;
        };
        let theirs = theirs.as_u64().unwrap_or(0);
        let reads = peer.info.pointer("/prefs/format").and_then(|f| f.as_u64());
        if peer.state != "online"
            || reads < Some(prefs.format as u64)
            || (if_older && theirs >= prefs.updated_ms)
        {
            return;
        }
        let r = self
            .fleet
            .call(&peer.station, "prefs.push", json!({"prefs": prefs}))
            .await;
        self.sync_sent("prefs", prefs.updated_ms, &peer.station, r)
            .await;
    }

    /// Another station offering its copy. Kept only if it is newer, and in a
    /// format this station can hold without losing any of it.
    pub fn prefs_push(&self, prefs: Prefs, _from: &str) -> Result<Value> {
        if prefs.format > FORMAT {
            return Ok(
                json!({"station": self.station, "ok": false, "kept": "that copy is in a newer format than this station knows"}),
            );
        }
        let prefs = prefs.clean().map_err(invalid)?;
        self.sync_saw("prefs", prefs.updated_ms);
        {
            let _held = self.prefs_lock.lock().unwrap();
            let mine = self.prefs();
            if prefs.format < mine.format || prefs.updated_ms <= mine.updated_ms {
                return Ok(
                    json!({"station": self.station, "ok": true, "kept": "this station's copy is as new or newer", "updated_ms": mine.updated_ms}),
                );
            }
            self.sync_replaced("prefs", &mine.changes, &prefs.changes);
            self.prefs_write(&prefs)?;
        }
        self.emit(
            "prefs.updated",
            None,
            None,
            json!({"updated_ms": prefs.updated_ms}),
        );
        Ok(json!({"station": self.station, "ok": true, "updated_ms": prefs.updated_ms}))
    }
}
