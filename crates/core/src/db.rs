//! SQLite state: the ship's log, pane records, messages, tasks, memory.
//!
//! Schema changes are append-only migrations keyed by `PRAGMA user_version`.
//! Never edit a shipped migration; add a new one.

//!
//! Nothing typed, stopped or answered waits for the database (DECISIONS.md,
//! "Nothing waits for the database"). One thread writes: the ship's log,
//! best effort and bounded (`log`); changes of state, kept until they are
//! written (`put`, `write_ack`). Reads have a connection of their own
//! (`read`). A caller that must know its write is stored before it answers
//! (a message) still writes itself (`with`, `with_tx`).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MIGRATIONS: &[&str] = &[
    // 1: ship's log, pane records, key/value
    r#"
    CREATE TABLE log (
        id      INTEGER PRIMARY KEY,
        ulid    TEXT NOT NULL UNIQUE,
        ts      INTEGER NOT NULL,
        kind    TEXT NOT NULL,
        pane    TEXT,
        project TEXT,
        task    TEXT,
        actor   TEXT,
        data    TEXT NOT NULL DEFAULT '{}'
    );
    CREATE INDEX log_ts ON log(ts);
    CREATE INDEX log_pane ON log(pane, ts);
    CREATE INDEX log_kind ON log(kind, ts);
    CREATE TABLE panes (
        id      TEXT PRIMARY KEY,
        meta    TEXT NOT NULL,
        token   TEXT NOT NULL,
        pid     INTEGER,
        created INTEGER NOT NULL,
        closed  INTEGER
    );
    CREATE TABLE kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
    "#,
    // 2: projects and approvals
    r#"
    CREATE TABLE projects (
        name    TEXT PRIMARY KEY,
        path    TEXT NOT NULL,
        added   INTEGER NOT NULL,
        open    INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE approvals (
        id        TEXT PRIMARY KEY,
        ts        INTEGER NOT NULL,
        pane      TEXT NOT NULL,
        project   TEXT,
        kind      TEXT NOT NULL,
        summary   TEXT NOT NULL,
        detail    TEXT NOT NULL DEFAULT '{}',
        status    TEXT NOT NULL DEFAULT 'pending',
        answered_by TEXT,
        answered  INTEGER,
        reason    TEXT
    );
    CREATE INDEX approvals_status ON approvals(status, ts);
    "#,
    // 3: fleet: queued stops and the notification inbox
    r#"
    CREATE TABLE stops (
        id        TEXT PRIMARY KEY,
        station   TEXT NOT NULL,
        scope     TEXT NOT NULL,
        issued    INTEGER NOT NULL,
        issued_by TEXT,
        status    TEXT NOT NULL,
        updated   INTEGER NOT NULL,
        result    TEXT NOT NULL DEFAULT '{}'
    );
    CREATE INDEX stops_status ON stops(status);
    CREATE TABLE notifications (
        id      TEXT PRIMARY KEY,
        ts      INTEGER NOT NULL,
        kind    TEXT NOT NULL,
        pane    TEXT,
        project TEXT,
        title   TEXT NOT NULL,
        body    TEXT NOT NULL DEFAULT '',
        url     TEXT,
        read    INTEGER NOT NULL DEFAULT 0,
        pushed  INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX notifications_ts ON notifications(ts);
    "#,
    // 4: messages, relay, tasks
    r#"
    CREATE TABLE messages (
        id        TEXT PRIMARY KEY,
        thread    TEXT NOT NULL,
        seq       INTEGER NOT NULL,
        ts        INTEGER NOT NULL,
        sender    TEXT NOT NULL,
        recipient TEXT NOT NULL,
        kind      TEXT NOT NULL,
        body      TEXT NOT NULL,
        refs      TEXT NOT NULL DEFAULT '[]',
        task      TEXT,
        project   TEXT,
        status    TEXT NOT NULL,
        received  INTEGER,
        read      INTEGER,
        acted     INTEGER,
        announced INTEGER NOT NULL DEFAULT 0,
        auth      TEXT
    );
    CREATE INDEX messages_recipient ON messages(recipient, status);
    CREATE INDEX messages_thread ON messages(thread, seq);
    CREATE TABLE outbox (
        msg       TEXT PRIMARY KEY,
        dest      TEXT NOT NULL,
        ord       INTEGER NOT NULL,
        attempts  INTEGER NOT NULL DEFAULT 0,
        next_try  INTEGER NOT NULL DEFAULT 0,
        state     TEXT NOT NULL DEFAULT 'pending'
    );
    CREATE INDEX outbox_dest ON outbox(dest, state, ord);
    CREATE TABLE receipts (msg TEXT PRIMARY KEY, ts INTEGER NOT NULL);
    CREATE TABLE tasks (
        id         TEXT PRIMARY KEY,
        created    INTEGER NOT NULL,
        updated    INTEGER NOT NULL,
        state      TEXT NOT NULL,
        goal       TEXT NOT NULL,
        acceptance TEXT NOT NULL DEFAULT '',
        role       TEXT,
        agent      TEXT,
        station    TEXT NOT NULL,
        project    TEXT,
        pane       TEXT,
        requester  TEXT,
        workspace  TEXT NOT NULL DEFAULT '{}',
        deps       TEXT NOT NULL DEFAULT '[]',
        outputs    TEXT NOT NULL DEFAULT '{}',
        review     TEXT NOT NULL DEFAULT '{}',
        resume     TEXT NOT NULL DEFAULT '{}',
        recipe     TEXT,
        parent     TEXT
    );
    CREATE INDEX tasks_state ON tasks(state, updated);
    CREATE TABLE routes (
        id        TEXT PRIMARY KEY,
        project   TEXT NOT NULL,
        sender    TEXT NOT NULL,
        station   TEXT NOT NULL,
        status    TEXT NOT NULL,
        expires   INTEGER NOT NULL,
        created   INTEGER NOT NULL
    );
    "#,
    // 5: project memory, sign-offs, checkpoints, recipe runs
    r#"
    CREATE TABLE notes (
        id      INTEGER PRIMARY KEY,
        ts      INTEGER NOT NULL,
        project TEXT NOT NULL,
        kind    TEXT NOT NULL,
        author  TEXT,
        task    TEXT,
        text    TEXT NOT NULL
    );
    CREATE INDEX notes_project ON notes(project, ts);
    CREATE VIRTUAL TABLE notes_fts USING fts5(text, content='notes', content_rowid='id');
    CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
        INSERT INTO notes_fts(rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER notes_ad AFTER DELETE ON notes BEGIN
        INSERT INTO notes_fts(notes_fts, rowid, text) VALUES ('delete', old.id, old.text);
    END;
    CREATE TABLE signoffs (
        id      TEXT PRIMARY KEY,
        ts      INTEGER NOT NULL,
        project TEXT NOT NULL,
        task    TEXT,
        commit_sha TEXT NOT NULL,
        by      TEXT NOT NULL,
        note    TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX signoffs_project ON signoffs(project, ts);
    CREATE TABLE checkpoints (
        id      TEXT PRIMARY KEY,
        ts      INTEGER NOT NULL,
        project TEXT,
        task    TEXT,
        pane    TEXT,
        path    TEXT NOT NULL,
        commit_sha TEXT NOT NULL,
        ref_name TEXT NOT NULL
    );
    CREATE TABLE recipes (
        id      TEXT PRIMARY KEY,
        kind    TEXT NOT NULL,
        created INTEGER NOT NULL,
        updated INTEGER NOT NULL,
        project TEXT,
        state   TEXT NOT NULL,
        data    TEXT NOT NULL
    );
    "#,
    // 6: project links. One row per direction a station knows: local
    // `project` is linked with `other` on `station`.
    r#"
    CREATE TABLE project_links (
        project TEXT NOT NULL,
        other   TEXT NOT NULL,
        station TEXT NOT NULL,
        since   INTEGER NOT NULL,
        PRIMARY KEY (project, other, station)
    );
    "#,
];

/// How long a caller of `write_ack` waits to be told its change is saved.
pub const ACK_WAIT: Duration = Duration::from_secs(1);
/// Entries of the ship's log (and captured notes) that may wait. When more
/// arrive the oldest are dropped, and counted.
pub const LOG_QUEUE: usize = 10_000;
/// Unsaved changes of state at which the station says it cannot write.
pub const OVERLAY_LIMIT: usize = 1_024;
/// Unsaved changes of state held at most. Past this a change to something
/// that is not already waiting is dropped, and counted: memory stays bounded
/// on a disk that takes nothing for hours.
pub const OVERLAY_HARD: usize = 4_096;
/// Writes failing for this long: the station says it cannot write.
pub const UNHEALTHY_AFTER: Duration = Duration::from_secs(30);
/// A write that failed is tried again this often.
const RETRY: Duration = Duration::from_secs(1);
/// How long the writer waits for another program's lock, per try.
const WRITER_BUSY: Duration = Duration::from_millis(250);
/// How long a caller that writes itself (`with`, `with_tx`) waits for one.
const CALLER_BUSY: Duration = Duration::from_secs(5);
const SLOW: Duration = Duration::from_millis(50);
const STATE_BATCH: usize = 256;
const LOG_BATCH: usize = 500;

/// One entry of the ship's log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRow {
    pub ulid: String,
    pub ts: u64,
    pub kind: String,
    pub pane: Option<String>,
    pub project: Option<String>,
    pub task: Option<String>,
    pub actor: Option<String>,
    pub data: Value,
}

/// A write nobody waits for and nothing depends on: it may be dropped when
/// the database takes nothing for a long time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Best {
    Log(LogRow),
    /// A note captured from a hook (project memory).
    Note {
        ts: u64,
        project: String,
        kind: String,
        author: Option<String>,
        task: Option<String>,
        text: String,
    },
}

impl Best {
    fn apply(&self, c: &Connection) -> rusqlite::Result<()> {
        match self {
            // OR IGNORE: a batch whose commit was not reported is written again.
            Best::Log(l) => c.execute(
                "INSERT OR IGNORE INTO log(ulid,ts,kind,pane,project,task,actor,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![l.ulid, l.ts as i64, l.kind, l.pane, l.project, l.task, l.actor, l.data.to_string()],
            ),
            Best::Note { ts, project, kind, author, task, text } => c.execute(
                "INSERT INTO notes(ts,project,kind,author,task,text) VALUES(?1,?2,?3,?4,?5,?6)",
                params![*ts as i64, project, kind, author, task, text],
            ),
        }
        .map(|_| ())
    }
}

/// A change of state that is kept until it is written: applied in memory by
/// its caller first, held here (the newest per thing), written ahead of the
/// ship's log and tried again until the database has it. Each says the
/// whole of what it writes, so a newer one replaces an older one for the
/// same thing, and what is left at an upgrade can be handed to the next
/// image as data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Op {
    /// A pane's record (pause, holds and its agent's session included).
    Pane {
        id: String,
        meta: String,
        token: String,
        pid: Option<i64>,
        created: i64,
        closed: Option<i64>,
    },
    /// The pane was closed and taken off the list.
    PaneClosed {
        id: String,
        at: i64,
    },
    /// A listed, ended pane the owner closed: its record no longer lists it.
    PaneUnlisted {
        id: String,
        meta: String,
        at: i64,
    },
    Kv {
        k: String,
        v: String,
    },
    KvDel {
        k: String,
    },
    /// A stop this station issued, for one station (stops.rs).
    Stop {
        id: String,
        station: String,
        scope: String,
        issued: i64,
        by: String,
        status: String,
        updated: i64,
        result: String,
    },
    StopDel {
        id: String,
    },
    ApprovalNew {
        id: String,
        ts: i64,
        pane: String,
        project: Option<String>,
        kind: String,
        summary: String,
        detail: String,
    },
    /// An approval was answered, expired or cancelled. `only_pending`: it
    /// changes nothing once the approval has an answer.
    ApprovalStatus {
        id: String,
        status: String,
        at: i64,
        by: Option<String>,
        reason: Option<String>,
        only_pending: bool,
    },
    /// A notice for the inbox.
    Notice {
        id: String,
        ts: i64,
        kind: String,
        pane: Option<String>,
        project: Option<String>,
        title: String,
        body: String,
        url: Option<String>,
        pushed: i64,
    },
}

impl Op {
    /// What it is a change of. Two changes with one key: the later replaces
    /// the earlier.
    pub fn key(&self) -> String {
        match self {
            Op::Pane { id, .. } => format!("pane:{id}"),
            Op::PaneClosed { id, .. } => format!("pane-closed:{id}"),
            Op::PaneUnlisted { id, .. } => format!("pane-unlisted:{id}"),
            Op::Kv { k, .. } | Op::KvDel { k } => format!("kv:{k}"),
            Op::Stop { id, .. } | Op::StopDel { id } => format!("stop-row:{id}"),
            Op::ApprovalNew { id, .. } => format!("approval:{id}"),
            Op::ApprovalStatus { id, .. } => format!("approval-status:{id}"),
            Op::Notice { id, .. } => format!("notice:{id}"),
        }
    }

    fn apply(&self, c: &Connection) -> rusqlite::Result<()> {
        match self {
            Op::Pane { id, meta, token, pid, created, closed } => c.execute(
                "INSERT INTO panes(id,meta,token,pid,created,closed) VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(id) DO UPDATE SET meta=excluded.meta, pid=excluded.pid, closed=excluded.closed",
                params![id, meta, token, pid, created, closed],
            ),
            Op::PaneClosed { id, at } => c.execute(
                "UPDATE panes SET closed=?2 WHERE id=?1 AND closed IS NULL",
                params![id, at],
            ),
            Op::PaneUnlisted { id, meta, at } => c.execute(
                "UPDATE panes SET meta=?2, closed=COALESCE(closed, ?3) WHERE id=?1",
                params![id, meta, at],
            ),
            Op::Kv { k, v } => c.execute(
                "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                params![k, v],
            ),
            Op::KvDel { k } => c.execute("DELETE FROM kv WHERE k=?1", [k]),
            Op::Stop { id, station, scope, issued, by, status, updated, result } => c.execute(
                "INSERT INTO stops(id,station,scope,issued,issued_by,status,updated,result) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(id) DO UPDATE SET status=excluded.status,
                   updated=excluded.updated, result=excluded.result
                 WHERE stops.status != 'confirmed'",
                params![id, station, scope, issued, by, status, updated, result],
            ),
            Op::StopDel { id } => c.execute("DELETE FROM stops WHERE id=?1", [id]),
            Op::ApprovalNew { id, ts, pane, project, kind, summary, detail } => c.execute(
                "INSERT OR IGNORE INTO approvals(id,ts,pane,project,kind,summary,detail) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![id, ts, pane, project, kind, summary, detail],
            ),
            Op::ApprovalStatus { id, status, at, by, reason, only_pending } => c.execute(
                "UPDATE approvals SET status=?2, answered=?3, answered_by=?4, reason=?5
                 WHERE id=?1 AND (?6=0 OR status='pending')",
                params![id, status, at, by, reason, *only_pending as i64],
            ),
            Op::Notice { id, ts, kind, pane, project, title, body, url, pushed } => c.execute(
                "INSERT OR IGNORE INTO notifications(id,ts,kind,pane,project,title,body,url,pushed) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![id, ts, kind, pane, project, title, body, url, pushed],
            ),
        }
        .map(|_| ())
    }
}

/// What was not written when the writer was stopped for an upgrade. The
/// next image writes it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Unwritten {
    #[serde(default)]
    pub state: Vec<Op>,
    #[serde(default)]
    pub log: Vec<Best>,
}

impl Unwritten {
    pub fn is_empty(&self) -> bool {
        self.state.is_empty() && self.log.is_empty()
    }
}

/// The writer's figures, for the Performance page.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DbStats {
    /// Off: writes are made by their callers, as before the writer.
    pub queued: bool,
    /// Changes of state not yet written.
    pub waiting: usize,
    /// Entries of the ship's log not yet written.
    pub log_waiting: usize,
    /// How long the oldest unwritten change of state has waited.
    pub oldest_ms: u64,
    /// Log entries dropped because 10,000 were waiting. Since start.
    pub log_dropped: u64,
    /// Changes of state dropped: the database refused them for good, or
    /// 4,096 were waiting. Since start.
    pub state_dropped: u64,
    /// Tries that failed. Since start.
    pub failures: u64,
    /// Batches that took over 50 ms. Since start.
    pub slow: u64,
    /// For how long writes have been failing, if they are.
    pub failing_ms: Option<u64>,
    pub last_error: Option<String>,
    pub healthy: bool,
}

struct Slot {
    op: Op,
    /// Since when this thing has had an unwritten change.
    since: Instant,
}

#[derive(Default)]
struct Queue {
    /// Unwritten changes of state, in the order they are to be written.
    slots: BTreeMap<u64, Slot>,
    index: HashMap<String, u64>,
    next: u64,
    best: VecDeque<Best>,
    /// How many entries were ever taken off the front of `best`.
    best_head: u64,
    in_flight: bool,
    paused: bool,
    quit: bool,
    retry_at: Option<Instant>,
    failing_since: Option<Instant>,
    last_error: Option<String>,
    log_dropped: u64,
    state_dropped: u64,
    failures: u64,
    slow: u64,
}

impl Queue {
    fn idle(&self) -> bool {
        self.slots.is_empty() && self.best.is_empty() && !self.in_flight
    }

    fn put(&mut self, op: Op) {
        let key = op.key();
        let mut since = Instant::now();
        if let Some(seq) = self.index.get(&key).copied() {
            // An answer is not replaced by an expiry that came after it.
            if matches!(
                &op,
                Op::ApprovalStatus {
                    only_pending: true,
                    ..
                }
            ) {
                return;
            }
            if let Some(old) = self.slots.remove(&seq) {
                since = old.since;
            }
        } else if self.slots.len() >= OVERLAY_HARD {
            self.state_dropped += 1;
            tracing::error!(
                key,
                "a change of state was dropped: too many are waiting for the database"
            );
            return;
        }
        self.next += 1;
        self.index.insert(key, self.next);
        self.slots.insert(self.next, Slot { op, since });
    }
}

struct Shared {
    q: Mutex<Queue>,
    /// For the writer: there is work, or it is time to try again.
    wake: Condvar,
    /// For those who wait for a write.
    done: Condvar,
    queued: AtomicBool,
    /// Tests: every try of the writer fails as if the database were locked.
    fail: AtomicBool,
}

pub struct Db {
    /// The one connection that writes. The writer thread and the callers
    /// that write themselves take turns at it.
    write: Arc<Mutex<Connection>>,
    /// A connection that only reads, so a read never stands behind a write
    /// that waits for a lock. None for a database in memory, which is one
    /// connection.
    read: Option<Mutex<Connection>>,
    shared: Arc<Shared>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// Run something that may wait without holding up the runtime thread it
/// was called on: the thread's other work is handed to another.
fn off_runtime<R>(f: impl FnOnce() -> R) -> R {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(h) if h.runtime_flavor() == RuntimeFlavor::MultiThread => tokio::task::block_in_place(f),
        _ => f(),
    }
}

fn locked() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some("database is locked".into()),
    )
}

/// A failure that trying again cannot mend: the write itself is wrong.
/// Everything else (locked, busy, disk full, I/O) is tried again.
fn hopeless(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(f, _) => matches!(
            f.code,
            rusqlite::ErrorCode::ConstraintViolation
                | rusqlite::ErrorCode::TypeMismatch
                | rusqlite::ErrorCode::TooBig
                | rusqlite::ErrorCode::ParameterOutOfRange
        ),
        _ => true,
    }
}

/// One batch in one transaction. A change the database refuses for good is
/// left out and counted; any other failure fails the whole batch, which is
/// then tried again. Returns how many were refused for good.
fn write_batch(
    shared: &Shared,
    conn: &Mutex<Connection>,
    state: &[(u64, Op)],
    best: &[Best],
) -> rusqlite::Result<u64> {
    if shared.fail.load(Ordering::Relaxed) {
        return Err(locked());
    }
    let mut conn = conn.lock().unwrap();
    conn.busy_timeout(WRITER_BUSY)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut refused = 0;
    let results = state
        .iter()
        .map(|(_, op)| (op.key(), op.apply(&tx)))
        .chain(best.iter().map(|b| (String::from("log"), b.apply(&tx))));
    for (key, r) in results {
        match r {
            Ok(()) => {}
            Err(e) if hopeless(&e) => {
                refused += 1;
                tracing::error!(error = %e, key, "the database refused a write for good; it was dropped");
            }
            Err(e) => return Err(e),
        }
    }
    tx.commit()?;
    Ok(refused)
}

fn run(shared: Arc<Shared>, conn: Arc<Mutex<Connection>>) {
    loop {
        let (state, best, best_end) = {
            let mut q = shared.q.lock().unwrap();
            loop {
                let work = !q.slots.is_empty() || !q.best.is_empty();
                let now = Instant::now();
                let due = q.retry_at.is_none_or(|t| now >= t);
                if work && due && !q.paused {
                    break;
                }
                if q.quit {
                    return;
                }
                q = match q.retry_at {
                    Some(t) if work && !q.paused => {
                        shared
                            .wake
                            .wait_timeout(q, t.saturating_duration_since(now))
                            .unwrap()
                            .0
                    }
                    _ => shared.wake.wait(q).unwrap(),
                };
            }
            q.in_flight = true;
            // State first: the ship's log is written when no state waits.
            let state: Vec<(u64, Op)> = q
                .slots
                .iter()
                .take(STATE_BATCH)
                .map(|(seq, s)| (*seq, s.op.clone()))
                .collect();
            let best: Vec<Best> = if state.is_empty() {
                q.best.iter().take(LOG_BATCH).cloned().collect()
            } else {
                vec![]
            };
            let end = q.best_head + best.len() as u64;
            (state, best, end)
        };
        let began = Instant::now();
        let result = write_batch(&shared, &conn, &state, &best);
        let took = began.elapsed();
        let mut q = shared.q.lock().unwrap();
        q.in_flight = false;
        match result {
            Ok(refused) => {
                for (seq, op) in &state {
                    // One that was changed again meanwhile has a new place
                    // and stays.
                    if q.slots.remove(seq).is_some() {
                        q.index.remove(&op.key());
                    }
                }
                // What was dropped from the front meanwhile is not taken twice.
                while q.best_head < best_end && !q.best.is_empty() {
                    q.best.pop_front();
                    q.best_head += 1;
                }
                q.best_head = q.best_head.max(best_end);
                q.state_dropped += refused;
                q.failing_since = None;
                q.last_error = None;
                q.retry_at = None;
                if took > SLOW {
                    q.slow += 1;
                }
            }
            Err(e) => {
                q.failures += 1;
                if q.failing_since.is_none() {
                    tracing::warn!(error = %e, "the database could not be written; trying again every second");
                    q.failing_since = Some(began);
                }
                q.last_error = Some(e.to_string());
                q.retry_at = Some(Instant::now() + RETRY);
            }
        }
        drop(q);
        shared.done.notify_all();
    }
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Db> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?, Some(path))
    }

    pub fn memory() -> anyhow::Result<Db> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(mut conn: Connection, file: Option<&Path>) -> anyhow::Result<Db> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(CALLER_BUSY)?;
        Self::migrate(&mut conn)?;
        let read = match file {
            Some(path) => {
                let read = Connection::open_with_flags(
                    path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                read.busy_timeout(CALLER_BUSY)?;
                Some(Mutex::new(read))
            }
            None => None,
        };
        let write = Arc::new(Mutex::new(conn));
        let shared = Arc::new(Shared {
            q: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            done: Condvar::new(),
            queued: AtomicBool::new(true),
            fail: AtomicBool::new(false),
        });
        let thread = {
            let (shared, write) = (shared.clone(), write.clone());
            std::thread::Builder::new()
                .name("db-writer".into())
                .spawn(move || run(shared, write))?
        };
        Ok(Db {
            write,
            read,
            shared,
            thread: Mutex::new(Some(thread)),
        })
    }

    fn migrate(conn: &mut Connection) -> anyhow::Result<()> {
        let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if current as usize > MIGRATIONS.len() {
            anyhow::bail!(
                "database schema {current} is newer than this spandeck ({}); upgrade spandeck",
                MIGRATIONS.len()
            );
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as u32)?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> u32 {
        self.read(|c| c.query_row("PRAGMA user_version", [], |r| r.get(0)))
            .unwrap_or(0)
    }

    pub const LATEST_SCHEMA: u32 = MIGRATIONS.len() as u32;

    /// Read. Never waits for a write: the database file has a connection
    /// that only reads. A write made through it is refused.
    pub fn read<R>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<R>,
    ) -> rusqlite::Result<R> {
        match &self.read {
            Some(read) => f(&read.lock().unwrap()),
            None => f(&self.write.lock().unwrap()),
        }
    }

    /// Run `f` with the connection that writes, on the caller's thread; it
    /// is written when this returns. A write here waits up to five seconds
    /// for another program's lock, so nothing typed, stopped or answered
    /// goes through it (those are `log`, `put` and `write_ack`). Reads
    /// belong in `read`.
    pub fn with<R>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<R>,
    ) -> rusqlite::Result<R> {
        let conn = self.write.lock().unwrap();
        conn.busy_timeout(CALLER_BUSY)?;
        f(&conn)
    }

    pub fn with_tx<R>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<R>,
    ) -> rusqlite::Result<R> {
        let mut conn = self.write.lock().unwrap();
        conn.busy_timeout(CALLER_BUSY)?;
        // The write lock is taken at the start. A transaction that reads
        // first and writes after is refused at once when another program
        // holds the lock (SQLite does not wait on that path), which is how
        // a message was refused in a moment where a single write waited.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let r = f(&tx)?;
        tx.commit()?;
        Ok(r)
    }

    /// For what must be stored before its caller is answered (a message
    /// sent, a message accepted from another station): the caller's own
    /// transaction, as `with_tx`, and the answer still comes only after it
    /// is committed. While it waits for a lock, the runtime thread it was
    /// called on is given up to other work, so one locked database does
    /// not park the threads everything else is served by.
    pub fn with_tx_stored<R>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<R>,
    ) -> rusqlite::Result<R> {
        off_runtime(|| self.with_tx(f))
    }

    // ------------------------------------------------------------ the writer

    /// The switch `db_writer`. Off: the ship's log and changes of state are
    /// written by their callers, as before the writer; one that fails is
    /// still kept and tried again.
    pub fn set_queued(&self, on: bool) {
        self.shared.queued.store(on, Ordering::Relaxed);
    }

    /// Tests: make every try of the writer fail, as a locked database does.
    #[doc(hidden)]
    pub fn fail_writes(&self, on: bool) {
        self.shared.fail.store(on, Ordering::Relaxed);
    }

    fn inline(&self) -> bool {
        !self.shared.queued.load(Ordering::Relaxed) && !self.shared.fail.load(Ordering::Relaxed)
    }

    /// A change of state: kept, written by the writer ahead of the ship's
    /// log, tried again until it is written. Never waits. The caller has
    /// already made the change in memory.
    pub fn put(&self, op: Op) {
        self.put_all(vec![op]);
    }

    fn put_all(&self, ops: Vec<Op>) {
        let mut q = self.shared.q.lock().unwrap();
        if self.inline() && q.slots.is_empty() && !q.in_flight {
            drop(q);
            let wrote = self.with_tx(|tx| ops.iter().try_for_each(|op| op.apply(tx)));
            match wrote {
                Ok(()) => return,
                Err(e) => {
                    tracing::warn!(error = %e, "a change of state was not written; it is kept and tried again")
                }
            }
            q = self.shared.q.lock().unwrap();
        }
        for op in ops {
            q.put(op);
        }
        drop(q);
        self.shared.wake.notify_one();
    }

    /// A change of state the caller reports on: true when it is written,
    /// false when it was not within `ACK_WAIT` (it is kept and tried again;
    /// say "not yet saved").
    pub fn write_ack(&self, op: Op) -> bool {
        self.write_ack_within(vec![op], ACK_WAIT)
    }

    /// Several at once; true when all of them are written.
    pub fn write_ack_within(&self, ops: Vec<Op>, wait: Duration) -> bool {
        let keys: Vec<String> = ops.iter().map(|op| op.key()).collect();
        self.put_all(ops);
        self.saved_within(&keys, wait)
    }

    /// Wait until no change of these things (each an `Op::key`) is
    /// unwritten, for at most `wait`. True when none is.
    pub fn saved_within(&self, keys: &[String], wait: Duration) -> bool {
        let unwritten = |q: &Queue| keys.iter().any(|k| q.index.contains_key(k));
        {
            let mut q = self.shared.q.lock().unwrap();
            if !unwritten(&q) {
                return true;
            }
            // Someone waits to be told: a write that failed is tried now,
            // not at its next second.
            if q.retry_at.take().is_some() {
                self.shared.wake.notify_one();
            }
        }
        let deadline = Instant::now() + wait;
        off_runtime(|| {
            let mut q = self.shared.q.lock().unwrap();
            loop {
                if !unwritten(&q) {
                    return true;
                }
                let now = Instant::now();
                if now >= deadline {
                    return false;
                }
                q = self.shared.done.wait_timeout(q, deadline - now).unwrap().0;
            }
        })
    }

    /// Changes of state not yet written, oldest first: what lists lay over
    /// what they read, so that nothing the owner did goes backwards.
    pub fn pending(&self) -> Vec<Op> {
        let q = self.shared.q.lock().unwrap();
        q.slots.values().map(|s| s.op.clone()).collect()
    }

    /// Is a change of this thing (an `Op::key`) still unwritten?
    pub fn unsaved(&self, key: &str) -> bool {
        self.shared.q.lock().unwrap().index.contains_key(key)
    }

    /// Can this station write its database? False when writes have failed
    /// for 30 seconds, or 1,024 changes of state wait.
    pub fn healthy(&self) -> bool {
        self.healthy_at(Instant::now())
    }

    pub fn healthy_at(&self, now: Instant) -> bool {
        let q = self.shared.q.lock().unwrap();
        q.slots.len() < OVERLAY_LIMIT
            && q.failing_since
                .is_none_or(|t| now.saturating_duration_since(t) < UNHEALTHY_AFTER)
    }

    pub fn stats(&self) -> DbStats {
        let now = Instant::now();
        let healthy = self.healthy_at(now);
        let q = self.shared.q.lock().unwrap();
        DbStats {
            queued: self.shared.queued.load(Ordering::Relaxed),
            waiting: q.slots.len(),
            log_waiting: q.best.len(),
            oldest_ms: q
                .slots
                .values()
                .map(|s| now.saturating_duration_since(s.since).as_millis() as u64)
                .max()
                .unwrap_or(0),
            log_dropped: q.log_dropped,
            state_dropped: q.state_dropped,
            failures: q.failures,
            slow: q.slow,
            failing_ms: q
                .failing_since
                .map(|t| now.saturating_duration_since(t).as_millis() as u64),
            last_error: q.last_error.clone(),
            healthy,
        }
    }

    /// Try now, and wait until everything is written or `wait` has passed.
    /// True when nothing is left.
    pub fn flush(&self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        let mut q = self.shared.q.lock().unwrap();
        q.retry_at = None;
        self.shared.wake.notify_one();
        loop {
            if q.idle() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            // A try that failed meanwhile is made again while someone
            // waits here, ten times a second.
            let step = (deadline - now).min(Duration::from_millis(100));
            q = self.shared.done.wait_timeout(q, step).unwrap().0;
            if q.retry_at.is_some() && !q.in_flight {
                q.retry_at = None;
                self.shared.wake.notify_one();
            }
        }
    }

    /// Before this process becomes another image, or ends: write what
    /// waits, for at most `wait`, then stop the writer and give back what
    /// is still unwritten. Nothing is written after this returns until
    /// `resume`.
    pub fn drain(&self, wait: Duration) -> Unwritten {
        self.flush(wait);
        let mut q = self.shared.q.lock().unwrap();
        q.paused = true;
        while q.in_flight {
            q = self.shared.done.wait(q).unwrap();
        }
        Unwritten {
            state: q.slots.values().map(|s| s.op.clone()).collect(),
            log: q.best.iter().cloned().collect(),
        }
    }

    /// The image was not replaced after all: write again.
    pub fn resume(&self) {
        self.shared.q.lock().unwrap().paused = false;
        self.shared.wake.notify_one();
    }

    /// What the image before this one could not write. Taken first, so
    /// anything newer replaces it.
    pub fn restore(&self, left: Unwritten) {
        let mut q = self.shared.q.lock().unwrap();
        for op in left.state {
            q.put(op);
        }
        for b in left.log.into_iter().take(LOG_QUEUE) {
            q.best.push_back(b);
        }
        drop(q);
        self.shared.wake.notify_one();
    }

    fn best(&self, b: Best) {
        let mut q = self.shared.q.lock().unwrap();
        if self.inline() && q.idle() {
            drop(q);
            if let Err(e) = self.with(|c| b.apply(c)) {
                tracing::warn!(error = %e, "ship's log write failed");
            }
            return;
        }
        if q.best.len() >= LOG_QUEUE {
            // The oldest goes. One being written now is passed over when
            // its batch is done.
            q.best.pop_front();
            q.best_head += 1;
            q.log_dropped += 1;
        }
        q.best.push_back(b);
        drop(q);
        self.shared.wake.notify_one();
    }

    // ------------------------------------------------------------ key/value

    /// Reads what `kv_put` has not written yet before the database.
    pub fn kv_get(&self, key: &str) -> Option<String> {
        {
            let q = self.shared.q.lock().unwrap();
            if let Some(slot) = q
                .index
                .get(&format!("kv:{key}"))
                .and_then(|s| q.slots.get(s))
            {
                return match &slot.op {
                    Op::Kv { v, .. } => Some(v.clone()),
                    _ => None,
                };
            }
        }
        self.read(|c| {
            c.query_row("SELECT v FROM kv WHERE k=?1", [key], |r| r.get(0))
                .optional()
        })
        .ok()
        .flatten()
    }

    /// Written when this returns (or not at all: the error is dropped, as
    /// it always was). Waits for a lock like any `with`.
    pub fn kv_set(&self, key: &str, value: &str) {
        if self.unsaved(&format!("kv:{key}")) {
            // An older value still waits: this one goes after it.
            return self.kv_put(key, value);
        }
        let _ = self.with(|c| {
            c.execute(
                "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                params![key, value],
            )
        });
    }

    /// Never waits: `kv_get` gives the value at once, the writer writes it
    /// and tries again until it has.
    pub fn kv_put(&self, key: &str, value: &str) {
        self.put(Op::Kv {
            k: key.to_string(),
            v: value.to_string(),
        });
    }

    // ------------------------------------------------------------ ship's log

    /// Append to the ship's log. Never waits, and is best effort: when
    /// 10,000 entries wait for a database that takes none, the oldest are
    /// dropped and counted.
    #[allow(clippy::too_many_arguments)]
    pub fn log(
        &self,
        ulid: &str,
        ts: u64,
        kind: &str,
        pane: Option<&str>,
        project: Option<&str>,
        task: Option<&str>,
        actor: Option<&str>,
        data: &Value,
    ) {
        self.best(Best::Log(LogRow {
            ulid: ulid.to_string(),
            ts,
            kind: kind.to_string(),
            pane: pane.map(String::from),
            project: project.map(String::from),
            task: task.map(String::from),
            actor: actor.map(String::from),
            data: data.clone(),
        }));
    }

    /// A note captured from a hook: best effort, like the ship's log.
    pub fn note_later(
        &self,
        ts: u64,
        project: &str,
        kind: &str,
        author: Option<&str>,
        task: Option<&str>,
        text: &str,
    ) {
        self.best(Best::Note {
            ts,
            project: project.to_string(),
            kind: kind.to_string(),
            author: author.map(String::from),
            task: task.map(String::from),
            text: text.to_string(),
        });
    }

    /// Read the ship's log, newest first: what is written, with what still
    /// waits to be written over it.
    pub fn log_query(
        &self,
        pane: Option<&str>,
        kind_prefix: Option<&str>,
        since_ms: u64,
        limit: u32,
    ) -> Vec<Value> {
        // What waits is looked at first: an entry written between the two
        // looks is then seen twice and dropped once, never missed.
        let mut out: Vec<Value> = {
            let q = self.shared.q.lock().unwrap();
            q.best
                .iter()
                .rev()
                .filter_map(|b| match b {
                    Best::Log(l) => Some(l),
                    _ => None,
                })
                .filter(|l| {
                    pane.is_none_or(|p| l.pane.as_deref() == Some(p))
                        && kind_prefix.is_none_or(|k| l.kind.starts_with(k))
                        && l.ts >= since_ms
                })
                .take(limit as usize)
                .map(|l| {
                    serde_json::json!({
                        "id": l.ulid, "ts": l.ts, "kind": l.kind, "pane": l.pane,
                        "project": l.project, "task": l.task, "actor": l.actor, "data": l.data,
                    })
                })
                .collect()
        };
        let written: Vec<Value> = self
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT ulid,ts,kind,pane,project,task,actor,data FROM log
                     WHERE (?1 IS NULL OR pane=?1) AND (?2 IS NULL OR kind LIKE ?2 || '%') AND ts>=?3
                     ORDER BY id DESC LIMIT ?4",
                )?;
                let rows = stmt.query_map(params![pane, kind_prefix, since_ms as i64, limit], |r| {
                    let data: String = r.get(7)?;
                    Ok(serde_json::json!({
                        "id": r.get::<_, String>(0)?,
                        "ts": r.get::<_, i64>(1)?,
                        "kind": r.get::<_, String>(2)?,
                        "pane": r.get::<_, Option<String>>(3)?,
                        "project": r.get::<_, Option<String>>(4)?,
                        "task": r.get::<_, Option<String>>(5)?,
                        "actor": r.get::<_, Option<String>>(6)?,
                        "data": serde_json::from_str::<Value>(&data).unwrap_or(Value::Null),
                    }))
                })?;
                rows.collect()
            })
            .unwrap_or_default();
        if out.is_empty() {
            return written;
        }
        for w in written {
            if !out.iter().any(|o| o["id"] == w["id"]) {
                out.push(w);
            }
        }
        out.truncate(limit as usize);
        out
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        // What waits gets one more try; the thread then ends.
        self.shared.q.lock().unwrap().quit = true;
        self.shared.wake.notify_one();
        if let Some(t) = self.thread.lock().unwrap().take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: Duration = Duration::from_secs(20);

    #[test]
    fn migrations_apply_and_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let db = Db::open(&path).unwrap();
        assert_eq!(db.schema_version(), Db::LATEST_SCHEMA);
        db.kv_set("a", "1");
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(db.schema_version(), Db::LATEST_SCHEMA);
        assert_eq!(db.kv_get("a").as_deref(), Some("1"));
    }

    #[test]
    fn a_newer_database_is_refused_not_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        drop(Db::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 999u32).unwrap();
        drop(conn);
        assert!(Db::open(&path).is_err());
    }

    fn pane(id: &str, meta: &str) -> Op {
        Op::Pane {
            id: id.into(),
            meta: meta.into(),
            token: "t".into(),
            pid: None,
            created: 1,
            closed: None,
        }
    }

    fn log(db: &Db, n: usize) {
        db.log(
            &format!("u{n:06}"),
            n as u64,
            "test.entry",
            None,
            None,
            None,
            None,
            &serde_json::json!({ "n": n }),
        );
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.read(|c| c.query_row(sql, [], |r| r.get(0))).unwrap()
    }

    /// FAILURES §9: a locked database stalled a keystroke for 10 s, because
    /// its entry in the ship's log was written by the caller. The log is
    /// now best effort and bounded: with a database that takes nothing,
    /// logging returns at once, the oldest entries go first and are
    /// counted, a read still shows what waits, and it is all written when
    /// the database is back.
    #[test]
    fn the_ships_log_is_never_waited_for_and_drops_its_oldest_when_full() {
        let db = Db::memory().unwrap();
        db.fail_writes(true);
        for n in 0..LOG_QUEUE + 3 {
            log(&db, n);
        }
        let stats = db.stats();
        assert_eq!(stats.log_waiting, LOG_QUEUE);
        assert_eq!(stats.log_dropped, 3);
        // What waits is read, newest first; the three oldest are gone.
        let newest = db.log_query(None, Some("test."), 0, 2);
        assert_eq!(newest[0]["id"], format!("u{:06}", LOG_QUEUE + 2));
        assert_eq!(newest[1]["id"], format!("u{:06}", LOG_QUEUE + 1));
        db.fail_writes(false);
        assert!(db.flush(LONG));
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM log"),
            LOG_QUEUE as i64,
            "everything that was kept is written, once"
        );
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM log WHERE ulid < 'u000003'"),
            0
        );
        // And is read once, from the database.
        assert_eq!(db.log_query(None, Some("test."), 0, 5).len(), 5);
    }

    /// FAILURES §9 and gap 12: a pane started while the database was
    /// locked never had its record written. A pane's record is now kept
    /// until it is written: the newest state of each pane, in the order
    /// the changes were made, with a close after it never undone by it.
    #[test]
    fn a_panes_record_is_kept_until_written_and_the_newest_wins() {
        let db = Db::memory().unwrap();
        db.fail_writes(true);
        db.put(pane("p1", "first"));
        db.put(pane("p2", "other"));
        db.put(pane("p1", "paused"));
        assert!(!db.write_ack_within(
            vec![Op::PaneClosed {
                id: "p2".into(),
                at: 7
            }],
            Duration::ZERO
        ));
        assert_eq!(db.stats().waiting, 3, "one per thing, not one per change");
        assert!(db.unsaved("pane:p1"));
        assert_eq!(count(&db, "SELECT COUNT(*) FROM panes"), 0);
        db.fail_writes(false);
        assert!(db.flush(LONG));
        let rows: Vec<(String, String, Option<i64>)> = db
            .read(|c| {
                let mut s = c.prepare("SELECT id, meta, closed FROM panes ORDER BY id")?;
                let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect()
            })
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("p1".into(), "paused".into(), None),
                ("p2".into(), "other".into(), Some(7)),
            ]
        );
        assert!(!db.unsaved("pane:p1"));
        assert!(db.write_ack_within(vec![pane("p3", "x")], LONG));
    }

    /// REVIEW2, "Database writer": a read on a second connection showed an
    /// older state than the change just made. What is not yet written is
    /// read back over the database: a value put is the value got, an
    /// answer to an approval is not replaced by the expiry that follows
    /// it, and all of it reaches the database in that state.
    #[test]
    fn what_was_just_changed_never_reads_older_and_an_answer_outlives_its_expiry() {
        let db = Db::memory().unwrap();
        db.kv_set("mark", "1");
        let answer = |status: &str, only_pending: bool| Op::ApprovalStatus {
            id: "a1".into(),
            status: status.into(),
            at: 5,
            by: Some("you".into()),
            reason: None,
            only_pending,
        };
        db.fail_writes(true);
        db.put(Op::ApprovalNew {
            id: "a1".into(),
            ts: 1,
            pane: "p1".into(),
            project: None,
            kind: "tool".into(),
            summary: "rm".into(),
            detail: "{}".into(),
        });
        db.kv_put("mark", "2");
        // A value that waits is not overtaken by one written directly.
        db.kv_set("mark", "3");
        assert_eq!(db.kv_get("mark").as_deref(), Some("3"));
        db.put(answer("allowed", false));
        db.put(answer("expired", true));
        assert!(db.pending().contains(&answer("allowed", false)));
        db.put(Op::KvDel { k: "gone".into() });
        assert_eq!(db.kv_get("gone"), None);
        db.fail_writes(false);
        assert!(db.flush(LONG));
        assert!(db.pending().is_empty());
        assert_eq!(db.kv_get("mark").as_deref(), Some("3"));
        let status: (String, Option<String>) = db
            .read(|c| {
                c.query_row(
                    "SELECT status, answered_by FROM approvals WHERE id='a1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
            })
            .unwrap();
        assert_eq!(status, ("allowed".into(), Some("you".into())));
    }

    /// REVIEW2: "a full disk grows memory for ever with no notice". The
    /// station says it cannot write after 30 seconds of failures or with
    /// 1,024 changes waiting, holds 4,096 at most, and says it can write
    /// again once a write succeeds.
    #[test]
    fn a_database_that_takes_nothing_is_said_to_be_so_and_memory_is_bounded() {
        let db = Db::memory().unwrap();
        assert!(db.healthy());
        db.fail_writes(true);
        assert!(!db.write_ack_within(vec![pane("p0", "m")], Duration::from_millis(50)));
        let stats = db.stats();
        assert!(stats.failures >= 1 && stats.failing_ms.is_some());
        assert_eq!(stats.last_error.as_deref(), Some("database is locked"));
        let now = Instant::now();
        assert!(db.healthy_at(now), "a short lock is not a notice");
        assert!(!db.healthy_at(now + UNHEALTHY_AFTER + Duration::from_secs(1)));
        for n in 1..OVERLAY_LIMIT {
            db.put(pane(&format!("p{n}"), "m"));
        }
        assert!(!db.healthy(), "1,024 changes wait");
        for n in OVERLAY_LIMIT..OVERLAY_HARD + 10 {
            db.put(pane(&format!("p{n}"), "m"));
        }
        // A change to something already waiting is still taken.
        db.put(pane("p0", "newer"));
        let stats = db.stats();
        assert_eq!(stats.waiting, OVERLAY_HARD);
        assert_eq!(stats.state_dropped, 10);
        db.fail_writes(false);
        assert!(db.flush(LONG));
        assert!(db.healthy_at(Instant::now() + UNHEALTHY_AFTER * 2));
        assert_eq!(db.stats().failing_ms, None);
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM panes WHERE meta='newer'"),
            1
        );
    }

    /// REVIEW2: "nothing says the queue is flushed or handed over before
    /// an in-place upgrade". Draining writes what it can, then stops the
    /// writer and gives what is left as data; the next image (here a
    /// second connection to the same file) writes it, and a newer change
    /// made there wins over the one handed over.
    #[test]
    fn what_is_unwritten_at_an_upgrade_is_handed_over_and_written_by_the_next_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let old = Db::open(&path).unwrap();
        old.put(pane("p1", "written"));
        assert!(
            old.drain(LONG).is_empty(),
            "a database that answers is drained"
        );
        old.resume();
        old.fail_writes(true);
        old.put(pane("p1", "paused"));
        old.put(pane("p2", "old"));
        old.kv_put("stop:x1", "{}");
        log(&old, 1);
        let left = old.drain(Duration::from_millis(50));
        assert_eq!(left.state.len(), 3);
        assert_eq!(left.log.len(), 1);
        // As it travels in the handoff file.
        let left: Unwritten = serde_json::from_str(&serde_json::to_string(&left).unwrap()).unwrap();
        // Stopped: nothing is written behind the next image's back.
        old.fail_writes(false);
        assert!(!old.flush(Duration::from_millis(50)));
        drop(old);

        let new = Db::open(&path).unwrap();
        new.restore(left);
        new.put(pane("p2", "new"));
        assert!(new.flush(LONG));
        let metas: Vec<String> = new
            .read(|c| {
                let mut s = c.prepare("SELECT meta FROM panes ORDER BY id")?;
                let rows = s.query_map([], |r| r.get(0))?;
                rows.collect()
            })
            .unwrap();
        assert_eq!(metas, vec!["paused", "new"]);
        assert_eq!(new.kv_get("stop:x1").as_deref(), Some("{}"));
        assert_eq!(new.log_query(None, Some("test."), 0, 9).len(), 1);
    }

    /// The switch `db_writer` off: a change is written by its caller, as
    /// before; one that cannot be written is still kept and tried again.
    #[test]
    fn with_the_switch_off_writes_are_made_by_the_caller() {
        let db = Db::memory().unwrap();
        db.set_queued(false);
        db.put(pane("p1", "m"));
        log(&db, 1);
        // Read with no flush: both were written before the calls returned.
        assert_eq!(count(&db, "SELECT COUNT(*) FROM panes"), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM log"), 1);
        assert_eq!(db.stats().waiting, 0);
    }
}
