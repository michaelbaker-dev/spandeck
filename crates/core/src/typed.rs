//! What a Control Deck types into a pane (DECISIONS.md, "A key can never
//! reach another pane" and "What is typed"): the switches and the words a
//! station announces, and the rule by which the station that receives
//! something typed decides whether it is still on time.
//!
//! The audit's gap 3: keys typed into a link that had stopped passing
//! anything were delivered when it returned, 232 of them up to 60 s late and
//! 2,996 up to ten minutes late through another station, into a shell. A
//! sender cannot prevent that: what it has written is in the connection. So
//! the receiver refuses what is late, and says so.
//!
//! **How old something typed is, without either wall clock.** The sender
//! stamps it with its own forward-only clock (`t`, milliseconds). The
//! receiver knows how the two clocks stand from the sender's pings, which
//! carry the same clock: `base` is the smallest "my clock when it arrived,
//! minus its `t`" over the last few pings, that is, the two clocks'
//! difference plus the quickest trip seen. Something typed is then as old as
//! it arrived later than `base` says a prompt one would have.
//!
//! - **Only a ping sets the baseline.** The second review: if the first
//!   stamped key set it, a key held by a page that froze would measure as
//!   fresh. Until a connection has a baseline, what is stamped is refused.
//! - **The last 8 pings**, so that two clocks that drift apart over hours do
//!   not slowly make everything look late, and one slow ping does not make
//!   anything look early (the smallest of them counts).
//! - **A sender that was not running says so** (`fresh` in its next ping):
//!   its clock may have stood still, and the pings from before prove nothing
//!   about it now. What it sent before that ping is judged by the readings
//!   from before; a station restarted is told apart the same way (`boot`).

use std::collections::VecDeque;

use serde_json::Value;

/// The switch (`[flags] deck_names = false`, or `station.flags`; read when
/// a connection begins), and what a station says in `caps` when it is on:
/// it takes the name a page gives an attachment (`pane.attach {chan}`), and
/// a page or a gateway that comes back replaces its own earlier connection.
pub const NAMES_FLAG: &str = "deck_names";
pub const NAMES_CAP: &str = "names";

/// The switch (`[flags] deck_input = false`, or `station.flags`; read when
/// a connection begins), and what a station says in `caps` when it is on
/// (and it answers pings): what is typed with a stamp is refused when it is
/// late, and numbered input is taken once.
pub const INPUT_FLAG: &str = "deck_input";
pub const INPUT_CAP: &str = "input";
/// How late a key may be, before twice the round trip is added
/// (`deck_input.fresh_ms`, between half a second and ten).
pub const FRESH_FLAG: &str = "deck_input.fresh_ms";
pub const FRESH_MS: u64 = 2000;
/// How late one deliberate act may be: a message sent from the message box,
/// a paste from the menu. The limit of a click.
pub const ACT_MS: u64 = 10_000;
/// The round trip a sender reports counts up to this (so a key's limit is
/// never over `fresh_ms` + 30 s, whatever a sender says).
pub const RTT_MAX_MS: u64 = 15_000;
/// A baseline read for a call that came by another road than the pings
/// (a station's call to another) is used only this long after its last ping.
pub const READING_MAX_AGE_MS: u64 = 10_000;

const SAMPLES: usize = 8;

/// `deck_input.fresh_ms` as a station uses it.
pub fn fresh_ms(flag: Option<&Value>) -> u64 {
    flag.and_then(|v| v.as_u64())
        .map_or(FRESH_MS, |ms| ms.clamp(500, 10_000))
}

/// What marks this process among every run of this station: a station that
/// restarts starts its forward-only clock again, and says so in its pings.
pub fn boot() -> &'static str {
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| crate::paths::random_token()[..16].to_string())
}

/// How one sender's clock stands to this station's, from its pings.
#[derive(Debug, Default, Clone)]
pub struct Baseline {
    samples: VecDeque<f64>,
    rtt_ms: u64,
    boot: Option<String>,
    last_ms: u64,
}

/// A baseline as it stood at one moment: when something was read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// Receiver's clock minus sender's, at the quickest trip seen.
    pub base: f64,
    /// The round trip the sender last reported.
    pub rtt_ms: u64,
}

impl Baseline {
    /// A ping arrived at `now_ms` (this station's forward-only clock):
    /// `{"ping": n, "t": <sender's clock>, "rtt": <its round trip>,
    /// "fresh": true, "boot": "…"}`. A ping without a usable `t` sets
    /// nothing.
    pub fn ping(&mut self, now_ms: u64, ping: &Value) {
        let Some(t) = ping
            .get("t")
            .and_then(|t| t.as_f64())
            .filter(|t| t.is_finite())
        else {
            return;
        };
        let boot = ping
            .get("boot")
            .and_then(|b| b.as_str())
            .map(|b| b.chars().take(32).collect::<String>());
        if ping.get("fresh").and_then(|f| f.as_bool()) == Some(true) || boot != self.boot {
            self.samples.clear();
        }
        self.boot = boot;
        if self.samples.len() == SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(now_ms as f64 - t);
        if let Some(rtt) = ping.get("rtt").and_then(|r| r.as_f64()) {
            self.rtt_ms = (rtt.max(0.0) as u64).min(RTT_MAX_MS);
        }
        self.last_ms = now_ms;
    }

    /// The baseline now, when there is one.
    pub fn reading(&self) -> Option<Reading> {
        let base = self.samples.iter().copied().reduce(f64::min)?;
        Some(Reading {
            base,
            rtt_ms: self.rtt_ms,
        })
    }

    /// The baseline for something that did not come down the connection
    /// the pings come down: only from this run of the sender (`boot`), and
    /// only while its pings are still arriving.
    pub fn reading_for(&self, boot: Option<&str>, now_ms: u64) -> Option<Reading> {
        (boot.is_some()
            && boot == self.boot.as_deref()
            && now_ms.saturating_sub(self.last_ms) <= READING_MAX_AGE_MS)
            .then(|| self.reading())
            .flatten()
    }
}

/// What something typed carries when its sender stamped it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stamp {
    /// When it was typed, by the sender's clock.
    pub t: f64,
    /// The round trip of the whole road so far, when a station that passed
    /// it on says (else the connection's own counts).
    pub rtt_ms: Option<u64>,
}

impl Stamp {
    /// From a request's parameters: `t`, and `rtt`. Nothing when it is not
    /// stamped (a page from before, the command line): taken as before.
    pub fn of(params: &Value) -> Option<Stamp> {
        let t = params.get("t")?.as_f64().filter(|t| t.is_finite())?;
        let rtt_ms = params
            .get("rtt")
            .and_then(|r| r.as_f64())
            .map(|r| (r.max(0.0) as u64).min(RTT_MAX_MS));
        Some(Stamp { t, rtt_ms })
    }
}

/// What is typed, by how long it may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Keys: `fresh_ms` plus twice the round trip.
    Key,
    /// One deliberate act (the message box, a paste from the menu): 10 s.
    Act,
}

impl Kind {
    pub fn of(method: &str) -> Option<Kind> {
        match method {
            "pane.input" => Some(Kind::Key),
            "pane.paste" | "pane.send_text" => Some(Kind::Act),
            _ => None,
        }
    }

    pub fn limit_ms(self, fresh_ms: u64, rtt_ms: u64) -> u64 {
        match self {
            Kind::Key => fresh_ms + 2 * rtt_ms.min(RTT_MAX_MS),
            Kind::Act => ACT_MS,
        }
    }
}

/// What the receiver makes of something typed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// On time. `typed_ms`: when it was typed, by this station's clock.
    Take { typed_ms: f64, rtt_ms: u64 },
    /// The connection has no baseline yet: refused.
    NotReady,
    /// It is older than its limit: refused.
    Late { age_ms: u64, limit_ms: u64 },
}

/// Decide. `reading`: the baseline as it stood when this was read off the
/// connection (not as it stands now: a sender that has since said it was
/// away must not make what it sent before look fresh). `now_ms`: this
/// station's clock at the moment it would be handed on.
pub fn judge(
    reading: Option<Reading>,
    stamp: Stamp,
    now_ms: u64,
    kind: Kind,
    fresh_ms: u64,
) -> Verdict {
    let Some(reading) = reading else {
        return Verdict::NotReady;
    };
    let typed_ms = stamp.t + reading.base;
    let rtt_ms = stamp.rtt_ms.unwrap_or(reading.rtt_ms);
    let limit_ms = kind.limit_ms(fresh_ms, rtt_ms);
    let age = now_ms as f64 - typed_ms;
    if age > limit_ms as f64 {
        Verdict::Late {
            age_ms: age as u64,
            limit_ms,
        }
    } else {
        Verdict::Take { typed_ms, rtt_ms }
    }
}

/// Numbered input is taken once: `iseq` counts up from 1 on each
/// attachment, and one that is not above the last taken is a repeat.
/// Unnumbered input (a page from before) is taken as before.
pub fn take_once(last: &mut u64, iseq: Option<u64>) -> bool {
    match iseq {
        None => true,
        Some(n) if n > *last => {
            *last = n;
            true
        }
        Some(_) => false,
    }
}

/// When a station passing keys on to another refuses them itself because
/// that station has said nothing: after 1.5 s without an answer to the ping
/// it sends every 2 s, or four round trips when that is longer, at most 6 s
/// (when the connection is given up anyway). The page's own rule for "not
/// answering", for the second hop.
pub fn silent_after_ms(ping_every_ms: u64, rtt_ms: u64) -> u64 {
    (ping_every_ms + (4 * rtt_ms).max(1500)).min(6000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(t: f64) -> Stamp {
        Stamp { t, rtt_ms: None }
    }

    /// The second review: "the first stamped frame on a connection sets its
    /// own baseline, so a key held by a frozen phone page can pass as
    /// fresh". Only a ping sets it; and the audit's 60 s and ten minutes of
    /// keys are refused, whatever the two clocks read.
    #[test]
    fn only_a_ping_sets_the_baseline_and_a_late_key_is_refused() {
        let mut b = Baseline::default();
        // The page's clock reads 5,000 when the station's reads 1,000,000:
        // nothing says so until a ping does.
        assert_eq!(b.reading(), None);
        assert_eq!(
            judge(b.reading(), key(5_000.0), 1_000_000, Kind::Key, 2000),
            Verdict::NotReady
        );
        // A ping without a time sets nothing; nor does anything typed.
        b.ping(1_000_000, &json!({"ping": 1}));
        assert_eq!(b.reading(), None);
        b.ping(1_000_020, &json!({"ping": 2, "t": 5_000.0, "rtt": 40.0}));
        let r = b.reading().unwrap();
        assert_eq!((r.base, r.rtt_ms), (995_020.0, 40));
        // Typed 100 ms after that ping, arrived 120 ms after it: on time.
        assert!(matches!(
            judge(Some(r), key(5_100.0), 1_000_140, Kind::Key, 2000),
            Verdict::Take { typed_ms, rtt_ms: 40 } if typed_ms == 1_000_120.0
        ));
        // The same key arriving after the link stood still for 60 s, and
        // for ten minutes: refused, with how late it was.
        assert_eq!(
            judge(Some(r), key(5_100.0), 1_060_120, Kind::Key, 2000),
            Verdict::Late {
                age_ms: 60_000,
                limit_ms: 2080
            }
        );
        assert!(matches!(
            judge(Some(r), key(5_100.0), 1_600_120, Kind::Key, 2000),
            Verdict::Late {
                age_ms: 600_000,
                ..
            }
        ));
        // A key the page held while it was frozen for a minute, sent when
        // it woke, ahead of the ping that says it was away: judged by the
        // reading it was read under, and refused.
        let before = b.reading();
        b.ping(
            1_061_000,
            &json!({"ping": 3, "t": 65_980.0, "fresh": true, "rtt": 40.0}),
        );
        assert!(matches!(
            judge(before, key(5_500.0), 1_061_000, Kind::Key, 2000),
            Verdict::Late { .. }
        ));
        // What it types after that ping is on time again.
        assert!(matches!(
            judge(b.reading(), key(66_000.0), 1_061_040, Kind::Key, 2000),
            Verdict::Take { .. }
        ));
    }

    /// The limit: 2 s plus twice the round trip for a key, so a link that is
    /// slow and alive can be typed over (the second review); 10 s for one
    /// deliberate act.
    #[test]
    fn the_limit_follows_the_round_trip() {
        let mut b = Baseline::default();
        b.ping(10_000, &json!({"ping": 1, "t": 0.0, "rtt": 0.0}));
        let at = |rtt: u64, age: u64, kind| {
            let mut b = b.clone();
            b.ping(10_000, &json!({"ping": 2, "t": 0.0, "rtt": rtt}));
            matches!(
                judge(b.reading(), key(1000.0), 11_000 + age, kind, 2000),
                Verdict::Take { .. }
            )
        };
        // A healthy link: 2 s, and not a millisecond more.
        assert!(at(0, 2000, Kind::Key) && !at(0, 2001, Kind::Key));
        assert!(at(40, 2080, Kind::Key) && !at(40, 2081, Kind::Key));
        // Two seconds each way (a round trip of 4 s): 10 s.
        assert!(at(4000, 10_000, Kind::Key) && !at(4000, 10_001, Kind::Key));
        // Whatever a sender says its round trip is, at most 30 s are added.
        assert!(at(900_000, 32_000, Kind::Key) && !at(900_000, 32_001, Kind::Key));
        // The message box: 10 s, whatever the round trip.
        assert!(at(0, 10_000, Kind::Act) && !at(0, 10_001, Kind::Act));
        assert!(!at(4000, 10_001, Kind::Act));
        // A station that passed it on says the round trip of the whole road.
        let far = Stamp {
            t: 1000.0,
            rtt_ms: Some(500),
        };
        assert!(matches!(
            judge(b.reading(), far, 14_000, Kind::Key, 2000),
            Verdict::Take { rtt_ms: 500, .. }
        ));
        assert!(matches!(
            judge(b.reading(), far, 14_001, Kind::Key, 2000),
            Verdict::Late { limit_ms: 3000, .. }
        ));
        // The switch's number: between half a second and ten.
        assert_eq!(fresh_ms(None), 2000);
        assert_eq!(fresh_ms(Some(&json!(3000))), 3000);
        assert_eq!(fresh_ms(Some(&json!(1))), 500);
        assert_eq!(fresh_ms(Some(&json!(99_999_999))), 10_000);
        assert_eq!(fresh_ms(Some(&json!("soon"))), 2000);
        assert_eq!(Kind::of("pane.input"), Some(Kind::Key));
        assert_eq!(Kind::of("pane.paste"), Some(Kind::Act));
        assert_eq!(Kind::of("pane.resize"), None);
    }

    /// Why the baseline is the smallest of the last 8 pings and not of all
    /// of them, or the latest: one slow ping must not make keys look early,
    /// clocks that drift must not make them look late for ever, and a
    /// station that restarted is another clock.
    #[test]
    fn the_baseline_is_the_quickest_of_the_last_pings() {
        let mut b = Baseline::default();
        b.ping(1000, &json!({"ping": 1, "t": 0.0}));
        // A ping that was held up 3 s on its way changes nothing.
        b.ping(5000, &json!({"ping": 2, "t": 1000.0}));
        assert_eq!(b.reading().unwrap().base, 1000.0);
        // The sender's clock runs slow (or stood still while it slept and
        // nothing said so): every ping from now reads 500 ms more. After 8
        // of them the old reading is gone.
        for n in 0..6 {
            b.ping(
                6000 + n * 1000,
                &json!({"ping": n, "t": 4500.0 + n as f64 * 1000.0}),
            );
            assert_eq!(b.reading().unwrap().base, 1000.0, "ping {n}");
        }
        b.ping(13_000, &json!({"ping": 9, "t": 11_500.0}));
        assert_eq!(b.reading().unwrap().base, 1500.0);
        // A quicker trip is taken at once.
        b.ping(14_000, &json!({"ping": 10, "t": 12_600.0}));
        assert_eq!(b.reading().unwrap().base, 1400.0);
        // A station's pings name its run: another run starts again, and a
        // reading is given to a call that came by another road only for
        // that run and while its pings are arriving.
        let mut b = Baseline::default();
        b.ping(1000, &json!({"ping": 1, "t": 900_000.0, "boot": "aa"}));
        assert_eq!(b.reading_for(Some("aa"), 1000).unwrap().base, -899_000.0);
        assert_eq!(b.reading_for(Some("bb"), 1000), None);
        assert_eq!(b.reading_for(None, 1000), None);
        assert!(b.reading_for(Some("aa"), 11_000).is_some());
        assert_eq!(b.reading_for(Some("aa"), 11_001), None);
        b.ping(2000, &json!({"ping": 1, "t": 5.0, "boot": "bb"}));
        assert_eq!(b.reading().unwrap().base, 1995.0);
    }

    /// "Keys arrive in the order typed, none twice": a number is taken
    /// once, and what is not numbered is taken as before.
    #[test]
    fn numbered_input_is_taken_once() {
        let mut last = 0;
        assert!(take_once(&mut last, Some(1)));
        assert!(take_once(&mut last, Some(2)));
        assert!(!take_once(&mut last, Some(2)));
        assert!(!take_once(&mut last, Some(1)));
        // The page did not send 3 (it refused that key itself): 4 is next.
        assert!(take_once(&mut last, Some(4)));
        assert!(!take_once(&mut last, Some(3)));
        assert!(!take_once(&mut last, Some(0)));
        assert!(take_once(&mut last, None));
        assert_eq!(last, 4);
        // The second hop's own "not answering": 3.5 s, more on a slow
        // link, never more than the 6 s after which it is given up.
        assert_eq!(silent_after_ms(2000, 0), 3500);
        assert_eq!(silent_after_ms(2000, 500), 4000);
        assert_eq!(silent_after_ms(2000, 4000), 6000);
    }
}
