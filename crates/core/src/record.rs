//! A project's record: what its agents did, how long each was in which
//! state, what they said to each other and what the owner did, written by
//! the station from what it holds (18 Nov 2026, the owner's request, after
//! such a record was put together by hand from the ship's log, the
//! messages, the notes and the usage totals).
//!
//! `project.record {project, since_ms?, until_ms?, all?}` answers the
//! numbers as JSON and the same as one Markdown document (`markdown`). It
//! is the owner's alone (rpc.rs). It only reads: the ship's log a page at
//! a time, the messages, the notes, the approvals and the usage totals.
//!
//! Nothing in it is a judgement. "What looks wrong" lists only what can be
//! counted, with the count, the limit and the entries it was counted from.
//! What the station does not record is said in the document (`missing`),
//! never filled in.
//!
//! Every time is UTC and every duration is in milliseconds in the JSON
//! (`*_ms`); the document says so. Text that came from an agent (a
//! message, a note, a name, a summary) is data: in the document it is only
//! ever inside a fenced block or a code span whose fence is longer than
//! any run of backticks in it, so it cannot start a heading or end its
//! block.

use std::collections::{BTreeMap, HashSet};

use serde_json::{json, Value};

use crate::daemon::{invalid, Daemon, Error, PaneMeta, Result};
use crate::db::LogRow;
use crate::paths::now_ms;
use crate::relay::split_address;
use crate::tailnet::rfc3339;

/// Entries of the ship's log read in one go.
const LOG_PAGE: u32 = 2000;
/// Entries read at most: the record then says it stopped there.
const LOG_CAP: usize = 200_000;
/// Messages looked at, at most.
const MSG_SCAN_CAP: usize = 200_000;
/// Messages of the thread shown unless `all` is asked for: the newest.
pub const THREAD_CAP: usize = 200;
/// Notes shown unless `all`.
const NOTES_CAP: usize = 300;
/// Lines of the timeline shown unless `all`.
const TIMELINE_CAP: usize = 600;
/// Minutes a timeline is worked out for (two weeks).
const TIMELINE_MINUTES: u64 = 20_160;
/// "What looks wrong": more state changes than this within one minute.
pub const FLAP_N: usize = 10;
/// "What looks wrong": a message unread, or an approval waiting, for more
/// than this many minutes.
pub const WAIT_MINUTES: u64 = 5;
const MINUTE: u64 = 60_000;

const STATES: [&str; 9] = [
    "working",
    "needs_you",
    "idle",
    "done",
    "error",
    "exited",
    "closed",
    "not_started",
    "unknown",
];

fn state_key(s: &str) -> &'static str {
    STATES
        .iter()
        .copied()
        .find(|k| *k == s || (s == "blocked" && *k == "needs_you"))
        .unwrap_or("unknown")
}

fn state_letter(s: &str) -> char {
    match s {
        "working" => 'W',
        "needs_you" => 'N',
        "idle" => 'I',
        "done" => 'D',
        "error" => 'E',
        "exited" => 'X',
        "closed" => 'C',
        "not_started" => '-',
        _ => '?',
    }
}

/// A pane of the project, as the station last knew it.
pub(crate) struct Pane {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub agent: String,
    pub program: String,
    pub model: Option<String>,
    pub live: bool,
    /// Its state now, for a pane whose state did not change in the period.
    pub state_now: Option<String>,
    /// Its row of `usage.summary`, or null.
    pub usage: Value,
}

pub(crate) struct Msg {
    pub id: String,
    pub ts: u64,
    pub from: String,
    pub to: String,
    pub kind: String,
    pub body: String,
    pub status: String,
    pub project: Option<String>,
    pub received: Option<u64>,
    pub read: Option<u64>,
    pub acted: Option<u64>,
}

pub(crate) struct Note {
    pub ts: u64,
    pub kind: String,
    pub author: Option<String>,
    pub text: String,
}

/// An approval as the approvals table has it: for one the log does not
/// say the end of (an approval that ran out is not logged).
pub(crate) struct Stored {
    pub status: String,
    pub answered: Option<u64>,
    pub by: Option<String>,
}

/// Everything a record is made from.
pub(crate) struct Input {
    pub project: String,
    pub station: String,
    pub generated_ms: u64,
    pub since_ms: Option<u64>,
    pub until_ms: Option<u64>,
    pub all: bool,
    pub panes: Vec<Pane>,
    /// The ship's log of the project, oldest first, without tool calls.
    pub log: Vec<LogRow>,
    pub log_truncated: bool,
    /// Tool calls per pane: (all, failed).
    pub tools: BTreeMap<String, (u64, u64)>,
    pub station_starts: Vec<u64>,
    pub messages: Vec<Msg>,
    pub messages_truncated: bool,
    /// Messages left out because the bare name they were sent to was also
    /// the name of another project's pane that was open then.
    pub messages_shared_name: u64,
    pub notes: Vec<Note>,
    pub activity_count: i64,
    pub approvals: BTreeMap<String, Stored>,
}

/// When a pane was open under its name: from its start until it ended
/// (`None`: it has not).
pub(crate) struct Open {
    pub name: String,
    pub from: u64,
    pub to: Option<u64>,
}

impl Open {
    fn at(&self, name: &str, ts: u64) -> bool {
        self.name == name && self.from <= ts && self.to.is_none_or(|to| ts <= to)
    }
}

/// Whether a message is this project's.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Whose {
    Ours,
    Not,
    /// Sent to a bare name that a pane of this project and a pane of
    /// another both had at that time: the station kept no project for it.
    SharedName,
}

/// What tells whose a message is: the project, its panes' ids, and when
/// its panes (`own`) and every other pane of the station (`others`) were
/// open under which name.
pub(crate) struct Panes<'a> {
    pub project: &'a str,
    pub station: &'a str,
    pub ids: &'a HashSet<&'a str>,
    pub own: &'a [Open],
    pub others: &'a [Open],
}

impl Panes<'_> {
    /// A message is the project's when it was sent by one of its panes or
    /// to one of them: a pane's id, or `project/name`. Never by a pane's
    /// name alone: other projects have a `builder` and a `critic` too. A
    /// bare name (a message stored before the station wrote the project
    /// into the address) is the project's only when a pane of the project
    /// had the name at that time and no other pane did.
    pub(crate) fn whose(&self, m: &Msg) -> Whose {
        let here = |addr: &str| -> Option<String> {
            let (name, station) = split_address(addr);
            station
                .is_none_or(|s| s == self.station)
                .then(|| name.to_string())
        };
        let ours = |name: &str| {
            self.ids.contains(name)
                || name
                    .split_once('/')
                    .is_some_and(|(project, _)| project == self.project)
        };
        if here(&m.from).is_some_and(|name| ours(&name)) {
            return Whose::Ours;
        }
        match here(&m.to) {
            Some(name) if ours(&name) => Whose::Ours,
            Some(name) if !name.contains('/') && self.own.iter().any(|o| o.at(&name, m.ts)) => {
                if self.others.iter().any(|o| o.at(&name, m.ts)) {
                    Whose::SharedName
                } else {
                    Whose::Ours
                }
            }
            _ => Whose::Not,
        }
    }
}

fn data_str<'a>(row: &'a LogRow, key: &str) -> &'a str {
    row.data.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// {n, median_ms, worst_ms} of some waits.
fn stats(mut v: Vec<u64>) -> Value {
    v.sort_unstable();
    let median = match v.len() {
        0 => None,
        n if n % 2 == 1 => Some(v[n / 2]),
        n => Some((v[n / 2 - 1] + v[n / 2]) / 2),
    };
    json!({"n": v.len(), "median_ms": median, "worst_ms": v.last()})
}

/// The state a pane was in at each change, oldest first.
struct Line(Vec<(u64, &'static str)>);

impl Line {
    fn at(&self, ts: u64) -> &'static str {
        let i = self.0.partition_point(|(t, _)| *t <= ts);
        self.0[i.saturating_sub(1)].1
    }

    /// Milliseconds in each state within `[from, to)`.
    fn spent(&self, from: u64, to: u64) -> BTreeMap<&'static str, u64> {
        let mut out = BTreeMap::new();
        for (i, (start, state)) in self.0.iter().enumerate() {
            let end = self.0.get(i + 1).map(|n| n.0).unwrap_or(u64::MAX);
            let (a, b) = ((*start).max(from), end.min(to));
            if b > a {
                *out.entry(*state).or_insert(0) += b - a;
            }
        }
        out
    }

    /// The state most of `[from, to)` was spent in.
    fn mostly(&self, from: u64, to: u64) -> &'static str {
        self.spent(from, to)
            .into_iter()
            .max_by_key(|(_, ms)| *ms)
            .map(|(s, _)| s)
            .unwrap_or_else(|| self.at(from))
    }
}

/// What the owner did, in words, from one entry of the ship's log.
fn owner_words(row: &LogRow) -> Option<String> {
    let d = |k: &str| data_str(row, k);
    Some(match row.kind.as_str() {
        "approval.resolved" => format!("answered an approval: {}", d("status")),
        "question.answered" => format!("answered a {}: {}", d("kind"), d("status")),
        "question.released" => format!("let a {} go to the terminal", d("kind")),
        "pane.trust_answered" => format!("the folder-trust question was answered ({})", d("how")),
        "pane.hooks_trusted" => format!("the review of hooks was answered ({})", d("how")),
        "pane.restarted" => "restarted the pane".to_string(),
        "pane.typed" => "typed into the pane (what was typed is not recorded)".to_string(),
        "pane.stopped" => "stopped the pane's turn".to_string(),
        "pane.stop_not_reading" => "asked the pane to stop; it was not reading".to_string(),
        "pane.paused" => "paused the pane".to_string(),
        "pane.unpaused" => "ended the pane's pause".to_string(),
        "pane.released" => "released the pane".to_string(),
        "pane.killed" => "killed the pane's program".to_string(),
        "pane.closed" => "closed the pane".to_string(),
        "pane.model" => "chose the pane's model".to_string(),
        "pane.pick" => "chose a row of a list in the pane".to_string(),
        "project.added" => "added the project".to_string(),
        "project.opened" => "opened the project".to_string(),
        "project.closed" => "closed the project".to_string(),
        "project.linked" => format!("linked the project with {}", code(d("with"))),
        "project.unlinked" => format!("ended the project's link with {}", code(d("with"))),
        "signoff.given" => "signed off the project's commit".to_string(),
        k if k.starts_with("msg.") || k == "pane.state" || k == "agent.tool" => return None,
        k => k.to_string(),
    })
}

/// The numbers of a record, from what was gathered. Nothing here reads
/// the station: the same input gives the same record.
pub(crate) fn build(inp: &Input) -> Value {
    // ------------------------------------------------------------ names
    let own = format!("{}/", inp.project);
    let bare = |addr: &str| -> String {
        let (name, station) = split_address(addr);
        if station.is_some_and(|s| s != inp.station) {
            return addr.to_string();
        }
        name.strip_prefix(&own).unwrap_or(name).to_string()
    };
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, p) in inp.panes.iter().enumerate() {
        // A name a closed pane had and a running one has is the running one's.
        if p.live || !index.contains_key(p.name.as_str()) {
            index.insert(&p.name, i);
        }
        index.insert(&p.id, i);
    }
    let pane_of = |addr: &str| index.get(bare(addr).as_str()).copied();
    let name_of = |pane: Option<&str>| -> String {
        pane.and_then(|id| index.get(id))
            .map(|i| inp.panes[*i].name.clone())
            .or_else(|| pane.map(String::from))
            .unwrap_or_default()
    };

    // ----------------------------------------------------------- period
    let times = inp
        .log
        .iter()
        .map(|l| l.ts)
        .chain(inp.messages.iter().map(|m| m.ts))
        .chain(inp.notes.iter().map(|n| n.ts));
    let first = times.clone().min();
    let last = times.max();
    let (first, last) = match (first, last) {
        (Some(f), Some(l)) => (f, l),
        _ => (0, 0),
    };
    let empty = first == 0 && last == 0;
    // What "still" means for something unanswered: the end of the period.
    let end = inp.until_ms.unwrap_or(last).max(last);
    let wall = last - first;

    // ----------------------------------------------------------- states
    let mut by_pane: BTreeMap<&str, Vec<&LogRow>> = BTreeMap::new();
    for row in &inp.log {
        if let Some(p) = row.pane.as_deref() {
            by_pane.entry(p).or_default().push(row);
        }
    }
    let lines: Vec<Line> = inp
        .panes
        .iter()
        .map(|p| {
            let rows = by_pane.get(p.id.as_str()).cloned().unwrap_or_default();
            let moves = |r: &&&LogRow| {
                matches!(
                    r.kind.as_str(),
                    "pane.created" | "pane.state" | "pane.exited" | "pane.resumed" | "pane.closed"
                )
            };
            let before = match rows.iter().find(moves) {
                Some(r) if r.kind == "pane.created" => "not_started",
                Some(r) if r.kind == "pane.state" => state_key(data_str(r, "from")),
                Some(_) => "unknown",
                None => p.state_now.as_deref().map(state_key).unwrap_or("unknown"),
            };
            let mut line = vec![(first, before)];
            for r in rows.iter().filter(moves) {
                let state = match r.kind.as_str() {
                    "pane.created" | "pane.resumed" => "idle",
                    "pane.state" => state_key(data_str(r, "state")),
                    "pane.exited" => "exited",
                    _ => "closed",
                };
                if line.last().is_some_and(|(_, s)| *s != state) {
                    line.push((r.ts.max(first), state));
                }
            }
            Line(line)
        })
        .collect();

    // -------------------------------------------------------- approvals
    let mut ends: BTreeMap<&str, (&LogRow, String)> = BTreeMap::new();
    for row in &inp.log {
        let status = match row.kind.as_str() {
            "approval.resolved" | "question.answered" => data_str(row, "status").to_string(),
            "question.released" => "left to the terminal".to_string(),
            _ => continue,
        };
        ends.entry(data_str(row, "id")).or_insert((row, status));
    }
    let approvals: Vec<Value> = inp
        .log
        .iter()
        .filter(|r| r.kind == "approval.requested")
        .map(|r| {
            let id = data_str(r, "id");
            let (answered, by, status) = match (ends.get(id), inp.approvals.get(id)) {
                (Some((e, status)), _) => (Some(e.ts), e.actor.clone(), status.clone()),
                (None, Some(s)) => (s.answered, s.by.clone(), s.status.clone()),
                (None, None) => (None, None, "no answer recorded".to_string()),
            };
            let answered = answered.filter(|a| *a >= r.ts);
            json!({
                "id": id, "pane": r.pane, "name": name_of(r.pane.as_deref()),
                "kind": data_str(r, "kind"), "summary": data_str(r, "summary"),
                "asked_ms": r.ts, "answered_ms": answered, "by": by, "status": status,
                "wait_ms": answered.map(|a| a - r.ts),
                "waiting_at_end_ms": answered.is_none().then(|| end.saturating_sub(r.ts)),
            })
        })
        .collect();

    // ----------------------------------------------------------- agents
    let mut flags: Vec<Value> = vec![];
    let agents: Vec<Value> = inp
        .panes
        .iter()
        .zip(&lines)
        .map(|(p, line)| {
            let rows = by_pane.get(p.id.as_str()).cloned().unwrap_or_default();
            let mut spent: BTreeMap<&str, u64> = STATES.iter().map(|s| (*s, 0)).collect();
            // `last + 1` would count a millisecond that is not in the wall
            // time: the sum is the wall time exactly.
            spent.extend(line.spent(first, last));
            let changes: Vec<u64> = rows
                .iter()
                .filter(|r| r.kind == "pane.state" && data_str(r, "state") != data_str(r, "from"))
                .map(|r| r.ts)
                .collect();
            // The most changes that began within any one minute.
            let (mut most, mut most_at, mut lo) = (0, 0, 0);
            for hi in 0..changes.len() {
                while changes[hi] - changes[lo] >= MINUTE {
                    lo += 1;
                }
                if hi + 1 - lo > most {
                    (most, most_at) = (hi + 1 - lo, changes[lo]);
                }
            }
            if most > FLAP_N {
                flags.push(json!({
                    "what": "state_changes", "pane": p.id, "name": p.name,
                    "count": most, "limit": FLAP_N, "from_ms": most_at,
                    "evidence": format!("{most} `pane.state` entries with a new state within one minute from {} (limit: more than {FLAP_N})", rfc3339(most_at)),
                }));
            }
            let work_periods = rows
                .iter()
                .filter(|r| {
                    r.kind == "pane.state"
                        && data_str(r, "state") == "working"
                        && data_str(r, "from") != "working"
                })
                .count();
            let restarts: Vec<Value> = rows
                .iter()
                .filter(|r| r.kind == "pane.restarted")
                .map(|r| json!({"ts_ms": r.ts, "by": r.actor}))
                .collect();
            let resumed = rows.iter().filter(|r| r.kind == "pane.resumed").count();
            // Marked exited, and running again.
            let mut exited: Option<&LogRow> = None;
            for r in &rows {
                match (exited, r.kind.as_str()) {
                    (_, "pane.exited") => exited = Some(*r),
                    (Some(e), "pane.created" | "pane.resumed" | "pane.state" | "agent.tool") => {
                        let restarted = rows.iter().find(|x| {
                            x.kind == "pane.restarted" && x.ts >= e.ts && x.ts <= r.ts + 5000
                        });
                        let how = match (r.kind.as_str(), restarted) {
                            (_, Some(x)) => format!(
                                "restarted by {}",
                                x.actor.as_deref().unwrap_or("nobody the log names")
                            ),
                            ("pane.resumed", _) => {
                                "started again by the station after its own restart".to_string()
                            }
                            ("pane.created", _) => {
                                "started again; the log has no restart for it".to_string()
                            }
                            (k, _) => format!("the log has a `{k}` for it while it was marked exited"),
                        };
                        flags.push(json!({
                            "what": "exited_and_running_again", "pane": p.id, "name": p.name,
                            "exited_ms": e.ts, "again_ms": r.ts, "how": how,
                            "evidence": format!("`pane.exited` at {} ({}), `{}` at {}: {how}", rfc3339(e.ts), e.data["exit"], r.kind, rfc3339(r.ts)),
                        }));
                        exited = None;
                    }
                    _ => {}
                }
            }
            // A nudge typed again for mail that was still unread.
            let again: Vec<&&LogRow> = rows
                .iter()
                .filter(|r| {
                    r.kind == "msg.nudged"
                        && r.data["typed"] == json!(true)
                        && r.data["tries"].as_u64().unwrap_or(0) >= 2
                })
                .collect();
            if let Some(worst) = again.iter().max_by_key(|r| r.data["tries"].as_u64()) {
                let gave_up = rows.iter().any(|r| r.kind == "msg.nudge_gave_up");
                flags.push(json!({
                    "what": "nudge_typed_again", "pane": p.id, "name": p.name,
                    "times": again.len(), "most_tries": worst.data["tries"], "gave_up": gave_up,
                    "evidence": format!(
                        "{} `msg.nudged` entries with `typed: true` and `tries` of 2 or more (the most: {} at {}){}",
                        again.len(), worst.data["tries"], rfc3339(worst.ts),
                        if gave_up { "; a `msg.nudge_gave_up` entry follows" } else { "" }
                    ),
                }));
            }
            let mine: Vec<&Value> = approvals
                .iter()
                .filter(|a| a["pane"].as_str() == Some(p.id.as_str()))
                .collect();
            let waits: Vec<u64> = mine.iter().filter_map(|a| a["wait_ms"].as_u64()).collect();
            let all = &p.usage["all"];
            let reported = all["total_tokens"].as_i64().is_some_and(|t| t > 0);
            let (tools, failed) = inp.tools.get(&p.id).copied().unwrap_or((0, 0));
            json!({
                "pane": p.id, "name": p.name, "role": p.role, "agent": p.agent,
                "program": p.program, "model": p.model, "running": p.live,
                "time_in_state_ms": spent,
                "state_changes": changes.len(),
                "most_state_changes_in_a_minute": most,
                "work_periods": work_periods,
                "tool_calls": tools, "tool_calls_failed": failed,
                "turns": all["turns"],
                "usage": {
                    "reported": reported,
                    "input_tokens": all["input_tokens"], "output_tokens": all["output_tokens"],
                    "cache_read_tokens": all["cache_read_tokens"],
                    "cache_write_tokens": all["cache_write_tokens"],
                    "total_tokens": all["total_tokens"],
                    "cost_estimate_usd": all["cost_estimate"],
                    "unpriced_models": all["unpriced_models"],
                    "covers": "the pane's whole time on this station, not only the period",
                },
                "restarts": restarts.len(), "restarted": restarts,
                "resumed_after_station_restart": resumed,
                "approvals": {
                    "asked": mine.len(), "answered": waits.len(),
                    "wait_total_ms": waits.iter().sum::<u64>(),
                    "wait_longest_ms": waits.iter().max(),
                },
            })
        })
        .collect();
    for a in &approvals {
        let waited = a["wait_ms"].as_u64().or(a["waiting_at_end_ms"].as_u64());
        if let Some(ms) = waited.filter(|ms| *ms > WAIT_MINUTES * MINUTE) {
            flags.push(json!({
                "what": "approval_waited", "pane": a["pane"], "name": a["name"], "id": a["id"],
                "waited_ms": ms, "limit_minutes": WAIT_MINUTES, "answered": !a["wait_ms"].is_null(),
                "evidence": format!(
                    "asked at {}, {} (limit: more than {WAIT_MINUTES} minutes)",
                    rfc3339(a["asked_ms"].as_u64().unwrap_or(0)),
                    match a["answered_ms"].as_u64() {
                        Some(t) => format!("answered at {} after {}", rfc3339(t), dur(ms)),
                        None => format!("not answered by the end of the period, {} later ({})", dur(ms), a["status"].as_str().unwrap_or("")),
                    }
                ),
            }));
        }
    }

    // --------------------------------------------------------- messages
    let mut pairs: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut waits: BTreeMap<&str, (Vec<u64>, Vec<u64>, u64)> = BTreeMap::new();
    let mut thread: Vec<Value> = vec![];
    for m in &inp.messages {
        let (from, to) = (bare(&m.from), bare(&m.to));
        *pairs.entry((from.clone(), to.clone())).or_insert(0) += 1;
        let to_state = pane_of(&m.to).map(|i| lines[i].at(m.ts));
        let group = match to_state {
            Some("working") => "recipient_working",
            Some(_) => "recipient_not_working",
            None => "recipient_not_a_pane_here",
        };
        let w = waits.entry(group).or_default();
        let delivered = m.received.map(|r| r.saturating_sub(m.ts));
        let read = m.read.map(|r| r.saturating_sub(m.ts));
        w.0.extend(delivered);
        match read {
            Some(r) => w.1.push(r),
            None => w.2 += 1,
        }
        let unread_for = read.unwrap_or_else(|| end.saturating_sub(m.ts));
        if unread_for > WAIT_MINUTES * MINUTE && !m.status.starts_with("undelivered") {
            flags.push(json!({
                "what": "message_unread", "id": m.id, "from": from, "to": to,
                "unread_ms": unread_for, "limit_minutes": WAIT_MINUTES, "read": m.read.is_some(),
                "evidence": format!(
                    "sent at {}, {} (limit: more than {WAIT_MINUTES} minutes)",
                    rfc3339(m.ts),
                    match m.read {
                        Some(t) => format!("read at {} after {}", rfc3339(t), dur(unread_for)),
                        None => format!("not read by the end of the period, {} later", dur(unread_for)),
                    }
                ),
            }));
        }
        thread.push(json!({
            "id": m.id, "ts_ms": m.ts, "from": from, "to": to, "kind": m.kind, "body": m.body,
            "status": m.status, "delivered_ms": m.received, "read_ms": m.read, "acted_ms": m.acted,
            "delivery_latency_ms": delivered, "read_latency_ms": read,
            "recipient_state_when_sent": to_state,
            "other_project": m.project.as_deref().filter(|p| *p != inp.project),
        }));
    }
    let count = thread.len();
    let cut = if inp.all {
        0
    } else {
        count.saturating_sub(THREAD_CAP)
    };
    let thread = thread.split_off(cut);
    let latency: BTreeMap<&str, Value> = [
        "recipient_working",
        "recipient_not_working",
        "recipient_not_a_pane_here",
    ]
    .into_iter()
    .map(|g| {
        let (d, r, unread) = waits.remove(g).unwrap_or_default();
        let n = r.len() as u64 + unread;
        (
            g,
            json!({"messages": n, "delivery": stats(d), "read": stats(r), "not_read": unread}),
        )
    })
    .collect();

    // ------------------------------------------------------------ notes
    let notes_cut = if inp.all {
        0
    } else {
        inp.notes.len().saturating_sub(NOTES_CAP)
    };
    let notes: Vec<Value> = inp.notes[notes_cut..]
        .iter()
        .map(|n| json!({"ts_ms": n.ts, "kind": n.kind, "author": n.author, "text": n.text}))
        .collect();

    // ------------------------------------------------------ owner's part
    let mut owner: Vec<Value> = vec![];
    for row in &inp.log {
        let by_owner = row.actor.as_deref().is_some_and(|a| a.starts_with("you"));
        let trust = matches!(
            row.kind.as_str(),
            "pane.trust_answered" | "pane.hooks_trusted"
        );
        if !(by_owner || trust) {
            continue;
        }
        if let Some(what) = owner_words(row) {
            owner.push(json!({
                "ts_ms": row.ts, "kind": row.kind, "what": what, "by": row.actor,
                "pane": row.pane, "name": name_of(row.pane.as_deref()),
            }));
        }
    }
    for m in &inp.messages {
        if bare(&m.from) == "you" {
            owner.push(json!({
                "ts_ms": m.ts, "kind": "msg.sent", "by": "you",
                "what": format!("sent a {}", m.kind),
                "pane": pane_of(&m.to).map(|i| inp.panes[i].id.clone()), "name": bare(&m.to),
            }));
        }
    }
    owner.sort_by_key(|o| o["ts_ms"].as_u64());

    // --------------------------------------------------------- timeline
    let minutes = if empty {
        0
    } else {
        last / MINUTE - first / MINUTE + 1
    };
    let mut timeline: Vec<Value> = vec![];
    let timeline_skipped = minutes > TIMELINE_MINUTES;
    if !timeline_skipped {
        let mut sent: BTreeMap<u64, Vec<String>> = BTreeMap::new();
        for m in &inp.messages {
            sent.entry(m.ts / MINUTE).or_default().push(format!(
                "{} -> {} ({})",
                bare(&m.from),
                bare(&m.to),
                m.kind
            ));
        }
        let mut open: Option<(u64, u64, Vec<&'static str>)> = None;
        let mut close = |open: &mut Option<(u64, u64, Vec<&'static str>)>, sent: Vec<String>| {
            if let Some((from, n, states)) = open.take() {
                let states: BTreeMap<&str, &str> = inp
                    .panes
                    .iter()
                    .map(|p| p.name.as_str())
                    .zip(states)
                    .collect();
                timeline.push(
                    json!({"from_ms": from * MINUTE, "minutes": n, "states": states, "sent": sent}),
                );
            }
        };
        for minute in first / MINUTE..first / MINUTE + minutes {
            let (a, b) = (
                (minute * MINUTE).max(first),
                ((minute + 1) * MINUTE).min(last + 1),
            );
            let states: Vec<&'static str> = lines.iter().map(|l| l.mostly(a, b)).collect();
            let said = sent.remove(&minute).unwrap_or_default();
            let same = said.is_empty() && open.as_ref().is_some_and(|(_, _, s)| *s == states);
            if let (true, Some((_, n, _))) = (same, &mut open) {
                *n += 1;
                continue;
            }
            close(&mut open, vec![]);
            open = Some((minute, 1, states));
            if !said.is_empty() {
                close(&mut open, said);
            }
        }
        close(&mut open, vec![]);
    }
    let timeline_cut = if inp.all {
        0
    } else {
        timeline.len().saturating_sub(TIMELINE_CAP)
    };
    let timeline = timeline.split_off(timeline_cut);

    // ------------------------------------------- what is not recorded
    let mut missing = vec![
        "Tokens and cost are each pane's totals for its whole time on this station: the station keeps no count of tokens between two times, so they are not cut to the period.".to_string(),
        "The model is the one last known for each pane: the ship's log does not keep which model a pane ran at an earlier time.".to_string(),
        "A nudge's log entry does not name the messages it was for. \"Typed again\" in section 7 means: typed again with no new mail, no change of the pane's state and no typing by the owner since the nudge before.".to_string(),
        "What was typed into a pane is not recorded, only that someone typed: once when they start, and again after five quiet minutes.".to_string(),
        "How a working agent was told of its mail (by its hooks) is not in the log: the read latency is from the message's own record (stored, received, read).".to_string(),
    ];
    if !inp.station_starts.is_empty() {
        missing.push(format!(
            "The station started {} time(s) in the period ({}). While it was away nothing was recorded: that time is counted in the state each pane was last recorded in.",
            inp.station_starts.len(),
            inp.station_starts.iter().map(|t| rfc3339(*t)).collect::<Vec<_>>().join(", ")
        ));
    }
    if inp.log_truncated {
        missing.push(format!(
            "The ship's log of this period has more than {LOG_CAP} entries for the project; only the oldest {LOG_CAP} were read. Ask for a shorter period."
        ));
    }
    if inp.messages_truncated {
        missing.push(format!(
            "More than {MSG_SCAN_CAP} messages of the station fall in the period; only the oldest {MSG_SCAN_CAP} were looked at."
        ));
    }
    if inp.messages_shared_name > 0 {
        missing.push(format!(
            "{} message(s) of the period were sent to a pane's bare name while a pane of another project had the same name; the station kept no project for them, so they are not in this record.",
            inp.messages_shared_name
        ));
    }
    if timeline_skipped {
        missing.push(format!(
            "The period is {minutes} minutes long; a minute-by-minute timeline is made for at most {TIMELINE_MINUTES}. Ask for a shorter period."
        ));
    }

    json!({
        "project": inp.project, "station": inp.station, "generated_ms": inp.generated_ms,
        "time_zone": "UTC",
        "units": "every `*_ms` time is milliseconds since 1970-01-01T00:00:00Z; every other `*_ms` is a duration in milliseconds; costs are US dollars, an estimate at API list prices",
        "period": {
            "since_ms": inp.since_ms, "until_ms": inp.until_ms,
            "first_event_ms": (!empty).then_some(first), "last_event_ms": (!empty).then_some(last),
            "wall_ms": wall,
        },
        "log": {"entries": inp.log.len(), "truncated": inp.log_truncated, "station_starts_ms": inp.station_starts},
        "agents": agents,
        "approvals": approvals,
        "messages": {
            "count": count,
            "pairs": pairs.iter().map(|((f, t), n)| json!({"from": f, "to": t, "count": n})).collect::<Vec<_>>(),
            "latency": latency,
            "thread": thread, "earlier_not_shown": cut,
        },
        "notes": {"notes": notes, "earlier_not_shown": notes_cut, "activity_count": inp.activity_count},
        "owner": owner,
        "timeline": {"lines": timeline, "earlier_not_shown": timeline_cut, "minutes": minutes},
        "looks_wrong": flags,
        "limits": {
            "state_changes_in_a_minute": FLAP_N, "unread_minutes": WAIT_MINUTES,
            "approval_wait_minutes": WAIT_MINUTES,
        },
        "missing": missing,
    })
}

/// A duration in words: "350 ms", "4.2 s", "3 min 05 s", "1 h 02 min 03 s".
pub fn dur(ms: u64) -> String {
    let s = ms / 1000;
    match ms {
        0..1000 => format!("{ms} ms"),
        1000..60_000 => format!("{}.{} s", s, ms % 1000 / 100),
        60_000..3_600_000 => format!("{} min {:02} s", s / 60, s % 60),
        _ => format!("{} h {:02} min {:02} s", s / 3600, s / 60 % 60, s % 60),
    }
}

fn longest_run(text: &str, of: char) -> usize {
    let (mut longest, mut run) = (0, 0);
    for c in text.chars() {
        run = if c == of { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// Text that is data, as a block of the document: fenced, with a fence
/// longer than any run of backticks in it, so nothing in it ends the block
/// or is read as Markdown.
pub fn fenced(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect();
    let fence = "`".repeat(longest_run(&clean, '`').max(2) + 1);
    format!("{fence}text\n{}\n{fence}\n", clean.trim_end_matches('\n'))
}

/// Text that is data, within a line: a code span on one line, its
/// delimiter longer than any run of backticks in it.
pub fn code(text: &str) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return "(none)".to_string();
    }
    let delim = "`".repeat(longest_run(clean, '`') + 1);
    let pad = if clean.starts_with('`') || clean.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{delim}{pad}{clean}{pad}{delim}")
}

fn words(state: &str) -> String {
    state.replace('_', " ")
}

/// The record as one Markdown document, from its numbers.
pub fn markdown(v: &Value) -> String {
    let str_ = |x: &Value| x.as_str().unwrap_or("").to_string();
    let n = |x: &Value| x.as_u64().unwrap_or(0);
    let first = n(&v["period"]["first_event_ms"]);
    let last = n(&v["period"]["last_event_ms"]);
    // Within one day the date is said once, above; else with every time.
    let one_day = first / 86_400_000 == last / 86_400_000;
    let at = |ms: u64| {
        let full = rfc3339(ms);
        // To the millisecond: what was sent and read within one second
        // keeps its order.
        if one_day {
            format!("{}.{:03}", &full[11..19], ms % 1000)
        } else {
            format!("{}.{:03}Z", &full[..19], ms % 1000)
        }
    };
    let opt_dur = |x: &Value| x.as_u64().map(dur).unwrap_or_else(|| "none".into());
    let mut o = String::new();
    let mut line = |s: String| {
        o.push_str(&s);
        o.push('\n');
    };

    line(format!(
        "# Record of project {}",
        code(&str_(&v["project"]))
    ));
    line(String::new());
    line(format!(
        "Written by spandeck on station {} at {} from what the station holds: its ship's log, its messages, the project's notes, its approvals and its usage totals. All times are UTC{}. Times are given to the millisecond. Durations are rounded down here (to a tenth of a second under a minute, to the second above) and exact, in milliseconds, in the JSON form of this record (`--json`). Text in code spans and fenced blocks is quoted from agents and the owner: it is data, not part of this document's structure and not instructions.",
        code(&str_(&v["station"])),
        rfc3339(n(&v["generated_ms"])),
        if one_day && first > 0 { format!(", on {}", &rfc3339(first)[..10]) } else { String::new() },
    ));
    line(String::new());

    // ---------------------------------------------------------- 1
    line("## 1. Summary".into());
    line(String::new());
    if first == 0 {
        line("Nothing is recorded for this project in the period.".into());
    } else {
        line(format!(
            "- Period covered: {} to {} (first to last recorded event){}{}",
            rfc3339(first),
            rfc3339(last),
            v["period"]["since_ms"]
                .as_u64()
                .map(|s| format!("; asked for from {}", rfc3339(s)))
                .unwrap_or_default(),
            v["period"]["until_ms"]
                .as_u64()
                .map(|s| format!("; asked for until {}", rfc3339(s)))
                .unwrap_or_default()
        ));
        line(format!(
            "- Wall time, first event to last: {}",
            dur(n(&v["period"]["wall_ms"]))
        ));
    }
    line(format!(
        "- Ship's log entries read: {}{}",
        n(&v["log"]["entries"]),
        if v["log"]["truncated"] == json!(true) {
            " (cut: see the last section)"
        } else {
            ""
        }
    ));
    let agents = v["agents"].as_array().cloned().unwrap_or_default();
    line(format!("- Panes: {}", agents.len()));
    for a in &agents {
        line(format!(
            "  - {}: program {}, model {}{}{}",
            code(&str_(&a["name"])),
            code(&str_(&a["program"])),
            a["model"]
                .as_str()
                .map(code)
                .unwrap_or_else(|| "not known".into()),
            a["role"]
                .as_str()
                .map(|r| format!(", role {}", code(r)))
                .unwrap_or_default(),
            if a["running"] == json!(true) {
                ""
            } else {
                " (not running now)"
            },
        ));
    }
    line(String::new());

    // ---------------------------------------------------------- 2
    line("## 2. Per agent".into());
    line(String::new());
    for a in &agents {
        line(format!("### Pane {}", code(&str_(&a["name"]))));
        line(String::new());
        let t = &a["time_in_state_ms"];
        let shown: Vec<String> = STATES
            .iter()
            .filter(|s| n(&t[**s]) > 0 || matches!(**s, "working" | "needs_you" | "idle" | "done"))
            .map(|s| format!("{} {}", words(s), dur(n(&t[*s]))))
            .collect();
        line(format!(
            "- Time in each state (from `pane.state`, sums to the wall time): {}",
            shown.join("; ")
        ));
        line(format!(
            "- State changes: {} (the most within one minute: {}); times it began to work: {}",
            n(&a["state_changes"]),
            n(&a["most_state_changes_in_a_minute"]),
            n(&a["work_periods"])
        ));
        line(format!(
            "- Tool calls logged: {} ({} failed)",
            n(&a["tool_calls"]),
            n(&a["tool_calls_failed"])
        ));
        let u = &a["usage"];
        line(format!(
            "- Turns, as its session data counts them: {}",
            a["turns"]
                .as_i64()
                .map(|t| t.to_string())
                .unwrap_or_else(|| "not reported".into())
        ));
        if u["reported"] == json!(true) {
            let unpriced: Vec<String> = u["unpriced_models"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m.as_str())
                .map(code)
                .collect();
            line(format!(
                "- Tokens ({}): {} in all; input {}, output {}, cache read {}, cache write {}",
                str_(&u["covers"]),
                n(&u["total_tokens"]),
                n(&u["input_tokens"]),
                n(&u["output_tokens"]),
                n(&u["cache_read_tokens"]),
                n(&u["cache_write_tokens"])
            ));
            line(format!(
                "- Cost: {}{}",
                match u["cost_estimate_usd"].as_f64() {
                    Some(c) => format!("${c:.4}, an estimate at API prices (a subscription is not billed per token)"),
                    None => "no estimate: unpriced".into(),
                },
                if unpriced.is_empty() { String::new() } else { format!("; no price is listed for {}, which is not in the estimate", unpriced.join(", ")) }
            ));
        } else {
            line("- Tokens and cost: not reported (this pane's session data gave the station no token counts)".into());
        }
        let restarted: Vec<String> = a["restarted"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| {
                format!(
                    "{} by {}",
                    at(n(&r["ts_ms"])),
                    code(r["by"].as_str().unwrap_or("?"))
                )
            })
            .collect();
        line(format!(
            "- Restarts: {}{}{}",
            n(&a["restarts"]),
            if restarted.is_empty() {
                String::new()
            } else {
                format!(" ({})", restarted.join(", "))
            },
            match n(&a["resumed_after_station_restart"]) {
                0 => String::new(),
                k => format!("; started again by the station after its own restart: {k}"),
            }
        ));
        let ap = &a["approvals"];
        line(format!(
            "- Approvals asked of the owner: {}; answered: {}; total wait for an answer: {}; longest: {}",
            n(&ap["asked"]), n(&ap["answered"]), dur(n(&ap["wait_total_ms"])), opt_dur(&ap["wait_longest_ms"])
        ));
        line(String::new());
    }
    let approvals = v["approvals"].as_array().cloned().unwrap_or_default();
    if !approvals.is_empty() {
        line("### Each approval".into());
        line(String::new());
        for a in &approvals {
            line(format!(
                "- {} pane {} asked ({}): {}; {}",
                at(n(&a["asked_ms"])),
                code(&str_(&a["name"])),
                code(&str_(&a["kind"])),
                code(&str_(&a["summary"])),
                match a["answered_ms"].as_u64() {
                    Some(t) => format!(
                        "{} at {} by {}, after {}",
                        code(&str_(&a["status"])),
                        at(t),
                        code(a["by"].as_str().unwrap_or("?")),
                        opt_dur(&a["wait_ms"])
                    ),
                    None => format!(
                        "not answered ({}); waiting {} at the end of the period",
                        code(&str_(&a["status"])),
                        opt_dur(&a["waiting_at_end_ms"])
                    ),
                }
            ));
        }
        line(String::new());
    }

    // ---------------------------------------------------------- 3
    line("## 3. Messages".into());
    line(String::new());
    let m = &v["messages"];
    line(format!("{} message(s).", n(&m["count"])));
    line(String::new());
    for p in m["pairs"].as_array().into_iter().flatten() {
        line(format!(
            "- {} to {}: {}",
            code(&str_(&p["from"])),
            code(&str_(&p["to"])),
            n(&p["count"])
        ));
    }
    line(String::new());
    line("Latency, by what the recipient was doing when the message was sent (delivery: sent to stored at its station; read: sent to taken by the recipient with its inbox):".into());
    line(String::new());
    for (key, title) in [
        ("recipient_working", "recipient working"),
        (
            "recipient_not_working",
            "recipient not working (idle, done, needs you, or not running)",
        ),
        (
            "recipient_not_a_pane_here",
            "recipient not a pane of this project here (the owner, another station)",
        ),
    ] {
        let g = &m["latency"][key];
        if n(&g["messages"]) == 0 {
            line(format!("- {title}: no messages"));
            continue;
        }
        line(format!(
            "- {title}: {} message(s); delivery median {}, worst {}; read {} of them, median {}, worst {}; not read: {}",
            n(&g["messages"]), opt_dur(&g["delivery"]["median_ms"]), opt_dur(&g["delivery"]["worst_ms"]),
            n(&g["read"]["n"]), opt_dur(&g["read"]["median_ms"]), opt_dur(&g["read"]["worst_ms"]), n(&g["not_read"])
        ));
    }
    line(String::new());
    line("### The thread, in time order".into());
    line(String::new());
    if n(&m["earlier_not_shown"]) > 0 {
        line(format!(
            "{} earlier messages not shown (`--all` shows them).",
            n(&m["earlier_not_shown"])
        ));
        line(String::new());
    }
    for t in m["thread"].as_array().into_iter().flatten() {
        line(format!(
            "{} · {} to {} · {} · {}{}",
            at(n(&t["ts_ms"])),
            code(&str_(&t["from"])),
            code(&str_(&t["to"])),
            code(&str_(&t["kind"])),
            match t["read_ms"].as_u64() {
                Some(r) => format!("read {} (after {})", at(r), opt_dur(&t["read_latency_ms"])),
                None => format!("not read ({})", code(&str_(&t["status"]))),
            },
            t["acted_ms"]
                .as_u64()
                .map(|a| format!(" · acted on {}", at(a)))
                .unwrap_or_default(),
        ));
        line(String::new());
        line(fenced(&str_(&t["body"])));
    }

    // ---------------------------------------------------------- 4
    line("## 4. Shared notes".into());
    line(String::new());
    let notes = &v["notes"];
    let listed = notes["notes"].as_array().cloned().unwrap_or_default();
    line(format!(
        "{} note(s) posted on purpose (decisions, findings, notes, statuses), oldest first. Activity spandeck recorded by itself (files edited, commands that failed, test runs): {} row(s), not listed here.",
        listed.len() as u64 + n(&notes["earlier_not_shown"]), notes["activity_count"].as_i64().unwrap_or(0)
    ));
    line(String::new());
    if n(&notes["earlier_not_shown"]) > 0 {
        line(format!(
            "{} earlier notes not shown (`--all` shows them).",
            n(&notes["earlier_not_shown"])
        ));
        line(String::new());
    }
    for note in &listed {
        line(format!(
            "{} · {} by {}",
            at(n(&note["ts_ms"])),
            code(&str_(&note["kind"])),
            code(note["author"].as_str().unwrap_or("?"))
        ));
        line(String::new());
        line(fenced(&str_(&note["text"])));
    }

    // ---------------------------------------------------------- 5
    line("## 5. The owner's part".into());
    line(String::new());
    let owner = v["owner"].as_array().cloned().unwrap_or_default();
    if owner.is_empty() {
        line("Nothing by the owner is recorded for these panes in the period.".into());
    }
    for e in &owner {
        line(format!(
            "- {} {}{} (by {})",
            at(n(&e["ts_ms"])),
            str_(&e["what"]),
            match e["name"].as_str() {
                Some(p) if !p.is_empty() => format!(": {}", code(p)),
                _ => String::new(),
            },
            code(e["by"].as_str().unwrap_or("?"))
        ));
    }
    line(String::new());

    // ---------------------------------------------------------- 6
    line("## 6. Timeline".into());
    line(String::new());
    line("One line for each minute, or for a run of minutes in which nothing changed and nothing was sent. The letter is the state each pane was in for most of the minute: W working, N needs you, I idle, D done, E error, X exited, C closed, - not started, ? not known.".into());
    line(String::new());
    let tl = &v["timeline"];
    if n(&tl["earlier_not_shown"]) > 0 {
        line(format!(
            "{} earlier lines not shown (`--all` shows them).",
            n(&tl["earlier_not_shown"])
        ));
        line(String::new());
    }
    let mut body = String::new();
    for l in tl["lines"].as_array().into_iter().flatten() {
        let from = n(&l["from_ms"]);
        let mins = n(&l["minutes"]);
        // To the minute: "12:03", or with its date when the period has several.
        let minute = |ms: u64| {
            let full = rfc3339(ms);
            if one_day {
                full[11..16].to_string()
            } else {
                full[..16].to_string()
            }
        };
        let when = if mins > 1 {
            format!(
                "{}-{} ({mins} min)",
                minute(from),
                minute(from + (mins - 1) * MINUTE)
            )
        } else {
            minute(from)
        };
        let states: Vec<String> = l["states"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, s)| format!("{name} {}", state_letter(s.as_str().unwrap_or(""))))
            .collect();
        let sent: Vec<String> = l["sent"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str())
            .map(String::from)
            .collect();
        body.push_str(&format!(
            "{when}  {}{}\n",
            states.join("  "),
            if sent.is_empty() {
                String::new()
            } else {
                format!("  | {}", sent.join("; "))
            }
        ));
    }
    if body.is_empty() {
        line("No timeline: see the last section.".into());
    } else {
        // One line each: a name with a line break in it cannot add a line.
        let flat: String = body
            .lines()
            .map(|l| l.replace('\r', " "))
            .collect::<Vec<_>>()
            .join("\n");
        line(fenced(&flat));
    }

    // ---------------------------------------------------------- 7
    line("## 7. What looks wrong".into());
    line(String::new());
    let limits = &v["limits"];
    line(format!(
        "Only what can be counted, each with what it was counted from. Limits: a pane whose state changed more than {} times within one minute; a message unread for more than {} minutes; an approval that waited more than {} minutes; a pane marked exited and running again; a nudge typed more than once for the same mail.",
        n(&limits["state_changes_in_a_minute"]), n(&limits["unread_minutes"]), n(&limits["approval_wait_minutes"])
    ));
    line(String::new());
    let flags = v["looks_wrong"].as_array().cloned().unwrap_or_default();
    if flags.is_empty() {
        line("Nothing was over a limit.".into());
    }
    for f in &flags {
        let who = match (f["name"].as_str(), f["from"].as_str()) {
            (Some(p), _) => format!("pane {}", code(p)),
            (None, Some(from)) => format!("{} to {}", code(from), code(&str_(&f["to"]))),
            _ => String::new(),
        };
        let what = match f["what"].as_str().unwrap_or("") {
            "state_changes" => "state changed often",
            "message_unread" => "message unread",
            "approval_waited" => "approval waited",
            "exited_and_running_again" => "marked exited and running again",
            "nudge_typed_again" => "nudge typed again",
            other => other,
        };
        // The evidence names log kinds in backticks of its own and quotes
        // nothing an agent wrote.
        line(format!("- {what}, {who}: {}", str_(&f["evidence"])));
    }
    line(String::new());

    line("## What this record cannot say".into());
    line(String::new());
    for miss in v["missing"].as_array().into_iter().flatten() {
        line(format!("- {}", str_(miss)));
    }
    o
}

impl Daemon {
    /// `project.record`: gather, count, write. Reads only.
    pub fn project_record(&self, params: &Value) -> Result<Value> {
        let project = params
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| invalid("missing parameter: project"))?;
        let since = params.get("since_ms").and_then(|v| v.as_u64());
        let until = params.get("until_ms").and_then(|v| v.as_u64());
        if since.zip(until).is_some_and(|(s, u)| s > u) {
            return Err(invalid("since_ms is after until_ms"));
        }
        let all = params.get("all").and_then(|v| v.as_bool()) == Some(true);
        let inp = self.record_input(project, since, until, all)?;
        let mut v = build(&inp);
        v["markdown"] = json!(markdown(&v));
        Ok(v)
    }

    fn record_input(
        &self,
        project: &str,
        since: Option<u64>,
        until: Option<u64>,
        all: bool,
    ) -> Result<Input> {
        let now = now_ms();
        let far = i64::MAX as u64;
        let (from, to) = (since.unwrap_or(0).min(far), until.unwrap_or(now).min(far));
        // ------------------------------------------------------ panes
        let mut metas: BTreeMap<String, (Value, bool)> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, meta FROM panes WHERE json_valid(meta) AND json_extract(meta,'$.project')=?1",
                )?;
                let rows = stmt.query_map([project], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(id, meta)| Some((id, (serde_json::from_str(&meta).ok()?, false))))
            .collect();
        for p in self.list_panes() {
            let m = p.meta();
            if m.project.as_deref() == Some(project) {
                let live = m.exit.is_none();
                metas.insert(m.id.clone(), (json!(m), live));
            }
        }
        if metas.is_empty() && self.project_path(project).is_none() {
            return Err(Error::NotFound(format!("project {project}")));
        }
        let usage = self.usage_summary(None);
        let mut panes: Vec<Pane> = metas
            .into_iter()
            .map(|(id, (meta, live))| {
                let row = usage["panes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|r| r["pane"].as_str() == Some(id.as_str()))
                    .cloned()
                    .unwrap_or(Value::Null);
                let model = serde_json::from_value::<PaneMeta>(meta.clone())
                    .ok()
                    .and_then(|m| self.pane_usage(&m)["model"].as_str().map(String::from))
                    .or_else(|| {
                        row["all"]["by_model"][0]["model"]
                            .as_str()
                            .map(String::from)
                    });
                let text = |v: &Value| v.as_str().map(String::from);
                Pane {
                    name: text(&meta["name"]).unwrap_or_else(|| id.clone()),
                    role: text(&meta["role"]),
                    agent: text(&meta["agent"]).unwrap_or_default(),
                    // The program's name, without the folder it is in.
                    program: text(&meta["spec"]["program"])
                        .and_then(|p| p.rsplit(['/', '\\']).next().map(String::from))
                        .filter(|p| !p.is_empty())
                        .or_else(|| text(&meta["agent"]))
                        .unwrap_or_default(),
                    model,
                    live,
                    state_now: live.then(|| text(&meta["state"])).flatten(),
                    usage: row,
                    id,
                }
            })
            .collect();
        panes.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        let ids: HashSet<&str> = panes.iter().map(|p| p.id.as_str()).collect();
        let ids_json = json!(panes.iter().map(|p| &p.id).collect::<Vec<_>>()).to_string();

        // -------------------------------------------------------- log
        let mut log: Vec<LogRow> = vec![];
        let mut tools: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut read = 0usize;
        let mut log_truncated = false;
        let mut keep = |row: LogRow, log: &mut Vec<LogRow>| {
            if row.kind == "agent.tool" {
                // Counted, not kept: there is one for every tool call.
                if let Some(p) = &row.pane {
                    let t = tools.entry(p.clone()).or_default();
                    t.0 += 1;
                    t.1 += (row.data["ok"] == json!(false)) as u64;
                }
            } else {
                log.push(row);
            }
        };
        let mut after = (from, 0i64);
        loop {
            let page = self.db.log_between(project, &ids_json, after, to, LOG_PAGE);
            let n = page.len();
            for (id, row) in page {
                after = (row.ts, id);
                seen.insert(row.ulid.clone());
                keep(row, &mut log);
            }
            read += n;
            if n < LOG_PAGE as usize {
                break;
            }
            if read >= LOG_CAP {
                log_truncated = true;
                break;
            }
        }
        // What still waits to be written is part of the log.
        if !log_truncated {
            for row in self.db.log_waiting() {
                let ours = row.project.as_deref() == Some(project)
                    || row.pane.as_deref().is_some_and(|p| ids.contains(p));
                if ours && row.ts >= from && row.ts <= to && seen.insert(row.ulid.clone()) {
                    keep(row, &mut log);
                }
            }
            log.sort_by_key(|r| r.ts);
        }
        let station_starts: Vec<u64> = {
            let mut starts: Vec<u64> = self
                .db
                .log_query(None, Some("station.started"), from, 200)
                .iter()
                .filter_map(|r| r["ts"].as_u64())
                .filter(|t| *t <= to)
                .collect();
            starts.sort_unstable();
            // The station's own first start is no restart in the period.
            match log.first() {
                Some(first) => starts.retain(|t| *t > first.ts),
                None => starts.clear(),
            }
            starts
        };

        // --------------------------------------------------- messages
        let (own, others) = self.open_names(project);
        let whose = Panes {
            project,
            station: &self.station,
            ids: &ids,
            own: &own,
            others: &others,
        };
        let mut messages_shared_name = 0u64;
        let mut messages: Vec<Msg> = vec![];
        let (mut last_row, mut scanned, mut messages_truncated) = (0i64, 0usize, false);
        loop {
            let page: Vec<(i64, Msg)> = self
                .db
                .read(|c| {
                    let mut stmt = c.prepare(
                        "SELECT rowid,id,ts,sender,recipient,kind,body,status,project,received,read,acted
                         FROM messages WHERE rowid>?1 AND ts>=?2 AND ts<=?3 ORDER BY rowid LIMIT 2000",
                    )?;
                    let ms = |v: Option<i64>| v.map(|v| v.max(0) as u64);
                    let rows = stmt.query_map(rusqlite::params![last_row, from as i64, to as i64], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            Msg {
                                id: r.get(1)?,
                                ts: r.get::<_, i64>(2)?.max(0) as u64,
                                from: r.get(3)?,
                                to: r.get(4)?,
                                kind: r.get(5)?,
                                body: r.get(6)?,
                                status: r.get(7)?,
                                project: r.get(8)?,
                                received: ms(r.get(9)?),
                                read: ms(r.get(10)?),
                                acted: ms(r.get(11)?),
                            },
                        ))
                    })?;
                    rows.collect()
                })
                .unwrap_or_default();
            let n = page.len();
            for (row, m) in page {
                last_row = row;
                match whose.whose(&m) {
                    Whose::Ours => messages.push(m),
                    Whose::SharedName => messages_shared_name += 1,
                    Whose::Not => {}
                }
            }
            scanned += n;
            if n < 2000 {
                break;
            }
            if scanned >= MSG_SCAN_CAP {
                messages_truncated = true;
                break;
            }
        }
        messages.sort_by_key(|m| m.ts);

        // ------------------------------------------------------ notes
        let notes: Vec<Note> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT ts,kind,author,text FROM notes WHERE project=?1 AND kind!='auto' AND ts>=?2 AND ts<=?3
                     ORDER BY id",
                )?;
                let rows = stmt.query_map(rusqlite::params![project, from as i64, to as i64], |r| {
                    Ok(Note {
                        ts: r.get::<_, i64>(0)?.max(0) as u64,
                        kind: r.get(1)?,
                        author: r.get(2)?,
                        text: r.get(3)?,
                    })
                })?;
                rows.collect()
            })
            .unwrap_or_default();
        let approvals: BTreeMap<String, Stored> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT id,status,answered,answered_by FROM approvals
                     WHERE (project=?1 OR pane IN (SELECT value FROM json_each(?2))) AND ts>=?3 AND ts<=?4",
                )?;
                let rows = stmt.query_map(rusqlite::params![project, ids_json, from as i64, to as i64], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        Stored {
                            status: r.get(1)?,
                            answered: r.get::<_, Option<i64>>(2)?.map(|v| v.max(0) as u64),
                            by: r.get(3)?,
                        },
                    ))
                })?;
                rows.collect()
            })
            .unwrap_or_default();
        Ok(Input {
            project: project.to_string(),
            station: self.station.clone(),
            generated_ms: now,
            since_ms: since,
            until_ms: until,
            all,
            activity_count: self.activity_count(project, from, to),
            panes,
            log,
            log_truncated,
            tools,
            station_starts,
            messages,
            messages_truncated,
            messages_shared_name,
            notes,
            approvals,
        })
    }

    /// When each pane the station knows of was open under its name: the
    /// project's own, and all the others.
    fn open_names(&self, project: &str) -> (Vec<Open>, Vec<Open>) {
        type Row = (String, Option<String>, Option<String>, i64, Option<i64>);
        let mut rows: BTreeMap<String, (Option<String>, Open)> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, json_extract(meta,'$.name'), json_extract(meta,'$.project'), created, closed
                     FROM panes WHERE json_valid(meta)",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<Row>>>()
            })
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(id, name, project, created, closed)| {
                let open = Open {
                    name: name?,
                    from: created.max(0) as u64,
                    to: closed.map(|c| c.max(0) as u64),
                };
                Some((id, (project, open)))
            })
            .collect();
        // A pane as it is now: its record may still wait to be written.
        for p in self.list_panes() {
            let m = p.meta();
            if m.exit.is_none() {
                let open = Open {
                    name: m.name.clone(),
                    from: m.created_ms,
                    to: None,
                };
                rows.insert(m.id.clone(), (m.project.clone(), open));
            }
        }
        let (mut own, mut others) = (vec![], vec![]);
        for (of, open) in rows.into_values() {
            if of.as_deref() == Some(project) {
                own.push(open);
            } else {
                others.push(open);
            }
        }
        (own, others)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The headings of a Markdown document, as a reader of Markdown finds
    /// them: lines that start with `#` outside every fenced block.
    fn headings(md: &str) -> Vec<String> {
        let mut open: Option<usize> = None;
        let mut out = vec![];
        for l in md.lines() {
            let ticks = l.trim_start().chars().take_while(|c| *c == '`').count();
            match open {
                None if ticks >= 3 => open = Some(ticks),
                Some(n) if ticks >= n && l.trim().len() == ticks => open = None,
                None if l.trim_start().starts_with('#') => out.push(l.to_string()),
                _ => {}
            }
        }
        assert!(open.is_none(), "a fence was left open");
        out
    }

    /// The reason for this test: text from agents is data (AGENTS.md, rule
    /// 6). A message, a note, a pane's name and an approval's summary that
    /// carry Markdown of their own add no heading and end no block.
    #[test]
    fn what_agents_wrote_cannot_restructure_the_document() {
        let evil = "fine\n# Heading\n```\n## Another\n````\n~~~\n# Third";
        let row = |ts: u64, kind: &str, data: Value| LogRow {
            ulid: format!("{ts}-{kind}"),
            ts,
            kind: kind.into(),
            pane: Some("p-1".into()),
            project: Some("demo".into()),
            task: None,
            actor: None,
            data,
        };
        let inp = Input {
            project: "demo".into(),
            station: "alpha".into(),
            generated_ms: 200_000,
            since_ms: None,
            until_ms: None,
            all: false,
            panes: vec![Pane {
                id: "p-1".into(),
                name: "bad`name\n# Name".into(),
                role: None,
                agent: "fake".into(),
                program: "fake".into(),
                model: Some("m``\n# Model".into()),
                live: true,
                state_now: None,
                usage: Value::Null,
            }],
            log: vec![
                row(100_000, "pane.created", json!({})),
                row(
                    100_500,
                    "approval.requested",
                    json!({"id": "a1", "kind": "tool", "summary": evil}),
                ),
                row(
                    101_000,
                    "pane.state",
                    json!({"state": "working", "from": "idle"}),
                ),
            ],
            log_truncated: false,
            tools: BTreeMap::new(),
            station_starts: vec![],
            messages: vec![Msg {
                id: "m1".into(),
                ts: 102_000,
                from: "you@alpha".into(),
                to: "bad`name\n# Name@alpha".into(),
                kind: "note".into(),
                body: evil.into(),
                status: "delivered".into(),
                project: None,
                received: Some(102_000),
                read: None,
                acted: None,
            }],
            messages_truncated: false,
            messages_shared_name: 0,
            notes: vec![Note {
                ts: 103_000,
                kind: "decision".into(),
                author: Some("# Author".into()),
                text: evil.into(),
            }],
            activity_count: 0,
            approvals: BTreeMap::new(),
        };
        let v = build(&inp);
        let md = markdown(&v);
        let found = headings(&md);
        let ours: Vec<&str> = found
            .iter()
            .map(|h| h.split('`').next().unwrap().trim())
            .collect();
        assert_eq!(
            ours,
            [
                "# Record of project",
                "## 1. Summary",
                "## 2. Per agent",
                "### Pane",
                "### Each approval",
                "## 3. Messages",
                "### The thread, in time order",
                "## 4. Shared notes",
                "## 5. The owner's part",
                "## 6. Timeline",
                "## 7. What looks wrong",
                "## What this record cannot say",
            ],
            "{md}"
        );
        // And what they wrote is all there, as it was written.
        assert_eq!(
            md.matches("# Heading\n```\n## Another\n````\n~~~\n# Third")
                .count(),
            2,
            "{md}"
        );
        // The numbers: one pane, a second not started, 3 s of wall time.
        assert_eq!(v["period"]["wall_ms"], json!(3000));
        let t = &v["agents"][0]["time_in_state_ms"];
        assert_eq!(
            (t["idle"].as_u64(), t["working"].as_u64()),
            (Some(1000), Some(2000)),
            "{t}"
        );
    }

    /// The limits of "what looks wrong" are counts: eleven changes of state
    /// within a minute are over the limit of ten, ten are not; a message
    /// read after six minutes and an approval never answered are listed
    /// with the entries they were counted from.
    #[test]
    fn what_looks_wrong_is_counted_against_its_limits() {
        let row = |ts: u64, pane: &str, kind: &str, actor: Option<&str>, data: Value| LogRow {
            ulid: format!("{ts}-{pane}-{kind}"),
            ts,
            kind: kind.into(),
            pane: Some(pane.into()),
            project: Some("demo".into()),
            task: None,
            actor: actor.map(String::from),
            data,
        };
        let pane = |id: &str, name: &str| Pane {
            id: id.into(),
            name: name.into(),
            role: None,
            agent: "fake".into(),
            program: "fake".into(),
            model: None,
            live: true,
            state_now: None,
            usage: Value::Null,
        };
        let t0 = 1_000_000_000;
        let mut log = vec![
            row(t0, "p-a", "pane.created", None, json!({})),
            row(t0, "p-b", "pane.created", None, json!({})),
        ];
        // p-a: eleven changes in 50 s. p-b: ten in the same time.
        for i in 0..=FLAP_N as u64 {
            let (state, from) = if i % 2 == 0 {
                ("working", "idle")
            } else {
                ("idle", "working")
            };
            log.push(row(
                t0 + 1000 + i * 5000,
                "p-a",
                "pane.state",
                None,
                json!({"state": state, "from": from}),
            ));
            if i < FLAP_N as u64 {
                log.push(row(
                    t0 + 1000 + i * 5000,
                    "p-b",
                    "pane.state",
                    None,
                    json!({"state": state, "from": from}),
                ));
            }
        }
        log.push(row(
            t0 + 60_000,
            "p-a",
            "approval.requested",
            None,
            json!({"id": "a1", "kind": "tool", "summary": "Write x"}),
        ));
        log.push(row(
            t0 + 70_000,
            "p-b",
            "pane.exited",
            None,
            json!({"exit": {"code": 1}}),
        ));
        log.push(row(t0 + 80_000, "p-b", "pane.created", None, json!({})));
        log.push(row(
            t0 + 80_100,
            "p-b",
            "pane.restarted",
            Some("you"),
            json!({}),
        ));
        log.push(row(
            t0 + 90_000,
            "p-a",
            "msg.nudged",
            None,
            json!({"count": 1, "typed": true, "tries": 1}),
        ));
        log.push(row(
            t0 + 95_000,
            "p-a",
            "msg.nudged",
            None,
            json!({"count": 1, "typed": true, "tries": 2}),
        ));
        log.push(row(
            t0 + 600_000,
            "p-a",
            "pane.state",
            None,
            json!({"state": "done", "from": "working"}),
        ));
        log.sort_by_key(|r| r.ts);
        let msg = |id: &str, ts: u64, read: Option<u64>| Msg {
            id: id.into(),
            ts,
            from: "demo/a@alpha".into(),
            to: "b@alpha".into(),
            kind: "note".into(),
            body: "x".into(),
            status: "delivered".into(),
            project: Some("demo".into()),
            received: Some(ts),
            read,
            acted: None,
        };
        let inp = Input {
            project: "demo".into(),
            station: "alpha".into(),
            generated_ms: t0 + 700_000,
            since_ms: None,
            until_ms: None,
            all: false,
            panes: vec![pane("p-a", "a"), pane("p-b", "b")],
            log,
            log_truncated: false,
            tools: BTreeMap::new(),
            station_starts: vec![],
            messages: vec![
                msg("late", t0 + 2000, Some(t0 + 2000 + 6 * MINUTE)),
                msg("soon", t0 + 3000, Some(t0 + 3000 + 4 * MINUTE)),
            ],
            messages_truncated: false,
            messages_shared_name: 0,
            notes: vec![],
            activity_count: 0,
            approvals: BTreeMap::new(),
        };
        let v = build(&inp);
        let flags: Vec<(&str, &str)> = v["looks_wrong"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["what"].as_str().unwrap(),
                    f["name"].as_str().or(f["id"].as_str()).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            flags,
            [
                ("state_changes", "a"),
                ("nudge_typed_again", "a"),
                ("exited_and_running_again", "b"),
                ("approval_waited", "a"),
                ("message_unread", "late"),
            ],
            "{}",
            v["looks_wrong"]
        );
        assert_eq!(v["looks_wrong"][0]["count"], json!(FLAP_N + 1));
        assert_eq!(v["looks_wrong"][2]["how"], json!("restarted by you"));
        // Each pane's time in its states is the wall time, to the millisecond.
        for a in v["agents"].as_array().unwrap() {
            let sum: u64 = a["time_in_state_ms"]
                .as_object()
                .unwrap()
                .values()
                .filter_map(|x| x.as_u64())
                .sum();
            assert_eq!(sum, 600_000, "{a}");
        }
        // The message was sent while `b` worked (its first change was at
        // 1 s, to working): its latency is on that side.
        let l = &v["messages"]["latency"];
        assert_eq!(l["recipient_working"]["messages"], json!(2), "{l}");
        assert_eq!(
            l["recipient_working"]["read"]["worst_ms"],
            json!(6 * MINUTE),
            "{l}"
        );
        assert_eq!(
            l["recipient_working"]["read"]["median_ms"],
            json!(5 * MINUTE),
            "{l}"
        );
    }

    /// The reason for this test: the record of `hero3` held eight messages
    /// of `pairtest`, an older project on the same station whose panes had
    /// the same names, and began at that project's time (19 Nov 2026). A
    /// message is a project's by its panes' ids and by `project/name`,
    /// never by a pane's name; a bare name counts only while no pane of
    /// another project had it.
    #[test]
    fn a_message_is_a_projects_by_its_panes_and_never_by_a_name_alone() {
        let ids: HashSet<&str> = ["p-new"].into();
        let open = |from: u64, to: Option<u64>| Open {
            name: "critic".into(),
            from,
            to,
        };
        // pairtest's critic was open from 100 to 200; hero3's from 300.
        let (own, others) = ([open(300, None)], [open(100, Some(200))]);
        let panes = Panes {
            project: "hero3",
            station: "spark2",
            ids: &ids,
            own: &own,
            others: &others,
        };
        let whose = |ts: u64, from: &str, to: &str| {
            panes.whose(&Msg {
                id: "m".into(),
                ts,
                from: from.into(),
                to: to.into(),
                kind: "note".into(),
                body: "b".into(),
                status: "read".into(),
                project: None,
                received: None,
                read: None,
                acted: None,
            })
        };
        // What was in the record and was not hero3's.
        assert_eq!(
            whose(150, "pairtest/builder@spark2", "critic@spark2"),
            Whose::Not
        );
        assert_eq!(
            whose(350, "pairtest/critic@spark2", "pairtest/builder@spark2"),
            Whose::Not
        );
        // hero3's own, by project and by id, from a pane and from the owner.
        assert_eq!(
            whose(350, "hero3/builder@spark2", "critic@spark2"),
            Whose::Ours
        );
        assert_eq!(whose(350, "you@spark2", "hero3/critic@spark2"), Whose::Ours);
        assert_eq!(
            whose(350, "pairtest/user@spark2", "p-new@spark2"),
            Whose::Ours
        );
        // A bare name only hero3's pane had then; the same name on another
        // station is that station's.
        assert_eq!(whose(350, "you@spark2", "critic@spark2"), Whose::Ours);
        assert_eq!(whose(350, "you@spark2", "critic@golf"), Whose::Not);
        // Both projects had a `critic` open: said, not guessed.
        let both = [open(100, None)];
        let panes = Panes {
            others: &both,
            ..panes
        };
        let m = Msg {
            id: "m".into(),
            ts: 350,
            from: "you@spark2".into(),
            to: "critic@spark2".into(),
            kind: "note".into(),
            body: "b".into(),
            status: "read".into(),
            project: None,
            received: None,
            read: None,
            acted: None,
        };
        assert_eq!(panes.whose(&m), Whose::SharedName);
    }
}
