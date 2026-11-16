//! Approvals: what an agent asked the owner, who waits for the answer, and
//! what was answered (DECISIONS.md, "While spandeck is away").
//!
//! An approval is a row; the hook that asked waits on it in memory. That
//! used to be all: when the daemon went (an upgrade, a restart in place, a
//! kill with the pane host keeping the agent alive) the row stayed
//! "pending" with nobody waiting, and the owner's answer went nowhere.
//!
//! Now the hook that asks makes a random id once and sends it with every
//! try. A hash of it is kept in the row (`detail.hook`). A hook that lost
//! its connection asks again with the same id and is given its own row
//! back: it waits on it again, or, if the owner answered meanwhile, is
//! given that answer at once. The limit on waiting counts from when the
//! row was first made.
//!
//! The id is the hook's secret. It is never shown: every list leaves
//! `detail.hook` out, and what is stored is a hash. A row is only ever
//! found again for the pane it belongs to.
//!
//! A question the agent asks the owner, and a plan it wants approved, are
//! held the same way (questions.rs; kinds `question` and `plan`): the same
//! rows, the same waiting, the same finding again after a restart. They
//! are what was said in a conversation, so they are in no list that the
//! approvals are in (`list`); the owner's own calls read them (`held`).
//!
//! This file is plain code over the database, so that it can be tested
//! without a daemon.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use crate::daemon::ApprovalAnswer;
use crate::db::{Db, Op};
use crate::paths::new_id;

/// How long after a start a waiting approval is given for its hook to find
/// it again, before it is expired.
pub const REATTACH_WAIT: Duration = Duration::from_secs(15);

/// What the owner is told when an answer was recorded for a hook that is
/// not connected at this moment.
pub const RECORDED: &str = "recorded: the agent will get it when it reconnects";

/// The kinds of row that hold an agent's question or plan for the owner.
/// What they say is the owner's conversation: never in `list`.
pub const HELD_KINDS: [&str; 2] = ["question", "plan"];

fn held_kind(v: &Value) -> bool {
    v["kind"].as_str().is_some_and(|k| HELD_KINDS.contains(&k))
}

pub struct ApprovalBook {
    db: Arc<Db>,
    station: String,
    /// approval id -> the hook waiting on it now. The number tells one
    /// waiter from the one that replaced it.
    waiters: Mutex<HashMap<String, (u64, oneshot::Sender<ApprovalAnswer>)>>,
    next: AtomicU64,
}

/// What asking came to.
pub enum Asked {
    /// Wait on `rx`, for `left` at most. `new`: the row was made now;
    /// otherwise the hook found its own row again.
    Wait {
        id: String,
        waiter: u64,
        rx: oneshot::Receiver<ApprovalAnswer>,
        left: Duration,
        new: bool,
    },
    /// The owner answered while the hook was away.
    Answered { id: String, answer: ApprovalAnswer },
    /// The hook's row is expired or cancelled.
    Gone,
}

/// What answering came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    /// The hook was waiting and has the answer.
    Delivered { pane: Option<String> },
    /// Nobody waits at this moment, and the hook that asked can find its
    /// row again: the answer is kept for it.
    Recorded { pane: String },
    /// Nobody waits and nobody can come back for it. `expired`: it was
    /// still listed as waiting, and has now been expired (its pane).
    NotWaiting { expired: Option<String> },
}

struct Row {
    id: String,
    ts: u64,
    pane: String,
    kind: String,
    status: String,
    reason: Option<String>,
    hook: Option<String>,
}

fn row_of(v: &Value) -> Option<Row> {
    Some(Row {
        id: v["id"].as_str()?.to_string(),
        ts: v["ts"].as_u64().unwrap_or(0),
        pane: v["pane"].as_str()?.to_string(),
        kind: v["kind"].as_str().unwrap_or("").to_string(),
        status: v["status"].as_str()?.to_string(),
        reason: v["reason"].as_str().map(String::from),
        hook: v["detail"]["hook"].as_str().map(String::from),
    })
}

/// What is kept of a hook's id: a hash, so that nothing that reads a row
/// can pass for the hook.
pub fn hook_mark(hook_id: &str) -> String {
    Sha256::digest(hook_id.as_bytes())
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Is this something a hook may send as its id? 16 to 64 letters, digits,
/// `-` or `_`, as for a request id.
pub fn hook_id_ok(id: &str) -> bool {
    (16..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl ApprovalBook {
    pub fn new(db: Arc<Db>, station: &str) -> ApprovalBook {
        ApprovalBook {
            db,
            station: station.to_string(),
            waiters: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    fn wait_on(&self, id: &str) -> (u64, oneshot::Receiver<ApprovalAnswer>) {
        let (tx, rx) = oneshot::channel();
        let waiter = self.next.fetch_add(1, Ordering::Relaxed);
        // A waiter already there is replaced: its hook lost its
        // connection, and this is that hook again.
        self.waiters
            .lock()
            .unwrap()
            .insert(id.to_string(), (waiter, tx));
        (waiter, rx)
    }

    /// A hook asks. With `hook_id`, a row this pane's hook already made
    /// under that id is found again instead of a second one being made.
    /// `limit`: how long an approval waits, counted from its first time.
    #[allow(clippy::too_many_arguments)]
    pub fn ask(
        &self,
        pane: &str,
        project: Option<&str>,
        kind: &str,
        summary: &str,
        mut detail: Value,
        hook_id: Option<&str>,
        now: u64,
        limit: Duration,
    ) -> Asked {
        let mark = hook_id.filter(|id| hook_id_ok(id)).map(hook_mark);
        if let Some(mark) = &mark {
            let mine = |r: &Row| r.pane == pane && r.hook.as_deref() == Some(mark.as_str());
            if let Some(row) = self.raw(false).iter().filter_map(row_of).find(mine) {
                return self.rejoin(row, now, limit);
            }
        }
        let id = new_id("a");
        if let Some(mark) = mark {
            if !detail.is_object() {
                detail = json!({"was": detail});
            }
            detail["hook"] = json!(mark);
        }
        let (waiter, rx) = self.wait_on(&id);
        // The hook does not wait for the database: the request is listed
        // from memory until its row is written (db.rs).
        self.db.put(Op::ApprovalNew {
            id: id.clone(),
            ts: now as i64,
            pane: pane.to_string(),
            project: project.map(String::from),
            kind: kind.to_string(),
            summary: summary.to_string(),
            detail: detail.to_string(),
        });
        Asked::Wait {
            id,
            waiter,
            rx,
            left: limit,
            new: true,
        }
    }

    fn answered(row: &Row) -> Option<ApprovalAnswer> {
        match row.status.as_str() {
            "allowed" => Some(ApprovalAnswer {
                allow: true,
                reason: row.reason.clone(),
            }),
            "denied" => Some(ApprovalAnswer {
                allow: false,
                reason: row.reason.clone(),
            }),
            _ => None,
        }
    }

    fn rejoin(&self, row: Row, now: u64, limit: Duration) -> Asked {
        if let Some(answer) = Self::answered(&row) {
            return Asked::Answered { id: row.id, answer };
        }
        if row.status != "pending" {
            return Asked::Gone;
        }
        let (waiter, rx) = self.wait_on(&row.id);
        // Answered, or expired, between that look and this waiter: an
        // answer given to a row with no waiter is written with the
        // waiters locked (`answer`, `expire_detached`), so it is either
        // seen here or finds this waiter.
        let again = self.row(&row.id);
        let settled = again.as_ref().filter(|r| r.status != "pending");
        if let Some(r) = settled {
            self.finish(&row.id, waiter);
            return match Self::answered(r) {
                Some(answer) => Asked::Answered { id: row.id, answer },
                None => Asked::Gone,
            };
        }
        Asked::Wait {
            left: limit.saturating_sub(Duration::from_millis(now.saturating_sub(row.ts))),
            id: row.id,
            waiter,
            rx,
            new: false,
        }
    }

    /// A waiter stopped waiting without an answer (its time ran out, or
    /// its sender went). True when it was still the one waiting on the
    /// row: the row is then the caller's to expire. False when another
    /// took its place, or the row was cancelled: nothing is to be done.
    pub fn finish(&self, id: &str, waiter: u64) -> bool {
        let mut waiters = self.waiters.lock().unwrap();
        match waiters.get(id) {
            Some((w, _)) if *w == waiter => {
                waiters.remove(id);
                true
            }
            _ => false,
        }
    }

    /// The row's time ran out with its hook still waiting.
    pub fn expire(&self, id: &str, now: u64) {
        self.db.put(Op::ApprovalStatus {
            id: id.to_string(),
            status: "expired".into(),
            at: now as i64,
            by: None,
            reason: None,
            only_pending: true,
        });
    }

    /// The owner's answer. `alive`: is this pane's program still running?
    /// The hook waiting on the row is answered first and at once. With
    /// nobody waiting, the answer is kept for a hook that can come back
    /// for it (the row has a hook's mark and its pane is running);
    /// otherwise the row, if it still read as waiting, is expired.
    ///
    /// The write is not waited for here: `saved` says whether it is there.
    pub fn answer(
        &self,
        id: &str,
        allow: bool,
        reason: Option<String>,
        by: &str,
        now: u64,
        alive: impl Fn(&str) -> bool,
    ) -> Answer {
        let status = if allow { "allowed" } else { "denied" };
        let mut waiters = self.waiters.lock().unwrap();
        let sent = waiters.remove(id).is_some_and(|(_, tx)| {
            tx.send(ApprovalAnswer {
                allow,
                reason: reason.clone(),
            })
            .is_ok()
        });
        let row = self.row(id);
        if sent {
            drop(waiters);
            // Who answered, and what: kept until it is written, and listed
            // from memory meanwhile.
            self.db.put(Op::ApprovalStatus {
                id: id.to_string(),
                status: status.to_string(),
                at: now as i64,
                by: Some(by.to_string()),
                reason,
                only_pending: false,
            });
            return Answer::Delivered {
                pane: row.map(|r| r.pane),
            };
        }
        let Some(row) = row.filter(|r| r.status == "pending") else {
            return Answer::NotWaiting { expired: None };
        };
        if row.hook.is_some() && alive(&row.pane) {
            // Written with the waiters locked: a hook that comes back at
            // this moment either sees the answer or is seen here.
            self.db.put(Op::ApprovalStatus {
                id: id.to_string(),
                status: status.to_string(),
                at: now as i64,
                by: Some(by.to_string()),
                reason,
                only_pending: true,
            });
            return Answer::Recorded { pane: row.pane };
        }
        self.expire(id, now);
        Answer::NotWaiting {
            expired: Some(row.pane),
        }
    }

    /// "Answer in the terminal instead": the owner lets a held question or
    /// plan go. The hook that waits falls through with no answer, so the
    /// agent shows its own prompt; one that comes back later is told the
    /// same. Nothing is answered. Returns its pane, or None when it was no
    /// longer waiting.
    pub fn release(&self, id: &str, by: &str, now: u64) -> Option<String> {
        let mut waiters = self.waiters.lock().unwrap();
        let row = self.row(id).filter(|r| r.status == "pending")?;
        // Dropping the sender is what tells the hook.
        waiters.remove(id);
        // Written with the waiters locked, as an answer is.
        self.db.put(Op::ApprovalStatus {
            id: id.to_string(),
            status: "released".into(),
            at: now as i64,
            by: Some(by.to_string()),
            reason: None,
            only_pending: true,
        });
        Some(row.pane)
    }

    /// What kind of request a row is, and how it stands.
    pub fn kind_and_status(&self, id: &str) -> Option<(String, String)> {
        self.row(id).map(|r| (r.kind, r.status))
    }

    /// Does this pane have a question or a plan held for the owner now?
    pub fn holds(&self, pane: &str) -> bool {
        self.raw(true)
            .iter()
            .any(|r| r["pane"] == pane && held_kind(r))
    }

    /// Is the answer to this approval in the database?
    pub fn saved(&self, id: &str) -> bool {
        self.db
            .saved_within(&[format!("approval-status:{id}")], crate::db::ACK_WAIT)
    }

    /// A pane ended, or was closed: what it was asking is cancelled.
    /// Dropping the sender makes the waiting hook fall through.
    pub fn cancel_pane(&self, pane: &str, now: u64) -> Vec<String> {
        let ids: Vec<String> = self
            .raw(true)
            .iter()
            .filter_map(row_of)
            .filter(|r| r.pane == pane)
            .map(|r| r.id)
            .collect();
        for id in &ids {
            self.waiters.lock().unwrap().remove(id);
            self.db.put(Op::ApprovalStatus {
                id: id.clone(),
                status: "cancelled".into(),
                at: now as i64,
                by: None,
                reason: None,
                only_pending: true,
            });
        }
        ids
    }

    /// Is any hook waiting on anything now? From memory: for a caller
    /// that would otherwise read the rows on every hook of every agent.
    pub fn any_waiting(&self) -> bool {
        !self.waiters.lock().unwrap().is_empty()
    }

    /// `cancel_pane`, for the pane's questions and plans only: the turn
    /// they belonged to is over. A permission request is left alone.
    pub fn cancel_held(&self, pane: &str, now: u64) -> Vec<String> {
        let ids: Vec<String> = self
            .raw(true)
            .iter()
            .filter(|r| held_kind(r))
            .filter_map(row_of)
            .filter(|r| r.pane == pane)
            .map(|r| r.id)
            .collect();
        for id in &ids {
            self.waiters.lock().unwrap().remove(id);
            self.db.put(Op::ApprovalStatus {
                id: id.clone(),
                status: "cancelled".into(),
                at: now as i64,
                by: None,
                reason: None,
                only_pending: true,
            });
        }
        ids
    }

    /// Expire every approval that still reads as waiting and has no hook
    /// waiting on it: after a start, the ones whose hook did not find its
    /// way back. Returns each one's id and pane.
    pub fn expire_detached(&self, now: u64) -> Vec<(String, String)> {
        let waiters = self.waiters.lock().unwrap();
        let gone: Vec<(String, String)> = self
            .raw(true)
            .iter()
            .filter_map(row_of)
            .filter(|r| !waiters.contains_key(&r.id))
            .map(|r| (r.id, r.pane))
            .collect();
        for (id, _) in &gone {
            self.expire(id, now);
        }
        gone
    }

    fn row(&self, id: &str) -> Option<Row> {
        self.raw(false)
            .iter()
            .filter_map(row_of)
            .find(|r| r.id == id)
    }

    /// The pane an approval is for.
    pub fn pane_of(&self, id: &str) -> Option<String> {
        self.row(id).map(|r| r.pane)
    }

    /// What the database does not have yet is laid over what it gave, so
    /// an approval just asked for is listed and one just answered never
    /// shows as waiting again (db.rs).
    ///
    /// `unwritten`: what was not written yet, taken *before* `rows` were
    /// read. Taken after, a change that was written in between is in
    /// neither: the rows are from before it and the list of what is
    /// unwritten no longer has it, and a question just let go read as
    /// still waiting (found by the scenario test, one run in five).
    fn overlaid(&self, mut rows: Vec<Value>, pending_only: bool, unwritten: Vec<Op>) -> Vec<Value> {
        for op in unwritten {
            match op {
                Op::ApprovalNew {
                    id,
                    ts,
                    pane,
                    project,
                    kind,
                    summary,
                    detail,
                } => {
                    if !rows.iter().any(|r| r["id"] == id.as_str()) {
                        rows.insert(
                            0,
                            json!({
                                "id": id, "ts": ts, "pane": pane, "project": project,
                                "kind": kind, "summary": summary,
                                "detail": serde_json::from_str::<Value>(&detail).unwrap_or(Value::Null),
                                "status": "pending", "answered_by": null, "reason": null,
                                "station": self.station,
                            }),
                        );
                    }
                }
                Op::ApprovalStatus {
                    id,
                    status,
                    by,
                    reason,
                    only_pending,
                    ..
                } => {
                    if let Some(r) = rows.iter_mut().find(|r| r["id"] == id.as_str()) {
                        if !only_pending || r["status"] == "pending" {
                            r["status"] = json!(status);
                            r["answered_by"] = json!(by);
                            r["reason"] = json!(reason);
                            // Held in memory: lost if this station ends
                            // before it is written.
                            r["saved"] = json!(false);
                        }
                    }
                }
                _ => {}
            }
        }
        if pending_only {
            rows.retain(|r| r["status"] == "pending");
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r["ts"].as_i64().unwrap_or(0)));
        rows
    }

    /// The rows as they are kept, a hook's mark included: for this file.
    fn raw(&self, pending_only: bool) -> Vec<Value> {
        // An answer that is not written yet may be for a row that still
        // reads as waiting: every row is read then, and the waiting ones
        // are picked after what is unwritten is laid over them.
        let ops = self.db.pending();
        let unwritten = !ops.is_empty() || self.db.stats().waiting > 0;
        let rows = self
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT id,ts,pane,project,kind,summary,detail,status,answered_by,reason FROM approvals
                     WHERE (?1=0 OR status='pending') ORDER BY ts DESC LIMIT 200",
                )?;
                let rows = stmt.query_map([(pending_only && !unwritten) as i64], |r| {
                    let detail: String = r.get(6)?;
                    Ok(json!({
                        "id": r.get::<_, String>(0)?, "ts": r.get::<_, i64>(1)?,
                        "pane": r.get::<_, String>(2)?, "project": r.get::<_, Option<String>>(3)?,
                        "kind": r.get::<_, String>(4)?, "summary": r.get::<_, String>(5)?,
                        "detail": serde_json::from_str::<Value>(&detail).unwrap_or(Value::Null),
                        "status": r.get::<_, String>(7)?, "answered_by": r.get::<_, Option<String>>(8)?,
                        "reason": r.get::<_, Option<String>>(9)?, "station": self.station,
                    }))
                })?;
                rows.collect::<rusqlite::Result<Vec<Value>>>()
            })
            .unwrap_or_default();
        self.overlaid(rows, pending_only, ops)
    }

    /// The approvals, as anyone is shown them: a hook's mark is left out,
    /// and one that waits says whether its hook is connected (`attached`:
    /// false while the agent is finding its way back after a restart).
    ///
    /// A held question or plan is not an approval and is not here: what it
    /// says is the owner's conversation, and this list is read by callers
    /// who may not read that (`held`).
    pub fn list(&self, pending_only: bool) -> Vec<Value> {
        self.shown(pending_only, false)
    }

    /// The questions and plans held for the owner, shown the same way.
    /// The owner's only.
    pub fn held(&self, pending_only: bool) -> Vec<Value> {
        self.shown(pending_only, true)
    }

    fn shown(&self, pending_only: bool, held: bool) -> Vec<Value> {
        let mut rows = self.raw(pending_only);
        rows.retain(|r| held_kind(r) == held);
        let waiters = self.waiters.lock().unwrap();
        for r in rows.iter_mut() {
            if let Some(detail) = r["detail"].as_object_mut() {
                detail.remove("hook");
            }
            if r["status"] == "pending" {
                let attached = r["id"].as_str().is_some_and(|id| waiters.contains_key(id));
                r["attached"] = json!(attached);
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_secs(300);
    const HOOK: &str = "0123456789abcdef0123456789abcdef";

    fn book() -> ApprovalBook {
        ApprovalBook::new(Arc::new(Db::memory().unwrap()), "alpha")
    }

    fn ask(b: &ApprovalBook, pane: &str, hook: Option<&str>, now: u64) -> Asked {
        b.ask(
            pane,
            None,
            "permission",
            "Bash: cargo publish",
            json!({"tool": {"name": "Bash"}}),
            hook,
            now,
            LIMIT,
        )
    }

    fn waiting(
        a: Asked,
    ) -> (
        String,
        u64,
        oneshot::Receiver<ApprovalAnswer>,
        Duration,
        bool,
    ) {
        match a {
            Asked::Wait {
                id,
                waiter,
                rx,
                left,
                new,
            } => (id, waiter, rx, left, new),
            Asked::Answered { .. } => panic!("answered"),
            Asked::Gone => panic!("gone"),
        }
    }

    fn status(b: &ApprovalBook, id: &str) -> String {
        b.list(false)
            .iter()
            .find(|r| r["id"] == json!(id))
            .map(|r| r["status"].as_str().unwrap().to_string())
            .unwrap()
    }

    /// The audit's gap 4: an approval that was waiting when the daemon
    /// went stayed "pending" for good and could not be answered. A hook
    /// that asks again with the id it made finds its own row: the same
    /// approval, still answerable, the time already waited counted, and
    /// the answer reaches the hook that came back, once. The hook it
    /// replaced is told nothing and expires nothing.
    #[test]
    fn a_hook_that_comes_back_finds_its_own_approval_and_gets_the_answer() {
        let b = book();
        let (id, first, mut old_rx, left, new) = waiting(ask(&b, "p1", Some(HOOK), 1_000));
        assert!(new && left == LIMIT);
        assert_eq!(b.list(true)[0]["attached"], json!(true));

        // The connection is lost; the same hook asks again 20 s later.
        let (again, second, mut rx, left, new) = waiting(ask(&b, "p1", Some(HOOK), 21_000));
        assert_eq!(again, id, "the same approval, not a second one");
        assert!(!new);
        assert_eq!(
            left,
            Duration::from_secs(280),
            "counted from the first time"
        );
        assert_eq!(b.list(true).len(), 1);
        // The waiter it replaced ends without an answer and must not
        // expire the row that is being waited on again.
        assert!(old_rx.try_recv().is_err());
        assert!(!b.finish(&id, first));
        assert_eq!(status(&b, &id), "pending");

        let out = b.answer(&id, true, None, "you", 22_000, |_| true);
        assert_eq!(
            out,
            Answer::Delivered {
                pane: Some("p1".into())
            }
        );
        assert!(rx.try_recv().unwrap().allow);
        assert_eq!(status(&b, &id), "allowed");
        // Answered once: a second answer finds nothing to answer.
        assert_eq!(
            b.answer(&id, false, None, "you", 23_000, |_| true),
            Answer::NotWaiting { expired: None }
        );
        assert_eq!(status(&b, &id), "allowed");
        assert!(!b.finish(&id, second), "nothing left to expire");
    }

    /// Answered while the hook was away (the daemon was started again and
    /// the owner pressed Deny before the hook reconnected): the answer is
    /// kept, said to be kept, and given to the hook the moment it is
    /// back. The same after the answer was written to the database.
    #[test]
    fn an_answer_given_while_the_hook_is_away_is_kept_for_it() {
        let db = Arc::new(Db::memory().unwrap());
        let b = ApprovalBook::new(db.clone(), "alpha");
        let (id, ..) = waiting(ask(&b, "p1", Some(HOOK), 1_000));
        // A new daemon: the row is there, nobody waits on it.
        let b = ApprovalBook::new(db.clone(), "alpha");
        assert_eq!(b.list(true)[0]["attached"], json!(false));
        let out = b.answer(&id, false, Some("not today".into()), "you", 5_000, |_| true);
        assert_eq!(out, Answer::Recorded { pane: "p1".into() });
        assert_eq!(status(&b, &id), "denied");
        assert!(b.list(true).is_empty(), "it has left the waiting list");
        for _ in 0..2 {
            match ask(&b, "p1", Some(HOOK), 6_000) {
                Asked::Answered { id: got, answer } => {
                    assert_eq!(got, id);
                    assert!(!answer.allow);
                    assert_eq!(answer.reason.as_deref(), Some("not today"));
                }
                _ => panic!("the kept answer was not given"),
            }
            assert!(db.flush(Duration::from_secs(5)));
        }
        // The other answer cannot replace it.
        assert_eq!(
            b.answer(&id, true, None, "you", 7_000, |_| true),
            Answer::NotWaiting { expired: None }
        );
        assert_eq!(status(&b, &id), "denied");
    }

    /// Guarantees, Trust: "An agent has only what its task grants";
    /// `approval.answer` is the owner's alone. Reattaching must not be a
    /// way round that: a hook's id finds a row only for the pane that
    /// made it; no list shows the id or its hash; and what a pane sends
    /// as an id that is not one is not kept at all.
    #[test]
    fn a_row_is_found_again_only_by_its_own_pane_and_the_id_is_never_shown() {
        let b = book();
        let (id, ..) = waiting(ask(&b, "p1", Some(HOOK), 1_000));
        // Another pane with the same id: a row of its own.
        let (other, ..) = waiting(ask(&b, "p2", Some(HOOK), 2_000));
        assert_ne!(other, id);
        // The approval's own id is not the hook's id.
        let (third, ..) = waiting(ask(&b, "p1", Some(&id), 3_000));
        assert_ne!(third, id);
        b.answer(&id, true, None, "you", 4_000, |_| true);
        // p2 does not get p1's answer by asking with p1's id.
        assert!(matches!(
            ask(&b, "p2", Some(HOOK), 5_000),
            Asked::Wait { new: false, .. }
        ));
        let shown = json!(b.list(false)).to_string();
        assert!(!shown.contains(HOOK) && !shown.contains(&hook_mark(HOOK)));
        assert!(!shown.contains("\"hook\""), "{shown}");
        assert_eq!(b.list(false)[0]["detail"]["tool"]["name"], "Bash");
        // Not an id: every ask is a new row, as with no id at all.
        for bad in [Some("short"), Some("has a space in it, and more"), None] {
            let (a1, ..) = waiting(ask(&b, "p3", bad, 6_000));
            let (a2, ..) = waiting(ask(&b, "p3", bad, 6_000));
            assert_ne!(a1, a2);
        }
    }

    /// After a start: an approval whose hook did not come back is expired
    /// and leaves the list (it stayed for ever, the audit's gap 4); one
    /// whose hook did come back is left alone. An approval with no hook
    /// id (asked for by a version before this) cannot be come back for,
    /// so an answer to it with nobody waiting expires it, as before. The
    /// same for one whose pane is no longer running.
    #[test]
    fn what_no_hook_came_back_for_is_expired_and_what_one_did_is_not() {
        let db = Arc::new(Db::memory().unwrap());
        let before = ApprovalBook::new(db.clone(), "alpha");
        let (back, ..) = waiting(ask(&before, "p1", Some(HOOK), 1_000));
        let (lost, ..) = waiting(ask(
            &before,
            "p2",
            Some("ffffffffffffffffffffffffffffffff"),
            1_000,
        ));
        let (old, ..) = waiting(ask(&before, "p3", None, 1_000));
        let (dead, ..) = waiting(ask(
            &before,
            "p4",
            Some("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
            1_000,
        ));
        let b = ApprovalBook::new(db, "alpha");
        assert_eq!(b.list(true).len(), 4);
        assert_eq!(
            b.answer(&old, true, None, "you", 2_000, |_| true),
            Answer::NotWaiting {
                expired: Some("p3".into())
            }
        );
        assert_eq!(
            b.answer(&dead, true, None, "you", 2_000, |pane| pane != "p4"),
            Answer::NotWaiting {
                expired: Some("p4".into())
            }
        );
        let (again, waiter, _rx, ..) = waiting(ask(&b, "p1", Some(HOOK), 3_000));
        assert_eq!(again, back);
        assert_eq!(
            b.expire_detached(16_000),
            vec![(lost.clone(), "p2".to_string())]
        );
        assert_eq!(status(&b, &lost), "expired");
        assert_eq!(status(&b, &back), "pending");
        // The hook of an expired row that turns up late is told it is gone.
        assert!(matches!(
            ask(&b, "p2", Some("ffffffffffffffffffffffffffffffff"), 17_000),
            Asked::Gone
        ));
        // Its time runs out with the hook waiting: the waiter expires it.
        assert!(b.finish(&back, waiter));
        b.expire(&back, 301_000);
        assert_eq!(status(&b, &back), "expired");
        // A pane that ends: what it asked is cancelled.
        let (asked, waiter, mut rx, ..) = waiting(ask(&b, "p5", Some(HOOK), 302_000));
        assert_eq!(b.cancel_pane("p5", 303_000), vec![asked.clone()]);
        assert!(rx.try_recv().is_err());
        assert!(!b.finish(&asked, waiter));
        assert_eq!(status(&b, &asked), "cancelled");
    }

    /// Mobile plan, phase M3. A question the agent asked is held like an
    /// approval and is what was said in a conversation, so it must not be
    /// where approvals are listed: `approval.list` and the snapshot are
    /// read by callers that may not read a conversation. And "Answer in
    /// the terminal instead" answers nothing: the hook that waits falls
    /// through, one that comes back is told it is gone, and no answer can
    /// be given afterwards.
    #[test]
    fn a_held_question_is_in_no_list_of_approvals_and_can_be_let_go() {
        let b = book();
        let asked = b.ask(
            "p1",
            None,
            "question",
            "asks a question",
            json!({"input": {"questions": [{"question": "Which herons count?"}]}}),
            Some(HOOK),
            1_000,
            LIMIT,
        );
        let (id, waiter, mut rx, ..) = waiting(asked);
        assert!(b.list(false).is_empty() && b.list(true).is_empty());
        assert!(b.holds("p1") && !b.holds("p2"));
        let held = b.held(true);
        assert_eq!(held.len(), 1);
        assert_eq!(held[0]["attached"], json!(true));
        assert!(!json!(held).to_string().contains("\"hook\""));
        assert_eq!(
            b.kind_and_status(&id),
            Some(("question".into(), "pending".into()))
        );
        // A permission request beside it is listed as it always was.
        let (perm, ..) = waiting(ask(&b, "p1", None, 1_500));
        assert_eq!(b.list(true).len(), 1);
        assert_eq!(b.list(true)[0]["id"], json!(perm));
        assert_eq!(b.held(true).len(), 1);

        assert_eq!(b.release(&id, "you", 2_000), Some("p1".into()));
        assert!(rx.try_recv().is_err(), "the hook is told nothing");
        assert!(!b.finish(&id, waiter), "and expires nothing");
        assert!(b.held(true).is_empty() && !b.holds("p1"));
        assert_eq!(b.held(false)[0]["status"], "released");
        assert_eq!(b.release(&id, "you", 2_500), None, "let go once");
        assert_eq!(
            b.answer(&id, true, Some("{}".into()), "you", 3_000, |_| true),
            Answer::NotWaiting { expired: None }
        );
        assert!(matches!(
            b.ask(
                "p1",
                None,
                "question",
                "asks a question",
                json!({}),
                Some(HOOK),
                4_000,
                LIMIT
            ),
            Asked::Gone
        ));
    }
}
