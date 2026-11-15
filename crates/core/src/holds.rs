//! Holds: a stopped or restarted agent stays put until the owner releases
//! it (DECISIONS.md, "A stopped agent stays stopped").
//!
//! A pane carries three marks, each on its own, in its record:
//!
//! - `paused`: the owner's Pause. Set and cleared by Pause only.
//! - `held_stopped`: set by a stop of the pane.
//! - `held_restored`: set when the pane was started again after the station
//!   restarted.
//!
//! A pane with any of them is given no mail in any form: no typed nudge,
//! no notice in a hook's answer, no turn continued at its end. The mail
//! stays unread and unannounced, and is announced when the last mark goes.
//! Clearing one mark never clears another.
//!
//! `held_stopped` and `held_restored` are released together and only by
//! the owner: typing or sending anything into the pane, Release, Release
//! all, `spandeck pane release`. Looking at a pane never releases it. No
//! agent and no other station has a call that releases.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spandeck_terminal::{Discarded, InputKind, Mark};

use crate::daemon::{Daemon, PaneEntry, PaneMeta, Result};
use crate::paths::now_ms;

/// Said in `caps`: this station holds stopped and restarted agents, and
/// has `pane.release`.
pub const CAP: &str = "holds";

/// What a stopped pane says when its program could not be given the key.
pub const NOT_READING: &str =
    "this program is not reading input: Stop could not reach it; use Kill";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeldStopped {
    pub at_ms: u64,
    /// Who stopped it, as the ship's log names them.
    pub by: String,
    /// False: the program was not reading, and the stop did not reach it.
    #[serde(default = "yes")]
    pub reached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeldRestored {
    pub at_ms: u64,
    /// `crash`, `stop` or `upgrade`: why the station started it again.
    pub why: String,
}

fn yes() -> bool {
    true
}

impl PaneMeta {
    /// The owner's release: the stop's mark and the restart's go, together.
    /// Pause is not a hold that Release ends: only Resume clears it. True
    /// when there was something to release.
    pub fn release(&mut self) -> bool {
        self.held_stopped.take().is_some() | self.held_restored.take().is_some()
    }
}

/// Which marks count on this station (`[flags]`, `station.flags`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InForce {
    pub stopped: bool,
    pub restored: bool,
}

impl InForce {
    /// Does anything keep mail from this pane? Pause always counts.
    pub fn no_mail(&self, m: &PaneMeta) -> bool {
        m.paused
            || (self.stopped && m.held_stopped.is_some())
            || (self.restored && m.held_restored.is_some())
    }
}

/// A stop's place in the input of each pane it may stop, taken the moment
/// the stop is read and before it runs: what is typed after that is kept
/// (terminal/input.rs). A pane with no entry is marked when its stop runs.
pub type StopMarks = std::collections::HashMap<String, Mark>;

/// Is this something a person typed, or only what a terminal says by
/// itself? A Control Deck's terminal answers the program's questions
/// (where the cursor is, what kind of terminal this is, its colours) and
/// reports the window gaining or losing focus and the mouse moving, all as
/// input. None of that is the owner typing: a pane is never released by
/// being looked at, clicked into or reconnected to.
pub fn is_typing(data: &[u8]) -> bool {
    let mut rest = data;
    while !rest.is_empty() {
        match report_len(rest) {
            Some(n) => rest = &rest[n..],
            None => return true,
        }
    }
    false
}

/// The length of the terminal report `b` begins with, if it begins with one.
fn report_len(b: &[u8]) -> Option<usize> {
    if b.len() < 3 || b[0] != 0x1b {
        return None;
    }
    match b[1] {
        b'[' => {
            // The old mouse report: three bytes follow, any bytes.
            if b[2] == b'M' {
                return (b.len() >= 6).then_some(6);
            }
            let end = b[2..].iter().position(|c| (0x40..=0x7e).contains(c))? + 2;
            let params = &b[2..end];
            let plain = params.iter().all(|c| c.is_ascii_digit() || *c == b';');
            let report = match b[end] {
                // Focus gained and lost.
                b'I' | b'O' => params.is_empty(),
                // Where the cursor is; a status report.
                b'R' | b'n' => !params.is_empty() && (plain || params[0] == b'?'),
                // What kind of terminal this is.
                b'c' => matches!(params.first(), Some(b'?' | b'>')),
                // The window's size.
                b't' => !params.is_empty() && plain,
                // A setting's state.
                b'y' => params.first() == Some(&b'?'),
                // The mouse, in the newer form.
                b'M' | b'm' => params.first() == Some(&b'<'),
                _ => false,
            };
            report.then_some(end + 1)
        }
        // A colour, or a setting's text: up to BEL or ESC \.
        b']' | b'P' => {
            let body = &b[2..];
            let bel = body.iter().position(|c| *c == 0x07).map(|i| i + 3);
            let st = body.windows(2).position(|w| w == b"\x1b\\").map(|i| i + 4);
            match (bel, st) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        _ => None,
    }
}

impl Daemon {
    /// Which marks count here. `holds = false` turns both off, and mail is
    /// held back by Pause alone, as before this.
    pub fn holds_in_force(&self) -> InForce {
        let all = self.flag_on("holds", true);
        InForce {
            stopped: all && self.flag_on("holds.stopped", true),
            restored: all && self.flag_on("holds.restored", true),
        }
    }

    /// Does anything keep mail from this pane now?
    pub(crate) fn no_mail(&self, m: &PaneMeta) -> bool {
        self.holds_in_force().no_mail(m)
    }

    /// `held` in a pane's view: the marks that count, and how much mail
    /// waits behind them. Null for a pane that is not held.
    pub(crate) fn held_view(&self, m: &PaneMeta) -> Value {
        let on = self.holds_in_force();
        let stopped = m.held_stopped.as_ref().filter(|_| on.stopped);
        let restored = m.held_restored.as_ref().filter(|_| on.restored);
        if stopped.is_none() && restored.is_none() {
            return Value::Null;
        }
        let names = {
            let mut names = vec![m.id.clone(), m.name.clone()];
            if let Some(project) = &m.project {
                names.push(format!("{project}/{}", m.name));
            }
            names
        };
        let mail = self.mail().unannounced(&names, i64::MAX as u64).len();
        json!({"stopped": stopped, "restored": restored, "mail": mail})
    }

    /// A held pane in words, for a Control Deck from before holds, which
    /// shows a pane's `needs_reason` and has no Release button: typing
    /// into the pane releases it from any deck. Times are the station's,
    /// in UTC.
    pub(crate) fn held_words(&self, m: &PaneMeta) -> Option<String> {
        let on = self.holds_in_force();
        let stopped = m.held_stopped.as_ref().filter(|_| on.stopped);
        let restored = m.held_restored.as_ref().filter(|_| on.restored);
        let what = match (stopped, restored) {
            (Some(s), _) if !s.reached => NOT_READING.to_string(),
            (Some(s), _) => format!(
                "stopped by {} at {}",
                s.by,
                crate::restore::clock_utc(s.at_ms)
            ),
            (None, Some(r)) => format!(
                "started again after a restart at {}",
                crate::restore::clock_utc(r.at_ms)
            ),
            (None, None) => return None,
        };
        Some(format!(
            "{what}: it takes no mail until you type into it or release it"
        ))
    }

    /// Set the stop's mark on an agent's pane, when that mark is in force.
    /// A nudge that was on its way and not yet typed is taken back.
    pub(crate) fn hold_stopped(&self, p: &PaneEntry, by: &str) -> bool {
        if !self.holds_in_force().stopped {
            return false;
        }
        {
            let mut m = p.meta.lock().unwrap();
            if !m.agent.is_agent() || m.exit.is_some() {
                return false;
            }
            m.held_stopped = Some(HeldStopped {
                at_ms: now_ms(),
                by: by.chars().take(120).collect(),
                reached: true,
            });
        }
        p.term.withdraw(InputKind::Nudge);
        self.save_pane(p);
        true
    }

    /// The owner releases a pane: `how` is `release`, `release_all` or
    /// `typed`. Returns whether there was anything to release.
    pub fn release(&self, id: &str, by: &str, how: &str) -> Result<bool> {
        let p = self.pane(id)?;
        let (was, meta) = {
            let mut m = p.meta.lock().unwrap();
            let was = (m.held_stopped.is_some(), m.held_restored.is_some());
            (was, m.release().then(|| m.clone()))
        };
        let Some(meta) = meta else {
            return Ok(false);
        };
        self.save_pane(&p);
        self.record(
            "pane.released",
            Some(&p.id),
            meta.project.as_deref(),
            meta.task.as_deref(),
            Some(by),
            json!({"how": how, "stopped": was.0, "restored": was.1, "paused": meta.paused}),
        );
        self.emit("pane.updated", Some(&p.id), None, json!({}));
        Ok(true)
    }

    /// Release every pane on this station. Pause is left as it is on each.
    pub fn release_all(&self, by: &str) -> Value {
        let released: Vec<String> = self
            .list_panes()
            .iter()
            .filter(|p| self.release(&p.id, by, "release_all").unwrap_or(false))
            .map(|p| p.id.clone())
            .collect();
        let still_paused = self
            .list_panes()
            .iter()
            .filter(|p| {
                let m = p.meta.lock().unwrap();
                m.paused && released.contains(&m.id)
            })
            .count();
        self.record(
            "station.released_all",
            None,
            None,
            None,
            Some(by),
            json!({"panes": released.len()}),
        );
        // Each record is on its way to the database; the answer says
        // whether all of them are there (a second at most is waited).
        let saved = self.db.saved_within(
            &released
                .iter()
                .map(|id| format!("pane:{id}"))
                .collect::<Vec<_>>(),
            crate::db::ACK_WAIT,
        );
        json!({
            "station": self.station, "released": released.len(), "panes": released,
            "still_paused": still_paused, "saved": saved,
        })
    }

    /// The owner typed or sent something into a pane: that releases it,
    /// before what was typed is queued.
    pub(crate) fn release_by_typing(&self, id: &str, by: &str) {
        let held = self
            .pane(id)
            .map(|p| {
                let m = p.meta.lock().unwrap();
                m.held_stopped.is_some() || m.held_restored.is_some()
            })
            .unwrap_or(false);
        if held {
            let _ = self.release(id, by, "typed");
        }
    }

    /// A stop's place in the input of every agent's pane here, for a stop
    /// that may stop any of them. Waits for nothing.
    pub fn stop_marks(&self) -> StopMarks {
        let mut marks = StopMarks::new();
        self.stop_marks_into(&mut marks);
        marks
    }

    /// `stop_marks`, for the panes `marks` has no place for yet.
    pub fn stop_marks_into(&self, marks: &mut StopMarks) {
        self.stop_marks_unless(marks, |_| false)
    }

    /// `stop_marks_into`, passing over the panes `skip` names: a connection
    /// that still holds something sent to a pane before the stop takes that
    /// pane's place when its turn comes, not ahead of what came first.
    pub fn stop_marks_unless(&self, marks: &mut StopMarks, skip: impl Fn(&str) -> bool) {
        for p in self.list_panes() {
            let agent = {
                let m = p.meta.lock().unwrap();
                m.exit.is_none() && m.agent.is_agent()
            };
            if agent && !marks.contains_key(&p.id) && !skip(&p.id) {
                marks.insert(p.id.clone(), p.term.mark_stop());
            }
        }
    }

    /// Does a "stop every agent" with this scope stop agents here?
    pub fn stop_is_for_here(&self, scope: &str) -> bool {
        matches!(scope, "station" | "" | "fleet") || scope == self.station
    }

    /// Tell whoever sent what a stop threw away: an event that names the
    /// pane and, per client, how many things it sent were not delivered.
    /// Never what they were.
    pub(crate) fn tell_discarded(&self, pane: &str, discarded: &[Discarded]) {
        if discarded.is_empty() {
            return;
        }
        let mut clients = serde_json::Map::new();
        for d in discarded {
            if let Some(from) = &d.from {
                let n = clients.get(from).and_then(|v| v.as_u64()).unwrap_or(0);
                clients.insert(from.clone(), json!(n + 1));
            }
        }
        self.emit(
            "pane.input_discarded",
            Some(pane),
            None,
            json!({"why": "stopped", "count": discarded.len(), "clients": clients}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> PaneMeta {
        serde_json::from_value(json!({
            "id": "p1", "name": "builder", "agent": "claude",
            "spec": {"program": "claude", "args": [], "env": [], "cwd": null, "cols": 80, "rows": 24},
            "cwd": "/", "created_ms": 1, "state": "idle", "state_since_ms": 1,
        }))
        .expect("a pane record from before holds is still read")
    }

    const ALL: InForce = InForce {
        stopped: true,
        restored: true,
    };

    /// Guarantee, Pause: "no new work is delivered". REVIEW2 blocker 3: the
    /// marks are independent. A paused pane that is stopped, restarted and
    /// then released is still paused, and still gets no mail.
    #[test]
    fn releasing_a_pane_never_unpauses_it() {
        let mut m = meta();
        assert!(!ALL.no_mail(&m), "a pane with no mark takes mail");
        m.paused = true;
        m.held_stopped = Some(HeldStopped {
            at_ms: 5,
            by: "you".into(),
            reached: true,
        });
        m.held_restored = Some(HeldRestored {
            at_ms: 9,
            why: "crash".into(),
        });
        assert!(m.release());
        assert!(m.held_stopped.is_none() && m.held_restored.is_none());
        assert!(m.paused, "Release is not Resume");
        assert!(ALL.no_mail(&m));
        assert!(!m.release(), "nothing left to release");
        m.paused = false;
        assert!(!ALL.no_mail(&m));
    }

    /// Each mark holds mail back by itself, a record written with the
    /// marks reads back with them (they are in the pane's record, so they
    /// survive a restart), and a switch turns off its own mark only: never
    /// Pause.
    #[test]
    fn each_mark_holds_mail_by_itself_and_a_switch_never_lifts_pause() {
        let stopped = {
            let mut m = meta();
            m.held_stopped = Some(HeldStopped {
                at_ms: 5,
                by: "you".into(),
                reached: false,
            });
            m
        };
        let restored = {
            let mut m = meta();
            m.held_restored = Some(HeldRestored {
                at_ms: 9,
                why: "crash".into(),
            });
            m
        };
        let paused = {
            let mut m = meta();
            m.paused = true;
            m
        };
        for m in [&stopped, &restored, &paused] {
            assert!(ALL.no_mail(m));
            let back: PaneMeta = serde_json::from_str(&serde_json::to_string(m).unwrap()).unwrap();
            assert_eq!(back.held_stopped, m.held_stopped);
            assert_eq!(back.held_restored, m.held_restored);
            assert_eq!(back.paused, m.paused);
        }
        let off = InForce {
            stopped: false,
            restored: false,
        };
        assert!(!off.no_mail(&stopped) && !off.no_mail(&restored));
        assert!(off.no_mail(&paused));
        let only_stopped = InForce {
            stopped: true,
            restored: false,
        };
        assert!(only_stopped.no_mail(&stopped) && !only_stopped.no_mail(&restored));
    }

    /// REVIEW2 blocker 3: release is explicit, "never by looking". A deck's
    /// terminal sends focus reports, the mouse and its answers to the
    /// program's questions as input whenever a pane is opened or clicked.
    #[test]
    fn what_a_terminal_says_by_itself_is_not_the_owner_typing() {
        for looked in [
            &b"\x1b[I"[..],
            b"\x1b[O",
            b"\x1b[12;40R",
            b"\x1b[?1;2c",
            b"\x1b[>0;276;0c",
            b"\x1b[0n",
            b"\x1b[?2026;2$y",
            b"\x1b[<35;10;4M",
            b"\x1b[<0;10;4m",
            b"\x1b[M !!",
            b"\x1b]11;rgb:0000/0000/0000\x1b\\",
            b"\x1b]10;rgb:ffff/ffff/ffff\x07",
            b"\x1bP1$r0m\x1b\\",
            b"\x1b[I\x1b[12;40R\x1b[O",
        ] {
            assert!(!is_typing(looked), "{:?}", String::from_utf8_lossy(looked));
        }
        for typed in [
            &b"a"[..],
            b"\r",
            b"\x1b",
            b"\x03",
            b"\x1b[A",
            b"\x1b[3~",
            b"\x1b[200~pasted\x1b[201~",
            b"\x1b[Ix",
            b"\x1bOP",
            b"\x1b[1;5C",
        ] {
            assert!(is_typing(typed), "{:?}", String::from_utf8_lossy(typed));
        }
    }
}
