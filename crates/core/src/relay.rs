//! Messages as stored records, and the relay that carries them between stations.
//!
//! Guarantees (Messages):
//! - Every message has a unique id. Delivery is at least once; the receiver
//!   drops duplicates by id, so a reconnect cannot start the same task twice.
//! - Ordering is guaranteed within a thread: each destination has one ordered
//!   queue and one sender, and the head is retried until acknowledged.
//! - Received and acted-on are separate acknowledgments.
//!
//! The store knows nothing about the network. `Transport` is the swappable
//! station link: HTTP in production, simulated TCP under Turmoil in tests.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::Db;
use crate::paths::new_id;

pub const KINDS: &[&str] = &["task", "question", "result", "note"];
/// How long a cross-station task authorization lasts.
pub const TASK_AUTH_MS: u64 = 10 * 60 * 1000;
const MAX_HOPS: u32 = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAuth {
    /// Who approved the route (the user, at a Control Deck).
    pub approved_by: String,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: String,
    pub thread: String,
    pub seq: u64,
    pub ts: u64,
    /// `name@station`
    pub sender: String,
    pub recipient: String,
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub refs: Value,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub auth: Option<TaskAuth>,
    #[serde(default)]
    pub hops: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayAck {
    pub received: bool,
    #[serde(default)]
    pub duplicate: bool,
    /// Set when the message was refused for good (never retried).
    #[serde(default)]
    pub refused: Option<String>,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The station link. An `Err` means "could not reach them": retry later.
pub trait Transport: Send + Sync {
    fn deliver<'a>(
        &'a self,
        station: &'a str,
        env: &'a Envelope,
    ) -> BoxFuture<'a, Result<RelayAck, String>>;
}

#[derive(Debug, Clone, Default)]
pub struct NewMessage {
    pub sender: String,
    pub recipient: String,
    pub kind: String,
    pub body: String,
    pub refs: Value,
    pub thread: Option<String>,
    pub task: Option<String>,
    pub project: Option<String>,
    pub auth: Option<TaskAuth>,
}

/// The row of a request id (rids.rs) that a message's transaction writes.
#[derive(Debug, Clone)]
pub struct SendMark {
    pub key: String,
    pub method: String,
    pub hash: String,
}

/// What the sender of a message is answered.
pub fn sent_answer(env: &Envelope, remote: bool) -> Value {
    json!({"id": env.id, "thread": env.thread, "status": if remote { "queued" } else { "delivered" }})
}

pub fn split_address(addr: &str) -> (&str, Option<&str>) {
    match addr.rsplit_once('@') {
        Some((name, station)) if !station.is_empty() => (name, Some(station)),
        _ => (addr, None),
    }
}

/// How long after a failed try the next one is: half a second, doubling
/// to 30 s.
pub fn retry_after_ms(attempts: u32) -> u64 {
    (500u64 << attempts.min(6)).min(30_000)
}

/// Who a note about a message that was not delivered is from.
pub const NOTE_FROM: &str = "spandeck";

/// The status of a message whose pane was closed before it read it.
pub const RETURNED: &str = "undelivered: its pane was closed before it read this";

/// Text from another station, to be shown or stored as text: no control
/// characters, and bounded.
fn plain(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// What the sender of a message is told when the other station refused it
/// for good. `why` is that station's own words: data, shown as text.
pub fn refusal_words(env: &Envelope, why: &str, attempts: u32) -> String {
    let expired = why == "the task authorization has expired";
    let reason = if expired {
        format!(
            "its task authorization expired while it waited (an authorization lasts {} minutes). It was not sent again: send the task again if it is still wanted",
            TASK_AUTH_MS / 60_000
        )
    } else {
        plain(why, 200)
    };
    let mut words = format!(
        "Not delivered to {}: {reason}. The message was your {} \"{}\" ({}).",
        env.recipient,
        env.kind,
        plain(&env.body, 80),
        env.id
    );
    // A station from before 15 Nov 2026 looks at the authorization before
    // it looks for a duplicate: a task it took at a try whose answer was
    // lost is refused as expired at the next.
    if expired && attempts > 0 {
        words.push_str(&format!(
            " An earlier try got no answer, so it may have arrived then: ask {} before sending it again.",
            env.recipient
        ));
    }
    words
}

#[derive(Clone)]
pub struct MessageStore {
    db: Arc<Db>,
    pub station: String,
}

impl MessageStore {
    pub fn new(db: Arc<Db>, station: &str) -> MessageStore {
        MessageStore {
            db,
            station: station.to_string(),
        }
    }

    fn qualify(&self, addr: &str) -> String {
        match split_address(addr) {
            (_, Some(_)) => addr.to_string(),
            (name, None) => format!("{name}@{}", self.station),
        }
    }

    /// Store a message. One for another station also joins that station's queue.
    pub fn send(&self, new: NewMessage, now: u64) -> Result<Envelope, String> {
        self.send_marked(new, now, None)
    }

    /// `send` for a request that carries an id: the id's row is written in
    /// the same transaction as the message, so the two are stored together
    /// or not at all, and a second try of the same request can never store
    /// a second message (the row's key is the table's primary key: a
    /// second insert fails the whole transaction).
    pub fn send_marked(
        &self,
        new: NewMessage,
        now: u64,
        mark: Option<&SendMark>,
    ) -> Result<Envelope, String> {
        if !KINDS.contains(&new.kind.as_str()) {
            return Err(format!("kind is one of {}", KINDS.join(", ")));
        }
        if new.body.trim().is_empty() {
            return Err("a message needs a body".into());
        }
        let sender = self.qualify(&new.sender);
        let recipient = self.qualify(&new.recipient);
        let dest = split_address(&recipient)
            .1
            .unwrap_or(&self.station)
            .to_string();
        let id = new_id("m");
        let thread = new.thread.unwrap_or_else(|| id.clone());
        let local = dest == self.station;
        let env = self
            .db
            .with_tx_stored(|tx| {
                let seq: i64 = tx.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM messages WHERE thread=?1", [&thread], |r| r.get(0))?;
                let env = Envelope {
                    id: id.clone(),
                    thread: thread.clone(),
                    seq: seq as u64,
                    ts: now,
                    sender: sender.clone(),
                    recipient: recipient.clone(),
                    kind: new.kind.clone(),
                    body: new.body.clone(),
                    refs: if new.refs.is_null() { json!([]) } else { new.refs.clone() },
                    task: new.task.clone(),
                    project: new.project.clone(),
                    auth: new.auth.clone(),
                    hops: 0,
                };
                insert(tx, &env, if local { "delivered" } else { "queued" }, local.then_some(now))?;
                if !local {
                    tx.execute(
                        "INSERT INTO outbox(msg,dest,ord) VALUES(?1,?2,(SELECT COALESCE(MAX(ord),0)+1 FROM outbox))",
                        params![env.id, dest],
                    )?;
                }
                if let Some(mark) = mark {
                    tx.execute(
                        "INSERT INTO kv(k,v) VALUES(?1,?2)",
                        params![
                            mark.key,
                            crate::rids::done_row(&mark.method, &mark.hash, &sent_answer(&env, !local), now)
                        ],
                    )?;
                }
                Ok(env)
            })
            .map_err(|e| e.to_string())?;
        Ok(env)
    }

    /// Receiver side. Idempotent: the same envelope twice is stored once.
    pub fn accept(&self, env: &Envelope, now: u64) -> RelayAck {
        let refused = |why: &str| RelayAck {
            received: false,
            duplicate: false,
            refused: Some(why.to_string()),
        };
        if split_address(&env.recipient).1 != Some(self.station.as_str()) {
            return refused("not for this station");
        }
        if env.hops > MAX_HOPS {
            return refused("too many hops");
        }
        if !KINDS.contains(&env.kind.as_str()) {
            return refused("unknown message kind");
        }
        // A message already taken is acknowledged whatever has changed
        // since: its authorization may have run out between the try whose
        // answer was lost and this one, and its sender would be told "not
        // delivered" of a task that was. Nothing new is accepted by this.
        let taken = self
            .db
            .read(|c| {
                c.query_row("SELECT 1 FROM receipts WHERE msg=?1", [&env.id], |_| Ok(()))
                    .optional()
            })
            .ok()
            .flatten()
            .is_some();
        if taken {
            return RelayAck {
                received: true,
                duplicate: true,
                refused: None,
            };
        }
        // No new remote assignment without a valid, unexpired authorization.
        if env.kind == "task" {
            match &env.auth {
                Some(a) if a.expires_ms > now => {}
                Some(_) => return refused("the task authorization has expired"),
                None => return refused("a task from another station needs an authorization"),
            }
        }
        let r = self.db.with_tx_stored(|tx| {
            let seen: Option<i64> = tx
                .query_row("SELECT ts FROM receipts WHERE msg=?1", [&env.id], |r| {
                    r.get(0)
                })
                .optional()?;
            if seen.is_some() {
                return Ok(true);
            }
            tx.execute(
                "INSERT INTO receipts(msg,ts) VALUES(?1,?2)",
                params![env.id, now as i64],
            )?;
            insert(tx, env, "delivered", Some(now))?;
            Ok(false)
        });
        match r {
            Ok(duplicate) => RelayAck {
                received: true,
                duplicate,
                refused: None,
            },
            // A storage error is not a refusal: the sender retries.
            Err(_) => RelayAck {
                received: false,
                duplicate: false,
                refused: None,
            },
        }
    }

    /// The oldest message waiting for `dest`, and how many tries of it
    /// have failed.
    pub fn head(&self, dest: &str) -> Option<(String, u32)> {
        self.db
            .read(|c| {
                c.query_row(
                    "SELECT msg, attempts FROM outbox WHERE dest=?1 AND state='pending' ORDER BY ord LIMIT 1",
                    [dest],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
            })
            .ok()
            .flatten()
    }

    /// The stations something is waiting for.
    pub fn waiting_for(&self) -> Vec<String> {
        self.db
            .read(|c| {
                let mut stmt =
                    c.prepare("SELECT DISTINCT dest FROM outbox WHERE state='pending' LIMIT 256")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect()
            })
            .unwrap_or_default()
    }

    /// The other station has it.
    fn settle_delivered(&self, msg: &str, now: u64) -> bool {
        self.db
            .with_tx(|tx| {
                tx.execute("UPDATE outbox SET state='done' WHERE msg=?1", [msg])?;
                tx.execute(
                    "UPDATE messages SET status='delivered', received=?2 WHERE id=?1 AND status='queued'",
                    params![msg, now as i64],
                )
            })
            .is_ok()
    }

    /// The other station refused it for good. Its sender is told in its
    /// own inbox, in the transaction that marks the refusal: the two are
    /// stored together or not at all, so a refusal is told once, whatever
    /// ends the sender in between. `Ok(None)`: it was already settled.
    fn settle_refused(
        &self,
        env: &Envelope,
        why: &str,
        attempts: u32,
        now: u64,
    ) -> rusqlite::Result<Option<Envelope>> {
        let words = refusal_words(env, why, attempts);
        let status = format!("refused: {}", plain(why, 200));
        self.db.with_tx(|tx| {
            let marked = tx.execute(
                "UPDATE outbox SET state='refused' WHERE msg=?1 AND state='pending'",
                [&env.id],
            )?;
            if marked == 0 {
                return Ok(None);
            }
            tx.execute(
                "UPDATE messages SET status=?2 WHERE id=?1",
                params![env.id, status],
            )?;
            let seq: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq),0)+1 FROM messages WHERE thread=?1",
                [&env.thread],
                |r| r.get(0),
            )?;
            let note = Envelope {
                id: new_id("m"),
                thread: env.thread.clone(),
                seq: seq as u64,
                ts: now,
                sender: format!("{NOTE_FROM}@{}", self.station),
                recipient: env.sender.clone(),
                kind: "note".into(),
                body: words.clone(),
                refs: json!([env.id]),
                task: env.task.clone(),
                project: env.project.clone(),
                auth: None,
                hops: 0,
            };
            insert(tx, &note, "delivered", Some(now))?;
            Ok(Some(note))
        })
    }

    /// A try got no answer, or the other station could not store it.
    fn settle_failed(&self, msg: &str) {
        let _ = self
            .db
            .with(|c| c.execute("UPDATE outbox SET attempts=attempts+1 WHERE msg=?1", [msg]));
    }

    pub fn queued(&self) -> u64 {
        self.db
            .read(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM outbox WHERE state='pending'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
            })
            .unwrap_or(0) as u64
    }

    pub fn get(&self, id: &str) -> Option<Envelope> {
        self.db
            .read(|c| {
                c.query_row(
                    "SELECT id,thread,seq,ts,sender,recipient,kind,body,refs,task,project,auth FROM messages WHERE id=?1",
                    [id],
                    row_env,
                )
                .optional()
            })
            .ok()
            .flatten()
    }

    /// Messages for any of these local names, oldest first, in thread order.
    pub fn inbox(
        &self,
        names: &[String],
        unread_only: bool,
        mark_read: bool,
        now: u64,
    ) -> Vec<Value> {
        let recipients: Vec<String> = names.iter().map(|n| self.qualify(n)).collect();
        let list = json!(recipients).to_string();
        let rows: Vec<Value> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT id,thread,seq,ts,sender,recipient,kind,body,refs,task,project,status,read,acted FROM messages
                     WHERE recipient IN (SELECT value FROM json_each(?1)) AND received IS NOT NULL
                       AND status NOT LIKE 'undelivered%'
                       AND (?2=0 OR read IS NULL) ORDER BY received, thread, seq LIMIT 200",
                )?;
                let rows = stmt.query_map(params![list, unread_only as i64], row_view)?;
                rows.collect()
            })
            .unwrap_or_default();
        if mark_read && !rows.is_empty() {
            let ids = json!(rows
                .iter()
                .filter_map(|r| r["id"].as_str())
                .collect::<Vec<_>>())
            .to_string();
            let _ = self.db.with(|c| {
                c.execute(
                    "UPDATE messages SET read=?2, status=CASE WHEN status='delivered' THEN 'read' ELSE status END, announced=1
                     WHERE id IN (SELECT value FROM json_each(?1)) AND read IS NULL",
                    params![ids, now as i64],
                )
            });
        }
        rows
    }

    /// Unread messages for these names that the agent has not been told about,
    /// or was told about before `told_before_ms` and still has not read.
    pub fn unannounced(
        &self,
        names: &[String],
        told_before_ms: u64,
    ) -> Vec<(String, String, String)> {
        let list = json!(names.iter().map(|n| self.qualify(n)).collect::<Vec<_>>()).to_string();
        self.db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, sender, body FROM messages WHERE recipient IN (SELECT value FROM json_each(?1))
                     AND received IS NOT NULL AND read IS NULL AND status NOT LIKE 'undelivered%'
                     AND (announced=0 OR announced < ?2) ORDER BY received",
                )?;
                let rows = stmt.query_map(params![list, told_before_ms as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect()
            })
            .unwrap_or_default()
    }

    /// Every unread message for these names: its id, its sender, and when
    /// its agent was last told of it (0: never), oldest first.
    pub fn unread_told(&self, names: &[String]) -> Vec<(String, String, u64)> {
        let list = json!(names.iter().map(|n| self.qualify(n)).collect::<Vec<_>>()).to_string();
        self.db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, sender, announced FROM messages WHERE recipient IN (SELECT value FROM json_each(?1))
                     AND received IS NOT NULL AND read IS NULL AND status NOT LIKE 'undelivered%'
                     ORDER BY received",
                )?;
                let rows = stmt.query_map(params![list], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)?.max(0) as u64))
                })?;
                rows.collect()
            })
            .unwrap_or_default()
    }

    /// A pane was closed for good with mail it had not read. Mail is
    /// addressed by name, and a name is free again once its pane is
    /// closed: left as it was, that mail would be handed to whatever pane
    /// is next given the name (found 16 Nov 2026: a new pane inherited a
    /// closed pane's unread mail and was nudged about it at once). So it
    /// goes back: each message is marked "undelivered", which no pane's
    /// inbox shows, and each sender is told once, in its own inbox, in
    /// the same transaction (a sender on another station through that
    /// station's queue). A note of spandeck's own is not answered with
    /// another. Returns each note, and how many messages went back.
    pub fn return_unread(
        &self,
        names: &[String],
        now: u64,
    ) -> rusqlite::Result<(Vec<Envelope>, usize)> {
        let list = json!(names.iter().map(|n| self.qualify(n)).collect::<Vec<_>>()).to_string();
        let from_us = format!("{NOTE_FROM}@");
        self.db.with_tx(|tx| {
            let unread: Vec<Envelope> = {
                let mut stmt = tx.prepare(
                    "SELECT id,thread,seq,ts,sender,recipient,kind,body,refs,task,project,auth FROM messages
                     WHERE recipient IN (SELECT value FROM json_each(?1)) AND received IS NOT NULL
                       AND read IS NULL AND status='delivered' ORDER BY received LIMIT 500",
                )?;
                let rows = stmt.query_map(params![list], row_env)?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            let mut notes: Vec<Envelope> = vec![];
            for env in &unread {
                tx.execute(
                    "UPDATE messages SET status=?2, announced=1 WHERE id=?1",
                    params![env.id, RETURNED],
                )?;
                if env.sender.starts_with(&from_us) {
                    continue;
                }
                // One note for each sender, naming its first message.
                if let Some(note) = notes.iter_mut().find(|n| n.recipient == env.sender) {
                    if let Some(refs) = note.refs.as_array_mut().filter(|r| r.len() < 20) {
                        refs.push(json!(env.id));
                    }
                    continue;
                }
                notes.push(Envelope {
                    id: new_id("m"),
                    thread: env.thread.clone(),
                    seq: 0,
                    ts: now,
                    sender: format!("{NOTE_FROM}@{}", self.station),
                    recipient: env.sender.clone(),
                    kind: "note".into(),
                    body: plain(&env.body, 80),
                    refs: json!([env.id]),
                    task: env.task.clone(),
                    project: env.project.clone(),
                    auth: None,
                    hops: 0,
                });
            }
            for note in &mut notes {
                let count = unread.iter().filter(|e| e.sender == note.recipient).count();
                let first = unread
                    .iter()
                    .find(|e| e.sender == note.recipient)
                    .expect("a note is made for a message");
                let what = if count == 1 {
                    format!("your {} \"{}\" ({})", first.kind, note.body, first.id)
                } else {
                    format!(
                        "your {count} messages (the first: {} \"{}\", {})",
                        first.kind, note.body, first.id
                    )
                };
                note.body = format!(
                    "Not delivered to {}: its pane was closed before it read {what}. Nothing was passed to another pane: send it again to a pane that is running, if it is still wanted.",
                    first.recipient
                );
                note.seq = tx.query_row(
                    "SELECT COALESCE(MAX(seq),0)+1 FROM messages WHERE thread=?1",
                    [&note.thread],
                    |r| r.get::<_, i64>(0),
                )? as u64;
                let dest = split_address(&note.recipient).1.unwrap_or(&self.station);
                let local = dest == self.station;
                insert(
                    tx,
                    note,
                    if local { "delivered" } else { "queued" },
                    local.then_some(now),
                )?;
                if !local {
                    tx.execute(
                        "INSERT INTO outbox(msg,dest,ord) VALUES(?1,?2,(SELECT COALESCE(MAX(ord),0)+1 FROM outbox))",
                        params![note.id, dest],
                    )?;
                }
            }
            Ok((notes, unread.len()))
        })
    }

    /// Remember when the recipient was told about these messages.
    pub fn mark_announced(&self, ids: &[String], now: u64) {
        let list = json!(ids).to_string();
        let _ = self.db.with(|c| {
            c.execute(
                "UPDATE messages SET announced=?2 WHERE id IN (SELECT value FROM json_each(?1))",
                params![list, now.max(1) as i64],
            )
        });
    }

    /// The acted-on acknowledgment (separate from received).
    pub fn ack(&self, id: &str, now: u64) -> bool {
        self.db
            .with(|c| {
                c.execute(
                    "UPDATE messages SET acted=?2, read=COALESCE(read, ?2), status='answered', announced=1 WHERE id=?1",
                    params![id, now as i64],
                )
            })
            .is_ok_and(|n| n > 0)
    }

    pub fn thread(&self, thread: &str) -> Vec<Value> {
        self.db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT id,thread,seq,ts,sender,recipient,kind,body,refs,task,project,status,read,acted FROM messages
                     WHERE thread=?1 ORDER BY seq",
                )?;
                let rows = stmt.query_map([thread], row_view)?;
                rows.collect()
            })
            .unwrap_or_default()
    }

    pub fn recent(&self, project: Option<&str>, limit: u32) -> Vec<Value> {
        self.db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT id,thread,seq,ts,sender,recipient,kind,body,refs,task,project,status,read,acted FROM messages
                     WHERE (?1 IS NULL OR project=?1) ORDER BY ts DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![project, limit], row_view)?;
                rows.collect()
            })
            .unwrap_or_default()
    }
}

fn insert(
    tx: &rusqlite::Transaction<'_>,
    env: &Envelope,
    status: &str,
    received: Option<u64>,
) -> rusqlite::Result<usize> {
    tx.execute(
        "INSERT INTO messages(id,thread,seq,ts,sender,recipient,kind,body,refs,task,project,status,received,auth)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            env.id,
            env.thread,
            env.seq as i64,
            env.ts as i64,
            env.sender,
            env.recipient,
            env.kind,
            env.body,
            env.refs.to_string(),
            env.task,
            env.project,
            status,
            received.map(|r| r as i64),
            env.auth.as_ref().and_then(|a| serde_json::to_string(a).ok()),
        ],
    )
}

fn row_env(r: &rusqlite::Row<'_>) -> rusqlite::Result<Envelope> {
    let refs: String = r.get(8)?;
    let auth: Option<String> = r.get(11)?;
    Ok(Envelope {
        id: r.get(0)?,
        thread: r.get(1)?,
        seq: r.get::<_, i64>(2)? as u64,
        ts: r.get::<_, i64>(3)? as u64,
        sender: r.get(4)?,
        recipient: r.get(5)?,
        kind: r.get(6)?,
        body: r.get(7)?,
        refs: serde_json::from_str(&refs).unwrap_or(Value::Null),
        task: r.get(9)?,
        project: r.get(10)?,
        auth: auth.and_then(|a| serde_json::from_str(&a).ok()),
        hops: 0,
    })
}

fn row_view(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let refs: String = r.get(8)?;
    Ok(json!({
        "id": r.get::<_, String>(0)?, "thread": r.get::<_, String>(1)?, "seq": r.get::<_, i64>(2)?,
        "ts": r.get::<_, i64>(3)?, "from": r.get::<_, String>(4)?, "to": r.get::<_, String>(5)?,
        "kind": r.get::<_, String>(6)?, "body": r.get::<_, String>(7)?,
        "refs": serde_json::from_str::<Value>(&refs).unwrap_or(Value::Null),
        "task": r.get::<_, Option<String>>(9)?, "project": r.get::<_, Option<String>>(10)?,
        "status": r.get::<_, String>(11)?, "read": r.get::<_, Option<i64>>(12)?, "acted": r.get::<_, Option<i64>>(13)?,
    }))
}

// ---------------------------------------------------------------------------
// The senders: one per destination station.
//
// The owner's rule (15 Nov 2026): "the slow or absent station has to be
// fixed with the right architecture for a system like this, never to slow
// everyone else down." Messages for other stations used to be tried by one
// loop, one station after another, the oldest for each once every half
// second: a station that hung held the next station's message up to five
// seconds at each try, and a burst of ten took five seconds to arrive.
//
// Now each destination has a sender of its own: a task that takes the
// oldest message waiting for that station, sends it, and takes the next
// only when that one is settled (received, or refused for good). It has
// its own place in its own queue, its own limit on a try and its own wait
// after a failure. No sender waits for another, and nothing else in the
// station waits for any of them.
//
//   - `push`: a message was stored. Its destination's sender is started,
//     or told, at once.
//   - `pump`: the safety net, every half second. Every destination with
//     something waiting has a sender; one that ended without emptying its
//     queue is started again. It only starts tasks and waits for nothing.
//   - `back`: the destination answers again. Its wait is over.
//
// Nothing depends on the push: without it the pump finds the row within
// half a second, and a sender that is waiting looks again by itself.

/// One try is given up after this long, whatever the link does (the real
/// link gives up after five seconds by itself).
const TRY_DEADLINE: Duration = Duration::from_secs(8);
/// A station that says it is back shortens a wait this often at most.
const BACK_EVERY_MS: u64 = 2_000;
/// The decks are told of what arrived after this many, and whenever a
/// sender pauses.
const TELL_EVERY: usize = 25;

/// The switch (`[flags] relay_push`, or `station.flags`; read when the
/// station starts). Off: no sender is started when a message is stored and
/// none stays; every half second each destination's oldest message is
/// tried once, each destination in a task of its own.
pub const SWITCH: &str = "relay_push";

/// The station link as a sender uses it, with what the station does when
/// something is settled.
pub trait Link: Transport + Clone + 'static {
    /// `count` messages arrived at `dest` (told in groups, for the decks).
    fn arrived(&self, _dest: &str, _count: usize) {}
    /// `dest` refused `env` for good; `note` is what its sender was told,
    /// already in its inbox.
    fn refused(&self, _dest: &str, _env: &Envelope, _why: &str, _note: &Envelope) {}
    /// The wall clock, for the time a message is marked delivered.
    fn now_ms(&self) -> u64 {
        crate::paths::now_ms()
    }
}

#[derive(Default)]
struct Dest {
    /// The oldest message is not tried before this, on the forward clock.
    not_before_ms: u64,
    /// When a `back` last shortened the wait.
    back_ms: Option<u64>,
    /// Tries in a row that were not settled.
    fails: u32,
    sender: Option<Sender>,
}

struct Sender {
    id: u64,
    wake: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}

struct Senders {
    store: MessageStore,
    /// Waits are counted on a clock that only goes forward, and kept in
    /// memory: a message once waited 223 s after the wall clock was
    /// corrected (audit, section 10). After a restart nothing is held, so
    /// every waiting message is tried at once.
    began: tokio::time::Instant,
    push: AtomicBool,
    dests: Mutex<HashMap<String, Dest>>,
    ids: AtomicU64,
    /// Senders started again after one ended with its queue not empty.
    restarted: AtomicU64,
    /// Counts tries that ended and senders that left: tests wait on it.
    tries: tokio::sync::watch::Sender<u64>,
}

#[derive(Clone)]
pub struct Relay(Arc<Senders>);

impl Relay {
    pub fn new(store: MessageStore) -> Relay {
        Relay(Arc::new(Senders {
            store,
            began: tokio::time::Instant::now(),
            push: AtomicBool::new(true),
            dests: Mutex::new(HashMap::new()),
            ids: AtomicU64::new(0),
            restarted: AtomicU64::new(0),
            tries: tokio::sync::watch::Sender::new(0),
        }))
    }

    /// The switch `relay_push`.
    pub fn set_push(&self, on: bool) {
        self.0.push.store(on, Ordering::Relaxed);
    }

    pub fn pushes(&self) -> bool {
        self.0.push.load(Ordering::Relaxed)
    }

    fn clock(&self) -> u64 {
        self.0.began.elapsed().as_millis() as u64
    }

    /// A message for `dest` was stored (its transaction is committed): its
    /// sender is started, or told. Returns at once.
    pub fn push<L: Link>(&self, link: &L, dest: &str) {
        if self.pushes() {
            self.ensure(link, dest, true, false);
        }
    }

    /// The safety net, every half second: every destination with something
    /// waiting has a sender. Starts tasks; waits for nothing.
    pub fn pump<L: Link>(&self, link: &L) {
        let waiting = self.0.store.waiting_for();
        let push = self.pushes();
        for dest in &waiting {
            // With the switch off a sender makes one try and ends, so one
            // is started only when a try is due.
            if push || self.due_in(dest) == 0 {
                self.ensure(link, dest, false, !push);
            }
        }
        // Nothing is kept for a station nothing waits for.
        self.0
            .dests
            .lock()
            .unwrap()
            .retain(|dest, d| d.sender.is_some() || waiting.contains(dest));
    }

    /// `dest` answers again (it is listed as online again, or made a call
    /// here): what waits for it is tried now, not when its wait is over.
    pub fn back(&self, dest: &str) {
        let now = self.clock();
        let mut dests = self.0.dests.lock().unwrap();
        let Some(d) = dests.get_mut(dest) else {
            return;
        };
        if d.not_before_ms <= now || d.back_ms.is_some_and(|at| now - at < BACK_EVERY_MS) {
            return;
        }
        (d.not_before_ms, d.back_ms) = (0, Some(now));
        if let Some(s) = &d.sender {
            s.wake.notify_one();
        }
    }

    fn due_in(&self, dest: &str) -> u64 {
        let now = self.clock();
        let dests = self.0.dests.lock().unwrap();
        dests
            .get(dest)
            .map_or(0, |d| d.not_before_ms.saturating_sub(now))
    }

    fn ensure<L: Link>(&self, link: &L, dest: &str, tell: bool, once: bool) {
        let mut dests = self.0.dests.lock().unwrap();
        let d = dests.entry(dest.to_string()).or_default();
        if let Some(s) = &d.sender {
            if !s.task.is_finished() {
                if tell {
                    s.wake.notify_one();
                }
                return;
            }
            // It ended without leaving (it panicked): started again.
            self.0.restarted.fetch_add(1, Ordering::Relaxed);
            tracing::error!(dest, "a message sender ended; it is started again");
        }
        let id = self.0.ids.fetch_add(1, Ordering::Relaxed) + 1;
        let wake = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn(self.clone().send_to(
            link.clone(),
            dest.to_string(),
            id,
            wake.clone(),
            once,
        ));
        d.sender = Some(Sender { id, wake, task });
    }

    /// The sender for `dest` leaves, if nothing waits for that station.
    /// Looked at with the list locked, and a `push` comes after its
    /// message is committed: so either this sees the message, or the push
    /// finds no sender and starts one.
    fn leave_if_empty(&self, dest: &str, id: u64) -> bool {
        let mut dests = self.0.dests.lock().unwrap();
        if self.0.store.head(dest).is_some() {
            return false;
        }
        if dests
            .get(dest)
            .is_some_and(|d| d.sender.as_ref().is_some_and(|s| s.id == id))
        {
            dests.remove(dest);
        }
        drop(dests);
        self.0.tries.send_modify(|n| *n += 1);
        true
    }

    /// A sender that makes one try has made it.
    fn left(&self, dest: &str, id: u64) {
        if let Some(d) = self.0.dests.lock().unwrap().get_mut(dest) {
            if d.sender.as_ref().is_some_and(|s| s.id == id) {
                d.sender = None;
            }
        }
        self.0.tries.send_modify(|n| *n += 1);
    }

    fn tried(&self, dest: &str, wait_ms: Option<u64>) {
        let now = self.clock();
        if let Some(d) = self.0.dests.lock().unwrap().get_mut(dest) {
            match wait_ms {
                Some(wait) => (d.not_before_ms, d.fails) = (now + wait, d.fails.saturating_add(1)),
                None => (d.not_before_ms, d.fails, d.back_ms) = (0, 0, None),
            }
        }
        self.0.tries.send_modify(|n| *n += 1);
    }

    /// One destination's sender: the oldest message, then the next, each
    /// only when the one before it is settled.
    async fn send_to<L: Link>(
        self,
        link: L,
        dest: String,
        id: u64,
        wake: Arc<tokio::sync::Notify>,
        once: bool,
    ) {
        let store = &self.0.store;
        let mut arrived = 0usize;
        let tell = |arrived: &mut usize| {
            if *arrived > 0 {
                link.arrived(&dest, std::mem::take(arrived));
            }
        };
        loop {
            let wait = loop {
                let wait = self.due_in(&dest);
                if wait > 0 {
                    break wait;
                }
                let Some((msg, attempts)) = store.head(&dest) else {
                    tell(&mut arrived);
                    if once {
                        break 0;
                    }
                    if self.leave_if_empty(&dest, id) {
                        return;
                    }
                    continue;
                };
                let Some(env) = store.get(&msg) else {
                    let _ = store
                        .db
                        .with(|c| c.execute("DELETE FROM outbox WHERE msg=?1", [&msg]));
                    continue;
                };
                let ack = tokio::time::timeout(TRY_DEADLINE, link.deliver(&dest, &env))
                    .await
                    .unwrap_or_else(|_| Err("no answer in time".into()));
                // Settled only when the record of it is written: a message
                // marked neither way stays the oldest and is sent again,
                // which the other station answers as a duplicate.
                let settled = match ack {
                    Ok(RelayAck { received: true, .. }) => {
                        let stored = store.settle_delivered(&msg, link.now_ms());
                        arrived += stored as usize;
                        stored
                    }
                    Ok(RelayAck {
                        refused: Some(why), ..
                    }) => match store.settle_refused(&env, &why, attempts, link.now_ms()) {
                        Ok(Some(note)) => {
                            link.refused(&dest, &env, &why, &note);
                            true
                        }
                        Ok(None) => true,
                        Err(_) => false,
                    },
                    // Unreachable, or they could not store it: it stays
                    // the oldest.
                    _ => {
                        store.settle_failed(&msg);
                        false
                    }
                };
                if !settled {
                    let wait = retry_after_ms(attempts);
                    self.tried(&dest, Some(wait));
                    break wait;
                }
                self.tried(&dest, None);
                if arrived >= TELL_EVERY {
                    tell(&mut arrived);
                }
                if once {
                    break 0;
                }
            };
            tell(&mut arrived);
            if once {
                self.left(&dest, id);
                return;
            }
            // Until the wait is over, or the station is back. A message
            // stored meanwhile tells this sender too; it then finds the
            // wait not over and waits on.
            tokio::select! {
                _ = wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(wait)) => {}
            }
        }
    }

    /// For the Performance page and for tests: each destination that has a
    /// sender or a wait.
    pub fn view(&self) -> Value {
        let now = self.clock();
        let dests = self.0.dests.lock().unwrap();
        let mut list: Vec<Value> = dests
            .iter()
            .map(|(dest, d)| {
                json!({
                    "station": dest,
                    "sender": d.sender.as_ref().is_some_and(|s| !s.task.is_finished()),
                    "failed_in_a_row": d.fails,
                    "next_try_in_ms": d.not_before_ms.saturating_sub(now),
                })
            })
            .collect();
        list.sort_by(|a, b| a["station"].as_str().cmp(&b["station"].as_str()));
        json!({
            "push": self.pushes(),
            "restarted": self.0.restarted.load(Ordering::Relaxed),
            "senders": list,
        })
    }

    /// Counts tries that ended and senders that left.
    pub fn tries(&self) -> tokio::sync::watch::Receiver<u64> {
        self.0.tries.subscribe()
    }

    /// Tests: end `dest`'s sender where it stands, as a panic would.
    #[doc(hidden)]
    pub fn kill(&self, dest: &str) -> bool {
        let dests = self.0.dests.lock().unwrap();
        match dests.get(dest).and_then(|d| d.sender.as_ref()) {
            Some(s) => {
                s.task.abort();
                true
            }
            None => false,
        }
    }
}
