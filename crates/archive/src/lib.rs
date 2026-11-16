//! Session archive: copy every agent session file as it grows, keep the raw
//! lines and a normalised, scrubbed, searchable copy.
//!
//! Raw history stays on this station. Search results come only from the
//! scrubbed copy.

pub mod conversation;
pub mod normalize;
pub mod scrub;
pub mod stores;
pub mod usage;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub use normalize::Entry;
pub use scrub::{find_keys, scrub};

/// Only the first part of a large entry is indexed; the raw line holds it all.
const INDEX_CAP: usize = 4000;
const EXCERPT_CHARS: usize = 320;

pub struct Archive {
    conn: Mutex<Connection>,
    /// Key-shaped values seen in session files since the last `take_found_keys`.
    /// Held in memory only, so the daemon can report them as exposed; never
    /// written anywhere by the archive.
    found: Mutex<Vec<(String, String)>>,
    /// Token usage of the conversations asked about since the daemon started.
    usage: Mutex<std::collections::HashMap<String, usage::Tracked>>,
    /// One copy of a file's new lines at a time: the every-second poll and
    /// a conversation that is being watched both ask for them.
    poll_lock: Mutex<()>,
    /// Per session, the last line that holds something the agent said.
    last_reply: Mutex<std::collections::HashMap<String, u64>>,
    /// Where each conversation's items are (conversation.rs). Memory only.
    convs: conversation::Cache,
    /// OpenCode's database as last seen for each session followed in it,
    /// and when: it is opened again only when it has changed.
    stores_seen: Mutex<std::collections::HashMap<String, (StoreStamp, std::time::Instant)>>,
}

/// A database file and its write-ahead log: length and modified time.
type FileStamp = Option<(u64, std::time::SystemTime)>;
type StoreStamp = (FileStamp, FileStamp);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub agent: String,
    pub path: String,
    pub project: Option<String>,
    pub pane: Option<String>,
    pub lines: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// Search only this session (an agent's own history).
    pub session: Option<String>,
    /// Search this project's sessions.
    pub project: Option<String>,
    /// Search everything on this station (cross-project: opt-in).
    pub all_projects: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub session: String,
    pub agent: String,
    pub project: Option<String>,
    pub line: u64,
    pub ts: Option<String>,
    pub role: String,
    pub kind: String,
    pub commit: Option<String>,
    /// Scrubbed excerpt. Quoted history, never instructions.
    pub excerpt: String,
}

impl Archive {
    pub fn open(path: &Path) -> anyhow::Result<Archive> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn memory() -> anyhow::Result<Archive> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Archive> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY, agent TEXT NOT NULL, path TEXT NOT NULL,
                project TEXT, pane TEXT, offset INTEGER NOT NULL DEFAULT 0,
                lines INTEGER NOT NULL DEFAULT 0, commit_sha TEXT, active INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE IF NOT EXISTS raw (
                session TEXT NOT NULL, line INTEGER NOT NULL, raw TEXT NOT NULL,
                PRIMARY KEY (session, line)
            ) WITHOUT ROWID;
            CREATE TABLE IF NOT EXISTS entries (
                id INTEGER PRIMARY KEY, session TEXT NOT NULL, line INTEGER NOT NULL,
                ts TEXT, role TEXT NOT NULL, kind TEXT NOT NULL, text TEXT NOT NULL,
                tool TEXT, files TEXT NOT NULL DEFAULT '[]', commit_sha TEXT
            );
            CREATE INDEX IF NOT EXISTS entries_session ON entries(session, line);
            CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(text, content='entries', content_rowid='id');
            CREATE TRIGGER IF NOT EXISTS entries_ai AFTER INSERT ON entries BEGIN
                INSERT INTO entries_fts(rowid, text) VALUES (new.id, new.text);
            END;
            "#,
        )?;
        conn.execute_batch(usage::SCHEMA)?;
        Ok(Archive {
            conn: Mutex::new(conn),
            found: Mutex::new(vec![]),
            usage: Mutex::new(Default::default()),
            poll_lock: Mutex::new(()),
            last_reply: Mutex::new(Default::default()),
            convs: Default::default(),
            stores_seen: Mutex::new(Default::default()),
        })
    }

    /// Start (or keep) following a session file.
    pub fn follow(
        &self,
        id: &str,
        agent: &str,
        path: &str,
        project: Option<&str>,
        pane: Option<&str>,
    ) {
        {
            let conn = self.conn.lock().unwrap();
            let _ = conn.execute(
                "INSERT INTO sessions(id,agent,path,project,pane) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET path=excluded.path, active=1,
               project=COALESCE(excluded.project, project), pane=COALESCE(excluded.pane, pane)",
                params![id, agent, path, project, pane],
            );
        }
        self.track_usage(id, agent, path, project, pane);
    }

    /// The commit the project is at; stamped on entries captured from now on.
    pub fn set_commit(&self, id: &str, commit: Option<&str>) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE sessions SET commit_sha=?2 WHERE id=?1",
            params![id, commit],
        );
    }

    pub fn sessions(&self) -> Vec<SessionInfo> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) =
            conn.prepare("SELECT id,agent,path,project,pane,lines FROM sessions ORDER BY rowid")
        else {
            return vec![];
        };
        stmt.query_map([], |r| {
            Ok(SessionInfo {
                id: r.get(0)?,
                agent: r.get(1)?,
                path: r.get(2)?,
                project: r.get(3)?,
                pane: r.get(4)?,
                lines: r.get::<_, i64>(5)? as u64,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    /// One followed session, as the archive has it.
    pub fn session(&self, id: &str) -> Option<SessionInfo> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id,agent,path,project,pane,lines FROM sessions WHERE id=?1",
            [id],
            |r| {
                Ok(SessionInfo {
                    id: r.get(0)?,
                    agent: r.get(1)?,
                    path: r.get(2)?,
                    project: r.get(3)?,
                    pane: r.get(4)?,
                    lines: r.get::<_, i64>(5)? as u64,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Copy any new complete lines from every followed file. Returns lines added.
    pub fn poll(&self) -> u64 {
        let ids: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            let Ok(mut stmt) = conn.prepare("SELECT id FROM sessions WHERE active=1") else {
                return 0;
            };
            stmt.query_map([], |r| r.get(0))
                .map(|rows| rows.filter_map(Result::ok).collect())
                .unwrap_or_default()
        };
        ids.iter().map(|id| self.poll_session(id)).sum()
    }

    /// Copy any new complete lines of one followed file, now. Where the file
    /// was read to is looked up inside the lock, so two callers never copy
    /// the same line twice.
    pub fn poll_session(&self, id: &str) -> u64 {
        let _one_at_a_time = self.poll_lock.lock().unwrap();
        let row: Option<(String, String, i64, i64, Option<String>)> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT agent,path,offset,lines,commit_sha FROM sessions WHERE id=?1 AND active=1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .ok()
            .flatten()
        };
        let Some((agent, path, offset, lines, commit)) = row else {
            return 0;
        };
        self.poll_one(
            id,
            &agent,
            &path,
            offset as u64,
            lines as u64,
            commit.as_deref(),
        )
    }

    fn poll_one(
        &self,
        id: &str,
        agent: &str,
        path: &str,
        offset: u64,
        lines: u64,
        commit: Option<&str>,
    ) -> u64 {
        if agent == "opencode" {
            return self.poll_opencode(id, path, offset as i64, lines, commit);
        }
        let Ok(mut file) = File::open(path) else {
            return 0;
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return 0;
        };
        if len <= offset {
            return 0;
        }
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return 0;
        }
        let mut buf = Vec::with_capacity((len - offset) as usize);
        if file.take(len - offset).read_to_end(&mut buf).is_err() {
            return 0;
        }
        // Only complete lines; a half-written last line waits for the next poll.
        let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
            return 0;
        };
        let complete = &buf[..=last_nl];
        let new: Vec<std::borrow::Cow<str>> = complete
            .split(|b| *b == b'\n')
            .filter(|raw| !raw.is_empty())
            .map(String::from_utf8_lossy)
            .collect();
        self.store_lines(
            id,
            agent,
            new.iter().map(|l| l.as_ref()),
            lines,
            offset + complete.len() as u64,
            commit,
        )
    }

    /// OpenCode keeps a conversation in its database, not in a file: its
    /// settled messages after the last one taken become this session's
    /// next lines (stores.rs). `seq`: the last message taken, kept where a
    /// file's offset is. The database is opened only when it or its
    /// write-ahead log has changed, or half a minute has passed.
    fn poll_opencode(
        &self,
        id: &str,
        path: &str,
        seq: i64,
        lines: u64,
        commit: Option<&str>,
    ) -> u64 {
        const RECHECK: std::time::Duration = std::time::Duration::from_secs(30);
        let stamp = |p: &str| {
            Path::new(p)
                .metadata()
                .ok()
                .map(|m| (m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)))
        };
        let now = (stamp(path), stamp(&format!("{path}-wal")));
        {
            let mut seen = self.stores_seen.lock().unwrap();
            if seen
                .get(id)
                .is_some_and(|(was, at)| *was == now && at.elapsed() < RECHECK)
            {
                return 0;
            }
            seen.insert(id.to_string(), (now, std::time::Instant::now()));
        }
        let new = stores::opencode_messages(Path::new(path), id, seq, 500);
        let Some(last) = new.last().map(|m| m.0) else {
            return 0;
        };
        self.store_lines(
            id,
            "opencode",
            new.iter().map(|m| m.1.as_str()),
            lines,
            last as u64,
            commit,
        )
    }

    /// Keep new lines of a session: raw, and normalised and scrubbed for
    /// search. `offset`: where the next look begins.
    fn store_lines<'a>(
        &self,
        id: &str,
        agent: &str,
        new: impl Iterator<Item = &'a str>,
        lines: u64,
        offset: u64,
        commit: Option<&str>,
    ) -> u64 {
        let mut conn = self.conn.lock().unwrap();
        let Ok(tx) = conn.transaction() else { return 0 };
        let mut line_no = lines;
        let mut replied = None;
        for raw in new {
            line_no += 1;
            let _ = tx.execute(
                "INSERT OR REPLACE INTO raw(session,line,raw) VALUES(?1,?2,?3)",
                params![id, line_no as i64, raw],
            );
            for e in normalize::normalize(agent, raw) {
                for (_, value) in find_keys(&e.text) {
                    let mut found = self.found.lock().unwrap();
                    if found.len() < 200 && !found.iter().any(|(_, v)| *v == value) {
                        found.push((id.to_string(), value));
                    }
                }
                if e.role == "assistant" && e.kind == "reply" {
                    replied = Some(line_no);
                }
                let mut text = scrub(&e.text);
                if text.len() > INDEX_CAP {
                    let mut cut = INDEX_CAP;
                    while !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text.truncate(cut);
                }
                let _ = tx.execute(
                    "INSERT INTO entries(session,line,ts,role,kind,text,tool,files,commit_sha) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        id,
                        line_no as i64,
                        e.ts,
                        e.role,
                        e.kind,
                        text,
                        e.tool,
                        serde_json::to_string(&e.files).unwrap_or_else(|_| "[]".into()),
                        commit
                    ],
                );
            }
        }
        let added = line_no - lines;
        let _ = tx.execute(
            "UPDATE sessions SET offset=?2, lines=?3 WHERE id=?1",
            params![id, offset as i64, line_no as i64],
        );
        let _ = tx.commit();
        drop(conn);
        if let Some(line) = replied {
            self.last_reply.lock().unwrap().insert(id.to_string(), line);
        }
        added
    }

    /// The last line of a session on which its agent said something to the
    /// reader (a reply, not a tool call): what "unread" is measured from.
    /// 0 when it has said nothing.
    pub fn last_reply_line(&self, session: &str) -> u64 {
        if let Some(line) = self.last_reply.lock().unwrap().get(session) {
            return *line;
        }
        let line: i64 = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT line FROM entries WHERE session=?1 AND role='assistant' AND kind='reply'
                 ORDER BY line DESC LIMIT 1",
                [session],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
            .unwrap_or(0)
        };
        *self
            .last_reply
            .lock()
            .unwrap()
            .entry(session.to_string())
            .or_insert(line as u64)
    }

    /// Raw lines after `after`, oldest first: at most `max_lines`, and no
    /// more once `max_bytes` have been read. Station-local use only.
    pub(crate) fn raw_after(
        &self,
        session: &str,
        after: u64,
        max_lines: u32,
        max_bytes: usize,
    ) -> Vec<(u64, String)> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare_cached(
            "SELECT line, raw FROM raw WHERE session=?1 AND line>?2 ORDER BY line LIMIT ?3",
        ) else {
            return vec![];
        };
        let Ok(mut rows) = stmt.query(params![session, after as i64, max_lines]) else {
            return vec![];
        };
        let mut out = vec![];
        let mut bytes = 0;
        while let Ok(Some(r)) = rows.next() {
            let (Ok(line), Ok(raw)) = (r.get::<_, i64>(0), r.get::<_, String>(1)) else {
                break;
            };
            bytes += raw.len();
            out.push((line as u64, raw));
            if bytes >= max_bytes {
                break;
            }
        }
        out
    }

    /// The raw lines with these numbers. Station-local use only.
    pub(crate) fn raw_at(&self, session: &str, lines: &[u64]) -> Vec<(u64, String)> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare_cached("SELECT raw FROM raw WHERE session=?1 AND line=?2")
        else {
            return vec![];
        };
        lines
            .iter()
            .filter_map(|line| {
                stmt.query_row(params![session, *line as i64], |r| r.get(0))
                    .ok()
                    .map(|raw| (*line, raw))
            })
            .collect()
    }

    /// Key-shaped values found since the last call, as (session, value).
    pub fn take_found_keys(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.found.lock().unwrap())
    }

    /// Raw lines exactly as written. Local use only: restore and resume.
    pub fn raw_lines(&self, session: &str) -> Vec<String> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare("SELECT raw FROM raw WHERE session=?1 ORDER BY line")
        else {
            return vec![];
        };
        stmt.query_map([session], |r| r.get(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Put a session file back where the agent expects it (after the vendor
    /// deleted it), so the conversation can be resumed.
    pub fn restore(&self, session: &str) -> anyhow::Result<String> {
        let row: Option<(String, String)> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT path, agent FROM sessions WHERE id=?1",
                [session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        };
        let (path, agent) = row.ok_or_else(|| anyhow::anyhow!("no archived session {session}"))?;
        // Its lines here are a copy of rows of OpenCode's database, which
        // is not a file of lines and is never written by spandeck.
        if agent == "opencode" {
            anyhow::bail!("OpenCode keeps its conversations in its own database: there is no session file to put back");
        }
        if Path::new(&path).exists() {
            anyhow::bail!("{path} still exists; not overwriting it");
        }
        if let Some(dir) = Path::new(&path).parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut body = self.raw_lines(session).join("\n");
        body.push('\n');
        std::fs::write(&path, body)?;
        Ok(path)
    }

    /// Keyword search over the scrubbed copy, newest first.
    pub fn search(&self, query: &str, scope: &Scope, limit: u32) -> Vec<Hit> {
        let words: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| !w.is_empty())
            .map(|w| format!("\"{w}\""))
            .collect();
        if words.is_empty() {
            return vec![];
        }
        let hits = self.search_fts(&words.join(" "), scope, limit);
        if !hits.is_empty() || words.len() == 1 {
            return hits;
        }
        // No line has every word: fall back to any word.
        self.search_fts(&words.join(" OR "), scope, limit)
    }

    fn search_fts(&self, fts: &str, scope: &Scope, limit: u32) -> Vec<Hit> {
        if scope.session.is_none() && scope.project.is_none() && !scope.all_projects {
            return vec![];
        }
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT e.session, s.agent, s.project, e.line, e.ts, e.role, e.kind, e.commit_sha,
                    snippet(entries_fts, 0, '', '', ' … ', 48)
             FROM entries_fts JOIN entries e ON e.id = entries_fts.rowid
             JOIN sessions s ON s.id = e.session
             WHERE entries_fts MATCH ?1
               AND (?2 IS NULL OR e.session = ?2)
               AND (?3 IS NULL OR s.project = ?3 OR e.session = ?2)
             ORDER BY e.id DESC LIMIT ?4",
        ) else {
            return vec![];
        };
        // A session scope with a project means "mine, plus my project's".
        let (session, project) = match (&scope.session, &scope.project, scope.all_projects) {
            (_, _, true) => (None, None),
            (s, Some(p), false) => {
                // project scope: session filter must not narrow it
                let _ = s;
                (None, Some(p.clone()))
            }
            (Some(s), None, false) => (Some(s.clone()), None),
            (None, None, false) => return vec![],
        };
        stmt.query_map(params![fts, session, project, limit], |r| {
            let excerpt: String = r.get(8)?;
            Ok(Hit {
                session: r.get(0)?,
                agent: r.get(1)?,
                project: r.get(2)?,
                line: r.get::<_, i64>(3)? as u64,
                ts: r.get(4)?,
                role: r.get(5)?,
                kind: r.get(6)?,
                commit: r.get(7)?,
                excerpt: excerpt.chars().take(EXCERPT_CHARS).collect(),
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    /// More context around a hit: scrubbed entries near a line.
    pub fn around(&self, session: &str, line: u64, radius: u64) -> Vec<Hit> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT e.session, s.agent, s.project, e.line, e.ts, e.role, e.kind, e.commit_sha, e.text
             FROM entries e JOIN sessions s ON s.id=e.session
             WHERE e.session=?1 AND e.line BETWEEN ?2 AND ?3 ORDER BY e.id",
        ) else {
            return vec![];
        };
        stmt.query_map(
            params![
                session,
                line.saturating_sub(radius) as i64,
                (line + radius) as i64
            ],
            |r| {
                let text: String = r.get(8)?;
                Ok(Hit {
                    session: r.get(0)?,
                    agent: r.get(1)?,
                    project: r.get(2)?,
                    line: r.get::<_, i64>(3)? as u64,
                    ts: r.get(4)?,
                    role: r.get(5)?,
                    kind: r.get(6)?,
                    commit: r.get(7)?,
                    excerpt: text.chars().take(1200).collect(),
                })
            },
        )
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }
}
