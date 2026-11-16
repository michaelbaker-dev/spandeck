//! The heartbeat of a long-lived connection (DECISIONS.md, "A connection
//! that has gone silent is noticed, and the screen says so").
//!
//! The audit: a browser whose link was half-open showed nothing for five
//! minutes, and a pane reached through another station froze for ten with
//! no notice, because no connection had a ping and nothing ever judged one
//! dead. The rule here is the whole of the judging, apart from the socket
//! and the clock, so that it is the same on every connection a station
//! holds: a deck's, a gateway's socket for one pane (both ends), and later
//! the events socket between stations (unit 10).
//!
//! - A ping goes out every `every`.
//! - A connection is dead when nothing at all was heard on it for `dead`,
//!   and is given up at that moment: any frame counts, not only the answer
//!   to a ping, so a link that is slow and alive (frames arrive late, but
//!   they arrive) is never taken down.
//! - **Time the judge itself was not running proves nothing about the other
//!   end.** A station that was asleep, frozen or held up finds every one of
//!   its connections silent for as long as it was away; it gives each a
//!   whole new `dead` to be heard from before it concludes anything, and a
//!   tick that is only a little late takes that much off the silence.

use std::time::{Duration, Instant};

/// The switch (`[flags] deck_ping = false`, or `station.flags`; read when a
/// connection begins), and what a station says in `caps` when it is on:
/// this station answers a page's `{"ping": n}` and pings its connections.
pub const FLAG: &str = "deck_ping";
pub const CAP: &str = "ping";

/// A Control Deck's connection: a ping every 15 s, closed after 30 s with
/// nothing heard. A browser answers a ping by itself, in the background
/// too, so this is only ever a page that is gone (a phone that was shut, a
/// link that is half-open); closing frees what the connection holds.
pub const DECK: (Duration, Duration) = (Duration::from_secs(15), Duration::from_secs(30));

/// A connection one station holds to another for one pane, at both ends: a
/// ping every 2 s, dead after 6 s with nothing heard. Three pings: one lost
/// frame or one slow second does not end it, and the owner looking at that
/// pane is told within the time a call to that station is given (6 s).
pub const STATION: (Duration, Duration) = (Duration::from_secs(2), Duration::from_secs(6));

/// Milliseconds on a clock of this process that only goes forward: what a
/// station puts in the answer to a page's ping (`s`), so that a page can
/// tell how long the station took between two of its answers without
/// either wall clock.
pub fn mono_ms() -> u64 {
    static BEGAN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    BEGAN.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// What a tick found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Send a ping.
    Ping,
    /// Nothing to do yet.
    Wait,
    /// Nothing was heard for `dead`: the connection is over.
    Dead,
}

/// One connection's heartbeat.
#[derive(Debug, Clone)]
pub struct Beat {
    every: Duration,
    dead: Duration,
    heard: Instant,
    pinged: Instant,
    due: Instant,
}

impl Beat {
    pub fn new((every, dead): (Duration, Duration), now: Instant) -> Beat {
        Beat {
            every,
            dead,
            heard: now,
            pinged: now,
            due: now + every,
        }
    }

    /// When `tick` is to be called next: when the next ping is due, or at
    /// the moment the silence will have lasted `dead`, whichever is first.
    /// So a connection is given up when it has been silent for `dead`, not
    /// at the first ping after that.
    pub fn due(&self) -> Instant {
        self.due
    }

    /// Something arrived on the connection, whatever it was.
    pub fn heard(&mut self, at: Instant) {
        if at > self.heard {
            self.heard = at;
        }
    }

    /// How long ago the other end was last heard.
    pub fn quiet(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.heard)
    }

    /// Called when `due` has come.
    pub fn tick(&mut self, now: Instant) -> Verdict {
        // However late this tick is, the judge was not running for that
        // long, and that time is not the other end's silence. More than a
        // whole interval late (asleep, frozen): whatever arrived meanwhile
        // has not even been read yet, and the other end gets all of `dead`.
        let late = now.saturating_duration_since(self.due);
        self.heard = if late > self.every {
            now
        } else {
            (self.heard + late).min(now).max(self.heard)
        };
        let verdict = if self.quiet(now) >= self.dead {
            Verdict::Dead
        } else if now.saturating_duration_since(self.pinged) >= self.every {
            self.pinged = now;
            Verdict::Ping
        } else {
            Verdict::Wait
        };
        self.due = (self.pinged + self.every)
            .min(self.heard + self.dead)
            .max(now + Duration::from_millis(1));
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The audit's gap 2 (FAILURES section 4 and 5): a half-open connection
    /// was never noticed. And the second review's condition on the cure: a
    /// link that is slow and alive must not be taken down, nor may a
    /// station that was itself asleep or frozen drop every connection it
    /// holds the moment it wakes.
    #[test]
    fn silence_ends_a_connection_and_the_judges_own_absence_does_not() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        // Called when it asks to be, as the connections do.
        let next = |beat: &mut Beat| {
            let at = beat.due();
            (at.duration_since(t0), beat.tick(at))
        };
        // Nothing heard: pinged at 2 and 4 s, given up at 6 s.
        let mut beat = Beat::new(STATION, t0);
        assert_eq!(next(&mut beat), (s(2), Verdict::Ping));
        assert_eq!(next(&mut beat), (s(4), Verdict::Ping));
        assert_eq!(next(&mut beat), (s(6), Verdict::Dead));

        // A link two seconds away each way: every answer is four seconds
        // old when it arrives, and one arrives every two seconds. Alive.
        let mut beat = Beat::new(STATION, t0);
        for n in 1..=30 {
            if n >= 2 {
                beat.heard(t0 + s(2 * n) - Duration::from_millis(1));
            }
            assert_eq!(next(&mut beat), (s(2 * n), Verdict::Ping), "tick {n}");
        }

        // Any frame counts, not only a pong; an older one does not move
        // the time back. Given up when the silence has lasted the half
        // minute, not at the first ping after that.
        let mut beat = Beat::new(DECK, t0);
        beat.heard(t0 + s(20));
        beat.heard(t0 + s(10));
        assert_eq!(next(&mut beat), (s(15), Verdict::Ping));
        assert_eq!(next(&mut beat), (s(30), Verdict::Ping));
        assert_eq!(next(&mut beat), (s(45), Verdict::Ping));
        assert_eq!(next(&mut beat), (s(50), Verdict::Dead));

        // The station was frozen for ten minutes (or its loop was held
        // up): the first tick after it concludes nothing, and the other
        // end has a whole six seconds to be heard from.
        let mut beat = Beat::new(STATION, t0);
        assert_eq!(next(&mut beat), (s(2), Verdict::Ping));
        let woke = t0 + s(602);
        assert_eq!(beat.tick(woke), Verdict::Ping);
        assert_eq!(next(&mut beat), (s(604), Verdict::Ping));
        assert_eq!(next(&mut beat), (s(606), Verdict::Ping));
        assert_eq!(
            next(&mut beat),
            (s(608), Verdict::Dead),
            "it really is gone"
        );
        // And one that answers after the wake goes on.
        let mut beat = Beat::new(STATION, t0);
        assert_eq!(beat.tick(woke), Verdict::Ping);
        beat.heard(woke + s(1));
        assert_eq!(next(&mut beat), (s(604), Verdict::Ping));
        assert_eq!(next(&mut beat), (s(606), Verdict::Ping));
        // A tick that is a little late (a busy moment, not a sleep) takes
        // that much off the silence and no more: one and a half seconds
        // late at five seconds of silence is still not six.
        let late = t0 + s(608) + Duration::from_millis(1500);
        assert_eq!(beat.tick(late), Verdict::Ping);
        assert_eq!(beat.quiet(late), s(5));
    }
}
