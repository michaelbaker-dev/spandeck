//! An answer the owner consented to beforehand, given to a screen an agent
//! program shows by itself when it starts. There are two kinds, and they
//! are the only prompts of a program that spandeck ever answers:
//!
//! - the folder-trust question (foldertrust.rs);
//! - the review of spandeck's own hooks (hookstrust.rs).
//!
//! What both hold to is here, once: the screen is the program's first (it
//! is first seen within `WINDOW_MS` of the program's start and before any
//! hook of the session), it has stood unchanged for `SETTLE_MS`, the owner
//! has not typed into the pane, it is answered once per start of the
//! program, a marker is moved at most once and read again before anything
//! is confirmed, and a pane says what waits when nothing was pressed.
//! Whose choice it is, which screen it is and which key is pressed are the
//! kind's own.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::json;
use spandeck_agents::AgentState;

use crate::daemon::{Daemon, PaneEntry, PaneMeta};

/// The screen is the program's first: one that first shows later than this
/// after the program started is not it.
pub const WINDOW_MS: u64 = 20_000;
/// A screen is acted on once it has stood unchanged this long: a program
/// may still be drawing it, and Claude Code takes no key in the first
/// moment its prompt is up.
pub const SETTLE_MS: u64 = 700;
/// How long the marker is given to move after the key that moves it.
pub(crate) const MOVE_MS: u64 = 3_000;

/// On a pane's record: the standing choice it was started with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Choice {
    /// Who started the pane.
    pub by: String,
    /// `folder`: the owner ticked this folder for this program. `project`:
    /// the pane is in the folders of a project the owner switched on.
    /// `station`: the owner's choice for this station (hookstrust.rs).
    pub why: String,
}

/// On a pane's record: its screen was answered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Answered {
    pub at_ms: u64,
    /// `standing`: by spandeck, on the choice the pane was started with.
    /// `tap`: by the owner's own tap.
    pub how: String,
    pub by: String,
}

/// On a pane's record: the screen is up and was not answered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Asks {
    pub since_ms: u64,
    /// The choices can be read, so the owner's one tap can answer it.
    pub button: bool,
    /// Why spandeck pressed nothing, by a short name (each kind has its
    /// own; see foldertrust.rs and hookstrust.rs).
    pub why: String,
    /// The pane's reason while it waits, where the kind words it per case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub says: Option<String>,
}

/// What is known of one pane's first screen, for one kind. `S`: the screen
/// as that kind reads it.
pub(crate) struct Watch<S> {
    /// The start of the program this is about.
    started_ms: u64,
    pub first_seen_ms: Option<u64>,
    /// The screen as last read, and since when it has read the same.
    same: Option<(S, u64)>,
    /// The key that moves the marker was sent, and when.
    moved_ms: Option<u64>,
    /// Nothing is pressed here any more without the owner.
    pub auto_off: bool,
    /// Never read again: answered, or past its start.
    pub closed: bool,
    /// The kind found, in this start of the program, that the screen must
    /// not be answered by anyone through spandeck (hookstrust.rs). It
    /// stays so until the program is started again.
    pub refused: bool,
}

impl<S> Watch<S> {
    fn new(started_ms: u64) -> Self {
        Watch {
            started_ms,
            first_seen_ms: None,
            same: None,
            moved_ms: None,
            auto_off: false,
            closed: false,
            refused: false,
        }
    }
}

/// One look at a pane's screen.
pub(crate) enum Stood<S> {
    /// Its screen is not the one this kind answers.
    NotIt,
    /// It is, and has not stood long enough yet.
    Wait,
    /// It has stood unchanged for the settle time: the screen, and when it
    /// was first seen.
    Stands(S, u64),
}

/// The one key that moves a program's marker.
pub(crate) enum Moving {
    /// Send it now: it has not been sent in this start of the program.
    Now,
    /// It was sent; the marker is given time.
    Wait,
    /// It was sent and the marker is not where it should be: nothing more
    /// is pressed here without the owner.
    Failed,
}

impl<S: PartialEq + Clone> Watch<S> {
    /// `behind`: the first screen is behind this pane (a hook of the
    /// session, an answer already given, a program that ended).
    /// `carried`: a pane carried through an upgrade was already at the
    /// screen if its record says so, and since when.
    pub fn look(
        &mut self,
        now: u64,
        behind: bool,
        carried: Option<u64>,
        read: impl FnOnce() -> Option<S>,
    ) -> Stood<S> {
        if self.closed {
            return Stood::NotIt;
        }
        if behind {
            self.closed = true;
            return Stood::NotIt;
        }
        let age = now.saturating_sub(self.started_ms);
        let Some(seen) = read() else {
            self.same = None;
            if age > WINDOW_MS {
                self.closed = true;
            }
            return Stood::NotIt;
        };
        if self.first_seen_ms.is_none() {
            self.first_seen_ms = match carried {
                Some(since) => Some(since),
                None if age > WINDOW_MS => {
                    self.closed = true;
                    return Stood::NotIt;
                }
                None => Some(now),
            };
        }
        match &self.same {
            Some((was, since)) if *was == seen => {
                if now.saturating_sub(*since) < SETTLE_MS {
                    return Stood::Wait;
                }
            }
            _ => {
                self.same = Some((seen, now));
                return Stood::Wait;
            }
        }
        Stood::Stands(seen, self.first_seen_ms.unwrap_or(now))
    }

    /// The marker is moved once per start of the program, and the screen
    /// must stand again, with the marker where it should be, before
    /// anything is confirmed.
    pub fn moving(&mut self, now: u64) -> Moving {
        match self.moved_ms {
            None => {
                self.moved_ms = Some(now);
                self.same = None;
                Moving::Now
            }
            Some(at) if now.saturating_sub(at) < MOVE_MS => Moving::Wait,
            Some(_) => {
                self.auto_off = true;
                Moving::Failed
            }
        }
    }
}

/// Every pane's watch, for one kind.
pub(crate) struct Watches<S> {
    /// Held while the kind's stored choices are changed.
    pub lock: Mutex<()>,
    watch: Mutex<HashMap<String, Watch<S>>>,
}

impl<S> Default for Watches<S> {
    fn default() -> Self {
        Watches {
            lock: Mutex::new(()),
            watch: Mutex::new(HashMap::new()),
        }
    }
}

impl<S: PartialEq + Clone> Watches<S> {
    pub fn keep_only(&self, live: &HashSet<&str>) {
        self.watch
            .lock()
            .unwrap()
            .retain(|id, _| live.contains(id.as_str()));
    }

    /// Is this start of the pane's program past its first screen?
    pub fn past(&self, pane: &str, started_ms: u64) -> bool {
        self.watch
            .lock()
            .unwrap()
            .get(pane)
            .is_some_and(|w| w.closed && w.started_ms == started_ms)
    }

    /// The owner typed into the pane: they are answering it themselves,
    /// and nothing is pressed for them after that.
    pub fn owner_typed(&self, pane: &str) {
        if let Some(w) = self.watch.lock().unwrap().get_mut(pane) {
            w.auto_off = true;
        }
    }

    /// One pane's turn, with the watches locked: what is decided in `f`
    /// (a claim to answer, say) is decided once.
    pub fn turn<R>(&self, pane: &str, started_ms: u64, f: impl FnOnce(&mut Watch<S>) -> R) -> R {
        let mut watch = self.watch.lock().unwrap();
        let w = watch
            .entry(pane.to_string())
            .or_insert_with(|| Watch::new(started_ms));
        if w.started_ms != started_ms {
            *w = Watch::new(started_ms);
        }
        f(w)
    }

    /// The owner's own tap claims the answer, so that it and a standing
    /// choice cannot both press. Only for a pane whose screen was seen and
    /// is still open, and only if `ok` (looked at with the watches locked).
    pub fn claim(&self, pane: &str, ok: impl FnOnce() -> bool) -> bool {
        let mut watch = self.watch.lock().unwrap();
        match watch
            .get_mut(pane)
            .filter(|w| !w.closed && w.first_seen_ms.is_some())
        {
            Some(w) if ok() => {
                w.closed = true;
                true
            }
            _ => false,
        }
    }

    /// Was this pane's screen refused in this start of its program?
    pub fn refused(&self, pane: &str) -> bool {
        self.watch
            .lock()
            .unwrap()
            .get(pane)
            .is_some_and(|w| w.refused)
    }

    /// A tap that confirmed nothing: the screen is the owner's to answer.
    pub fn reopen(&self, pane: &str) {
        if let Some(w) = self.watch.lock().unwrap().get_mut(pane) {
            w.closed = false;
            w.auto_off = true;
        }
    }
}

impl Daemon {
    /// The owner typed into a pane: neither kind presses anything there
    /// after that.
    pub(crate) fn start_screen_owner_typed(&self, pane: &str) {
        self.folder_trust.owner_typed(pane);
        self.hooks_trust.owner_typed(pane);
        self.model_owner_typed(pane);
        self.picker_owner_typed(pane);
    }

    /// Set or clear what a pane says about the screen it waits at. `slot`:
    /// the kind's own place on the pane's record. `reason`: the pane's
    /// reason while it waits.
    pub(crate) fn start_screen_say(
        &self,
        p: &PaneEntry,
        slot: fn(&mut PaneMeta) -> &mut Option<Asks>,
        asks: Option<Asks>,
        reason: &str,
    ) {
        let (changed, was) = {
            let mut m = p.meta.lock().unwrap();
            let was = slot(&mut m).take();
            *slot(&mut m) = asks.clone();
            (was != asks, was)
        };
        match &asks {
            Some(_) => {
                self.set_state(p, AgentState::NeedsYou, Some(reason.to_string()));
            }
            None => {
                // Only what this said is taken back.
                let said = was
                    .as_ref()
                    .and_then(|a| a.says.as_deref())
                    .unwrap_or(reason);
                let mine = {
                    let m = p.meta.lock().unwrap();
                    m.state == AgentState::NeedsYou && m.needs_reason.as_deref() == Some(said)
                };
                if mine {
                    self.set_state(p, AgentState::Idle, None);
                }
            }
        }
        if changed {
            self.save_pane(p);
            self.emit("pane.updated", Some(&p.id), None, json!({}));
        }
    }
}
