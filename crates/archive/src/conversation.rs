//! A session file as a conversation: ordered items for the Control Deck's
//! conversation view (DECISIONS.md, "The conversation view").
//!
//! Built from the raw lines the archive keeps, so it outlives the agent's
//! own file. Neither vendor's format is a contract: a record this code does
//! not know is skipped and counted, and the count is part of every answer,
//! so the view can say that it has stopped understanding.
//!
//! What is served from here is the raw history, unscrubbed. It is for the
//! owner's signed-in browsers only (`crates/core/src/conversation.rs`);
//! nothing here is reachable through `search`, which other stations and
//! agents use.
//!
//! Shapes were read from Claude Code 2.1.302 to 2.1.325 and Codex 0.169.0
//! session files (record types and key names only). `fixtures/` holds
//! made-up conversations in those shapes.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Bytes of text in one item; the rest is fetched as detail.
pub const TEXT_CAP: usize = 16 * 1024;
/// How the refusal of a plan the owner sent back from the Control Deck
/// begins (core, questions.rs says the rest).
pub const SENT_BACK: &str = "The owner read this plan in spandeck and did not approve it";
/// Characters of a tool call's one-line summary.
pub const SUMMARY_CAP: usize = 200;
/// Bytes of text in one detail answer; `next` says where the rest starts.
pub const DETAIL_CAP: usize = 64 * 1024;
/// Bytes of one input field in a detail answer.
pub const FIELD_CAP: usize = 8 * 1024;
/// Lines of diff in one detail answer.
pub const DIFF_LINES_CAP: usize = 2000;
/// Items in one page.
pub const PAGE_MAX: usize = 200;
pub const PAGE_DEFAULT: usize = 50;
/// Items indexed per conversation; past this the oldest half is let go.
const SLOTS_MAX: usize = 100_000;
/// Conversations indexed at once; the least recently asked for goes first.
const CONVS_MAX: usize = 8;
/// Tool calls waiting for a result that are remembered at once.
const OPEN_MAX: usize = 256;
/// Raw lines read from the database in one short call.
const CHUNK_LINES: u32 = 256;
const CHUNK_BYTES: usize = 8 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Claude,
    Codex,
}

impl Dialect {
    /// The agents whose session files can be read as a conversation. The
    /// fake agent of the test suite writes Claude Code's format.
    pub fn of(agent: &str) -> Option<Dialect> {
        match agent {
            "claude" | "fake" => Some(Dialect::Claude),
            "codex" => Some(Dialect::Codex),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Question {
    pub header: String,
    pub question: String,
    pub multi: bool,
    pub options: Vec<Choice>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Choice {
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// One thing in the conversation. `kind` says which fields mean something:
/// user, text, thinking, tool, question, plan, tasks, notice.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Item {
    /// `<line>.<block>`: the session line and the block in it. Stable.
    pub id: String,
    pub line: u64,
    pub kind: &'static str,
    /// When the record says it was written, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<u64>,
    /// user: what was sent. text and plan: markdown source. notice: words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `text` was cut at `TEXT_CAP`: the rest is detail.
    #[serde(skip_serializing_if = "is_false")]
    pub more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<u32>,
    /// user: you, queued, agent, task, schedule, system.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<&'static str>,
    /// user from another agent: what that agent is called.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Codex text: commentary or final_answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// tool: the tool's name. tasks: the kind of sub-agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// tool and tasks: running, ok, error, denied, interrupted, done.
    /// question: waiting, answered, dismissed. plan: waiting, approved,
    /// rejected, dismissed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
    #[serde(skip_serializing_if = "is_false")]
    pub background: bool,
    /// Lines an edit added and took away.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<Question>>,
    /// question, once answered: the question's words, then the answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<(String, String)>>,
    /// notice: compacted, summary, turn_ended, interrupted, error, info,
    /// away_summary, command_output, task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub what: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_tokens: Option<u64>,
    /// The vendor's id of a tool call, which its result names. Not served.
    #[serde(skip)]
    pub tool_id: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Item {
    fn new(line: u64, block: usize, kind: &'static str, ts_ms: Option<u64>) -> Item {
        Item {
            id: format!("{line}.{block}"),
            line,
            kind,
            ts_ms,
            ..Default::default()
        }
    }

    fn with_text(mut self, text: &str) -> Item {
        self.set_text(text);
        self
    }

    /// The item's text, cleaned and cut as every item's is.
    pub fn set_text(&mut self, text: &str) {
        let (text, more) = cap(&clean(text), TEXT_CAP);
        self.text = Some(text);
        self.more = more;
    }

    fn notice(line: u64, block: usize, what: &'static str, ts_ms: Option<u64>) -> Item {
        let mut i = Item::new(line, block, "notice", ts_ms);
        i.what = Some(what);
        i
    }

    /// The agent's turn is over, or the owner spoke: a call still waiting
    /// for a result at this point never got one.
    fn is_boundary(&self) -> bool {
        self.kind == "user"
            || (self.kind == "notice"
                && matches!(self.what, Some("turn_ended") | Some("interrupted")))
    }

    fn waits_for_result(&self) -> bool {
        self.tool_id.is_some()
    }
}

/// What a later line says of a tool call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolResult {
    pub tool_id: String,
    /// ok, error, denied, interrupted, done (finished, outcome not said).
    pub status: &'static str,
    pub ts_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub added: Option<u32>,
    pub removed: Option<u32>,
    pub answers: Option<Vec<(String, String)>>,
    pub background: bool,
    /// For a plan that was approved: the plan as the result has it. The
    /// call's own record can hold an earlier text (see `settle`).
    pub plan: Option<String>,
}

/// What one line of a session file adds to the conversation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    pub items: Vec<Item>,
    pub results: Vec<ToolResult>,
    pub title: Option<String>,
    /// The line was not understood: what kind of record it is.
    pub unknown: Option<String>,
}

// ------------------------------------------------------------------ text

/// Text from a file is data. Terminal escape sequences and control
/// characters are dropped here; the view inserts what is left as text.
pub fn clean(s: &str) -> String {
    if !s.chars().any(|c| c.is_control() && c != '\n') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameters, then one final byte.
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC: to BEL or ESC \.
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// At most `max` bytes, cut between characters. True when something was cut.
pub fn cap(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    (s[..cut].to_string(), true)
}

/// What a tool call's item says of a command or a file, for telling
/// whether a request held by a hook is about that call.
pub fn summary_line(s: &str) -> String {
    one_line(s, SUMMARY_CAP)
}

fn one_line(s: &str, max_chars: usize) -> String {
    let line = clean(s.trim()).lines().next().unwrap_or("").to_string();
    if line.chars().count() <= max_chars {
        return line;
    }
    let mut out: String = line.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// A record's kind as written in a file: a name, bounded, nothing else.
fn kind_name(prefix: &str, s: &str) -> String {
    let name: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .take(40)
        .collect();
    format!("{prefix}{}", if name.is_empty() { "?" } else { &name })
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `<tag>inner</tag>` in text an agent program wrapped.
fn tagged<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&close)? + start;
    Some(&s[start..end])
}

/// `2026-11-15T14:31:02.123Z` as milliseconds since 1970.
pub fn ts_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, sec) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let ms = match s.get(19..) {
        Some(rest) if rest.starts_with('.') => {
            let digits: String = rest[1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .take(3)
                .collect();
            let v = digits.parse::<i64>().unwrap_or(0);
            v * 10i64.pow(3 - digits.len() as u32)
        }
        _ => 0,
    };
    // Days since 1970 from a civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(((days * 24 + h) * 60 + mi) * 60_000 + sec * 1000 + ms).ok()
}

fn ts_of(v: &Value) -> Option<u64> {
    v.get("timestamp").and_then(|t| t.as_str()).and_then(ts_ms)
}

// ---------------------------------------------------------------- Claude

/// Only what says which record a line is. The rest of a line (megabytes,
/// for some) is passed over without being built.
#[derive(Deserialize)]
struct Head<'a> {
    #[serde(borrow, rename = "type")]
    ty: Option<Cow<'a, str>>,
    #[serde(borrow)]
    payload: Option<PayloadHead<'a>>,
}

#[derive(Deserialize)]
struct PayloadHead<'a> {
    #[serde(borrow, rename = "type")]
    ty: Option<Cow<'a, str>>,
}

/// Records Claude Code writes that are not conversation. Seen in 2.1.302
/// to 2.1.325, with `summary` and `progress` from versions before.
const CLAUDE_QUIET: &[&str] = &[
    "mode",
    "permission-mode",
    "session-owner",
    "file-history-snapshot",
    "file-history-delta",
    "attachment",
    "atis-latch",
    "api-request-shape",
    "api-request-blob",
    "api-request",
    "last-prompt",
    "tools-used",
    "bridge-session",
    "artifact-comment-monitor",
    "artifact-autoreact-ledger",
    "queue-operation",
    "cost-state",
    "frame-link",
    "summary",
    "progress",
];

const CLAUDE_QUIET_SYSTEM: &[&str] = &["stop_hook_summary", "bridge_status", "scheduled_task_fire"];

fn summary_of(input: &Value) -> String {
    const KEYS: &[&str] = &[
        "command",
        "file_path",
        "notebook_path",
        "path",
        "pattern",
        "query",
        "url",
        "skill",
        "description",
        "prompt",
        "name",
    ];
    if let Some(s) = input.as_str() {
        return one_line(s, SUMMARY_CAP);
    }
    for k in KEYS {
        match input.get(*k) {
            Some(Value::String(s)) if !s.trim().is_empty() => return one_line(s, SUMMARY_CAP),
            // Codex gives a command as a list of words.
            Some(Value::Array(words)) if *k == "command" => {
                let joined = words
                    .iter()
                    .filter_map(|w| w.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                return one_line(&joined, SUMMARY_CAP);
            }
            _ => {}
        }
    }
    String::new()
}

/// The questions of an AskUserQuestion call, as an item shows them: every
/// string cleaned and cut. Also what a held question shows (core, questions.rs).
pub fn questions_of(input: &Value) -> Vec<Question> {
    let short = |v: &Value, max: usize| one_line(v.as_str().unwrap_or(""), max);
    input
        .get("questions")
        .and_then(|q| q.as_array())
        .map(|qs| {
            qs.iter()
                .take(8)
                .map(|q| Question {
                    header: short(&q["header"], 80),
                    question: cap(&clean(q["question"].as_str().unwrap_or("")), 2000).0,
                    multi: q["multiSelect"] == json!(true),
                    options: q["options"]
                        .as_array()
                        .map(|os| {
                            os.iter()
                                .take(16)
                                .map(|o| Choice {
                                    label: short(&o["label"], 200),
                                    description: cap(
                                        &clean(o["description"].as_str().unwrap_or("")),
                                        1000,
                                    )
                                    .0,
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn patch_counts(patch: &Value) -> Option<(u32, u32)> {
    let hunks = patch.as_array()?;
    let (mut added, mut removed) = (0, 0);
    for line in hunks
        .iter()
        .filter_map(|h| h.get("lines").and_then(|l| l.as_array()))
        .flatten()
        .filter_map(|l| l.as_str())
    {
        match line.as_bytes().first() {
            Some(b'+') => added += 1,
            Some(b'-') => removed += 1,
            _ => {}
        }
    }
    Some((added, removed))
}

fn claude_result(v: &Value, block: &Value, ts: Option<u64>) -> Option<ToolResult> {
    let tool_id = block.get("tool_use_id")?.as_str()?.to_string();
    let tur = v.get("toolUseResult").unwrap_or(&Value::Null);
    let said = text_of(block.get("content").unwrap_or(&Value::Null));
    let said = said.trim_start();
    let status =
        if tur["interrupted"] == json!(true) || said.starts_with("[Request interrupted by user") {
            "interrupted"
        } else if v.get("toolDenialKind").is_some_and(|k| !k.is_null())
            || said.starts_with("The user doesn't want to proceed")
            // A plan the owner sent back from the Control Deck: the hook's
            // refusal, in spandeck's own words (core, questions.rs). How
            // Claude Code writes a call a hook refused has not been seen
            // in a real session; its hook reference says the reason is
            // shown to the agent.
            || said.contains(SENT_BACK)
        {
            "denied"
        } else if block["is_error"] == json!(true) {
            "error"
        } else {
            "ok"
        };
    let counts = tur.get("structuredPatch").and_then(patch_counts);
    let answers = tur.get("answers").and_then(|a| a.as_object()).map(|a| {
        a.iter()
            .take(8)
            .map(|(q, ans)| {
                let ans = ans
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| ans.to_string());
                (cap(&clean(q), 2000).0, cap(&clean(&ans), 2000).0)
            })
            .collect()
    });
    Some(ToolResult {
        tool_id,
        status,
        ts_ms: ts,
        duration_ms: tur.get("durationMs").and_then(|d| d.as_u64()),
        added: counts.map(|c| c.0),
        removed: counts.map(|c| c.1),
        answers,
        plan: tur.get("plan").and_then(|p| p.as_str()).map(String::from),
        background: tur["status"] == json!("async_launched")
            || tur["isAsync"] == json!(true)
            || tur.get("backgroundTaskId").is_some_and(|b| !b.is_null()),
    })
}

fn claude_user(line: u64, v: &Value, out: &mut Parsed) {
    let ts = ts_of(v);
    let content = v.pointer("/message/content").unwrap_or(&Value::Null);
    if let Some(blocks) = content.as_array() {
        out.results.extend(
            blocks
                .iter()
                .filter(|b| b["type"] == json!("tool_result"))
                .filter_map(|b| claude_result(v, b, ts)),
        );
    }
    // Text Claude Code put there for the model, not something anyone said.
    if v["isMeta"] == json!(true) {
        return;
    }
    let (text, images) = match content {
        Value::String(s) => (s.clone(), 0),
        Value::Array(blocks) => (
            blocks
                .iter()
                .filter(|b| b["type"] == json!("text"))
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            blocks
                .iter()
                .filter(|b| b["type"] == json!("image"))
                .count() as u32,
        ),
        _ => return,
    };
    let said = text.trim();
    if said.is_empty() && images == 0 {
        return;
    }
    if v["isCompactSummary"] == json!(true) {
        out.items
            .push(Item::notice(line, 0, "summary", ts).with_text(said));
        return;
    }
    if said.starts_with("[Request interrupted by user") {
        out.items.push(Item::notice(line, 0, "interrupted", ts));
        return;
    }
    if said.starts_with("<local-command-stdout>") {
        let shown = tagged(said, "local-command-stdout").unwrap_or("").trim();
        if !shown.is_empty() {
            out.items
                .push(Item::notice(line, 0, "command_output", ts).with_text(shown));
        }
        return;
    }
    if said.starts_with("<task-notification>") {
        let mut item = Item::notice(line, 0, "task", ts)
            .with_text(tagged(said, "summary").unwrap_or("").trim());
        item.level = tagged(said, "status").map(|s| one_line(s, 40));
        out.items.push(item);
        return;
    }
    let mut item = Item::new(line, 0, "user", ts);
    // A slash command, as Claude Code records one.
    item = match tagged(said, "command-name") {
        Some(name) if said.starts_with("<command-") => item.with_text(
            format!(
                "{} {}",
                name.trim(),
                tagged(said, "command-args").unwrap_or("").trim()
            )
            .trim(),
        ),
        _ => item.with_text(said),
    };
    item.images = (images > 0).then_some(images);
    let origin = v.pointer("/origin/kind").and_then(|k| k.as_str());
    item.origin = Some(match origin {
        Some("peer") => "agent",
        Some("task-notification") => "task",
        _ if v["turnOrigin"] == json!("scheduled") => "schedule",
        Some("human") | None if v["promptSource"] == json!("queued") => "queued",
        Some("human") | None if v["promptSource"] == json!("system") => "system",
        Some("human") | None => "you",
        Some(_) => "system",
    });
    if origin == Some("peer") {
        item.from = v
            .pointer("/origin/name")
            .and_then(|n| n.as_str())
            .map(|n| one_line(n, 80));
    }
    out.items.push(item);
}

fn claude_assistant(line: u64, v: &Value, out: &mut Parsed) {
    let ts = ts_of(v);
    let Some(blocks) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
        return;
    };
    if v["isApiErrorMessage"] == json!(true) {
        out.items
            .push(Item::notice(line, 0, "error", ts).with_text(&text_of(&json!(blocks))));
        return;
    }
    for (n, b) in blocks.iter().enumerate() {
        match b["type"].as_str().unwrap_or("") {
            "text" => {
                let text = b["text"].as_str().unwrap_or("");
                if !text.trim().is_empty() {
                    out.items
                        .push(Item::new(line, n, "text", ts).with_text(text));
                }
            }
            // How long, never what.
            "thinking" | "redacted_thinking" => {
                let mut item = Item::new(line, n, "thinking", ts);
                item.duration_ms = v.get("thinkingDurationMs").and_then(|d| d.as_u64());
                out.items.push(item);
            }
            "tool_use" => {
                let name = b["name"].as_str().unwrap_or("tool");
                let input = b.get("input").unwrap_or(&Value::Null);
                let mut item = match name {
                    "AskUserQuestion" => {
                        let mut i = Item::new(line, n, "question", ts);
                        i.questions = Some(questions_of(input));
                        i.status = Some("waiting");
                        i
                    }
                    "ExitPlanMode" => {
                        let mut i = Item::new(line, n, "plan", ts)
                            .with_text(input["plan"].as_str().unwrap_or(""));
                        i.status = Some("waiting");
                        i
                    }
                    "Agent" | "Task" => {
                        let mut i = Item::new(line, n, "tasks", ts);
                        i.name = Some(one_line(
                            input["subagent_type"].as_str().unwrap_or("sub-agent"),
                            80,
                        ));
                        i.summary = Some(one_line(
                            input["description"].as_str().unwrap_or(""),
                            SUMMARY_CAP,
                        ));
                        i.status = Some("running");
                        i
                    }
                    _ => {
                        let mut i = Item::new(line, n, "tool", ts);
                        i.name = Some(one_line(name, 120));
                        i.summary = Some(summary_of(input));
                        i.status = Some("running");
                        i
                    }
                };
                item.tool_id = b["id"].as_str().map(String::from);
                out.items.push(item);
            }
            other => out.unknown = Some(kind_name("assistant:", other)),
        }
    }
}

fn claude_system(line: u64, v: &Value, out: &mut Parsed) {
    let ts = ts_of(v);
    let said = v["content"].as_str().unwrap_or("");
    match v["subtype"].as_str().unwrap_or("") {
        "turn_duration" => {
            let mut item = Item::notice(line, 0, "turn_ended", ts);
            item.duration_ms = v.get("durationMs").and_then(|d| d.as_u64());
            out.items.push(item);
        }
        "compact_boundary" => {
            let meta = v.get("compactMetadata").unwrap_or(&Value::Null);
            let mut item = Item::notice(line, 0, "compacted", ts);
            item.trigger = meta["trigger"].as_str().map(|t| one_line(t, 40));
            item.pre_tokens = meta["preTokens"].as_u64();
            item.post_tokens = meta["postTokens"].as_u64();
            item.duration_ms = meta["durationMs"].as_u64();
            out.items.push(item);
        }
        "away_summary" => out
            .items
            .push(Item::notice(line, 0, "away_summary", ts).with_text(said)),
        "informational" | "api_error" => {
            let level = v["level"].as_str().unwrap_or("info");
            let what = if level == "error" || v["subtype"] == json!("api_error") {
                "error"
            } else {
                "info"
            };
            let mut item = Item::notice(line, 0, what, ts).with_text(said);
            item.level = Some(one_line(level, 40));
            out.items.push(item);
        }
        quiet if CLAUDE_QUIET_SYSTEM.contains(&quiet) => {}
        other => out.unknown = Some(kind_name("system:", other)),
    }
}

/// Claude Code `~/.claude/projects/<dir>/<session>.jsonl`.
fn claude_line(line: u64, raw: &str) -> Parsed {
    let mut out = Parsed::default();
    let Ok(head) = serde_json::from_str::<Head>(raw) else {
        out.unknown = Some("not-json".into());
        return out;
    };
    let ty = head.ty.as_deref().unwrap_or("");
    match ty {
        "user" | "assistant" | "system" | "ai-title" | "custom-title" => {}
        quiet if CLAUDE_QUIET.contains(&quiet) => return out,
        other => {
            out.unknown = Some(kind_name("", other));
            return out;
        }
    }
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return out;
    };
    // A sub-agent's own lines, when they are in the same file.
    if v["isSidechain"] == json!(true) {
        return out;
    }
    match ty {
        "user" => claude_user(line, &v, &mut out),
        "assistant" => claude_assistant(line, &v, &mut out),
        "system" => claude_system(line, &v, &mut out),
        _ => {
            out.title = v
                .get("aiTitle")
                .or_else(|| v.get("customTitle"))
                .and_then(|t| t.as_str())
                .map(|t| one_line(t, 200))
                .filter(|t| !t.is_empty());
        }
    }
    out
}

// ----------------------------------------------------------------- Codex

const CODEX_QUIET: &[&str] = &[
    "session_meta",
    "turn_context",
    "world_state",
    "token_usage_record",
];

/// `event_msg` records that repeat a `response_item` or carry no words.
const CODEX_QUIET_EVENTS: &[&str] = &[
    "task_started",
    "item_completed",
    "token_count",
    "thread_settings_applied",
    "user_message",
    "agent_message",
    "agent_reasoning",
];

/// Text Codex puts in a user message that nobody typed.
const CODEX_WRAPPERS: &[&str] = &["<environment_context>", "<user_instructions>"];

fn codex_text(content: &Value) -> String {
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| matches!(b["type"].as_str(), Some("input_text" | "output_text")))
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| content.as_str().unwrap_or("").to_string())
}

/// Codex `~/.codex/sessions/.../rollout-*.jsonl`, as 0.169.0 writes it.
fn codex_line(line: u64, raw: &str) -> Parsed {
    let mut out = Parsed::default();
    let Ok(head) = serde_json::from_str::<Head>(raw) else {
        out.unknown = Some("not-json".into());
        return out;
    };
    let ty = head.ty.as_deref().unwrap_or("");
    let sub = head
        .payload
        .as_ref()
        .and_then(|p| p.ty.as_deref())
        .unwrap_or("");
    match (ty, sub) {
        ("response_item", "additional_tools") => return out,
        ("response_item", _) | ("compacted", _) => {}
        ("event_msg", quiet) if CODEX_QUIET_EVENTS.contains(&quiet) => return out,
        ("event_msg", _) => {}
        (quiet, _) if CODEX_QUIET.contains(&quiet) => return out,
        (other, _) => {
            out.unknown = Some(kind_name("", other));
            return out;
        }
    }
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return out;
    };
    let ts = ts_of(&v);
    let p = v.get("payload").unwrap_or(&Value::Null);
    match (ty, sub) {
        ("compacted", _) => out.items.push(Item::notice(line, 0, "compacted", ts)),
        ("event_msg", "task_complete") => {
            let mut item = Item::notice(line, 0, "turn_ended", ts);
            item.duration_ms = p["duration_ms"].as_u64();
            out.items.push(item);
        }
        ("event_msg", "turn_aborted") => out.items.push(Item::notice(line, 0, "interrupted", ts)),
        ("event_msg", "context_compacted") => {
            out.items.push(Item::notice(line, 0, "compacted", ts))
        }
        ("event_msg", "error" | "stream_error") => out.items.push(
            Item::notice(line, 0, "error", ts).with_text(p["message"].as_str().unwrap_or("")),
        ),
        ("event_msg", other) => out.unknown = Some(kind_name("event_msg:", other)),
        (_, "message") => {
            let text = codex_text(&p["content"]);
            let said = text.trim();
            match p["role"].as_str().unwrap_or("") {
                // Instructions Codex gives itself.
                "developer" | "system" => {}
                "user" => {
                    if said.is_empty() || CODEX_WRAPPERS.iter().any(|w| said.starts_with(w)) {
                        return out;
                    }
                    if said.starts_with("<turn_aborted>") {
                        out.items.push(Item::notice(line, 0, "interrupted", ts));
                        return out;
                    }
                    let mut item = Item::new(line, 0, "user", ts).with_text(said);
                    item.origin = Some("you");
                    let images = p["content"]
                        .as_array()
                        .map(|b| {
                            b.iter()
                                .filter(|b| b["type"] == json!("input_image"))
                                .count() as u32
                        })
                        .unwrap_or(0);
                    item.images = (images > 0).then_some(images);
                    out.items.push(item);
                }
                "assistant" => {
                    if !said.is_empty() {
                        let mut item = Item::new(line, 0, "text", ts).with_text(said);
                        item.phase = p["phase"].as_str().map(|s| one_line(s, 40));
                        out.items.push(item);
                    }
                }
                other => out.unknown = Some(kind_name("message:", other)),
            }
        }
        (_, "reasoning") => out.items.push(Item::new(line, 0, "thinking", ts)),
        (_, "function_call" | "custom_tool_call" | "local_shell_call") => {
            let mut item = Item::new(line, 0, "tool", ts);
            item.name = Some(one_line(p["name"].as_str().unwrap_or("shell"), 120));
            let input = codex_input(p);
            item.summary = Some(summary_of(&input));
            item.status = Some("running");
            item.tool_id = p["call_id"].as_str().map(String::from);
            out.items.push(item);
        }
        (_, "function_call_output" | "custom_tool_call_output") => {
            if let Some(id) = p["call_id"].as_str() {
                out.results.push(ToolResult {
                    tool_id: id.to_string(),
                    // The file does not say whether the call succeeded.
                    status: "done",
                    ts_ms: ts,
                    ..Default::default()
                });
            }
        }
        (_, other) => out.unknown = Some(kind_name("response_item:", other)),
    }
    out
}

/// What a Codex tool call was given: `arguments` is JSON in a string,
/// `input` is the text itself.
fn codex_input(p: &Value) -> Value {
    if let Some(args) = p.get("arguments") {
        return args
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| args.clone());
    }
    p.get("input")
        .or_else(|| p.get("action"))
        .cloned()
        .unwrap_or(Value::Null)
}

pub fn parse_line(dialect: Dialect, line: u64, raw: &str) -> Parsed {
    match dialect {
        Dialect::Claude => claude_line(line, raw),
        Dialect::Codex => codex_line(line, raw),
    }
}

/// Put what a later line said of a call into the call's item.
fn settle(item: &mut Item, r: &ToolResult) {
    item.duration_ms = r.duration_ms.or_else(|| {
        let (from, to) = (item.ts_ms?, r.ts_ms?);
        to.checked_sub(from)
    });
    item.status = Some(match (item.kind, r.status) {
        ("question", "ok") => "answered",
        ("question", _) => "dismissed",
        ("plan", "ok") => "approved",
        ("plan", "denied") => "rejected",
        ("plan", _) => "dismissed",
        (_, status) => status,
    });
    item.background = r.background;
    if item.kind == "tool" {
        item.added = r.added;
        item.removed = r.removed;
    }
    if item.kind == "question" {
        item.answers = r.answers.clone();
    }
    // Claude Code keeps a plan in a file and puts what the file held into
    // the call's record when the call is made. When the plan is changed in
    // the same step, the record has the plan from before (seen with
    // 2.1.325 on 16 Nov 2026: a second plan shown with the first one's
    // text). The result of an approved plan has the plan that was approved.
    if item.kind == "plan" {
        if let Some(plan) = r.plan.as_deref().filter(|p| !p.trim().is_empty()) {
            item.set_text(plan);
        }
    }
}

/// A call that never got a result: the turn went on without it.
fn abandon(item: &mut Item) {
    item.status = Some(match item.kind {
        "question" | "plan" => "dismissed",
        _ => "interrupted",
    });
}

// ----------------------------------------------------------------- detail

pub fn paged(full: &str, offset: usize) -> Value {
    let mut start = offset.min(full.len());
    while !full.is_char_boundary(start) {
        start += 1;
    }
    let (text, more) = cap(&full[start..], DETAIL_CAP);
    let end = start + text.len();
    json!({
        "text": text, "offset": start, "total": full.len(),
        "next": more.then_some(end),
    })
}

fn input_fields(input: &Value) -> Value {
    let field = |v: &Value| -> (String, bool) {
        match v {
            Value::String(s) => cap(&clean(s), FIELD_CAP),
            other => cap(&other.to_string(), FIELD_CAP),
        }
    };
    let mut cut = false;
    let mut out = Map::new();
    match input {
        Value::Object(fields) => {
            for (k, v) in fields.iter().take(32) {
                let (text, more) = field(v);
                cut |= more;
                out.insert(one_line(k, 80), json!(text));
            }
        }
        Value::Null => {}
        other => {
            let (text, more) = field(other);
            cut |= more;
            out.insert("input".into(), json!(text));
        }
    }
    json!({"fields": out, "more": cut})
}

fn hunks_of(patch: &Value, budget: &mut usize, cut: &mut bool) -> Vec<Value> {
    let mut out = vec![];
    for h in patch.as_array().map(|a| a.as_slice()).unwrap_or_default() {
        let all = h["lines"].as_array().map(|l| l.as_slice()).unwrap_or(&[]);
        if *budget == 0 {
            *cut = true;
            break;
        }
        let take = all.len().min(*budget);
        *cut |= take < all.len();
        *budget -= take;
        let lines: Vec<String> = all[..take]
            .iter()
            .map(|l| cap(&clean(l.as_str().unwrap_or("")), 2000).0)
            .collect();
        out.push(json!({
            "old_start": h["oldStart"].as_u64(), "old_lines": h["oldLines"].as_u64(),
            "new_start": h["newStart"].as_u64(), "new_lines": h["newLines"].as_u64(),
            "lines": lines,
        }));
    }
    out
}

/// What an edit changed, from the structured patch Claude Code records.
fn diffs_of(tur: &Value) -> Option<Value> {
    let mut budget = DIFF_LINES_CAP;
    let mut cut = false;
    let mut files = vec![];
    if tur.get("structuredPatch").is_some_and(|p| p.is_array()) {
        let hunks = hunks_of(&tur["structuredPatch"], &mut budget, &mut cut);
        if !hunks.is_empty() {
            files.push(json!({
                "file": one_line(tur["filePath"].as_str().unwrap_or(""), 400),
                "created": tur["type"] == json!("create"),
                "hunks": hunks,
            }));
        }
    }
    for f in tur
        .pointer("/bashEditDiff/files")
        .and_then(|f| f.as_array())
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .take(64)
    {
        files.push(json!({
            "file": one_line(f["filePath"].as_str().unwrap_or(""), 400),
            "created": f["created"] == json!(true),
            "deleted": f["deleted"] == json!(true),
            "hunks": hunks_of(&f["hunks"], &mut budget, &mut cut),
        }));
    }
    (!files.is_empty()).then(|| json!({"files": files, "more": cut}))
}

/// Everything a tool call's row can open: what it was given, what came
/// back, and what it changed. Each part capped, with a mark when cut.
fn detail_of(
    dialect: Dialect,
    item: &Item,
    block: usize,
    call_raw: &str,
    result_raw: Option<&str>,
    offset: usize,
) -> Value {
    let call: Value = serde_json::from_str(call_raw).unwrap_or(Value::Null);
    let mut out = json!({"item": item});
    let tool_id = item.tool_id.as_deref();
    match item.kind {
        "thinking" => return out,
        "tool" | "tasks" | "question" | "plan" => {}
        // A message or notice that fits in its item has nothing more.
        _ if !item.more => return out,
        // A long one: the same text, in full, a page at a time.
        _ => {
            let full = match dialect {
                Dialect::Claude => match call["type"].as_str() {
                    Some("assistant") => call["message"]["content"][block]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                    Some("system") => call["content"].as_str().unwrap_or("").to_string(),
                    _ => match &call["message"]["content"] {
                        Value::String(s) => s.clone(),
                        blocks => text_of(blocks),
                    },
                },
                Dialect::Codex => codex_text(&call["payload"]["content"]),
            };
            out["text"] = paged(&clean(full.trim()), offset);
            return out;
        }
    }
    let input = match dialect {
        Dialect::Claude => call["message"]["content"][block]
            .get("input")
            .cloned()
            .unwrap_or(Value::Null),
        Dialect::Codex => codex_input(&call["payload"]),
    };
    if item.kind == "plan" {
        // The plan that was approved, when the result has it; else what
        // the call's record holds (see `settle`). The call's id goes with
        // it for the station, which may have the text its hook was given;
        // it is not served.
        let approved = result_raw
            .and_then(|r| serde_json::from_str::<Value>(r).ok())
            .and_then(|r| r["toolUseResult"]["plan"].as_str().map(String::from))
            .filter(|p| !p.trim().is_empty());
        let plan = approved.unwrap_or_else(|| input["plan"].as_str().unwrap_or("").to_string());
        out["text"] = paged(&clean(&plan), offset);
        out["tool_use_id"] = json!(tool_id);
        return out;
    }
    out["input"] = input_fields(&input);
    let Some(result) = result_raw.and_then(|r| serde_json::from_str::<Value>(r).ok()) else {
        return out;
    };
    let mut images = 0;
    let full = match dialect {
        Dialect::Claude => {
            let tur = &result["toolUseResult"];
            if let Some(diff) = diffs_of(tur) {
                out["diff"] = diff;
            }
            let block = result["message"]["content"]
                .as_array()
                .and_then(|blocks| {
                    blocks
                        .iter()
                        .find(|b| b["tool_use_id"].as_str() == tool_id && tool_id.is_some())
                })
                .cloned()
                .unwrap_or(Value::Null);
            if let Some(parts) = block["content"].as_array() {
                images = parts.iter().filter(|p| p["type"] == json!("image")).count();
            }
            match (tur["stdout"].as_str(), tur["stderr"].as_str()) {
                (Some(o), Some(e)) if !e.trim().is_empty() => format!("{o}\n{e}"),
                (Some(o), _) => o.to_string(),
                _ => text_of(&block["content"]),
            }
        }
        Dialect::Codex => {
            let o = &result["payload"]["output"];
            if let Some(parts) = o.as_array() {
                images = parts
                    .iter()
                    .filter(|p| p["type"] == json!("input_image"))
                    .count();
            }
            match o {
                Value::String(s) => s.clone(),
                parts => parts
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default(),
            }
        }
    };
    out["output"] = paged(&clean(&full), offset);
    if images > 0 {
        out["output"]["images"] = json!(images);
    }
    out
}

// ------------------------------------------------------------------ index

/// One item's place in the file: 32 bytes, whatever the item holds.
#[derive(Debug, Clone, Copy)]
struct Slot {
    line: u64,
    block: u16,
    /// The call was let go without a result (`closed_at` says where).
    abandoned: bool,
    /// The line that settled this call; 0 while it waits, or for an item
    /// that is not a call.
    closed_at: u64,
}

struct Open {
    tool_id: String,
    name: String,
    summary: String,
    slot: usize,
}

/// Where each item of one conversation is, so that a page is a few lines
/// read by number and never the whole file again.
struct Conv {
    dialect: Dialect,
    /// The last raw line taken in.
    upto: u64,
    slots: Vec<Slot>,
    /// (the line that settled a call, the call's slot), by line.
    closes: Vec<(u64, usize)>,
    open: Vec<Open>,
    title: Option<String>,
    unknown: u64,
    unknown_kinds: Vec<(String, u64)>,
    /// The oldest items were let go to keep to `SLOTS_MAX`.
    trimmed: bool,
    used: Instant,
}

impl Conv {
    fn new(dialect: Dialect) -> Conv {
        Conv {
            dialect,
            upto: 0,
            slots: vec![],
            closes: vec![],
            open: vec![],
            title: None,
            unknown: 0,
            unknown_kinds: vec![],
            trimmed: false,
            used: Instant::now(),
        }
    }

    fn feed(&mut self, line: u64, raw: &str) {
        let p = parse_line(self.dialect, line, raw);
        for r in &p.results {
            if let Some(at) = self.open.iter().position(|o| o.tool_id == r.tool_id) {
                let o = self.open.remove(at);
                self.slots[o.slot].closed_at = line;
                self.closes.push((line, o.slot));
            }
        }
        for item in p.items {
            if item.is_boundary() {
                for o in self.open.drain(..) {
                    self.slots[o.slot].closed_at = line;
                    self.slots[o.slot].abandoned = true;
                    self.closes.push((line, o.slot));
                }
            }
            let block = item
                .id
                .rsplit('.')
                .next()
                .and_then(|b| b.parse().ok())
                .unwrap_or(0);
            if item.waits_for_result() {
                if self.open.len() >= OPEN_MAX {
                    self.open.remove(0);
                }
                self.open.push(Open {
                    tool_id: item.tool_id.clone().unwrap_or_default(),
                    name: item.name.clone().unwrap_or_default(),
                    summary: item.summary.clone().unwrap_or_default(),
                    slot: self.slots.len(),
                });
            }
            self.slots.push(Slot {
                line,
                block,
                abandoned: false,
                closed_at: 0,
            });
        }
        if p.title.is_some() {
            self.title = p.title;
        }
        if let Some(kind) = p.unknown {
            self.unknown += 1;
            match self.unknown_kinds.iter().position(|(k, _)| *k == kind) {
                Some(at) => self.unknown_kinds[at].1 += 1,
                None if self.unknown_kinds.len() < 8 => self.unknown_kinds.push((kind, 1)),
                None => {}
            }
        }
        self.upto = line;
        if self.slots.len() > SLOTS_MAX {
            let gone = self.slots.len() / 2;
            self.slots.drain(..gone);
            self.closes.retain(|(_, s)| *s >= gone);
            self.closes.iter_mut().for_each(|(_, s)| *s -= gone);
            self.open.retain(|o| o.slot >= gone);
            self.open.iter_mut().for_each(|o| o.slot -= gone);
            self.trimmed = true;
        }
    }

    /// The first slot on a line after `line`.
    fn after(&self, line: u64) -> usize {
        self.slots.partition_point(|s| s.line <= line)
    }

    fn find(&self, line: u64, block: u16) -> Option<usize> {
        self.slots
            .binary_search_by(|s| (s.line, s.block).cmp(&(line, block)))
            .ok()
    }
}

/// Which part of a conversation is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// The newest items.
    Tail,
    /// Items on lines after this one, oldest first: the cursor.
    After(u64),
    /// The items just before this line: the page above.
    Before(u64),
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Page {
    pub items: Vec<Item>,
    /// Items sent before (on lines up to the `after` asked for) that a line
    /// since has changed: a call that got its result. Replace by `id`.
    pub updated: Vec<Item>,
    /// The line this answer reaches. Ask `after` it for what comes next.
    pub cursor: u64,
    /// The line of the first item here. Ask `before` it for the page above.
    pub first: Option<u64>,
    pub more_before: bool,
    pub more_after: bool,
    pub title: Option<String>,
    /// Records in this session the builder did not understand.
    pub not_understood: u64,
    /// What kinds they were, and how many of each (eight kinds at most).
    pub not_understood_kinds: Vec<(String, u64)>,
    /// The session is longer than is indexed: its oldest items are not
    /// served.
    pub older_dropped: bool,
}

/// A call still waiting for its result: for tying an approval to its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub item: String,
    pub tool_id: String,
    pub name: String,
    /// The call's one line, as its item has it.
    pub summary: String,
}

#[derive(Default)]
pub(crate) struct Cache {
    convs: Mutex<HashMap<String, Arc<Mutex<Conv>>>>,
    /// Per session: how many of its lines were looked at for a title, and
    /// the last title found (`session_title`).
    titles: Mutex<HashMap<String, (u64, Option<String>)>>,
}

impl crate::Archive {
    /// The index of a session's conversation, brought up to the last line
    /// the archive holds. None for an agent whose file is not understood.
    fn conv(&self, session: &str) -> Option<Arc<Mutex<Conv>>> {
        let known = self.convs.convs.lock().unwrap().get(session).cloned();
        let conv = match known {
            Some(c) => c,
            None => {
                let dialect = Dialect::of(&self.session(session)?.agent)?;
                let mut convs = self.convs.convs.lock().unwrap();
                if convs.len() >= CONVS_MAX && !convs.contains_key(session) {
                    let oldest = convs
                        .iter()
                        .filter_map(|(id, c)| Some((id.clone(), c.try_lock().ok()?.used)))
                        .min_by_key(|(_, used)| *used)
                        .map(|(id, _)| id);
                    if let Some(id) = oldest {
                        convs.remove(&id);
                    }
                }
                convs
                    .entry(session.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(Conv::new(dialect))))
                    .clone()
            }
        };
        {
            let mut c = conv.lock().unwrap();
            c.used = Instant::now();
            // A chunk at a time: the database is never held for long, and
            // the lines are parsed with it free.
            loop {
                let rows = self.raw_after(session, c.upto, CHUNK_LINES, CHUNK_BYTES);
                if rows.is_empty() {
                    break;
                }
                for (line, raw) in &rows {
                    c.feed(*line, raw);
                }
            }
        }
        Some(conv)
    }

    /// A page of a session's conversation. New lines of the session's file
    /// are copied first, so an answer is as fresh as the file. None when the
    /// session is not recorded here or its agent's file is not understood.
    pub fn conversation(&self, session: &str, range: Range, limit: usize) -> Option<Page> {
        self.poll_session(session);
        let conv = self.conv(session)?;
        let limit = limit.clamp(1, PAGE_MAX);
        let (dialect, wanted, changed, mut page) = {
            let c = conv.lock().unwrap();
            let len = c.slots.len();
            let (from, to) = match range {
                Range::Tail => (len.saturating_sub(limit), len),
                Range::After(line) => {
                    let from = c.after(line);
                    (from, (from + limit).min(len))
                }
                Range::Before(line) => {
                    let to = c.slots.partition_point(|s| s.line < line);
                    (to.saturating_sub(limit), to)
                }
            };
            let (mut from, mut to) = (from, to);
            // Whole lines only, so that a line number is a cursor.
            while to > from && to < len && c.slots[to].line == c.slots[to - 1].line {
                to += 1;
            }
            while from > 0 && from < to && c.slots[from - 1].line == c.slots[from].line {
                from -= 1;
            }
            let cursor = match range {
                Range::Before(_) => c.slots[from..to].last().map(|s| s.line).unwrap_or(0),
                _ if to == len => c.upto,
                Range::After(line) => c.slots[from..to].last().map(|s| s.line).unwrap_or(line),
                Range::Tail => c.upto,
            };
            let changed: Vec<Slot> = match range {
                Range::After(line) => {
                    let start = c.closes.partition_point(|(at, _)| *at <= line);
                    let end = c.closes.partition_point(|(at, _)| *at <= cursor);
                    c.closes[start..end]
                        .iter()
                        .filter(|(_, slot)| *slot < from)
                        .map(|(_, slot)| c.slots[*slot])
                        .collect()
                }
                _ => vec![],
            };
            let page = Page {
                cursor,
                first: c.slots[from..to].first().map(|s| s.line),
                more_before: from > 0,
                more_after: to < len,
                title: c.title.clone(),
                not_understood: c.unknown,
                not_understood_kinds: c.unknown_kinds.clone(),
                older_dropped: c.trimmed,
                ..Default::default()
            };
            (c.dialect, c.slots[from..to].to_vec(), changed, page)
        };
        let mut parsed = self.parse_lines(session, dialect, wanted.iter().chain(&changed));
        page.items = wanted
            .iter()
            .filter_map(|s| build(s, &mut parsed))
            .collect();
        page.updated = changed
            .iter()
            .filter_map(|s| build(s, &mut parsed))
            .collect();
        Some(page)
    }

    /// The lines a set of slots needs, each read and parsed once.
    fn parse_lines<'a>(
        &self,
        session: &str,
        dialect: Dialect,
        slots: impl Iterator<Item = &'a Slot>,
    ) -> HashMap<u64, Parsed> {
        let mut lines: Vec<u64> = slots
            .flat_map(|s| [s.line, if s.abandoned { 0 } else { s.closed_at }])
            .filter(|l| *l > 0)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        let mut parsed = HashMap::new();
        for chunk in lines.chunks(32) {
            for (line, raw) in self.raw_at(session, chunk) {
                parsed.insert(line, parse_line(dialect, line, &raw));
            }
        }
        parsed
    }

    /// One item in full: a tool call's input, output and diff, or the whole
    /// of a text that was cut. `offset` continues a text where `next` said.
    pub fn conversation_detail(&self, session: &str, item: &str, offset: usize) -> Option<Value> {
        let (line, block) = item.split_once('.')?;
        let (line, block): (u64, u16) = (line.parse().ok()?, block.parse().ok()?);
        let conv = self.conv(session)?;
        let (dialect, slot) = {
            let c = conv.lock().unwrap();
            (c.dialect, c.slots[c.find(line, block)?])
        };
        let result_line = (!slot.abandoned && slot.closed_at > 0).then_some(slot.closed_at);
        let mut wanted = vec![slot.line];
        wanted.extend(result_line);
        let mut raws: HashMap<u64, String> = self.raw_at(session, &wanted).into_iter().collect();
        let call_raw = raws.remove(&slot.line)?;
        let result_raw = result_line.and_then(|l| raws.remove(&l));
        let mut parsed = HashMap::new();
        parsed.insert(slot.line, parse_line(dialect, slot.line, &call_raw));
        if let (Some(l), Some(raw)) = (result_line, &result_raw) {
            parsed.insert(l, parse_line(dialect, l, raw));
        }
        let built = build(&slot, &mut parsed)?;
        Some(detail_of(
            dialect,
            &built,
            block as usize,
            &call_raw,
            result_raw.as_deref(),
            offset,
        ))
    }

    /// Calls of a session that have no result yet, newest last, as the file
    /// has them now.
    pub fn conversation_waiting(&self, session: &str) -> Vec<Waiting> {
        self.poll_session(session);
        let Some(conv) = self.conv(session) else {
            return vec![];
        };
        let c = conv.lock().unwrap();
        c.open
            .iter()
            .map(|o| Waiting {
                item: format!("{}.{}", c.slots[o.slot].line, c.slots[o.slot].block),
                tool_id: o.tool_id.clone(),
                name: o.name.clone(),
                summary: o.summary.clone(),
            })
            .collect()
    }

    /// The session's title, when its file has one.
    pub fn conversation_title(&self, session: &str) -> Option<String> {
        self.conv(session)?.lock().unwrap().title.clone()
    }

    /// The session's title without building its index, for a list of every
    /// pane's session (the index is kept for eight sessions only, and a
    /// list would push out the ones being read). Only lines written since
    /// the last look are read, and of those only the ones that could hold a
    /// title.
    pub fn session_title(&self, session: &str) -> Option<String> {
        let info = self.session(session)?;
        let dialect = Dialect::of(&info.agent)?;
        let (seen, mut title) = self
            .convs
            .titles
            .lock()
            .unwrap()
            .get(session)
            .cloned()
            .unwrap_or_default();
        if info.lines <= seen {
            return title;
        }
        let rows: Vec<(u64, String)> = {
            let conn = self.conn.lock().unwrap();
            conn.prepare_cached(
                "SELECT line, raw FROM raw WHERE session=?1 AND line>?2
                 AND instr(raw, '-title\"')>0 ORDER BY line",
            )
            .and_then(|mut stmt| {
                stmt.query_map(rusqlite::params![session, seen as i64], |r| {
                    Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
                })?
                .collect()
            })
            .unwrap_or_default()
        };
        for (line, raw) in rows {
            if let Some(t) = parse_line(dialect, line, &raw).title {
                title = Some(t);
            }
        }
        self.convs
            .titles
            .lock()
            .unwrap()
            .insert(session.to_string(), (info.lines, title.clone()));
        title
    }
}

fn build(slot: &Slot, parsed: &mut HashMap<u64, Parsed>) -> Option<Item> {
    let id = format!("{}.{}", slot.line, slot.block);
    let mut item = parsed
        .get(&slot.line)?
        .items
        .iter()
        .find(|i| i.id == id)?
        .clone();
    if slot.abandoned {
        abandon(&mut item);
    } else if slot.closed_at > 0 {
        let result = parsed.get(&slot.closed_at).and_then(|p| {
            p.results
                .iter()
                .find(|r| Some(r.tool_id.as_str()) == item.tool_id.as_deref())
        });
        if let Some(r) = result {
            settle(&mut item, r);
        }
    }
    Some(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Archive;
    use std::io::Write;

    const CLAUDE: &str = include_str!("../fixtures/claude-2.1.325.jsonl");
    const CODEX: &str = include_str!("../fixtures/codex-0.169.0.jsonl");

    /// An archive following a file that holds the first `lines` lines of a
    /// fixture, as an agent that has written that much.
    fn following(agent: &str, fixture: &str, lines: usize) -> (Archive, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for l in fixture.lines().take(lines) {
            writeln!(f, "{l}").unwrap();
        }
        let a = Archive::memory().unwrap();
        a.follow("s1", agent, path.to_str().unwrap(), None, Some("p1"));
        (a, dir)
    }

    fn grow(dir: &tempfile::TempDir, fixture: &str, from: usize, to: usize) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("session.jsonl"))
            .unwrap();
        for l in fixture.lines().skip(from).take(to - from) {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn kinds(items: &[Item]) -> Vec<&str> {
        items
            .iter()
            .map(|i| i.what.unwrap_or(i.kind))
            .collect::<Vec<_>>()
    }

    /// Found with the real Claude Code 2.1.325 on 16 Nov 2026: a second plan
    /// was shown with the first plan's text. Claude Code keeps a plan in a
    /// file and writes what the file held into the call's record; when the
    /// plan is changed in the same step the record has the plan from
    /// before, and the result of the approved call has the plan that was
    /// approved. The shapes are that session's: the refusal of a plan sent
    /// back from the Control Deck, the record with the old text, and its
    /// result.
    #[test]
    fn an_approved_plan_is_shown_as_it_was_approved() {
        let old = "## Plan: add a one-line README.md";
        let new = "## Plan: add a two-line README.md";
        let call = |id: &str, plan: &str| {
            json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": id, "name": "ExitPlanMode", "input": {"plan": plan, "planFilePath": "/home/ada/.claude/plans/p.md"}}]}})
        };
        let lines = [
            json!({"type": "user", "message": {"role": "user", "content": "plan it"}}),
            call("toolu_1", old),
            json!({"type": "user", "toolDenialKind": "permission-rule",
                "toolUseResult": "Error: PreToolUse:ExitPlanMode hook error: The owner read this plan in spandeck and did not approve it: keep planning, and do not start the work.",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "is_error": true,
                    "content": "PreToolUse:ExitPlanMode hook error: The owner read this plan in spandeck and did not approve it: keep planning, and do not start the work."}]}}),
            call("toolu_2", old),
            json!({"type": "user", "toolUseResult": {"plan": new, "isAgent": false, "filePath": "/home/ada/.claude/plans/p.md"},
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_2",
                    "content": "User has approved your plan. You can now start coding."}]}}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let (a, _dir) = following("claude", &text, usize::MAX);
        let page = a.conversation("s1", Range::Tail, 200).unwrap();
        let plans: Vec<_> = page
            .items
            .iter()
            .filter(|i| i.kind == "plan")
            .map(|i| (i.status.unwrap(), i.text.clone().unwrap()))
            .collect();
        assert_eq!(
            plans,
            vec![("rejected", old.to_string()), ("approved", new.to_string())]
        );
        let approved = page.items.iter().rfind(|i| i.kind == "plan").unwrap();
        let detail = a.conversation_detail("s1", &approved.id, 0).unwrap();
        assert_eq!(detail["text"]["text"], new);
        assert_eq!(detail["tool_use_id"], "toolu_2");
    }

    /// Phase M1: a Claude Code session file becomes the items the view
    /// shows, calls paired with their results by id, and what is not
    /// understood is counted, not hidden.
    #[test]
    fn a_claude_session_becomes_items_and_says_what_it_did_not_understand() {
        let (a, _dir) = following("claude", CLAUDE, usize::MAX);
        let page = a.conversation("s1", Range::Tail, 200).unwrap();
        assert_eq!(
            kinds(&page.items),
            vec![
                "user",
                "thinking",
                "text",
                "tool",
                "tool",
                "tool",
                "question",
                "plan",
                "tasks",
                "text",
                "turn_ended",
                "user",
                "compacted",
                "summary",
                "user",
                "tool",
                "interrupted",
                "user",
                "task",
                "tool",
                "error",
            ],
            "{:#?}",
            page.items
        );
        let by_kind = |k: &str| -> Vec<&Item> {
            page.items
                .iter()
                .filter(|i| i.what.unwrap_or(i.kind) == k)
                .collect()
        };
        assert_eq!(
            page.title.as_deref(),
            Some("Heron counts for the marsh survey")
        );

        // The owner's message: the words, the picture counted, never its bytes.
        let users = by_kind("user");
        assert_eq!(
            users[0].text.as_deref(),
            Some("Count the herons in survey.csv and fix the tally script. [Image #1]")
        );
        assert_eq!((users[0].images, users[0].origin), (Some(1), Some("you")));
        assert_eq!(users[1].text.as_deref(), Some("/compact keep the tally"));
        assert_eq!(users[2].origin, Some("queued"));
        assert_eq!(
            (users[3].origin, users[3].from.as_deref()),
            (Some("agent"), Some("general-purpose"))
        );

        // Thinking is a length of time and nothing else.
        let thought = serde_json::to_string(by_kind("thinking")[0]).unwrap();
        assert!(thought.contains("\"duration_ms\":4200"), "{thought}");
        assert!(!thought.contains("reed bed"), "{thought}");

        // Each call has the result that names its id, whatever order they came in.
        let tools = by_kind("tool");
        assert_eq!(
            (tools[0].name.as_deref(), tools[0].summary.as_deref()),
            (Some("Bash"), Some("wc -l survey.csv"))
        );
        assert_eq!(
            (tools[0].status, tools[0].duration_ms),
            (Some("ok"), Some(2000))
        );
        assert_eq!(
            (
                tools[1].name.as_deref(),
                tools[1].status,
                tools[1].added,
                tools[1].removed
            ),
            (Some("Edit"), Some("ok"), Some(2), Some(1))
        );
        assert_eq!(
            (tools[2].summary.as_deref(), tools[2].status),
            (Some("cargo test tally"), Some("error"))
        );
        // A call the owner's Escape cut off, and one refused.
        assert_eq!(tools[3].status, Some("interrupted"));
        assert_eq!(tools[4].status, Some("denied"));

        let q = by_kind("question")[0];
        assert_eq!(q.status, Some("answered"));
        let asked = &q.questions.as_ref().unwrap()[0];
        assert_eq!(
            (asked.header.as_str(), asked.multi, asked.options.len()),
            ("Species", false, 2)
        );
        assert_eq!(
            q.answers.as_ref().unwrap()[0],
            (
                "Which herons count?".to_string(),
                "Grey herons only".to_string()
            )
        );
        let plan = by_kind("plan")[0];
        assert_eq!(plan.status, Some("approved"));
        assert!(plan.text.as_ref().unwrap().starts_with("# Tally plan"));
        let task = by_kind("tasks")[0];
        assert_eq!(
            (task.name.as_deref(), task.status, task.background),
            (Some("Explore"), Some("ok"), true)
        );
        assert_eq!(by_kind("turn_ended")[0].duration_ms, Some(61000));
        let compacted = by_kind("compacted")[0];
        assert_eq!(
            (compacted.trigger.as_deref(), compacted.pre_tokens),
            (Some("auto"), Some(180000))
        );

        // Two records of kinds nobody has seen, and a block of a new kind.
        assert_eq!(page.not_understood, 3);
        assert_eq!(
            page.not_understood_kinds,
            vec![
                ("hologram-state".to_string(), 1),
                ("system:tide_table".to_string(), 1),
                ("assistant:smell".to_string(), 1)
            ]
        );
        // A sub-agent's lines and Claude Code's notes to itself are not shown.
        let all = serde_json::to_string(&page).unwrap();
        assert!(!all.contains("sidechain") && !all.contains("caveat"));
    }

    /// Phase M1: the same for Codex, whose rollout file is another shape.
    #[test]
    fn a_codex_session_becomes_items_and_says_what_it_did_not_understand() {
        let (a, _dir) = following("codex", CODEX, usize::MAX);
        let page = a.conversation("s1", Range::Tail, 200).unwrap();
        assert_eq!(
            kinds(&page.items),
            vec![
                "user",
                "thinking",
                "text",
                "tool",
                "tool",
                "text",
                "turn_ended",
                "user",
                "tool",
                "interrupted"
            ],
            "{:#?}",
            page.items
        );
        let i = &page.items;
        assert_eq!(
            i[0].text.as_deref(),
            Some("How many otters are in the weir log?")
        );
        assert_eq!(i[2].phase.as_deref(), Some("commentary"));
        assert_eq!(
            (i[3].name.as_deref(), i[3].summary.as_deref(), i[3].status),
            (Some("exec"), Some("grep -c otter weir.log"), Some("done"))
        );
        assert_eq!(i[3].duration_ms, Some(1500));
        assert_eq!(
            (i[4].name.as_deref(), i[4].summary.as_deref()),
            (Some("sleep"), Some(""))
        );
        assert_eq!(i[5].phase.as_deref(), Some("final_answer"));
        assert_eq!(i[6].duration_ms, Some(9000));
        // No result ever came for the last call: the turn was aborted.
        assert_eq!(i[8].status, Some("interrupted"));
        assert_eq!(page.not_understood, 2);
        assert_eq!(
            page.not_understood_kinds,
            vec![
                ("response_item:hologram".to_string(), 1),
                ("event_msg:tide_turned".to_string(), 1)
            ]
        );
        // Codex's instructions to itself and its environment note are not shown.
        let all = serde_json::to_string(&page).unwrap();
        assert!(!all.contains("sandbox") && !all.contains("weir-station"));

        let d = a.conversation_detail("s1", &i[3].id, 0).unwrap();
        assert_eq!(
            d["input"]["fields"]["input"],
            json!("grep -c otter weir.log")
        );
        assert_eq!(d["output"]["text"], json!("7\nexit 0"));
    }

    /// Phase M1: "the cursor returns no duplicate and no gap across a file
    /// that grows". Reading a session a few lines at a time, whatever the
    /// page size, gives exactly what reading it whole gives, and a call
    /// whose result arrives later is sent again as an update.
    #[test]
    fn the_cursor_gives_no_duplicate_and_no_gap_as_the_file_grows() {
        let whole = {
            let (a, _dir) = following("claude", CLAUDE, usize::MAX);
            a.conversation("s1", Range::Tail, 200).unwrap().items
        };
        let total = CLAUDE.lines().count();
        for (step, limit) in [(1, 200), (3, 2), (7, 1), (total, 4)] {
            let (a, dir) = following("claude", CLAUDE, 0);
            let mut seen: Vec<Item> = vec![];
            let mut cursor = 0;
            let mut written = 0;
            let mut updates = 0;
            loop {
                let page = a.conversation("s1", Range::After(cursor), limit).unwrap();
                assert!(page.cursor >= cursor);
                for item in page.items {
                    assert!(
                        seen.iter().all(|s| s.id != item.id),
                        "{} came twice (step {step}, limit {limit})",
                        item.id
                    );
                    assert!(item.line > cursor);
                    seen.push(item);
                }
                for item in page.updated {
                    updates += 1;
                    let old = seen
                        .iter_mut()
                        .find(|s| s.id == item.id)
                        .expect("an update is for an item sent before");
                    *old = item;
                }
                cursor = page.cursor;
                if page.more_after {
                    continue;
                }
                if written == total {
                    break;
                }
                let to = (written + step).min(total);
                grow(&dir, CLAUDE, written, to);
                written = to;
            }
            assert_eq!(seen, whole, "step {step}, limit {limit}");
            if step == 1 {
                assert!(updates >= 8, "results arrive as updates: {updates}");
            }
        }
    }

    /// Phase M1: pages above the newest, a call's detail on demand, and the
    /// caps that keep one answer small.
    #[test]
    fn older_pages_and_capped_detail() {
        let (a, dir) = following("claude", CLAUDE, usize::MAX);
        let tail = a.conversation("s1", Range::Tail, 5).unwrap();
        assert!(tail.more_before && !tail.more_after);
        let mut all = tail.items.clone();
        let mut first = tail.first.unwrap();
        loop {
            let page = a.conversation("s1", Range::Before(first), 4).unwrap();
            assert!(page.items.iter().all(|i| i.line < first));
            let mut older = page.items.clone();
            older.extend(all);
            all = older;
            if !page.more_before {
                break;
            }
            first = page.first.unwrap();
        }
        assert_eq!(all, a.conversation("s1", Range::Tail, 200).unwrap().items);

        let tools: Vec<Item> = all.into_iter().filter(|i| i.kind == "tool").collect();
        let bash = a.conversation_detail("s1", &tools[0].id, 0).unwrap();
        assert_eq!(
            bash["input"]["fields"]["command"],
            json!("wc -l survey.csv")
        );
        // The escape codes a command printed are gone; its words are not.
        assert_eq!(
            bash["output"]["text"],
            json!("412 survey.csv\nwarning: 3 blank rows")
        );
        let edit = a.conversation_detail("s1", &tools[1].id, 0).unwrap();
        let file = &edit["diff"]["files"][0];
        assert_eq!(file["file"], json!("/home/ada/marsh/tally.py"));
        assert_eq!(file["hunks"][0]["old_start"], json!(10));
        assert_eq!(
            file["hunks"][0]["lines"],
            json!([
                " def tally(rows):",
                "-    return len(rows)",
                "+    herons = [r for r in rows if r.species == 'heron']",
                "+    return len(herons)"
            ])
        );

        // A reply longer than an item holds, and an output longer than one
        // detail answer holds: each says so, and the rest can be fetched.
        let long = "reed ".repeat(30_000);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("session.jsonl"))
            .unwrap();
        writeln!(f, "{}", json!({"type": "assistant", "timestamp": "2026-11-15T10:20:00.000Z", "message": {"role": "assistant", "content": [{"type": "text", "text": long}]}})).unwrap();
        writeln!(f, "{}", json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_long", "name": "Bash", "input": {"command": "yes reed"}}]}})).unwrap();
        writeln!(f, "{}", json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_long", "content": "cut"}]}, "toolUseResult": {"stdout": long, "stderr": "", "interrupted": false}})).unwrap();
        // A line that holds several items (versions before 2.1.3xx, and the
        // test suite's agent, write a whole reply on one line) is never
        // split between two pages.
        writeln!(f, "{}", json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "one"}, {"type": "text", "text": "two"}, {"type": "text", "text": "three"}]}})).unwrap();
        let page = a.conversation("s1", Range::Tail, 1).unwrap();
        assert_eq!(page.items.len(), 3);
        let above = a
            .conversation("s1", Range::Before(page.first.unwrap()), 1)
            .unwrap();
        assert_eq!((above.items.len(), above.items[0].kind), (1, "tool"));
        let page = a
            .conversation("s1", Range::Before(page.first.unwrap()), 2)
            .unwrap();
        let (text, tool) = (&page.items[0], &page.items[1]);
        assert!(text.more && text.text.as_ref().unwrap().len() == TEXT_CAP);
        let mut got = String::new();
        let mut offset = 0;
        loop {
            let d = a.conversation_detail("s1", &tool.id, offset).unwrap();
            assert_eq!(d["output"]["total"], json!(long.len()));
            let part = d["output"]["text"].as_str().unwrap();
            assert!(part.len() <= DETAIL_CAP);
            got.push_str(part);
            match d["output"]["next"].as_u64() {
                Some(next) => offset = next as usize,
                None => break,
            }
        }
        assert_eq!(got, long);
        let full = a.conversation_detail("s1", &text.id, 0).unwrap();
        assert_eq!(full["text"]["total"], json!(long.trim().len()));
        assert_eq!(full["text"]["next"], json!(DETAIL_CAP));
    }

    /// A session of another agent program has no conversation here: the
    /// caller is told so and shows the terminal.
    #[test]
    fn an_agent_whose_file_is_not_understood_has_no_conversation() {
        let (a, _dir) = following("antigravity", CLAUDE, usize::MAX);
        assert!(a.conversation("s1", Range::Tail, 10).is_none());
        assert!(a.conversation("never-followed", Range::Tail, 10).is_none());
    }

    /// What a long session costs. Not part of the every-change tier:
    ///   SPANDECK_CONV_BENCH=/path/to/session.jsonl cargo test -p spandeck-archive \
    ///     --release -- --ignored conversation_cost --nocapture
    /// Prints sizes and times only, never a line of the file.
    #[test]
    #[ignore = "a measurement, run by hand on a real session file"]
    fn conversation_cost() {
        let Ok(path) = std::env::var("SPANDECK_CONV_BENCH") else {
            return;
        };
        let agent = std::env::var("SPANDECK_CONV_AGENT").unwrap_or_else(|_| "claude".into());
        let dir = tempfile::tempdir().unwrap();
        let a = Archive::open(&dir.path().join("archive.db")).unwrap();
        a.follow("bench", &agent, &path, None, None);
        let t = Instant::now();
        let lines = a.poll();
        let copied = t.elapsed();
        let t = Instant::now();
        let page = a.conversation("bench", Range::Tail, 50).unwrap();
        let first = t.elapsed();
        let t = Instant::now();
        let again = a
            .conversation("bench", Range::After(page.cursor), 50)
            .unwrap();
        let idle = t.elapsed();
        let t = Instant::now();
        let mut before = page.first.unwrap_or(0);
        let mut pages = 0;
        let mut items = page.items.len();
        while let Some(p) = a
            .conversation("bench", Range::Before(before), 200)
            .filter(|p| !p.items.is_empty())
        {
            pages += 1;
            items += p.items.len();
            before = p.first.unwrap();
        }
        let conv = a.conv("bench").unwrap();
        let c = conv.lock().unwrap();
        println!(
            "file {} MB, {lines} lines; archive copy {copied:?}; first page (index built) {first:?}; \
             a poll with nothing new {idle:?} ({} items); every older page ({pages} of 200) {:?}; \
             {items} items, index {} KB; not understood {} {:?}",
            std::fs::metadata(&path).map(|m| m.len() >> 20).unwrap_or(0),
            again.items.len(),
            t.elapsed(),
            (c.slots.len() * std::mem::size_of::<Slot>()
                + c.closes.len() * std::mem::size_of::<(u64, usize)>())
                >> 10,
            c.unknown,
            c.unknown_kinds,
        );
    }
}
