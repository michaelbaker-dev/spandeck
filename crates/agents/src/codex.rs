//! Codex adapter.
//!
//! UNVERIFIED on this build: Codex is not installed on the build station.
//! Shapes follow the published hooks reference (fetched 13 Nov 2026), which
//! mirrors Claude Code's dialect and adds `model`, `turn_id` and `Interrupt`.
//! `capabilities().verified` stays false until a live canary passes, and the
//! Control Deck shows Codex rules as guidance, not enforced, until then.
//!
//! Known caveats from the reference:
//! - Hooks must be reviewed and trusted in Codex (`/hooks`) before they run.
//! - PreToolUse covers shell, apply_patch and MCP tools, not hosted tools; the
//!   reference calls hooks "a guardrail, not a complete enforcement boundary".
//! - herdr 0.9.5 stopped installing Codex turn hooks because of a startup
//!   warning, so screen detection must stay a working fallback.

use serde_json::Value;

use crate::claude::{normalize_claude_dialect, reply_claude_dialect};
use crate::detect;
use crate::event::{AgentEvent, AgentState};
use crate::{Adapter, AgentKind, Capabilities, HookDecision, HookReply, Launch, LaunchOpts};

pub struct Codex;

impl Adapter for Codex {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            hooks: true,
            block_tools: true,
            remote_approval: true,
            inject_context: true,
            resume: true,
            verified: false,
            note: "From the Codex hooks reference, not yet run against a real Codex. Hooks need to be trusted in Codex first and do not cover hosted tools.",
        }
    }

    fn normalize(&self, payload: &Value) -> Option<AgentEvent> {
        normalize_claude_dialect(AgentKind::Codex, payload)
    }

    fn reply(&self, raw_event: &str, decision: &HookDecision) -> HookReply {
        // Codex fails a PreToolUse hook that returns anything but deny/context,
        // and the shared renderer only ever emits those.
        reply_claude_dialect(raw_event, decision)
    }

    fn launch(&self, opts: &LaunchOpts) -> Launch {
        let mut args = Vec::new();
        if let Some(id) = &opts.resume {
            args.push("resume".to_string());
            args.push(id.clone());
        }
        args.extend(opts.extra_args.iter().cloned());
        if let Some(prompt) = &opts.prompt {
            args.push(prompt.clone());
        }
        Launch {
            program: opts.program.clone().unwrap_or_else(|| "codex".into()),
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
        detect::detect(detect::CODEX_RULES, lines)
    }
}
