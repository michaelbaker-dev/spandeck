//! Stop-all, apart from the daemon: the rows a station keeps for the stops
//! it issued, how one is sent, and how the station that is stopped decides
//! which work it was meant for. The link to another station and the clock
//! are given from outside (`StopLink`), so all of it runs under simulated
//! time and a simulated network (`tests/stop_sim.rs`).
//!
//! Guarantees (DECISIONS.md, "Stop, pause and offline"): three separate
//! states, and an unreachable station is never shown as stopped; a stop for
//! an unreachable station is delivered on reconnect and applies only to the
//! work that was running when it was issued.
//!
//! The cut-off. A stop's row keeps when it was issued, on the issuing
//! station's clock, and nothing else about time. At each send that moment is
//! converted to the stopped station's clock (`cutoff_ms`), from a reading of
//! the difference between the two clocks that is either standing and fresh
//! or taken just before the stop on the same connection. The stopped station
//! compares the cut-off with when each pane's work began, by its own clock,
//! so a stop that arrives late is still aimed at the moment it was pressed.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::db::Db;
use crate::perf::ClockSample;
use crate::relay::BoxFuture;

/// What a station that reads `cutoff_ms` announces (`caps`), and the name of
/// the switch (`[flags] stop_cutoff = false` turns it off: only `age_ms` is
/// sent and read, as before).
pub const CAP: &str = "stop_cutoff";
/// A standing reading of the other clock is used only when it is younger
/// than this…
pub const FRESH_SECS: u64 = 15;
/// …and the reading before it says the same to within this.
pub const AGREE_MS: u64 = 250;
/// How far the newest reading's age may differ between this station's own
/// clock and the one that only runs forward (which counts whole seconds).
const OWN_CLOCK_MS: i64 = 2000;
/// The reading before it is no older than this.
const BEFORE_SECS: u64 = 60;
/// One station's send, its clock reading included.
pub const SEND_LIMIT: Duration = Duration::from_secs(5);
/// How many applied stops are remembered in memory, in front of the
/// database's record of them.
const APPLIED_KEPT: usize = 512;

// ------------------------------------------------------------ being stopped

/// What a station knows of one pane, for deciding whether a stop is meant
/// for the work in it. All times are this station's own clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Work {
    /// An agent that has not ended and is working or waiting for the owner.
    pub busy: bool,
    pub created_ms: u64,
    pub state_since_ms: u64,
    /// When a hook last said a prompt was submitted. 0: none seen.
    pub turn_started_ms: u64,
    /// When the pane last went from not busy to busy. 0: a record from
    /// before this was kept.
    pub busy_since_ms: u64,
}

/// What a stop is aimed at, on the stopped station's clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Aim {
    /// The moment it was issued, converted by the issuing station.
    Cutoff(u64),
    /// Counted back from now by how old the issuing station said it was
    /// when it sent it (or, from a station older still, its own clock's
    /// time): late by however long the call was on the way.
    Age(u64),
}

impl Aim {
    pub fn ms(&self) -> u64 {
        match self {
            Aim::Cutoff(ms) | Aim::Age(ms) => *ms,
        }
    }
}

/// `cutoff_ms` is `None` when the issuing station sent none or this
/// station's switch is off. A cut-off is never taken to be later than
/// counting back by `age_ms` gives: that figure is already late by the time
/// the call took, so a later one can only come from a wrong clock reading,
/// and would stop work started after the stop.
pub fn aim(now_ms: u64, issued_ms: u64, age_ms: Option<u64>, cutoff_ms: Option<u64>) -> Aim {
    let by_age = age_ms.map(|age| now_ms.saturating_sub(age));
    match cutoff_ms {
        Some(cutoff) => Aim::Cutoff(by_age.map_or(cutoff, |a| a.min(cutoff))),
        None => Aim::Age(by_age.unwrap_or(issued_ms)),
    }
}

/// Is the stop meant for this pane's work?
pub fn hit(w: &Work, aim: &Aim) -> bool {
    if !w.busy {
        return false;
    }
    match *aim {
        // As it was before cut-offs.
        Aim::Age(issued) => {
            w.created_ms <= issued
                && w.state_since_ms <= issued.max(w.turn_started_ms)
                && w.turn_started_ms <= issued
        }
        Aim::Cutoff(cutoff) => {
            // When the work began: the later of the last submitted prompt
            // and the pane last becoming busy. An agent with no prompt hook
            // has only the second. With neither (a busy pane whose record is
            // from before the second was kept, and no prompt hook) the
            // start is not known, and the work is stopped.
            let began = match (w.turn_started_ms, w.busy_since_ms) {
                (0, 0) => None,
                (turn, busy) => Some(turn.max(busy)),
            };
            w.created_ms <= cutoff && began.is_none_or(|b| b <= cutoff)
        }
    }
}

#[derive(Default)]
struct Applied {
    order: VecDeque<String>,
    answers: HashMap<String, Value>,
}

impl Applied {
    fn put(&mut self, id: &str, answer: Value) {
        if self.answers.insert(id.to_string(), answer).is_none() {
            self.order.push_back(id.to_string());
            if self.order.len() > APPLIED_KEPT {
                if let Some(old) = self.order.pop_front() {
                    self.answers.remove(&old);
                }
            }
        }
    }
}

/// A stop is applied once, however often it arrives: a retry after a lost
/// reply, or a request that was on the way through a cut and arrives after
/// its retry. The ids applied are kept in memory, so this holds when the
/// database cannot be read or written; the database keeps them across a
/// restart.
pub struct StopApplier {
    db: Arc<Db>,
    applied: Mutex<Applied>,
}

impl StopApplier {
    pub fn new(db: Arc<Db>) -> StopApplier {
        StopApplier {
            db,
            applied: Mutex::new(Applied::default()),
        }
    }

    /// `stop` does the stopping and gives the answer; it runs only for an
    /// id not seen before. Returns the answer and whether this was the first
    /// arrival. A second arrival that comes while the first is being applied
    /// waits for it.
    pub fn apply(&self, id: &str, stop: impl FnOnce() -> Value) -> (Value, bool) {
        let mut applied = self.applied.lock().unwrap();
        if let Some(prev) = applied.answers.get(id) {
            return (prev.clone(), false);
        }
        let key = format!("stop:{id}");
        if let Some(prev) = self.db.kv_get(&key) {
            let prev = serde_json::from_str(&prev).unwrap_or(json!({"stopped": []}));
            applied.put(id, prev.clone());
            return (prev, false);
        }
        let answer = stop();
        applied.put(id, answer.clone());
        self.db.kv_set(&key, &answer.to_string());
        (answer, true)
    }
}

// ------------------------------------------------------------------ issuing

/// One target of one stop, as the issuing station keeps it.
#[derive(Debug, Clone)]
pub struct StopRow {
    pub id: String,
    pub station: String,
    pub scope: String,
    /// When it was issued, on the issuing station's clock.
    pub issued_ms: u64,
    pub by: String,
    /// requested | unreachable | confirmed
    pub status: String,
    pub updated_ms: u64,
    pub result: Value,
    /// It is in the database, so it outlives this process.
    pub saved: bool,
    /// The database has an older state of it than this.
    dirty: bool,
    /// A send is under way.
    trying: bool,
}

/// States only move forward: requested, unreachable, confirmed.
fn rank(status: &str) -> u8 {
    match status {
        "confirmed" => 2,
        "unreachable" => 1,
        _ => 0,
    }
}

const COLUMNS: &str = "id,station,scope,issued,issued_by,status,updated,result";

fn from_db(r: &rusqlite::Row<'_>) -> rusqlite::Result<StopRow> {
    let result: String = r.get(7)?;
    Ok(StopRow {
        id: r.get(0)?,
        station: r.get(1)?,
        scope: r.get(2)?,
        issued_ms: r.get::<_, i64>(3)? as u64,
        by: r
            .get::<_, Option<String>>(4)?
            .unwrap_or_else(|| "you".into()),
        status: r.get(5)?,
        updated_ms: r.get::<_, i64>(6)? as u64,
        result: serde_json::from_str(&result).unwrap_or(Value::Null),
        saved: true,
        dirty: false,
        trying: false,
    })
}

/// The stops this station issued. What is being worked on is held in
/// memory, where a state is changed first and at once; the database is
/// written after, and a write that fails is tried again (`save`). A row
/// leaves memory once it is confirmed and saved.
pub struct StopBook {
    db: Arc<Db>,
    station: String,
    rows: Mutex<Vec<StopRow>>,
}

impl StopBook {
    pub fn new(db: Arc<Db>, station: &str) -> StopBook {
        StopBook {
            db,
            station: station.to_string(),
            rows: Mutex::new(vec![]),
        }
    }

    /// A row per target, in memory. Nothing is waited for.
    pub fn issue(&self, targets: &[String], scope: &str, by: &str, issued_ms: u64) -> Vec<String> {
        let mut rows = self.rows.lock().unwrap();
        targets
            .iter()
            .map(|station| {
                let id = crate::paths::new_id("x");
                rows.push(StopRow {
                    id: id.clone(),
                    station: station.clone(),
                    scope: scope.to_string(),
                    issued_ms,
                    by: by.to_string(),
                    status: "requested".into(),
                    updated_ms: issued_ms,
                    result: json!({}),
                    saved: false,
                    dirty: true,
                    trying: false,
                });
                id
            })
            .collect()
    }

    /// Move a row to a state. Never back: a failure that comes in after the
    /// station confirmed (an earlier request's time limit, say) changes
    /// nothing. Returns whether the row changed.
    pub fn mark(&self, id: &str, status: &str, result: Value, now_ms: u64) -> bool {
        let mut rows = self.rows.lock().unwrap();
        let Some(row) = rows.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        if row.status == "confirmed" || rank(status) < rank(&row.status) {
            return false;
        }
        row.status = status.to_string();
        row.result = result;
        row.updated_ms = now_ms;
        row.dirty = true;
        true
    }

    /// Write what the database does not have yet. True when nothing is left
    /// unwritten. Blocking: the daemon calls it off its request threads.
    pub fn save(&self) -> bool {
        let dirty: Vec<StopRow> = {
            let rows = self.rows.lock().unwrap();
            rows.iter().filter(|r| r.dirty).cloned().collect()
        };
        if !dirty.is_empty() {
            let wrote = self.db.with_tx(|tx| {
                for r in &dirty {
                    tx.execute(
                        &format!(
                            "INSERT INTO stops({COLUMNS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                             ON CONFLICT(id) DO UPDATE SET status=excluded.status,
                               updated=excluded.updated, result=excluded.result
                             WHERE stops.status != 'confirmed'"
                        ),
                        rusqlite::params![
                            r.id,
                            r.station,
                            r.scope,
                            r.issued_ms as i64,
                            r.by,
                            r.status,
                            r.updated_ms as i64,
                            r.result.to_string()
                        ],
                    )?;
                }
                Ok(())
            });
            if wrote.is_err() {
                return false;
            }
        }
        let mut rows = self.rows.lock().unwrap();
        for d in &dirty {
            if let Some(r) = rows.iter_mut().find(|r| r.id == d.id) {
                r.saved = true;
                if r.status == d.status && r.updated_ms == d.updated_ms {
                    r.dirty = false;
                }
            }
        }
        rows.retain(|r| r.dirty || r.trying || r.status != "confirmed");
        rows.iter().all(|r| !r.dirty)
    }

    /// Rows that wait in the database and are not in memory: after a
    /// restart, the stops still to deliver. Blocking, like `save`.
    pub fn load_waiting(&self) {
        let waiting: Vec<StopRow> = self
            .db
            .with(|c| {
                let mut stmt = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM stops WHERE status IN ('requested','unreachable') AND station != ?1"
                ))?;
                let rows = stmt.query_map([&self.station], from_db)?;
                rows.collect()
            })
            .unwrap_or_default();
        let mut rows = self.rows.lock().unwrap();
        for w in waiting {
            if !rows.iter().any(|r| r.id == w.id) {
                rows.push(w);
            }
        }
    }

    /// Rows for other stations that are not confirmed and not being sent.
    pub fn waiting(&self) -> Vec<StopRow> {
        let rows = self.rows.lock().unwrap();
        rows.iter()
            .filter(|r| r.status != "confirmed" && !r.trying && r.station != self.station)
            .cloned()
            .collect()
    }

    /// How many rows are not in the database yet.
    pub fn unsaved(&self) -> usize {
        self.rows.lock().unwrap().iter().filter(|r| r.dirty).count()
    }

    /// A stop that cannot be delivered (its station is not in the fleet) is
    /// taken out, not left saying it will be.
    pub fn drop_row(&self, id: &str) {
        self.rows.lock().unwrap().retain(|r| r.id != id);
        let _ = self
            .db
            .with(|c| c.execute("DELETE FROM stops WHERE id=?1", [id]));
    }

    /// A station was removed from the fleet: what waited for it in memory
    /// goes (the database's rows are the caller's to delete).
    pub fn forget_station(&self, station: &str) {
        self.rows
            .lock()
            .unwrap()
            .retain(|r| r.station != station || r.status == "confirmed");
    }

    fn begin(&self, id: &str) -> Option<StopRow> {
        let mut rows = self.rows.lock().unwrap();
        let row = rows
            .iter_mut()
            .find(|r| r.id == id && r.status != "confirmed" && !r.trying)?;
        row.trying = true;
        Some(row.clone())
    }

    /// The newest stops, as the Control Deck shows them: the database's
    /// rows, with what memory knows that the database does not.
    pub fn list(&self, limit: u32, now_ms: u64) -> Vec<Value> {
        let mut all: Vec<StopRow> = self
            .db
            .with(|c| {
                let mut stmt = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM stops ORDER BY issued DESC, rowid DESC LIMIT ?1"
                ))?;
                let rows = stmt.query_map([limit], from_db)?;
                rows.collect()
            })
            .unwrap_or_default();
        let mut fresh = vec![];
        for m in self.rows.lock().unwrap().iter() {
            match all.iter_mut().find(|r| r.id == m.id) {
                Some(r) => {
                    *r = StopRow {
                        saved: true,
                        ..m.clone()
                    }
                }
                None => fresh.push(m.clone()),
            }
        }
        fresh.reverse();
        fresh.append(&mut all);
        fresh.sort_by_key(|r| std::cmp::Reverse(r.issued_ms));
        fresh.truncate(limit as usize);
        fresh
            .iter()
            .map(|r| {
                json!({
                    "id": r.id, "station": r.station, "scope": r.scope,
                    // The station that issued it, which keeps the row and delivers it,
                    // and how long ago that was by its own clock.
                    "from": self.station,
                    "age_ms": now_ms.saturating_sub(r.issued_ms),
                    "issued_ms": r.issued_ms, "by": r.by,
                    "label": match r.status.as_str() { "confirmed" => "stop confirmed", "unreachable" => "station unreachable", _ => "stop requested" },
                    "status": r.status, "updated_ms": r.updated_ms,
                    "result": r.result,
                    // False: held in memory only. If this station's daemon ends
                    // before it is written, a stop still waiting is not delivered.
                    "saved": r.saved,
                })
            })
            .collect()
    }
}

/// What `station.hello` said.
#[derive(Debug, Clone, Default)]
pub struct Hello {
    pub station: Option<String>,
    /// Its clock when it answered.
    pub now_ms: Option<u64>,
    /// It announces that it reads `cutoff_ms`.
    pub cutoff: bool,
}

/// What sending a stop needs of the world: this station's clock, what it
/// knows of the other station, and the two calls.
pub trait StopLink: Send + Sync {
    /// This station's clock.
    fn now_ms(&self) -> u64;
    /// The readings of that station's clock against this one's, oldest
    /// first, and the present on the clock their `at_sec` is counted on.
    fn samples(&self, station: &str) -> (Vec<ClockSample>, u64);
    /// How the station is listed (online, unreachable, update_needed,
    /// not_linked), and whether it announced that it reads `cutoff_ms`
    /// when it last answered.
    fn peer(&self, station: &str) -> Option<(String, bool)>;
    /// `station.hello`. An implementation keeps the reading it gives.
    fn hello<'a>(&'a self, station: &'a str) -> BoxFuture<'a, Result<Hello, String>>;
    /// `stop.apply`, signed. `Err`: not reached, or refused.
    fn apply<'a>(&'a self, station: &'a str, params: Value)
        -> BoxFuture<'a, Result<Value, String>>;
}

/// The standing difference between the clocks (theirs minus ours), if it
/// may be used: the newest reading is under 15 seconds old and the one
/// before it agrees to within 250 ms. A clock that stepped between the two
/// makes them disagree, and then the clock is asked again. So it is when
/// the newest reading's age by this station's own clock is not its age by
/// the clock that only runs forward: this station's clock was set since, or
/// the machine slept.
pub fn standing(samples: &[ClockSample], now_sec: u64, now_ms: u64) -> Option<i64> {
    let [.., before, newest] = samples else {
        return None;
    };
    let age_ms = now_sec.saturating_sub(newest.at_sec) as i64 * 1000;
    let by_own_clock = now_ms as i64 - newest.at_ms as i64;
    (now_sec.saturating_sub(newest.at_sec) < FRESH_SECS
        && (by_own_clock - age_ms).abs() <= OWN_CLOCK_MS
        && now_sec.saturating_sub(before.at_sec) <= BEFORE_SECS
        && newest.offset_ms.abs_diff(before.offset_ms) <= AGREE_MS)
        .then_some(newest.offset_ms)
}

/// The difference to convert with, or `None` towards a station that does
/// not read a cut-off. `Err`: the station did not answer.
async fn difference(link: &dyn StopLink, row: &StopRow) -> Result<Option<i64>, String> {
    // What the list says of a station counts only while it answers.
    let announces = link
        .peer(&row.station)
        .filter(|(state, _)| state == "online" || state == "update_needed")
        .map(|(_, cutoff)| cutoff);
    match announces {
        // An older station: nothing is asked first, `age_ms` as before.
        Some(false) => return Ok(None),
        // The first send to a station that answers: the standing reading,
        // if it is fresh. After a send that failed (the station was away,
        // or the request may still be on its way), always a new reading.
        Some(true) if row.status == "requested" => {
            let (samples, now_sec) = link.samples(&row.station);
            if let Some(diff) = standing(&samples, now_sec, link.now_ms()) {
                return Ok(Some(diff));
            }
        }
        _ => {}
    }
    let (sent, began) = (link.now_ms(), tokio::time::Instant::now());
    let hello = link.hello(&row.station).await?;
    let rtt = began.elapsed().as_millis() as u64;
    if hello.station.as_deref().is_some_and(|s| s != row.station) {
        return Err(format!(
            "unreachable: another station answers at {}'s address",
            row.station
        ));
    }
    Ok(match (hello.cutoff, hello.now_ms) {
        // Its answer is taken to have been written halfway through.
        (true, Some(theirs)) => Some(theirs as i64 - (sent + rtt / 2) as i64),
        _ => None,
    })
}

async fn attempt(link: &dyn StopLink, row: &StopRow, cutoff: bool) -> Result<Value, String> {
    let mut params = json!({"id": row.id, "issued_ms": row.issued_ms, "by": row.by});
    if cutoff {
        if let Some(diff) = difference(link, row).await? {
            params["cutoff_ms"] = json!((row.issued_ms as i64).saturating_add(diff).max(0));
        }
    }
    // How long ago, for a station that reads no cut-off; and when by this
    // station's clock, for one from before it read that.
    params["age_ms"] = json!(link.now_ms().saturating_sub(row.issued_ms));
    link.apply(&row.station, params).await
}

struct Trying<'a>(&'a StopBook, &'a str);

impl Drop for Trying<'_> {
    fn drop(&mut self) {
        let mut rows = self.0.rows.lock().unwrap();
        if let Some(r) = rows.iter_mut().find(|r| r.id == self.1) {
            r.trying = false;
        }
    }
}

/// Send one stop to its station, within its own time limit, and mark the
/// row with what came of it. Unreachable is its own state: it is never
/// shown as stopped. Nothing happens for a row that is confirmed or being
/// sent already.
pub async fn send(book: &StopBook, link: &dyn StopLink, id: &str, cutoff: bool) {
    let Some(row) = book.begin(id) else {
        return;
    };
    let _trying = Trying(book, id);
    let outcome = tokio::time::timeout(SEND_LIMIT, attempt(link, &row, cutoff))
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "unreachable: no answer in {} seconds",
                SEND_LIMIT.as_secs()
            ))
        });
    match outcome {
        Ok(result) => book.mark(id, "confirmed", result, link.now_ms()),
        Err(e) => book.mark(id, "unreachable", json!({"error": e}), link.now_ms()),
    };
}

/// What a round of deliveries did.
#[derive(Debug, Default)]
pub struct Round {
    /// Rows a send was made for.
    pub tried: Vec<String>,
    /// Rows taken out because their station is not in this fleet: (id, station).
    pub dropped: Vec<(String, String)>,
}

/// A stop for a station that was unreachable is delivered when it is back:
/// every waiting row whose station answers is sent, all at once.
pub async fn deliver_waiting(book: &StopBook, link: &dyn StopLink, cutoff: bool) -> Round {
    let mut round = Round::default();
    for row in book.waiting() {
        match link.peer(&row.station).map(|(state, _)| state).as_deref() {
            // A member that answers, on any version: it is tried.
            Some("online" | "update_needed") => round.tried.push(row.id),
            // It is no longer (or never was) in this fleet.
            Some("not_linked") => {
                book.drop_row(&row.id);
                round.dropped.push((row.id, row.station));
            }
            _ => {}
        }
    }
    crate::fleet::futures_join_all(round.tried.iter().map(|id| send(book, link, id, cutoff))).await;
    round
}

/// What the daemon holds for stops.
pub struct StopsState {
    pub book: Arc<StopBook>,
    pub applier: StopApplier,
}

impl StopsState {
    pub fn new(db: Arc<Db>, station: &str) -> StopsState {
        StopsState {
            book: Arc::new(StopBook::new(db.clone(), station)),
            applier: StopApplier::new(db),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(turn_started_ms: u64, busy_since_ms: u64) -> Work {
        Work {
            busy: true,
            created_ms: 10,
            state_since_ms: busy_since_ms,
            turn_started_ms,
            busy_since_ms,
        }
    }

    /// Guarantee: a stop "applies only to the work that was running when it
    /// was issued". The rule, case by case, for a stop issued at 1000.
    #[test]
    fn a_cut_off_is_compared_with_when_the_work_began() {
        let at = Aim::Cutoff(1000);
        assert!(hit(&work(900, 900), &at), "a turn running since before");
        assert!(
            hit(&work(1000, 1000), &at),
            "started in the same millisecond"
        );
        assert!(!hit(&work(1001, 900), &at), "a prompt submitted after it");
        // No prompt hook: the pane went busy before, or after.
        assert!(hit(&work(0, 900), &at));
        assert!(!hit(&work(0, 1001), &at));
        // An older prompt, and busy again only after the stop.
        assert!(!hit(&work(500, 1200), &at));
        // The state changed since (working, then asking the owner): the
        // work is the same work. Counting back by age missed this.
        let asked = Work {
            state_since_ms: 5000,
            ..work(900, 900)
        };
        assert!(hit(&asked, &at));
        assert!(!hit(&asked, &Aim::Age(1000)));
        // The start is not known: stopped.
        assert!(hit(&work(0, 0), &at));
        // A pane made after the stop, and one that is not busy: never.
        assert!(!hit(
            &Work {
                created_ms: 1001,
                ..work(0, 0)
            },
            &at
        ));
        assert!(!hit(
            &Work {
                busy: false,
                ..work(900, 900)
            },
            &at
        ));

        // A standing reading of the other clock: fresh, and agreed with.
        let s = |at_sec: u64, offset_ms| ClockSample {
            at_sec,
            at_ms: 50_000 + at_sec * 1000,
            offset_ms,
            rtt_ms: 40,
        };
        let now = |sec: u64| (sec, 50_000 + sec * 1000);
        let two = [s(0, 90_010), s(10, 90_000)];
        assert_eq!(standing(&two, now(12).0, now(12).1), Some(90_000));
        assert_eq!(standing(&two, now(25).0, now(25).1), None, "15 s old");
        assert_eq!(standing(&two[1..], now(12).0, now(12).1), None, "one");
        let stepped = [s(0, 90_010), s(10, 120_000)];
        assert_eq!(standing(&stepped, now(12).0, now(12).1), None);
        // This station's own clock was set 30 s on, or it slept 30 s.
        assert_eq!(standing(&two, 12, now(12).1 + 30_000), None);

        // The cut-off is never later than counting back by age gives.
        assert_eq!(aim(9000, 1, Some(500), Some(1000)), Aim::Cutoff(1000));
        assert_eq!(aim(9000, 1, Some(8500), Some(1000)), Aim::Cutoff(500));
        assert_eq!(aim(9000, 1, Some(500), None), Aim::Age(8500));
        assert_eq!(aim(9000, 777, None, None), Aim::Age(777));
    }
}
