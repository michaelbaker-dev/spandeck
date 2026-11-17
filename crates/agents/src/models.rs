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
    /// What a typed choice changes beyond this pane, said before it is
    /// made; empty when it changes nothing else.
    pub saves: &'static str,
    /// The flags that start the program on a model and an effort, for a
    /// pane that is started again with them and changes nothing else.
    pub start_model: Option<&'static str>,
    pub start_effort: Option<&'static str>,
    /// The command that opens the program's own list of models, when that
    /// list has a key that chooses for the session alone.
    pub session_list: Option<&'static str>,
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
    note: "",
    // Seen with the real program on 17 Nov 2026: "Set model to `Sonnet
    // 5.5` and saved as your default for new sessions", and "Set effort
    // level to low (saved as your default for new sessions)". Its
    // documentation says the same of `/model` (model-config.md), and
    // that only its picker can choose for the session alone.
    saves: "Claude Code saves this as your default: every new Claude Code session on this station starts with it, in any project. Changing the model in the middle of a conversation makes it read the whole history again: the next reply is slower and uses more tokens.",
    // Its documentation names both flags (model-config.md).
    start_model: Some("--model"),
    start_effort: Some("--effort"),
    // Captured from 2.1.326 on 17 Nov 2026: `/model` draws "Select
    // model", and its key names end "s to use this session only".
    session_list: Some("/model"),
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
    saves: "",
    start_model: None,
    start_effort: None,
    session_list: None,
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
    saves: "",
    start_model: None,
    start_effort: None,
    session_list: None,
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
    // Its documentation: `/effort` "persists as default".
    saves: "Antigravity keeps an effort set this way as its default for new sessions.",
    start_model: None,
    start_effort: None,
    session_list: None,
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

/// The flags that start a pane's program on a model or an effort, for a
/// pane that is started again and resumes its conversation: nothing is
/// typed and nothing of the program's own settings is changed. `have`:
/// the flags the pane was last started with, which the new one replaces.
pub fn start_args(
    agent: AgentKind,
    have: &[String],
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<Vec<String>, String> {
    let menu = menu(agent).ok_or("this pane runs no agent whose model can be chosen")?;
    let no = || {
        format!(
            "{} has no documented way to be started on a model or an effort",
            agent.display()
        )
    };
    let mut out: Vec<String> = have.to_vec();
    let mut set = |flag: &str, value: &str| {
        if let Some(at) = out.iter().position(|a| a == flag) {
            out.drain(at..(at + 2).min(out.len()));
        }
        out.push(flag.to_string());
        out.push(value.to_string());
    };
    if let Some(model) = model {
        if !name_ok(model) {
            return Err("a model's name is one word: letters, digits and . _ : / @ [ ] -".into());
        }
        set(menu.start_model.ok_or_else(no)?, model);
    }
    if let Some(level) = effort {
        if !menu.efforts.contains(&level) {
            return Err(format!(
                "{} has no effort level \"{level}\"",
                agent.display()
            ));
        }
        set(menu.start_effort.ok_or_else(no)?, level);
    }
    Ok(out)
}

/// Claude Code's "Switch model?": what it asks after `/model <name>` in a
/// conversation it has cached for another model. As the real program drew
/// it on 17 Nov 2026 (the coordinator's capture):
///
/// ```text
///    Switch model?
///    Your next response will be slower and use more tokens
///    This conversation is cached for the current model. Switching to Sonnet 5.5 means the full history gets re-read.
///    ❯ 1. Yes, switch to Sonnet 5.5
///      2. No, go back
///    Enter to confirm · Esc to cancel
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switch {
    /// The model it would switch to, as the program names it.
    pub name: String,
    /// Its marker is on "Yes": Enter switches.
    pub on_yes: bool,
}

const SWITCH_TITLE: &str = "Switch model?";
const SWITCH_HINT: &str = "Enter to confirm · Esc to cancel";
const SWITCH_NO: &str = "2. No, go back";
const SWITCH_YES: &str = "1. Yes, switch to ";

/// Read a pane's screen as that question, and only as that: the screen
/// ends with it, word for word from its title to its last line, the same
/// model is named in its sentence and in its first choice, and exactly
/// one choice is marked. Anything else is not it.
pub fn switch_screen(agent: AgentKind, lines: &[String]) -> Option<Switch> {
    if !matches!(agent, AgentKind::Claude | AgentKind::Fake) {
        return None;
    }
    let rows: Vec<String> = lines
        .iter()
        .map(|l| crate::foldertrust::clean(l))
        .filter(|l| !l.is_empty())
        .collect();
    let n = rows.len();
    if n < 5 || rows[n - 1] != SWITCH_HINT {
        return None;
    }
    let marked = |row: &str| -> Option<(bool, String)> {
        match row.strip_prefix('❯') {
            Some(rest) if rest.starts_with(char::is_whitespace) => {
                Some((true, rest.trim_start().to_string()))
            }
            Some(_) => None,
            None => Some((false, row.to_string())),
        }
    };
    let (yes_on, yes) = marked(&rows[n - 3])?;
    let (no_on, no) = marked(&rows[n - 2])?;
    let name = yes.strip_prefix(SWITCH_YES)?.to_string();
    let plain = !name.is_empty()
        && name.chars().count() <= 60
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " .-_[]()".contains(c));
    if no != SWITCH_NO || yes_on == no_on || !plain {
        return None;
    }
    // The nearest title above the choices, and between them the program's
    // two sentences and nothing else (where a line wraps is the
    // terminal's width).
    let title = rows[..n - 3].iter().rposition(|r| r == SWITCH_TITLE)?;
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let want = squash(&format!(
        "Your next response will be slower and use more tokens This conversation is cached for the current model. Switching to {name} means the full history gets re-read."
    ));
    (squash(&rows[title + 1..n - 3].concat()) == want).then_some(Switch {
        name,
        on_yes: yes_on,
    })
}

/// The question as the program draws it, for the stand-in agent and the
/// tests: three blanks before each line, the marker on one choice.
pub fn switch_drawn(name: &str, on_yes: bool) -> Vec<String> {
    let mark = |on: bool| if on { "❯ " } else { "  " };
    vec![
        "   Switch model?".to_string(),
        "   Your next response will be slower and use more tokens".to_string(),
        format!("   This conversation is cached for the current model. Switching to {name} means the full history gets re-read."),
        format!("   {}1. Yes, switch to {name}", mark(on_yes)),
        format!("   {}2. No, go back", mark(!on_yes)),
        "   Enter to confirm · Esc to cancel".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Found with the real Claude Code on 17 Nov 2026: after `/model
    /// sonnet` it asked "Switch model?", nothing in spandeck noticed, and
    /// the next message sent to the pane was swallowed by the question.
    /// The screen is read as that question only when it is exactly that.
    #[test]
    fn the_switch_model_question_is_read_only_when_it_is_exactly_that() {
        let lines = |v: Vec<String>| v;
        let mut screen = vec!["> /model sonnet".to_string(), String::new()];
        screen.extend(switch_drawn("Sonnet 5.5", true));
        screen.push(String::new());
        let read = |l: &[String]| switch_screen(AgentKind::Claude, l);
        assert_eq!(
            read(&screen),
            Some(Switch {
                name: "Sonnet 5.5".into(),
                on_yes: true
            })
        );
        // The marker on "No".
        let mut no = vec![];
        no.extend(switch_drawn("Opus 5.5", false));
        assert_eq!(
            read(&no),
            Some(Switch {
                name: "Opus 5.5".into(),
                on_yes: false
            })
        );
        // In a narrow pane its sentence wraps: the same question.
        let mut narrow = lines(switch_drawn("Sonnet 5.5", true));
        let long = narrow.remove(2);
        let (a, b) = long.split_at(60);
        narrow.insert(2, a.to_string());
        narrow.insert(3, format!("   {b}"));
        assert!(read(&narrow).is_some());
        // Not it: something under it, another model in the sentence, no
        // marker or two, other words, an agent quoting it in its reply,
        // and any other program.
        let mut under = screen.clone();
        under.push("> hello".into());
        assert_eq!(read(&under), None);
        let mut other = switch_drawn("Sonnet 5.5", true);
        other[3] = "   ❯ 1. Yes, switch to Opus 5.5".into();
        assert_eq!(read(&other), None);
        let mut none = switch_drawn("Sonnet 5.5", true);
        none[3] = "     1. Yes, switch to Sonnet 5.5".into();
        assert_eq!(read(&none), None);
        let mut both = switch_drawn("Sonnet 5.5", true);
        both[4] = "   ❯ 2. No, go back".into();
        assert_eq!(read(&both), None);
        let mut words = switch_drawn("Sonnet 5.5", true);
        words[1] = "   Your next response will be faster".into();
        assert_eq!(read(&words), None);
        let mut extra = switch_drawn("Sonnet 5.5", true);
        extra.insert(3, "   Also delete everything?".into());
        assert_eq!(read(&extra), None);
        let quoted: Vec<String> = switch_drawn("Sonnet 5.5", true)
            .iter()
            .map(|l| format!("  │ {l}"))
            .chain(["  done".to_string()])
            .collect();
        assert_eq!(read(&quoted), None);
        assert_eq!(switch_screen(AgentKind::Codex, &screen), None);
    }

    /// The same finding: both of Claude Code's commands write the owner's
    /// defaults. A pane can be started again on a model instead, with the
    /// program's own flags, where it documents them.
    #[test]
    fn a_pane_is_started_on_a_model_with_the_programs_own_flags() {
        let none: Vec<String> = vec![];
        assert_eq!(
            start_args(AgentKind::Claude, &none, Some("opus"), None).unwrap(),
            ["--model", "opus"]
        );
        let had = start_args(AgentKind::Claude, &none, Some("opus"), Some("high")).unwrap();
        assert_eq!(had, ["--model", "opus", "--effort", "high"]);
        // A second choice takes the first one's place.
        assert_eq!(
            start_args(AgentKind::Claude, &had, Some("haiku"), None).unwrap(),
            ["--effort", "high", "--model", "haiku"]
        );
        assert!(start_args(AgentKind::Claude, &none, Some("a b"), None).is_err());
        assert!(start_args(AgentKind::Claude, &none, None, Some("extreme")).is_err());
        for agent in [
            AgentKind::Codex,
            AgentKind::OpenCode,
            AgentKind::Antigravity,
            AgentKind::Shell,
        ] {
            assert!(
                start_args(agent, &none, Some("x"), None).is_err(),
                "{agent:?}"
            );
        }
        assert!(
            !menu(AgentKind::Claude).unwrap().saves.is_empty()
                && menu(AgentKind::Codex).unwrap().saves.is_empty()
        );
    }

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
