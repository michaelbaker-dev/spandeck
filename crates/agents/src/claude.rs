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
        "SubagentStart" => (
            EventKind::SubagentStart,
            None,
            None,
            s(p, "agent_type"),
            None,
        ),
        "SubagentStop" => (
            EventKind::SubagentStop,
            None,
            None,
            s(p, "agent_type"),
            None,
        ),
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
}
