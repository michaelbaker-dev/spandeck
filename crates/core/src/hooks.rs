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
        // The file to follow for the transcript the hook named (for
        // Antigravity, the whole one: discover.rs). Worked out before the
        // pane's state is held.
        let named = ev.transcript_path.as_deref().and_then(|tp| {
            let was = p.meta();
            let same = was.session_id.as_deref() == ev.session_id.as_deref();
            self.transcript_named(
                ev.agent,
                ev.session_id.as_deref(),
                tp,
                was.transcript_path.as_deref().filter(|_| same),
            )
        });
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
                    // Another conversation of Antigravity's, whose
                    // transcript is not there yet: not the last one's.
                    if ev.agent == AgentKind::Antigravity && named.is_none() {
                        m.transcript_path = None;
                    }
                }
            }
            if let Some(tp) = &named {
                if m.transcript_path.as_deref() != Some(tp) {
                    m.transcript_path = Some(tp.clone());
                    session_changed = true;
                }
            }
            if ev.effort.is_some() {
                m.effort = ev.effort.clone();
                m.effort_at_ms = now;
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
            self.approvals_overtaken(p, None);
        }
        // A tool call that has run was allowed, here or in the terminal: a
        // permission request still waiting for that call is over.
        if ev.kind == EventKind::ToolEnd {
            self.approvals_overtaken(p, ev.tool.as_ref());
        }

        if session_changed {
            // What was found among its files counts for nothing now.
            self.forget_found(&p.id);
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
                // Claude Code asks permission for its own question and its
                // own plan at the moment it shows them in the terminal
                // (seen with 2.1.325 on 16 Nov 2026). That is not a thing
                // to allow or deny: its answer is one of its choices. So it
                // is never held as an approval, and the hook says nothing at
                // once: the prompt in the terminal is the only one. (Before,
                // it was listed as an approval for as long as it waited.)
                let asks = ev
                    .tool
                    .as_ref()
                    .and_then(|t| crate::questions::kind_of_tool(&t.name));
                if let Some(kind) = asks {
                    // The reason the station gave when it let the question
                    // go to the terminal says more: it stays.
                    let said = meta
                        .needs_reason
                        .as_deref()
                        .is_some_and(crate::questions::says_in_terminal);
                    if !(said && meta.state == AgentState::NeedsYou) {
                        self.set_state(
                            p,
                            AgentState::NeedsYou,
                            Some(crate::questions::in_terminal_words(kind).to_string()),
                        );
                    }
                    return HookDecision::none();
                }
                // A program started to review these for itself (Codex's
                // `--approve-for-me`) answers them itself: the hook says
                // nothing, and nothing waits for the owner. What its own
                // review still puts to the owner is shown in its terminal.
                // (Before, each one waited for the owner whatever the
                // program's mode: the real Codex, 18 Nov 2026.)
                if meta.own_review {
                    return HookDecision::none();
                }
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
                p.wake
                    .greeted_ms
                    .store(now_ms(), std::sync::atomic::Ordering::Relaxed);
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
                if carries_mail(meta.agent, &ev.raw_event, false) {
                    decision.context = self.mail_notice(&meta.id);
                }
            }
            EventKind::TurnEnd => {
                // Pause: no new work is delivered; the current turn finishes.
                // And a stopped turn is not continued with mail.
                if stuck.is_none() && carries_mail(meta.agent, &ev.raw_event, true) {
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
        } else if ev.kind == EventKind::Notification
            && ev.state == Some(AgentState::NeedsYou)
            && meta.state == AgentState::NeedsYou
            && meta
                .needs_reason
                .as_deref()
                .is_some_and(crate::questions::says_in_terminal)
        {
            // The agent's own notice that its prompt is up ("Claude needs
            // your permission") arrives after the pane already says which
            // prompt it is and why it is in the terminal: that says more.
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

    /// A permission request that still waits although what it was about
    /// has been settled without it: the owner answered the agent's own
    /// prompt in the terminal and the tool ran (`tool`), or the turn it
    /// belonged to is over (None: all of the pane's). It leaves the list.
    /// Found with the real Claude Code on 16 Nov 2026: such a request
    /// stayed listed as waiting until its five minutes were over.
    fn approvals_overtaken(&self, p: &PaneEntry, tool: Option<&spandeck_agents::ToolCall>) {
        if !self.approvals.any_waiting() {
            return;
        }
        let same = |r: &Value| {
            let asked = &r["detail"]["tool"];
            r["kind"] == "permission"
                && tool.is_none_or(|t| {
                    asked["name"].as_str() == Some(t.name.as_str())
                        && asked["command"].as_str() == t.command.as_deref()
                        && asked["file_path"].as_str() == t.file_path.as_deref()
                })
        };
        for id in self.approvals.cancel_where(&p.id, now_ms(), same) {
            self.emit(
                "approval.resolved",
                Some(&p.id),
                None,
                json!({"id": id, "status": "cancelled", "why": "settled in the terminal"}),
            );
        }
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

/// Does this program's answer to this hook carry a mail notice to its
/// agent? Asked of the program's own renderer, with a notice that says
/// nothing. A notice now gives the messages themselves and marks them
/// read (messages.rs, `mail_notice`), so one must never be made for an
/// answer that drops it: Claude Code's `PostToolUseFailure` is a tool's
/// end and its answer has no place for context; Antigravity takes context
/// at a prompt only. Mail a hook cannot carry waits for one that can, or
/// for the nudge.
fn carries_mail(agent: spandeck_agents::AgentKind, raw_event: &str, at_turn_end: bool) -> bool {
    const PROBE: &str = "spandeck-mail-probe";
    let mut decision = HookDecision::none();
    if at_turn_end {
        decision.continue_with = Some(PROBE.into());
    } else {
        decision.context = Some(PROBE.into());
    }
    spandeck_agents::adapter(agent)
        .is_some_and(|a| a.reply(raw_event, &decision).stdout.contains(PROBE))
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
