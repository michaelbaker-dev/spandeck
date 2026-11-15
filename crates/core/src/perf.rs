//! Where time goes on this station: the numbers behind the Performance page.
//!
//! Always on, in memory, of fixed size. Nothing here is written to disk and
//! no decision the station makes reads it: it measures and is shown. What is
//! kept is lengths of time, counts, the name of a method and of a station.
//! Never a parameter, a message, a path or anything a pane shows.
//!
//! A histogram is 128 counters per ten seconds, kept for five minutes; to
//! record is to add one to a counter. Each is about 16 KB, and a station
//! keeps 11 of them and two per station of its fleet (64 at most).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::daemon::{Caller, Daemon, Result};
use crate::paths::now_ms;

const BUCKETS: usize = 128;
/// The ten seconds being filled, and the thirty before them.
const WINDOWS: usize = 31;
const WINDOW_SECS: u64 = 10;
/// What "the last five minutes" is, in windows.
pub const FIVE_MIN: usize = 30;
const ONE_MIN: usize = 6;
/// How many stations' links are kept.
const MAX_LINKS: usize = 64;
/// A clock reading older than this says nothing about now.
const CLOCK_STALE_SECS: u64 = 60;
/// How far apart two clocks may be before signed calls are refused
/// (`trust.rs`): the page says so in words from here.
pub const CLOCK_LIMIT_MS: i64 = 120_000;
/// Requests that took less than this are never among "the slowest".
const SLOW_FLOOR: Duration = Duration::from_millis(20);
const SLOW_KEPT: usize = 16;
/// Calls that wait on purpose: for an agent to finish, for a task.
const WAITS: &[&str] = &["wait", "task.wait"];
/// A hook can be held for the owner's answer, so it is no "slow request".
const HELD: &[&str] = &["hook.ingest", "trust.approve", "fleet.join"];

/// Seconds on a clock that only goes forward, from the first time it is read.
pub fn secs() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs()
}

/// Which counter a length of time (in microseconds) is counted in: four to
/// each doubling, so a figure read back is within about an eighth of what
/// was measured, from a microsecond to over two hours.
fn bucket(micros: u64) -> usize {
    if micros < 4 {
        return micros as usize;
    }
    let exp = 63 - micros.leading_zeros() as usize;
    let sub = ((micros >> (exp - 2)) & 3) as usize;
    ((exp - 1) * 4 + sub).min(BUCKETS - 1)
}

/// The shortest time counted in a bucket.
fn bucket_floor(i: usize) -> u64 {
    if i < 4 {
        return i as u64;
    }
    ((4 + i % 4) as u64) << (i / 4 - 1)
}

struct Window {
    /// Which ten seconds this holds, counted from one; 0 is never used.
    epoch: AtomicU64,
    max: AtomicU64,
    buckets: [AtomicU32; BUCKETS],
}

/// How long something took, over the last five minutes.
pub struct Hist {
    windows: Box<[Window]>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Summary {
    pub count: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub max_us: u64,
}

impl Summary {
    pub fn json(&self) -> Value {
        let ms = |us: u64| (us as f64 / 10.0).round() / 100.0;
        if self.count == 0 {
            return json!({"count": 0});
        }
        json!({"count": self.count, "p50_ms": ms(self.p50_us), "p95_ms": ms(self.p95_us), "max_ms": ms(self.max_us)})
    }
}

impl Default for Hist {
    fn default() -> Self {
        Hist {
            windows: (0..WINDOWS)
                .map(|_| Window {
                    epoch: AtomicU64::new(0),
                    max: AtomicU64::new(0),
                    buckets: std::array::from_fn(|_| AtomicU32::new(0)),
                })
                .collect(),
        }
    }
}

impl Hist {
    pub fn record(&self, took: Duration) {
        self.record_at(secs(), took.as_micros() as u64);
    }

    /// A window that turns over is emptied by whoever gets to it first. A
    /// sample recorded in that same instant by another thread can be lost;
    /// that is accepted, for the sake of no lock here.
    pub fn record_at(&self, sec: u64, micros: u64) {
        let epoch = sec / WINDOW_SECS + 1;
        let w = &self.windows[(epoch % WINDOWS as u64) as usize];
        if w.epoch.load(Relaxed) != epoch && w.epoch.swap(epoch, Relaxed) != epoch {
            for b in w.buckets.iter() {
                b.store(0, Relaxed);
            }
            w.max.store(0, Relaxed);
        }
        w.buckets[bucket(micros)].fetch_add(1, Relaxed);
        w.max.fetch_max(micros, Relaxed);
    }

    pub fn summary(&self, windows: usize) -> Summary {
        self.summary_at(secs(), windows)
    }

    /// Over the window being filled and the `windows` before it.
    pub fn summary_at(&self, sec: u64, windows: usize) -> Summary {
        let now = sec / WINDOW_SECS + 1;
        let mut counts = [0u64; BUCKETS];
        let (mut total, mut max) = (0u64, 0u64);
        for back in 0..=(windows.min(WINDOWS - 1) as u64) {
            let Some(epoch) = now.checked_sub(back).filter(|e| *e > 0) else {
                break;
            };
            let w = &self.windows[(epoch % WINDOWS as u64) as usize];
            if w.epoch.load(Relaxed) != epoch {
                continue;
            }
            for (i, b) in w.buckets.iter().enumerate() {
                let n = b.load(Relaxed) as u64;
                counts[i] += n;
                total += n;
            }
            max = max.max(w.max.load(Relaxed));
        }
        // The value below which this share of the samples fall: the middle
        // of its bucket, and never more than the longest one seen.
        let at = |share: f64| -> u64 {
            let want = ((total as f64) * share).ceil().max(1.0) as u64;
            let mut seen = 0;
            for (i, n) in counts.iter().enumerate() {
                seen += n;
                if seen >= want {
                    let (lo, hi) = (bucket_floor(i), bucket_floor(i + 1));
                    return (lo + (hi - lo) / 2).min(max);
                }
            }
            max
        };
        if total == 0 {
            return Summary::default();
        }
        Summary {
            count: total,
            p50_us: at(0.50),
            p95_us: at(0.95),
            max_us: max,
        }
    }
}

/// How often something happened: in the last five minutes, and since start.
#[derive(Default)]
pub struct Count {
    total: AtomicU64,
    windows: [(AtomicU64, AtomicU32); WINDOWS],
}

impl Count {
    pub fn add(&self) {
        self.add_at(secs());
    }

    pub fn add_at(&self, sec: u64) {
        self.total.fetch_add(1, Relaxed);
        let epoch = sec / WINDOW_SECS + 1;
        let (at, n) = &self.windows[(epoch % WINDOWS as u64) as usize];
        if at.load(Relaxed) != epoch && at.swap(epoch, Relaxed) != epoch {
            n.store(0, Relaxed);
        }
        n.fetch_add(1, Relaxed);
    }

    pub fn total(&self) -> u64 {
        self.total.load(Relaxed)
    }

    pub fn recent_at(&self, sec: u64, windows: usize) -> u64 {
        let now = sec / WINDOW_SECS + 1;
        (0..=(windows.min(WINDOWS - 1) as u64))
            .filter_map(|back| now.checked_sub(back).filter(|e| *e > 0))
            .map(|epoch| {
                let (at, n) = &self.windows[(epoch % WINDOWS as u64) as usize];
                if at.load(Relaxed) == epoch {
                    n.load(Relaxed) as u64
                } else {
                    0
                }
            })
            .sum()
    }

    fn json(&self) -> Value {
        json!({"last_5m": self.recent_at(secs(), FIVE_MIN), "since_start": self.total()})
    }
}

/// How a request reached the station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A Control Deck's connection (one WebSocket per browser tab).
    Deck = 0,
    /// One HTTP request to the API.
    Http = 1,
    /// Another station's signed call.
    Station = 2,
    /// The local socket: the command line, hooks and agents' tools.
    Local = 3,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::Deck, Kind::Http, Kind::Station, Kind::Local];
    fn name(self) -> &'static str {
        ["deck", "http", "station", "local"][self as usize]
    }
}

struct Slow {
    at_sec: u64,
    at_ms: u64,
    micros: u64,
    kind: Kind,
    method: String,
    station: Option<String>,
}

/// One reading of another station's clock against this one's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockSample {
    pub at_sec: u64,
    /// This station's own clock when the reading was taken. A reading
    /// whose age by this clock and by `at_sec` differ was taken before
    /// this station slept or its clock was set: stops.rs does not use it.
    pub at_ms: u64,
    /// Theirs minus ours, taking the answer to have been written halfway
    /// through the round trip.
    pub offset_ms: i64,
    pub rtt_ms: u64,
}

/// What is known of the difference between two clocks: theirs minus ours,
/// and how far off that figure can be (half the round trip it was read in).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clock {
    pub diff_ms: i64,
    pub bound_ms: u64,
    pub age_secs: u64,
}

impl Clock {
    /// The clocks are surely further apart than signed calls allow.
    pub fn refused(&self) -> bool {
        self.diff_ms.unsigned_abs() > CLOCK_LIMIT_MS as u64 + self.bound_ms
    }
}

/// The best of the last readings: the one taken in the shortest round trip,
/// since its halfway guess is off by the least. A reading older than a
/// minute is not used; with none left, nothing is known.
pub fn clock_estimate(samples: &[ClockSample], now_sec: u64) -> Option<Clock> {
    samples
        .iter()
        .filter(|s| now_sec.saturating_sub(s.at_sec) <= CLOCK_STALE_SECS)
        .min_by_key(|s| s.rtt_ms)
        .map(|s| Clock {
            diff_ms: s.offset_ms,
            bound_ms: s.rtt_ms / 2 + 1,
            age_secs: now_sec.saturating_sub(s.at_sec),
        })
}

/// "5 minutes", "45 seconds": a clock difference in words.
pub fn clock_words(diff_ms: i64) -> String {
    let secs = diff_ms.unsigned_abs() / 1000;
    let (n, unit) = match secs {
        s if s >= 5400 => ((s + 1800) / 3600, "hour"),
        s if s >= 90 => ((s + 30) / 60, "minute"),
        s => (s, "second"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

#[derive(Default)]
struct Link {
    station: String,
    /// online | unreachable | update_needed | not_linked, and since when.
    state: String,
    since_ms: u64,
    was: Option<String>,
    /// `station.hello`, which does no work there: the link itself.
    rtt: Hist,
    last_rtt_us: Option<u64>,
    /// Every call made to it, with the time it took there.
    calls: Hist,
    fails: Count,
    last_fail: Option<(u64, &'static str)>,
    last_ok_ms: u64,
    clock: Vec<ClockSample>,
    /// This station's rounds with it (rounds.rs): how long each took, and
    /// those in which it did not answer.
    round: Hist,
    round_silent: Count,
}

/// One kind of periodic work (loops.rs).
pub struct LoopStat {
    every_ms: u64,
    local: bool,
    /// How much later than its time a run began, and how long it took.
    pub late: Hist,
    pub work: Hist,
    /// Times its work panicked and it was started again.
    pub panics: Count,
}

#[derive(Default)]
pub struct Perf {
    handled: [Hist; 4],
    /// A Control Deck's requests are taken one at a time: how long one
    /// waited behind the ones before it on its connection.
    waited: Hist,
    slow: Mutex<Vec<Slow>>,
    /// The periodic work that waits for no other machine (screens, mail
    /// wake-ups, the archive, recipes, upkeep): how late a run began, and
    /// how long it took. With the switch `split_tick` off, all of the
    /// periodic work, as before.
    pub tick_late: Hist,
    pub tick_work: Hist,
    /// Each kind of periodic work by itself.
    loops: Mutex<BTreeMap<&'static str, Arc<LoopStat>>>,
    /// One look at the fleet.
    pub look: Hist,
    /// A client that fell behind a pane's output and was repainted.
    pub resyncs: Count,
    /// A message for another station: sent here, to stored there.
    pub msg_received: Hist,
    /// A message stored here, to its agent being told of it.
    pub msg_woken: Hist,
    links: Mutex<BTreeMap<String, Link>>,
}

impl Perf {
    /// A request was handled. `inner` is the method a gateway call carried,
    /// and `station` the station it was for.
    pub fn request(
        &self,
        kind: Kind,
        method: &str,
        inner: Option<&str>,
        station: Option<&str>,
        took: Duration,
        waited: Option<Duration>,
    ) {
        if let Some(w) = waited {
            self.waited.record(w);
        }
        if WAITS.contains(&method) {
            return;
        }
        self.handled[kind as usize].record(took);
        if took < SLOW_FLOOR || HELD.contains(&method) || inner.is_some_and(|m| WAITS.contains(&m))
        {
            return;
        }
        // A method's name is whatever the caller sent: kept short and plain.
        let clean = |s: &str| -> String {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                .take(48)
                .collect()
        };
        let (now, micros) = (secs(), took.as_micros() as u64);
        let mut slow = self.slow.lock().unwrap();
        slow.retain(|s| now.saturating_sub(s.at_sec) <= 300);
        if slow.len() >= SLOW_KEPT {
            let (least, _) = slow
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.micros)
                .expect("not empty");
            if slow[least].micros >= micros {
                return;
            }
            slow.swap_remove(least);
        }
        slow.push(Slow {
            at_sec: now,
            at_ms: now_ms(),
            micros,
            kind,
            method: match inner {
                Some(inner) => format!("{} > {}", clean(method), clean(inner)),
                None => clean(method),
            },
            station: station.map(clean),
        });
    }

    /// The counters of one kind of periodic work, made when first asked for.
    pub fn loop_stat(&self, name: &'static str, every: Duration, local: bool) -> Arc<LoopStat> {
        let mut loops = self.loops.lock().unwrap();
        loops
            .entry(name)
            .or_insert_with(|| {
                Arc::new(LoopStat {
                    every_ms: every.as_millis() as u64,
                    local,
                    late: Hist::default(),
                    work: Hist::default(),
                    panics: Count::default(),
                })
            })
            .clone()
    }

    /// A round with the station at `addr` ended: how long it took, and
    /// whether the station answered. Only for a station on the list.
    pub fn link_round(&self, addr: &str, took: Duration, answered: bool) {
        let mut links = self.links.lock().unwrap();
        let Some(link) = links.get_mut(addr) else {
            return;
        };
        link.round.record(took);
        if !answered {
            link.round_silent.add();
        }
    }

    /// After a look: the stations listed, and when each last changed state.
    /// A link is kept only for a station on the list.
    pub fn links_follow(&self, peers: &[(String, String, String)]) {
        let mut links = self.links.lock().unwrap();
        links.retain(|addr, _| peers.iter().any(|(a, _, _)| a == addr));
        for (addr, station, state) in peers.iter().take(MAX_LINKS) {
            let link = links.entry(addr.clone()).or_default();
            link.station = station.clone();
            if link.state != *state {
                link.was = (!link.state.is_empty()).then(|| std::mem::take(&mut link.state));
                link.state = state.clone();
                link.since_ms = now_ms();
            }
        }
    }

    /// A call to another station came back (whatever it answered), or did
    /// not: `problem` says how. Only for a station on the list.
    pub fn link_call(&self, addr: &str, took: Duration, problem: Option<&'static str>) {
        let mut links = self.links.lock().unwrap();
        let Some(link) = links.get_mut(addr) else {
            return;
        };
        match problem {
            None => {
                link.calls.record(took);
                link.last_ok_ms = now_ms();
            }
            Some(code) => {
                link.fails.add();
                link.last_fail = Some((now_ms(), code));
            }
        }
    }

    /// `station.hello` was answered: the round trip, and the clock it said.
    /// `sent_ms` is this station's clock when the question left.
    pub fn link_hello(&self, addr: &str, took: Duration, sent_ms: u64, their_now_ms: Option<u64>) {
        let mut links = self.links.lock().unwrap();
        let Some(link) = links.get_mut(addr) else {
            return;
        };
        link.rtt.record(took);
        link.last_rtt_us = Some(took.as_micros() as u64);
        if let Some(theirs) = their_now_ms {
            let rtt_ms = took.as_millis() as u64;
            if link.clock.len() >= 8 {
                link.clock.remove(0);
            }
            link.clock.push(ClockSample {
                at_sec: secs(),
                at_ms: sent_ms + rtt_ms,
                offset_ms: theirs as i64 - (sent_ms + rtt_ms / 2) as i64,
                rtt_ms,
            });
        }
    }

    /// The readings of a station's clock, oldest first (their times are
    /// counted on `secs`). A stop's cut-off is converted with them.
    pub fn clock_samples(&self, station: &str) -> Vec<ClockSample> {
        let links = self.links.lock().unwrap();
        links
            .values()
            .find(|l| l.station == station)
            .map(|l| l.clock.clone())
            .unwrap_or_default()
    }

    /// What is known of a station's clock against this one's.
    pub fn clock_of(&self, station: &str) -> Option<Clock> {
        let links = self.links.lock().unwrap();
        let link = links.values().find(|l| l.station == station)?;
        clock_estimate(&link.clock, secs())
    }

    fn links_view(&self) -> Vec<Value> {
        let now = secs();
        let links = self.links.lock().unwrap();
        links
            .iter()
            .map(|(addr, l)| {
                let clock = clock_estimate(&l.clock, now);
                json!({
                    "station": l.station, "addr": addr, "state": l.state, "since_ms": l.since_ms, "was": l.was,
                    "rtt": l.rtt.summary_at(now, FIVE_MIN).json(),
                    "rtt_now_ms": l.last_rtt_us.map(|us| (us as f64 / 10.0).round() / 100.0),
                    "calls": l.calls.summary_at(now, FIVE_MIN).json(),
                    "failures": {
                        "last_5m": l.fails.recent_at(now, FIVE_MIN), "since_start": l.fails.total(),
                        "last": l.last_fail.map(|(at, code)| json!({"at_ms": at, "why": code})),
                    },
                    "last_ok_ms": (l.last_ok_ms > 0).then_some(l.last_ok_ms),
                    "round": {
                        "took": l.round.summary_at(now, FIVE_MIN).json(),
                        "silent": {"last_5m": l.round_silent.recent_at(now, FIVE_MIN), "since_start": l.round_silent.total()},
                    },
                    "clock": clock.map(|c| json!({"diff_ms": c.diff_ms, "bound_ms": c.bound_ms, "age_secs": c.age_secs, "refused": c.refused()})),
                })
            })
            .collect()
    }

    fn view(&self) -> Value {
        let now = secs();
        let handled: serde_json::Map<String, Value> = Kind::ALL
            .iter()
            .map(|k| {
                (
                    k.name().to_string(),
                    self.handled[*k as usize].summary_at(now, FIVE_MIN).json(),
                )
            })
            .collect();
        let mut slow: Vec<&Slow> = vec![];
        let held = self.slow.lock().unwrap();
        slow.extend(held.iter().filter(|s| now.saturating_sub(s.at_sec) <= 300));
        slow.sort_by_key(|a| std::cmp::Reverse(a.micros));
        let slowest: Vec<Value> = slow
            .iter()
            .take(5)
            .map(|s| json!({"method": s.method, "kind": s.kind.name(), "station": s.station, "ms": (s.micros as f64 / 10.0).round() / 100.0, "at_ms": s.at_ms}))
            .collect();
        let mut late = self.tick_late.summary_at(now, FIVE_MIN).json();
        late["worst_1m_ms"] =
            json!((self.tick_late.summary_at(now, ONE_MIN).max_us as f64 / 10.0).round() / 100.0);
        let loops: serde_json::Map<String, Value> = self
            .loops
            .lock()
            .unwrap()
            .iter()
            .map(|(name, l)| {
                let v = json!({
                    "every_ms": l.every_ms, "local": l.local,
                    "late": l.late.summary_at(now, FIVE_MIN).json(),
                    "work": l.work.summary_at(now, FIVE_MIN).json(),
                    "panics": l.panics.json(),
                });
                (name.to_string(), v)
            })
            .collect();
        json!({
            "requests": {"handled": handled, "waited": {"deck": self.waited.summary_at(now, FIVE_MIN).json()}, "slowest": slowest},
            "periodic": {"late": late, "work": self.tick_work.summary_at(now, FIVE_MIN).json(), "loops": loops},
            "look": self.look.summary_at(now, FIVE_MIN).json(),
            "terminal": {"resyncs": self.resyncs.json()},
            "messages": {
                "received": self.msg_received.summary_at(now, FIVE_MIN).json(),
                "woken": self.msg_woken.summary_at(now, FIVE_MIN).json(),
            },
            "links": self.links_view(),
        })
    }
}

impl Daemon {
    /// `dispatch`, timed. Each of the four ways in calls this once per
    /// request; a call a request makes of its own goes to `dispatch`, so
    /// nothing is counted twice. `waited`: how long it stood behind other
    /// requests on its connection, where that is known.
    pub async fn dispatch_timed(
        &self,
        kind: Kind,
        waited: Option<Duration>,
        caller: &Caller,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        self.dispatch_timed_rid(kind, waited, caller, method, params, None)
            .await
    }

    /// `dispatch_timed` for a request that came with an id beside its
    /// token (rids.rs): a change is done once, a second try answered.
    pub async fn dispatch_timed_rid(
        &self,
        kind: Kind,
        waited: Option<Duration>,
        caller: &Caller,
        method: &str,
        params: Value,
        retry: Option<crate::rids::Retry>,
    ) -> Result<Value> {
        let gateway = matches!(method, "peer.call" | "rpc.forward");
        let text = |key: &str| {
            gateway
                .then(|| params.get(key).and_then(|v| v.as_str()).map(String::from))
                .flatten()
        };
        let (inner, station) = (text("method"), text("station"));
        let began = Instant::now();
        let out = self.dispatch_rid(caller, method, params, retry).await;
        self.perf.request(
            kind,
            method,
            inner.as_deref(),
            station.as_deref(),
            began.elapsed(),
            waited,
        );
        out
    }

    /// The station was started, and why, for the ship's log: an upgrade or
    /// a restart in place says so itself (`station.upgraded`).
    pub(crate) fn note_started(&self, resumed: usize) {
        // What the log last says of this station's own running: a clean stop
        // writes `station.stopped`; a crash, a kill or a power cut nothing.
        let last = self
            .db
            .log_query(None, Some("station."), 0, 200)
            .into_iter()
            .find(|e| {
                matches!(
                    e["kind"].as_str(),
                    Some("station.stopped" | "station.started" | "station.upgraded")
                )
            });
        let why = match last.as_ref().and_then(|e| e["kind"].as_str()) {
            None => "first start",
            Some("station.stopped") => "started after it was stopped",
            Some(_) => "started after it ended without being stopped (a crash, a kill or the machine going down)",
        };
        self.record(
            "station.started",
            None,
            None,
            None,
            None,
            json!({"version": crate::daemon::VERSION, "why": why, "resumed": resumed, "clean": last.is_none() || why.starts_with("started after it was")}),
        );
    }

    /// The station is stopping because it was asked to.
    pub fn note_stopping(&self, why: &str) {
        // The next start is told from one that followed a crash.
        self.restore_stopping();
        self.record(
            "station.stopped",
            None,
            None,
            None,
            None,
            json!({"why": why}),
        );
    }

    /// Starts, upgrades and restarts in the last day, newest first, from
    /// the ship's log: what the page shows as "restarts, and why".
    fn restarts(&self) -> Value {
        let since = now_ms().saturating_sub(24 * 3_600_000);
        let hour = now_ms().saturating_sub(3_600_000);
        let all: Vec<Value> = self
            .db
            .log_query(None, Some("station."), since, 500)
            .into_iter()
            .filter_map(|e| {
                let d = &e["data"];
                let (what, why) = match e["kind"].as_str()? {
                    "station.started" => ("started", d["why"].as_str().unwrap_or("").to_string()),
                    "station.upgraded" if d["from"] == d["to"] => {
                        ("restarted", "restarted in place; every terminal kept".to_string())
                    }
                    "station.upgraded" => (
                        "upgraded",
                        format!(
                            "{} to {}",
                            d["from"].as_str().unwrap_or("?"),
                            d["to"].as_str().unwrap_or("?")
                        ),
                    ),
                    "station.rolled_back" => (
                        "went back",
                        format!(
                            "the new version did not start properly: {}",
                            d["reason"].as_str().unwrap_or("")
                        ),
                    ),
                    _ => return None,
                };
                Some(json!({"at_ms": e["ts"], "what": what, "why": why,
                    "panes": d.get("panes"), "kept": d.get("adopted"), "resumed": d.get("resumed")}))
            })
            .collect();
        // Going back is said twice by the log (the handoff, and the reason):
        // it is one restart.
        let counted = |from: u64| {
            all.iter()
                .filter(|r| r["what"] != json!("went back") && r["at_ms"].as_u64() >= Some(from))
                .count()
        };
        json!({"last_24h": counted(since), "last_hour": counted(hour), "list": all.iter().take(12).collect::<Vec<_>>()})
    }

    /// Messages for other stations that have not got there: how many, and
    /// how long the oldest has waited (by this station's clock throughout).
    fn messages_waiting(&self) -> Vec<Value> {
        let now = now_ms();
        self.db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT o.dest, COUNT(*), MIN(m.ts) FROM outbox o JOIN messages m ON m.id=o.msg
                     WHERE o.state='pending' GROUP BY o.dest ORDER BY o.dest LIMIT 64",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok(json!({
                        "station": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?,
                        "oldest_ms": now.saturating_sub(r.get::<_, i64>(2)? as u64),
                    }))
                })?;
                rows.collect()
            })
            .unwrap_or_default()
    }

    /// `perf.get`: this station's own numbers. The owner reads it, and
    /// another station may, so that any Control Deck shows the whole fleet;
    /// an agent may not (rule 7 of AGENTS.md).
    pub fn perf_view(&self) -> Value {
        let mut v = self.perf.view();
        v["v"] = json!(1);
        v["station"] = json!(self.station);
        v["version"] = json!(crate::daemon::VERSION);
        v["now_ms"] = json!(now_ms());
        v["started_ms"] = json!(self.started_ms);
        v["restarts"] = self.restarts();
        // What the last restart in a day left: sessions started again,
        // ended, held back. And the switches in force on this station.
        v["recovered"] = self.last_recovered();
        v["flags"] = self.flags_view()["flags"].clone();
        // The database's writer: what waits, what was dropped, whether it
        // can write (db.rs).
        v["database"] = json!(self.db.stats());
        v["messages"]["waiting"] = json!(self.messages_waiting());
        v["panes"] = json!(self
            .list_panes()
            .iter()
            .filter(|p| p.meta.lock().unwrap().exit.is_none())
            .count());
        // Which way each link goes, as this station's Tailscale client has it.
        let tailnet = self.tailnet.get();
        let rounds = self.rounds.view();
        if let Some(links) = v["links"].as_array_mut() {
            for link in links {
                let station = link["station"].as_str().unwrap_or_default().to_string();
                let device = self
                    .fleet
                    .peer(&station)
                    .and_then(|p| self.tailnet_device_of(&p, tailnet.as_ref()));
                // Its place in this station's schedule of rounds.
                if let Some(at) = link["addr"].as_str().and_then(|a| rounds.get(a)) {
                    for (k, v) in at.as_object().into_iter().flatten() {
                        link["round"][k] = v.clone();
                    }
                }
                link["path"] = json!(device.as_ref().and_then(|d| d.path.clone()));
                link["relay"] = json!(device.as_ref().and_then(|d| d.relay.clone()));
            }
        }
        v["tailnet"] = self.tailnet_view();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Performance page (the owner's request, 15 Nov 2026) reads these:
    /// a figure must be near what was measured, and must be of the last
    /// five minutes, not of all time. No clock is read here: time is given.
    #[test]
    fn a_histogram_reports_the_last_five_minutes_and_forgets_what_is_older() {
        let h = Hist::default();
        // 100 samples of 1 ms and five of 80 ms, in the same ten seconds.
        for _ in 0..100 {
            h.record_at(1000, 1_000);
        }
        for _ in 0..5 {
            h.record_at(1000, 80_000);
        }
        let s = h.summary_at(1005, FIVE_MIN);
        assert_eq!((s.count, s.max_us), (105, 80_000));
        assert!((900..=1_150).contains(&s.p50_us), "{s:?}");
        // 95% of 105 is the 100th: still a 1 ms sample. The longest is exact.
        assert!(s.p95_us <= 1_150, "{s:?}");
        for _ in 0..10 {
            h.record_at(1000, 80_000);
        }
        let s = h.summary_at(1005, FIVE_MIN);
        assert!((70_000..=80_000).contains(&s.p95_us), "{s:?}");
        // Four minutes on it is still counted; after five it is not, and
        // "the last minute" never had it.
        assert_eq!(h.summary_at(1000 + 240, FIVE_MIN).count, 115);
        assert_eq!(h.summary_at(1000 + 240, ONE_MIN).count, 0);
        assert_eq!(h.summary_at(1000 + 320, FIVE_MIN), Summary::default());
        // The same counters are used again five minutes later, emptied.
        h.record_at(1000 + 310, 2_000);
        let s = h.summary_at(1000 + 310, FIVE_MIN);
        assert_eq!((s.count, s.max_us), (1, 2_000));
        // Every length of time has a bucket, and a bucket's floor is in it.
        for us in [0, 1, 3, 4, 7, 8, 1_000, 123_456, 3_600_000_000, u64::MAX] {
            let b = bucket(us);
            assert!(b < BUCKETS && bucket_floor(b) <= us, "{us}");
            assert!(b == BUCKETS - 1 || us < bucket_floor(b + 1), "{us}");
        }
        // A count is of five minutes as well, and its total is of all time.
        let c = Count::default();
        c.add_at(50);
        c.add_at(55);
        assert_eq!((c.recent_at(60, FIVE_MIN), c.total()), (2, 2));
        assert_eq!((c.recent_at(400, FIVE_MIN), c.total()), (0, 2));
    }

    /// Audit §10: two stations whose clocks differ by more than two minutes
    /// called each other "unreachable", and nothing said "clock". The
    /// difference is read from an unsigned answer; this is the arithmetic.
    #[test]
    fn a_clock_difference_is_the_best_of_the_last_readings_and_goes_stale() {
        let s = |at_sec, offset_ms, rtt_ms| ClockSample {
            at_sec,
            at_ms: 0,
            offset_ms,
            rtt_ms,
        };
        // The reading from the shortest round trip wins: it is off the least.
        let readings = [
            s(100, 300_900, 800),
            s(110, 300_020, 40),
            s(120, 299_600, 300),
        ];
        let c = clock_estimate(&readings, 125).unwrap();
        assert_eq!((c.diff_ms, c.bound_ms, c.age_secs), (300_020, 21, 15));
        assert!(c.refused());
        assert_eq!(clock_words(c.diff_ms), "5 minutes");
        // A reading over a minute old is not used; with none left, unknown.
        let c = clock_estimate(&readings, 175).unwrap();
        assert_eq!(c.diff_ms, 299_600);
        assert_eq!(clock_estimate(&readings, 181), None);
        // Refused only when the difference is surely past two minutes: one
        // read in a slow round trip that may be under the limit is not.
        assert!(!clock_estimate(&[s(0, 121_000, 4_000)], 1)
            .unwrap()
            .refused());
        assert!(clock_estimate(&[s(0, -125_000, 4_000)], 1)
            .unwrap()
            .refused());
        assert!(!clock_estimate(&[s(0, 90_000, 10)], 1).unwrap().refused());
        assert_eq!(clock_words(-45_000), "45 seconds");
        assert_eq!(clock_words(7_200_000), "2 hours");
        assert_eq!(clock_words(61_000), "61 seconds");
    }

    /// What recording costs, for the design's "under 1%": a million samples.
    /// Nightly tier: it prints a time and asserts only a generous ceiling.
    #[test]
    #[ignore = "a timing: run by hand or nightly, `cargo test -p spandeck-core --release perf::tests::recording -- --ignored --nocapture`"]
    fn recording_a_million_samples_is_cheap() {
        let h = Hist::default();
        let began = Instant::now();
        for i in 0..1_000_000u64 {
            h.record_at(i / 100_000, i % 50_000);
        }
        let fixed = began.elapsed();
        let began = Instant::now();
        for i in 0..1_000_000u64 {
            h.record(Duration::from_micros(i % 50_000));
        }
        let clocked = began.elapsed();
        println!("1,000,000 samples: {fixed:?} with the time given, {clocked:?} reading the clock each time");
        assert!(h.summary(FIVE_MIN).count > 0);
        assert!(clocked < Duration::from_millis(500), "{clocked:?}");
    }
}
