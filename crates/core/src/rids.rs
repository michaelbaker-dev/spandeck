//! Doing a change once (DECISIONS.md, "A retried change is done once").
//!
//! A caller that may ask twice (the CLI and an agent's tools, when the
//! daemon goes away before it answers) makes a random request id once and
//! sends it with every try. The station that carries the change out keeps
//! the answer for two minutes, under the caller, the method and a hash of
//! the parameters, in the `kv` table: a second try is given the first
//! one's answer and nothing is done again.
//!
//! What this file holds is plain code over the database: who may be
//! answered from the store, what is kept, for how long, and what is said
//! when the station cannot tell. `rpc.rs` decides which methods come here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::db::{Db, Op};

/// Said in `caps`: this station keeps request ids. A change is tried again
/// by its id only towards a station that says so.
pub const CAP: &str = "rid";
/// The switch (`[flags] request_ids = false`, or `station.flags`).
pub const FLAG: &str = "request_ids";

/// How long an answer is kept after the change is done.
pub const KEEP: Duration = Duration::from_secs(120);
/// A second try that says its first was longer ago than this is not
/// answered from "no record": the record may have gone. Every caller's
/// retry window (5 s, 10 s, 15 s) is well inside it.
pub const AGAIN_MAX: Duration = Duration::from_secs(30);
/// When a caller's rows are full, an answer older than this may go early.
/// Longer than `AGAIN_MAX`, so no try that is still taken finds it gone.
const EVICT_AFTER: Duration = Duration::from_secs(60);
/// What a second try's "how long ago" may be short by: the time between
/// its caller measuring it and this station reading it. On the local
/// socket that is nothing; three seconds allow for a daemon that has just
/// started and is slow to read. A try that came through another station
/// took a network call to arrive: `FAR_MS` more is added to its age.
const SLACK: Duration = Duration::from_secs(3);
pub const FAR_MS: u64 = 7_000;
/// How many requests are remembered: for one caller, and in all. Past
/// that a change still runs, with no row (see `Begin::Unkept`).
const PER_CALLER: usize = 1024;
const TOTAL: usize = 8192;
/// An answer larger than this is not kept, nor one that would take the
/// answers held in memory past `BYTES_MAX`; the change still runs once,
/// and a second try is told it was carried out.
const REPLY_MAX: usize = 64 * 1024;
const BYTES_MAX: usize = 32 * 1024 * 1024;

/// What a request carries beside its token: the id made once by whoever
/// first asked, and, on a second try, how long ago the first was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    pub rid: String,
    pub again_ms: Option<u64>,
}

impl Retry {
    /// From a request's `rid` and `again_ms`. An id is 16 to 64 letters,
    /// digits, `-` or `_`; anything else is refused, never guessed at.
    pub fn parse(rid: Option<&Value>, again: Option<&Value>) -> Result<Option<Retry>, String> {
        let Some(rid) = rid.filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let ok = |s: &&str| {
            (16..=64).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        let Some(rid) = rid.as_str().filter(ok) else {
            return Err("rid: 16 to 64 letters, digits, - or _".into());
        };
        Ok(Some(Retry {
            rid: rid.to_string(),
            again_ms: again.and_then(|v| v.as_u64()),
        }))
    }
}

/// A new request id: 128 random bits from the OS, as hex.
pub fn new_rid() -> String {
    let mut buf = [0u8; 16];
    spandeck_terminal::sys::random_bytes(&mut buf).expect("the OS random source is available");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// A hash of a request's parameters that does not depend on the order of
/// an object's fields.
pub fn hash(params: &Value) -> String {
    fn feed(v: &Value, h: &mut Sha256) {
        match v {
            Value::Object(o) => {
                let mut keys: Vec<&String> = o.keys().collect();
                keys.sort();
                h.update(b"{");
                for k in keys {
                    h.update(json!(k).to_string().as_bytes());
                    h.update(b":");
                    feed(&o[k], h);
                    h.update(b",");
                }
                h.update(b"}");
            }
            Value::Array(a) => {
                h.update(b"[");
                for x in a {
                    feed(x, h);
                    h.update(b",");
                }
                h.update(b"]");
            }
            other => h.update(other.to_string().as_bytes()),
        }
    }
    let mut h = Sha256::new();
    feed(params, &mut h);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// An answer as it is kept: the result, or the error's code and words.
pub type Outcome = Result<Value, (String, String)>;

#[derive(Debug, Clone, PartialEq)]
pub enum Kept {
    Answer(Outcome),
    /// The change was carried out; its answer was too large to keep, or
    /// had a field that looked like a secret.
    NotKept,
}

/// A field with one of these names anywhere in an answer: the answer is
/// not kept. The methods that are known to answer with a token are never
/// brought here at all (rpc.rs); this is for one added later by mistake.
fn looks_secret(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.iter().any(|(k, v)| {
            matches!(
                k.to_ascii_lowercase().as_str(),
                "token" | "secret" | "password" | "fleet_key" | "authorization"
            ) || looks_secret(v)
        }),
        Value::Array(a) => a.iter().any(looks_secret),
        _ => false,
    }
}

/// What is kept of an answer, and its size.
fn kept_of(outcome: &Outcome) -> (Kept, usize) {
    let bytes = match outcome {
        Ok(v) if looks_secret(v) => return (Kept::NotKept, 0),
        Ok(v) => v.to_string().len(),
        Err((code, message)) => code.len() + message.len(),
    };
    if bytes > REPLY_MAX {
        return (Kept::NotKept, 0);
    }
    (Kept::Answer(outcome.clone()), bytes)
}

/// The row as the `kv` table has it.
fn row(method: &str, hash: &str, kept: Option<&Kept>, now_ms: u64) -> String {
    let (state, answer) = match kept {
        None => ("running", Value::Null),
        Some(Kept::NotKept) => ("done", Value::Null),
        Some(Kept::Answer(Ok(v))) => ("done", json!({"ok": v})),
        Some(Kept::Answer(Err((code, message)))) => {
            ("done", json!({"err": {"code": code, "message": message}}))
        }
    };
    json!({"m": method, "h": hash, "s": state, "r": answer, "t": now_ms}).to_string()
}

/// The row a message's own transaction writes beside the message: done,
/// with the answer its sender is given.
pub fn done_row(method: &str, hash: &str, answer: &Value, now_ms: u64) -> String {
    row(
        method,
        hash,
        Some(&Kept::Answer(Ok(answer.clone()))),
        now_ms,
    )
}

fn kept_from_row(v: &Value) -> Kept {
    if let Some(ok) = v["r"].get("ok") {
        return Kept::Answer(Ok(ok.clone()));
    }
    match v["r"].get("err") {
        Some(e) => Kept::Answer(Err((
            e["code"].as_str().unwrap_or("internal").to_string(),
            e["message"].as_str().unwrap_or("").to_string(),
        ))),
        None => Kept::NotKept,
    }
}

/// How the first try's mark is made durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// "Being carried out" is written and waited for (a second at most)
    /// before the change runs.
    Before,
    /// The change's own transaction writes the finished row with what it
    /// stores (a message): the two are stored together or not at all.
    InTx,
}

enum State {
    Running(watch::Receiver<Option<Kept>>),
    Done { at: Instant, kept: Kept },
}

struct Row {
    who: String,
    method: String,
    hash: String,
    state: State,
    /// The size of the answer held, once there is one.
    bytes: usize,
}

struct Inner {
    rows: HashMap<String, Row>,
    /// The answers held in memory, together.
    bytes: usize,
    /// Since when every image of this station's daemon has kept ids, with
    /// nothing lost between them: this process's start, or further back
    /// when it took over in place from an image that did.
    since: Instant,
    on: Option<bool>,
    /// When a caller last had a change run with no row because its rows
    /// were full (`""`: because the station's were). Until that is as old
    /// as a second try's first, "no record" does not mean "never carried
    /// out" for that caller.
    over: HashMap<String, Instant>,
}

impl Inner {
    fn remove(&mut self, key: &str) {
        if let Some(row) = self.rows.remove(key) {
            self.bytes = self.bytes.saturating_sub(row.bytes);
        }
    }
}

/// When a fleet stop was asked, and the stops it issued once it has.
type StopsIssued = (Instant, Option<Vec<String>>);

pub struct Rids {
    db: Arc<Db>,
    inner: Mutex<Inner>,
    /// "Stop everything" asked with an id: the stops it issued. A record
    /// of its own, written without waiting: a stop never waits for the
    /// database, and is never held back by this.
    stops: Mutex<HashMap<String, StopsIssued>>,
    /// This image is being replaced and its writer has been stopped: a
    /// mark made now would reach neither the database nor the next image.
    frozen: std::sync::atomic::AtomicBool,
}

/// What a fleet stop with an id is told.
pub enum StopBegin {
    /// Not seen before: issue it, then `stop_issued` (or `stop_refused`).
    First(String),
    /// Asked before: the stops issued then (none yet, in the moment
    /// before the first try has issued them).
    Repeat(Option<Vec<String>>),
}

/// What a request with an id is told before anything runs.
pub enum Begin {
    /// Not seen before: carry it out, then `Run::finish`.
    Run(Run),
    /// Carried out before: the answer then given.
    Done(Kept),
    /// Being carried out now, in this process: wait for its answer.
    Wait(watch::Receiver<Option<Kept>>),
    /// The station cannot tell whether it was carried out.
    Unknown(&'static str),
    /// The id was used for another method or other parameters.
    Reused,
    /// Too many of this caller's requests are being remembered to
    /// remember one more: carry it out with no row. It is never refused
    /// for that. A second try of anything this caller asked since then is
    /// told "cannot tell", because no record no longer means never done.
    Unkept,
}

/// The one try that carries a request out.
pub struct Run {
    rids: Arc<Rids>,
    pub key: String,
    method: String,
    hash: String,
    mark: Mark,
    tx: Option<watch::Sender<Option<Kept>>>,
}

pub fn key(who: &str, rid: &str) -> String {
    format!("rid:{who}:{rid}")
}

impl Rids {
    pub fn new(db: Arc<Db>) -> Arc<Rids> {
        Arc::new(Rids {
            db,
            inner: Mutex::new(Inner {
                rows: HashMap::new(),
                bytes: 0,
                since: Instant::now(),
                on: None,
                over: HashMap::new(),
            }),
            stops: Mutex::new(HashMap::new()),
            frozen: Default::default(),
        })
    }

    /// Set just before the writer is stopped for the image that replaces
    /// this one, and taken back if that image does not come. While it is
    /// set, a change that needs its mark written first is not begun: it
    /// waits, its caller's connection ends with this image, and its second
    /// try is carried out, once, by the next one.
    pub fn freeze(&self, on: bool) {
        self.frozen.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn frozen(&self) -> bool {
        self.frozen.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// "Stop everything" with an id. A second try is given the first
    /// one's stops; when there is no record of a first (the daemon died
    /// before it was written), the stop is issued again: a stop errs on
    /// the side of stopping.
    pub fn stop_begin(&self, who: &str, rid: &str) -> StopBegin {
        let key = format!("stoprid:{who}:{rid}");
        let mut stops = self.stops.lock().unwrap();
        if let Some((_, ids)) = stops.get(&key) {
            return StopBegin::Repeat(ids.clone());
        }
        let ids = self
            .db
            .kv_get(&key)
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| serde_json::from_value::<Vec<String>>(v["ids"].clone()).ok());
        if let Some(ids) = ids {
            return StopBegin::Repeat(Some(ids));
        }
        stops.insert(key.clone(), (Instant::now(), None));
        StopBegin::First(key)
    }

    pub fn stop_issued(&self, key: &str, ids: &[String], now_ms: u64) {
        if let Some(entry) = self.stops.lock().unwrap().get_mut(key) {
            entry.1 = Some(ids.to_vec());
        }
        self.db
            .kv_put(key, &json!({"ids": ids, "t": now_ms}).to_string());
    }

    /// Refused before anything was stopped: nothing to remember.
    pub fn stop_refused(&self, key: &str) {
        let mut stops = self.stops.lock().unwrap();
        if matches!(stops.get(key), Some((_, None))) {
            stops.remove(key);
        }
    }

    /// The image before this one kept ids for this long and handed over
    /// everything it had not written: its records are this one's.
    pub fn kept_before(&self, kept: Duration) {
        let mut i = self.inner.lock().unwrap();
        i.since = Instant::now().checked_sub(kept).unwrap_or(i.since);
    }

    /// The switch as it stands. Turned on after being off: what was done
    /// meanwhile has no record, so the count starts again.
    pub fn set_on(&self, on: bool) {
        let mut i = self.inner.lock().unwrap();
        if on && i.on == Some(false) {
            i.since = Instant::now();
        }
        i.on = Some(on);
    }

    pub fn on(&self) -> bool {
        self.inner.lock().unwrap().on == Some(true)
    }

    /// For the image that takes over in place: how long ids have been
    /// kept. None when they are not.
    pub fn kept_for(&self) -> Option<Duration> {
        let i = self.inner.lock().unwrap();
        (i.on == Some(true)).then(|| i.since.elapsed())
    }

    /// Does "no record of this id" mean "never carried out" for a try
    /// whose first was written `age` ago? Only when ids have been kept,
    /// without a gap, since before then.
    fn vouches(i: &Inner, who: &str, age: Duration) -> bool {
        let before = age + SLACK;
        age <= AGAIN_MAX
            && i.since.elapsed() >= before
            && [who, ""]
                .iter()
                .all(|w| i.over.get(*w).is_none_or(|at| at.elapsed() >= before))
    }

    pub fn vouches_for(&self, who: &str, again_ms: u64) -> bool {
        Self::vouches(
            &self.inner.lock().unwrap(),
            who,
            Duration::from_millis(again_ms),
        )
    }

    /// A request with an id, from `who` (`owner`, or `pane:<id>`: worked
    /// out from the caller's token, never from what the request says).
    pub fn begin(
        self: &Arc<Self>,
        who: &str,
        method: &str,
        hash: &str,
        retry: &Retry,
        mark: Mark,
        now_ms: u64,
    ) -> Begin {
        let key = key(who, &retry.rid);
        let (tx, rx) = watch::channel(None);
        {
            let mut i = self.inner.lock().unwrap();
            if let Some(r) = i.rows.get(&key) {
                if r.method != method || r.hash != hash {
                    return Begin::Reused;
                }
                return match &r.state {
                    State::Running(rx) => Begin::Wait(rx.clone()),
                    State::Done { kept, .. } => Begin::Done(kept.clone()),
                };
            }
            // Not this process's: one an image before this one wrote?
            if let Some(v) = self
                .db
                .kv_get(&key)
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            {
                if v["m"] != json!(method) || v["h"] != json!(hash) {
                    return Begin::Reused;
                }
                return match v["s"].as_str() {
                    Some("done") => Begin::Done(kept_from_row(&v)),
                    _ => Begin::Unknown(
                        "it was being carried out when this station's daemon ended or was replaced",
                    ),
                };
            }
            if let Some(age) = retry.again_ms {
                if !Self::vouches(&i, who, Duration::from_millis(age)) {
                    return Begin::Unknown(
                        "this station has no record of it, and cannot say it was never carried out: it has not kept records for long enough, or had too many to keep",
                    );
                }
            }
            if !Self::room(&mut i, who) {
                return Begin::Unkept;
            }
            i.rows.insert(
                key.clone(),
                Row {
                    who: who.to_string(),
                    method: method.to_string(),
                    hash: hash.to_string(),
                    state: State::Running(rx),
                    bytes: 0,
                },
            );
        }
        if mark == Mark::Before {
            // Waited for, a second at most. Not written in that second
            // (the database is locked): the change still runs, the row is
            // written when it can be, and if the daemon dies first a
            // second try finds no record at a station that has just
            // started, which is "cannot tell", never "do it again".
            self.db.write_ack(Op::Kv {
                k: key.clone(),
                v: row(method, hash, None, now_ms),
            });
        }
        Begin::Run(Run {
            rids: self.clone(),
            key,
            method: method.to_string(),
            hash: hash.to_string(),
            mark,
            tx: Some(tx),
        })
    }

    /// Room for one more row of this caller's. When there is none, the
    /// oldest finished one goes, if it is old enough that no try still
    /// taken can ask for it.
    fn room(i: &mut Inner, who: &str) -> bool {
        let mine = i.rows.values().filter(|r| r.who == who).count();
        if mine < PER_CALLER && i.rows.len() < TOTAL {
            return true;
        }
        let only_mine = mine >= PER_CALLER;
        let oldest = i
            .rows
            .iter()
            .filter(|(_, r)| !only_mine || r.who == who)
            .filter_map(|(k, r)| match &r.state {
                State::Done { at, .. } if at.elapsed() >= EVICT_AFTER => Some((k.clone(), *at)),
                _ => None,
            })
            .min_by_key(|(_, at)| *at);
        match oldest {
            Some((k, _)) => {
                i.remove(&k);
                true
            }
            None => {
                let now = Instant::now();
                i.over.insert(who.to_string(), now);
                if !only_mine {
                    i.over.insert(String::new(), now);
                }
                false
            }
        }
    }

    /// Every two minutes' worth of rows goes: from memory, and from the
    /// table (rows an image before this one wrote included). Never waits
    /// for the database.
    pub fn prune(&self, now_ms: u64) {
        let keep = KEEP.as_millis() as u64;
        let live: Vec<String> = {
            let mut i = self.inner.lock().unwrap();
            i.rows.retain(|_, r| match &r.state {
                State::Running(_) => true,
                State::Done { at, .. } => at.elapsed() < KEEP,
            });
            i.bytes = i.rows.values().map(|r| r.bytes).sum();
            i.over.retain(|_, at| at.elapsed() < KEEP);
            i.rows
                .iter()
                .filter(|(_, r)| matches!(r.state, State::Running(_)))
                .map(|(k, _)| k.clone())
                .collect()
        };
        self.stops
            .lock()
            .unwrap()
            .retain(|_, (at, _)| at.elapsed() < KEEP);
        let old: Vec<String> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT k, v FROM kv WHERE (k >= 'rid:' AND k < 'rid;') OR (k >= 'stoprid:' AND k < 'stoprid;')",
                )?;
                let rows =
                    stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
                rows.collect::<rusqlite::Result<Vec<(String, String)>>>()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|(k, v)| {
                let t = serde_json::from_str::<Value>(v)
                    .ok()
                    .and_then(|v| v["t"].as_u64())
                    .unwrap_or(0);
                // Too old, or stamped by a clock that has since gone back.
                (t + keep < now_ms || t > now_ms + keep) && !live.contains(k)
            })
            .map(|(k, _)| k)
            .collect();
        for k in old {
            self.db.put(Op::KvDel { k });
        }
    }

    /// Rows held in memory, for tests and the Performance page.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Run {
    /// The change was carried out (or refused): keep what its caller is
    /// told, tell whoever waits, and give it back.
    pub fn finish(mut self, outcome: Outcome, now_ms: u64) {
        let (mut kept, bytes) = kept_of(&outcome);
        let mut i = self.rids.inner.lock().unwrap();
        // A message that was refused stored nothing, and its row was to be
        // written with it: there is nothing to remember, and a second try
        // is a first one.
        if self.mark == Mark::InTx && outcome.is_err() {
            i.remove(&self.key);
        } else {
            let fits = i.bytes + bytes <= BYTES_MAX;
            if !fits {
                kept = Kept::NotKept;
            }
            if let Some(r) = i.rows.get_mut(&self.key) {
                r.state = State::Done {
                    at: Instant::now(),
                    kept: kept.clone(),
                };
                if fits {
                    r.bytes = bytes;
                    i.bytes += bytes;
                }
            }
        }
        drop(i);
        if self.mark == Mark::Before {
            // Kept until it is written; never waited for. A daemon that
            // dies first leaves "being carried out", which a second try
            // is told as "cannot tell".
            self.rids.db.put(Op::Kv {
                k: self.key.clone(),
                v: row(&self.method, &self.hash, Some(&kept), now_ms),
            });
        }
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Some(kept));
        }
    }
}

impl Drop for Run {
    /// Dropped before it finished (its task was cancelled): this process
    /// forgets it, and the row in the table, if one was written, goes on
    /// saying "being carried out", so a second try is told "cannot tell".
    fn drop(&mut self) {
        if self.tx.take().is_some() {
            self.rids.inner.lock().unwrap().remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(rid: &str) -> Retry {
        Retry {
            rid: rid.into(),
            again_ms: None,
        }
    }

    fn again(rid: &str, ms: u64) -> Retry {
        Retry {
            rid: rid.into(),
            again_ms: Some(ms),
        }
    }

    const RID: &str = "0123456789abcdef0123456789abcdef";

    fn store() -> (Arc<Db>, Arc<Rids>) {
        let db = Arc::new(Db::memory().unwrap());
        let rids = Rids::new(db.clone());
        rids.set_on(true);
        (db, rids)
    }

    /// Guarantee (Trust, and "a retried change is done once"): a stored
    /// answer is only ever the answer to the same request from the same
    /// caller. Review B3: the store as first specified was a bearer handle.
    #[test]
    fn an_answer_is_given_only_to_the_same_caller_method_and_parameters() {
        let (db, rids) = store();
        let params = json!({"agent": "shell", "name": "a"});
        let h = hash(&params);
        let Begin::Run(run) = rids.begin("owner", "pane.spawn", &h, &first(RID), Mark::Before, 1)
        else {
            panic!("a first try runs");
        };
        // While it runs, a second try waits for it instead of running.
        let Begin::Wait(mut waiting) =
            rids.begin("owner", "pane.spawn", &h, &again(RID, 50), Mark::Before, 2)
        else {
            panic!("a second try waits for the first");
        };
        run.finish(Ok(json!({"id": "p-1"})), 3);
        assert_eq!(
            waiting.borrow_and_update().clone(),
            Some(Kept::Answer(Ok(json!({"id": "p-1"}))))
        );
        // Afterwards: the same answer, and nothing to run.
        let Begin::Done(Kept::Answer(Ok(v))) =
            rids.begin("owner", "pane.spawn", &h, &again(RID, 900), Mark::Before, 4)
        else {
            panic!("a second try is given the first answer");
        };
        assert_eq!(v, json!({"id": "p-1"}));
        // The same fields in another order are the same request.
        assert_eq!(h, hash(&json!({"name": "a", "agent": "shell"})));
        // Other parameters, or another method, under the same id: refused,
        // and never given the stored answer.
        let other = hash(&json!({"agent": "shell", "name": "b"}));
        assert!(matches!(
            rids.begin("owner", "pane.spawn", &other, &first(RID), Mark::Before, 5),
            Begin::Reused
        ));
        assert!(matches!(
            rids.begin("owner", "pane.close", &h, &first(RID), Mark::Before, 5),
            Begin::Reused
        ));
        // Another caller with the same id is another request altogether:
        // it runs, and reads nothing of the owner's. An agent's `who` is
        // its pane, so it can neither read nor plant the owner's row.
        let Begin::Run(agent) =
            rids.begin("pane:p-9", "pane.spawn", &h, &first(RID), Mark::Before, 6)
        else {
            panic!("another caller's request runs separately");
        };
        agent.finish(Err(("denied".into(), "not allowed".into())), 7);
        let Begin::Done(Kept::Answer(Ok(still))) =
            rids.begin("owner", "pane.spawn", &h, &again(RID, 900), Mark::Before, 8)
        else {
            panic!("the owner's answer is still the owner's");
        };
        assert_eq!(still, json!({"id": "p-1"}));
        // An error is an answer too, and is the one given again.
        assert!(matches!(
            rids.begin("pane:p-9", "pane.spawn", &h, &again(RID, 10), Mark::Before, 9),
            Begin::Done(Kept::Answer(Err((code, _)))) if code == "denied"
        ));
        // The rows are in `kv`, under the caller.
        assert!(db.flush(Duration::from_secs(5)));
        assert!(db.kv_get(&key("owner", RID)).is_some());
        assert!(db.kv_get(&key("pane:p-9", RID)).is_some());
    }

    /// The crash cases (REVIEW2, "Reply store": outcome unknown needs a
    /// durable mark before the work runs). A daemon that ended between
    /// doing and storing leaves "being carried out"; one that ended before
    /// its mark was written leaves nothing. Neither is ever run again
    /// blind, and an image that took over in place answers for the one
    /// before it.
    #[test]
    fn what_a_second_try_is_told_after_the_daemon_ended() {
        let db = Arc::new(Db::memory().unwrap());
        let h = hash(&json!({"name": "x"}));
        // The image that ended: one request finished, one still running.
        {
            let old = Rids::new(db.clone());
            old.set_on(true);
            let Begin::Run(done) =
                old.begin("owner", "project.open", &h, &first(RID), Mark::Before, 1)
            else {
                panic!()
            };
            done.finish(Ok(json!({"name": "x"})), 2);
            let Begin::Run(running) = old.begin(
                "owner",
                "pane.spawn",
                &h,
                &first("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                Mark::Before,
                3,
            ) else {
                panic!()
            };
            // The process ends here: nothing more is written.
            std::mem::forget(running);
            assert!(db.flush(Duration::from_secs(5)));
        }
        // A daemon started again (not in place): it has kept ids for no
        // time at all.
        let new = Rids::new(db.clone());
        new.set_on(true);
        // Finished before the crash: the same answer.
        assert!(matches!(
            new.begin("owner", "project.open", &h, &again(RID, 2000), Mark::Before, 9),
            Begin::Done(Kept::Answer(Ok(v))) if v == json!({"name": "x"})
        ));
        // Crashed between doing and storing: cannot tell. Said every time.
        for _ in 0..2 {
            assert!(matches!(
                new.begin(
                    "owner",
                    "pane.spawn",
                    &h,
                    &again("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 2000),
                    Mark::Before,
                    9
                ),
                Begin::Unknown(_)
            ));
        }
        // No record at all, of a try written before this daemon started
        // (the mark was not written in time, or the daemon that read it
        // kept no ids): cannot tell, and it is not carried out.
        let lost = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        assert!(matches!(
            new.begin(
                "owner",
                "pane.spawn",
                &h,
                &again(lost, 2000),
                Mark::Before,
                9
            ),
            Begin::Unknown(_)
        ));
        assert!(db.kv_get(&key("owner", lost)).is_none());
        // The same try at an image that took over in place from one that
        // kept ids for a minute: no record means it never arrived, so it
        // runs. One written longer ago than any answer is promised for is
        // still "cannot tell".
        let heir = Rids::new(db.clone());
        heir.kept_before(Duration::from_secs(60));
        heir.set_on(true);
        assert!(matches!(
            heir.begin(
                "owner",
                "pane.spawn",
                &h,
                &again(lost, 2000),
                Mark::Before,
                9
            ),
            Begin::Run(_)
        ));
        assert!(matches!(
            heir.begin(
                "owner",
                "pane.spawn",
                &h,
                &again("cccccccccccccccccccccccccccccccc", 45_000),
                Mark::Before,
                9
            ),
            Begin::Unknown(_)
        ));
        // The switch turned off and on again: what was done meanwhile has
        // no record, so nothing from before is vouched for.
        heir.set_on(false);
        heir.set_on(true);
        assert!(!heir.vouches_for("owner", 2000));
        // A first try (no "again") always runs: nothing was sent before.
        assert!(matches!(
            new.begin(
                "owner",
                "pane.spawn",
                &h,
                &first("dddddddddddddddddddddddddddddddd"),
                Mark::Before,
                9
            ),
            Begin::Run(_)
        ));
    }

    /// Review B3 ("holds no secrets" was false) and the bound: an answer
    /// with a token in it is never written, a caller's rows are bounded,
    /// and rows go after two minutes.
    #[test]
    fn what_is_kept_is_bounded_and_never_a_token() {
        let (db, rids) = store();
        let h = hash(&json!({}));
        let Begin::Run(run) = rids.begin("owner", "x.y", &h, &first(RID), Mark::Before, 1) else {
            panic!()
        };
        run.finish(Ok(json!({"ok": true, "browser": {"token": "s3cret"}})), 2);
        assert!(db.flush(Duration::from_secs(5)));
        let stored = db.kv_get(&key("owner", RID)).unwrap();
        assert!(!stored.contains("s3cret"), "{stored}");
        // The change is still known to be done: not run again, and the
        // caller is told its answer was not kept.
        assert!(matches!(
            rids.begin("owner", "x.y", &h, &again(RID, 100), Mark::Before, 3),
            Begin::Done(Kept::NotKept)
        ));
        // A caller's rows are bounded. Past the bound its change still
        // runs, with no row; and from then on "no record" is not taken to
        // mean "never carried out" for that caller. Another caller is not
        // touched by it: an agent cannot fill the store for the owner.
        rids.kept_before(Duration::from_secs(60));
        for n in 0..PER_CALLER {
            let rid = format!("{n:032}");
            match rids.begin("pane:p-1", "note.add", &h, &first(&rid), Mark::InTx, 4) {
                Begin::Run(run) => run.finish(Ok(json!({})), 4),
                _ => panic!("row {n} of an agent's fits"),
            }
        }
        assert!(rids.vouches_for("pane:p-1", 500));
        let past = "f".repeat(32);
        assert!(matches!(
            rids.begin("pane:p-1", "note.add", &h, &first(&past), Mark::InTx, 5),
            Begin::Unkept
        ));
        assert!(matches!(
            rids.begin(
                "pane:p-1",
                "note.add",
                &h,
                &again(&past, 500),
                Mark::InTx,
                5
            ),
            Begin::Unknown(_)
        ));
        assert!(rids.vouches_for("owner", 500));
        assert!(matches!(
            rids.begin("owner", "x.y", &h, &first(&"e".repeat(32)), Mark::InTx, 5),
            Begin::Run(_)
        ));
        // Two minutes on, the table's rows go; a row stamped by a clock
        // that has since gone back goes too.
        db.kv_set(
            "rid:owner:from-the-future",
            &row("x.y", &h, None, 9_000_000),
        );
        rids.prune(2 + KEEP.as_millis() as u64 + 1);
        assert!(db.flush(Duration::from_secs(5)));
        assert!(db.kv_get(&key("owner", RID)).is_none());
        assert!(db.kv_get("rid:owner:from-the-future").is_none());
        // An id is checked, not guessed at.
        assert!(Retry::parse(Some(&json!("short")), None).is_err());
        assert!(Retry::parse(Some(&json!("has:colon:0123456789abcdef")), None).is_err());
        assert_eq!(Retry::parse(None, Some(&json!(5))), Ok(None));
        assert_eq!(new_rid().len(), 32);
    }
}
