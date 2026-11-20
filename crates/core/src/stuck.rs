//! Stuck detection: when a pane is said to need the owner because its
//! agent may be going round in circles, and when it is not.
//!
//! Two kinds. **Idle turns**: so many turns in a row each ran five or more
//! tools and changed no file. **The same failure**: one command failed so
//! many times in a row within a turn. Both are a guess, and say so ("may
//! be stuck"): nothing is asked of the owner at a prompt, so the Control
//! Deck shows a quiet notice and never the terminal's rows and keys.
//!
//! The owner's report of 20 Nov 2026: an agent that watched long renders
//! (a `tail` or two each turn, no file changed) was said to be "stuck: 8
//! turns in a row", in a card that could not be dismissed and came back
//! after every turn. Since then:
//!
//! - **the owner speaking starts the count again**: a prompt that follows
//!   the owner's own typing into the pane (hooks.rs, `owner_spoke`);
//! - **a turn that waits on background work is not counted**: a sub-agent,
//!   a command left running, a helper pane;
//! - **only a turn that is itself counted raises the reason** (before, any
//!   turn's end raised it again once the count was reached);
//! - **the owner can say it is fine** (`pane.stuck_fine`): the reason goes
//!   at once, and that kind stays quiet for the pane until its session
//!   changes or a file is changed.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::{invalid, Daemon, PaneMeta, Result, TurnStats};
use spandeck_agents::AgentState;

/// A turn with this many tool calls and no file changed is an idle turn.
pub const BUSY_TOOL_CALLS: u32 = 5;

const PREFIX: &str = "may be stuck: ";
/// What a station from before 20 Nov 2026 wrote, and a pane's record may
/// still hold.
const OLD_PREFIX: &str = "stuck: ";
const FAILED: &str = "the same command failed ";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    IdleTurns,
    SameFailure,
}

/// What the owner said is fine for this pane: kept with the pane, so it
/// holds through a restart of the station.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fine {
    /// Turns that use tools and change no files are fine.
    #[serde(default)]
    pub idle: bool,
    /// This command failing again and again is fine.
    #[serde(default)]
    pub failure: Option<String>,
}

impl Fine {
    /// The pane's session changed, or a file was changed: whatever was
    /// said to be fine is watched for again.
    pub fn forget(&mut self) {
        *self = Fine::default();
    }
}

pub fn idle_reason(turns: u32) -> String {
    format!(
        "{PREFIX}{turns} turns in a row ran {BUSY_TOOL_CALLS} or more tools each and changed no files"
    )
}

pub fn failure_reason(times: u32, command: &str) -> String {
    format!(
        "{PREFIX}{FAILED}{times} times in a row ({})",
        crate::hooks::truncate(command, 80)
    )
}

/// What was counted, without "may be stuck": the words of the notice.
fn counted(reason: &str) -> Option<&str> {
    reason
        .strip_prefix(PREFIX)
        .or_else(|| reason.strip_prefix(OLD_PREFIX))
}

/// Is this reason one of stuck detection's, and which?
pub fn kind_of(reason: &str) -> Option<Kind> {
    let what = counted(reason)?;
    if what.starts_with(FAILED) {
        Some(Kind::SameFailure)
    } else if what.contains("turns in a row") {
        Some(Kind::IdleTurns)
    } else {
        None
    }
}

/// The kind the pane is said to be stuck by right now, if it is.
pub fn now(meta: &PaneMeta) -> Option<Kind> {
    if meta.state != AgentState::NeedsYou || meta.exit.is_some() {
        return None;
    }
    meta.needs_reason.as_deref().and_then(kind_of)
}

/// `stuck` in a pane's view and in its conversation's `live`: `null`, or
/// `{kind, says}`. A deck shows a notice with "It is fine" for it, and
/// never the terminal's rows and keys.
pub fn view(meta: &PaneMeta) -> Value {
    match now(meta) {
        Some(kind) => json!({
            "kind": kind,
            "says": meta.needs_reason.as_deref().and_then(counted),
        }),
        None => Value::Null,
    }
}

/// A turn that used tools and changed no file: one the count is about.
pub fn idle_turn(turn: &TurnStats) -> bool {
    turn.tool_calls >= BUSY_TOOL_CALLS && turn.file_changes == 0
}

/// A turn begins. `owner_spoke`: its prompt follows the owner's own typing
/// into the pane, so the agent is being steered and the count starts
/// again. A nudge for mail and what another agent sent leave it as it is.
pub fn turn_began(turn: &TurnStats, owner_spoke: bool, now_ms: u64) -> TurnStats {
    TurnStats {
        started_ms: now_ms,
        idle_turns: if owner_spoke { 0 } else { turn.idle_turns },
        ..Default::default()
    }
}

/// A turn ended. The reason to raise, if this turn is the one that brings
/// the count to `threshold` or keeps it there. `background`: the agent has
/// work running beside its turn, which a turn that changes nothing may
/// well be waiting for: such a turn is neither counted nor a new start.
pub fn turn_ended(
    turn: &mut TurnStats,
    fine: &Fine,
    background: bool,
    threshold: u32,
) -> Option<String> {
    if turn.file_changes > 0 {
        turn.idle_turns = 0;
        return None;
    }
    if !idle_turn(turn) || background {
        return None;
    }
    turn.idle_turns += 1;
    (!fine.idle && turn.idle_turns >= threshold).then(|| idle_reason(turn.idle_turns))
}

/// A command failed, and `turn` has counted it. The reason to raise.
pub fn command_failed(turn: &TurnStats, fine: &Fine, threshold: u32) -> Option<String> {
    let command = turn.fail_cmd.as_deref()?;
    (fine.failure.as_deref() != Some(command) && turn.fail_streak >= threshold)
        .then(|| failure_reason(turn.fail_streak, command))
}

impl Daemon {
    /// The owner typed, pasted or sent this into the pane: the prompt
    /// that follows is the owner speaking.
    pub(crate) fn owner_spoke(&self, id: &str) {
        if let Ok(p) = self.pane(id) {
            p.wake.typed_by(true);
        }
    }

    /// Has the pane's agent work running beside its turn that spandeck
    /// knows of: a sub-agent its hooks named, or what its conversation
    /// lists as running (a sub-agent or a command left in the background,
    /// from its session file; a helper pane it started)?
    pub(crate) fn background_work(&self, p: &crate::daemon::PaneEntry, meta: &PaneMeta) -> bool {
        !meta.subagents.is_empty()
            || self
                .conversation_running(p, meta)
                .as_array()
                .is_some_and(|r| !r.is_empty())
    }

    /// `pane.stuck_fine {pane}`: the owner says the pane is not stuck.
    /// The reason goes at once (it is the station's state, so on every
    /// deck), and that kind of it stays quiet for this pane until its
    /// session changes or a file is changed. The owner's alone.
    pub fn stuck_fine(&self, id: &str, by: &str) -> Result<Value> {
        let p = self.pane(id)?;
        let (kind, meta) = {
            let mut m = p.meta.lock().unwrap();
            if m.exit.is_some() {
                return Err(invalid("that pane's program has ended"));
            }
            let Some(kind) = now(&m) else {
                // Said twice, or the pane has moved on: nothing to do.
                return Ok(json!({"ok": true, "pane": p.id, "dismissed": false}));
            };
            match kind {
                Kind::IdleTurns => m.stuck_fine.idle = true,
                Kind::SameFailure => m.stuck_fine.failure = m.turn.fail_cmd.clone(),
            }
            (kind, m.clone())
        };
        // Idle turns are found at a turn's end, the same failure within
        // a turn: the state the pane would have had.
        let under = match kind {
            Kind::IdleTurns => AgentState::Done,
            Kind::SameFailure => AgentState::Working,
        };
        self.set_state(&p, under, None);
        self.save_pane(&p);
        self.record(
            "pane.stuck_fine",
            Some(&p.id),
            meta.project.as_deref(),
            meta.task.as_deref(),
            Some(by),
            json!({"kind": kind, "reason": meta.needs_reason}),
        );
        self.emit("pane.updated", Some(&p.id), None, json!({}));
        Ok(json!({"ok": true, "pane": p.id, "dismissed": true, "kind": kind}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(tool_calls: u32, file_changes: u32, idle_turns: u32) -> TurnStats {
        TurnStats {
            tool_calls,
            file_changes,
            idle_turns,
            ..Default::default()
        }
    }

    /// The fault of 20 Nov 2026, as a rule: which turns are counted, what
    /// starts the count again, and when the reason is raised.
    #[test]
    fn which_turns_are_counted_and_what_starts_the_count_again() {
        let fine = Fine::default();
        // Two idle turns, then the third raises it.
        let mut t = turn(5, 0, 0);
        assert_eq!(turn_ended(&mut t, &fine, false, 3), None);
        let mut t = turn(9, 0, turn_began(&t, false, 1).idle_turns);
        assert_eq!(turn_ended(&mut t, &fine, false, 3), None);
        let mut t = turn(5, 0, turn_began(&t, false, 2).idle_turns);
        let reason = turn_ended(&mut t, &fine, false, 3).unwrap();
        assert_eq!(
            reason,
            "may be stuck: 3 turns in a row ran 5 or more tools each and changed no files"
        );
        assert_eq!(kind_of(&reason), Some(Kind::IdleTurns));
        // A turn of a few tools after that is not counted and raises
        // nothing: what the owner met came back after every turn.
        let mut few = turn(2, 0, 3);
        assert_eq!(turn_ended(&mut few, &fine, false, 3), None);
        assert_eq!(few.idle_turns, 3);
        // A turn that waits on background work: not counted, no new start.
        let mut waits = turn(7, 0, 2);
        assert_eq!(turn_ended(&mut waits, &fine, true, 3), None);
        assert_eq!(waits.idle_turns, 2);
        // The owner spoke: from nothing. A nudge or another agent: not.
        assert_eq!(turn_began(&t, true, 9).idle_turns, 0);
        assert_eq!(turn_began(&t, false, 9).idle_turns, 3);
        // A file changed: from nothing.
        let mut wrote = turn(6, 1, 3);
        assert_eq!(turn_ended(&mut wrote, &fine, false, 3), None);
        assert_eq!(wrote.idle_turns, 0);
        // "It is fine": counted still, and quiet.
        let quiet = Fine {
            idle: true,
            failure: None,
        };
        let mut t = turn(5, 0, 3);
        assert_eq!(turn_ended(&mut t, &quiet, false, 3), None);
        assert_eq!(t.idle_turns, 4);
    }

    #[test]
    fn the_same_failure_is_quiet_only_for_the_command_said_to_be_fine() {
        let t = TurnStats {
            fail_cmd: Some("make build".into()),
            fail_streak: 3,
            ..Default::default()
        };
        let reason = command_failed(&t, &Fine::default(), 3).unwrap();
        assert_eq!(
            reason,
            "may be stuck: the same command failed 3 times in a row (make build)"
        );
        assert_eq!(kind_of(&reason), Some(Kind::SameFailure));
        let fine = |c: &str| Fine {
            idle: false,
            failure: Some(c.into()),
        };
        assert_eq!(command_failed(&t, &fine("make build"), 3), None);
        assert!(command_failed(&t, &fine("make test"), 3).is_some());
        // What a station from before wrote is still known for what it is,
        // and no other reason is taken for one of these.
        assert_eq!(
            kind_of("stuck: 8 turns in a row used tools but changed no files"),
            Some(Kind::IdleTurns)
        );
        assert_eq!(kind_of("wants to run Bash: make"), None);
        assert_eq!(kind_of("may be stuck: something else"), None);
    }
}
