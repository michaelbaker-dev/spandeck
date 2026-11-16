//! An idle agent is woken when its mail arrives, not at the next look.
//!
//! The typed nudge used to come only from a look made once a second
//! (`pump_mail`), so a message waited half a second at the median before
//! its agent was told (measured: 515 ms on one station, 1126 ms across
//! two). Now the pane a message is for is looked at the moment the message
//! is stored, and again the moment it will have been idle long enough. The
//! look once a second stays: it repeats a nudge that was ignored and
//! catches a wake that was missed.
//!
//! Every road does the same things in the same order, for one pane at a
//! time (`PaneWake::turn`): the marks are read, the nudge goes into the
//! pane's input only if they still allow it with the input locked
//! (terminal/input.rs, `send_if`), and only a nudge that went in announces
//! anything. So a wake cannot pass a Pause, a Stop or a restart's hold, and
//! two roads cannot both type.

use std::sync::Mutex;

use spandeck_agents::AgentState;

use crate::daemon::PaneMeta;
use crate::holds::InForce;
use crate::relay::MessageStore;

/// The switch (`[flags] mail_wake = false`, or `station.flags`): off, an
/// agent is told of its mail by the look once a second alone, as before.
pub const SWITCH: &str = "mail_wake";

/// An agent is nudged only when it has been idle for longer than this.
pub const IDLE_MS: u64 = 1200;

/// After a nudge, a pane whose state has not changed is not typed into
/// again for this long: the time the look once a second always left an
/// agent to take the first one. Mail that arrives meanwhile is announced
/// by the agent's own prompt hook, or by the next look.
pub const NUDGE_GAP_MS: u64 = 1000;

/// What a look at one pane decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// Not an agent that can be woken: ended, held, paused, or in a turn.
    No,
    /// It may be typed into now.
    Now,
    /// It may be at this time (the station's clock, ms), if nothing changes.
    At(u64),
}

/// What the decision rests on, read from the pane's record in one look.
#[derive(Debug, Clone, Copy)]
pub struct Seen {
    pub agent: bool,
    pub exited: bool,
    /// Any mark that keeps mail back: Pause, a stop's, a restart's.
    pub held: bool,
    pub state: AgentState,
    pub state_since_ms: u64,
}

impl Seen {
    pub fn of(m: &PaneMeta, in_force: InForce) -> Seen {
        Seen {
            agent: m.agent.is_agent(),
            exited: m.exit.is_some(),
            held: in_force.no_mail(m),
            state: m.state,
            state_since_ms: m.state_since_ms,
        }
    }
}

/// May this pane be typed a nudge, and when? `last_nudge_ms`: when it was
/// last typed one.
pub fn decide(seen: &Seen, last_nudge_ms: Option<u64>, now: u64) -> Wake {
    if !seen.agent
        || seen.exited
        || seen.held
        || !matches!(seen.state, AgentState::Idle | AgentState::Done)
    {
        return Wake::No;
    }
    let mut at = seen.state_since_ms + IDLE_MS + 1;
    if let Some(nudged) = last_nudge_ms.filter(|n| *n >= seen.state_since_ms) {
        at = at.max(nudged + NUDGE_GAP_MS);
    }
    if now >= at {
        Wake::Now
    } else {
        Wake::At(at)
    }
}

/// A pane's own part in this: whose turn it is to look at its mail, when
/// it was last nudged, and the look that is already set for later.
#[derive(Default)]
pub struct PaneWake {
    /// Held for the whole of one look (read, type, mark announced), by
    /// whichever road makes it.
    pub(crate) turn: Mutex<Turn>,
    /// The earliest later look that is set, so that a burst of mail for a
    /// pane that is not yet idle long enough sets one, not one each.
    pub(crate) timer_at: Mutex<Option<u64>>,
}

#[derive(Default)]
pub struct Turn {
    pub last_nudge_ms: Option<u64>,
    /// The nudge last typed into this pane, while nothing shows that the
    /// agent took it.
    pub typed: Option<Typed>,
}

/// A nudge as it was typed: when, and the pane's `state_since_ms` at that
/// moment. An agent that takes the nudge starts a turn, which changes its
/// state; one whose state has not changed since has not been seen to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Typed {
    pub at_ms: u64,
    pub state_since_ms: u64,
    /// Enter was pressed once more for it.
    pub entered_again: bool,
}

/// A nudge whose Enter the program did not take is given one more Enter
/// this long after it was typed (found 16 Nov 2026: Codex left the nudge
/// in its input box once in five, and it sat there for the full minute
/// until the next nudge).
pub const ENTER_AGAIN_MS: u64 = 2000;

/// How many of the bottom lines of a pane's screen an agent's input box
/// is looked for in.
const BOX_LINES: usize = 10;

/// Is the typed nudge on the pane's screen, near the bottom, where an
/// agent's input box is? `mark`: what every nudge has in it.
pub fn on_screen(lines: &[String], mark: &str) -> bool {
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(0);
    lines[end.saturating_sub(BOX_LINES)..end]
        .iter()
        .any(|l| l.contains(mark))
}

/// Is the nudge last typed still sitting in the program's input, unsent?
/// It is taken to be when the pane has not changed state since it was
/// typed (no turn began) and its text is still on the screen. Then a
/// second copy is never typed on top of it (found 16 Nov 2026: OpenCode's
/// input box held five nudges after five minutes): Enter is pressed.
pub fn unsent(typed: Option<&Typed>, state_since_ms: u64, shown: bool) -> bool {
    shown && typed.is_some_and(|t| t.state_since_ms == state_since_ms)
}

/// Is it time for the one more Enter?
pub fn enter_again(typed: Option<&Typed>, state_since_ms: u64, shown: bool, now: u64) -> bool {
    unsent(typed, state_since_ms, shown)
        && typed.is_some_and(|t| !t.entered_again && now >= t.at_ms + ENTER_AGAIN_MS)
}

/// One look at one pane's mail, with its turn held: if mail waits that the
/// agent was not told of (or was told of before `told_before_ms` and has
/// not read), `type_it` is asked to type the nudge, with how many
/// messages there are and who sent them (never what they say). Only when
/// it says it did is anything announced: `told`
/// is given the messages' ids first (for the Performance page), then they
/// are marked. Returns how many were announced.
pub fn announce(
    mail: &MessageStore,
    names: &[String],
    now: u64,
    told_before_ms: u64,
    turn: &mut Turn,
    type_it: impl FnOnce(usize, &[String]) -> bool,
    told: impl FnOnce(&[String]),
) -> usize {
    let waiting = mail.unannounced(names, told_before_ms);
    if waiting.is_empty() {
        return 0;
    }
    let senders: Vec<String> = waiting.iter().map(|w| w.1.clone()).collect();
    if !type_it(waiting.len(), &senders) {
        return 0;
    }
    turn.last_nudge_ms = Some(now);
    let ids: Vec<String> = waiting.iter().map(|w| w.0.clone()).collect();
    told(&ids);
    mail.mark_announced(&ids, now);
    ids.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::relay::NewMessage;
    use serde_json::json;
    use std::sync::Arc;

    fn idle_since(ms: u64) -> Seen {
        Seen {
            agent: true,
            exited: false,
            held: false,
            state: AgentState::Idle,
            state_since_ms: ms,
        }
    }

    /// DECISIONS.md: "A typed one-line nudge only wakes an idle agent";
    /// Guarantees, Stop, pause and offline: "no mail reaches a paused agent
    /// by any road", "a stopped agent stays stopped". The wake on arrival
    /// is a new road, and decides by the same rule as the look once a
    /// second: never a pane that is held, in a turn, ended or not an
    /// agent, and never one idle for 1200 ms or less.
    #[test]
    fn only_an_agent_that_nothing_holds_and_that_has_been_idle_long_enough_is_woken() {
        let now = 100_000;
        assert_eq!(decide(&idle_since(now - 1201), None, now), Wake::Now);
        // Idle for 300 ms: at 1201 ms, not now and not never.
        assert_eq!(
            decide(&idle_since(now - 300), None, now),
            Wake::At(now + 901)
        );
        assert_eq!(
            decide(&idle_since(now - 1200), None, now),
            Wake::At(now + 1)
        );
        let done = Seen {
            state: AgentState::Done,
            ..idle_since(now - 5000)
        };
        assert_eq!(decide(&done, None, now), Wake::Now);
        for state in [AgentState::Working, AgentState::NeedsYou, AgentState::Error] {
            let busy = Seen {
                state,
                ..idle_since(now - 5000)
            };
            assert_eq!(decide(&busy, None, now), Wake::No, "{state:?}");
        }
        let long_idle = idle_since(now - 5000);
        for no in [
            Seen {
                held: true,
                ..long_idle
            },
            Seen {
                exited: true,
                ..long_idle
            },
            Seen {
                agent: false,
                ..long_idle
            },
        ] {
            assert_eq!(decide(&no, None, now), Wake::No, "{no:?}");
        }
        // Each mark holds by itself, through the same rule the pump uses.
        let mut m: PaneMeta = serde_json::from_value(json!({
            "id": "p1", "name": "builder", "agent": "claude",
            "spec": {"program": "claude", "args": [], "env": [], "cwd": null, "cols": 80, "rows": 24},
            "cwd": "/", "created_ms": 1, "state": "idle", "state_since_ms": 1,
        }))
        .unwrap();
        let all = InForce {
            stopped: true,
            restored: true,
        };
        assert_eq!(decide(&Seen::of(&m, all), None, now), Wake::Now);
        m.paused = true;
        assert_eq!(decide(&Seen::of(&m, all), None, now), Wake::No);
        m.paused = false;
        m.held_stopped = Some(crate::holds::HeldStopped {
            at_ms: 5,
            by: "you".into(),
            reached: true,
        });
        assert_eq!(decide(&Seen::of(&m, all), None, now), Wake::No);
        m.held_stopped = None;
        m.held_restored = Some(crate::holds::HeldRestored {
            at_ms: 5,
            why: "crash".into(),
        });
        assert_eq!(decide(&Seen::of(&m, all), None, now), Wake::No);
    }

    /// The look once a second never typed two nudges into a pane within a
    /// second. Waking on arrival must not either: after a nudge, a pane
    /// whose state has not changed waits out the gap; one that took the
    /// nudge, worked and is idle again is judged by its idle time alone.
    #[test]
    fn a_pane_just_nudged_is_not_typed_into_again_at_once() {
        let now = 100_000;
        let seen = idle_since(now - 5000);
        assert_eq!(
            decide(&seen, Some(now - 10), now),
            Wake::At(now - 10 + NUDGE_GAP_MS)
        );
        assert_eq!(decide(&seen, Some(now - NUDGE_GAP_MS), now), Wake::Now);
        // Nudged, then a turn, then idle again: the old nudge counts for nothing.
        assert_eq!(
            decide(&idle_since(now - 2000), Some(now - 2500), now),
            Wake::Now
        );
    }

    /// Found with real agents on 16 Nov 2026. Codex did not take the
    /// nudge's Enter once in five: the line sat in its input box for a
    /// minute. OpenCode, which could not take a turn, had five copies of
    /// the nudge in its input box after five minutes. So: a nudge that is
    /// still on the screen of a pane that has not changed state since is
    /// unsent; it gets one more Enter after two seconds, once; and while
    /// it is unsent no second copy is typed.
    #[test]
    fn an_unsent_nudge_gets_one_more_enter_and_never_a_second_copy() {
        let mark = "new spandeck message";
        let screen = |text: &str| -> Vec<String> { text.lines().map(String::from).collect() };
        let in_box = screen("codex\n\n> You have 1 new spandeck message from a@x. Read it\n  with the spandeck inbox tool\n\n  ? for shortcuts\n\n\n");
        assert!(on_screen(&in_box, mark));
        assert!(!on_screen(&screen("> \n"), mark));
        // Scrolled far up the screen, it is history, not the input box.
        let mut old = vec!["> You have 1 new spandeck message from a@x.".to_string()];
        old.extend((0..12).map(|n| format!("line {n}")));
        assert!(!on_screen(&old, mark));

        let typed = Typed {
            at_ms: 10_000,
            state_since_ms: 4_000,
            entered_again: false,
        };
        // Nothing typed, or a turn since (the state changed), or no longer
        // on the screen: not unsent.
        assert!(!unsent(None, 4_000, true));
        assert!(!unsent(Some(&typed), 10_050, true));
        assert!(!unsent(Some(&typed), 4_000, false));
        assert!(unsent(Some(&typed), 4_000, true));
        // One more Enter: not before two seconds, and once.
        assert!(!enter_again(Some(&typed), 4_000, true, 11_999));
        assert!(enter_again(Some(&typed), 4_000, true, 12_000));
        let again = Typed {
            entered_again: true,
            ..typed
        };
        assert!(!enter_again(Some(&again), 4_000, true, 60_000));
        assert!(unsent(Some(&again), 4_000, true), "and still no second copy");
    }

    fn store() -> MessageStore {
        MessageStore::new(Arc::new(Db::memory().unwrap()), "alpha")
    }

    fn send(mail: &MessageStore, n: usize, now: u64) {
        mail.send(
            NewMessage {
                sender: "you".into(),
                recipient: "reader".into(),
                kind: "note".into(),
                body: format!("message {n}"),
                refs: json!([]),
                thread: None,
                task: None,
                project: None,
                auth: None,
            },
            now,
        )
        .unwrap();
    }

    /// 50 messages stored at once, each one waking its pane: one nudge is
    /// typed, and it announces all of them (a nudge per message would be
    /// 50 prompts for a paid agent). A nudge that was not typed, because a
    /// hold was set between the look and the typing, announces nothing:
    /// "A nudge that is not typed announces nothing" (DECISIONS.md, A
    /// stopped agent stays stopped). Mail not read is announced again only
    /// after the repeat time.
    #[test]
    fn many_messages_at_once_are_one_nudge_and_a_nudge_not_typed_announces_nothing() {
        let mail = store();
        let names = vec!["reader".to_string()];
        let mut turn = Turn::default();
        let now = 50_000;
        for n in 0..50 {
            send(&mail, n, now);
        }
        // The hold landed first: nothing is typed, and nothing is announced.
        let told = announce(&mail, &names, now, 0, &mut turn, |_, _| false, |_| {});
        assert_eq!((told, turn.last_nudge_ms), (0, None));
        assert_eq!(mail.unannounced(&names, 0).len(), 50);

        let mut typed = 0;
        let mut counted = 0;
        for _ in 0..50 {
            announce(
                &mail,
                &names,
                now,
                0,
                &mut turn,
                |count, senders| {
                    assert_eq!(count, 50);
                    assert_eq!(senders[0], "you@alpha");
                    typed += 1;
                    true
                },
                |ids| counted += ids.len(),
            );
        }
        assert_eq!(
            (typed, counted),
            (1, 50),
            "one nudge, for all fifty messages"
        );
        assert_eq!(turn.last_nudge_ms, Some(now));
        // Still unread a minute later: told again, once.
        let later = now + 61_000;
        let mut again = 0;
        for _ in 0..2 {
            announce(
                &mail,
                &names,
                later,
                later - 60_000,
                &mut turn,
                |_, _| {
                    again += 1;
                    true
                },
                |_| {},
            );
        }
        assert_eq!(again, 1);
    }
}
