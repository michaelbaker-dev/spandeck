//! OpenCode adapter. Checked against OpenCode 2.3.0's own help and config
//! loader on the build station (14 Nov 2026); never run through a turn.
//!
//! What OpenCode gives spandeck:
//! - `--session <id>`: "Session ID to continue, or to create if it does not
//!   exist". The id must start with `ses`. So spandeck chooses the id when it
//!   starts the pane and resumes by the same id; no hook is needed to learn it.
//! - Configuration for one process through the environment
//!   (`OPENCODE_CONFIG_CONTENT`), read only by a private server
//!   (`--standalone`): the background service ignores the caller's
//!   environment. The daemon passes spandeck's MCP server and skills that way.
//!
//! What it does not give: lifecycle hooks a command can answer. State comes
//! from the screen, nothing can be blocked, and approvals stay in the pane.

use serde_json::Value;

use crate::detect;
use crate::event::{AgentEvent, AgentState};
use crate::{Adapter, AgentKind, Capabilities, HookDecision, HookReply, Launch, LaunchOpts};

pub struct OpenCode;

impl Adapter for OpenCode {
    fn kind(&self) -> AgentKind {
        AgentKind::OpenCode
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            hooks: false,
            block_tools: false,
            remote_approval: false,
            inject_context: false,
            resume: true,
            verified: false,
            note: "No hooks: state is read from the screen, rules are guidance, and approvals are answered in the pane. spandeck's tools and skills are attached at launch.",
        }
    }

    fn normalize(&self, _payload: &Value) -> Option<AgentEvent> {
        None
    }

    fn reply(&self, _raw_event: &str, _decision: &HookDecision) -> HookReply {
        HookReply::default()
    }

    fn launch(&self, opts: &LaunchOpts) -> Launch {
        let mut args = Vec::new();
        if let Some(id) = opts.resume.as_ref().or(opts.session.as_ref()) {
            args.push("--session".to_string());
            args.push(id.clone());
        }
        args.extend(opts.extra_args.iter().cloned());
        if let Some(prompt) = &opts.prompt {
            args.push("--prompt".to_string());
            args.push(prompt.clone());
        }
        Launch {
            program: opts.program.clone().unwrap_or_else(|| "opencode".into()),
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
        detect::detect(detect::OPENCODE_RULES, lines)
    }

    fn session_prefix(&self) -> Option<&'static str> {
        Some("ses_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner asked that an updated agent comes back to its conversation.
    /// OpenCode reports no session id, so the pane must be started under one
    /// spandeck chose and resumed with the same flag.
    #[test]
    fn a_new_pane_and_a_resumed_one_name_the_session() {
        let fresh = OpenCode.launch(&LaunchOpts {
            session: Some("ses_abc".into()),
            extra_args: vec!["--standalone".into()],
            ..Default::default()
        });
        assert_eq!(fresh.args, ["--session", "ses_abc", "--standalone"]);
        let resumed = OpenCode.launch(&LaunchOpts {
            resume: Some("ses_old".into()),
            session: Some("ses_new".into()),
            ..Default::default()
        });
        assert_eq!(resumed.args, ["--session", "ses_old"]);
    }
}
