//! The conversation view, on the station (DECISIONS.md, "The conversation
//! view"; the contract for whoever builds the view is
//! `docs/conversation-api.md`).
//!
//! A pane's agent session as ordered items, read from the raw lines the
//! archive keeps. This is the owner's own history, unscrubbed, and it is
//! served to the owner's signed-in browsers only: `pane.conversation`,
//! `pane.conversation.detail` and `pane.read` are on no agent's list, not on
//! the read-only one, and on no station's; another station reaches them only
//! by passing on the owner's request (`rpc.forward`), and keeps nothing.
//! Agents and other stations still see history only through `recall`, which
//! reads the scrubbed copy.
//!
//! Everything here reads files and the databases: call it off the async
//! runtime (`spawn_blocking`).

use serde_json::{json, Value};
use spandeck_archive::conversation::{
    summary_line, Dialect, Range, Waiting, PAGE_DEFAULT, PAGE_MAX,
};

use spandeck_archive::stores;

use crate::daemon::{invalid, Daemon, PaneEntry, PaneMeta, Result};
use crate::paths::now_ms;

/// Something said of a pane's model at a moment of its conversation: what
/// its program printed for a command (`name`), or the owner's choice for
/// the pane alone (no name: the flags are on the pane's record).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Said {
    #[serde(default)]
    pub name: String,
    /// When: the record's own time, else this station's clock when it was
    /// first read.
    pub at_ms: u64,
    /// The session it was in, and the last line of it then.
    pub session: String,
    pub line: u64,
}

/// Which word on a pane's model holds now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Now {
    /// The model its last reply names (or nothing is known).
    Reply,
    /// What its program printed when a command set the model.
    Said,
    /// What the owner chose for this pane, which it was started on.
    Chosen,
}

/// The rule (docs/conversation-api.md, `live.model`): **the last word in
/// the conversation's own order holds.** A reply that names its model,
/// written after the program printed "Set model to ...", or after the
/// pane was started again on a model, is the truth; one written before
/// is not. In the same session the order is the lines'. Across sessions
/// (the program began another file when it was started again) it is the
/// records' own times, and a reply with no time is taken to be the newer:
/// nothing is claimed that a reply may have overtaken. Of the two that
/// are not replies, the later holds.
pub(crate) fn model_now(
    session: &str,
    reply: Option<(u64, u64)>,
    said: Option<&Said>,
    chosen: Option<&Said>,
) -> Now {
    let stands = |w: &Said| match reply {
        None => true,
        Some((line, _)) if w.session == session => line <= w.line,
        Some((_, ts)) => ts != 0 && ts <= w.at_ms,
    };
    match (said.filter(|w| stands(w)), chosen.filter(|w| stands(w))) {
        (Some(s), Some(c)) if c.at_ms >= s.at_ms => Now::Chosen,
        (Some(_), _) => Now::Said,
        (None, Some(_)) => Now::Chosen,
        (None, None) => Now::Reply,
    }
}

fn read_key(pane: &str) -> String {
    format!("conversation.read.{pane}")
}

fn cleared_key(pane: &str) -> String {
    format!("conversation.cleared.{pane}")
}

/// Notes the owner cleared from a conversation, kept for each pane: 200.
const CLEARED_MAX: usize = 200;
/// Tasks served with every answer: all that run (to this many), and the
/// newest finished ones.
const TASKS_RUNNING_MAX: usize = 50;
const TASKS_FINISHED_MAX: usize = 30;
/// How many sub-agents' rows are given a "now" (or what one said last):
/// the end of a record is read for each.
const STEPS_MAX: usize = 24;
/// A sub-agent's transcript is read from its end: this much of the file.
const SIDE_BYTES: u64 = 2 << 20;

/// An item's id as the station makes them: `<line>.<block>`.
fn item_id_ok(id: &str) -> bool {
    id.len() <= 24
        && id.split_once('.').is_some_and(|(a, b)| {
            !a.is_empty()
                && !b.is_empty()
                && a.bytes().all(|c| c.is_ascii_digit())
                && b.bytes().all(|c| c.is_ascii_digit())
        })
}

impl Daemon {
    /// The session a pane's conversation is read from, or why it has none.
    /// With how it is known: `hooks` (the agent's hooks named it),
    /// `resume` (the pane was started into it), or `files` (found among
    /// the program's own session files: discover.rs).
    fn conversation_session(
        &self,
        meta: &PaneMeta,
    ) -> std::result::Result<(String, &'static str), String> {
        if !meta.agent.is_agent() {
            return Err("it runs a shell, not an agent".into());
        }
        if Dialect::of(meta.agent.as_str()).is_none() {
            return Err(format!(
                "{} keeps its conversation where spandeck cannot read it yet",
                meta.agent.display()
            ));
        }
        // (OpenCode has no hooks: it never says.)
        let unsaid = if meta.agent == spandeck_agents::AgentKind::OpenCode {
            "OpenCode does not say which conversation it is in (it has no hooks)".to_string()
        } else {
            format!(
                "{} has not said which session it is in yet (nothing has come from its hooks)",
                meta.agent.display()
            )
        };
        // A session a hook named always wins. With none, the program's
        // own files are looked through, where that is known how.
        let opencode = meta.agent == spandeck_agents::AgentKind::OpenCode;
        let (session, file, from) = match meta.session_id.clone() {
            // OpenCode has no hook to name its session: the pane was
            // started under an id spandeck chose, and OpenCode begins
            // another with `/new`. So its database is looked through as
            // for a pane that named none, and the id it was started under
            // counts when nothing surer is found.
            Some(started) if opencode => match self.find_session(meta) {
                Some(Ok((session, file))) => (session, Some(file), "files"),
                other => {
                    let known = self.archive.session(&started).is_some()
                        || self.session_file(meta, &started).is_some();
                    match other {
                        _ if known => (started, None, "resume"),
                        Some(Err(why)) => return Err(format!("{unsaid}, and {why}")),
                        _ => return Err(unsaid),
                    }
                }
            },
            Some(session) => {
                let from = if meta.hooks_seen { "hooks" } else { "resume" };
                (session, None, from)
            }
            None => match self.find_session(meta) {
                Some(Ok((session, file))) => (session, Some(file), "files"),
                Some(Err(why)) => return Err(format!("{unsaid}, and {why}")),
                None => return Err(unsaid),
            },
        };
        if self.archive.session(&session).is_none() {
            // A pane from before a restart whose session the archive was
            // never told of: follow it now, if it says where its file is,
            // or its file is found by the session's id.
            let path = file
                .or_else(|| meta.transcript_path.clone())
                .or_else(|| self.session_file(meta, &session));
            let Some(path) = path else {
                return Err(match meta.agent {
                    spandeck_agents::AgentKind::Antigravity => "the transcript of the conversation Antigravity named is not there yet (Antigravity begins one with its first message)".to_string(),
                    _ => format!(
                        "{} has not said where its session file is",
                        meta.agent.display()
                    ),
                });
            };
            self.archive.follow(
                &session,
                meta.agent.as_str(),
                &path,
                meta.project.as_deref(),
                Some(&meta.id),
            );
        }
        Ok((session, from))
    }

    /// The line of a pane's session the owner has read to. A mark made in
    /// another session of the pane counts for nothing.
    fn read_line(&self, pane: &str, session: &str) -> u64 {
        let mut marks = self.read_marks.lock().unwrap();
        let mark = marks.entry(pane.to_string()).or_insert_with(|| {
            self.db
                .kv_get(&read_key(pane))
                .and_then(|v| serde_json::from_str::<Value>(&v).ok())
                .map(|v| {
                    (
                        v["session"].as_str().unwrap_or("").to_string(),
                        v["line"].as_u64().unwrap_or(0),
                    )
                })
                .unwrap_or_default()
        });
        if mark.0 == session {
            mark.1
        } else {
            0
        }
    }

    /// Has the pane's agent said something the owner has not read? Kept at
    /// the pane's station, so it is the same from every station's deck.
    pub(crate) fn conversation_unread(&self, meta: &PaneMeta) -> bool {
        // A session found among the program's files counts once someone
        // has looked at the pane's conversation: nothing is read here.
        // (For OpenCode the one found comes first: see `conversation_session`.)
        let found = self.found_session(&meta.id);
        let session = match meta.agent {
            spandeck_agents::AgentKind::OpenCode => found.or_else(|| meta.session_id.clone()),
            _ => meta.session_id.clone().or(found),
        };
        let Some(session) = session else {
            return false;
        };
        if Dialect::of(meta.agent.as_str()).is_none() {
            return false;
        }
        self.archive.last_reply_line(&session) > self.read_line(&meta.id, &session)
    }

    /// `pane.read`: the owner has read this pane's conversation up to a
    /// line (all of it, when no line is given). The mark only moves forward.
    pub fn conversation_read(&self, pane: &str, upto: Option<u64>) -> Result<Value> {
        let p = self.pane(pane)?;
        let meta = p.meta();
        let (session, _) = self
            .conversation_session(&meta)
            .map_err(|why| invalid(format!("no conversation for this pane: {why}")))?;
        self.archive.poll_session(&session);
        let end = self.archive.session(&session).map_or(0, |s| s.lines);
        let line = upto
            .unwrap_or(end)
            .min(end)
            .max(self.read_line(&p.id, &session));
        self.db.kv_put(
            &read_key(&p.id),
            &json!({"session": session, "line": line, "ts": now_ms()}).to_string(),
        );
        self.read_marks
            .lock()
            .unwrap()
            .insert(p.id.clone(), (session.clone(), line));
        let unread = self.conversation_unread(&meta);
        self.emit(
            "pane.read",
            Some(&p.id),
            meta.project.as_deref(),
            json!({"session": session, "read_line": line, "unread": unread}),
        );
        Ok(json!({"pane": p.id, "session": session, "read_line": line, "unread": unread}))
    }

    /// `pane.titles`: the title of each pane's session, for the panes whose
    /// session file has one (Claude Code writes one). A title is made from
    /// what was said, so this is the owner's, like the conversation itself:
    /// it is not put in `pane.list`, which agents read.
    pub fn conversation_titles(&self) -> Value {
        let mut titles = serde_json::Map::new();
        for p in self.list_panes() {
            let meta = p.meta();
            // The file's new lines are copied first, as for a conversation.
            let title = meta.session_id.as_deref().and_then(|s| {
                self.archive.poll_session(s);
                self.archive.session_title(s)
            });
            if let Some(title) = title {
                titles.insert(meta.id.clone(), json!(title));
            }
        }
        json!({ "titles": titles })
    }

    /// A closed pane's read mark is of no more use.
    pub(crate) fn conversation_forget(&self, pane: &str) {
        self.read_marks.lock().unwrap().remove(pane);
        self.forget_found(pane);
        self.db.put(crate::db::Op::KvDel { k: read_key(pane) });
        self.db.put(crate::db::Op::KvDel {
            k: cleared_key(pane),
        });
        self.attach_forget(pane);
        self.made_forget(pane);
    }

    /// The notes of a pane's conversation the owner cleared: ids of items
    /// of this session. Cleared in another session of the pane counts for
    /// nothing. Nothing is removed from the session file or the archive:
    /// the view leaves these out, on every device, until they are
    /// brought back.
    fn notes_cleared(&self, pane: &str, session: &str) -> Vec<String> {
        self.db
            .kv_get(&cleared_key(pane))
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .filter(|v| v["session"] == json!(session))
            .and_then(|v| {
                v["ids"].as_array().map(|ids| {
                    ids.iter()
                        .filter_map(|i| i.as_str().map(String::from))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// `pane.notes.clear {pane, session, item}` hides one note of the
    /// pane's conversation for the owner, everywhere; `{pane, session,
    /// restore: true}` brings every cleared note back. The owner's only.
    pub fn conversation_clear(&self, pane: &str, params: &Value) -> Result<Value> {
        let p = self.pane(pane)?;
        let session = params
            .get("session")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty() && s.len() <= 200)
            .ok_or_else(|| invalid("missing parameter: session"))?;
        let mut ids = self.notes_cleared(&p.id, session);
        if params.get("restore").and_then(|r| r.as_bool()) == Some(true) {
            ids.clear();
        } else {
            let item = params
                .get("item")
                .and_then(|i| i.as_str())
                .filter(|i| item_id_ok(i))
                .ok_or_else(|| invalid("item is an item's id, as the conversation gave it"))?;
            if !ids.iter().any(|i| i == item) {
                ids.push(item.to_string());
            }
            if ids.len() > CLEARED_MAX {
                let gone = ids.len() - CLEARED_MAX;
                ids.drain(..gone);
            }
        }
        self.db.kv_put(
            &cleared_key(&p.id),
            &json!({"session": session, "ids": ids}).to_string(),
        );
        Ok(json!({"pane": p.id, "session": session, "cleared": ids}))
    }

    /// What the pane's agent has running beside its turn, and what of it
    /// is over: its own sub-agents and the commands it left running (from
    /// its session file, where the program writes them: Claude Code), and
    /// the panes it started through spandeck. `running` is how many run.
    fn conversation_tasks(&self, p: &PaneEntry, meta: &PaneMeta, session: Option<&str>) -> Value {
        let mut running = vec![];
        let mut finished = vec![];
        let jobs = match session {
            Some(s) => self.archive.conversation_tasks(s),
            None => vec![],
        };
        let stem = session
            .and_then(|s| self.archive.session(s))
            .map(|s| s.path)
            .and_then(|path| path.strip_suffix(".jsonl").map(String::from));
        // Where the turn now (or the last one) began: a sub-agent of it
        // is `current`, and so is one still at work. A view's chip counts
        // those and no others.
        let (turn_line, turn_ms) = session.map_or((0, None), |s| self.archive.conversation_turn(s));
        let alive = meta.exit.is_none();
        // What Claude Code wrote beside the session for each sub-agent.
        let sides = stem
            .as_deref()
            .map(crate::subagents::claude_sides)
            .unwrap_or_default();
        // Only the first rows are given a "now": the end of a record is
        // read for each.
        let steps = std::cell::Cell::new(STEPS_MAX);
        let step = |row: &mut Value, file: Option<&std::path::Path>, dialect: Dialect| {
            let Some(file) = file.filter(|_| steps.get() > 0) else {
                return;
            };
            steps.set(steps.get() - 1);
            let (now, said) = crate::subagents::step_of(file, dialect);
            if row["status"] == json!("running") {
                row["now"] = json!(now);
            } else {
                row["said"] = json!(said);
            }
        };
        // The program's own id of each sub-agent listed, and its row.
        let mut known: std::collections::HashMap<String, (String, bool, bool)> = Default::default();
        let mut unlinked = 0;
        for job in jobs {
            let mut row = serde_json::to_value(&job).unwrap_or(Value::Null);
            // A task the file still has as running cannot be: its program
            // has ended, or was started again since it began.
            let before = match (job.started_ms, meta.proc_started_ms) {
                (Some(began), Some(program)) => began < program,
                _ => false,
            };
            if job.status == "running" && (!alive || before) {
                row["status"] = json!("lost");
            }
            let run = row["status"] == json!("running");
            if job.kind == "agent" {
                let id = claude_agent_id(&job, &sides);
                let file = side_file(stem.as_deref(), id.as_deref()).filter(|f| f.exists());
                let current = run || job.line >= turn_line;
                row["transcript"] = json!(file.is_some());
                row["state"] = json!(crate::subagents::state_of(
                    row["status"].as_str().unwrap_or("")
                ));
                row["depth"] = json!(1);
                row["current"] = json!(current);
                // (The rows of the turn now come first to what is read
                // for a "now": an older one says how it ended and no more.)
                if current {
                    step(&mut row, file.as_deref(), Dialect::Claude);
                }
                // A record with no time of its own: when the hooks told
                // of this one's start, and of its stop.
                if let Some(id) = &id {
                    let began = meta
                        .subagents
                        .iter()
                        .find(|s| s.id.as_ref() == Some(id))
                        .map(|s| s.since_ms);
                    let ended = meta.subagents_over.iter().find(|o| o.id == *id);
                    if row["started_ms"].is_null() {
                        row["started_ms"] =
                            json!(began.or(ended.map(|o| o.since_ms).filter(|s| *s > 0)));
                    }
                    if !run && row["ended_ms"].is_null() {
                        row["ended_ms"] = json!(ended.map(|o| o.ended_ms));
                    }
                }
                match id {
                    Some(id) => {
                        known.insert(id, (job.id.clone(), run, current));
                    }
                    None if run => unlinked += 1,
                    None => {}
                }
            } else {
                row["transcript"] = json!(false);
            }
            if run {
                running.push(row);
            } else {
                finished.push(row);
            }
        }
        // The sub-agents Claude Code's sub-agents began: no call in the
        // session's file names them, only the file each has beside it. A
        // parent is listed before its own (the less deep first), and one
        // whose parent is not listed is left out.
        let mut nested: Vec<&crate::subagents::Side> =
            sides.iter().filter(|s| !s.kind.is_empty()).collect();
        nested.sort_by_key(|s| s.depth);
        for side in nested {
            let Some(parent) = side.parent.as_ref().and_then(|p| known.get(p)).cloned() else {
                continue;
            };
            if known.contains_key(&side.id) {
                continue;
            }
            let file = side_file(stem.as_deref(), Some(&side.id)).filter(|f| f.exists());
            // The program writes `finished` when one is over. One that
            // has none and whose parent is over was let go with it.
            let run = !side.finished && parent.1 && alive;
            let id = format!("sub:{}", side.id);
            let status = if run {
                "running"
            } else if side.finished {
                "done"
            } else {
                "interrupted"
            };
            let mut row = json!({
                "id": id, "kind": "agent", "name": side.kind, "title": side.title,
                "status": status, "state": crate::subagents::state_of(status),
                "background": false,
                "started_ms": file.as_deref().and_then(crate::subagents::first_ms),
                "ended_ms": if run { Value::Null } else { json!(side.modified_ms) },
                "transcript": file.is_some(), "parent": parent.0, "depth": side.depth.max(2),
                "current": parent.2,
            });
            if parent.2 {
                step(&mut row, file.as_deref(), Dialect::Claude);
            }
            known.insert(side.id.clone(), (id, run, parent.2));
            if run {
                running.push(row);
            } else {
                finished.push(row);
            }
        }
        // A program that keeps a conversation of its own for each
        // sub-agent (Codex, OpenCode, Antigravity): what its store says
        // of them.
        for child in session.map_or(vec![], |s| self.store_children(meta, s)) {
            let dialect = meta.agent.as_str();
            // Where the store does not say whether one is still at work
            // (an Antigravity from before it did; a Codex file whose end
            // has no mark of its turn), it is taken to be while its
            // record was written to in the last fifteen seconds, and the
            // row says which it is.
            let now = now_ms();
            let (run, sure) = match child.running {
                Some(r) => (r, true),
                None => (now.saturating_sub(child.modified_ms) < 15_000, false),
            };
            let run = run && alive;
            let status = if run {
                "running"
            } else {
                child.ended.unwrap_or("done")
            };
            let current = run || turn_ms.is_none_or(|t| child.started_ms >= t);
            let id = format!("child:{}", child.id);
            let mut row = json!({
                "id": id, "kind": "agent",
                "title": summary_line(&child.title),
                "status": status, "state": crate::subagents::state_of(status),
                "background": false, "started_ms": child.started_ms,
                "ended_ms": if run { Value::Null } else { json!(child.modified_ms) },
                "transcript": true, "from": dialect, "sure": sure,
                "depth": child.depth.max(1), "current": current,
            });
            if !child.name.is_empty() {
                row["name"] = json!(child.name);
            }
            if let Some(parent) = &child.parent {
                row["parent"] = json!(format!("child:{parent}"));
            }
            match (&child.path, meta.agent) {
                _ if !current => {}
                (Some(path), agent) => {
                    if let Some(dialect) = Dialect::of(agent.as_str()) {
                        step(&mut row, Some(path), dialect);
                    }
                }
                (None, spandeck_agents::AgentKind::OpenCode) if run && steps.get() > 0 => {
                    steps.set(steps.get() - 1);
                    let lines = stores::opencode_tail(
                        &spandeck_archive::usage::opencode_db(&self.home),
                        &child.id,
                        6,
                    );
                    row["now"] = json!(
                        spandeck_archive::conversation::latest_step(
                            Dialect::OpenCode,
                            lines.iter().map(|l| l.as_str())
                        )
                        .0
                    );
                }
                _ => {}
            }
            if let Some(said) = child.said.as_ref().filter(|_| !run) {
                row["said"] = json!(said);
            }
            known.insert(child.id.clone(), (id, run, current));
            if run {
                running.push(row);
            } else {
                finished.push(row);
            }
        }
        // What the hooks say and nothing above has: a sub-agent that
        // runs (a program whose records were not found, or not yet), and
        // one the hooks said was over. A running row above with no id of
        // its own yet is taken to be one of those the hooks report.
        let hooked = meta
            .subagents
            .iter()
            .enumerate()
            .filter(|(_, s)| s.id.as_ref().is_none_or(|id| !known.contains_key(id)))
            .skip(unlinked);
        for (n, s) in hooked.filter(|_| alive) {
            let id = s.id.clone().unwrap_or_else(|| n.to_string());
            running.push(json!({
                "id": format!("hook:{id}"), "kind": "agent", "name": s.kind, "title": "",
                "status": "running", "state": "working", "background": false,
                "started_ms": s.since_ms, "transcript": false, "from": "hooks",
                "depth": 1, "current": true,
            }));
        }
        for o in meta
            .subagents_over
            .iter()
            .filter(|o| o.session.as_deref() == session && session.is_some())
            .filter(|o| !known.contains_key(&o.id))
        {
            let file = self.over_file(meta, stem.as_deref(), o);
            let current = turn_ms.is_none_or(|t| o.ended_ms >= t);
            let mut row = json!({
                "id": format!("hook:{}", o.id), "kind": "agent", "name": o.kind, "title": "",
                "status": "done", "state": "done", "background": false,
                "started_ms": if o.since_ms > 0 { json!(o.since_ms) } else { Value::Null },
                "ended_ms": o.ended_ms, "transcript": file.is_some(), "from": "hooks",
                "depth": 1, "current": current,
            });
            if let Some(dialect) = Dialect::of(meta.agent.as_str()).filter(|_| current) {
                step(&mut row, file.as_deref(), dialect);
            }
            finished.push(row);
        }
        // Panes this one started through spandeck (`delegate`): helpers,
        // each a pane of its own, which is where it is read and stopped.
        for child in self.list_panes() {
            let m = child.meta();
            if m.parent_pane.as_deref() != Some(p.id.as_str()) {
                continue;
            }
            let goal = m
                .task
                .as_deref()
                .and_then(|t| self.task_get(t).ok())
                .and_then(|t| t["goal"].as_str().map(summary_line))
                .unwrap_or_default();
            let alive = m.exit.is_none();
            let state = serde_json::to_value(m.state).unwrap_or(Value::Null);
            let busy = alive && matches!(state.as_str(), Some("working") | Some("needs_you"));
            let row = json!({
                "id": format!("pane:{}", m.id), "kind": "helper", "name": m.name,
                "title": goal, "pane": m.id,
                "status": if !alive { json!("ended") } else if busy { json!("running") } else { state.clone() },
                "state": state, "background": true,
                "started_ms": m.proc_started_ms,
                // Stopping a helper is stopping its pane's turn, which the
                // station does for any pane (`pane.stop`).
                "can_stop": busy,
            });
            if busy {
                running.push(row);
            } else {
                finished.push(row);
            }
        }
        let total = finished.len();
        running.truncate(TASKS_RUNNING_MAX);
        // The newest that are over; a sub-agent of the turn now is among
        // them before an older task is.
        let mut finished: Vec<Value> = finished.into_iter().rev().collect();
        finished.sort_by_key(|t| t["current"] != json!(true));
        finished.truncate(TASKS_FINISHED_MAX);
        json!({"running": running, "finished": finished, "finished_total": total})
    }

    /// The record a hook named for a sub-agent that is over, where it is
    /// one of the pane's program's own (subagents.rs, `over_file`).
    fn over_file(
        &self,
        meta: &PaneMeta,
        stem: Option<&str>,
        over: &crate::subagents::SubOver,
    ) -> Option<std::path::PathBuf> {
        let root = match meta.agent {
            spandeck_agents::AgentKind::Codex => {
                Some(spandeck_agents::sessions::codex_sessions_dir(&self.home))
            }
            _ => stem.map(|s| std::path::Path::new(s).join("subagents")),
        };
        crate::subagents::over_file(over.transcript.as_deref(), root.as_deref())
    }

    /// The conversations the pane's program began for sub-agents of this
    /// one, from its own store, read at most every two seconds.
    fn store_children(&self, meta: &PaneMeta, session: &str) -> Vec<stores::Child> {
        use spandeck_agents::AgentKind;
        if !matches!(
            meta.agent,
            AgentKind::OpenCode | AgentKind::Antigravity | AgentKind::Codex
        ) {
            return vec![];
        }
        type Seen = std::collections::HashMap<String, (std::time::Instant, Vec<stores::Child>)>;
        static SEEN: std::sync::OnceLock<std::sync::Mutex<Seen>> = std::sync::OnceLock::new();
        let key = format!("{}|{}|{session}", self.home.display(), meta.id);
        let seen = SEEN.get_or_init(Default::default);
        if let Some((at, kids)) = seen.lock().unwrap().get(&key) {
            if at.elapsed() < std::time::Duration::from_secs(2) {
                return kids.clone();
            }
        }
        let kids = match meta.agent {
            AgentKind::OpenCode => stores::opencode_children(
                &spandeck_archive::usage::opencode_db(&self.home),
                session,
            ),
            // (A child is begun after its parent: only what was written
            // since the session's own file was begun is looked at.)
            AgentKind::Codex => stores::codex_children(
                &spandeck_agents::sessions::codex_sessions_dir(&self.home),
                session,
                self.archive
                    .session(session)
                    .and_then(|s| crate::subagents::first_ms(std::path::Path::new(&s.path)))
                    .or(meta.proc_started_ms)
                    .unwrap_or(0),
            ),
            _ => stores::antigravity_children(&stores::antigravity_dir(&self.home), session),
        };
        let mut seen = seen.lock().unwrap();
        if seen.len() > 64 {
            seen.clear();
        }
        seen.insert(key, (std::time::Instant::now(), kids.clone()));
        kids
    }

    /// What the deck's model chip shows and offers: the model in use (as
    /// the agent's own session data last said), the effort (as its hooks
    /// last said), and what its program takes as a typed command
    /// (`spandeck_agents::models`). `null` for a shell.
    fn conversation_model(&self, p: &PaneEntry, meta: &PaneMeta, session: Option<&str>) -> Value {
        let Some(menu) = spandeck_agents::models::menu(meta.agent) else {
            return Value::Null;
        };
        let usage = self.pane_usage(meta);
        // What the session's file says, in its own order; and what the
        // program printed for a command is kept with the pane.
        let words = session
            .map(|s| self.archive.conversation_set(s))
            .unwrap_or_default();
        let keep = |had: &Option<Said>,
                    now: &Option<spandeck_archive::conversation::Said>|
         -> Option<Said> {
            let (line, name, ts) = now.clone()?;
            let session = session?.to_string();
            match had {
                Some(h) if h.session == session && h.line == line && h.name == name => None,
                _ => Some(Said {
                    name,
                    at_ms: if ts > 0 { ts } else { now_ms() },
                    session,
                    line,
                }),
            }
        };
        let (model_said, effort_said) = (
            keep(&meta.model_said, &words.model),
            keep(&meta.effort_said, &words.effort),
        );
        if model_said.is_some() || effort_said.is_some() {
            let mut m = p.meta.lock().unwrap();
            if let Some(said) = model_said.clone() {
                m.model_said = Some(said);
            }
            if let Some(said) = effort_said.clone() {
                m.effort_said = Some(said);
            }
        }
        let model_said = model_said.or_else(|| meta.model_said.clone());
        let effort_said = effort_said.or_else(|| meta.effort_said.clone());
        let flag = |name: Option<&str>| {
            let at = meta
                .model_args
                .iter()
                .position(|a| Some(a.as_str()) == name)?;
            meta.model_args.get(at + 1).cloned()
        };
        let now = model_now(
            session.unwrap_or(""),
            words.reply,
            model_said.as_ref(),
            meta.model_chosen
                .as_ref()
                .filter(|_| flag(menu.start_model).is_some()),
        );
        // The effort: whichever spoke last of the agent's hooks, what the
        // program printed for its command, and the flag the pane was
        // started with.
        let mut effort = (meta.effort.clone(), meta.effort_at_ms, "hooks");
        if let Some(said) = effort_said.as_ref().filter(|s| s.at_ms >= effort.1) {
            effort = (Some(said.name.clone()), said.at_ms, "said");
        }
        if let (Some(chosen), Some(level)) = (meta.model_chosen.as_ref(), flag(menu.start_effort)) {
            if chosen.at_ms >= effort.1 {
                effort = (Some(level), chosen.at_ms, "chosen");
            }
        }
        json!({
            "model": usage["model"], "effort": effort.0,
            "effort_from": effort.0.as_ref().map(|_| effort.2),
            // The program's own name for the model a command just set
            // ("Sonnet 5.5"), until a reply written after it names the
            // model itself.
            "set": match now { Now::Said => model_said.as_ref().map(|s| s.name.clone()), _ => None },
            // The model the owner chose for this pane alone, which its
            // program was started on, until a reply written after that
            // names the model.
            "chosen": match now { Now::Chosen => flag(menu.start_model), _ => None },
            "models": menu.models.iter().map(|m| json!({"id": m.id, "name": m.name, "says": m.says})).collect::<Vec<_>>(),
            "efforts": if menu.effort_cmd.is_some() { menu.efforts } else { &[] },
            "can_set": menu.model_cmd.is_some(),
            "saves": menu.saves,
            // Its own list of models has a key for "this session only".
            "only_session": menu.session_list.is_some(),
            "only_pane": {
                "model": menu.start_model.is_some(), "effort": menu.start_effort.is_some(),
                // What the pane was last started with, for it alone.
                "args": meta.model_args,
            },
            "picker": menu.picker, "note": menu.note, "source": menu.source,
        })
    }

    /// `pane.model {pane, model? , effort?}`: the owner chooses the pane's
    /// model, or how hard it thinks. What is typed is the agent program's
    /// own command (`spandeck_agents::models`), through the road every
    /// command typed into an agent takes (`agents.command`): only while it
    /// is idle. In the middle of a turn nothing is typed and the answer
    /// says so.
    pub async fn pane_model(&self, pane: &str, params: &Value, by: &str) -> Result<Value> {
        let p = self.pane(pane)?;
        let meta = p.meta();
        let word = |key: &str| params.get(key).and_then(|v| v.as_str());
        // For this pane alone: its program is started again with its own
        // flags and resumes its conversation. Nothing is typed, and the
        // program's saved defaults are not touched.
        // For this session alone, through the program's own list.
        if params.get("session_only").and_then(|v| v.as_bool()) == Some(true) {
            let menu = spandeck_agents::models::menu(meta.agent);
            let (Some(command), Some(model)) = (menu.and_then(|m| m.session_list), word("model"))
            else {
                return Err(invalid(
                    "this program has no list of models with a key for the session alone",
                ));
            };
            if !spandeck_agents::models::name_ok(model) {
                return Err(invalid("a model's name is one word"));
            }
            return self.model_for_session(&p, command, model, by).await;
        }
        if params.get("only_pane").and_then(|v| v.as_bool()) == Some(true) {
            let args = spandeck_agents::models::start_args(
                meta.agent,
                &meta.model_args,
                word("model"),
                word("effort"),
            )
            .map_err(invalid)?;
            if word("model").is_none() && word("effort").is_none() {
                return Err(invalid("give a model or an effort"));
            }
            if meta.exit.is_some() {
                return Err(invalid("this pane has ended"));
            }
            if matches!(
                meta.state,
                spandeck_agents::AgentState::Working | spandeck_agents::AgentState::NeedsYou
            ) {
                return Ok(json!({"ok": false, "sent": false, "busy": true,
                    "why": "in the middle of a turn: starting it again now would end the turn"}));
            }
            if self.resumable(&meta).is_none() {
                return Err(invalid(
                    "this pane has no saved conversation to resume yet: starting it again would begin a new one",
                ));
            }
            // Where its conversation stands now: a reply written after this
            // is by the model it was started on. What the program has
            // written is read first: a last reply not yet copied would
            // otherwise count as written after the choice.
            let session = meta.session_id.clone().unwrap_or_default();
            self.archive.poll_session(&session);
            let line = self.archive.session(&session).map_or(0, |s| s.lines);
            {
                let mut m = p.meta.lock().unwrap();
                m.model_args = args.clone();
                m.model_chosen = Some(Said {
                    name: String::new(),
                    at_ms: now_ms(),
                    session,
                    line,
                });
            }
            self.record(
                "pane.model",
                Some(&p.id),
                meta.project.as_deref(),
                None,
                Some(by),
                json!({"only_pane": true, "args": args}),
            );
            let d = self.clone();
            let (id, who) = (p.id.clone(), by.to_string());
            tokio::task::spawn_blocking(move || d.restart_pane(&id, false, &who).map(|_| ()))
                .await
                .map_err(|e| invalid(e.to_string()))??;
            return Ok(json!({"ok": true, "sent": false, "restarted": true, "args": args}));
        }
        let command = match (word("model"), word("effort")) {
            (Some(model), None) => spandeck_agents::models::model_command(meta.agent, model),
            (None, Some(level)) => spandeck_agents::models::effort_command(meta.agent, level),
            _ => Err("give a model or an effort, one at a time".to_string()),
        }
        .map_err(invalid)?;
        let done = self
            .agents_command(&command, Some(&p.id), None, false, false, by)
            .await?;
        let here = &done["stations"][0];
        if let Some(why) = here["skipped"][0]["why"].as_str() {
            return Ok(
                json!({"ok": false, "sent": false, "command": command, "why": why,
                "busy": why == "in the middle of a turn"}),
            );
        }
        if here["sent"].as_array().is_none_or(|s| s.is_empty()) {
            return Err(invalid("this pane has ended, or runs no agent"));
        }
        // The program may ask before it switches ("Switch model?"): the
        // owner chose this in the sheet, so that question, when it
        // follows this command, is answered (modelswitch.rs).
        if word("model").is_some() {
            self.model_typed(&p.id);
        }
        Ok(json!({"ok": true, "sent": true, "command": command}))
    }

    /// A sub-agent's own transcript, as the items of a conversation: the
    /// end of the file Claude Code keeps for it beside the session. Read
    /// when asked for, kept nowhere, and the owner's only, like every
    /// answer here.
    fn conversation_side(&self, meta: &PaneMeta, session: &str, task: &str) -> Result<Value> {
        // A child conversation of OpenCode's or Antigravity's: its id is
        // one the program's own store lists under this session, and
        // nothing else is read.
        if let Some(id) = task.strip_prefix("child:") {
            let child = self
                .store_children(meta, session)
                .into_iter()
                .find(|c| c.id == id)
                .ok_or_else(|| crate::daemon::Error::NotFound(format!("task {task}")))?;
            let (dialect, lines): (Dialect, Vec<String>) = match meta.agent {
                // Codex's: the end of the thread's own file.
                spandeck_agents::AgentKind::Codex => (
                    Dialect::Codex,
                    child
                        .path
                        .as_deref()
                        .map(|file| stores::tail_lines(file, SIDE_BYTES))
                        .ok_or_else(|| {
                            crate::daemon::Error::NotFound("its file is not there".into())
                        })?,
                ),
                spandeck_agents::AgentKind::OpenCode => (
                    Dialect::OpenCode,
                    stores::opencode_messages(
                        &spandeck_archive::usage::opencode_db(&self.home),
                        &child.id,
                        0,
                        2000,
                    )
                    .into_iter()
                    .map(|(_, line)| line)
                    .collect(),
                ),
                _ => {
                    let file = stores::antigravity_transcript(
                        &stores::antigravity_dir(&self.home),
                        &child.id,
                    )
                    .ok_or_else(|| {
                        crate::daemon::Error::NotFound("its transcript is not there".into())
                    })?;
                    let text =
                        std::fs::read_to_string(&file).map_err(|e| invalid(e.to_string()))?;
                    let mut lines: Vec<String> = text.lines().map(String::from).collect();
                    if lines.len() > 4000 {
                        lines.drain(..lines.len() - 4000);
                    }
                    (Dialect::Antigravity, lines)
                }
            };
            let mut items = spandeck_archive::conversation::side_items(
                dialect,
                lines
                    .iter()
                    .map(|l| l.as_str())
                    .filter(|l| !l.trim().is_empty()),
            );
            let cut = items.len() > PAGE_MAX;
            if cut {
                items.drain(..items.len() - PAGE_MAX);
            }
            return Ok(
                json!({"task": {"id": task, "kind": "agent", "title": child.title}, "items": items, "cut": cut}),
            );
        }
        let stem = self
            .archive
            .session(session)
            .and_then(|s| s.path.strip_suffix(".jsonl").map(String::from));
        let absent = || {
            crate::daemon::Error::NotFound(
                "this sub-agent's transcript is not on the station".into(),
            )
        };
        let mut dialect = Dialect::Claude;
        let (file, title) = if let Some(id) = task.strip_prefix("hook:") {
            // One the hooks said was over: the record they named, where
            // it is the program's own.
            let over = meta
                .subagents_over
                .iter()
                .find(|o| o.id == id && o.session.as_deref() == Some(session))
                .ok_or_else(|| crate::daemon::Error::NotFound(format!("task {task}")))?;
            dialect = Dialect::of(meta.agent.as_str()).unwrap_or(Dialect::Claude);
            (
                self.over_file(meta, stem.as_deref(), over)
                    .ok_or_else(absent)?,
                String::new(),
            )
        } else if let Some(id) = task.strip_prefix("sub:") {
            // A sub-agent's own sub-agent: one the program wrote a file
            // for beside this session, and no other.
            let side = stem
                .as_deref()
                .map(crate::subagents::claude_sides)
                .unwrap_or_default()
                .into_iter()
                .find(|s| s.id == id)
                .ok_or_else(|| crate::daemon::Error::NotFound(format!("task {task}")))?;
            (
                side_file(stem.as_deref(), Some(&side.id))
                    .filter(|f| f.exists())
                    .ok_or_else(absent)?,
                side.title,
            )
        } else {
            let job = self
                .archive
                .conversation_tasks(session)
                .into_iter()
                .find(|j| j.id == task && j.kind == "agent")
                .ok_or_else(|| crate::daemon::Error::NotFound(format!("task {task}")))?;
            let sides = stem
                .as_deref()
                .map(crate::subagents::claude_sides)
                .unwrap_or_default();
            (
                side_file(stem.as_deref(), claude_agent_id(&job, &sides).as_deref())
                    .filter(|f| f.exists())
                    .ok_or_else(absent)?,
                job.title.clone(),
            )
        };
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&file).map_err(|e| invalid(e.to_string()))?;
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let from = len.saturating_sub(SIDE_BYTES);
        f.seek(SeekFrom::Start(from))
            .map_err(|e| invalid(e.to_string()))?;
        let mut bytes = vec![];
        f.take(SIDE_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|e| invalid(e.to_string()))?;
        let text = String::from_utf8_lossy(&bytes);
        // From a line's beginning, and only whole lines.
        let mut lines: Vec<&str> = text.split('\n').collect();
        if from > 0 && !lines.is_empty() {
            lines.remove(0);
        }
        if !text.ends_with('\n') {
            lines.pop();
        }
        let mut items = spandeck_archive::conversation::side_items(
            dialect,
            lines.into_iter().filter(|l| !l.trim().is_empty()),
        );
        let cut = from > 0 || items.len() > PAGE_MAX;
        if items.len() > PAGE_MAX {
            items.drain(..items.len() - PAGE_MAX);
        }
        Ok(
            json!({"task": {"id": task, "kind": "agent", "title": title}, "items": items, "cut": cut}),
        )
    }

    /// What the view needs beside the items, all of it already known to the
    /// station: the pane's state, its tool and since when, and what waits
    /// for the owner. Times are this station's; `now_ms` is its clock, so
    /// the view never compares the browser's clock with a station's.
    pub(crate) fn conversation_live(&self, p: &PaneEntry, session: Option<&str>) -> Value {
        // With what is not written yet laid over the rows (hooks.rs): a
        // request just made is here, one just answered is not.
        let mut pending: Vec<Value> = self
            .approvals(true)
            .into_iter()
            .filter(|a| a["pane"] == p.id.as_str())
            .map(|a| {
                json!({
                    "id": a["id"], "ts": a["ts"], "kind": a["kind"],
                    "summary": a["summary"], "detail": a["detail"],
                })
            })
            .collect();
        pending.sort_by_key(|a| a["ts"].as_i64().unwrap_or(0));
        pending.truncate(20);
        // The pane's state is read after the requests, never before: a pane
        // is marked as needing the owner before its request is stored, so
        // an answer that shows a request also shows the state that goes
        // with it.
        let meta = &p.meta();
        // A held approval is for a call the transcript shows as running. A
        // card tied to the wrong call is worse than a card tied to none, so
        // it is tied only to the call with the same id, when the hook gave
        // one; else to the newest running call of that tool with the same
        // command or file; else, for a tool that has neither, to the only
        // running call of that tool.
        // A question or a plan the hook holds (questions.rs). Read before
        // the pane's state, like the approvals.
        let mut questions = self.questions_held(&p.id);
        let waiting = match session {
            Some(s) if !pending.is_empty() || !questions.is_empty() => {
                self.archive.conversation_waiting(s)
            }
            _ => vec![],
        };
        // Tied to its item by the call's own id; else to the only call of
        // that tool that waits; else to none, and the card is drawn all
        // the same from what the hook was given.
        for q in questions.iter_mut() {
            let name = match q["kind"].as_str() {
                Some("plan") => "ExitPlanMode",
                _ => "AskUserQuestion",
            };
            let item = waiting
                .iter()
                .find(|w| q["tool_use_id"].as_str() == Some(w.tool_id.as_str()))
                .or_else(|| {
                    let mut same = waiting.iter().filter(|w| w.name == name);
                    match (same.next(), same.next()) {
                        (Some(only), None) => Some(only),
                        _ => None,
                    }
                })
                .map(|w| w.item.clone());
            q["item"] = json!(item);
            if let Some(o) = q.as_object_mut() {
                o.remove("tool_use_id");
            }
        }
        let approvals: Vec<Value> = pending
            .iter()
            .map(|a| {
                let tool = &a["detail"]["tool"];
                let named = |w: &&Waiting| tool["name"].as_str() == Some(w.name.as_str());
                let what: Vec<String> = [&tool["command"], &tool["file_path"]]
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(summary_line)
                    .collect();
                let item = waiting
                    .iter()
                    .find(|w| tool["id"].as_str() == Some(w.tool_id.as_str()))
                    .or_else(|| {
                        waiting
                            .iter()
                            .rev()
                            .filter(named)
                            .find(|w| what.contains(&w.summary))
                    })
                    .or_else(|| {
                        let mut same = waiting.iter().filter(named);
                        match (same.next(), same.next()) {
                            (Some(only), None) if what.is_empty() => Some(only),
                            _ => None,
                        }
                    })
                    .map(|w| w.item.clone());
                json!({
                    "kind": "approval", "id": format!("approval:{}", a["id"].as_str().unwrap_or("")),
                    "approval": a["id"], "what": a["kind"], "summary": a["summary"],
                    "tool": tool["name"], "ts_ms": a["ts"], "item": item,
                })
            })
            .collect();
        json!({
            "state": meta.state, "state_since_ms": meta.state_since_ms,
            "needs_reason": meta.needs_reason,
            "tool": meta.tool, "tool_since_ms": meta.tool_since_ms,
            "subagents": meta.subagents, "paused": meta.paused,
            "held": self.held_view(meta),
            "trust": self.folder_trust_view(meta),
            "prompt": meta.prompt,
            "exited": meta.exit.is_some(), "resumed": meta.resumed,
            "hooks_seen": meta.hooks_seen,
            "approvals": approvals,
            "questions": questions,
            "tasks": self.conversation_tasks(p, meta, session),
            "model": self.conversation_model(p, meta, session),
            "cleared": match session {
                Some(s) => self.notes_cleared(&p.id, s),
                None => self.notes_cleared(&p.id, &Self::screen_session(meta)),
            },
            "unread": self.conversation_unread(meta),
            "read_line": session.map(|s| self.read_line(&p.id, s)),
            "now_ms": now_ms(),
        })
    }

    /// `pane.conversation {pane, after?, before?, limit?, session?}`.
    pub fn conversation(&self, pane: &str, params: &Value) -> Result<Value> {
        let p = self.pane(pane)?;
        let meta = p.meta();
        let num = |key: &str| params.get(key).and_then(|v| v.as_u64());
        let limit = num("limit")
            .map_or(PAGE_DEFAULT, |l| l as usize)
            .min(PAGE_MAX);
        let mut out = json!({
            "pane": p.id, "station": self.station, "agent": meta.agent,
            "available": false, "items": [], "updated": [],
        });
        // A caller that can show the screen as a conversation says so
        // (`screen: true`); one from before is answered as before.
        let screen = params.get("screen").and_then(|v| v.as_bool()) == Some(true);
        let (session, from) = match self.conversation_session(&meta) {
            Ok(s) => s,
            Err(why) if screen => return self.screen_conversation(&p, &meta, params, &why, out),
            Err(why) => {
                out["reason"] = json!(format!("no conversation for this pane: {why}"));
                out["live"] = self.conversation_live(&p, None);
                return Ok(out);
            }
        };
        // A cursor from another session of this pane (the agent was
        // restarted, or `/clear` began a new one) means nothing here: the
        // caller is told to start again, and gets the newest items.
        let reset = params
            .get("session")
            .and_then(|s| s.as_str())
            .is_some_and(|s| s != session);
        let range = match (num("after"), num("before")) {
            _ if reset => Range::Tail,
            (Some(_), Some(_)) => return Err(invalid("give after or before, not both")),
            (Some(line), None) => Range::After(line),
            (None, Some(line)) => Range::Before(line),
            (None, None) => Range::Tail,
        };
        let Some(mut page) = self.archive.conversation(&session, range, limit) else {
            if screen {
                let why = "its session is not in the archive";
                return self.screen_conversation(&p, &meta, params, why, out);
            }
            out["reason"] =
                json!("no conversation for this pane: its session is not in the archive");
            out["live"] = self.conversation_live(&p, None);
            return Ok(out);
        };
        // A plan that was held for the owner is shown as its hook was
        // given it: the call's record can hold an earlier plan
        // (questions.rs, `plans_given`).
        let plan =
            |i: &spandeck_archive::conversation::Item| i.kind == "plan" && i.tool_id.is_some();
        if page.items.iter().chain(&page.updated).any(plan) {
            let given = self.plans_given(&p.id);
            for item in page.items.iter_mut().chain(page.updated.iter_mut()) {
                let text = given
                    .iter()
                    .find(|(id, _)| item.kind == "plan" && Some(id) == item.tool_id.as_ref());
                if let Some((_, text)) = text {
                    item.set_text(text);
                }
            }
        }
        let page = serde_json::to_value(page).map_err(|e| invalid(e.to_string()))?;
        if let (Some(out), Some(page)) = (out.as_object_mut(), page.as_object()) {
            out.extend(page.clone());
        }
        out["available"] = json!(true);
        out["source"] = json!("transcript");
        out["session_from"] = json!(from);
        out["session"] = json!(session);
        out["reset"] = json!(reset);
        out["live"] = self.conversation_live(&p, Some(&session));
        self.attach_decorate(&p.id, &mut out);
        self.made_decorate(&p.id, &meta.cwd, &mut out);
        Ok(out)
    }

    /// `pane.conversation.detail {pane, item, offset?, session?}`, or
    /// `{pane, task, session?}` for a sub-agent's own transcript.
    pub fn conversation_detail(&self, pane: &str, params: &Value) -> Result<Value> {
        let p = self.pane(pane)?;
        let meta = p.meta();
        let item = params
            .get("item")
            .or_else(|| params.get("task"))
            .and_then(|i| i.as_str())
            .ok_or_else(|| invalid("missing parameter: item"))?;
        // An item of the screen's conversation (screentalk.rs).
        if params
            .get("session")
            .and_then(|s| s.as_str())
            .is_some_and(|s| s.starts_with("screen:"))
        {
            return self.screen_detail(&p, &meta, params);
        }
        let (session, _) = self
            .conversation_session(&meta)
            .map_err(|why| invalid(format!("no conversation for this pane: {why}")))?;
        if params
            .get("session")
            .and_then(|s| s.as_str())
            .is_some_and(|s| s != session)
        {
            return Err(invalid(
                "that item is from another session of this pane: ask for the conversation again",
            ));
        }
        // A sub-agent's own transcript: `task` is the id of its row in
        // `live.tasks`.
        if let Some(task) = params.get("task").and_then(|t| t.as_str()) {
            let mut side = self.conversation_side(&meta, &session, task)?;
            side["pane"] = json!(p.id);
            side["session"] = json!(session);
            return Ok(side);
        }
        let offset = params.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) as usize;
        let mut detail = self
            .archive
            .conversation_detail(&session, item, offset)
            .ok_or_else(|| crate::daemon::Error::NotFound(format!("item {item}")))?;
        // A plan: the call's id came along for this, and is not served.
        let call = detail
            .as_object_mut()
            .and_then(|d| d.remove("tool_use_id"))
            .and_then(|id| id.as_str().map(String::from));
        if let Some(call) = call {
            let given = self.plans_given(&p.id);
            if let Some((_, text)) = given.iter().find(|(id, _)| *id == call) {
                let clean = spandeck_archive::conversation::clean(text);
                detail["text"] = spandeck_archive::conversation::paged(&clean, offset);
                if let Some(item) = detail["item"].as_object_mut() {
                    let (cut, more) = spandeck_archive::conversation::cap(
                        &clean,
                        spandeck_archive::conversation::TEXT_CAP,
                    );
                    item.insert("text".into(), json!(cut));
                    if more {
                        item.insert("more".into(), json!(true));
                    }
                }
            }
        }
        detail["pane"] = json!(p.id);
        detail["session"] = json!(session);
        Ok(detail)
    }
}

impl Daemon {
    /// Refuse a message for a pane whose program is asking a question of
    /// its own on its screen: typed there it would be taken as the answer
    /// and lost (seen with the real Claude Code on 17 Nov 2026). The
    /// caller keeps what was written.
    pub(crate) fn not_into_a_prompt(&self, pane: &str) -> Result<()> {
        let p = self.pane(pane)?;
        match self.prompt_up(&p) {
            Some(asks) => Err(invalid(format!(
                "not sent: {} is asking {asks} in this pane, and a message typed now would be taken as its answer. Answer it first (above the message box, or in the terminal), then send again",
                p.meta().agent.display()
            ))),
            None => Ok(()),
        }
    }
}

/// The program's own id of the sub-agent a call in the session's file
/// began: what the call's result says (`agentId`), and before there is
/// one, the file Claude Code wrote beside the session when the sub-agent
/// began, which names the call (`toolUseId`).
fn claude_agent_id(
    job: &spandeck_archive::conversation::Job,
    sides: &[crate::subagents::Side],
) -> Option<String> {
    job.task_id.clone().or_else(|| {
        sides
            .iter()
            .find(|s| !job.tool_id.is_empty() && s.tool_use_id == job.tool_id && s.depth <= 1)
            .map(|s| s.id.clone())
    })
}

/// Where Claude Code keeps a sub-agent's own transcript: beside the
/// session's file, `<session>/subagents/agent-<id>.jsonl` (its
/// documentation, sub-agents.md, read 16 Nov 2026). The id is the
/// program's, from the session file: it is used only when it is a plain
/// word, so it can name no other file.
fn side_file(stem: Option<&str>, task_id: Option<&str>) -> Option<std::path::PathBuf> {
    let id = task_id?;
    let plain = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    plain
        .then(|| {
            std::path::Path::new(stem?)
                .join("subagents")
                .join(format!("agent-{id}.jsonl"))
                .into()
        })
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's report of 17 Nov 2026, from a phone: "when I changed
    /// from fable to opus, it either didn't work or it didn't update the
    /// model name". The program had switched; the chip named the model of
    /// the last reply, and nothing told it of the change. Which word
    /// holds is the order of the conversation, in both orders, and
    /// through a program started again.
    #[test]
    fn the_last_word_on_a_panes_model_holds() {
        let at = |session: &str, line: u64, at_ms: u64| Said {
            name: "Opus 5.5".into(),
            at_ms,
            session: session.into(),
            line,
        };
        // Nothing but replies.
        assert_eq!(model_now("s", Some((9, 100)), None, None), Now::Reply);
        // The command's words after the last reply: they hold. A reply
        // after them: the reply.
        assert_eq!(
            model_now("s", Some((9, 100)), Some(&at("s", 12, 200)), None),
            Now::Said
        );
        assert_eq!(
            model_now("s", Some((14, 300)), Some(&at("s", 12, 200)), None),
            Now::Reply
        );
        // Before any reply at all.
        assert_eq!(
            model_now("s", None, Some(&at("s", 2, 200)), None),
            Now::Said
        );
        // Started again on a model at line 9: the reply on line 9 was by
        // the model before; one after it is the truth.
        assert_eq!(
            model_now("s", Some((9, 100)), None, Some(&at("s", 9, 400))),
            Now::Chosen
        );
        assert_eq!(
            model_now("s", Some((11, 500)), None, Some(&at("s", 9, 400))),
            Now::Reply
        );
        // Both: the later of the two.
        assert_eq!(
            model_now(
                "s",
                Some((9, 100)),
                Some(&at("s", 12, 200)),
                Some(&at("s", 12, 400))
            ),
            Now::Chosen
        );
        assert_eq!(
            model_now(
                "s",
                Some((9, 100)),
                Some(&at("s", 15, 600)),
                Some(&at("s", 12, 400))
            ),
            Now::Said
        );
        // The program began another session when it was started again:
        // by the records' own times.
        assert_eq!(
            model_now("t", Some((3, 100)), Some(&at("s", 12, 200)), None),
            Now::Said
        );
        assert_eq!(
            model_now("t", Some((3, 100)), None, Some(&at("s", 12, 400))),
            Now::Chosen
        );
        assert_eq!(
            model_now(
                "t",
                Some((30, 900)),
                Some(&at("s", 12, 200)),
                Some(&at("s", 12, 400))
            ),
            Now::Reply
        );
        // And a reply with no time is taken to be the newer.
        assert_eq!(
            model_now("t", Some((3, 0)), Some(&at("s", 12, 200)), None),
            Now::Reply
        );
    }
}
