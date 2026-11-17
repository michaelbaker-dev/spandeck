//! Claude Code's "Switch model?" (DECISIONS.md, "The chat's bar, tasks,
//! copy, notes and the model": found with the real program, 17 Nov 2026).
//!
//! After `/model <name>` in a conversation it has cached for another
//! model, Claude Code asks before it switches, and no hook says so. Until
//! this, nothing in spandeck noticed: the pane read "done", and the next
//! message sent to it was typed into the question, where its Enter
//! answered "Yes" and the message was lost.
//!
//! The screen is read exactly (`spandeck_agents::models::switch_screen`).
//!
//! - **After spandeck's own command** (`pane.model`: the owner chose the
//!   model in the deck's sheet, which says what switching costs), with the
//!   program's marker on "Yes", within a few seconds, and nothing typed by
//!   the owner since: Enter is pressed, once.
//! - **At any other time** (the owner typed `/model` in the terminal, the
//!   marker is on "No", it came late): the pane needs the owner, says
//!   "asks whether to switch model to <name>", and nothing is pressed.
//! - **While a question of the program's own is on a pane's screen** (this
//!   one, its folder-trust question, its review of hooks), nothing sent as
//!   a message is typed into the pane (`prompt_up`): it would be taken as
//!   the answer. Keys still go, since they are how it is answered.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::json;
use spandeck_agents::models::switch_screen;
use spandeck_agents::{AgentKind, AgentState};
use spandeck_terminal::input::Item;

use crate::daemon::{Daemon, PaneEntry};
use crate::paths::now_ms;

/// How long after spandeck's own command the question is taken to be its
/// answer.
const OURS_MS: u64 = 8_000;

/// A question of the program's own on a pane's screen, as the pane's
/// record says it (`prompt` in the pane list and in `live`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    /// `model_switch`.
    pub kind: String,
    /// The model it would switch to, as the program names it.
    pub name: String,
    /// Its marker is on "Yes": Enter switches, Esc goes back.
    pub on_yes: bool,
    pub since_ms: u64,
}

/// spandeck typed a model command into a pane.
#[derive(Debug, Clone, Copy, Default)]
struct Typed {
    at_ms: u64,
    /// How often the question was seen since (it is answered at the second
    /// look, half a second after the first: a screen still being drawn is
    /// not answered).
    seen: u8,
    pressed_ms: u64,
}

#[derive(Default)]
struct Watch {
    typed: HashMap<String, Typed>,
    /// The state a pane was in before it was said to need the owner.
    before: HashMap<String, AgentState>,
}

fn watch() -> &'static Mutex<Watch> {
    static WATCH: OnceLock<Mutex<Watch>> = OnceLock::new();
    WATCH.get_or_init(Default::default)
}

fn reason(name: &str) -> String {
    format!("asks whether to switch model to {name}: answer it in Chat or in the terminal")
}

impl Daemon {
    fn switch_key(&self, pane: &str) -> String {
        format!("{}|{pane}", self.home.display())
    }

    /// `pane.model` typed its command into this pane, now.
    pub(crate) fn model_typed(&self, pane: &str) {
        watch().lock().unwrap().typed.insert(
            self.switch_key(pane),
            Typed {
                at_ms: now_ms(),
                ..Default::default()
            },
        );
    }

    /// The owner typed into the pane: nothing is pressed there for them.
    pub(crate) fn model_owner_typed(&self, pane: &str) {
        watch().lock().unwrap().typed.remove(&self.switch_key(pane));
    }

    /// What a pane's program is asking on its screen now, when it is a
    /// question spandeck knows: a message typed into the pane would be
    /// taken as its answer. Read from the screen as it is at this moment
    /// for the model's question (the half second between two looks is
    /// when a message was lost), and from the pane's record for the rest.
    pub(crate) fn prompt_up(&self, p: &PaneEntry) -> Option<&'static str> {
        let m = p.meta();
        if m.exit.is_some() {
            return None;
        }
        if switch_screen(m.agent, &p.term.lines()).is_some() || m.prompt.is_some() {
            return Some("Switch model?");
        }
        if m.trust_asks.is_some() {
            return Some("whether to trust this folder");
        }
        if m.hooks_trust_asks.is_some() {
            return Some("to review its hooks");
        }
        // Our own command was typed a moment ago and its question may be
        // about to be drawn.
        let typed = watch()
            .lock()
            .unwrap()
            .typed
            .get(&self.switch_key(&p.id))
            .copied();
        typed
            .is_some_and(|t| now_ms().saturating_sub(t.at_ms) < 1_500)
            .then_some("Switch model?")
    }

    /// One look at every pane's screen for the question (twice a second,
    /// with the other screens: `detect_screens`). The panes it answers
    /// for: their state is not read from the rules in this look.
    pub(crate) fn model_switch_look(&self) -> HashSet<String> {
        let mut showing = HashSet::new();
        let now = now_ms();
        for p in self.list_panes() {
            let m = p.meta();
            if m.exit.is_some() || !matches!(m.agent, AgentKind::Claude | AgentKind::Fake) {
                continue;
            }
            let key = self.switch_key(&p.id);
            let Some(sw) = switch_screen(m.agent, &p.term.lines()) else {
                let mut w = watch().lock().unwrap();
                if w.typed
                    .get(&key)
                    .is_some_and(|t| now.saturating_sub(t.at_ms) > OURS_MS)
                {
                    w.typed.remove(&key);
                }
                let before = w.before.remove(&key);
                drop(w);
                // The question is gone: the pane is what it was, unless a
                // hook has said something since.
                if m.prompt.is_some() {
                    let ours = m
                        .needs_reason
                        .as_deref()
                        .is_some_and(|r| r.starts_with("asks whether to switch model"));
                    p.meta.lock().unwrap().prompt = None;
                    if ours && m.state == AgentState::NeedsYou {
                        self.set_state(&p, before.unwrap_or(AgentState::Idle), None);
                    } else {
                        self.emit("pane.updated", Some(&p.id), m.project.as_deref(), json!({}));
                    }
                }
                continue;
            };
            showing.insert(p.id.clone());
            // Ours to answer: the owner chose this in the sheet, a moment
            // ago, and the program's own marker is on "Yes".
            let act = {
                let mut w = watch().lock().unwrap();
                match w.typed.get_mut(&key) {
                    Some(t) if t.pressed_ms > 0 && now.saturating_sub(t.pressed_ms) < 3_000 => {
                        "wait"
                    }
                    Some(t)
                        if t.pressed_ms == 0
                            && sw.on_yes
                            && now.saturating_sub(t.at_ms) <= OURS_MS
                            && self.flag_on("model_switch", true) =>
                    {
                        t.seen += 1;
                        if t.seen >= 2 {
                            t.pressed_ms = now;
                            "press"
                        } else {
                            "wait"
                        }
                    }
                    _ => "say",
                }
            };
            match act {
                "wait" => {}
                "press" => {
                    let sent = p.term.send(Item::keys(b"\r")).is_ok();
                    self.record(
                        "model.switch.confirmed",
                        Some(&p.id),
                        m.project.as_deref(),
                        None,
                        Some("spandeck"),
                        json!({"model": sw.name, "sent": sent, "why": "the owner chose it in the deck"}),
                    );
                }
                _ => {
                    watch().lock().unwrap().typed.remove(&key);
                    let prompt = Prompt {
                        kind: "model_switch".into(),
                        name: sw.name.clone(),
                        on_yes: sw.on_yes,
                        since_ms: m.prompt.as_ref().map_or(now, |p| p.since_ms),
                    };
                    let why = reason(&sw.name);
                    let said = m.prompt.as_ref() == Some(&prompt)
                        && m.needs_reason.as_deref() == Some(why.as_str());
                    if !said {
                        if m.prompt.is_none() {
                            watch().lock().unwrap().before.insert(key, m.state);
                        }
                        p.meta.lock().unwrap().prompt = Some(prompt);
                        if !self.set_state(&p, AgentState::NeedsYou, Some(why)) {
                            self.emit("pane.updated", Some(&p.id), m.project.as_deref(), json!({}));
                        }
                    }
                }
            }
        }
        showing
    }
}
