//! Antigravity CLI (`agy`) adapter.
//!
//! Flags are from `agy --help` on 1.5.9 (14 Nov 2026). The hook shapes are
//! from the hooks guide that ships inside the CLI
//! (`builtin/skills/agy-customizations/docs/hooks.md`); they have NOT been
//! seen from a live, signed-in session, so `verified` stays false.
//!
//! Antigravity's hooks are not Claude Code's dialect: keys are camelCase
//! (`hookEventName`, `conversationId`), the events are SessionStart,
//! PreInvocation, PostInvocation, PreToolUse, PostToolUse and Stop, and the
//! replies differ. spandeck hooks three of them:
//! - SessionStart and PreInvocation: the conversation id, "working", and a
//!   place to add context (`injectSteps`).
//! - Stop: the turn ended, and a place to keep the agent going
//!   (`decision: "continue"`).
//!
//! PreToolUse is deliberately not hooked. Its reply must carry a decision
//! (allow, deny, ask, force_ask) and the guide documents no "no opinion"
//! answer, so hooking it would change what the user is asked. That means
//! spandeck cannot block an Antigravity tool call or answer its approvals.
//!
//! State also arrives over OSC 7501, which the CLI emits when
//! `AGY_CLI_OSC7501=on` (seen live: `state=blocked:kind=auth:…`).

use serde_json::{json, Value};

use crate::detect;
use crate::event::{cap, AgentEvent, AgentState, EventKind};
use crate::{Adapter, AgentKind, Capabilities, HookDecision, HookReply, Launch, LaunchOpts};

pub struct Antigravity;

/// The hooks spandeck asks Antigravity to run, as `hooks.json` wants them:
/// one named hook, each event a flat list of handlers.
pub fn hooks_json(bin: &str) -> Value {
    let command = format!("'{}' hook antigravity", bin.replace('\'', "'\\''"));
    let handler = json!([{"type": "command", "command": command, "timeout": 10}]);
    json!({"spandeck": {
        "SessionStart": handler,
        "PreInvocation": handler,
        "Stop": handler,
    }})
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|x| x.to_string())
}

impl Adapter for Antigravity {
    fn kind(&self) -> AgentKind {
        AgentKind::Antigravity
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            hooks: true,
            block_tools: false,
            remote_approval: false,
            inject_context: true,
            resume: true,
            answer_questions: false,
            verified: false,
            note: "From the hooks guide shipped with the CLI, not yet seen from a live session. Tool calls are not hooked, so rules are guidance and approvals are answered in the pane.",
        }
    }

    fn normalize(&self, p: &Value) -> Option<AgentEvent> {
        let raw = s(p, "hookEventName")?;
        let (kind, state, message) = match raw.as_str() {
            "SessionStart" => (EventKind::SessionStart, Some(AgentState::Idle), None),
            "PreInvocation" => (
                EventKind::PromptSubmit,
                Some(AgentState::Working),
                s(p, "lastUserInput").map(|m| cap(&m, 200)),
            ),
            "Stop" => match s(p, "terminationReason").as_deref() {
                Some("ERROR") => (
                    EventKind::TurnFailed,
                    Some(AgentState::Error),
                    s(p, "error")
                        .filter(|e| !e.is_empty())
                        .map(|e| cap(&e, 200)),
                ),
                Some("USER_CANCELED") => (
                    EventKind::Interrupted,
                    Some(AgentState::Idle),
                    Some("interrupted".to_string()),
                ),
                _ => (EventKind::TurnEnd, Some(AgentState::Done), None),
            },
            _ => return None,
        };
        Some(AgentEvent {
            kind,
            agent: AgentKind::Antigravity,
            session_id: s(p, "conversationId"),
            // The conversation's transcript, as JSON lines (the archive
            // reads it: `spandeck_archive::stores`).
            transcript_path: s(p, "transcriptPath"),
            cwd: s(p, "cwd"),
            state,
            tool: None,
            message,
            source: None,
            effort: None,
            sub_id: None,
            sub_transcript: None,
            raw_event: raw,
        })
    }

    fn reply(&self, raw_event: &str, d: &HookDecision) -> HookReply {
        let out = match raw_event {
            "SessionStart" | "PreInvocation" => match &d.context {
                Some(text) => {
                    json!({"injectSteps": [{"systemMessage": {"systemMessage": text}}]})
                }
                None => json!({}),
            },
            "Stop" => match &d.continue_with {
                Some(text) => json!({"decision": "continue", "reason": text}),
                None => json!({}),
            },
            _ => json!({}),
        };
        HookReply {
            stdout: out.to_string(),
            exit_code: 0,
        }
    }

    fn launch(&self, opts: &LaunchOpts) -> Launch {
        let mut args = Vec::new();
        if let Some(id) = &opts.resume {
            args.push("--conversation".to_string());
            args.push(id.clone());
        }
        args.extend(opts.extra_args.iter().cloned());
        if let Some(prompt) = &opts.prompt {
            args.push("--prompt-interactive".to_string());
            args.push(prompt.clone());
        }
        Launch {
            program: opts.program.clone().unwrap_or_else(|| "agy".into()),
            args,
        }
    }

    fn interrupt_keys(&self) -> &'static [u8] {
        b"\x1b"
    }

    fn instructions_file(&self) -> &'static str {
        "AGENTS.md"
    }

    fn detect_screen(&self, lines: &[String]) -> Option<AgentState> {
        detect::detect(detect::ANTIGRAVITY_RULES, lines)
    }

    fn detect_screen_why(&self, lines: &[String]) -> Option<(AgentState, Option<String>)> {
        detect::detect_why(detect::ANTIGRAVITY_RULES, lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resuming after an update needs the conversation id, and Antigravity's
    /// hooks name it `conversationId`, not `session_id`. The payloads are the
    /// examples from the CLI's own hooks guide.
    #[test]
    fn the_documented_payloads_give_the_conversation_and_the_turn() {
        let common = json!({
            "conversationId": "ec33ebf9-0cba-4100-8142-c61503f6c587",
            "workspacePaths": ["/w"], "modelName": "auto",
            "transcriptPath": "/w/.gemini/antigravity-cli/transcript.jsonl",
            "lastUserInput": "Fix the failing unit test", "cwd": "/w",
        });
        let with = |extra: Value| {
            let mut p = common.clone();
            for (k, v) in extra.as_object().unwrap() {
                p[k] = v.clone();
            }
            Antigravity.normalize(&p)
        };
        let start = with(json!({"hookEventName": "SessionStart"})).unwrap();
        assert_eq!(start.kind, EventKind::SessionStart);
        assert_eq!(
            start.session_id.as_deref(),
            Some("ec33ebf9-0cba-4100-8142-c61503f6c587")
        );
        assert_eq!(
            start.transcript_path.as_deref(),
            Some("/w/.gemini/antigravity-cli/transcript.jsonl")
        );
        let pre = with(json!({"hookEventName": "PreInvocation", "invocationNum": 3})).unwrap();
        assert_eq!(pre.state, Some(AgentState::Working));
        let done = with(json!({"hookEventName": "Stop", "terminationReason": "NO_TOOL_CALL"}));
        assert_eq!(done.unwrap().state, Some(AgentState::Done));
        let failed =
            with(json!({"hookEventName": "Stop", "terminationReason": "ERROR", "error": "boom"}));
        assert_eq!(failed.unwrap().kind, EventKind::TurnFailed);
        // Tool events are not hooked, and are ignored if they arrive anyway.
        assert!(with(json!({"hookEventName": "PreToolUse"})).is_none());
    }

    /// Mail reaches an agent as context at a prompt, or by keeping its turn
    /// going at the end: each needs Antigravity's own reply shape.
    #[test]
    fn replies_use_antigravitys_shapes() {
        let ctx = Antigravity.reply("PreInvocation", &HookDecision::context("you have mail"));
        assert_eq!(
            serde_json::from_str::<Value>(&ctx.stdout).unwrap(),
            json!({"injectSteps": [{"systemMessage": {"systemMessage": "you have mail"}}]})
        );
        let go = Antigravity.reply(
            "Stop",
            &HookDecision {
                continue_with: Some("read your inbox".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            serde_json::from_str::<Value>(&go.stdout).unwrap(),
            json!({"decision": "continue", "reason": "read your inbox"})
        );
        assert_eq!(
            Antigravity.reply("Stop", &HookDecision::none()).stdout,
            "{}"
        );
    }
}
