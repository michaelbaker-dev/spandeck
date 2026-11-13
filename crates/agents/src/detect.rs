//! Screen detection: the fallback observer when hooks are missing.
//!
//! It only observes. It can never block anything (Guarantees: Enforcement).
//! Rules look at the last lines of the visible grid; the first match wins, so
//! "needs you" rules come before "working" rules.

use crate::event::AgentState;

pub struct Rule {
    pub state: AgentState,
    /// Any of these substrings, in the bottom `window` lines.
    pub any: &'static [&'static str],
    pub window: usize,
}

pub const CLAUDE_RULES: &[Rule] = &[
    Rule {
        state: AgentState::NeedsYou,
        any: &["Do you want to proceed?", "Do you want to make this edit", "Do you want to create", "❯ 1. Yes", "Would you like to proceed"],
        window: 16,
    },
    Rule { state: AgentState::Working, any: &["esc to interrupt", "ctrl+c to interrupt"], window: 8 },
];

pub const CODEX_RULES: &[Rule] = &[
    Rule {
        state: AgentState::NeedsYou,
        any: &["Allow command?", "Would you like to run the following command", "Apply changes?", "› 1. Yes", "Approve?"],
        window: 16,
    },
    Rule { state: AgentState::Working, any: &["esc to interrupt", "Esc to interrupt", "Working ("], window: 8 },
];

pub const FAKE_RULES: &[Rule] = &[
    Rule { state: AgentState::NeedsYou, any: &["[fake] permission?"], window: 6 },
    Rule { state: AgentState::Working, any: &["[fake] working"], window: 3 },
];

pub fn detect(rules: &[Rule], lines: &[String]) -> Option<AgentState> {
    // Ignore trailing blank rows so "the bottom lines" means the content.
    let end = lines.iter().rposition(|l| !l.trim().is_empty()).map(|i| i + 1).unwrap_or(0);
    let lines = &lines[..end];
    for rule in rules {
        let start = lines.len().saturating_sub(rule.window);
        if lines[start..].iter().any(|l| rule.any.iter().any(|needle| l.contains(needle))) {
            return Some(rule.state);
        }
    }
    None
}

/// OSC 7501 (Program Status Protocol): read when present, never required.
/// Returns the reported state from `ESC ] 7501 ; key=value:... ST` sequences.
pub fn parse_osc7501(bytes: &[u8]) -> Option<AgentState> {
    let marker = b"\x1b]7501;";
    let start = bytes.windows(marker.len()).rposition(|w| w == marker)? + marker.len();
    let rest = &bytes[start..];
    let end = rest.iter().position(|b| *b == 0x07 || *b == 0x1b)?;
    let body = std::str::from_utf8(&rest[..end]).ok()?;
    for pair in body.split(':') {
        if let Some(v) = pair.strip_prefix("status=").or_else(|| pair.strip_prefix("state=")) {
            return Some(match v {
                "idle" => AgentState::Idle,
                "working" => AgentState::Working,
                "blocked" => AgentState::NeedsYou,
                "done" => AgentState::Done,
                "error" => AgentState::Error,
                _ => return None,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(|l| l.to_string()).collect()
    }

    #[test]
    fn permission_prompt_beats_spinner() {
        let screen = lines("✻ Thinking… (esc to interrupt)\n\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n\n\n");
        assert_eq!(detect(CLAUDE_RULES, &screen), Some(AgentState::NeedsYou));
    }

    #[test]
    fn spinner_means_working_and_blank_means_unknown() {
        assert_eq!(detect(CLAUDE_RULES, &lines("✻ Pondering… (12s · esc to interrupt)\n\n")), Some(AgentState::Working));
        assert_eq!(detect(CLAUDE_RULES, &lines("> \n")), None);
    }

    #[test]
    fn osc7501_is_read_when_present() {
        assert_eq!(parse_osc7501(b"hi\x1b]7501;id=a:status=blocked\x07"), Some(AgentState::NeedsYou));
        assert_eq!(parse_osc7501(b"\x1b]7501;status=working\x1b\\"), Some(AgentState::Working));
        assert_eq!(parse_osc7501(b"plain output"), None);
    }
}
