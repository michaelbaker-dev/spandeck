//! SQLite state: the ship's log, pane records, messages, tasks, memory.
//!
//! Schema changes are append-only migrations keyed by `PRAGMA user_version`.
//! Never edit a shipped migration; add a new one.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
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
];

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Db> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn memory() -> anyhow::Result<Db> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Db> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let db = Db { conn: Mutex::new(conn) };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
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
        self.with(|c| c.query_row("PRAGMA user_version", [], |r| r.get(0))).unwrap_or(0)
    }

    pub const LATEST_SCHEMA: u32 = MIGRATIONS.len() as u32;

    /// Run `f` with the connection. Keep it short: one lock for the daemon.
    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<R>) -> rusqlite::Result<R> {
        f(&self.conn.lock().unwrap())
    }

    pub fn with_tx<R>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<R>,
    ) -> rusqlite::Result<R> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let r = f(&tx)?;
        tx.commit()?;
        Ok(r)
    }

    pub fn kv_get(&self, key: &str) -> Option<String> {
        self.with(|c| c.query_row("SELECT v FROM kv WHERE k=?1", [key], |r| r.get(0)).optional())
            .ok()
            .flatten()
    }

    pub fn kv_set(&self, key: &str, value: &str) {
        let _ = self.with(|c| {
            c.execute(
                "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                params![key, value],
            )
        });
    }

    /// Append to the ship's log.
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
        let r = self.with(|c| {
            c.execute(
                "INSERT INTO log(ulid,ts,kind,pane,project,task,actor,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![ulid, ts as i64, kind, pane, project, task, actor, data.to_string()],
            )
        });
        if let Err(e) = r {
            tracing::warn!(error = %e, kind, "ship's log write failed");
        }
    }

    /// Read the ship's log, newest first.
    pub fn log_query(
        &self,
        pane: Option<&str>,
        kind_prefix: Option<&str>,
        since_ms: u64,
        limit: u32,
    ) -> Vec<Value> {
        self.with(|c| {
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
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
