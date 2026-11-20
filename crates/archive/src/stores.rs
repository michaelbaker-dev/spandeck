//! The two agent programs that keep no session file of Claude Code's or
//! Codex's kind: where their conversations are, read-only (DECISIONS.md,
//! "One chat for every pane").
//!
//! - **OpenCode** (seen on 2.3.0, 16 Nov 2026) keeps every conversation in
//!   one SQLite database, `<data>/opencode/opencode.db`. `session_v2` has a
//!   row for each conversation (`id`, `directory`, `parent_id`,
//!   `time_created`, `time_updated`, `time_archived`), and
//!   `session_message` a row for each message of it (`session_id`, `seq`,
//!   `type`, `time_created`, and `data`, a JSON object). The types seen:
//!   `user` (`text`), `assistant` (`content`: a list of `text`, `reasoning`
//!   and `tool` parts, a tool part holding its input and what came back;
//!   `time.completed`; `error`) and `idle` (`outcome`: the turn is over).
//!   A row is rewritten while its message is being made, so a message is
//!   taken only once it is settled: it is complete, or a later row exists.
//!   Each settled row becomes one line of the archive, in `seq` order: the
//!   archive's own shape (`mirror_line`), with OpenCode's `data` untouched
//!   inside it.
//! - **Antigravity** (`agy` 1.5.9) writes a transcript for each
//!   conversation as JSON lines:
//!   `~/.gemini/antigravity-cli/brain/<conversation id>/.system_generated/logs/transcript_full.jsonl`
//!   (and `transcript.jsonl`, the same with long fields cut at 4 KB). Its
//!   hooks name the conversation (`conversationId`) and the file
//!   (`transcriptPath`). Which folder a conversation belongs to is in
//!   `conversation_summaries.db` beside `brain/` (`conversation_id`,
//!   `workspace_uris`: a JSON list of `file://` addresses,
//!   `parent_conversation_id`).
//!
//! These stores hold the owner's conversations and are the programs' own:
//! they are opened read-only, never written, never checkpointed, and what is
//! read goes only where a session file's lines go.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

use crate::conversation::ts_ms;

/// A conversation as its program's own store records it: enough to decide
/// whose it is (core, discover.rs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub id: String,
    /// The folders it was begun in, as recorded.
    pub folders: Vec<String>,
    pub started_ms: u64,
    pub modified_ms: u64,
    /// What the archive follows: the transcript file, or the database.
    pub path: PathBuf,
}

fn read_only(db: &Path) -> Option<Connection> {
    if !db.exists() {
        return None;
    }
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_millis(200));
    Some(conn)
}

fn modified_ms(path: &Path) -> u64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

// --------------------------------------------------------------- OpenCode

/// OpenCode's own conversations begun at or after `since_ms`: not one it
/// started itself (a child), not an archived one.
pub fn opencode_since(db: &Path, since_ms: u64) -> Vec<Stored> {
    let Some(conn) = read_only(db) else {
        return vec![];
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, directory, time_created, time_updated FROM session_v2
         WHERE parent_id IS NULL AND time_archived IS NULL AND time_created >= ?1
         ORDER BY time_created",
    ) else {
        return vec![];
    };
    let rows = stmt.query_map([since_ms as i64], |r| {
        Ok(Stored {
            id: r.get(0)?,
            folders: vec![r.get(1)?],
            started_ms: r.get::<_, i64>(2)?.max(0) as u64,
            modified_ms: r.get::<_, i64>(3)?.max(0) as u64,
            path: db.to_path_buf(),
        })
    });
    rows.map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// A conversation a program began itself, for a sub-agent of the one
/// named: what its own store says of it, and no more.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Child {
    pub id: String,
    /// The store's own title for it, when it keeps one.
    pub title: String,
    pub started_ms: u64,
    pub modified_ms: u64,
    /// Whether it is still at work, as far as the store says: `None` when
    /// the store has no word for it.
    pub running: Option<bool>,
    /// What the program calls this sub-agent (its kind, role or name),
    /// when the store says. Else empty.
    pub name: String,
    /// How it ended, when the store says it did not simply finish:
    /// `failed` or `stopped`.
    pub ended: Option<&'static str>,
    /// The child it was begun under, when that is not the conversation
    /// asked about: a sub-agent of a sub-agent.
    pub parent: Option<String>,
    /// 1 for a sub-agent of the conversation asked about, 2 for one of
    /// its own, and so on.
    pub depth: u32,
    /// The file its record is in, for a program that keeps one each.
    pub path: Option<PathBuf>,
    /// What it reported back, in one line, where the store keeps that
    /// beside it.
    pub said: Option<String>,
}

/// One line of at most `max` characters of what a store holds.
fn line_of(s: &str, max: usize) -> String {
    let line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line,
    }
}

/// How many children of one conversation are listed: the newest.
pub const CHILDREN_MAX: usize = 50;

/// The conversations OpenCode began for sub-agents of `parent`
/// (`session_v2.parent_id`), newest first.
///
/// Whether one is at work: its row's own times where it has them
/// (`time_busy` later than `time_idle`: seen on 2.3.0 for the owner's own
/// conversations, where an idle one has `time_idle` at or after
/// `time_busy`), and never once its newest message is the turn's end;
/// with no such times, while its newest message is still being made, or
/// it has none yet. `idle_outcome` says a turn failed. What it is called
/// is the row's `agent`.
///
/// The `job` table (2.3.0's schema: `session_id`, `child_session_id`,
/// `title`, `status`, `output`, `error`) names a child the conversation
/// started as a job: its title where the child has none, a status that
/// says it failed or was cancelled, and the first line of what it gave
/// back. No row of it was on the machine this was written on: its
/// status words are taken by what they contain, and nothing else is.
pub fn opencode_children(db: &Path, parent: &str) -> Vec<Child> {
    let Some(conn) = read_only(db) else {
        return vec![];
    };
    // (A database from before these columns is read as it was.)
    let all = "SELECT id, COALESCE(title, ''), time_created, time_updated,
         COALESCE(agent, ''), time_busy, time_idle, COALESCE(idle_outcome, '') FROM session_v2
         WHERE parent_id=?1 ORDER BY time_created DESC LIMIT ?2";
    let few = "SELECT id, COALESCE(title, ''), time_created, time_updated, '', NULL, NULL, ''
         FROM session_v2 WHERE parent_id=?1 ORDER BY time_created DESC LIMIT ?2";
    let Ok(mut stmt) = conn.prepare(all).or_else(|_| conn.prepare(few)) else {
        return vec![];
    };
    type Row = (
        String,
        String,
        i64,
        i64,
        String,
        Option<i64>,
        Option<i64>,
        String,
    );
    let rows: Vec<Row> = stmt
        .query_map(rusqlite::params![parent, CHILDREN_MAX as i64], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    // What the conversation's jobs say of its children.
    type Job = (String, String, String, String, String);
    let jobs: Vec<Job> = conn
        .prepare(
            "SELECT child_session_id, COALESCE(title, ''), status, COALESCE(output, ''),
             COALESCE(error, '') FROM job WHERE session_id=?1 AND child_session_id IS NOT NULL
             ORDER BY time_updated LIMIT 200",
        )
        .and_then(|mut s| {
            s.query_map([parent], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map(|rows| rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();
    rows.into_iter()
        .map(
            |(id, title, created, updated, agent, busy, idle, outcome)| {
                let last: Option<(String, String)> = conn
                    .query_row(
                        "SELECT type, data FROM session_message WHERE session_id=?1
                     ORDER BY seq DESC LIMIT 1",
                        [&id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .ok();
                let by_times = match (busy, idle) {
                    (Some(b), Some(i)) => Some(b > i),
                    (Some(_), None) => Some(true),
                    (None, Some(_)) => Some(false),
                    (None, None) => None,
                };
                let running = match (by_times, last) {
                    (Some(false), _) => false,
                    (_, Some((ty, _))) if ty == "idle" => false,
                    (Some(true), _) => true,
                    (None, None) => true,
                    (None, Some((ty, data))) => {
                        ty != "assistant"
                            || !serde_json::from_str::<Value>(&data).is_ok_and(|d| settled(&d))
                    }
                };
                let job = jobs.iter().rev().find(|j| j.0 == id);
                let word = job.map(|j| j.2.to_ascii_lowercase()).unwrap_or_default();
                let has = |words: &[&str]| words.iter().any(|w| word.contains(w));
                let ended = if running {
                    None
                } else if outcome == "failed" || has(&["fail", "error"]) {
                    Some("failed")
                } else if has(&["cancel", "abort", "interrupt", "stop", "kill"]) {
                    Some("stopped")
                } else {
                    None
                };
                let said = job
                    .map(|j| if j.3.trim().is_empty() { &j.4 } else { &j.3 })
                    .map(|s| line_of(s, 160))
                    .filter(|s| !s.is_empty() && !running);
                Child {
                    title: match job {
                        Some(j) if title.trim().is_empty() => j.1.clone(),
                        _ => title,
                    },
                    id,
                    started_ms: created.max(0) as u64,
                    modified_ms: updated.max(0) as u64,
                    running: Some(running),
                    name: line_of(&agent, 80),
                    ended,
                    depth: 1,
                    said,
                    ..Child::default()
                }
            },
        )
        .collect()
}

/// Is this one of OpenCode's conversations?
pub fn opencode_has(db: &Path, session: &str) -> bool {
    read_only(db).is_some_and(|conn| {
        conn.query_row(
            "SELECT 1 FROM session_v2 WHERE id=?1",
            [session],
            |_| Ok(()),
        )
        .is_ok()
    })
}

/// A message that will not change any more: it says it is complete (or
/// failed) and none of its tool calls is still under way.
fn settled(data: &Value) -> bool {
    let done = data
        .pointer("/time/completed")
        .is_some_and(|t| t.is_number())
        || data.get("error").is_some_and(|e| e.is_object());
    let running = data["content"].as_array().is_some_and(|parts| {
        parts.iter().any(|p| {
            matches!(
                p.pointer("/state/status").and_then(|s| s.as_str()),
                Some("pending" | "running")
            )
        })
    });
    done && !running
}

/// One settled row as a line of the archive.
fn mirror_line(id: &str, ty: &str, seq: i64, ts: i64, data: Value, extra: Value) -> String {
    let mut line = json!({"type": ty, "id": id, "seq": seq, "ts": ts.max(0), "data": data});
    if let (Some(line), Some(extra)) = (line.as_object_mut(), extra.as_object()) {
        line.extend(extra.clone());
    }
    line.to_string()
}

/// The settled messages of a conversation after `after_seq`, oldest first,
/// each as `(seq, line)`. Stops at the first message still being made: the
/// order of the archive's lines is the order of the conversation.
pub fn opencode_messages(
    db: &Path,
    session: &str,
    after_seq: i64,
    max: usize,
) -> Vec<(i64, String)> {
    let Some(conn) = read_only(db) else {
        return vec![];
    };
    let last: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM session_message WHERE session_id=?1",
            [session],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, type, seq, time_created, data FROM session_message
         WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3",
    ) else {
        return vec![];
    };
    let Ok(mut rows) = stmt.query(rusqlite::params![session, after_seq, max as i64]) else {
        return vec![];
    };
    let mut out = vec![];
    while let Ok(Some(r)) = rows.next() {
        let (Ok(id), Ok(ty), Ok(seq), Ok(ts), Ok(data)) = (
            r.get::<_, String>(0),
            r.get::<_, String>(1),
            r.get::<_, i64>(2),
            r.get::<_, i64>(3),
            r.get::<_, String>(4),
        ) else {
            break;
        };
        let data: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
        if ty == "assistant" && seq >= last && !settled(&data) {
            break;
        }
        let mut extra = json!({});
        if ty == "idle" {
            // How long the turn took, and whether the message before it
            // already said why it failed: both are other rows' to say.
            let asked: Option<i64> = conn
                .query_row(
                    "SELECT MAX(time_created) FROM session_message
                     WHERE session_id=?1 AND type='user' AND seq<?2",
                    rusqlite::params![session, seq],
                    |r| r.get(0),
                )
                .ok()
                .flatten();
            if let Some(asked) = asked.filter(|a| *a <= ts) {
                extra["turn_ms"] = json!(ts - asked);
            }
            let before: Option<(String, String)> = conn
                .query_row(
                    "SELECT type, data FROM session_message
                     WHERE session_id=?1 AND seq<?2 ORDER BY seq DESC LIMIT 1",
                    rusqlite::params![session, seq],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok();
            let said = before.is_some_and(|(ty, data)| {
                ty == "assistant"
                    && serde_json::from_str::<Value>(&data)
                        .is_ok_and(|d| d.get("error").is_some_and(|e| e.is_object()))
            });
            extra["error_said"] = json!(said);
        }
        out.push((seq, mirror_line(&id, &ty, seq, ts, data, extra)));
    }
    out
}

/// The newest `max` messages of a conversation as lines, oldest first,
/// one still being made among them: what a sub-agent is doing now is its
/// newest message, settled or not. Never what the archive keeps.
pub fn opencode_tail(db: &Path, session: &str, max: usize) -> Vec<String> {
    let Some(conn) = read_only(db) else {
        return vec![];
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, type, seq, time_created, data FROM session_message
         WHERE session_id=?1 ORDER BY seq DESC LIMIT ?2",
    ) else {
        return vec![];
    };
    let mut lines: Vec<String> = stmt
        .query_map(rusqlite::params![session, max as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|(id, ty, seq, ts, data)| {
            let data = serde_json::from_str(&data).unwrap_or(Value::Null);
            mirror_line(&id, &ty, seq, ts, data, json!({}))
        })
        .collect();
    lines.reverse();
    lines
}

// ------------------------------------------------------------ Antigravity

/// Where the Antigravity CLI keeps what it knows.
pub fn antigravity_dir(home: &Path) -> PathBuf {
    home.join(".gemini").join("antigravity-cli")
}

/// A conversation's id is a name in a path here: letters, digits and `-`.
fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The file beside a transcript that holds the same lines with nothing
/// cut, when the program wrote one; else the file itself.
pub fn antigravity_whole(named: &Path) -> PathBuf {
    if named.file_name().and_then(|n| n.to_str()) == Some("transcript.jsonl") {
        let full = named.with_file_name("transcript_full.jsonl");
        if full.is_file() {
            return full;
        }
    }
    named.to_path_buf()
}

/// The transcript of a conversation, by its id, if it has one.
pub fn antigravity_transcript(dir: &Path, id: &str) -> Option<PathBuf> {
    if !plain_id(id) {
        return None;
    }
    let logs = dir.join("brain").join(id).join(".system_generated/logs");
    let whole = antigravity_whole(&logs.join("transcript.jsonl"));
    whole.is_file().then_some(whole)
}

/// `file:///home/ada/my%20work` as a path.
fn uri_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| rest.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The conversations Antigravity began for sub-agents of `parent`
/// (`parent_conversation_id`), newest first. When one began is its
/// transcript's first line, and when it was last written to is the file's.
///
/// Its store (seen on `agy` 1.5.10) has no title for a child and does
/// say the rest: `status` (`CASCADE_RUN_STATUS_IDLE`, and by the
/// program's own names `…_RUNNING`, `…_BUSY`, `…_CANCELING`: only the
/// idle one was in the store read), `killed`, and `agent_name`, the kind
/// of sub-agent. What the parent made it for is beside the parent,
/// `brain/<parent>/.system_generated/subagents/<child>.json`
/// (`subagentDescriptor.typeName` and `role`, a few words; its `state`
/// stays `SUBAGENT_STATE_ALIVE` for one long idle, and is not read). A
/// store from before those columns, or a status that is none of those
/// words, says nothing of whether one is at work (`running: None`).
pub fn antigravity_children(dir: &Path, parent: &str) -> Vec<Child> {
    let Some(conn) = read_only(&dir.join("conversation_summaries.db")) else {
        return vec![];
    };
    let all = "SELECT conversation_id, status, agent_name, killed FROM conversation_summaries
         WHERE parent_conversation_id=?1 LIMIT 200";
    let few = "SELECT conversation_id, '', '', 0 FROM conversation_summaries
         WHERE parent_conversation_id=?1 LIMIT 200";
    let Ok(mut stmt) = conn.prepare(all).or_else(|_| conn.prepare(few)) else {
        return vec![];
    };
    let rows: Vec<(String, String, String, bool)> = stmt
        .query_map([parent], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                matches!(
                    r.get::<_, rusqlite::types::Value>(3),
                    Ok(rusqlite::types::Value::Integer(k)) if k != 0
                ),
            ))
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    let described = |id: &str| -> Option<(String, String)> {
        if !plain_id(parent) || !plain_id(id) {
            return None;
        }
        let file = dir
            .join("brain")
            .join(parent)
            .join(".system_generated/subagents")
            .join(format!("{id}.json"));
        if file.metadata().ok()?.len() > 256 * 1024 {
            return None;
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
        let word = |key: &str| line_of(v["subagentDescriptor"][key].as_str().unwrap_or(""), 200);
        Some((word("typeName"), word("role")))
    };
    let mut out: Vec<Child> = rows
        .into_iter()
        .filter_map(|(id, status, agent, killed)| {
            let path = antigravity_transcript(dir, &id)?;
            let mut first = String::new();
            BufReader::new(std::fs::File::open(&path).ok()?)
                .read_line(&mut first)
                .ok()?;
            let first: Value = serde_json::from_str(&first).ok()?;
            let running = match status.rsplit('_').next().unwrap_or("") {
                _ if killed => Some(false),
                "IDLE" => Some(false),
                "RUNNING" | "BUSY" | "CANCELING" => Some(true),
                _ => None,
            };
            let (kind, role) = described(&id).unwrap_or_default();
            Some(Child {
                title: role,
                started_ms: first["created_at"].as_str().and_then(ts_ms)?,
                modified_ms: modified_ms(&path),
                running,
                name: if agent.trim().is_empty() {
                    kind
                } else {
                    line_of(&agent, 80)
                },
                ended: killed.then_some("stopped"),
                depth: 1,
                path: Some(path),
                id,
                ..Child::default()
            })
        })
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.started_ms));
    out.truncate(CHILDREN_MAX);
    out
}

/// Antigravity's own conversations whose transcript was begun at or after
/// `since_ms`: not one it started itself (a child). When one began is its
/// transcript's first line, to the second; a conversation whose line is not
/// there to read is left out.
pub fn antigravity_since(dir: &Path, since_ms: u64) -> Vec<Stored> {
    let Some(conn) = read_only(&dir.join("conversation_summaries.db")) else {
        return vec![];
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT conversation_id, workspace_uris FROM conversation_summaries
         WHERE parent_conversation_id = ''",
    ) else {
        return vec![];
    };
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    rows.into_iter()
        .filter_map(|(id, uris)| {
            let path = antigravity_transcript(dir, &id)?;
            // A transcript touched before the pane started cannot have been
            // begun since: its first line need not be read.
            let modified = modified_ms(&path);
            if modified < since_ms {
                return None;
            }
            let mut first = String::new();
            BufReader::new(std::fs::File::open(&path).ok()?)
                .read_line(&mut first)
                .ok()?;
            let first: Value = serde_json::from_str(&first).ok()?;
            let started_ms = first["created_at"].as_str().and_then(ts_ms)?;
            let folders = serde_json::from_str::<Vec<String>>(&uris)
                .unwrap_or_default()
                .iter()
                .filter_map(|u| uri_path(u))
                .collect();
            Some(Stored {
                id,
                folders,
                started_ms,
                modified_ms: modified,
                path,
            })
        })
        .filter(|s| s.started_ms >= since_ms)
        .collect()
}

// ------------------------------------------------------------------ Codex

/// What the first record of a Codex thread's file says of it.
#[derive(Debug, Clone)]
struct CodexHead {
    id: String,
    parent: Option<String>,
    /// A thread the program runs for itself (its approval reviewer, a
    /// compaction): no sub-agent anyone asked for.
    internal: bool,
    name: String,
    started_ms: u64,
}

/// How many of Codex's files are looked at for one conversation's children.
const CODEX_FILES_MAX: usize = 500;

fn read_line_of(path: &Path, max: u64) -> Option<String> {
    use std::io::Read;
    let mut reader = BufReader::new(std::fs::File::open(path).ok()?.take(max));
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).ok()?;
    (line.last() == Some(&b'\n')).then(|| String::from_utf8_lossy(&line).into_owned())
}

/// The first record of a Codex thread's file (`session_meta`), once it is
/// whole. Read once for a file: the record does not change.
fn codex_head(path: &Path) -> Option<CodexHead> {
    type Heads = std::collections::HashMap<PathBuf, Option<CodexHead>>;
    static HEADS: std::sync::OnceLock<std::sync::Mutex<Heads>> = std::sync::OnceLock::new();
    let heads = HEADS.get_or_init(Default::default);
    if let Some(known) = heads.lock().unwrap().get(path) {
        return known.clone();
    }
    let head = read_line_of(path, 4 << 20).and_then(|line| {
        let v: Value = serde_json::from_str(&line).ok()?;
        if v["type"] != json!("session_meta") {
            return None;
        }
        let p = &v["payload"];
        let sub = &p["source"]["subagent"];
        let spawn = &sub["thread_spawn"];
        let word = |key: &str| {
            spawn[key]
                .as_str()
                .or_else(|| p[key].as_str())
                .map(|w| line_of(w, 80))
                .filter(|w| !w.is_empty())
        };
        let guardian = |w: &Value| w.as_str().is_some_and(|w| w.contains("guardian"));
        Some(CodexHead {
            id: p["id"].as_str().filter(|id| plain_id(id))?.to_string(),
            parent: p["parent_thread_id"]
                .as_str()
                .or_else(|| spawn["parent_thread_id"].as_str())
                .filter(|id| plain_id(id))
                .map(String::from),
            internal: guardian(&p["thread_source"])
                || guardian(&sub["other"])
                || matches!(sub.as_str(), Some("compact") | Some("memory_consolidation")),
            name: word("agent_role")
                .or_else(|| word("agent_nickname"))
                .or_else(|| sub.as_str().map(|w| line_of(w, 80)))
                .unwrap_or_default(),
            started_ms: p["timestamp"].as_str().and_then(ts_ms).unwrap_or(0),
        })
    });
    // (A first line still being written is looked at again.)
    let settled = head.is_some()
        || std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .is_ok_and(|now| (now.as_millis() as u64).saturating_sub(modified_ms(path)) > 10_000);
    if settled {
        let mut heads = heads.lock().unwrap();
        if heads.len() > 4000 {
            heads.clear();
        }
        heads.insert(path.to_path_buf(), head.clone());
    }
    head
}

/// The end of a file: its last `max` bytes as whole lines.
pub fn tail_lines(path: &Path, max: u64) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return vec![];
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(max);
    let mut bytes = vec![];
    if f.seek(SeekFrom::Start(from)).is_err() || f.take(max).read_to_end(&mut bytes).is_err() {
        return vec![];
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if from > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    if !text.ends_with('\n') {
        lines.pop();
    }
    lines
        .into_iter()
        .filter(|l| !l.trim().is_empty())
        .map(String::from)
        .collect()
}

/// Whether a Codex thread is at work, by the last of its turn's own marks
/// in the end of its file (`event_msg`: `task_started`, `task_complete`,
/// `turn_aborted`): `(running, how it ended)`. `None` when the end of the
/// file has none of them.
fn codex_turn(path: &Path) -> (Option<bool>, Option<&'static str>) {
    // (The end of a file is read again only when the file has grown.)
    type Turn = (Option<bool>, Option<&'static str>);
    type Turns = std::collections::HashMap<PathBuf, (u64, Turn)>;
    static TURNS: std::sync::OnceLock<std::sync::Mutex<Turns>> = std::sync::OnceLock::new();
    let turns = TURNS.get_or_init(Default::default);
    let len = path.metadata().map(|m| m.len()).unwrap_or(0);
    if let Some((_, turn)) = turns.lock().unwrap().get(path).filter(|(l, _)| *l == len) {
        return *turn;
    }
    let turn = codex_turn_read(path);
    let mut turns = turns.lock().unwrap();
    if turns.len() > 1000 {
        turns.clear();
    }
    turns.insert(path.to_path_buf(), (len, turn));
    turn
}

fn codex_turn_read(path: &Path) -> (Option<bool>, Option<&'static str>) {
    for line in tail_lines(path, 128 * 1024).iter().rev() {
        if !line.contains("\"event_msg\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v["payload"]["type"].as_str() {
            Some("task_started") => return (Some(true), None),
            Some("task_complete") => return (Some(false), None),
            Some("turn_aborted") => return (Some(false), Some("stopped")),
            _ => {}
        }
    }
    (None, None)
}

/// What a Codex thread was first asked, in one line: the first message in
/// its file that someone sent, among its first records.
fn codex_asked(path: &Path) -> Option<String> {
    use crate::conversation::{parse_line, Dialect};
    use std::io::Read;
    // (Found once for a file: its first message does not change.)
    type Asked = std::collections::HashMap<PathBuf, String>;
    static ASKED: std::sync::OnceLock<std::sync::Mutex<Asked>> = std::sync::OnceLock::new();
    let asked = ASKED.get_or_init(Default::default);
    if let Some(known) = asked.lock().unwrap().get(path) {
        return Some(known.clone());
    }
    let reader = BufReader::new(std::fs::File::open(path).ok()?.take(8 << 20));
    let found = reader
        .lines()
        .map_while(Result::ok)
        .enumerate()
        .skip(1)
        .take(80)
        .find_map(|(n, raw)| {
            parse_line(Dialect::Codex, n as u64 + 1, &raw)
                .items
                .into_iter()
                .find(|i| i.kind == "user")
                .and_then(|i| i.text)
        })
        .map(|t| line_of(&t, 200))
        .filter(|t| !t.is_empty())?;
    let mut asked = asked.lock().unwrap();
    if asked.len() > 1000 {
        asked.clear();
    }
    asked.insert(path.to_path_buf(), found.clone());
    Some(found)
}

/// What Codex's own state database says of the threads `parent` spawned
/// (`thread_spawn_edges`, `threads`; schema seen on codex-cli 0.170.0,
/// where no row of the first was): `(child, role or nickname, first
/// message, file)`. The newest `state_*.sqlite` beside `sessions`.
fn codex_spawned(sessions: &Path, parent: &str) -> Vec<(String, String, String, String)> {
    let Some(home) = sessions.parent() else {
        return vec![];
    };
    let mut dbs: Vec<PathBuf> = std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.strip_prefix("state_")
                    .and_then(|n| n.strip_suffix(".sqlite"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
        })
        .collect();
    dbs.sort_by_key(|p| {
        p.file_stem()
            .and_then(|n| n.to_str())
            .and_then(|n| n["state_".len()..].parse::<u64>().ok())
    });
    let Some(conn) = dbs.last().and_then(|db| read_only(db)) else {
        return vec![];
    };
    conn.prepare(
        "SELECT e.child_thread_id, COALESCE(t.agent_role, ''), COALESCE(t.agent_nickname, ''),
         COALESCE(t.first_user_message, ''), COALESCE(t.rollout_path, '')
         FROM thread_spawn_edges e LEFT JOIN threads t ON t.id = e.child_thread_id
         WHERE e.parent_thread_id=?1 LIMIT 50",
    )
    .and_then(|mut s| {
        s.query_map([parent], |r| {
            let (role, nick): (String, String) = (r.get(1)?, r.get(2)?);
            Ok((
                r.get(0)?,
                if role.trim().is_empty() { nick } else { role },
                r.get(3)?,
                r.get(4)?,
            ))
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
    })
    .unwrap_or_default()
}

/// The threads Codex began for sub-agents of `parent`, and theirs, newest
/// first: each has a file of its own under `sessions`
/// (`rollout-<time>-<id>.jsonl`) whose first record names the thread it
/// was begun under (`session_meta`: `parent_thread_id`; seen on codex-cli
/// 0.170.0). The parent's own file never names them. The program's own
/// reviewer of approvals is such a thread too (`thread_source`:
/// `guardian_review`; `source`: `{"subagent": {"other": "guardian"}}`),
/// and is not listed: it is the only kind that was on the machine this
/// was written on. What one is called (`agent_role`, `agent_nickname`)
/// is read where Codex's public source puts it, and from its state
/// database; neither was seen filled.
///
/// Only files in a day's folder that was added to since `since_ms` are
/// looked at (a child is begun after its parent), `CODEX_FILES_MAX` at
/// most, and each file's first record once.
pub fn codex_children(sessions: &Path, parent: &str, since_ms: u64) -> Vec<Child> {
    if !plain_id(parent) {
        return vec![];
    }
    let since = since_ms.saturating_sub(60_000);
    let names = |dir: &Path| -> Vec<PathBuf> {
        let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        names.sort_by(|a, b| b.cmp(a));
        names
    };
    let mut heads: Vec<(PathBuf, CodexHead)> = vec![];
    let mut seen = 0;
    'days: for year in names(sessions) {
        for month in names(&year) {
            for day in names(&month) {
                if modified_ms(&day) < since {
                    break 'days;
                }
                for file in names(&day) {
                    let rollout = file
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"));
                    if !rollout || modified_ms(&file) < since {
                        continue;
                    }
                    seen += 1;
                    if seen > CODEX_FILES_MAX {
                        break 'days;
                    }
                    if let Some(head) = codex_head(&file).filter(|h| h.parent.is_some()) {
                        heads.push((file, head));
                    }
                }
            }
        }
    }
    let mut out: Vec<Child> = vec![];
    let mut under = vec![(parent.to_string(), 0u32)];
    while let Some((id, depth)) = under.pop() {
        if depth >= 3 {
            continue;
        }
        let spawned = codex_spawned(sessions, &id);
        // One the state database names and no file looked at is: its file
        // is taken only where its name ends with the thread's id, under
        // `sessions`.
        for (child, _, _, file) in &spawned {
            let file = Path::new(file);
            let named = file
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&format!("-{child}.jsonl")));
            if plain_id(child)
                && named
                && file.starts_with(sessions)
                && !file
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
                && !heads.iter().any(|(_, h)| h.id == *child)
            {
                if let Some(mut head) = codex_head(file) {
                    head.parent = Some(id.clone());
                    heads.push((file.to_path_buf(), head));
                }
            }
        }
        for (file, head) in heads
            .iter()
            .filter(|(_, h)| h.parent.as_deref() == Some(id.as_str()) && !h.internal)
        {
            if out.len() >= CHILDREN_MAX || out.iter().any(|c| c.id == head.id) {
                continue;
            }
            let (running, ended) = codex_turn(file);
            let told = spawned.iter().find(|s| s.0 == head.id);
            out.push(Child {
                id: head.id.clone(),
                title: codex_asked(file)
                    .or_else(|| told.map(|s| line_of(&s.2, 200)))
                    .unwrap_or_default(),
                started_ms: head.started_ms,
                modified_ms: modified_ms(file),
                running,
                name: match told {
                    Some(s) if head.name.is_empty() => line_of(&s.1, 80),
                    _ => head.name.clone(),
                },
                ended,
                parent: (depth > 0).then(|| id.clone()),
                depth: depth + 1,
                path: Some(file.clone()),
                said: None,
            });
            under.push((head.id.clone(), depth + 1));
        }
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.started_ms));
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// OpenCode 2.3.0's two tables, with the columns that are read and the
    /// ones its own `NOT NULL` asks for. Made-up words.
    pub fn opencode_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_v2 (id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
                slug text NOT NULL, directory text NOT NULL, title text, version text NOT NULL,
                time_created integer NOT NULL, time_updated integer NOT NULL, time_archived integer);
             CREATE TABLE session_message (id text PRIMARY KEY, session_id text NOT NULL, type text NOT NULL,
                seq integer NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL);",
        )
        .unwrap();
        conn
    }

    pub fn opencode_say(
        conn: &Connection,
        session: &str,
        seq: i64,
        ty: &str,
        ts: i64,
        data: Value,
    ) {
        conn.execute(
            "INSERT OR REPLACE INTO session_message VALUES(?1,?2,?3,?4,?5,?5,?6)",
            rusqlite::params![
                format!("msg_{session}_{seq}"),
                session,
                ty,
                seq,
                ts,
                data.to_string()
            ],
        )
        .unwrap();
    }

    /// The owner's report of 16 Nov 2026: OpenCode's Chat was its screen.
    /// Its conversation is read from its database, a message only once it
    /// will not change: a reply still being written, or a tool call still
    /// running, is not taken, and nothing after it is taken before it.
    #[test]
    fn an_opencode_message_is_taken_once_it_is_settled_and_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let conn = opencode_db(&db);
        conn.execute(
            "INSERT INTO session_v2 VALUES('ses_a','p',NULL,'s','/w/reeds',NULL,'2.3.0',1000,1000,NULL)",
            [],
        )
        .unwrap();
        opencode_say(
            &conn,
            "ses_a",
            1,
            "user",
            1000,
            json!({"text": "count the reeds", "time": {"created": 1000}}),
        );
        let writing =
            json!({"content": [{"type": "text", "text": "coun"}], "time": {"created": 1100}});
        opencode_say(&conn, "ses_a", 2, "assistant", 1100, writing);
        let seqs = |after| -> Vec<i64> {
            opencode_messages(&db, "ses_a", after, 100)
                .iter()
                .map(|m| m.0)
                .collect()
        };
        assert_eq!(seqs(0), vec![1]);
        // Complete, but its tool call is still running.
        let running = json!({"content": [{"type": "tool", "name": "shell", "state": {"status": "running", "input": {"command": "wc -l"}}}],
            "time": {"created": 1100, "completed": 1200}});
        opencode_say(&conn, "ses_a", 2, "assistant", 1100, running);
        assert_eq!(seqs(1), Vec::<i64>::new());
        let done = json!({"content": [{"type": "tool", "name": "shell", "state": {"status": "completed", "input": {"command": "wc -l"},
            "content": [{"type": "text", "text": "41"}]}}], "time": {"created": 1100, "completed": 1300}});
        opencode_say(&conn, "ses_a", 2, "assistant", 1100, done);
        opencode_say(
            &conn,
            "ses_a",
            3,
            "idle",
            4000,
            json!({"outcome": "succeeded", "time": {"created": 4000}}),
        );
        let got = opencode_messages(&db, "ses_a", 1, 100);
        assert_eq!(got.iter().map(|m| m.0).collect::<Vec<_>>(), vec![2, 3]);
        let idle: Value = serde_json::from_str(&got[1].1).unwrap();
        assert_eq!(
            (
                idle["type"].as_str(),
                idle["turn_ms"].as_i64(),
                idle["error_said"].as_bool()
            ),
            (Some("idle"), Some(3000), Some(false))
        );
        // Whose it could be: begun since, not a child, not archived.
        conn.execute_batch(
            "INSERT INTO session_v2 VALUES('ses_child','p','ses_a','s','/w/reeds',NULL,'2.3.0',2000,2000,NULL);
             INSERT INTO session_v2 VALUES('ses_old','p',NULL,'s','/w/reeds',NULL,'2.3.0',500,9000,NULL);",
        )
        .unwrap();
        let ids: Vec<String> = opencode_since(&db, 1000)
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec!["ses_a"]);
        assert!(opencode_has(&db, "ses_old") && !opencode_has(&db, "ses_none"));
        assert!(opencode_messages(&dir.path().join("absent.db"), "ses_a", 0, 10).is_empty());
    }

    /// The owner, 17 Nov 2026: "make sure this all this is for any agent
    /// chat not just Claude". OpenCode and Antigravity keep a conversation
    /// of its own for each sub-agent, under its parent's id: those are the
    /// background tasks their chats list.
    #[test]
    fn a_programs_own_child_conversations_are_listed_under_their_parent() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let oc = opencode_db(&db);
        oc.execute_batch(
            "INSERT INTO session_v2 VALUES('ses_parent', 'p', NULL, 's', '/w/reeds', 'Reeds', '2.3.0', 1000, 2000, NULL);
             INSERT INTO session_v2 VALUES('ses_kid1', 'p', 'ses_parent', 's', '/w/reeds', 'Count the nests', '2.3.0', 1100, 1500, NULL);
             INSERT INTO session_v2 VALUES('ses_kid2', 'p', 'ses_parent', 's', '/w/reeds', 'Map the bank', '2.3.0', 1200, 1900, NULL);
             INSERT INTO session_v2 VALUES('ses_other', 'p', 'ses_else', 's', '/w/reeds', 'Not ours', '2.3.0', 1300, 1900, NULL);
             INSERT INTO session_message VALUES('m1', 'ses_kid1', 'assistant', 1, 1100, 1500, '{\"time\":{\"completed\":1500},\"content\":[]}');
             INSERT INTO session_message VALUES('m2', 'ses_kid2', 'assistant', 1, 1200, 1900, '{\"content\":[{\"state\":{\"status\":\"running\"}}]}');",
        )
        .unwrap();
        let kids = opencode_children(&db, "ses_parent");
        assert_eq!(
            kids.iter()
                .map(|c| (c.id.as_str(), c.title.as_str(), c.running))
                .collect::<Vec<_>>(),
            vec![
                ("ses_kid2", "Map the bank", Some(true)),
                ("ses_kid1", "Count the nests", Some(false))
            ]
        );
        assert!(opencode_children(&db, "ses_kid1").is_empty());

        let agy = dir.path();
        let conn = Connection::open(agy.join("conversation_summaries.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversation_summaries (conversation_id text PRIMARY KEY, workspace_uris text NOT NULL,
                parent_conversation_id text NOT NULL DEFAULT '');
             INSERT INTO conversation_summaries VALUES('aaaa-1', '[]', '');
             INSERT INTO conversation_summaries VALUES('bbbb-2', '[]', 'aaaa-1');
             INSERT INTO conversation_summaries VALUES('cccc-3', '[]', 'aaaa-1');",
        )
        .unwrap();
        // (One of the two has no transcript to read yet: it is left out.)
        let logs = agy.join("brain/bbbb-2/.system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("transcript_full.jsonl"),
            "{\"step_index\":0,\"type\":\"USER_INPUT\",\"created_at\":\"2026-11-16T10:00:00Z\"}\n",
        )
        .unwrap();
        let kids = antigravity_children(agy, "aaaa-1");
        assert_eq!(kids.len(), 1, "{kids:?}");
        assert_eq!((kids[0].id.as_str(), kids[0].running), ("bbbb-2", None));
        assert_eq!(Some(kids[0].started_ms), ts_ms("2026-11-16T10:00:00Z"));
    }

    /// One file of Codex's for a thread, as codex-cli 0.170.0 writes one
    /// (`session_meta` with `parent_thread_id`, `source`, `thread_source`;
    /// then the turn's own marks and what was said). Made-up words.
    fn codex_thread(
        sessions: &Path,
        id: &str,
        at: &str,
        meta: Value,
        asked: &str,
        then: &[Value],
    ) -> PathBuf {
        let day = sessions.join("2026/11/20");
        std::fs::create_dir_all(&day).unwrap();
        let mut payload = json!({"id": id, "session_id": id, "timestamp": at, "cwd": "/w/reeds",
            "originator": "codex_cli_rs", "cli_version": "0.170.0", "model_provider": "openai",
            "history_mode": "full", "base_instructions": {"text": "made-up"}});
        payload
            .as_object_mut()
            .unwrap()
            .extend(meta.as_object().unwrap().clone());
        let mut lines = vec![
            json!({"timestamp": at, "type": "session_meta", "payload": payload}),
            json!({"timestamp": at, "type": "response_item", "payload": {"type": "message", "role": "user",
                "content": [{"type": "input_text", "text": "<environment_context>\n  <cwd>/w/reeds</cwd>\n</environment_context>"}]}}),
            json!({"timestamp": at, "type": "event_msg", "payload": {"type": "task_started", "turn_id": "t1"}}),
            json!({"timestamp": at, "type": "response_item", "payload": {"type": "message", "role": "user",
                "content": [{"type": "input_text", "text": asked}]}}),
        ];
        lines.extend(then.iter().cloned());
        let file = day.join(format!("rollout-2026-11-20T10-00-00-{id}.jsonl"));
        std::fs::write(
            &file,
            lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
        )
        .unwrap();
        file
    }

    /// The owner's request of 20 Nov 2026: "having subagent view ... inside
    /// a agent pane", for every program. Codex showed a count and no more:
    /// its sub-agents are threads with files of their own, each naming the
    /// thread it was begun under. Its own reviewer of approvals is such a
    /// thread too, and is not a sub-agent anyone asked for.
    #[test]
    fn codex_threads_begun_under_a_conversation_are_its_sub_agents_and_its_reviewer_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let (parent, kid, done, grand, guard, other) = (
            "019ab3c4-0000-7000-8000-0000000000aa",
            "019ab3c4-0000-7000-8000-0000000000b1",
            "019ab3c4-0000-7000-8000-0000000000b2",
            "019ab3c4-0000-7000-8000-0000000000c1",
            "019ab3c4-0000-7000-8000-0000000000d1",
            "019ab3c4-0000-7000-8000-0000000000e1",
        );
        let spawned = |under: &str, depth: u32, role: &str| {
            json!({"parent_thread_id": under, "thread_source": "subagent",
                "source": {"subagent": {"thread_spawn": {"parent_thread_id": under, "depth": depth,
                    "agent_nickname": "Heron", "agent_role": role}}}})
        };
        let said = |text: &str| json!({"timestamp": "2026-11-20T10:00:09.000Z", "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}], "phase": "final_answer"}});
        let over = json!({"timestamp": "2026-11-20T10:00:10.000Z", "type": "event_msg", "payload": {"type": "task_complete", "turn_id": "t1"}});
        codex_thread(
            &sessions,
            parent,
            "2026-11-20T10:00:00.000Z",
            json!({"source": "cli", "thread_source": "user"}),
            "Count the reeds",
            &[],
        );
        let kid_file = codex_thread(
            &sessions,
            kid,
            "2026-11-20T10:00:02.000Z",
            spawned(parent, 1, "worker"),
            "Count the nests\non the east bank",
            &[],
        );
        codex_thread(
            &sessions,
            done,
            "2026-11-20T10:00:03.000Z",
            spawned(parent, 1, "explorer"),
            "Map the bank",
            &[said("Mapped."), over.clone()],
        );
        codex_thread(
            &sessions,
            grand,
            "2026-11-20T10:00:04.000Z",
            spawned(kid, 2, "worker"),
            "Weigh one nest",
            &[
                json!({"timestamp": "2026-11-20T10:00:05.000Z", "type": "event_msg", "payload": {"type": "turn_aborted", "reason": "interrupted"}}),
            ],
        );
        // As the reviewer's files are on a real machine.
        codex_thread(
            &sessions,
            guard,
            "2026-11-20T10:00:05.000Z",
            json!({"parent_thread_id": parent, "thread_source": "guardian_review",
                "source": {"subagent": {"other": "guardian"}}}),
            "Review this command",
            std::slice::from_ref(&over),
        );
        codex_thread(
            &sessions,
            other,
            "2026-11-20T10:00:06.000Z",
            spawned("019ab3c4-0000-7000-8000-0000000000ff", 1, "worker"),
            "Not ours",
            &[],
        );
        let since = ts_ms("2026-11-20T10:00:00.000Z").unwrap();
        let kids = codex_children(&sessions, parent, 0);
        assert_eq!(
            kids.iter()
                .map(|c| (
                    c.id.as_str(),
                    c.name.as_str(),
                    c.title.as_str(),
                    c.running,
                    c.ended,
                    c.depth,
                    c.parent.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    grand,
                    "worker",
                    "Weigh one nest",
                    Some(false),
                    Some("stopped"),
                    2,
                    Some(kid)
                ),
                (done, "explorer", "Map the bank", Some(false), None, 1, None),
                (
                    kid,
                    "worker",
                    "Count the nests on the east bank",
                    Some(true),
                    None,
                    1,
                    None
                ),
            ]
        );
        assert_eq!(
            (kids[2].started_ms, kids[2].path.as_deref()),
            (since + 2000, Some(kid_file.as_path()))
        );
        // A conversation with none, an id that is no name, and a folder
        // nothing was added to since.
        assert!(codex_children(&sessions, other, 0).is_empty());
        assert!(codex_children(&sessions, "../x", 0).is_empty());
        assert!(codex_children(&sessions, parent, u64::MAX).is_empty());
        assert!(codex_children(&dir.path().join("absent"), parent, 0).is_empty());

        // Codex's own state database names a thread it spawned and what
        // it is called (the tables of 0.170.0, with the columns read): a
        // file it names is taken only where it is one of Codex's.
        let named = "019ab3c4-0000-7000-8000-0000000000f1";
        let old = dir.path().join("sessions/2026/11/19");
        std::fs::create_dir_all(&old).unwrap();
        let moved = old.join(format!("rollout-2026-11-19T09-00-00-{named}.jsonl"));
        std::fs::rename(
            codex_thread(
                &sessions,
                named,
                "2026-11-19T09:00:00.000Z",
                json!({"source": {"subagent": {"thread_spawn": {"depth": 1}}}}),
                "Ring the chicks",
                &[over],
            ),
            &moved,
        )
        .unwrap();
        let conn = Connection::open(dir.path().join("state_5.sqlite")).unwrap();
        conn.execute_batch(
            "CREATE TABLE thread_spawn_edges (parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL PRIMARY KEY, status TEXT NOT NULL);
             CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, title TEXT NOT NULL,
                first_user_message TEXT NOT NULL DEFAULT '', agent_nickname TEXT, agent_role TEXT);",
        )
        .unwrap();
        let edge = |child: &str, path: &str| {
            conn.execute(
                "INSERT INTO thread_spawn_edges VALUES(?1, ?2, 'closed')",
                [parent, child],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO threads VALUES(?1, ?2, 't', 'Ring the chicks', 'Wren', NULL)",
                [child, path],
            )
            .unwrap();
        };
        edge(named, moved.to_str().unwrap());
        let outside = dir
            .path()
            .join("rollout-x-019ab3c4-0000-7000-8000-0000000000f2.jsonl");
        std::fs::copy(&moved, &outside).unwrap();
        edge(
            "019ab3c4-0000-7000-8000-0000000000f2",
            outside.to_str().unwrap(),
        );
        // (The folder of the day before was added to before the parent
        // began: only the database names this one.)
        let kids = codex_children(&sessions, parent, modified_ms(&kid_file));
        let ring = kids.iter().find(|c| c.id == named).expect("the named one");
        assert_eq!(
            (ring.name.as_str(), ring.title.as_str(), ring.running),
            ("Wren", "Ring the chicks", Some(false))
        );
        assert_eq!(kids.len(), 4, "{kids:?}");
    }

    /// The same request, for OpenCode and Antigravity: what their stores
    /// say of a child beside its id. OpenCode 2.3.0's `session_v2` has
    /// `agent`, `time_busy`, `time_idle` and `idle_outcome`, and a `job`
    /// table; Antigravity 1.5.10's summaries have `status`, `agent_name`
    /// and `killed`, and the parent keeps a small file about each child.
    /// (The tables with the columns read and their own names; made-up
    /// words.)
    #[test]
    fn a_child_conversation_says_what_it_is_called_and_how_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let oc = Connection::open(&db).unwrap();
        oc.execute_batch(
            "CREATE TABLE session_v2 (id text PRIMARY KEY, project_id text NOT NULL, parent_id text, slug text NOT NULL,
                directory text NOT NULL, title text, version text NOT NULL, agent text, time_created integer NOT NULL,
                time_updated integer NOT NULL, time_busy integer, time_idle integer, idle_outcome text, time_archived integer);
             CREATE TABLE session_message (id text PRIMARY KEY, session_id text NOT NULL, type text NOT NULL,
                seq integer NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL);
             CREATE TABLE job (notification_id text PRIMARY KEY, id text NOT NULL, session_id text NOT NULL,
                child_session_id text, kind text NOT NULL, title text, status text NOT NULL, output text, error text,
                recovery text NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL);
             INSERT INTO session_v2 VALUES('ses_p', 'p', NULL, 's', '/w', 'Reeds', '2.3.0', 'build', 1000, 5000, 1000, NULL, NULL, NULL);
             INSERT INTO session_v2 VALUES('ses_busy', 'p', 'ses_p', 's', '/w', 'Count the nests', '2.3.0', 'explore', 1100, 5000, 1100, NULL, NULL, NULL);
             INSERT INTO session_v2 VALUES('ses_ok', 'p', 'ses_p', 's', '/w', '', '2.3.0', 'general', 1200, 4000, 1200, 4000, 'succeeded', NULL);
             INSERT INTO session_v2 VALUES('ses_bad', 'p', 'ses_p', 's', '/w', 'Map the bank', '2.3.0', 'general', 1300, 4100, 1300, 4100, 'failed', NULL);
             INSERT INTO session_v2 VALUES('ses_cut', 'p', 'ses_p', 's', '/w', 'Weigh one nest', '2.3.0', 'general', 1400, 4200, 1400, 4200, 'succeeded', NULL);
             INSERT INTO session_message VALUES('m1', 'ses_busy', 'assistant', 1, 1100, 1500, '{\"time\":{\"completed\":1500},\"content\":[{\"type\":\"tool\",\"name\":\"shell\",\"state\":{\"status\":\"completed\",\"input\":{\"command\":\"ls nests\"}}}]}');
             INSERT INTO session_message VALUES('m2', 'ses_busy', 'assistant', 2, 1600, 1700, '{\"content\":[{\"type\":\"tool\",\"name\":\"shell\",\"state\":{\"status\":\"running\",\"input\":{\"command\":\"wc -l nests.csv\"}}}]}');
             INSERT INTO job VALUES('n1', 'j1', 'ses_p', 'ses_ok', 'task', 'Ring the chicks', 'completed', 'Rang nine.\nNone lost.', NULL, 'none', 1200, 4000);
             INSERT INTO job VALUES('n2', 'j2', 'ses_p', 'ses_cut', 'task', 'x', 'cancelled', NULL, NULL, 'none', 1400, 4200);
             INSERT INTO job VALUES('n3', 'j3', 'ses_p', NULL, 'shell', 'a command', 'running', NULL, NULL, 'none', 1500, 4300);",
        )
        .unwrap();
        let kids = opencode_children(&db, "ses_p");
        assert_eq!(
            kids.iter()
                .map(|c| (
                    c.id.as_str(),
                    c.name.as_str(),
                    c.title.as_str(),
                    c.running,
                    c.ended,
                    c.said.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "ses_cut",
                    "general",
                    "Weigh one nest",
                    Some(false),
                    Some("stopped"),
                    None
                ),
                (
                    "ses_bad",
                    "general",
                    "Map the bank",
                    Some(false),
                    Some("failed"),
                    None
                ),
                (
                    "ses_ok",
                    "general",
                    "Ring the chicks",
                    Some(false),
                    None,
                    Some("Rang nine. None lost.")
                ),
                (
                    "ses_busy",
                    "explore",
                    "Count the nests",
                    Some(true),
                    None,
                    None
                ),
            ]
        );
        // What the one at work is doing now: its newest message, settled
        // or not.
        let tail = opencode_tail(&db, "ses_busy", 6);
        assert_eq!(
            crate::conversation::latest_step(
                crate::conversation::Dialect::OpenCode,
                tail.iter().map(|l| l.as_str())
            )
            .0
            .as_deref(),
            Some("shell · wc -l nests.csv")
        );

        let agy = dir.path();
        let conn = Connection::open(agy.join("conversation_summaries.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversation_summaries (conversation_id text, title text NOT NULL DEFAULT '', step_count integer NOT NULL DEFAULT 0,
                workspace_uris text NOT NULL, status text NOT NULL DEFAULT '', agent_name text NOT NULL DEFAULT '',
                parent_conversation_id text NOT NULL DEFAULT '', nesting_depth integer NOT NULL DEFAULT 0,
                killed numeric NOT NULL DEFAULT false, PRIMARY KEY (conversation_id));
             INSERT INTO conversation_summaries VALUES('aaaa-1', 'Reeds', 9, '[]', 'CASCADE_RUN_STATUS_RUNNING', '', '', 0, false);
             INSERT INTO conversation_summaries VALUES('bbbb-2', '', 4, '[]', 'CASCADE_RUN_STATUS_RUNNING', 'browser_agent', 'aaaa-1', 1, false);
             INSERT INTO conversation_summaries VALUES('cccc-3', '', 12, '[]', 'CASCADE_RUN_STATUS_IDLE', '', 'aaaa-1', 1, false);
             INSERT INTO conversation_summaries VALUES('dddd-4', '', 2, '[]', 'CASCADE_RUN_STATUS_RUNNING', 'browser_agent', 'aaaa-1', 1, true);
             INSERT INTO conversation_summaries VALUES('eeee-5', '', 2, '[]', '', 'browser_agent', 'aaaa-1', 1, false);",
        )
        .unwrap();
        for (n, id) in ["bbbb-2", "cccc-3", "dddd-4", "eeee-5"].iter().enumerate() {
            let logs = agy.join("brain").join(id).join(".system_generated/logs");
            std::fs::create_dir_all(&logs).unwrap();
            std::fs::write(
                logs.join("transcript_full.jsonl"),
                format!("{}\n", json!({"step_index": 0, "source": "SYSTEM", "type": "SYSTEM_MESSAGE", "status": "DONE", "created_at": format!("2026-11-20T10:00:0{n}Z")})),
            )
            .unwrap();
        }
        let about = agy.join("brain/aaaa-1/.system_generated/subagents");
        std::fs::create_dir_all(&about).unwrap();
        std::fs::write(
            about.join("cccc-3.json"),
            json!({"conversationId": "cccc-3", "subagentDescriptor": {"typeName": "research_agent", "role": "Reader of the survey"},
                "state": "SUBAGENT_STATE_ALIVE", "spawnStepIndex": 4, "workspaceUris": []})
            .to_string(),
        )
        .unwrap();
        let kids = antigravity_children(agy, "aaaa-1");
        assert_eq!(
            kids.iter()
                .map(|c| (
                    c.id.as_str(),
                    c.name.as_str(),
                    c.title.as_str(),
                    c.running,
                    c.ended
                ))
                .collect::<Vec<_>>(),
            vec![
                ("eeee-5", "browser_agent", "", None, None),
                ("dddd-4", "browser_agent", "", Some(false), Some("stopped")),
                (
                    "cccc-3",
                    "research_agent",
                    "Reader of the survey",
                    Some(false),
                    None
                ),
                ("bbbb-2", "browser_agent", "", Some(true), None),
            ]
        );
    }

    /// The same report, for Antigravity: a conversation is tied to a folder
    /// by the program's own summaries, and to a start by its transcript's
    /// first line. An id that is not a plain name is never made into a path.
    #[test]
    fn antigravity_conversations_are_listed_with_their_folder_and_start() {
        let dir = tempfile::tempdir().unwrap();
        let agy = dir.path();
        let conn = Connection::open(agy.join("conversation_summaries.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversation_summaries (conversation_id text PRIMARY KEY, workspace_uris text NOT NULL,
                parent_conversation_id text NOT NULL DEFAULT '');
             INSERT INTO conversation_summaries VALUES('aaaa-1', '[\"file:///w/my%20reeds\"]', '');
             INSERT INTO conversation_summaries VALUES('bbbb-2', '[\"file:///w/other\"]', 'aaaa-1');
             INSERT INTO conversation_summaries VALUES('cccc-3', '[\"file:///w/none\"]', '');",
        )
        .unwrap();
        for id in ["aaaa-1", "bbbb-2"] {
            let logs = agy.join("brain").join(id).join(".system_generated/logs");
            std::fs::create_dir_all(&logs).unwrap();
            let line = json!({"step_index": 0, "type": "USER_INPUT", "created_at": "2026-11-16T12:00:05Z", "content": "x"});
            std::fs::write(logs.join("transcript.jsonl"), format!("{line}\n")).unwrap();
        }
        let at = ts_ms("2026-11-16T12:00:05Z").unwrap();
        let got = antigravity_since(agy, at);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            (
                got[0].id.as_str(),
                got[0].folders.clone(),
                got[0].started_ms
            ),
            ("aaaa-1", vec!["/w/my reeds".to_string()], at)
        );
        assert!(antigravity_since(agy, at + 1).is_empty());
        // The whole transcript is preferred to the cut one, where it is.
        let logs = agy.join("brain/aaaa-1/.system_generated/logs");
        std::fs::write(logs.join("transcript_full.jsonl"), "").unwrap();
        assert_eq!(
            antigravity_transcript(agy, "aaaa-1").unwrap(),
            logs.join("transcript_full.jsonl")
        );
        assert_eq!(antigravity_transcript(agy, "../aaaa-1"), None);
    }
}
