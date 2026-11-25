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
    /// For a screen that is known: what it is waiting for, in plain words
    /// for the owner. `{rest}` stands for what follows the matched words
    /// on their line (a time, say). Such a screen is believed even when
    /// the agent's hooks are reporting: no hook says that a program sits
    /// at a sign-in screen. spandeck never answers any of them.
    pub reason: Option<&'static str>,
}

pub const CLAUDE_RULES: &[Rule] = &[
    Rule {
        state: AgentState::NeedsYou,
        // The folder-trust prompt was seen on a real 2.1.323 session: a new
        // directory blocks Claude Code until someone answers it.
        any: &[
            "Do you want to proceed?",
            "Do you want to make this edit",
            "Do you want to create",
            "❯ 1. Yes",
            "Would you like to proceed",
            "Yes, I trust this folder",
            // Signed out: the agent cannot work until someone runs /login.
            "Please run /login",
            "Run /login",
        ],
        window: 16,
        reason: None,
    },
    // "Switch model?" (seen with the real program on 17 Nov 2026, after
    // `/model sonnet`): believed with hooks reporting, since no hook says
    // it. Read exactly, with its model's name, the station says more
    // (`models::switch_screen`; core, modelswitch.rs); this is for a
    // screen that is that question in other words.
    Rule {
        state: AgentState::NeedsYou,
        any: &["❯ 1. Yes, switch to "],
        window: 6,
        reason: Some("asks whether to switch model: answer in the terminal"),
    },
    Rule {
        state: AgentState::Working,
        any: &["esc to interrupt", "ctrl+c to interrupt"],
        window: 8,
        reason: None,
    },
];

pub const CODEX_RULES: &[Rule] = &[
    // Seen on a real Codex 0.169.0 (16 Nov 2026). At its plan-mode
    // question the screen says "Question 1/1 (1 unanswered)" and, at the
    // bottom, "tab to add notes | enter to submit answer | esc to
    // interrupt": the last words are the Working rule's, so this rule
    // stands before it. The pane was shown as working while it waited.
    Rule {
        state: AgentState::NeedsYou,
        any: &["enter to submit answer"],
        window: 16,
        reason: Some("asks you a question, in the terminal"),
    },
    // "Hooks need review / N hooks are new or changed … 1. Review hooks
    // 2. Trust all and continue 3. Continue without trusting (hooks won't
    // run)": Codex skips hooks whose definition changed until the owner
    // trusts them again. Whose hooks they are is not known here: where the
    // review is the program's first screen the pane's reason comes from
    // core/hookstrust.rs, which reads the files, and this rule is for a
    // review that shows later.
    Rule {
        state: AgentState::NeedsYou,
        any: &[
            "Hooks need review",
            "Trust all and continue",
            "Continue without trusting",
        ],
        window: 16,
        reason: Some("asks you to review hooks: answer in the terminal (run /hooks)"),
    },
    Rule {
        state: AgentState::NeedsYou,
        any: &[
            "Allow command?",
            "Would you like to run the following command",
            "Apply changes?",
            "› 1. Yes",
            "Approve?",
        ],
        window: 16,
        reason: None,
    },
    Rule {
        state: AgentState::Working,
        any: &["esc to interrupt", "Esc to interrupt", "Working ("],
        window: 8,
        reason: None,
    },
];

/// From the words in OpenCode 2.3.0's program text, and (the first three)
/// from a real 2.3.0 screen on 16 Nov 2026; treat a wrong guess here as a
/// rule to fix, not a mystery.
pub const OPENCODE_RULES: &[Rule] = &[
    // "You've used today's free messages. They reset at 12:00 AM ·
    // 11/17/2026.": the program takes no turn, and read as idle, so it
    // was nudged about its mail for ever.
    Rule {
        state: AgentState::NeedsYou,
        any: &["free messages. They reset at "],
        window: 16,
        reason: Some("out of free messages until {rest}"),
    },
    Rule {
        state: AgentState::NeedsYou,
        any: &["You've used today's free messages"],
        window: 16,
        reason: Some("out of free messages"),
    },
    // The sign-in dialog: "OpenCode Console account … Enter code: …
    // Waiting for authorization…".
    Rule {
        state: AgentState::NeedsYou,
        any: &["Waiting for authorization"],
        window: 16,
        reason: Some("asks you to sign in"),
    },
    Rule {
        state: AgentState::NeedsYou,
        any: &["Permission required", "Allow once"],
        window: 16,
        reason: None,
    },
    Rule {
        state: AgentState::Working,
        any: &["esc interrupt", "esc to interrupt"],
        window: 8,
        reason: None,
    },
];

/// A fallback only: Antigravity reports its state over OSC 7501. The sign-in
/// screen was seen live on 1.5.9; the rest is from the program's text.
pub const ANTIGRAVITY_RULES: &[Rule] = &[
    // Seen on a real 1.5.9 (16 Nov 2026): "Do you trust the contents of
    // this project? … Yes, I trust this folder / No, exit" in a folder it
    // has not been run in, and "Allow calling this tool?" before a call
    // of an MCP server's tool (spandeck's own among them).
    Rule {
        state: AgentState::NeedsYou,
        any: &[
            "Do you trust the contents of this project?",
            "Yes, I trust this folder",
        ],
        window: 16,
        reason: Some("asks whether to trust this folder"),
    },
    Rule {
        state: AgentState::NeedsYou,
        any: &["Allow calling this tool?"],
        window: 16,
        reason: Some("asks permission for a tool call"),
    },
    Rule {
        state: AgentState::NeedsYou,
        any: &["You are currently not signed in", "Do you want to proceed?"],
        window: 16,
        reason: None,
    },
    Rule {
        state: AgentState::Working,
        any: &["esc to interrupt", "Esc to cancel", "esc to cancel"],
        window: 8,
        reason: None,
    },
];

pub const FAKE_RULES: &[Rule] = &[
    // A screen that is known, as the real agents have: believed over hooks.
    Rule {
        state: AgentState::NeedsYou,
        any: &["[fake] sign in: "],
        window: 6,
        reason: Some("asks you to sign in ({rest})"),
    },
    Rule {
        state: AgentState::NeedsYou,
        any: &["[fake] permission?"],
        window: 6,
        reason: None,
    },
    Rule {
        state: AgentState::Working,
        any: &["[fake] working"],
        window: 3,
        reason: None,
    },
];

pub fn detect(rules: &[Rule], lines: &[String]) -> Option<AgentState> {
    detect_why(rules, lines).map(|(state, _)| state)
}

/// `detect`, with the rule's reason when it has one. What `{rest}` is
/// filled with comes off the program's screen: it is data, kept short,
/// with no control characters, and shown as text.
pub fn detect_why(rules: &[Rule], lines: &[String]) -> Option<(AgentState, Option<String>)> {
    // Ignore trailing blank rows so "the bottom lines" means the content.
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(0);
    let lines = &lines[..end];
    for rule in rules {
        let start = lines.len().saturating_sub(rule.window);
        let found = lines[start..].iter().find_map(|l| {
            rule.any
                .iter()
                .find_map(|needle| l.find(needle).map(|at| &l[at + needle.len()..]))
        });
        if let Some(rest) = found {
            let reason = rule.reason.map(|r| {
                let rest: String = rest.chars().filter(|c| !c.is_control()).take(40).collect();
                let rest = rest.trim().trim_end_matches(['.', '…', '│', '|', ' ']);
                r.replace("{rest}", rest)
            });
            return Some((rule.state, reason));
        }
    }
    None
}

/// OSC 7501 (Program Status Protocol): read when present, never required.
/// Returns the reported state from `ESC ] 7501 ; key=value:... ST` sequences.
pub fn parse_osc7501(bytes: &[u8]) -> Option<AgentState> {
    parse_osc7501_why(bytes).map(|(state, _)| state)
}

/// The state, and what the program said with it: every pair but its id and
/// the state itself, as it sent them (seen live: `kind=auth:…`). It is the
/// program's text, so it is cut to a length and its control characters go.
pub fn parse_osc7501_why(bytes: &[u8]) -> Option<(AgentState, Option<String>)> {
    let marker = b"\x1b]7501;";
    let start = bytes.windows(marker.len()).rposition(|w| w == marker)? + marker.len();
    let rest = &bytes[start..];
    let end = rest.iter().position(|b| *b == 0x07 || *b == 0x1b)?;
    let body = std::str::from_utf8(&rest[..end]).ok()?;
    let mut state = None;
    let mut said: Vec<&str> = Vec::new();
    for pair in body.split(':') {
        if let Some(v) = pair
            .strip_prefix("status=")
            .or_else(|| pair.strip_prefix("state="))
        {
            if state.is_none() {
                state = Some(match v {
                    "idle" => AgentState::Idle,
                    "working" => AgentState::Working,
                    "blocked" => AgentState::NeedsYou,
                    "done" => AgentState::Done,
                    "error" => AgentState::Error,
                    _ => return None,
                });
                continue;
            }
        }
        if state.is_none() && pair.starts_with("id=") {
            continue;
        }
        said.push(pair);
    }
    let said: String = said
        .join(":")
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect();
    let said = said.trim().to_string();
    Some((state?, (!said.is_empty()).then_some(said)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(|l| l.to_string()).collect()
    }

    #[test]
    fn permission_prompt_beats_spinner() {
        let screen = lines(
            "✻ Thinking… (esc to interrupt)\n\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n\n\n",
        );
        assert_eq!(detect(CLAUDE_RULES, &screen), Some(AgentState::NeedsYou));
    }

    #[test]
    fn spinner_means_working_and_blank_means_unknown() {
        assert_eq!(
            detect(
                CLAUDE_RULES,
                &lines("✻ Pondering… (12s · esc to interrupt)\n\n")
            ),
            Some(AgentState::Working)
        );
        assert_eq!(detect(CLAUDE_RULES, &lines("> \n")), None);
    }

    /// Found with the real programs on 16 Nov 2026 (the owner's matrix
    /// run; the screens below are written by hand from the words that
    /// were seen, and nothing else). Each of these screens waited for the
    /// owner and spandeck showed the pane as idle or working: OpenCode
    /// out of quota and at its sign-in dialog read as idle and was nudged
    /// about its mail for ever; Codex at its plan-mode question read as
    /// working, because the question's footer ends in "esc to interrupt".
    #[test]
    fn the_screens_that_wait_for_the_owner_say_so_and_say_why() {
        let why = |rules: &[Rule], screen: &str| detect_why(rules, &lines(screen));
        let needs = |reason: &str| Some((AgentState::NeedsYou, Some(reason.to_string())));

        assert_eq!(
            why(
                OPENCODE_RULES,
                "\n  You've used today's free messages. They reset at 12:00 AM · 11/17/2026.\n\n"
            ),
            needs("out of free messages until 12:00 AM · 11/17/2026")
        );
        // The same words broken over two lines by a narrow pane.
        assert_eq!(
            why(
                OPENCODE_RULES,
                "  You've used today's free messages. They\n  reset at 12:00 AM · 11/17/2026.\n"
            ),
            needs("out of free messages")
        );
        assert_eq!(
            why(
                OPENCODE_RULES,
                "  OpenCode Console account\n\n  Enter code: \n\n  Waiting for authorization…\n"
            ),
            needs("asks you to sign in")
        );

        assert_eq!(
            why(
                CODEX_RULES,
                "  Question 1/1 (1 unanswered)\n\n\n  tab to add notes | enter to submit answer | esc to interrupt\n"
            ),
            needs("asks you a question, in the terminal")
        );
        assert_eq!(
            why(
                CODEX_RULES,
                "  Hooks need review\n  3 hooks are new or changed\n\n  1. Review hooks\n  2. Trust all and continue\n  3. Continue without trusting (hooks won't run)\n"
            ),
            needs("asks you to review hooks: answer in the terminal (run /hooks)")
        );
        // An ordinary turn is still a turn.
        assert_eq!(
            why(CODEX_RULES, "• Working (12s • esc to interrupt)\n"),
            Some((AgentState::Working, None))
        );

        assert_eq!(
            why(
                ANTIGRAVITY_RULES,
                "  Do you trust the contents of this project?\n\n  > Yes, I trust this folder\n    No, exit\n"
            ),
            needs("asks whether to trust this folder")
        );
        assert_eq!(
            why(
                ANTIGRAVITY_RULES,
                "  spandeck_spandeck/inbox\n\n  Allow calling this tool?\n"
            ),
            needs("asks permission for a tool call")
        );
        // What is taken off a screen for a reason is short, plain text.
        let long = format!(
            "free messages. They reset at {}\x1b[31m{}",
            "9".repeat(30),
            "8".repeat(60)
        );
        let (_, reason) = why(OPENCODE_RULES, &long).unwrap();
        let reason = reason.unwrap();
        assert!(
            reason.len() <= "out of free messages until ".len() + 40,
            "{reason}"
        );
        assert!(!reason.contains('\x1b'));
    }

    #[test]
    fn osc7501_is_read_when_present() {
        assert_eq!(
            parse_osc7501(b"hi\x1b]7501;id=a:status=blocked\x07"),
            Some(AgentState::NeedsYou)
        );
        assert_eq!(
            parse_osc7501(b"\x1b]7501;status=working\x1b\\"),
            Some(AgentState::Working)
        );
        assert_eq!(parse_osc7501(b"plain output"), None);
        // What it said with the state is kept: a pane that needs the owner
        // says why (LLM-Server, 25 Nov 2026: "waiting for you" and no more,
        // sixteen seconds after a turn ended, and nothing to tell why from).
        assert_eq!(
            parse_osc7501_why(b"\x1b]7501;state=blocked:kind=auth:sign in at http://x\x07"),
            Some((
                AgentState::NeedsYou,
                Some("kind=auth:sign in at http://x".to_string())
            ))
        );
        assert_eq!(
            parse_osc7501_why(b"\x1b]7501;id=a:status=done\x07"),
            Some((AgentState::Done, None))
        );
    }
}
