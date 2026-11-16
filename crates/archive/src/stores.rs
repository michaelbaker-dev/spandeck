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
