//! Work for another station runs in a round of that station's own.
//!
//! The owner's rule (15 Nov 2026): "the slow or absent station has to be
//! fixed with the right architecture for a system like this, never to slow
//! everyone else down." So nothing here waits for one station on behalf of
//! another, or on behalf of this station's own work:
//!
//!   - each address has a round of its own, in a task of its own, with a
//!     deadline of its own;
//!   - a round for an address is never started while its last one runs, so
//!     rounds for a station that is slow do not pile up;
//!   - an address that keeps failing is asked less often (`gap`), and at
//!     once again when it answers, is heard from, or Tailscale says it is
//!     back;
//!   - whoever starts rounds (`kick`) only starts them. Only a caller that
//!     wants the whole fleet looked at (`settle`: the owner's change to a
//!     synced document, `fleet.refresh`) waits, and for a bounded time.
//!
//! This file is the schedule and the bookkeeping, with the link to a station
//! behind a trait, so it runs under simulated time (tests/round_sim.rs).
//! What a round does at a real station is in fleet.rs (`station_round`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

pub use crate::relay::BoxFuture;

/// The time between two looks at the fleet.
pub const LOOK: Duration = Duration::from_secs(10);
/// A round that would begin within this of a look is begun at that look.
const SLACK_MS: u64 = 1_000;
/// A station that is heard from is asked at once, but not more often.
const HEARD_EVERY_MS: u64 = 2_000;

/// What is at an address, as far as this station knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A station of this fleet, or an address the config names.
    Member,
    /// A machine under the owner's login that has not joined.
    Other,
}

/// What a round came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A station answered there.
    Answered,
    /// Nothing answered in time.
    Silent,
}

/// Which rounds a kick starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    /// Those whose time has come.
    Due,
    /// Every member now, and the others when their time has come.
    Members,
    /// Every address now: the owner is adding a machine or joining a fleet.
    All,
}

/// How long after a round began the next one for that address is due.
///
/// A member: every look; after three failures in a row every second look,
/// then every third, and no less often than that, so a station that is back
/// and was not heard from is found within 30 seconds. A machine that has
/// not joined: every look for its first minute online, then once a minute;
/// after three failures in a row every five minutes.
pub fn gap(kind: Kind, online_for: Duration, fails: u32) -> Duration {
    match kind {
        Kind::Member => LOOK * (fails.saturating_sub(1)).clamp(1, 3),
        Kind::Other if fails >= 3 => Duration::from_secs(300),
        Kind::Other if online_for < Duration::from_secs(60) => LOOK,
        Kind::Other => Duration::from_secs(60),
    }
}

/// The link to a station: what one round does there.
pub trait Link: Clone + Send + Sync + 'static {
    /// Everything this station has to do with the station at `addr`, once.
    fn round<'a>(&'a self, addr: &'a str) -> BoxFuture<'a, Outcome>;

    /// A round ended (or was cut off at its deadline): for the counters.
    fn ended(&self, _addr: &str, _took: Duration, _outcome: Outcome) {}
}

#[derive(Debug, Clone)]
struct Dest {
    kind: Kind,
    /// Since when it has been listed, or online again.
    since_ms: u64,
    online: Option<bool>,
    fails: u32,
    due_ms: u64,
    /// When the round that is running began.
    running: Option<u64>,
    /// No longer listed; dropped when its round ends.
    gone: bool,
    /// Made due while its round ran: due again when that ends.
    again: bool,
    heard_ms: u64,
    rounds: u64,
    last: Option<(u64, Outcome)>,
}

/// One address to look at, as a look lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub addr: String,
    pub kind: Kind,
    /// What Tailscale says, when it says anything about this address.
    pub online: Option<bool>,
}

pub struct Rounds {
    began: tokio::time::Instant,
    dests: Mutex<BTreeMap<String, Dest>>,
    /// Counts rounds that ended: whoever waits for the fleet watches it.
    ended: tokio::sync::watch::Sender<u64>,
    /// A round is cut off after this long.
    pub deadline: Duration,
}

impl Rounds {
    pub fn new(deadline: Duration) -> Arc<Rounds> {
        Arc::new(Rounds {
            began: tokio::time::Instant::now(),
            dests: Mutex::new(BTreeMap::new()),
            ended: tokio::sync::watch::Sender::new(0),
            deadline,
        })
    }

    /// Milliseconds on a clock that only goes forward.
    fn now(&self) -> u64 {
        self.began.elapsed().as_millis() as u64
    }

    /// The addresses to look at, as of now. One that is new, or that
    /// Tailscale says is online again, is due at once and owed nothing for
    /// its past failures; one that became a member is due at once.
    pub fn follow(&self, listed: &[Listed]) {
        let now = self.now();
        let mut dests = self.dests.lock().unwrap();
        dests.retain(|addr, d| {
            d.gone = !listed.iter().any(|l| l.addr == *addr);
            !d.gone || d.running.is_some()
        });
        for l in listed {
            let d = dests.entry(l.addr.clone()).or_insert(Dest {
                kind: l.kind,
                since_ms: now,
                online: l.online,
                fails: 0,
                due_ms: now,
                running: None,
                gone: false,
                again: false,
                heard_ms: 0,
                rounds: 0,
                last: None,
            });
            let back = d.online == Some(false) && l.online == Some(true);
            if back {
                (d.since_ms, d.fails) = (now, 0);
            }
            if back || (d.kind == Kind::Other && l.kind == Kind::Member) {
                (d.due_ms, d.again) = (now, d.running.is_some());
            }
            (d.kind, d.online) = (l.kind, l.online);
        }
    }

    /// The station at `addr` made a call here, so it is there: ask it now
    /// if it was being asked less often, or is not due for a while. True
    /// when a kick would now start its round.
    pub fn heard(&self, addr: &str) -> bool {
        let now = self.now();
        let mut dests = self.dests.lock().unwrap();
        let Some(d) = dests.get_mut(addr) else {
            return false;
        };
        if d.running.is_some() || (d.heard_ms > 0 && now - d.heard_ms < HEARD_EVERY_MS) {
            return false;
        }
        let waiting = d.fails > 0 || d.last.is_none();
        if waiting {
            (d.heard_ms, d.due_ms) = (now, now);
        }
        waiting
    }

    /// Start the rounds `select` names, each in a task of its own, and
    /// return at once: nothing here waits for a station. An address whose
    /// round is still running is passed over. Returns how many began.
    pub fn kick<L: Link>(self: &Arc<Self>, link: &L, select: Select) -> usize {
        let now = self.now();
        let begin: Vec<String> = {
            let mut dests = self.dests.lock().unwrap();
            dests
                .iter_mut()
                .filter(|(_, d)| d.running.is_none() && !d.gone)
                .filter(|(_, d)| match select {
                    Select::All => true,
                    Select::Members if d.kind == Kind::Member => true,
                    _ => d.due_ms <= now + SLACK_MS,
                })
                .map(|(addr, d)| {
                    d.running = Some(now);
                    addr.clone()
                })
                .collect()
        };
        for addr in &begin {
            let (link, addr) = (link.clone(), addr.clone());
            // Ended when the task ends, however it ends: a round that
            // panics must not leave its station "running" for ever.
            let mut running = Running {
                rounds: self.clone(),
                link: link.clone(),
                addr: addr.clone(),
                outcome: Outcome::Silent,
            };
            let deadline = self.deadline;
            tokio::spawn(async move {
                if let Ok(outcome) = tokio::time::timeout(deadline, link.round(&addr)).await {
                    running.came_to(outcome);
                }
            });
        }
        begin.len()
    }

    fn end(&self, addr: &str, outcome: Outcome) -> Duration {
        let now = self.now();
        let mut took = Duration::ZERO;
        {
            let mut dests = self.dests.lock().unwrap();
            if let Some(d) = dests.get_mut(addr) {
                let started = d.running.take().unwrap_or(now);
                took = Duration::from_millis(now - started);
                d.fails = match outcome {
                    Outcome::Answered => 0,
                    Outcome::Silent => d.fails.saturating_add(1),
                };
                let online_for = Duration::from_millis(started.saturating_sub(d.since_ms));
                // An address made due while its round ran (it became a
                // member, or is online again) is due at once.
                let next = started + gap(d.kind, online_for, d.fails).as_millis() as u64;
                d.due_ms = if std::mem::take(&mut d.again) {
                    now
                } else {
                    next
                };
                d.rounds += 1;
                d.last = Some((took.as_millis() as u64, outcome));
                if d.gone {
                    dests.remove(addr);
                }
            }
        }
        self.ended.send_modify(|n| *n += 1);
        took
    }

    fn running(&self, members_only: bool) -> bool {
        let dests = self.dests.lock().unwrap();
        dests
            .values()
            .any(|d| d.running.is_some() && !(members_only && d.kind == Kind::Other))
    }

    /// Wait until no round is running, or `within` has passed, whichever
    /// is first. `members_only`: machines that have not joined are not
    /// waited for. For a caller that wants the fleet to have been looked
    /// at; the rounds themselves never wait for each other.
    pub async fn settle(&self, members_only: bool, within: Duration) {
        let mut ended = self.ended.subscribe();
        let _ = tokio::time::timeout(within, async {
            while self.running(members_only) {
                if ended.changed().await.is_err() {
                    return;
                }
            }
        })
        .await;
    }

    /// How many rounds have ended, at every address, since the start.
    pub fn ended_count(&self) -> u64 {
        *self.ended.borrow()
    }

    /// For the Performance page and for tests: each address's schedule.
    pub fn view(&self) -> BTreeMap<String, Value> {
        let now = self.now();
        let dests = self.dests.lock().unwrap();
        dests
            .iter()
            .map(|(addr, d)| {
                let v = json!({
                    "member": d.kind == Kind::Member,
                    "rounds": d.rounds,
                    "failed_in_a_row": d.fails,
                    "running_ms": d.running.map(|at| now - at),
                    "next_in_ms": d.running.is_none().then(|| d.due_ms.saturating_sub(now)),
                    "last_ms": d.last.map(|(ms, _)| ms),
                    "last_answered": d.last.map(|(_, o)| o == Outcome::Answered),
                });
                (addr.clone(), v)
            })
            .collect()
    }
}

struct Running<L: Link> {
    rounds: Arc<Rounds>,
    link: L,
    addr: String,
    outcome: Outcome,
}

impl<L: Link> Running<L> {
    fn came_to(&mut self, outcome: Outcome) {
        self.outcome = outcome;
    }
}

impl<L: Link> Drop for Running<L> {
    fn drop(&mut self) {
        let took = self.rounds.end(&self.addr, self.outcome);
        self.link.ended(&self.addr, took, self.outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit 07 of the performance plan: twelve online devices under the
    /// owner's login were asked every ten seconds whether they run spandeck,
    /// and a station that never answers was asked as often as one that
    /// does. This is the schedule that replaces that, as a pure function.
    #[test]
    fn a_machine_that_has_not_joined_is_asked_less_often_and_a_failing_one_less_still() {
        let secs = |k, online: u64, fails| gap(k, Duration::from_secs(online), fails).as_secs();
        // A member: every look, whatever else; backed off after three
        // failures in a row, never beyond 30 s.
        assert_eq!(
            [0, 1, 2, 3, 4, 5, 50].map(|f| secs(Kind::Member, 9_999, f)),
            [10, 10, 10, 20, 30, 30, 30]
        );
        // Not a member: every look in its first minute online, then once
        // a minute; every five minutes after three failures in a row, even
        // in its first minute.
        assert_eq!(secs(Kind::Other, 0, 0), 10);
        assert_eq!(secs(Kind::Other, 59, 2), 10);
        assert_eq!(secs(Kind::Other, 60, 0), 60);
        assert_eq!(secs(Kind::Other, 3_600, 2), 60);
        assert_eq!(secs(Kind::Other, 5, 3), 300);
        assert_eq!(secs(Kind::Other, 3_600, 9), 300);
    }
}
