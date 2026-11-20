//! Claude Code adapter. Payload shapes verified against Claude Code 2.1.323
//! (fixtures/claude-code/2.1.323, captured from a real run).

use serde_json::{json, Value};

use crate::detect;
use crate::event::{cap, AgentEvent, AgentState, EventKind, ToolCall};
use crate::{Adapter, AgentKind, Capabilities, HookDecision, HookReply, Launch, LaunchOpts};

pub struct ClaudeCode;

/// The test agent: Claude Code's hook dialect, a different launcher.
pub struct FakeAgent;

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|x| x.to_string())
}

/// An id a program gave that may name a file or a row: letters, digits,
/// `-` and `_`, and not long.
pub fn plain_word(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Shared by every agent that speaks the Claude-style hook dialect
/// (Claude Code, Codex, the fake agent).
pub(crate) fn normalize_claude_dialect(agent: AgentKind, p: &Value) -> Option<AgentEvent> {
    let raw = s(p, "hook_event_name")?;
    let tool = || -> Option<ToolCall> {
        let name = s(p, "tool_name")?;
        let input = p.get("tool_input").cloned().unwrap_or(Value::Null);
        let command = match input.get("command") {
            Some(Value::String(c)) => Some(c.clone()),
            // Codex's shell tool passes argv.
            Some(Value::Array(a)) => Some(
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        };
        let file_path = s(&input, "file_path").or_else(|| s(&input, "path"));
        let writes = matches!(
            name.as_str(),
            "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "apply_patch"
        );
        Some(ToolCall {
            name,
            id: s(p, "tool_use_id"),
            command,
            file_path,
            writes,
            ok: None,
            error: None,
            duration_ms: p.get("duration_ms").and_then(|d| d.as_u64()),
        })
    };

    let (kind, state, tool, message, source) = match raw.as_str() {
        "SessionStart" => (
            EventKind::SessionStart,
            Some(AgentState::Idle),
            None,
            None,
            s(p, "source"),
        ),
        "UserPromptSubmit" => (
            EventKind::PromptSubmit,
            Some(AgentState::Working),
            None,
            s(p, "prompt").map(|m| cap(&m, 200)),
            None,
        ),
        "PreToolUse" => (
            EventKind::ToolStart,
            Some(AgentState::Working),
            tool(),
            None,
            None,
        ),
        "PostToolUse" => {
            let mut t = tool();
            if let Some(t) = t.as_mut() {
                // Codex reports a failed command as a normal PostToolUse.
                let exit = p
                    .get("tool_response")
                    .and_then(|r| r.get("exit_code").or_else(|| r.get("exitCode")))
                    .and_then(|c| c.as_i64());
                t.ok = Some(exit.map(|c| c == 0).unwrap_or(true));
                if let Some(c) = exit.filter(|c| *c != 0) {
                    t.error = Some(format!("Exit code {c}"));
                }
            }
            (EventKind::ToolEnd, Some(AgentState::Working), t, None, None)
        }
        // The user interrupted the turn mid-tool. Claude Code fires no Stop
        // hook for an interrupt (seen live on 2.1.323), so this is the signal.
        "PostToolUseFailure" if p.get("is_interrupt").and_then(|i| i.as_bool()) == Some(true) => (
            EventKind::Interrupted,
            Some(AgentState::Idle),
            tool(),
            Some("interrupted".to_string()),
            None,
        ),
        "PostToolUseFailure" => {
            let mut t = tool();
            if let Some(t) = t.as_mut() {
                t.ok = Some(false);
                t.error = s(p, "error").map(|e| cap(&e, 200));
            }
            (EventKind::ToolEnd, Some(AgentState::Working), t, None, None)
        }
        "PermissionRequest" => (
            EventKind::PermissionRequest,
            Some(AgentState::NeedsYou),
            tool(),
            None,
            None,
        ),
        "Notification" => {
            let ty = s(p, "notification_type").unwrap_or_default();
            // "idle_prompt" only says the agent has been sitting at its prompt
            // for a while. That is an agent with nothing to do, not one that
            // needs the user: it was shown as "needs you" and it is not.
            let needs = matches!(
                ty.as_str(),
                "permission_prompt" | "agent_needs_input" | "elicitation_dialog"
            );
            (
                EventKind::Notification,
                needs.then_some(AgentState::NeedsYou),
                None,
                s(p, "message").map(|m| cap(&m, 200)),
                Some(ty).filter(|t| !t.is_empty()),
            )
        }
        "Stop" => (
            EventKind::TurnEnd,
            Some(AgentState::Done),
            None,
            s(p, "last_assistant_message").map(|m| cap(&m, 200)),
            None,
        ),
        "StopFailure" => (
            EventKind::TurnFailed,
            Some(AgentState::Error),
            None,
            s(p, "error")
                .or_else(|| s(p, "error_details"))
                .map(|m| cap(&m, 200)),
            None,
        ),
        "Interrupt" => (
            EventKind::Interrupted,
            Some(AgentState::Idle),
            None,
            None,
            None,
        ),
        // A sub-agent starting or stopping says nothing about the main agent's
        // turn. Claude Code also runs helpers of its own a few seconds after a
        // turn ends, and their events used to flip a finished pane back to
        // "working", where it stayed until the next prompt.
        //
        // The program also reports helpers of its own this way, with an
        // empty `agent_type` (Claude Code 2.1.329): they are no sub-agent
        // the owner or the agent asked for, and are not events here. A
        // payload with no `agent_type` at all is an older program's.
        "SubagentStart" | "SubagentStop" => {
            if s(p, "agent_type").is_some_and(|t| t.trim().is_empty()) {
                return None;
            }
            (
                if raw == "SubagentStart" {
                    EventKind::SubagentStart
                } else {
                    EventKind::SubagentStop
                },
                None,
                None,
                s(p, "agent_type").map(|t| cap(&t, 80)),
                None,
            )
        }
        "PreCompact" | "PostCompact" => (
            EventKind::Compact,
            None,
            None,
            None,
            s(p, "trigger").or(Some(raw.clone())),
        ),
        "SessionEnd" => (
            EventKind::SessionEnd,
            Some(AgentState::Idle),
            None,
            None,
            s(p, "reason"),
        ),
        _ => return None,
    };

    let sub = matches!(kind, EventKind::SubagentStart | EventKind::SubagentStop);
    Some(AgentEvent {
        kind,
        agent,
        session_id: s(p, "session_id"),
        transcript_path: s(p, "transcript_path"),
        cwd: s(p, "cwd"),
        state,
        tool,
        message,
        source,
        // Claude Code's hook reference (read 16 Nov 2026): the events of a
        // tool call and a turn's end carry `effort`, an object with `level`.
        effort: p
            .pointer("/effort/level")
            .and_then(|l| l.as_str())
            .filter(|l| l.len() <= 16 && l.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(String::from),
        // A sub-agent's own id and where its record is (Claude Code's and
        // Codex's `SubagentStart`/`SubagentStop`: `agent_id`,
        // `agent_transcript_path`). The id is used only as a plain word.
        sub_id: sub
            .then(|| s(p, "agent_id"))
            .flatten()
            .filter(|id| plain_word(id)),
        sub_transcript: sub
            .then(|| s(p, "agent_transcript_path"))
            .flatten()
            .filter(|t| !t.is_empty() && t.len() <= 1024),
        raw_event: raw,
    })
}

/// Reply format shared by the Claude-style dialect. Verified on Claude Code
/// 2.1.323: PreToolUse deny, PermissionRequest allow, SessionStart context.
pub(crate) fn reply_claude_dialect(raw_event: &str, d: &HookDecision) -> HookReply {
    if d.is_none() {
        return HookReply::default();
    }
    let out = match raw_event {
        "PreToolUse" => {
            if let Some(reason) = &d.deny {
                json!({"hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }})
            } else if let (true, Some(input)) = (d.allow, &d.updated_input) {
                // The owner's answer to a question or a plan, given from
                // the Control Deck. Claude Code's hook reference, "Tools
                // that require user interaction" (read 16 Nov 2026):
                // allow alone is not enough for these; the answer goes
                // back as the call's whole input. Seen to work with
                // Claude Code 2.1.325 (16 Nov 2026, by hand).
                json!({"hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "updatedInput": input,
                }})
            } else if let Some(ctx) = &d.context {
                json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": ctx}})
            } else {
                return HookReply::default();
            }
        }
        "PermissionRequest" => {
            if let Some(reason) = &d.deny {
                json!({"hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {"behavior": "deny", "message": reason},
                }})
            } else if d.allow {
                json!({"hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {"behavior": "allow"},
                }})
            } else {
                // No decision: the agent shows its own prompt in the terminal.
                return HookReply::default();
            }
        }
        "Stop" | "SubagentStop" => match &d.continue_with {
            Some(reason) => json!({"decision": "block", "reason": reason}),
            None => return HookReply::default(),
        },
        "UserPromptSubmit" if d.deny.is_some() => {
            json!({"decision": "block", "reason": d.deny})
        }
        "SessionStart" | "UserPromptSubmit" | "PostToolUse" => match &d.context {
            Some(ctx) => {
                json!({"hookSpecificOutput": {"hookEventName": raw_event, "additionalContext": ctx}})
            }
            None => return HookReply::default(),
        },
        _ => return HookReply::default(),
    };
    HookReply {
        stdout: out.to_string(),
        exit_code: 0,
    }
}

impl Adapter for ClaudeCode {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            hooks: true,
            block_tools: true,
            remote_approval: true,
            inject_context: true,
            resume: true,
            answer_questions: true,
            verified: true,
            note: "Verified live against Claude Code 2.1.323: a hook blocks any tool call, answers permission prompts and adds context.",
        }
    }

    fn normalize(&self, payload: &Value) -> Option<AgentEvent> {
        normalize_claude_dialect(AgentKind::Claude, payload)
    }

    fn reply(&self, raw_event: &str, decision: &HookDecision) -> HookReply {
        reply_claude_dialect(raw_event, decision)
    }

    fn launch(&self, opts: &LaunchOpts) -> Launch {
        let mut args = Vec::new();
        if let Some(id) = &opts.resume {
            args.push("--resume".to_string());
            args.push(id.clone());
        }
        args.extend(opts.extra_args.iter().cloned());
        if let Some(prompt) = &opts.prompt {
            // `--mcp-config` and `--allowedTools` take several values and would
            // swallow the prompt without the separator (found by the live canary).
            args.push("--".to_string());
            args.push(prompt.clone());
        }
        Launch {
            program: opts.program.clone().unwrap_or_else(|| "claude".into()),
            args,
        }
    }

    fn interrupt_keys(&self) -> &'static [u8] {
        b"\x1b"
    }

    fn instructions_file(&self) -> &'static str {
        "CLAUDE.md"
    }

    fn detect_screen(&self, lines: &[String]) -> Option<AgentState> {
        detect::detect(detect::CLAUDE_RULES, lines)
    }
}

impl Adapter for FakeAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Fake
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            hooks: true,
            block_tools: true,
            remote_approval: true,
            inject_context: true,
            resume: true,
            answer_questions: true,
            verified: true,
            note: "Test agent.",
        }
    }

    fn normalize(&self, payload: &Value) -> Option<AgentEvent> {
        normalize_claude_dialect(AgentKind::Fake, payload)
    }

    fn reply(&self, raw_event: &str, decision: &HookDecision) -> HookReply {
        reply_claude_dialect(raw_event, decision)
    }

    fn launch(&self, opts: &LaunchOpts) -> Launch {
        let mut args = Vec::new();
        if let Some(id) = &opts.resume {
            args.push("--resume".to_string());
            args.push(id.clone());
        }
        args.extend(opts.extra_args.iter().cloned());
        if let Some(prompt) = &opts.prompt {
            args.push("--prompt".to_string());
            args.push(prompt.clone());
        }
        Launch {
            program: opts
                .program
                .clone()
                .unwrap_or_else(|| "spandeck-fake-agent".into()),
            args,
        }
    }

    fn interrupt_keys(&self) -> &'static [u8] {
        b"\x1b"
    }

    fn instructions_file(&self) -> &'static str {
        "CLAUDE.md"
    }

    fn detect_screen(&self, lines: &[String]) -> Option<AgentState> {
        detect::detect(detect::FAKE_RULES, lines)
    }

    fn detect_screen_why(&self, lines: &[String]) -> Option<(AgentState, Option<String>)> {
        detect::detect_why(detect::FAKE_RULES, lines)
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;
    use serde_json::json;

    // A bug the owner reported: a Claude pane waiting at its prompt was shown
    // as needing attention.
    #[test]
    fn sitting_at_the_prompt_is_not_needing_the_user() {
        let state = |ty: &str| {
            normalize_claude_dialect(
                AgentKind::Claude,
                &json!({"hook_event_name": "Notification", "session_id": "s", "notification_type": ty, "message": "m"}),
            )
            .unwrap()
            .state
        };
        assert_eq!(state("idle_prompt"), None);
        assert_eq!(state("permission_prompt"), Some(AgentState::NeedsYou));
        assert_eq!(state("elicitation_dialog"), Some(AgentState::NeedsYou));
    }

    // A bug the owner reported: a pane showed "working · 10m" while Claude sat
    // at its prompt. The log showed "done", then "working" six seconds later
    // with no prompt and no tool call in between.
    #[test]
    fn a_sub_agent_event_does_not_restart_a_finished_turn() {
        for name in ["SubagentStart", "SubagentStop"] {
            let ev = normalize_claude_dialect(
                AgentKind::Claude,
                &json!({"hook_event_name": name, "session_id": "s", "agent_type": "helper"}),
            )
            .unwrap();
            assert_eq!(ev.state, None, "{name}");
        }
    }

    // The owner's request of 20 Nov 2026: a sub-agent is a row of its own,
    // so its start and its stop are told apart by the program's own id.
    // Payloads in the shapes Claude Code 2.1.329 and codex-cli 0.170.0 send.
    #[test]
    fn a_sub_agent_event_carries_its_id_and_an_internal_one_is_no_event() {
        let ev = |agent, v: Value| normalize_claude_dialect(agent, &v);
        let stop = ev(
            AgentKind::Claude,
            json!({"hook_event_name": "SubagentStop", "session_id": "s", "agent_id": "a3f9c1d2e4b5a6c70",
                "agent_type": "Explore", "agent_transcript_path": "/h/.claude/projects/p/s/subagents/agent-a3f9c1d2e4b5a6c70.jsonl",
                "last_assistant_message": "Two scripts."}),
        )
        .unwrap();
        assert_eq!(
            (stop.kind, stop.message.as_deref(), stop.sub_id.as_deref()),
            (
                EventKind::SubagentStop,
                Some("Explore"),
                Some("a3f9c1d2e4b5a6c70")
            )
        );
        assert!(stop
            .sub_transcript
            .unwrap()
            .ends_with("agent-a3f9c1d2e4b5a6c70.jsonl"));
        let start = ev(
            AgentKind::Codex,
            json!({"hook_event_name": "SubagentStart", "session_id": "s", "turn_id": "t1",
                "agent_id": "019ab3c4-0000-7000-8000-00000000c0de", "agent_type": "worker"}),
        )
        .unwrap();
        assert_eq!(
            start.sub_id.as_deref(),
            Some("019ab3c4-0000-7000-8000-00000000c0de")
        );
        // An id that could name another file is not one.
        let odd = ev(
            AgentKind::Claude,
            json!({"hook_event_name": "SubagentStart", "agent_id": "../../x", "agent_type": "Explore"}),
        )
        .unwrap();
        assert_eq!(odd.sub_id, None);
        // The program's own helper: an empty type.
        for name in ["SubagentStart", "SubagentStop"] {
            assert!(ev(
                AgentKind::Claude,
                json!({"hook_event_name": name, "agent_id": "a1", "agent_type": ""})
            )
            .is_none());
        }
        // An older program: no type and no id.
        let old = ev(
            AgentKind::Claude,
            json!({"hook_event_name": "SubagentStart"}),
        )
        .unwrap();
        assert_eq!((old.message, old.sub_id), (None, None));
        // No other event has them.
        let end = ev(
            AgentKind::Claude,
            json!({"hook_event_name": "Stop", "agent_id": "a1", "agent_transcript_path": "/x"}),
        )
        .unwrap();
        assert_eq!((end.sub_id, end.sub_transcript), (None, None));
    }
}
