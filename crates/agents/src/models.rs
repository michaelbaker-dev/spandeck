//! Choosing an agent's model, and how hard it thinks, from the Control Deck
//! (DECISIONS.md, "The chat's bar, tasks, copy and the model", 17 Nov 2026).
//!
//! The deck sends nothing of its own: a choice is the agent program's own
//! command, typed into the pane as the owner would type it, and only while
//! the agent is idle. What each program takes is written here, in one
//! place, with where it was read and when. A program whose model is chosen
//! only in a picker of its own is given no command: the deck opens the
//! terminal at that picker instead, and never presses keys in it blind.
//!
//! The lists are short on purpose and are not the truth: a program's
//! models change with its releases and the owner's account. "Other…" in the
//! deck takes any name, so a new model never waits for a spandeck release.

use crate::AgentKind;

/// One model the deck offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// What is typed after the command.
    pub id: &'static str,
    pub name: &'static str,
    /// One line, in the program's own documentation's words.
    pub says: &'static str,
}

/// What the deck can offer for one agent program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Menu {
    pub models: &'static [Choice],
    /// The levels its effort command takes; empty when it has none.
    pub efforts: &'static [&'static str],
    /// What is typed before a model's name, when the program takes a
    /// model as a typed command.
    pub model_cmd: Option<&'static str>,
    /// What is typed before an effort level, likewise.
    pub effort_cmd: Option<&'static str>,
    /// The command that opens the program's own picker in its terminal,
    /// for a program that takes no model as a typed command.
    pub picker: Option<&'static str>,
    /// What the owner should know before choosing, in a sentence.
    pub note: &'static str,
    /// Where this was read, and when.
    pub source: &'static str,
}

/// Claude Code. Read on 16 Nov 2026 from
/// https://code.claude.com/docs/en/model-config.md ("During session: use
/// `/model <alias|name>` to switch immediately, or run `/model` with no
/// argument to open the picker"), commands.md ("`/effort
/// [level|auto|status|ultracode [on|off]]` Set the effort level: `low` to
/// `xhigh`, `max`, or `auto`") and interactive-mode.md. The aliases and
/// their lines are that page's. `/model <name>` also saves the choice as
/// the default for new sessions (the same page): the deck says so.
const CLAUDE: Menu = Menu {
    models: &[
        Choice {
            id: "default",
            name: "Default",
            says: "Clears any model override and goes back to the default for your account",
        },
        Choice {
            id: "fable",
            name: "Fable",
            says: "For your hardest and longest-running tasks",
        },
        Choice {
            id: "opus",
            name: "Opus",
            says: "The latest Opus model, for complex reasoning tasks",
        },
        Choice {
            id: "sonnet",
            name: "Sonnet",
            says: "The latest Sonnet model, for daily coding tasks",
        },
        Choice {
            id: "haiku",
            name: "Haiku",
            says: "The fast and efficient Haiku model, for simple tasks",
        },
        Choice {
            id: "opusplan",
            name: "Opus to plan",
            says: "Opus during plan mode, then Sonnet for the work",
        },
    ],
    efforts: &["low", "medium", "high", "xhigh", "max"],
    model_cmd: Some("/model "),
    effort_cmd: Some("/effort "),
    picker: None,
    note: "Claude Code also keeps a model chosen here as the default for new sessions.",
    source: "code.claude.com/docs/en/model-config.md and commands.md, read 16 Nov 2026",
};

/// Codex 0.169.0. Read on 16 Nov 2026: its commands page
/// (learn.chatgpt.com/docs/developer-commands?surface=cli) gives "`/model
/// [model] [--reasoning=<level>] [--speed=<tier>]`", for the session, and
/// `/model --reasoning=high` keeps the model; in the source at that tag,
/// `codex-rs/tui/src/slash_command.rs` lists `Model` under
/// `supports_inline_args`, and a word after the model's name that is not a
/// setting is refused ("Unexpected argument"). No model is listed here: the
/// catalogue is the program's (`codex debug models`), and was not read on
/// this machine. The levels are the usual three; the program names the
/// ones a model takes when it refuses one, and that was not read either.
const CODEX: Menu = Menu {
    models: &[],
    efforts: &["low", "medium", "high"],
    model_cmd: Some("/model "),
    effort_cmd: Some("/model --reasoning="),
    picker: None,
    note: "For this session of Codex only. Its models are listed by /model in its terminal: type a name under Other.",
    source: "learn.chatgpt.com/docs/developer-commands and codex-rs/tui at rust-v0.169.0, read 16 Nov 2026",
};

/// OpenCode. Read on 16 Nov 2026 (opencode.ai/docs/tui/ and the source):
/// `/models` opens a picker and takes no argument, and so does its choice
/// of a model's variant. Nothing is typed for it.
const OPENCODE: Menu = Menu {
    models: &[],
    efforts: &[],
    model_cmd: None,
    effort_cmd: None,
    picker: Some("/models"),
    note: "OpenCode chooses its model in a picker of its own, in its terminal.",
    source: "opencode.ai/docs/tui/, read 16 Nov 2026",
};

/// Antigravity CLI. Read on 16 Nov 2026 (antigravity.google/docs/cli):
/// `/model` is a picker ("Run `/model`, select ... and press Enter"), and
/// `/model <model> <prompt>` runs one prompt on another model: whether
/// `/model <model>` alone changes the model could not be established, so
/// no model is typed. `/effort <level>` sets the level directly (low,
/// medium, high, xhigh, max) and keeps it as the default.
const ANTIGRAVITY: Menu = Menu {
    models: &[],
    efforts: &["low", "medium", "high", "xhigh", "max"],
    model_cmd: None,
    effort_cmd: Some("/effort "),
    picker: Some("/model"),
    note: "Antigravity chooses its model in a picker of its own, in its terminal, and keeps the choice. Its effort can be set from here.",
    source: "antigravity.google/docs/cli/reference/ and /commands/effort/, read 16 Nov 2026",
};

pub fn menu(agent: AgentKind) -> Option<Menu> {
    match agent {
        // The stand-in agent speaks Claude Code's dialect, here too.
        AgentKind::Claude | AgentKind::Fake => Some(CLAUDE),
        AgentKind::Codex => Some(CODEX),
        AgentKind::OpenCode => Some(OPENCODE),
        AgentKind::Antigravity => Some(ANTIGRAVITY),
        AgentKind::Shell => None,
    }
}

/// A model's name as a program takes it: one word of the characters model
/// names are made of. Anything else is refused, so that what is typed
/// into the pane is the command and a name, and nothing more.
pub fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@[]-".contains(c))
}

/// The line that sets the model, exactly as the owner would type it.
/// `Err` says why there is none.
pub fn model_command(agent: AgentKind, model: &str) -> Result<String, String> {
    let menu = menu(agent).ok_or("this pane runs no agent whose model can be chosen")?;
    if !name_ok(model) {
        return Err("a model's name is one word: letters, digits and . _ : / @ [ ] -".into());
    }
    match menu.model_cmd {
        Some(cmd) => Ok(format!("{cmd}{model}")),
        None => Err(format!(
            "{} chooses its model in a picker of its own: type {} in its terminal",
            agent.display(),
            menu.picker.unwrap_or("/model")
        )),
    }
}

/// The line that sets how hard it thinks.
pub fn effort_command(agent: AgentKind, level: &str) -> Result<String, String> {
    let menu = menu(agent).ok_or("this pane runs no agent with such a setting")?;
    match menu.effort_cmd {
        Some(cmd) if menu.efforts.contains(&level) => Ok(format!("{cmd}{level}")),
        _ => Err(format!(
            "{} has no effort level \"{level}\" that can be typed as a command",
            agent.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's report of 17 Nov 2026 ("a model picker ... for all
    /// chats"): what is typed into an agent is that program's documented
    /// command and nothing else, and a program with only a picker gets no
    /// typed command at all.
    #[test]
    fn the_command_is_the_programs_own_and_a_picker_is_never_typed_into() {
        for agent in [AgentKind::Claude, AgentKind::Fake] {
            assert_eq!(model_command(agent, "opus").unwrap(), "/model opus");
            assert_eq!(
                model_command(agent, "claude-fable-5-2").unwrap(),
                "/model claude-fable-5-2"
            );
            assert_eq!(
                model_command(agent, "sonnet[1m]").unwrap(),
                "/model sonnet[1m]"
            );
            assert_eq!(effort_command(agent, "xhigh").unwrap(), "/effort xhigh");
            assert!(effort_command(agent, "extreme").is_err());
        }
        // A name is one word: nothing rides in behind it.
        for bad in [
            "",
            "opus; rm -rf /",
            "opus\n/clear",
            "a b",
            "-x",
            "$(id)",
            "`id`",
        ] {
            assert!(model_command(AgentKind::Claude, bad).is_err(), "{bad:?}");
        }
        // Codex: its `/model` takes the name, and the effort as a setting.
        assert_eq!(
            model_command(AgentKind::Codex, "gpt-5.2-codex").unwrap(),
            "/model gpt-5.2-codex"
        );
        assert_eq!(
            effort_command(AgentKind::Codex, "high").unwrap(),
            "/model --reasoning=high"
        );
        // OpenCode: a picker for both. Antigravity: a picker for the model
        // (a name typed after `/model` could be taken for a prompt), and a
        // typed command for the effort.
        assert!(model_command(AgentKind::OpenCode, "some/model").is_err());
        assert!(effort_command(AgentKind::OpenCode, "high").is_err());
        assert!(model_command(AgentKind::Antigravity, "some-model").is_err());
        assert_eq!(
            effort_command(AgentKind::Antigravity, "high").unwrap(),
            "/effort high"
        );
        assert_eq!(menu(AgentKind::OpenCode).unwrap().picker, Some("/models"));
        assert_eq!(menu(AgentKind::Antigravity).unwrap().picker, Some("/model"));
        assert!(menu(AgentKind::Shell).is_none());
        assert!(model_command(AgentKind::Shell, "opus").is_err());
    }
}
