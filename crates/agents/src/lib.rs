//! Agent adapters.
//!
//! One interface per agent, shaped by five verbs: observe, send work, enforce,
//! inject context, capture history. Hook-driven agents (Claude Code, Codex)
//! implement it through their hook payloads; Antigravity through its own
//! hook dialect; OpenCode, which has no hooks, through launch settings and
//! the screen. `catalogue` says how each program is installed and updated.
//!
//! This crate is pure: no daemon state, no I/O beyond config-file edits in
//! `install` and reading the agents' saved conversations in `sessions`. The
//! daemon feeds it vendor payloads and gets normalized events and
//! vendor-shaped replies back.

pub mod antigravity;
pub mod catalogue;
pub mod claude;
pub mod codex;
pub mod detect;
pub mod event;
pub mod install;
pub mod opencode;
pub mod sessions;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use event::{AgentEvent, AgentState, EventKind, ToolCall};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    /// A plain shell or any program spandeck does not understand.
    Shell,
    Claude,
    Codex,
    OpenCode,
    Antigravity,
    /// The scriptable test agent. Speaks Claude Code's hook dialect.
    Fake,
}

impl AgentKind {
    pub fn parse(s: &str) -> Option<AgentKind> {
        Some(match s {
            "shell" => AgentKind::Shell,
            "claude" | "claude-code" => AgentKind::Claude,
            "codex" => AgentKind::Codex,
            "opencode" => AgentKind::OpenCode,
            "antigravity" | "agy" => AgentKind::Antigravity,
            "fake" => AgentKind::Fake,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Shell => "shell",
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::OpenCode => "opencode",
            AgentKind::Antigravity => "antigravity",
            AgentKind::Fake => "fake",
        }
    }

    pub fn display(&self) -> &'static str {
        match self {
            AgentKind::Shell => "Shell",
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::OpenCode => "OpenCode",
            AgentKind::Antigravity => "Antigravity",
            AgentKind::Fake => "Fake agent",
        }
    }

    pub fn is_agent(&self) -> bool {
        !matches!(self, AgentKind::Shell)
    }
}

/// What the daemon wants a hook to tell the agent.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HookDecision {
    /// Block the tool call or deny the permission request, with a reason the
    /// agent sees.
    pub deny: Option<String>,
    /// Approve a permission request.
    pub allow: bool,
    /// Text added to the agent's context (digest, mail notice, orders).
    pub context: Option<String>,
    /// At turn end: keep the agent going with this instruction.
    pub continue_with: Option<String>,
}

impl HookDecision {
    pub fn none() -> Self {
        Self::default()
    }
    pub fn deny(reason: impl Into<String>) -> Self {
        HookDecision {
            deny: Some(reason.into()),
            ..Self::default()
        }
    }
    pub fn allow() -> Self {
        HookDecision {
            allow: true,
            ..Self::default()
        }
    }
    pub fn context(text: impl Into<String>) -> Self {
        HookDecision {
            context: Some(text.into()),
            ..Self::default()
        }
    }
    pub fn is_none(&self) -> bool {
        *self == Self::default()
    }
}

/// What the `spandeck hook` process must print and exit with.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HookReply {
    pub stdout: String,
    pub exit_code: i32,
}

/// How much an agent's integration can do. This is the enforcement table:
/// a rule is shown as enforced only where `block_tools` is true and verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Lifecycle hooks report state (otherwise screen detection only).
    pub hooks: bool,
    /// A hook can block a tool call before it runs.
    pub block_tools: bool,
    /// A hook can answer a permission prompt from elsewhere.
    pub remote_approval: bool,
    /// A hook can add text to the agent's context.
    pub inject_context: bool,
    /// The conversation can be resumed by session id.
    pub resume: bool,
    /// Checked against the real agent on this build (not only against docs).
    pub verified: bool,
    /// Plain-words note shown beside the table.
    pub note: &'static str,
}

/// How to launch or resume an agent in a pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchOpts {
    /// Resume this vendor session instead of starting fresh.
    pub resume: Option<String>,
    /// First prompt (a delegated task's brief).
    pub prompt: Option<String>,
    /// Extra arguments from the project's launcher config.
    pub extra_args: Vec<String>,
    /// Override the program (config or tests).
    pub program: Option<String>,
    /// The id to give a new conversation, for an agent that lets the caller
    /// choose it (see `Adapter::session_prefix`).
    pub session: Option<String>,
}

pub trait Adapter: Send + Sync {
    fn kind(&self) -> AgentKind;
    fn capabilities(&self) -> Capabilities;

    /// Observe: vendor hook payload to the one normalized event.
    fn normalize(&self, payload: &Value) -> Option<AgentEvent>;

    /// Enforce / inject: the daemon's decision in the vendor's reply format.
    fn reply(&self, raw_event: &str, decision: &HookDecision) -> HookReply;

    /// Send work: how to start or resume the agent.
    fn launch(&self, opts: &LaunchOpts) -> Launch;

    /// Bytes that interrupt the current turn without ending the process.
    fn interrupt_keys(&self) -> &'static [u8];

    /// The instructions file this agent reads from the working tree.
    fn instructions_file(&self) -> &'static str;

    /// Observe (fallback): guess state from the visible screen.
    fn detect_screen(&self, lines: &[String]) -> Option<AgentState>;

    /// For an agent with no hooks to report its session id, but which starts
    /// a conversation under an id the caller chooses: what such an id starts
    /// with. The daemon makes one up at launch and resumes by it later.
    fn session_prefix(&self) -> Option<&'static str> {
        None
    }
}

/// Is this station a Windows machine? Plain code, not a `cfg`: what differs
/// there (install commands, a hook's command line) is chosen where it is
/// used, and both forms are compiled and tested on every system.
pub fn on_windows() -> bool {
    std::env::consts::OS == "windows"
}

pub fn adapter(kind: AgentKind) -> Option<&'static dyn Adapter> {
    match kind {
        AgentKind::Claude => Some(&claude::ClaudeCode),
        AgentKind::Codex => Some(&codex::Codex),
        AgentKind::OpenCode => Some(&opencode::OpenCode),
        AgentKind::Antigravity => Some(&antigravity::Antigravity),
        AgentKind::Fake => Some(&claude::FakeAgent),
        AgentKind::Shell => None,
    }
}

/// Guess the agent from a command line, so `claude` typed into a shell pane
/// or a launcher with a custom path is still recognised.
pub fn kind_from_command(program: &str) -> AgentKind {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
    // Windows: `claude.exe`, or npm's `claude.cmd`.
    let lower = base.to_ascii_lowercase();
    let base = [".exe", ".cmd", ".bat"]
        .iter()
        .find_map(|ext| lower.strip_suffix(ext).map(|stem| &base[..stem.len()]))
        .unwrap_or(base);
    match base {
        "claude" => AgentKind::Claude,
        "codex" => AgentKind::Codex,
        "opencode" => AgentKind::OpenCode,
        "agy" => AgentKind::Antigravity,
        b if b.starts_with("spandeck-fake-agent") => AgentKind::Fake,
        _ => AgentKind::Shell,
    }
}

/// Which picture stands for a pane. Agents spandeck has an adapter for are
/// known by their kind; any other program by its name, since a few agents
/// (Gemini, Grok) run in a pane with no adapter yet.
pub fn icon(kind: AgentKind, program: &str) -> &'static str {
    match kind {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
        AgentKind::OpenCode => "opencode",
        AgentKind::Antigravity => "antigravity",
        AgentKind::Fake => "agent",
        AgentKind::Shell => match program.rsplit('/').next().unwrap_or(program) {
            "gemini" => "gemini",
            "grok" => "grok",
            "opencode" => "opencode",
            "agy" => "antigravity",
            _ => "shell",
        },
    }
}

/// The enforcement table (Phase 7), one row per agent kind.
pub fn enforcement_table() -> Vec<(AgentKind, Capabilities)> {
    [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::OpenCode,
        AgentKind::Antigravity,
    ]
    .into_iter()
    .filter_map(|k| adapter(k).map(|a| (k, a.capabilities())))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Control Deck draws a pane's icon from this (the pane view's
    /// `icon`); its eight values are the contract with the deck.
    #[test]
    fn a_pane_icon_comes_from_the_agent_kind_or_else_the_program_name() {
        for (kind, program, want) in [
            (AgentKind::Claude, "/opt/wrappers/my-launcher", "claude"),
            (AgentKind::Codex, "/bin/sh", "codex"),
            (AgentKind::Fake, "spandeck-fake-agent", "agent"),
            (AgentKind::Shell, "/usr/local/bin/gemini", "gemini"),
            (AgentKind::Shell, "grok", "grok"),
            (
                AgentKind::Shell,
                "/home/me/.opencode/bin/opencode",
                "opencode",
            ),
            (AgentKind::OpenCode, "/bin/sh", "opencode"),
            (
                AgentKind::Antigravity,
                "/home/me/.local/bin/agy",
                "antigravity",
            ),
            (AgentKind::Shell, "agy", "antigravity"),
            (AgentKind::Shell, "/bin/zsh", "shell"),
            (AgentKind::Shell, "gemini-wrapper", "shell"),
        ] {
            assert_eq!(icon(kind, program), want, "{kind:?} {program}");
        }
    }
}
