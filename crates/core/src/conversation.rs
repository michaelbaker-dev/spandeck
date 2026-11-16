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

use crate::daemon::{invalid, Daemon, PaneEntry, PaneMeta, Result};
use crate::paths::now_ms;

fn read_key(pane: &str) -> String {
    format!("conversation.read.{pane}")
}

impl Daemon {
    /// The session a pane's conversation is read from, or why it has none.
    fn conversation_session(&self, meta: &PaneMeta) -> std::result::Result<String, String> {
        if !meta.agent.is_agent() {
            return Err("it runs a shell, not an agent".into());
        }
        if Dialect::of(meta.agent.as_str()).is_none() {
            return Err(format!(
                "{} keeps its conversation where spandeck cannot read it yet",
                meta.agent.display()
            ));
        }
        let Some(session) = meta.session_id.clone() else {
            return Err(format!(
                "{} has not said which session it is in yet (nothing has come from its hooks)",
                meta.agent.display()
            ));
        };
        if self.archive.session(&session).is_none() {
            // A pane from before a restart whose session the archive was
            // never told of: follow it now, if it says where its file is.
            let Some(path) = meta.transcript_path.as_deref() else {
                return Err(format!(
                    "{} has not said where its session file is",
                    meta.agent.display()
                ));
            };
            self.archive.follow(
                &session,
                meta.agent.as_str(),
                path,
                meta.project.as_deref(),
                Some(&meta.id),
            );
        }
        Ok(session)
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
        let Some(session) = meta.session_id.as_deref() else {
            return false;
        };
        if Dialect::of(meta.agent.as_str()).is_none() {
            return false;
        }
        self.archive.last_reply_line(session) > self.read_line(&meta.id, session)
    }

    /// `pane.read`: the owner has read this pane's conversation up to a
    /// line (all of it, when no line is given). The mark only moves forward.
    pub fn conversation_read(&self, pane: &str, upto: Option<u64>) -> Result<Value> {
        let p = self.pane(pane)?;
        let meta = p.meta();
        let session = self
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
        self.db.put(crate::db::Op::KvDel { k: read_key(pane) });
    }

    /// What the view needs beside the items, all of it already known to the
    /// station: the pane's state, its tool and since when, and what waits
    /// for the owner. Times are this station's; `now_ms` is its clock, so
    /// the view never compares the browser's clock with a station's.
    fn conversation_live(&self, p: &PaneEntry, session: Option<&str>) -> Value {
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
            "exited": meta.exit.is_some(), "resumed": meta.resumed,
            "hooks_seen": meta.hooks_seen,
            "approvals": approvals,
            "questions": questions,
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
        let session = match self.conversation_session(&meta) {
            Ok(s) => s,
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
        let Some(page) = self.archive.conversation(&session, range, limit) else {
            out["reason"] =
                json!("no conversation for this pane: its session is not in the archive");
            out["live"] = self.conversation_live(&p, None);
            return Ok(out);
        };
        let page = serde_json::to_value(page).map_err(|e| invalid(e.to_string()))?;
        if let (Some(out), Some(page)) = (out.as_object_mut(), page.as_object()) {
            out.extend(page.clone());
        }
        out["available"] = json!(true);
        out["session"] = json!(session);
        out["reset"] = json!(reset);
        out["live"] = self.conversation_live(&p, Some(&session));
        Ok(out)
    }

    /// `pane.conversation.detail {pane, item, offset?, session?}`.
    pub fn conversation_detail(&self, pane: &str, params: &Value) -> Result<Value> {
        let p = self.pane(pane)?;
        let meta = p.meta();
        let item = params
            .get("item")
            .and_then(|i| i.as_str())
            .ok_or_else(|| invalid("missing parameter: item"))?;
        let session = self
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
        let offset = params.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) as usize;
        let mut detail = self
            .archive
            .conversation_detail(&session, item, offset)
            .ok_or_else(|| crate::daemon::Error::NotFound(format!("item {item}")))?;
        detail["pane"] = json!(p.id);
        detail["session"] = json!(session);
        Ok(detail)
    }
}
