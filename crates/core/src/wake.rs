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
    /// The owner typed into the pane: the count of nudges it did not take
    /// starts again. Set without the turn (typing waits for nothing), and
    /// read at the next look.
    pub(crate) owner_acted: std::sync::atomic::AtomicBool,
    /// When the pane's program last said, by its hook, that its session
    /// has started: it is past whatever it asks before that (0: not yet).
    pub(crate) greeted_ms: std::sync::atomic::AtomicU64,
    /// When a look last found the pane at a screen of its program's that
    /// asks something (the notice of a newer version, the folder-trust
    /// question, the review of hooks, a list to choose from), or one was
    /// answered (0: never).
    pub(crate) screen_ms: std::sync::atomic::AtomicU64,
}

impl PaneWake {
    /// The pane is at a screen that asks something, or one was just
    /// answered: what comes after it is given time to be drawn.
    pub(crate) fn at_screen(&self, now: u64) {
        self.screen_ms
            .fetch_max(now, std::sync::atomic::Ordering::Relaxed);
    }
}

/// How long a program is left alone after it starts before a nudge may be
/// typed into it.
pub const START_MS: u64 = 8_000;

/// How long it is left alone after a screen of its own that asks something
/// was last seen or answered: the next one may be about to be drawn.
pub const AFTER_SCREEN_MS: u64 = 2_500;

/// A screen that changed less than this long ago is still being drawn.
pub const DRAWN_MS: u64 = 1_500;

/// A program that keeps drawing is waited for this long and no longer.
pub const DRAWING_MS: u64 = 30_000;

/// What is known of a program that may still be starting.
#[derive(Debug, Clone, Copy)]
pub struct Starting {
    /// Its hook said its session started: it is past what it asks first.
    pub greeted: bool,
    /// Since the program started.
    pub age_ms: u64,
    /// Since it was last seen at a screen that asks something, or one was
    /// answered.
    pub screen_ago_ms: Option<u64>,
    /// Since its screen last changed.
    pub quiet_ms: u64,
    /// Its screen is empty: it has drawn nothing to type into, and what
    /// is typed now waits in its terminal for whatever it asks first.
    pub blank: bool,
}

/// Is the program still starting, so that nothing is typed into it? What
/// it shows first may be a question of its own, which is known for one
/// only once it is drawn and read, and a nudge's Enter answers a question.
/// So: its first eight seconds; a moment after each such screen, since
/// another may follow it (Codex: the notice of a newer version, then the
/// folder-trust question, then the review of hooks); and, to half a
/// minute from either, while its screen is still being drawn; and for as
/// long as it has drawn nothing at all (a nudge typed at an empty screen
/// was kept by the terminal and answered the question the program drew
/// thirteen seconds later, in a scenario of 19 Nov 2026). A screen
/// that is read as one of those questions is not typed into at all,
/// whatever this says (`Daemon::start_question`).
pub fn starting(s: Starting) -> bool {
    if s.greeted {
        return false;
    }
    let since = s.age_ms.min(s.screen_ago_ms.unwrap_or(u64::MAX));
    s.blank
        || s.age_ms < START_MS
        || s.screen_ago_ms.is_some_and(|ago| ago < AFTER_SCREEN_MS)
        || (since < DRAWING_MS && s.quiet_ms < DRAWN_MS)
}

#[derive(Default, Clone)]
pub struct Turn {
    pub last_nudge_ms: Option<u64>,
    /// The nudge last typed into this pane, while nothing shows that the
    /// agent took it.
    pub typed: Option<Typed>,
    /// How many times in a row this pane was nudged without anything
    /// changing: its state, new mail, the owner typing into it.
    pub tries: u32,
    /// The pane's `state_since_ms` those nudges are counted under.
    pub tries_since_ms: u64,
    /// It was nudged as often as it is going to be, and the owner told.
    pub gave_up: bool,
    /// When a look last found this pane's mail waiting because its Codex
    /// is scrolled back in its own history (`SCROLLED_BACK_WORDS`).
    pub scrolled_back_ms: Option<u64>,
}

/// A Codex that is scrolled back in its own history is typed nothing: the
/// Enter that should submit a message only returns it to its latest line,
/// and the words sit unsent in its box (found 18 Nov 2026 with the real
/// 0.169.0; the screens are in agents/picker.rs, `codex_scrolled_back`).
/// Its mail waits as behind any hold: stored, unread, not announced, and
/// looked at again at the next look. No key is pressed to bring Codex
/// back: Esc also interrupts a Codex at work, and what each key does in
/// each state has not been verified on a real one. A Codex in a turn is
/// told by its hooks, as before. What the pane says meanwhile:
pub const SCROLLED_BACK_WORDS: &str = "has mail waiting: Codex is scrolled back in its own history, where a message typed now would not be sent; it is told when it is back at its latest line";

/// What a look that found it so is believed for. The look once a second
/// says it again while it is so; a pane that began a turn, or whose mail
/// was read by another road, stops saying it by itself.
pub const SCROLLED_BACK_MS: u64 = 3000;

/// Is this pane's mail waiting for a Codex that is scrolled back, as far
/// as the last look knows?
pub fn scrolled_back(turn: &Turn, now: u64) -> bool {
    turn.scrolled_back_ms
        .is_some_and(|at| now.saturating_sub(at) <= SCROLLED_BACK_MS)
}

/// The waits between nudges for mail that is not taken, in units of the
/// station's `nudge_secs` (a minute): after the first nudge one, then two,
/// five and fifteen; after that the pane is not nudged again until
/// something changes. (Found 16 Nov 2026: an OpenCode that was out of
/// quota was nudged every minute for as long as it sat there.)
pub const BACKOFF: [u64; 4] = [1, 2, 5, 15];

/// What a look at a pane's unread mail decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Nothing to say now.
    Nothing,
    /// Tell the agent: type the nudge (or press Enter on an unsent one).
    Nudge,
    /// It has been told as often as it will be: say so to the owner, once.
    GiveUp,
}

/// Is a nudge due? `told`: for each unread message, when its agent was
/// last told of it (0: never). `state_since_ms`: when the pane's state
/// last changed; `owner_acted`: the owner typed into it since the last
/// look. A change of state, new mail or the owner starts the count again.
pub fn due(
    told: &[u64],
    turn: &mut Turn,
    state_since_ms: u64,
    owner_acted: bool,
    now: u64,
    nudge_ms: u64,
) -> Due {
    let fresh = told.contains(&0);
    if told.is_empty() || fresh || owner_acted || turn.tries_since_ms != state_since_ms {
        turn.tries = 0;
        turn.gave_up = false;
        turn.tries_since_ms = state_since_ms;
    }
    if told.is_empty() {
        return Due::Nothing;
    }
    if fresh {
        return Due::Nudge;
    }
    if turn.gave_up {
        return Due::Nothing;
    }
    let last = told.iter().copied().max().unwrap_or(0);
    let step = (turn.tries.max(1) as usize - 1).min(BACKOFF.len() - 1);
    if now.saturating_sub(last) < BACKOFF[step] * nudge_ms {
        return Due::Nothing;
    }
    if turn.tries as usize >= BACKOFF.len() {
        turn.gave_up = true;
        return Due::GiveUp;
    }
    Due::Nudge
}

/// A pane that keeps not taking its mail, in words for its state: said
/// from the second nudge it did not take.
pub fn not_taken_words(turn: &Turn, state_since_ms: u64) -> Option<String> {
    if turn.tries_since_ms != state_since_ms || (turn.tries < 2 && !turn.gave_up) {
        return None;
    }
    let mut words = format!("has mail it has not taken: nudged {} times", turn.tries);
    if turn.gave_up {
        words.push_str(
            ", and not again until it changes state, new mail arrives or you type into it",
        );
    }
    Some(words)
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

/// When a look at a pane's mail was made, and what it is judged by.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub now: u64,
    /// The station's `nudge_secs`, in ms.
    pub nudge_ms: u64,
    pub state_since_ms: u64,
    pub owner_acted: bool,
}

/// One look at one pane's mail, with its turn held: if mail waits that the
/// agent was not told of, or was told of long enough ago by the back-off
/// (`due`) and has not read, `type_it` is asked to type the nudge, with
/// how many messages there are and who sent them (never what they say).
/// Only when it says it did is anything announced: `told` is given the
/// messages' ids first (for the Performance page), then they are marked.
/// Returns how many were announced, and `Due::GiveUp` the one time the
/// pane has been nudged as often as it will be.
pub fn announce(
    mail: &MessageStore,
    names: &[String],
    look: Look,
    turn: &mut Turn,
    type_it: impl FnOnce(usize, &[String]) -> bool,
    told: impl FnOnce(&[String]),
) -> (usize, Due) {
    let waiting = mail.unread_told(names);
    let times: Vec<u64> = waiting.iter().map(|w| w.2).collect();
    let what = due(
        &times,
        turn,
        look.state_since_ms,
        look.owner_acted,
        look.now,
        look.nudge_ms,
    );
    if what != Due::Nudge {
        return (0, what);
    }
    let senders: Vec<String> = waiting.iter().map(|w| w.1.clone()).collect();
    if !type_it(waiting.len(), &senders) {
        return (0, Due::Nothing);
    }
    turn.last_nudge_ms = Some(look.now);
    turn.tries += 1;
    let ids: Vec<String> = waiting.iter().map(|w| w.0.clone()).collect();
    told(&ids);
    mail.mark_announced(&ids, look.now);
    (ids.len(), Due::Nudge)
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

    /// A bug that happened (a real Codex 0.169.0, 18 Nov 2026, in the
    /// station's own log): its notice of a newer version stood for 40
    /// seconds, the owner pressed Escape, and the pane's role was typed
    /// 1.2 seconds later, into the folder-trust question that came next.
    /// Its Enter trusted the folder for the owner, the question swallowed
    /// the words, and the agent heard of its mail a minute later. The
    /// program was "past its start" because only its age was counted.
    /// Now the time is also counted from the last such screen.
    #[test]
    fn a_program_is_still_starting_just_after_a_screen_that_asks_something() {
        let at = |age_ms, screen_ago_ms, quiet_ms| {
            starting(Starting {
                greeted: false,
                age_ms,
                screen_ago_ms,
                quiet_ms,
                blank: false,
            })
        };
        // The fault: 40 s old, the notice gone 1.2 s ago, a quiet screen.
        assert!(at(40_000, Some(1_200), 1_200));
        // And while what follows is being drawn, however old the program.
        assert!(at(90_000, Some(5_000), 300));
        // Promptly once that is over: not a minute, and not at the next
        // of anything.
        assert!(!at(40_000, Some(AFTER_SCREEN_MS), DRAWN_MS));
        // A program that never stops drawing is not waited for for ever.
        assert!(!at(90_000, Some(DRAWING_MS), 0));
        // As before: its first eight seconds, and no such screen at all.
        assert!(at(START_MS - 1, None, 60_000));
        assert!(!at(START_MS, None, DRAWN_MS));
        assert!(at(20_000, None, 100) && !at(DRAWING_MS, None, 100));
        // A program whose session has started is past all of it.
        assert!(!starting(Starting {
            greeted: true,
            age_ms: 100,
            screen_ago_ms: Some(0),
            quiet_ms: 0,
            blank: true,
        }));
        // One that has drawn nothing is not typed into, however old.
        assert!(starting(Starting {
            greeted: false,
            age_ms: 600_000,
            screen_ago_ms: None,
            quiet_ms: 600_000,
            blank: true,
        }));
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
        assert!(
            unsent(Some(&again), 4_000, true),
            "and still no second copy"
        );
    }

    /// Found with real agents on 16 Nov 2026: an OpenCode that was out of
    /// quota (and any program stuck at a sign-in screen) was nudged every
    /// 60 seconds for ever. Now a pane that does not take its mail is
    /// nudged after 1, 2, 5 and 15 minutes more, each wait longer, and
    /// then left alone and the owner told, once; a change of the pane's
    /// state, new mail, or the owner typing into it starts again.
    #[test]
    fn mail_that_is_not_taken_is_nudged_less_and_less_and_then_not_at_all() {
        const MIN: u64 = 60_000;
        let mut turn = Turn::default();
        let since = 500;
        let mut told = vec![0u64, 0];
        let mut nudged_at = vec![];
        let mut gave_up_at = vec![];
        // An hour, looked at once a second, the mail never read.
        for now in (1_000..3_600_000).step_by(1_000) {
            match due(&told, &mut turn, since, false, now, MIN) {
                Due::Nudge => {
                    turn.tries += 1;
                    told = vec![now, now];
                    nudged_at.push((now - 1_000) / MIN);
                }
                Due::GiveUp => gave_up_at.push((now - 1_000) / MIN),
                Due::Nothing => {}
            }
        }
        assert_eq!(
            nudged_at,
            [0, 1, 3, 8],
            "at once, then after 1, 2 and 5 minutes"
        );
        assert_eq!(gave_up_at, [23], "15 minutes after the fourth, said once");
        assert_eq!(
            not_taken_words(&turn, since).as_deref(),
            Some("has mail it has not taken: nudged 4 times, and not again until it changes state, new mail arrives or you type into it")
        );
        let later = 4_000_000;
        assert_eq!(
            due(&told, &mut turn, since, false, later, MIN),
            Due::Nothing
        );
        // Each of the three things that change starts it again.
        let mut again = turn.clone();
        assert_eq!(due(&told, &mut again, since, true, later, MIN), Due::Nudge);
        assert_eq!((again.tries, again.gave_up), (0, false));
        let mut again = turn.clone();
        assert_eq!(
            due(&told, &mut again, since + 9, false, later, MIN),
            Due::Nudge
        );
        let mut again = turn.clone();
        let with_new = [told[0], told[1], 0];
        assert_eq!(
            due(&with_new, &mut again, since, false, later, MIN),
            Due::Nudge
        );
        // Read: nothing is due and nothing is said of the pane.
        assert_eq!(due(&[], &mut turn, since, false, later, MIN), Due::Nothing);
        assert_eq!(not_taken_words(&turn, since), None);
        // One nudge not yet taken is not worth saying.
        let once = Turn {
            tries: 1,
            tries_since_ms: since,
            ..Turn::default()
        };
        assert_eq!(not_taken_words(&once, since), None);
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
        let look = |now| Look {
            now,
            nudge_ms: 60_000,
            state_since_ms: 7,
            owner_acted: false,
        };
        let (told, _) = announce(&mail, &names, look(now), &mut turn, |_, _| false, |_| {});
        assert_eq!((told, turn.last_nudge_ms), (0, None));
        assert_eq!(mail.unannounced(&names, 0).len(), 50);

        let mut typed = 0;
        let mut counted = 0;
        for _ in 0..50 {
            announce(
                &mail,
                &names,
                look(now),
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
                look(later),
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
