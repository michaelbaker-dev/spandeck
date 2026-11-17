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
        for job in jobs {
            let mut row = serde_json::to_value(&job).unwrap_or(Value::Null);
            // A task the file still has as running cannot be: its program
            // has ended, or was started again since it began.
            let before = match (job.started_ms, meta.proc_started_ms) {
                (Some(began), Some(program)) => began < program,
                _ => false,
            };
            if job.status == "running" && (meta.exit.is_some() || before) {
                row["status"] = json!("lost");
            }
            let side = job.kind == "agent"
                && side_file(stem.as_deref(), job.task_id.as_deref()).is_some_and(|f| f.exists());
            row["transcript"] = json!(side);
            if row["status"] == json!("running") {
                running.push(row);
            } else {
                finished.push(row);
            }
        }
        // A program that keeps a conversation of its own for each
        // sub-agent (OpenCode, Antigravity): what its store says of them.
        for child in session.map_or(vec![], |s| self.store_children(meta, s)) {
            let dialect = meta.agent.as_str();
            // Antigravity's store does not say whether one is still at
            // work: it is taken to be while its transcript was written to
            // in the last fifteen seconds, and the row says which it is.
            let now = now_ms();
            let (run, sure) = match child.running {
                Some(r) => (r, true),
                None => (now.saturating_sub(child.modified_ms) < 15_000, false),
            };
            let run = run && meta.exit.is_none();
            let row = json!({
                "id": format!("child:{}", child.id), "kind": "agent",
                "title": summary_line(&child.title),
                "status": if run { "running" } else { "done" },
                "background": false, "started_ms": child.started_ms,
                "ended_ms": if run { Value::Null } else { json!(child.modified_ms) },
                "transcript": true, "from": dialect, "sure": sure,
            });
            if run {
                running.push(row);
            } else {
                finished.push(row);
            }
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
        let finished: Vec<Value> = finished
            .into_iter()
            .rev()
            .take(TASKS_FINISHED_MAX)
            .collect();
        json!({"running": running, "finished": finished, "finished_total": total})
    }

    /// The conversations the pane's program began for sub-agents of this
    /// one, from its own store, read at most every two seconds.
    fn store_children(&self, meta: &PaneMeta, session: &str) -> Vec<stores::Child> {
        use spandeck_agents::AgentKind;
        if !matches!(meta.agent, AgentKind::OpenCode | AgentKind::Antigravity) {
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
    fn conversation_model(&self, meta: &PaneMeta) -> Value {
        let Some(menu) = spandeck_agents::models::menu(meta.agent) else {
            return Value::Null;
        };
        let usage = self.pane_usage(meta);
        json!({
            "model": usage["model"], "effort": meta.effort,
            "models": menu.models.iter().map(|m| json!({"id": m.id, "name": m.name, "says": m.says})).collect::<Vec<_>>(),
            "efforts": if menu.effort_cmd.is_some() { menu.efforts } else { &[] },
            "can_set": menu.model_cmd.is_some(),
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
        let job = self
            .archive
            .conversation_tasks(session)
            .into_iter()
            .find(|j| j.id == task && j.kind == "agent")
            .ok_or_else(|| crate::daemon::Error::NotFound(format!("task {task}")))?;
        let stem = self
            .archive
            .session(session)
            .and_then(|s| s.path.strip_suffix(".jsonl").map(String::from));
        let file = side_file(stem.as_deref(), job.task_id.as_deref())
            .filter(|f| f.exists())
            .ok_or_else(|| {
                crate::daemon::Error::NotFound(
                    "this sub-agent's transcript is not on the station".into(),
                )
            })?;
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
        let dialect = Dialect::Claude;
        let mut items = spandeck_archive::conversation::side_items(
            dialect,
            lines.into_iter().filter(|l| !l.trim().is_empty()),
        );
        let cut = from > 0 || items.len() > PAGE_MAX;
        if items.len() > PAGE_MAX {
            items.drain(..items.len() - PAGE_MAX);
        }
        Ok(json!({"task": job, "items": items, "cut": cut}))
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
            "exited": meta.exit.is_some(), "resumed": meta.resumed,
            "hooks_seen": meta.hooks_seen,
            "approvals": approvals,
            "questions": questions,
            "tasks": self.conversation_tasks(p, meta, session),
            "model": self.conversation_model(meta),
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
