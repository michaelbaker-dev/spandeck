//! Trusting spandeck's own hooks (DECISIONS.md, "Trusting spandeck's own
//! hooks").
//!
//! Codex shows "Hooks need review" the first time on a machine, and again
//! whenever spandeck's hooks change, and runs none of them until someone
//! answers. The owner's choice, on by default and kept per station, is
//! that spandeck answers it with "Trust all and continue", and only when
//! every hook Codex can be asking about is spandeck's own.
//!
//! - **Whose choice.** Only the owner's: the tick in the New pane dialog,
//!   `spandeck pane new --no-trust-hooks`, or the switch in Agent setup. It
//!   is kept in this station's own state, never in a project's file. An
//!   agent has no method that sets it, reads it or answers the screen; a
//!   pane an agent asks for, and a project's launcher, start with the
//!   owner's standing choice as it is.
//! - **The safety condition** (agents/hookstrust.rs, `only_ours`): "Trust
//!   all" trusts every new or changed hook from every source, so the files
//!   Codex takes hooks from are read each time before a key is pressed.
//!   When they do not show that the hooks can only be spandeck's, nothing
//!   is pressed, the pane says why, and no one-tap answer is offered.
//! - **What is answered** is held to everything the folder-trust question
//!   is (startscreen.rs): the program's own screen from its first row to
//!   its last, first seen soon after the program started and before any
//!   hook of the session, unchanged for the settle time, the owner not
//!   having typed, once per start of the program. Moving the marker from
//!   "1. Review hooks" to "2. Trust all and continue" is part of what the
//!   owner chose: it is moved, the screen is read again, and Enter is
//!   pressed only with the marker seen on the trusting choice.
//! - Codex's own trust records are never written: a key is pressed, and
//!   the program records what it records.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spandeck_agents::foldertrust::Level;
use spandeck_agents::hookstrust::{self as screen, Seen, Start, Step, Unsafe};
use spandeck_agents::AgentKind;
use spandeck_terminal::Item;

use crate::daemon::{invalid, Daemon, PaneEntry, PaneMeta, Result};
use crate::paths::now_ms;
use crate::startscreen::{Answered, Asks, Choice, Moving, Stood, Watch, MOVE_MS};

const KEY: &str = "hooks_trust";
/// Said in `caps` by a station that has this.
pub const CAP: &str = "hooks_trust";
/// A pane's reason when the hooks can only be spandeck's and nothing was
/// pressed all the same (not chosen, the owner typed, a screen that says
/// more than the known one).
pub const ASKS: &str = "asks you to trust spandeck's hooks: answer in the terminal";

/// Said when the files show only spandeck's hooks now and did not when
/// this pane's program was first looked at.
const CHANGED_SINCE: &str = "the hooks it found were not only spandeck's when it started, whatever the files say now (start the pane again to have it look afresh)";

/// Panes that may still be at their program's review of hooks.
pub(crate) type State = crate::startscreen::Watches<Seen>;

/// The owner's standing choice on this station. Not there: on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Standing {
    on: bool,
    at_ms: u64,
    by: String,
}

/// A pane's reason when the hooks may not all be spandeck's.
fn not_ours_reason(why: &str) -> String {
    format!("asks you to trust hooks: {why}. Review them in the terminal")
}

impl Daemon {
    fn hooks_trust_standing(&self) -> Option<Standing> {
        self.db
            .kv_get(KEY)
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    /// The owner's standing choice: on until they turn it off. On by
    /// default because the hooks are spandeck's own, which the owner
    /// installed, and nothing else is ever trusted by it.
    pub fn hooks_trust_on(&self) -> bool {
        self.hooks_trust_standing().is_none_or(|s| s.on)
    }

    /// `hookstrust.set`: the owner's switch for this station.
    pub fn hooks_trust_set(&self, on: bool, by: &str) -> Value {
        let _one = self.hooks_trust.lock.lock().unwrap();
        if self.hooks_trust_on() != on {
            let standing = Standing {
                on,
                at_ms: now_ms(),
                by: by.to_string(),
            };
            self.db
                .kv_set(KEY, &serde_json::to_string(&standing).unwrap_or_default());
            self.record(
                "hookstrust.set",
                None,
                None,
                None,
                Some(by),
                json!({"on": on}),
            );
        }
        json!({"station": self.station, "on": on})
    }

    /// The choice a new pane starts with. `asked`: the tick box, when the
    /// owner's own start sent one; not sent (a deck from before this, the
    /// command line without the flag, a launcher, a helper), the standing
    /// choice as it is.
    pub fn hooks_trust_at_start(
        &self,
        asked: Option<bool>,
        agent: AgentKind,
        by: &str,
    ) -> Option<Choice> {
        if !screen::asks(agent) {
            return None;
        }
        asked
            .unwrap_or_else(|| self.hooks_trust_on())
            .then(|| Choice {
                by: by.to_string(),
                why: "station".to_string(),
            })
    }

    /// Once the owner's pane has started: the tick they sent is the
    /// standing choice from now on.
    pub fn hooks_trust_keep(&self, asked: Option<bool>, agent: AgentKind, by: &str) {
        if let (Some(on), true) = (asked, screen::asks(agent)) {
            self.hooks_trust_set(on, by);
        }
    }

    /// Why this station answers no review at all, if it does not: it is
    /// not the one that keeps spandeck's hooks in the agents' settings.
    fn hooks_trust_not_here(&self) -> Option<String> {
        (!self.owns_agent_home()).then(|| self.not_owning_words())
    }

    /// For `spandeck doctor`, after what it says of Codex's hooks: what
    /// this station does about the review now.
    pub(crate) fn hooks_trust_doctor_words(&self) -> String {
        match (self.hooks_trust_not_here().is_some(), self.hooks_trust_on()) {
            (true, _) => " On this station the review is not answered for you: it does not keep spandeck's hooks.".into(),
            (false, true) => " On this station that is ON: the review is answered for you.".into(),
            (false, false) => " On this station that is OFF: you turned it off, and the review waits for you.".into(),
        }
    }

    /// `hookstrust.choice`: what the New pane dialog shows before a pane
    /// of `agent` is started, and (with no agent) what Agent setup shows.
    pub fn hooks_trust_choice(&self, agent: Option<AgentKind>) -> Value {
        let standing = self.hooks_trust_standing();
        let mut v = json!({
            "station": self.station,
            "on": standing.as_ref().is_none_or(|s| s.on),
            "at_ms": standing.as_ref().map(|s| s.at_ms),
            "by": standing.as_ref().map(|s| s.by.clone()),
            // A station that does not keep the hooks answers nothing.
            "answers": self.owns_agent_home(),
            "not_here": self.hooks_trust_not_here(),
            "programs": screen::PROGRAMS.iter().map(|k| json!({
                "agent": k, "agent_name": k.display(), "answer": screen::answer(*k),
            })).collect::<Vec<_>>(),
        });
        if let Some(agent) = agent {
            v["asks"] = json!(screen::asks(agent));
            v["agent"] = json!(agent);
            v["agent_name"] = json!(agent.display());
            v["answer"] = json!(screen::answer(agent));
        }
        v
    }

    /// `hooks_trust` in a pane's view.
    pub(crate) fn hooks_trust_view(&self, m: &PaneMeta) -> Value {
        if m.hooks_trust_choice.is_none()
            && m.hooks_trust_asks.is_none()
            && m.hooks_trust_answered.is_none()
        {
            return Value::Null;
        }
        json!({
            "chosen": m.hooks_trust_choice, "asks": m.hooks_trust_asks,
            "answered": m.hooks_trust_answered,
        })
    }

    /// Can the hooks this pane's program asks about only be spandeck's?
    /// Read from the files, anew, each time it is asked.
    fn hooks_trust_safe(
        &self,
        m: &PaneMeta,
        on_screen: Option<u32>,
    ) -> std::result::Result<u32, String> {
        if let Some(why) = self.hooks_trust_not_here() {
            return Err(format!(
                "this station does not keep spandeck's hooks ({why})"
            ));
        }
        // The settings folder as the pane's program sees it.
        let set = m
            .spec
            .env
            .iter()
            .rev()
            .find(|(k, _)| k == "CODEX_HOME")
            .map(|(_, v)| PathBuf::from(v));
        let inherited = (!m.spec.env_remove.iter().any(|k| k == "CODEX_HOME"))
            .then(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .flatten();
        let settings_home = set.or(inherited);
        screen::only_ours(
            m.agent,
            &Start {
                home: &self.home,
                cwd: Path::new(&m.cwd),
                bin: &self.exe.display().to_string(),
                args: &m.spec.args,
                settings_home: settings_home.as_deref(),
                system: Path::new("/etc/codex"),
            },
            on_screen,
        )
        .map_err(|u: Unsafe| u.says())
    }

    /// Look at every pane that may still be at its program's review of
    /// hooks. Returns the panes whose screen is the review, which the
    /// ordinary screen detection leaves alone: what they say comes from
    /// here.
    pub(crate) fn hooks_trust_look(&self) -> HashSet<String> {
        let mut showing = HashSet::new();
        let now = now_ms();
        let panes = self.list_panes();
        self.hooks_trust
            .keep_only(&panes.iter().map(|p| p.id.as_str()).collect());
        for p in panes {
            let m = {
                let m = p.meta.lock().unwrap();
                if !screen::asks(m.agent) {
                    continue;
                }
                let started = m.proc_started_ms.unwrap_or(m.created_ms);
                if self.hooks_trust.past(&p.id, started) && m.hooks_trust_asks.is_none() {
                    continue;
                }
                m.clone()
            };
            let started = m.proc_started_ms.unwrap_or(m.created_ms);
            let act = self
                .hooks_trust
                .turn(&p.id, started, |w| self.hooks_trust_decide(&p, &m, now, w));
            match act {
                Some(act) => {
                    showing.insert(p.id.clone());
                    match act {
                        Act::Wait => {}
                        Act::Keys(keys) => {
                            let _ = p.term.send(Item::keys(keys));
                        }
                        Act::Answer(keys, choice) => {
                            self.hooks_trust_answered(&p, &m, keys, "standing", &choice.by)
                        }
                        Act::Say(asks) => self.hooks_trust_say(&p, Some(asks)),
                    }
                }
                None if m.hooks_trust_asks.is_some() => self.hooks_trust_say(&p, None),
                None => {}
            }
        }
        showing
    }

    fn hooks_trust_decide(
        &self,
        p: &PaneEntry,
        m: &PaneMeta,
        now: u64,
        w: &mut Watch<Seen>,
    ) -> Option<Act> {
        let behind = m.hooks_seen || m.hooks_trust_answered.is_some() || m.exit.is_some();
        let carried = m.hooks_trust_asks.as_ref().map(|a| a.since_ms);
        let read = || screen::read(m.agent, &p.term.lines());
        let (seen, since_ms) = match w.look(now, behind, carried, read) {
            Stood::NotIt => return None,
            Stood::Wait => return Some(Act::Wait),
            Stood::Stands(seen, since_ms) => (seen, since_ms),
        };
        let asks = |button: bool, why: &str, says: String| {
            Some(Act::Say(Asks {
                since_ms,
                button,
                why: why.to_string(),
                says: Some(says),
            }))
        };
        // First, whose hooks they are. Not known to be only spandeck's:
        // nothing is pressed and no "trust all" is offered, whatever the
        // owner chose.
        if let Err(why) = self.hooks_trust_safe(m, seen.count) {
            w.refused = true;
            return asks(false, "not_only_spandecks", not_ours_reason(&why));
        }
        // The program read its hooks when it started. Files that were put
        // right while it waits say nothing about what it asks: once
        // refused, this start of the program stays refused (also through
        // an upgrade, by the pane's record).
        let was_refused = m
            .hooks_trust_asks
            .as_ref()
            .is_some_and(|a| a.why == "not_only_spandecks");
        if w.refused || was_refused {
            w.refused = true;
            return asks(false, "not_only_spandecks", not_ours_reason(CHANGED_SINCE));
        }
        // They are spandeck's own. Where the choices can be read the
        // owner's one tap can answer; whether spandeck does is the choice.
        let button = seen.level >= Level::Choices && seen.mark.is_some();
        let waits = |why: &str| asks(button, why, ASKS.to_string());
        let Some(choice) = m
            .hooks_trust_choice
            .as_ref()
            .filter(|_| self.hooks_trust_on())
        else {
            return waits("not_chosen");
        };
        if w.auto_off {
            return waits("you_typed");
        }
        let (Level::Exact, Some(mark)) = (seen.level, seen.mark) else {
            return waits("not_the_known_screen");
        };
        match screen::step(mark) {
            Step::Confirm(keys) => {
                // Claimed here, with the watch locked: it is answered once.
                w.closed = true;
                Some(Act::Answer(keys, choice.clone()))
            }
            Step::Move(keys) => match w.moving(now) {
                Moving::Now => Some(Act::Keys(keys)),
                Moving::Wait => Some(Act::Wait),
                Moving::Failed => waits("marker_did_not_move"),
            },
        }
    }

    /// Set or clear what a pane says about the review on its screen.
    fn hooks_trust_say(&self, p: &PaneEntry, asks: Option<Asks>) {
        let reason = asks
            .as_ref()
            .and_then(|a| a.says.clone())
            .unwrap_or_else(|| ASKS.to_string());
        self.start_screen_say(p, |m| &mut m.hooks_trust_asks, asks, &reason);
    }

    /// Press the confirming key, once, and put it on record.
    fn hooks_trust_answered(&self, p: &PaneEntry, m: &PaneMeta, keys: &[u8], how: &str, by: &str) {
        if p.term.send(Item::keys(keys)).is_err() {
            return;
        }
        p.meta.lock().unwrap().hooks_trust_answered = Some(Answered {
            at_ms: now_ms(),
            how: how.to_string(),
            by: by.to_string(),
        });
        self.hooks_trust_say(p, None);
        self.save_pane(p);
        let chose = screen::answer(m.agent).unwrap_or_default();
        let says = match how {
            "standing" => format!(
                "spandeck answered {}'s review of hooks with \"{chose}\": every hook it could ask about was spandeck's own",
                m.agent.display()
            ),
            _ => format!(
                "{by} answered {}'s review of hooks with \"{chose}\"",
                m.agent.display()
            ),
        };
        self.record(
            "pane.hooks_trusted",
            Some(&p.id),
            m.project.as_deref(),
            m.task.as_deref(),
            Some(if how == "standing" { "spandeck" } else { by }),
            json!({
                "agent": m.agent, "how": how, "name": m.name, "says": says,
                "started_by": m.hooks_trust_choice.as_ref().map(|c| c.by.clone()),
            }),
        );
        self.emit("pane.updated", Some(&p.id), None, json!({}));
    }

    /// `pane.trust_hooks`: the owner's own "Trust spandeck's hooks" on a
    /// pane that shows the review. The same safety condition holds for it:
    /// where the hooks may not all be spandeck's it presses nothing. The
    /// marker is moved to the trusting choice, the screen is read again,
    /// and only then is Enter pressed.
    pub async fn hooks_trust_tap(&self, pane: &str, by: &str) -> Result<Value> {
        let p = self.pane(pane)?;
        let m = p.meta();
        let not_now = || {
            invalid("this pane is not showing a review of hooks that spandeck can read: answer it in the terminal")
        };
        let read = || {
            screen::read(m.agent, &p.term.lines())
                .filter(|s| s.level >= Level::Choices)
                .and_then(|s| s.mark.map(|mark| (mark, s.count)))
        };
        let safe = |count| {
            self.hooks_trust_safe(&m, count).map_err(|why| {
                invalid(format!(
                    "nothing was pressed: {why}. Review the hooks in the terminal"
                ))
            })
        };
        let (_, count) = read().ok_or_else(not_now)?;
        safe(count)?;
        if self.hooks_trust.refused(pane) {
            return Err(invalid(format!(
                "nothing was pressed: {CHANGED_SINCE}. Review the hooks in the terminal"
            )));
        }
        if !self
            .hooks_trust
            .claim(pane, || m.exit.is_none() && read().is_some())
        {
            return Err(not_now());
        }
        // At most the two moves there are between three choices.
        for _ in 0..3 {
            let Some((mark, count)) = read() else { break };
            match screen::step(mark) {
                Step::Confirm(keys) => {
                    // Read from the files once more, at the last moment.
                    if let Err(e) = safe(count) {
                        self.hooks_trust.reopen(pane);
                        return Err(e);
                    }
                    self.hooks_trust_answered(&p, &m, keys, "tap", by);
                    return Ok(json!({"ok": true}));
                }
                Step::Move(keys) => {
                    p.term.send(Item::keys(keys))?;
                    for _ in 0..(MOVE_MS / 50) {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        if read().is_some_and(|(now, _)| now != mark) {
                            break;
                        }
                    }
                    if read().is_none_or(|(now, _)| now == mark) {
                        break;
                    }
                }
            }
        }
        self.hooks_trust.reopen(pane);
        Err(invalid("the program's marker did not move to \"Trust all and continue\", so nothing was confirmed: answer it in the terminal"))
    }
}

enum Act {
    /// The screen is the review; nothing to do yet.
    Wait,
    /// The key that moves the marker towards the trusting choice.
    Keys(&'static [u8]),
    /// The key that confirms it.
    Answer(&'static [u8], Choice),
    Say(Asks),
}
