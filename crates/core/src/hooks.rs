//! The hook pipeline: one vendor payload in, one normalized event applied,
//! one decision out.
//!
//! Observing and enforcing are separate. This is the only place that can
//! block an agent (Guarantees: Enforcement).

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use spandeck_agents::{
    adapter, AgentEvent, AgentKind, AgentState, EventKind, HookDecision, HookReply,
};

use crate::approvals::{Answer, Asked};
use crate::daemon::{invalid, ApprovalAnswer, Daemon, PaneEntry, PaneMeta, Result, TurnStats};
use crate::paths::now_ms;

/// What answering an approval came to, for whoever answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answered {
    /// The answer is in the database (false: held in memory, and written
    /// when the database takes it).
    pub saved: bool,
    /// The hook that asked has the answer. False: it is not connected at
    /// this moment, and will be given the answer when it is back.
    pub delivered: bool,
}

impl Daemon {
    /// `hook_id`: the id the hook made for this one firing, sent again with
    /// every try. What it asks the owner is found again by it after the
    /// connection was lost (approvals.rs).
    pub async fn hook_ingest(
        &self,
        pane_id: &str,
        vendor: &str,
        payload: Value,
        hook_id: Option<&str>,
    ) -> Result<HookReply> {
        let p = self.pane(pane_id)?;
        let kind =
            AgentKind::parse(vendor).ok_or_else(|| invalid(format!("unknown agent {vendor}")))?;
        let a = adapter(kind).ok_or_else(|| invalid("that agent has no hooks"))?;
        let Some(ev) = a.normalize(&payload) else {
            return Ok(HookReply::default());
        };
        // With a vault connected, raw keys do not belong in commands, files or
        // prompts: block the tool call and say what to do instead.
        if self.vault_connected() {
            let raw_event = payload
                .get("hook_event_name")
                .and_then(|e| e.as_str())
                .unwrap_or("");
            let text = match raw_event {
                "PreToolUse" => payload.get("tool_input").map(|t| t.to_string()),
                "UserPromptSubmit" => payload
                    .get("prompt")
                    .and_then(|t| t.as_str())
                    .map(String::from),
                _ => None,
            };
            let found = text
                .map(|t| spandeck_archive::find_keys(&t))
                .unwrap_or_default();
            if let Some((kind, _)) = found.first() {
                let meta = p.meta();
                for (_, value) in &found {
                    self.vault_report(
                        value,
                        if raw_event == "PreToolUse" {
                            "a command or file an agent wrote"
                        } else {
                            "a prompt"
                        },
                    )
                    .await;
                }
                if raw_event == "PreToolUse" {
                    self.record(
                        "order.blocked",
                        Some(&p.id),
                        meta.project.as_deref(),
                        meta.task.as_deref(),
                        None,
                        json!({"order": "no-raw-keys", "layer": "fleet", "tool": payload.get("tool_name")}),
                    );
                    return Ok(a.reply(
                        "PreToolUse",
                        &HookDecision::deny(format!(
                            "Blocked by spandeck: this contains what looks like a {kind} key. Keys must not be put in commands or files. Use the vault tools instead (vault_list, then vault_call) so the key is attached for you and never appears here. If the key came from the user, tell them it is now exposed and should be rotated."
                        )),
                    ));
                }
                self.notify(
                    "error",
                    Some(&p.id),
                    meta.project.as_deref(),
                    &format!("A {kind} key was pasted into {}", meta.name),
                    "It is now in that agent's transcript. Rotate it in the vault, and use the vault tools instead of pasting keys.",
                );
            }
        }
        let decision = self.apply_hook_event(&p, &ev, hook_id).await;
        Ok(a.reply(&ev.raw_event, &decision))
    }

    /// Apply a normalized event to a pane and decide what to tell the agent.
    /// Hook-driven and API-driven agents both end up here.
    pub async fn apply_agent_event(&self, p: &PaneEntry, ev: &AgentEvent) -> HookDecision {
        self.apply_hook_event(p, ev, None).await
    }

    /// `apply_agent_event`, for an event a hook sent under an id of its own.
    #[allow(clippy::collapsible_match)]
    pub async fn apply_hook_event(
        &self,
        p: &PaneEntry,
        ev: &AgentEvent,
        hook_id: Option<&str>,
    ) -> HookDecision {
        let now = now_ms();
        let mut stuck: Option<String> = None;
        let (meta, session_changed) = {
            let mut m = p.meta.lock().unwrap();
            m.hooks_seen = true;
            // A pane started as a shell that turns out to run an agent.
            if !m.agent.is_agent() {
                m.agent = ev.agent;
            }
            let mut session_changed = false;
            if let Some(sid) = &ev.session_id {
                if m.session_id.as_deref() != Some(sid) {
                    m.session_id = Some(sid.clone());
                    session_changed = true;
                }
            }
            if let Some(tp) = &ev.transcript_path {
                if m.transcript_path.as_deref() != Some(tp) {
                    m.transcript_path = Some(tp.clone());
                    session_changed = true;
                }
            }
            match ev.kind {
                EventKind::PromptSubmit => {
                    let idle_turns = m.turn.idle_turns;
                    m.turn = TurnStats {
                        started_ms: now,
                        idle_turns,
                        ..Default::default()
                    };
                    m.last_message = ev.message.clone();
                }
                EventKind::ToolStart => {
                    m.tool = ev.tool.as_ref().map(|t| t.name.clone());
                    m.tool_since_ms = now;
                    m.turn.tool_calls += 1;
                }
                EventKind::ToolEnd => {
                    m.tool = None;
                    if let Some(t) = &ev.tool {
                        let ok = t.ok.unwrap_or(true);
                        if ok && t.writes {
                            m.turn.file_changes += 1;
                        }
                        match (&t.command, ok) {
                            (Some(cmd), false) => {
                                if m.turn.fail_cmd.as_deref() == Some(cmd.as_str()) {
                                    m.turn.fail_streak += 1;
                                } else {
                                    m.turn.fail_cmd = Some(cmd.clone());
                                    m.turn.fail_streak = 1;
                                }
                                if m.turn.fail_streak >= self.cfg.stuck_same_failure {
                                    stuck = Some(format!(
                                        "stuck: the same command failed {} times in a row ({})",
                                        m.turn.fail_streak,
                                        truncate(cmd, 80)
                                    ));
                                }
                            }
                            (Some(_), true) => {
                                m.turn.fail_cmd = None;
                                m.turn.fail_streak = 0;
                            }
                            _ => {}
                        }
                    }
                }
                EventKind::SubagentStart => {
                    if m.subagents.len() < 24 {
                        m.subagents.push(crate::daemon::SubAgent {
                            kind: ev.message.clone().unwrap_or_else(|| "sub-agent".into()),
                            since_ms: now,
                        });
                    }
                }
                EventKind::SubagentStop => {
                    let kind = ev.message.clone().unwrap_or_else(|| "sub-agent".into());
                    match m.subagents.iter().position(|s| s.kind == kind) {
                        Some(i) => {
                            m.subagents.remove(i);
                        }
                        None => {
                            m.subagents.pop();
                        }
                    }
                }
                EventKind::TurnEnd | EventKind::TurnFailed | EventKind::Interrupted => {
                    m.tool = None;
                    m.subagents.clear();
                    m.last_message = ev.message.clone().or(m.last_message.take());
                    if ev.kind == EventKind::TurnEnd {
                        if m.turn.tool_calls >= 5 && m.turn.file_changes == 0 {
                            m.turn.idle_turns += 1;
                        } else if m.turn.file_changes > 0 {
                            m.turn.idle_turns = 0;
                        }
                        if m.turn.idle_turns >= self.cfg.stuck_idle_turns {
                            stuck = Some(format!(
                                "stuck: {} turns in a row used tools but changed no files",
                                m.turn.idle_turns
                            ));
                        }
                    }
                }
                EventKind::SessionStart => {
                    m.tool = None;
                    m.subagents.clear();
                }
                _ => {}
            }
            (m.clone(), session_changed)
        };
        // A question or a plan held for the owner belongs to its turn.
        // When that turn is over, or another begins, it is not something
        // the owner can still answer (questions.rs).
        if matches!(
            ev.kind,
            EventKind::PromptSubmit
                | EventKind::TurnEnd
                | EventKind::TurnFailed
                | EventKind::Interrupted
                | EventKind::SessionStart
                | EventKind::SessionEnd
        ) {
            self.questions_moot(p);
        }

        if session_changed {
            self.save_pane(p);
            if let (Some(sid), Some(path)) = (&meta.session_id, &meta.transcript_path) {
                self.archive.follow(
                    sid,
                    meta.agent.as_str(),
                    path,
                    meta.project.as_deref(),
                    Some(&meta.id),
                );
            }
            self.emit(
                "pane.session",
                Some(&p.id),
                meta.project.as_deref(),
                json!({"session_id": meta.session_id}),
            );
        }
        if matches!(ev.kind, EventKind::SessionStart | EventKind::TurnEnd) {
            if let Some(sid) = &meta.session_id {
                self.archive
                    .set_commit(sid, crate::git::head(Path::new(&meta.cwd)).as_deref());
            }
        }

        // The ship's log keeps a tool call's name, timing and result, not its contents.
        match ev.kind {
            EventKind::ToolEnd => {
                if let Some(t) = &ev.tool {
                    self.record(
                        "agent.tool",
                        Some(&p.id),
                        meta.project.as_deref(),
                        meta.task.as_deref(),
                        None,
                        json!({"tool": t.name, "ok": t.ok, "duration_ms": t.duration_ms}),
                    );
                }
            }
            EventKind::ToolStart => {
                self.emit(
                    "agent.tool_start",
                    Some(&p.id),
                    meta.project.as_deref(),
                    json!({"tool": ev.tool.as_ref().map(|t| t.name.clone())}),
                );
            }
            EventKind::SessionStart
            | EventKind::SessionEnd
            | EventKind::Compact
            | EventKind::TurnFailed => {
                self.record(
                    &format!(
                        "agent.{}",
                        serde_json::to_value(ev.kind)
                            .ok()
                            .and_then(|v| v.as_str().map(String::from))
                            .unwrap_or_default()
                    ),
                    Some(&p.id),
                    meta.project.as_deref(),
                    meta.task.as_deref(),
                    None,
                    json!({"source": ev.source, "session_id": ev.session_id}),
                );
            }
            _ => {}
        }

        self.memory_capture(&meta, ev);

        // Decide.
        let mut decision = HookDecision::none();
        match ev.kind {
            EventKind::ToolStart => {
                if let Some(tool) = &ev.tool {
                    if let Some(block) = self.orders_check(&meta, tool) {
                        self.record(
                            "order.blocked",
                            Some(&p.id),
                            meta.project.as_deref(),
                            meta.task.as_deref(),
                            None,
                            json!({"order": block.order, "layer": block.layer, "tool": tool.name}),
                        );
                        self.set_state(p, AgentState::Working, None);
                        return HookDecision::deny(block.reason);
                    }
                }
            }
            EventKind::PermissionRequest => {
                let summary = ev
                    .tool
                    .as_ref()
                    .map(|t| match (&t.command, &t.file_path) {
                        (Some(c), _) => format!("{}: {}", t.name, truncate(c, 160)),
                        (None, Some(f)) => format!("{} {}", t.name, f),
                        _ => t.name.clone(),
                    })
                    .unwrap_or_else(|| "permission".into());
                self.set_state(
                    p,
                    AgentState::NeedsYou,
                    Some(format!("wants to run {summary}")),
                );
                let answer = self
                    .request_approval(p, "permission", &summary, json!({"tool": ev.tool}), hook_id)
                    .await;
                return match answer {
                    Some(ApprovalAnswer { allow: true, .. }) => {
                        self.set_state(p, AgentState::Working, None);
                        HookDecision::allow()
                    }
                    Some(ApprovalAnswer {
                        allow: false,
                        reason,
                    }) => {
                        self.set_state(p, AgentState::Working, None);
                        HookDecision::deny(
                            reason.unwrap_or_else(|| "Denied from the Control Deck.".into()),
                        )
                    }
                    // No remote answer in time: the agent's own prompt takes over.
                    None => HookDecision::none(),
                };
            }
            EventKind::SessionStart => {
                let mut parts: Vec<String> = vec![];
                if let Some(d) = self.start_digest(&meta, ev.source.as_deref()) {
                    parts.push(d);
                }
                if let Some(o) = self.orders_guidance(&meta) {
                    parts.push(o);
                }
                // No mail notice here: the agent is not in a turn yet, so it
                // would not act on one. The nudge wakes an idle agent instead.
                if !parts.is_empty() {
                    decision.context = Some(parts.join("\n\n"));
                }
            }
            // Mail by a hook: none for a pane that is paused, stopped or
            // waiting for the owner after a restart. `mail_notice` looks at
            // the marks as they are now (holds.rs).
            EventKind::PromptSubmit | EventKind::ToolEnd => {
                decision.context = self.mail_notice(&meta.id);
            }
            EventKind::TurnEnd => {
                // Pause: no new work is delivered; the current turn finishes.
                // And a stopped turn is not continued with mail.
                if stuck.is_none() {
                    decision.continue_with = self.mail_notice(&meta.id);
                }
            }
            _ => {}
        }

        if let Some(reason) = stuck {
            self.set_state(p, AgentState::NeedsYou, Some(reason.clone()));
            self.record(
                "agent.stuck",
                Some(&p.id),
                meta.project.as_deref(),
                meta.task.as_deref(),
                None,
                json!({"reason": reason}),
            );
        } else if ev.kind == EventKind::ToolStart
            && ev
                .tool
                .as_ref()
                .is_some_and(|t| crate::questions::kind_of_tool(&t.name).is_some())
            && self.approvals.any_waiting()
            && self.approvals.holds(&p.id)
        {
            // The call that asks the owner a question, seen by the hook
            // every tool call has, while the other hook of the same call
            // holds it for the owner: the pane needs the owner, and this
            // hook does not say "working" over that (the two run side by
            // side, in either order).
        } else if let Some(state) = ev.state {
            // A turn that is being continued with mail is still working.
            let state = if decision.continue_with.is_some() {
                AgentState::Working
            } else {
                state
            };
            let reason = (state == AgentState::NeedsYou).then(|| {
                ev.message
                    .clone()
                    .unwrap_or_else(|| "waiting for you".into())
            });
            self.set_state(p, state, reason);
        }
        decision
    }

    // -------------------------------------------------------------- approvals

    /// Ask the user, wherever they are. None when nobody answered in time.
    ///
    /// `hook_id`: the id of the hook that asks, when it sent one. A hook
    /// that lost its connection while it waited (the daemon was upgraded,
    /// started again, or ended) asks again with the same id and waits on
    /// the same approval, or is given the answer the owner gave meanwhile.
    pub async fn request_approval(
        &self,
        p: &PaneEntry,
        kind: &str,
        summary: &str,
        detail: Value,
        hook_id: Option<&str>,
    ) -> Option<ApprovalAnswer> {
        let meta = p.meta();
        let asked = self.approvals.ask(
            &p.id,
            meta.project.as_deref(),
            kind,
            summary,
            detail,
            hook_id,
            now_ms(),
            Duration::from_secs(self.cfg.approval_timeout_secs),
        );
        let (id, waiter, rx, left, new) = match asked {
            Asked::Wait {
                id,
                waiter,
                rx,
                left,
                new,
            } => (id, waiter, rx, left, new),
            Asked::Answered { id, answer } => {
                self.record(
                    "approval.collected",
                    Some(&p.id),
                    meta.project.as_deref(),
                    meta.task.as_deref(),
                    None,
                    json!({"id": id, "allow": answer.allow}),
                );
                return Some(answer);
            }
            Asked::Gone => return None,
        };
        if new {
            self.record(
                "approval.requested",
                Some(&p.id),
                meta.project.as_deref(),
                meta.task.as_deref(),
                None,
                json!({"id": id, "kind": kind, "summary": summary, "pane_name": meta.name}),
            );
            self.notify(
                "approval",
                Some(&p.id),
                meta.project.as_deref(),
                &format!("{} needs approval", meta.name),
                summary,
            );
        } else {
            // The same approval, waited on again: nobody is told twice.
            self.record(
                "approval.reattached",
                Some(&p.id),
                meta.project.as_deref(),
                meta.task.as_deref(),
                None,
                json!({"id": id}),
            );
        }
        if let Some(task) = &meta.task {
            self.task_follow(task, "running", None);
            self.task_follow(task, "waiting_for_approval", Some(summary));
        }
        let answer = tokio::time::timeout(left, rx).await;
        if let Some(task) = &meta.task {
            self.task_follow(task, "running", None);
        }
        match answer {
            Ok(Ok(a)) => Some(a),
            _ => {
                // Its time ran out with this hook still the one waiting.
                // (Not when the hook was replaced by itself coming back,
                // or the pane's approvals were cancelled.)
                if self.approvals.finish(&id, waiter) {
                    self.approvals.expire(&id, now_ms());
                    self.emit(
                        "approval.resolved",
                        Some(&p.id),
                        None,
                        json!({"id": id, "status": "expired"}),
                    );
                }
                None
            }
        }
    }

    /// The owner's answer. The hook that waits is answered at once and
    /// never waits for the database. One whose hook is not connected at
    /// this moment (the station has just started again and the agent's
    /// hook is finding its way back) has the answer kept for it:
    /// `delivered` is then false.
    pub fn answer_approval(
        &self,
        id: &str,
        allow: bool,
        reason: Option<String>,
        by: &str,
    ) -> Result<Answered> {
        let status = if allow { "allowed" } else { "denied" };
        // A question or a plan is not allowed or denied: its answer is
        // made of its own choices (questions.rs).
        if self
            .approvals
            .kind_and_status(id)
            .is_some_and(|(kind, _)| crate::questions::is_held(&kind))
        {
            return Err(invalid(
                "that is a question or a plan, not an approval: answer it with question.answer",
            ));
        }
        let alive = |pane: &str| {
            self.pane(pane)
                .is_ok_and(|p| p.meta.lock().unwrap().exit.is_none())
        };
        let (pane, delivered) =
            match self
                .approvals
                .answer(id, allow, reason.clone(), by, now_ms(), alive)
            {
                Answer::Delivered { pane } => (pane, true),
                Answer::Recorded { pane } => (Some(pane), false),
                Answer::NotWaiting { expired } => {
                    // Still listed as waiting with nobody who can come back
                    // for it: it has left the list now, instead of staying
                    // there for ever.
                    if let Some(pane) = expired {
                        self.emit(
                            "approval.resolved",
                            Some(&pane),
                            None,
                            json!({"id": id, "status": "expired", "why": "restart"}),
                        );
                    }
                    return Err(invalid(
                    "that request is no longer waiting (answered, expired, or the agent moved on)",
                ));
                }
            };
        // The owner is told when it is not saved yet.
        let saved = self.approvals.saved(id);
        self.record(
            "approval.resolved",
            pane.as_deref(),
            None,
            None,
            Some(by),
            json!({"id": id, "status": status, "reason": reason, "saved": saved, "delivered": delivered}),
        );
        Ok(Answered { saved, delivered })
    }

    pub(crate) fn cancel_pane_approvals(&self, pane: &str) {
        for id in self.approvals.cancel_pane(pane, now_ms()) {
            self.emit(
                "approval.resolved",
                Some(pane),
                None,
                json!({"id": id, "status": "cancelled"}),
            );
        }
    }

    /// A while after a start: the approvals whose hook did not come back
    /// (its agent went with the daemon, or gave up) leave the list.
    pub(crate) fn expire_detached_approvals(&self) -> usize {
        let gone = self.approvals.expire_detached(now_ms());
        for (id, pane) in &gone {
            self.emit(
                "approval.resolved",
                Some(pane),
                None,
                json!({"id": id, "status": "expired", "why": "restart"}),
            );
        }
        gone.len()
    }

    pub fn approvals(&self, pending_only: bool) -> Vec<Value> {
        self.approvals.list(pending_only)
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[allow(dead_code)]
fn _meta_is_send(_: &PaneMeta) {}
