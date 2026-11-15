//! Token usage, read from the same session files the archive follows.
//!
//! Nothing here is estimated: every number is one the agent wrote down.
//!
//! - Claude Code (`<session>.jsonl`): each assistant line carries
//!   `message.model` and `message.usage`. One model call is written as several
//!   lines (one per content block) that share `message.id` and repeat the
//!   usage, sometimes with a larger output count on the later line, so a line
//!   with the id of the line before it replaces that line's numbers instead of
//!   adding to them. Sub-agents write their own files under
//!   `<session>/subagents/`; they count toward the totals, under the model
//!   that ran them, but never toward the context gauge.
//! - Codex (`rollout-*.jsonl`): `token_count` events carry a running total
//!   for the conversation, so what is added is the difference from the last
//!   total seen. `token_usage_record` lines repeat the same usage and are not
//!   read. The model comes from `turn_context`, the provider from
//!   `session_meta`, the context window from the event itself.
//! - OpenCode keeps a running total per session in its own database
//!   (`session_v2`); it is read like Codex's running total. No per-call
//!   figure is kept there, so there is no context gauge for it.
//!
//! Totals are kept per session, model and hour, so "today" can be answered in
//! any time zone without reading a file again.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::Archive;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

impl Tally {
    pub fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
    pub fn add(&mut self, o: &Tally) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
    }
    fn minus(&self, o: &Tally) -> Tally {
        Tally {
            input: self.input - o.input,
            output: self.output - o.output,
            cache_read: self.cache_read - o.cache_read,
            cache_write: self.cache_write - o.cache_write,
        }
    }
    fn is_zero(&self) -> bool {
        *self == Tally::default()
    }
    fn any_negative(&self) -> bool {
        self.input < 0 || self.output < 0 || self.cache_read < 0 || self.cache_write < 0
    }
}

/// One model's share of a conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUse {
    /// The agent you talk to.
    pub main: Tally,
    /// Its sub-agents.
    pub side: Tally,
    /// Prompts this model was the first to answer.
    pub turns: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct FileState {
    offset: u64,
    last_id: Option<String>,
    last: Tally,
    last_hour: i64,
    last_model: String,
}

/// What is known about one conversation, and what the reader needs to carry
/// on from where it stopped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    /// Who serves the model, when the session data says or implies it.
    pub provider: Option<String>,
    /// The model in use now: the one behind the latest main-chain reply.
    pub model: Option<String>,
    /// What the latest model call had in its context.
    pub context_tokens: Option<u64>,
    /// Only when the session data states it.
    pub context_window: Option<u64>,
    /// Prompts no model has answered yet.
    pub pending_turns: u64,
    /// False for an agent that does not record them.
    pub counts_turns: bool,
    pub updated_ms: Option<u64>,
    /// A cost figure the agent itself wrote down, if it did.
    pub reported_cost_usd: Option<f64>,
    pub by_model: BTreeMap<String, ModelUse>,
    files: BTreeMap<String, FileState>,
    running: Option<Tally>,
}

/// A change to the hourly table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub hour: i64,
    pub model: String,
    pub side: bool,
    pub tally: Tally,
    pub turns: i64,
}

/// "2026-11-14T09:30:00.123Z" as milliseconds. Session files write UTC.
pub fn iso_ms(ts: &str) -> Option<u64> {
    let b = ts.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let ms = if b.get(19) == Some(&b'.') {
        let digits: String = ts[20..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .take(3)
            .collect();
        format!("{digits:0<3}").parse::<i64>().unwrap_or(0)
    } else {
        0
    };
    // Days since 1970-01-01 (Howard Hinnant's civil-date arithmetic).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + hh * 3600 + mm * 60 + ss;
    u64::try_from(secs * 1000 + ms).ok()
}

const HOUR_MS: u64 = 3_600_000;
const OPENCODE_RECHECK_MS: u64 = 15_000;

#[derive(Deserialize, Default)]
#[serde(default)]
struct ClaudeUsage {
    input_tokens: i64,
    output_tokens: i64,
    cache_creation_input_tokens: i64,
    cache_read_input_tokens: i64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ClaudeMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CompactMeta {
    #[serde(rename = "postTokens")]
    post_tokens: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ClaudeLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    subtype: Option<String>,
    timestamp: Option<String>,
    is_sidechain: bool,
    is_meta: bool,
    is_compact_summary: bool,
    tool_use_result: Option<IgnoredAny>,
    #[serde(rename = "sourceToolAssistantUUID")]
    source_tool_assistant_uuid: Option<IgnoredAny>,
    message: Option<ClaudeMessage>,
    compact_metadata: Option<CompactMeta>,
    #[serde(rename = "totalCostUSD")]
    total_cost_usd: Option<f64>,
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(default)]
struct CodexUsage {
    input_tokens: i64,
    cached_input_tokens: i64,
    cache_write_input_tokens: i64,
    output_tokens: i64,
    total_tokens: i64,
}

impl CodexUsage {
    /// Codex counts cached input inside `input_tokens`; here they are apart.
    fn tally(&self) -> Tally {
        Tally {
            input: (self.input_tokens - self.cached_input_tokens - self.cache_write_input_tokens)
                .max(0),
            output: self.output_tokens,
            cache_read: self.cached_input_tokens,
            cache_write: self.cache_write_input_tokens,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CodexInfo {
    total_token_usage: Option<CodexUsage>,
    last_token_usage: Option<CodexUsage>,
    model_context_window: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CodexPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
    model_provider: Option<String>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CodexLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    payload: Option<CodexPayload>,
}

impl Usage {
    /// Prompts so far: those answered, plus any still waiting for a reply.
    pub fn turns(&self) -> Option<u64> {
        self.counts_turns
            .then(|| self.by_model.values().map(|m| m.turns).sum::<u64>() + self.pending_turns)
    }

    fn touch(&mut self, ts: u64) {
        self.updated_ms = Some(self.updated_ms.unwrap_or(0).max(ts));
    }

    pub fn total(&self) -> Tally {
        let mut t = Tally::default();
        for m in self.by_model.values() {
            t.add(&m.main);
            t.add(&m.side);
        }
        t
    }

    fn apply(&mut self, out: &mut Vec<Delta>, hour: i64, model: &str, side: bool, tally: Tally) {
        let m = self.by_model.entry(model.to_string()).or_default();
        let mut turns = 0;
        if side {
            m.side.add(&tally);
        } else {
            m.main.add(&tally);
            turns = std::mem::take(&mut self.pending_turns);
            m.turns += turns;
        }
        if tally.is_zero() && turns == 0 {
            return;
        }
        out.push(Delta {
            hour,
            model: model.to_string(),
            side,
            tally,
            turns: turns as i64,
        });
    }

    /// One line of a session file. `agent` is the pane's agent ("claude",
    /// "codex"); `file` names the file within the session ("" for the main
    /// one) and `side_file` says it is a sub-agent's.
    pub fn feed(
        &mut self,
        agent: &str,
        file: &str,
        side_file: bool,
        line: &str,
        now_ms: u64,
        out: &mut Vec<Delta>,
    ) {
        match agent {
            "codex" => self.feed_codex(line, now_ms, out),
            "claude" | "fake" => self.feed_claude(file, side_file, line, now_ms, out),
            _ => {}
        }
    }

    fn feed_claude(
        &mut self,
        file: &str,
        side_file: bool,
        line: &str,
        now_ms: u64,
        out: &mut Vec<Delta>,
    ) {
        let Ok(l) = serde_json::from_str::<ClaudeLine>(line) else {
            return;
        };
        self.counts_turns = true;
        let ts = l.timestamp.as_deref().and_then(iso_ms).unwrap_or(now_ms);
        let side = side_file || l.is_sidechain;
        match (l.kind.as_deref(), l.subtype.as_deref()) {
            (Some("assistant"), _) => {
                let Some(msg) = l.message else { return };
                let (Some(model), Some(u)) = (msg.model, msg.usage) else {
                    return;
                };
                // Claude Code's own placeholder replies: no model was called.
                if model.starts_with('<') {
                    return;
                }
                let tally = Tally {
                    input: u.input_tokens,
                    output: u.output_tokens,
                    cache_read: u.cache_read_input_tokens,
                    cache_write: u.cache_creation_input_tokens,
                };
                let hour = (ts / HOUR_MS) as i64;
                let fs = self.files.entry(file.to_string()).or_default();
                if msg.id.is_some() && fs.last_id == msg.id {
                    // The same model call, written again: its numbers replace
                    // the earlier line's.
                    let change = tally.minus(&fs.last);
                    fs.last = tally;
                    let (h, m) = (fs.last_hour, fs.last_model.clone());
                    let pending = std::mem::take(&mut self.pending_turns);
                    self.apply(out, h, &m, side, change);
                    self.pending_turns = pending;
                } else {
                    fs.last_id = msg.id;
                    fs.last = tally;
                    fs.last_hour = hour;
                    fs.last_model = model.clone();
                    self.apply(out, hour, &model, side, tally);
                }
                if !side {
                    self.provider.get_or_insert_with(|| "anthropic".into());
                    self.model = Some(model);
                    self.context_tokens =
                        Some((tally.input + tally.cache_read + tally.cache_write).max(0) as u64);
                }
                self.touch(ts);
            }
            (Some("user"), _)
                if !side
                    && !l.is_meta
                    && !l.is_compact_summary
                    && l.tool_use_result.is_none()
                    && l.source_tool_assistant_uuid.is_none() =>
            {
                self.pending_turns += 1;
                self.touch(ts);
            }
            (Some("system"), Some("compact_boundary")) if !side => {
                if let Some(post) = l.compact_metadata.and_then(|m| m.post_tokens) {
                    self.context_tokens = Some(post);
                }
            }
            (Some("cost-state"), _) => {
                if let Some(cost) = l.total_cost_usd {
                    self.reported_cost_usd = Some(cost);
                }
            }
            _ => {}
        }
    }

    fn feed_codex(&mut self, line: &str, now_ms: u64, out: &mut Vec<Delta>) {
        let Ok(l) = serde_json::from_str::<CodexLine>(line) else {
            return;
        };
        let Some(p) = l.payload else { return };
        self.counts_turns = true;
        let ts = l.timestamp.as_deref().and_then(iso_ms).unwrap_or(now_ms);
        match (l.kind.as_deref(), p.kind.as_deref()) {
            (Some("session_meta"), _) => {
                if p.model_provider.is_some() {
                    self.provider = p.model_provider;
                }
                if p.model.is_some() {
                    self.model = p.model;
                }
            }
            (Some("turn_context"), _) => {
                if p.model.is_some() {
                    self.model = p.model;
                }
            }
            (Some("event_msg"), Some("task_started")) => {
                self.pending_turns += 1;
                self.touch(ts);
            }
            (Some("event_msg"), Some("token_count")) => {
                let Some(info) = p.info else { return };
                if let Some(w) = info.model_context_window {
                    self.context_window = Some(w);
                }
                if let Some(last) = info.last_token_usage {
                    self.context_tokens = Some(last.total_tokens.max(0) as u64);
                }
                let Some(total) = info.total_token_usage else {
                    return;
                };
                let last = info.last_token_usage.map(|u| u.tally());
                self.running_total(total.tally(), last, ts, out);
            }
            _ => {}
        }
    }

    /// A running total for the whole conversation: add what is new since the
    /// last one. If the total went down (the agent started counting again),
    /// add only the latest call, when the agent says what that was.
    fn running_total(&mut self, total: Tally, last: Option<Tally>, ts: u64, out: &mut Vec<Delta>) {
        let change = match self.running {
            None => total,
            Some(before) => {
                let d = total.minus(&before);
                if d.any_negative() {
                    last.unwrap_or_default()
                } else {
                    d
                }
            }
        };
        self.running = Some(total);
        if change.is_zero() {
            return;
        }
        let model = self.model.clone().unwrap_or_else(|| "unknown".into());
        self.apply(out, (ts / HOUR_MS) as i64, &model, false, change);
        self.touch(ts);
    }

    /// OpenCode's per-session totals, as read from its database.
    pub fn feed_snapshot(&mut self, snap: &OpenCodeSession, now_ms: u64, out: &mut Vec<Delta>) {
        if snap.model.is_some() {
            self.model = snap.model.clone();
            self.provider = snap.provider.clone();
        }
        if snap.cost > 0.0 {
            self.reported_cost_usd = Some(snap.cost);
        }
        self.running_total(snap.total, None, snap.updated_ms.unwrap_or(now_ms), out);
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenCodeSession {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub total: Tally,
    pub cost: f64,
    pub updated_ms: Option<u64>,
}

/// Where OpenCode keeps its sessions.
pub fn opencode_db(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"))
        .join("opencode/opencode.db")
}

/// One session's totals from OpenCode's database, opened read-only. `None`
/// when the file, the table or the row is not there: OpenCode's schema is its
/// own business and may change.
pub fn opencode_session(db: &Path, session: &str) -> Option<OpenCodeSession> {
    if !db.exists() {
        return None;
    }
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_millis(200));
    conn.query_row(
        "SELECT model, tokens_input, tokens_output, tokens_cache_read, tokens_cache_write, cost, time_updated
         FROM session_v2 WHERE id=?1",
        [session],
        |r| {
            let model: Option<String> = r.get(0)?;
            let model: Option<serde_json::Value> =
                model.and_then(|m| serde_json::from_str(&m).ok());
            let field = |k: &str| {
                model
                    .as_ref()
                    .and_then(|m| m.get(k))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            };
            Ok(OpenCodeSession {
                model: field("id"),
                provider: field("providerID"),
                total: Tally {
                    input: r.get(1)?,
                    output: r.get(2)?,
                    cache_read: r.get(3)?,
                    cache_write: r.get(4)?,
                },
                cost: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                updated_ms: r.get::<_, Option<i64>>(6)?.map(|t| t.max(0) as u64),
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

/// OpenCode's most recently used conversation whose folder `in_folder`
/// accepts and that `taken` does not claim: its id and when it was last
/// used. Not a conversation OpenCode started itself (a child), not an
/// archived one, and not one nothing was ever said in. `None` as for
/// `opencode_session`. Columns seen on OpenCode 2.3.0 (15 Nov 2026).
pub fn opencode_last(
    db: &Path,
    in_folder: &dyn Fn(&str) -> bool,
    taken: &dyn Fn(&str) -> bool,
) -> Option<(String, u64)> {
    if !db.exists() {
        return None;
    }
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_millis(200));
    let mut stmt = conn
        .prepare(
            "SELECT id, directory, time_updated FROM session_v2
             WHERE parent_id IS NULL AND time_archived IS NULL
               AND tokens_input + tokens_output > 0
             ORDER BY time_updated DESC",
        )
        .ok()?;
    let mut rows = stmt.query([]).ok()?;
    while let Ok(Some(r)) = rows.next() {
        let (Ok(id), Ok(dir), Ok(at)) = (
            r.get::<_, String>(0),
            r.get::<_, String>(1),
            r.get::<_, i64>(2),
        ) else {
            continue;
        };
        if in_folder(&dir) && !taken(&id) {
            return Some((id, at.max(0) as u64));
        }
    }
    None
}

/// A followed conversation.
#[derive(Debug, Clone)]
pub(crate) struct Tracked {
    agent: String,
    path: String,
    usage: Usage,
    /// OpenCode only: its database as last seen (length and modified time).
    stamp: Option<(u64, u64, std::time::SystemTime)>,
    read_ms: u64,
}

/// Totals for one session and model, for the station summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRow {
    pub session: String,
    pub agent: String,
    pub pane: Option<String>,
    pub project: Option<String>,
    pub provider: Option<String>,
    pub model: String,
    pub side: bool,
    pub today: Tally,
    pub today_turns: i64,
    pub all: Tally,
    pub all_turns: i64,
}

pub(crate) const SCHEMA: &str = r#"
    CREATE TABLE IF NOT EXISTS usage_sessions (
        session TEXT PRIMARY KEY, agent TEXT NOT NULL, path TEXT NOT NULL,
        project TEXT, pane TEXT, provider TEXT, state TEXT NOT NULL DEFAULT '{}'
    );
    CREATE TABLE IF NOT EXISTS usage_hours (
        session TEXT NOT NULL, hour INTEGER NOT NULL, model TEXT NOT NULL, side INTEGER NOT NULL,
        input INTEGER NOT NULL DEFAULT 0, output INTEGER NOT NULL DEFAULT 0,
        cache_read INTEGER NOT NULL DEFAULT 0, cache_write INTEGER NOT NULL DEFAULT 0,
        turns INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (session, hour, model, side)
    ) WITHOUT ROWID;
"#;

/// Read complete new lines of `path` from `offset`; returns the new offset.
fn read_lines(path: &Path, offset: u64, mut each: impl FnMut(&str)) -> u64 {
    let Ok(mut file) = File::open(path) else {
        return offset;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len <= offset || file.seek(SeekFrom::Start(offset)).is_err() {
        return offset;
    }
    let mut reader = BufReader::with_capacity(1 << 16, file);
    let mut at = offset;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(n) if n > 0 && buf.last() == Some(&b'\n') => {
                at += n as u64;
                if let Ok(line) = std::str::from_utf8(&buf[..n - 1]) {
                    if !line.is_empty() {
                        each(line);
                    }
                }
            }
            // The end, or a line still being written: it waits for next time.
            _ => return at,
        }
    }
}

fn subagent_files(main: &str) -> Vec<(String, PathBuf)> {
    let Some(stem) = main.strip_suffix(".jsonl") else {
        return vec![];
    };
    let Ok(dir) = std::fs::read_dir(Path::new(stem).join("subagents")) else {
        return vec![];
    };
    let mut files: Vec<(String, PathBuf)> = dir
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.ends_with(".jsonl").then(|| (name, e.path()))
        })
        .collect();
    files.sort();
    files
}

/// Bring one conversation's usage up to date. Returns the hourly changes.
fn advance(t: &mut Tracked, session: &str, now_ms: u64) -> Vec<Delta> {
    let mut out = vec![];
    if t.agent == "opencode" {
        let db = Path::new(&t.path);
        let wal = PathBuf::from(format!("{}-wal", t.path));
        let stamp = |p: &Path| {
            p.metadata()
                .ok()
                .map(|m| (m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)))
        };
        let (Some((len, _)), wal_stamp) = (stamp(db), stamp(&wal)) else {
            return out;
        };
        let (wal_len, when) = wal_stamp.unwrap_or((0, std::time::UNIX_EPOCH));
        let seen = Some((len, wal_len, when));
        // Looked at again now and then even when it seems untouched: a write
        // in the same instant as the last read leaves the same stamp.
        if t.stamp == seen && now_ms.saturating_sub(t.read_ms) < OPENCODE_RECHECK_MS {
            return out;
        }
        t.stamp = seen;
        t.read_ms = now_ms;
        if let Some(snap) = opencode_session(db, session) {
            t.usage.feed_snapshot(&snap, now_ms, &mut out);
        }
        return out;
    }
    let mut files = vec![(String::new(), PathBuf::from(&t.path), false)];
    if t.agent != "codex" {
        files.extend(
            subagent_files(&t.path)
                .into_iter()
                .map(|(name, path)| (name, path, true)),
        );
    }
    for (key, path, side) in files {
        let offset = t.usage.files.get(&key).map(|f| f.offset).unwrap_or(0);
        let (agent, usage) = (&t.agent, &mut t.usage);
        let end = read_lines(&path, offset, |line| {
            usage.feed(agent, &key, side, line, now_ms, &mut out)
        });
        if end != offset {
            t.usage.files.entry(key).or_default().offset = end;
        }
    }
    out
}

impl Archive {
    /// Start (or keep) counting a conversation's tokens. `path` is its
    /// session file, or for OpenCode its database.
    pub fn track_usage(
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
                "INSERT INTO usage_sessions(session,agent,path,project,pane) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(session) DO UPDATE SET path=excluded.path,
                   project=COALESCE(excluded.project, project), pane=COALESCE(excluded.pane, pane)",
                params![id, agent, path, project, pane],
            );
        }
        if let Some(t) = self.usage.lock().unwrap().get_mut(id) {
            t.path = path.to_string();
            return;
        }
        self.load_usage(id);
    }

    fn load_usage(&self, id: &str) -> Option<Usage> {
        let row: Option<(String, String, String)> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT agent, path, state FROM usage_sessions WHERE session=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten()
        };
        let (agent, path, state) = row?;
        let usage: Usage = serde_json::from_str(&state).unwrap_or_default();
        let mut cache = self.usage.lock().unwrap();
        let t = cache.entry(id.to_string()).or_insert(Tracked {
            agent,
            path,
            usage,
            stamp: None,
            read_ms: 0,
        });
        Some(t.usage.clone())
    }

    /// What is known about a conversation's usage. Cheap: kept in memory for
    /// every conversation asked about since the daemon started.
    pub fn usage(&self, session: &str) -> Option<Usage> {
        if let Some(t) = self.usage.lock().unwrap().get(session) {
            return Some(t.usage.clone());
        }
        self.load_usage(session)
    }

    /// Read what is new in every conversation being counted. Returns the
    /// sessions whose numbers changed.
    pub fn poll_usage(&self, now_ms: u64) -> Vec<String> {
        let work: Vec<(String, Tracked)> = self
            .usage
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut changed = vec![];
        for (id, mut t) in work {
            let before = t.usage.clone();
            let deltas = advance(&mut t, &id, now_ms);
            if t.usage != before {
                self.save_usage(&id, &t.usage, &deltas);
                // Only the reader's place in the file moved: nothing to show.
                let (mut a, mut b) = (before, t.usage.clone());
                (a.files, b.files) = Default::default();
                if a != b {
                    changed.push(id.clone());
                }
            }
            if let Some(slot) = self.usage.lock().unwrap().get_mut(&id) {
                slot.usage = t.usage;
                slot.stamp = t.stamp;
                slot.read_ms = t.read_ms;
            }
        }
        changed
    }

    fn save_usage(&self, id: &str, usage: &Usage, deltas: &[Delta]) {
        let mut conn = self.conn.lock().unwrap();
        let Ok(tx) = conn.transaction() else { return };
        for d in deltas {
            let _ = tx.execute(
                "INSERT INTO usage_hours(session,hour,model,side,input,output,cache_read,cache_write,turns)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT(session,hour,model,side) DO UPDATE SET input=input+excluded.input,
                   output=output+excluded.output, cache_read=cache_read+excluded.cache_read,
                   cache_write=cache_write+excluded.cache_write, turns=turns+excluded.turns",
                params![
                    id,
                    d.hour,
                    d.model,
                    d.side as i64,
                    d.tally.input,
                    d.tally.output,
                    d.tally.cache_read,
                    d.tally.cache_write,
                    d.turns
                ],
            );
        }
        let _ = tx.execute(
            "UPDATE usage_sessions SET state=?2, provider=?3 WHERE session=?1",
            params![
                id,
                serde_json::to_string(usage).unwrap_or_else(|_| "{}".into()),
                usage.provider
            ],
        );
        let _ = tx.commit();
    }

    /// Every conversation's totals per model, for all time and since
    /// `today_from_ms` (rounded down to the hour).
    pub fn usage_rows(&self, today_from_ms: u64) -> Vec<UsageRow> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT h.session, s.agent, s.pane, s.project, s.provider, h.model, h.side,
                    SUM(CASE WHEN h.hour >= ?1 THEN h.input ELSE 0 END),
                    SUM(CASE WHEN h.hour >= ?1 THEN h.output ELSE 0 END),
                    SUM(CASE WHEN h.hour >= ?1 THEN h.cache_read ELSE 0 END),
                    SUM(CASE WHEN h.hour >= ?1 THEN h.cache_write ELSE 0 END),
                    SUM(CASE WHEN h.hour >= ?1 THEN h.turns ELSE 0 END),
                    SUM(h.input), SUM(h.output), SUM(h.cache_read), SUM(h.cache_write), SUM(h.turns)
             FROM usage_hours h JOIN usage_sessions s ON s.session = h.session
             GROUP BY h.session, h.model, h.side ORDER BY s.rowid, h.model, h.side",
        ) else {
            return vec![];
        };
        stmt.query_map([(today_from_ms / HOUR_MS) as i64], |r| {
            Ok(UsageRow {
                session: r.get(0)?,
                agent: r.get(1)?,
                pane: r.get(2)?,
                project: r.get(3)?,
                provider: r.get(4)?,
                model: r.get(5)?,
                side: r.get::<_, i64>(6)? != 0,
                today: Tally {
                    input: r.get(7)?,
                    output: r.get(8)?,
                    cache_read: r.get(9)?,
                    cache_write: r.get(10)?,
                },
                today_turns: r.get(11)?,
                all: Tally {
                    input: r.get(12)?,
                    output: r.get(13)?,
                    cache_read: r.get(14)?,
                    cache_write: r.get(15)?,
                },
                all_turns: r.get(16)?,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &str = include_str!("../fixtures/usage/claude.jsonl");
    const SUBAGENT: &str = include_str!("../fixtures/usage/claude-subagent.jsonl");
    const COMPACTION: &str = include_str!("../fixtures/usage/claude-compaction.jsonl");
    const CODEX: &str = include_str!("../fixtures/usage/codex.jsonl");

    fn tally(input: i64, output: i64, cache_read: i64, cache_write: i64) -> Tally {
        Tally {
            input,
            output,
            cache_read,
            cache_write,
        }
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    /// The acceptance criterion for Claude Code: totals follow the file as it
    /// grows, a model call written as several lines is counted once (with the
    /// last line's numbers), sub-agents count toward the totals under their
    /// own model but not toward the context, and it all survives a restart.
    #[test]
    fn claude_usage_follows_the_file_counts_each_call_once_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("archive.db");
        let file = dir.path().join("s1.jsonl");
        let path = file.display().to_string();
        // Stop in the middle of the line that repeats msg_01 with more output.
        let cut = CLAUDE.match_indices('\n').nth(2).unwrap().0 + 40;
        append(&file, &CLAUDE[..cut]);

        let a = Archive::open(&db).unwrap();
        a.follow("s1", "claude", &path, Some("proj"), Some("p-1"));
        assert_eq!(a.poll_usage(0), vec!["s1".to_string()]);
        let u = a.usage("s1").unwrap();
        assert_eq!(u.by_model["claude-opus-5-5"].main, tally(4, 10, 1000, 200));
        assert_eq!(u.context_tokens, Some(1204));
        assert_eq!(u.turns(), Some(1));
        assert!(a.poll_usage(0).is_empty(), "nothing new, nothing changed");

        append(&file, &CLAUDE[cut..]);
        let sub = dir.path().join("s1/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        append(&sub.join("agent-a1.jsonl"), SUBAGENT);
        assert_eq!(a.poll_usage(0).len(), 1);
        let check = |u: &Usage| {
            // msg_01: 4/120/1000/200 (the later line), msg_02 once: 6/50/1200/30.
            assert_eq!(
                u.by_model["claude-opus-5-5"],
                ModelUse {
                    main: tally(10, 170, 2200, 230),
                    side: Tally::default(),
                    turns: 1
                }
            );
            assert_eq!(
                u.by_model["claude-haiku-5-5"],
                ModelUse {
                    main: tally(3, 20, 500, 0),
                    side: tally(2, 40, 100, 50),
                    turns: 1
                }
            );
            assert_eq!(u.by_model.len(), 2, "the placeholder reply is no model");
            assert_eq!(u.model.as_deref(), Some("claude-haiku-5-5"));
            assert_eq!(u.provider.as_deref(), Some("anthropic"));
            assert_eq!(u.context_tokens, Some(503), "the sub-agent's is not it");
            assert_eq!(u.context_window, None, "the file does not state one");
            assert_eq!(u.turns(), Some(2), "tool results and notes are not prompts");
            assert_eq!(u.reported_cost_usd, Some(0.42));
            assert_eq!(u.updated_ms, iso_ms("2026-11-14T00:10:04.000Z"));
        };
        check(&a.usage("s1").unwrap());
        drop(a);

        let a = Archive::open(&db).unwrap();
        check(&a.usage("s1").unwrap());
        assert!(a.poll_usage(0).is_empty(), "nothing is read twice");
        // Today began at midnight UTC on the 14th: the first turn was before.
        let rows = a.usage_rows(iso_ms("2026-11-14T00:00:00Z").unwrap());
        let row = |model: &str, side: bool| {
            rows.iter()
                .find(|r| r.model == model && r.side == side)
                .unwrap()
        };
        let opus = row("claude-opus-5-5", false);
        assert_eq!((opus.today, opus.today_turns), (Tally::default(), 0));
        assert_eq!((opus.all, opus.all_turns), (tally(10, 170, 2200, 230), 1));
        assert_eq!(opus.project.as_deref(), Some("proj"));
        assert_eq!(opus.pane.as_deref(), Some("p-1"));
        assert_eq!(row("claude-haiku-5-5", false).today, tally(3, 20, 500, 0));
        assert_eq!(row("claude-haiku-5-5", true).today, tally(2, 40, 100, 50));
        assert_eq!(rows.len(), 3);

        // After compaction the context is what the file says is left.
        append(&file, COMPACTION);
        a.poll_usage(0);
        let u = a.usage("s1").unwrap();
        assert_eq!(u.context_tokens, Some(900));
        assert_eq!(u.turns(), Some(2), "the summary is not a prompt");
    }

    /// Codex writes a running total: adding the events up, or reading the
    /// `token_usage_record` lines as well, would count everything twice.
    #[test]
    fn codex_usage_is_the_growth_of_its_running_total_under_the_model_of_each_turn() {
        let mut u = Usage::default();
        let mut out = vec![];
        for line in CODEX.lines() {
            u.feed("codex", "", false, line, 0, &mut out);
        }
        assert_eq!(u.provider.as_deref(), Some("openai"));
        assert_eq!(u.model.as_deref(), Some("gpt-6-astra"));
        // Cached input is inside Codex's input_tokens: 17917 - 11904 = 6013.
        assert_eq!(
            u.by_model["gpt-6.1-sol"],
            ModelUse {
                main: tally(6013, 157, 11904, 0),
                side: Tally::default(),
                turns: 1
            }
        );
        assert_eq!(
            u.by_model["gpt-6-astra"],
            ModelUse {
                main: tally(3987, 243, 18096, 0),
                side: Tally::default(),
                turns: 1
            }
        );
        assert_eq!(u.total().total(), 40400, "Codex's own total_tokens");
        assert_eq!(u.context_tokens, Some(22326));
        assert_eq!(u.context_window, Some(475000));
        assert_eq!(u.turns(), Some(2));
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[1].hour,
            (iso_ms("2026-11-14T15:00:00Z").unwrap() / HOUR_MS) as i64
        );
    }

    /// OpenCode's schema is not ours: this pins the columns that are read.
    #[test]
    fn opencode_totals_are_read_from_its_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let oc = rusqlite::Connection::open(&db).unwrap();
        oc.execute_batch(
            r#"CREATE TABLE session_v2 (id text PRIMARY KEY, model text, cost real DEFAULT 0 NOT NULL,
                 tokens_input integer DEFAULT 0 NOT NULL, tokens_output integer DEFAULT 0 NOT NULL,
                 tokens_reasoning integer DEFAULT 0 NOT NULL, tokens_cache_read integer DEFAULT 0 NOT NULL,
                 tokens_cache_write integer DEFAULT 0 NOT NULL, time_updated integer NOT NULL);
               INSERT INTO session_v2 VALUES('ses_1', '{"id":"big-pickle","providerID":"opencode"}', 0.0,
                 8731, 872, 260, 63872, 0, 1794678815044);"#,
        )
        .unwrap();
        let a = Archive::memory().unwrap();
        a.track_usage(
            "ses_1",
            "opencode",
            &db.display().to_string(),
            None,
            Some("p-2"),
        );
        assert_eq!(a.poll_usage(0).len(), 1);
        let u = a.usage("ses_1").unwrap();
        assert_eq!(u.by_model["big-pickle"].main, tally(8731, 872, 63872, 0));
        assert_eq!(u.provider.as_deref(), Some("opencode"));
        assert_eq!((u.turns(), u.context_tokens), (None, None));
        assert_eq!(u.reported_cost_usd, None, "a cost of zero is not a cost");

        oc.execute(
            "UPDATE session_v2 SET tokens_output = 9000, cost = 0.5, model = '{\"id\":\"other\",\"providerID\":\"opencode\"}'",
            [],
        )
        .unwrap();
        a.poll_usage(60_000);
        let u = a.usage("ses_1").unwrap();
        assert_eq!(u.by_model["big-pickle"].main, tally(8731, 872, 63872, 0));
        assert_eq!(u.by_model["other"].main, tally(0, 8128, 0, 0));
        assert_eq!(u.reported_cost_usd, Some(0.5));
        assert_eq!(opencode_session(&db, "ses_none"), None);
        assert_eq!(
            opencode_session(&dir.path().join("absent.db"), "ses_1"),
            None
        );
    }

    /// The owner's request (15 Nov 2026): a new pane continues the last
    /// conversation in its folder, or starts a new one when there is none.
    /// OpenCode's schema is not ours: this pins the columns that are read.
    #[test]
    fn opencode_last_is_the_conversation_last_used_in_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let oc = rusqlite::Connection::open(&db).unwrap();
        oc.execute_batch(
            r#"CREATE TABLE session_v2 (id text PRIMARY KEY, parent_id text, directory text NOT NULL,
                 tokens_input integer DEFAULT 0 NOT NULL, tokens_output integer DEFAULT 0 NOT NULL,
                 time_updated integer NOT NULL, time_archived integer);
               INSERT INTO session_v2 VALUES('ses_old', NULL, '/w/repo', 5, 5, 100, NULL);
               INSERT INTO session_v2 VALUES('ses_recent', NULL, '/w/repo', 5, 5, 200, NULL);
               INSERT INTO session_v2 VALUES('ses_child', 'ses_recent', '/w/repo', 5, 5, 300, NULL);
               INSERT INTO session_v2 VALUES('ses_archived', NULL, '/w/repo', 5, 5, 400, 450);
               INSERT INTO session_v2 VALUES('ses_unused', NULL, '/w/repo', 0, 0, 500, NULL);
               INSERT INTO session_v2 VALUES('ses_elsewhere', NULL, '/w/other', 5, 5, 600, NULL);"#,
        )
        .unwrap();
        let here = |d: &str| d == "/w/repo";
        assert_eq!(
            opencode_last(&db, &here, &|_| false),
            Some(("ses_recent".to_string(), 200))
        );
        assert_eq!(
            opencode_last(&db, &here, &|id| id == "ses_recent"),
            Some(("ses_old".to_string(), 100))
        );
        assert_eq!(opencode_last(&db, &|_| false, &|_| false), None);
        assert_eq!(
            opencode_last(&dir.path().join("absent.db"), &here, &|_| false),
            None
        );
    }
}
