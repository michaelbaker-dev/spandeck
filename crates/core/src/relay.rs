//! Messages as stored records, and the relay that carries them between stations.
//!
//! Guarantees (Messages):
//! - Every message has a unique id. Delivery is at least once; the receiver
//!   drops duplicates by id, so a reconnect cannot start the same task twice.
//! - Ordering is guaranteed within a thread: each destination has one ordered
//!   queue and the head is retried until acknowledged.
//! - Received and acted-on are separate acknowledgments.
//!
//! The store knows nothing about the network. `Transport` is the swappable
//! station link: HTTP in production, simulated TCP under Turmoil in tests.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

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

pub fn split_address(addr: &str) -> (&str, Option<&str>) {
    match addr.rsplit_once('@') {
        Some((name, station)) if !station.is_empty() => (name, Some(station)),
        _ => (addr, None),
    }
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

    /// Try the head of each destination's queue once. Returns messages delivered.
    pub async fn pump(&self, transport: &dyn Transport, now: u64) -> usize {
        let heads: Vec<(String, String)> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT o.msg, o.dest FROM outbox o
                     WHERE o.state='pending' AND o.ord = (SELECT MIN(ord) FROM outbox WHERE dest=o.dest AND state='pending')
                       AND o.next_try <= ?1",
                )?;
                let rows = stmt.query_map([now as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect()
            })
            .unwrap_or_default();
        let mut delivered = 0;
        for (msg, dest) in heads {
            let Some(env) = self.get(&msg) else {
                let _ = self
                    .db
                    .with(|c| c.execute("DELETE FROM outbox WHERE msg=?1", [&msg]));
                continue;
            };
            match transport.deliver(&dest, &env).await {
                Ok(RelayAck { received: true, .. }) => {
                    let _ = self.db.with_tx(|tx| {
                        tx.execute("UPDATE outbox SET state='done' WHERE msg=?1", [&msg])?;
                        tx.execute(
                            "UPDATE messages SET status='delivered', received=?2 WHERE id=?1 AND status='queued'",
                            params![msg, now as i64],
                        )
                    });
                    delivered += 1;
                }
                Ok(RelayAck {
                    refused: Some(why), ..
                }) => {
                    let _ = self.db.with_tx(|tx| {
                        tx.execute("UPDATE outbox SET state='refused' WHERE msg=?1", [&msg])?;
                        tx.execute(
                            "UPDATE messages SET status=?2 WHERE id=?1",
                            params![msg, format!("refused: {why}")],
                        )
                    });
                }
                // Unreachable, or they could not store it: keep it at the head.
                _ => {
                    let _ = self.db.with(|c| {
                        c.execute(
                            "UPDATE outbox SET attempts=attempts+1,
                               next_try=?2 + MIN(30000, 500 * (1 << MIN(attempts, 6))) WHERE msg=?1",
                            params![msg, now as i64],
                        )
                    });
                }
            }
        }
        delivered
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
                     AND received IS NOT NULL AND read IS NULL AND (announced=0 OR announced < ?2) ORDER BY received",
                )?;
                let rows = stmt.query_map(params![list, told_before_ms as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect()
            })
            .unwrap_or_default()
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
