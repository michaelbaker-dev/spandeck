//! The one normalized agent event. Every vendor hook maps to this.

use serde::{Deserialize, Serialize};

use crate::AgentKind;

/// What the Control Deck shows for a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Nothing running: a shell, or an agent that has not been given work.
    Idle,
    Working,
    /// Blocked on the user: a permission prompt, a question, or stuck.
    NeedsYou,
    /// The last turn finished.
    Done,
    Error,
}

impl AgentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Working => "working",
            AgentState::NeedsYou => "needs_you",
            AgentState::Done => "done",
            AgentState::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<AgentState> {
        Some(match s {
            "idle" => AgentState::Idle,
            "working" => AgentState::Working,
            "needs_you" | "blocked" => AgentState::NeedsYou,
            "done" => AgentState::Done,
            "error" => AgentState::Error,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    SessionStart,
    PromptSubmit,
    ToolStart,
    ToolEnd,
    PermissionRequest,
    Notification,
    TurnEnd,
    TurnFailed,
    Interrupted,
    SubagentStart,
    SubagentStop,
    Compact,
    SessionEnd,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Shell command, when the tool runs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// File the tool reads or writes, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// True when the tool changes files (Edit, Write, apply_patch).
    #[serde(default)]
    pub writes: bool,
    /// Set on ToolEnd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub kind: EventKind,
    pub agent: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The state this event puts the agent in, when it changes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<AgentState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolCall>,
    /// Short human text: the prompt, the notification, the last reply. Capped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// startup, resume, compact, auto, manual: the vendor's qualifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The vendor's own event name, kept for replies and debugging.
    pub raw_event: String,
}

pub(crate) fn cap(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}
