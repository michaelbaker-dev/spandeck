//! The question an agent program asks when it is started in a folder it has
//! not seen: "do you trust this folder?" (DECISIONS.md, "The folder-trust
//! question").
//!
//! This file only reads a screen and says what it is. It presses nothing;
//! whether a key is pressed, and for whom, is decided in the daemon
//! (core/foldertrust.rs). A screen is read at three levels:
//!
//! - `Exact`: the whole screen is the program's own prompt as it was
//!   captured, word for word, for the folder the pane was started in, with
//!   nothing before it and nothing after it. Only this may be answered on
//!   the owner's standing choice.
//! - `Choices`: the two choices and the line under them are the program's,
//!   but something else is said above them (a version that words the
//!   question differently, or Claude Code listing what the folder's
//!   settings would switch on). The owner can answer it with one tap,
//!   having read it; nothing is pressed for them.
//! - `Mentioned`: it talks about trusting the folder and spandeck cannot
//!   read its choices. The pane says so, and the owner answers in the
//!   terminal.
//!
//! Sources of the screens, none of them a guess:
//!
//! - Claude Code: recorded from 2.1.323 (`terminal/recordings/
//!   claude-code-tui.110x36.bin`, 13 Nov 2026) and the same words and
//!   options in the program text of 2.1.325 (`TrustDialog`: `cancelFirst`,
//!   `focus: "cancel"`). Its own choice is "No, exit": Enter alone leaves.
//! - Antigravity: read from a real 1.5.9 pane on 16 Nov 2026 (the rows are
//!   in this file's tests), Google's codelab "Hands-on with Antigravity
//!   CLI" (1.1.27), and the same words in the program text of the installed
//!   1.5.9. Its own choice is "Yes, I trust this folder". 1.5.9 draws one
//!   more row straight under its hint: the model and effort, at the right
//!   edge ("Gemini 4 Carbon · medium"). That one row is allowed for, by
//!   its shape (`status_row`), and nothing else is.
//! - Codex (0.169.0) asks "Trust this folder?" too, but its screen has not
//!   been captured, so it has no shape here: it is only ever `Mentioned`.

use crate::AgentKind;

/// Which of the two choices the program's marker is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// "Yes, I trust this folder": Enter accepts.
    Trust,
    /// "No, exit": Enter leaves the program.
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Mentioned,
    Choices,
    Exact,
}

/// What a screen was read as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    pub level: Level,
    /// Known whenever the choices could be read.
    pub focus: Option<Focus>,
}

/// One program's folder-trust screen, as captured.
pub struct Shape {
    title: &'static str,
    /// What is said between the folder's path and the choices.
    body: &'static [&'static str],
    trust: &'static str,
    exit: &'static str,
    marker: char,
    /// The trusting choice is the upper one.
    trust_first: bool,
    hint: &'static str,
    /// The program draws a status row (its model) straight under the hint.
    status: bool,
}

const CLAUDE: Shape = Shape {
    title: "Accessing workspace:",
    body: &[
        "Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source project, or work from your team). If not, take a moment to review what's in this folder first.",
        "Claude Code'll be able to read, edit, and execute files here.",
        "Security guide",
    ],
    trust: "Yes, I trust this folder",
    exit: "No, exit",
    marker: '❯',
    trust_first: false,
    hint: "Enter to confirm · Esc to cancel",
    status: false,
};

const ANTIGRAVITY: Shape = Shape {
    title: "Accessing workspace:",
    body: &[
        "Do you trust the contents of this project?",
        "Antigravity CLI requires permission to read, edit, and execute files here.",
    ],
    trust: "Yes, I trust this folder",
    exit: "No, exit",
    marker: '>',
    trust_first: true,
    hint: "↑/↓ Navigate · enter Confirm",
    status: true,
};

/// What the owner is told a program's folder trust allows, from the
/// program's own documentation.
pub struct Info {
    /// One line for the tick box.
    pub allows: &'static str,
    /// Where that is written.
    pub source: &'static str,
    /// How far the program's own record of the trust reaches.
    pub reach: &'static str,
}

const CLAUDE_INFO: Info = Info {
    allows: "Claude Code will read, edit and run files there, and the folder's own settings apply without asking: its hooks and helper commands run, its permission rules and extra folders count, its MCP servers can be offered.",
    source: "https://code.claude.com/docs/en/permissions#project-allow-rules-and-workspace-trust",
    reach: "Claude Code keeps its trust for the whole git repository the folder is in (a worktree counts as its main checkout), or for the folder and everything under it when it is in no repository.",
};

const ANTIGRAVITY_INFO: Info = Info {
    allows: "Antigravity will read, edit and run files there.",
    source: "https://codelabs.developers.google.com/antigravity-cli-hands-on",
    reach: "Antigravity keeps its trust for that exact folder: a subfolder is asked about again.",
};

fn shape(kind: AgentKind) -> Option<&'static Shape> {
    match kind {
        AgentKind::Claude => Some(&CLAUDE),
        AgentKind::Antigravity => Some(&ANTIGRAVITY),
        _ => None,
    }
}

/// Programs whose folder-trust screen spandeck can read, and so the ones
/// the tick box is offered for.
pub fn info(kind: AgentKind) -> Option<&'static Info> {
    match kind {
        AgentKind::Claude => Some(&CLAUDE_INFO),
        AgentKind::Antigravity => Some(&ANTIGRAVITY_INFO),
        _ => None,
    }
}

/// Enter: takes the choice the marker is on.
pub const CONFIRM: &[u8] = b"\r";

/// The arrow key that moves the marker from the leaving choice to the
/// trusting one, for a program whose own choice is to leave.
pub fn move_to_trust(kind: AgentKind) -> Option<&'static [u8]> {
    let s = shape(kind)?;
    Some(if s.trust_first { b"\x1b[A" } else { b"\x1b[B" })
}

/// A line without the frame a program may draw around its prompt.
fn clean(line: &str) -> String {
    line.chars()
        .filter(|c| !('\u{2500}'..='\u{257f}').contains(c))
        .collect::<String>()
        .trim()
        .to_string()
}

/// Words only: where a line was wrapped is the terminal's width, not the
/// program's wording.
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// A choice's line: `Some(true)` with the marker before it, `Some(false)`
/// without, `None` when the line is something else.
fn choice(row: &str, label: &str, marker: char) -> Option<bool> {
    if row == label {
        return Some(false);
    }
    let rest = row.strip_prefix(marker)?;
    (rest.starts_with(char::is_whitespace) && rest.trim_start() == label).then_some(true)
}

/// Read a pane's visible screen. `folders`: the ways the pane's folder may
/// be written (as it was started, and with links resolved); an exact
/// screen names one of them.
pub fn read(kind: AgentKind, lines: &[String], folders: &[String]) -> Option<Seen> {
    let rows: Vec<String> = lines
        .iter()
        .map(|l| clean(l))
        .filter(|l| !l.is_empty())
        .collect();
    if let Some(s) = shape(kind) {
        if let Some(seen) = read_shape(s, &rows, folders) {
            return Some(seen);
        }
        // The same screen with the program's one status row under it.
        if s.status && under_hint(s, lines) {
            if let Some(seen) = read_shape(s, &rows[..rows.len() - 1], folders) {
                return Some(seen);
            }
        }
    }
    mentions(&rows).then_some(Seen {
        level: Level::Mentioned,
        focus: None,
    })
}

/// Is the last thing on the screen one status row, on the row straight
/// after the program's hint?
fn under_hint(s: &Shape, lines: &[String]) -> bool {
    let Some(last) = lines.iter().rposition(|l| !l.trim().is_empty()) else {
        return false;
    };
    last >= 1 && clean(&lines[last - 1]) == s.hint && status_row(&lines[last])
}

/// A status row as Antigravity 1.5.9 draws it: short, at the right edge
/// (more room before it than it is long), and nothing that could be a
/// choice, a question or a list: no marker, no question mark, no "1.".
fn status_row(line: &str) -> bool {
    let text = line.trim();
    let before = line.chars().take_while(|c| *c == ' ').count();
    let len = text.chars().count();
    let listed = text
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .starts_with(['.', ')'])
        && text.starts_with(|c: char| c.is_ascii_digit());
    (1..=40).contains(&len)
        && before >= len
        && !text.contains(['>', '❯', '›', '?', '\u{1b}'])
        && !listed
        && !text.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c))
}

fn read_shape(s: &Shape, rows: &[String], folders: &[String]) -> Option<Seen> {
    // The screen ends with the two choices and the line under them, and
    // exactly one choice is marked.
    let n = rows.len();
    if n < 4 || rows[n - 1] != s.hint {
        return None;
    }
    let (upper, lower) = if s.trust_first {
        (s.trust, s.exit)
    } else {
        (s.exit, s.trust)
    };
    let up = choice(&rows[n - 3], upper, s.marker)?;
    let low = choice(&rows[n - 2], lower, s.marker)?;
    let focus = match (up, low, s.trust_first) {
        (true, false, true) | (false, true, false) => Focus::Trust,
        (true, false, false) | (false, true, true) => Focus::Exit,
        _ => return None,
    };
    let title = rows[..n - 3].iter().position(|r| r == s.title)?;
    // Word for word, from the first thing on the screen: the title, the
    // pane's own folder, what the program says, and nothing else.
    let said = squash(&rows[title + 1..n - 3].concat());
    let exact = title == 0
        && folders.iter().any(|folder| {
            let mut want = squash(folder);
            want.push_str(&squash(&s.body.concat()));
            !folder.is_empty() && want == said
        });
    Some(Seen {
        level: if exact { Level::Exact } else { Level::Choices },
        focus: Some(focus),
    })
}

/// Does the screen ask about trusting the folder at all? A line that
/// begins with one of the programs' own questions or their trusting choice;
/// the same words inside a sentence are not it. Never Codex's "Hooks need
/// review", which is about hooks and is not a folder's trust.
fn mentions(rows: &[String]) -> bool {
    rows.iter().any(|r| {
        let r = r.trim_start_matches(|c: char| {
            c.is_whitespace() || c.is_ascii_digit() || matches!(c, '❯' | '>' | '›' | '.')
        });
        [
            "Yes, I trust this folder",
            "Trust this folder?",
            "Do you trust the contents",
            "Do you trust the files",
            "Quick safety check: Is this a project you created or one you trust?",
        ]
        .iter()
        .any(|start| r.starts_with(start))
    })
}

/// The screens, drawn as the programs draw them: what the stand-in agent
/// shows in the scenario tests, and what this file's own tests read.
pub mod drawn {
    use super::Focus;

    fn mark(on: bool, marker: &str) -> String {
        if on {
            format!("{marker} ")
        } else {
            "  ".to_string()
        }
    }

    /// Greedy wrap at `width` columns, as a terminal program wraps a
    /// paragraph it indents by one column.
    fn wrap(text: &str, width: usize) -> Vec<String> {
        let mut out = vec![];
        let mut line = String::new();
        for word in text.split(' ') {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
        out
    }

    /// Claude Code 2.1.323's screen at `cols` columns. `extra`: lines it
    /// adds above "Security guide" when the folder's settings would switch
    /// something on.
    pub fn claude(folder: &str, focus: Focus, cols: usize, extra: &[&str]) -> Vec<String> {
        let mut out = vec![String::new(), "─".repeat(cols)];
        out.push(" Accessing workspace:".into());
        out.push(String::new());
        out.push(format!(" {folder}"));
        out.push(String::new());
        for para in &super::CLAUDE.body[..2] {
            out.extend(wrap(para, cols - 2).into_iter().map(|l| format!(" {l}")));
            out.push(String::new());
        }
        for line in extra {
            out.push(format!(" {line}"));
        }
        if !extra.is_empty() {
            out.push(String::new());
        }
        out.push(" Security guide".into());
        out.push(String::new());
        out.push(format!(" {}No, exit", mark(focus == Focus::Exit, "❯")));
        out.push(format!(
            " {}Yes, I trust this folder",
            mark(focus == Focus::Trust, "❯")
        ));
        out.push(String::new());
        out.push(" Enter to confirm · Esc to cancel".into());
        out
    }

    /// Antigravity 1.5.9's screen as a real pane showed it (16 Nov 2026),
    /// at 120 columns, with the model it was set to.
    pub fn antigravity(folder: &str, focus: Focus) -> Vec<String> {
        antigravity_with(folder, focus, Some("Gemini 4 Carbon · medium"))
    }

    /// `status`: the row 1.5.9 draws under its hint, at the right edge.
    /// `None`: the screen as Google's codelab shows 1.1.27, without it.
    pub fn antigravity_with(folder: &str, focus: Focus, status: Option<&str>) -> Vec<String> {
        let mut out = vec![
            "Accessing workspace:".into(),
            String::new(),
            folder.to_string(),
            String::new(),
            "Do you trust the contents of this project?".into(),
            String::new(),
            "Antigravity CLI requires permission to read, edit, and execute files here.".into(),
            String::new(),
            format!(
                "{}Yes, I trust this folder",
                mark(focus == Focus::Trust, ">")
            ),
            format!("{}No, exit", mark(focus == Focus::Exit, ">")),
            String::new(),
            "  ↑/↓ Navigate · enter Confirm".into(),
        ];
        if let Some(status) = status {
            out.push(format!("{status:>120}"));
        }
        out
    }

    /// Codex's review of new hooks, from the words in 0.169.0's program
    /// text. Not a folder's trust: it is here so that the tests can show
    /// it is never read as one.
    pub fn codex_hooks() -> Vec<String> {
        vec![
            "Hooks need review".into(),
            "1 hook is new or changed.".into(),
            "Hooks can run outside the sandbox after you trust them.".into(),
            String::new(),
            "› 1. Review hooks".into(),
            "  2. Trust all and continue".into(),
            "  3. Continue without trusting (hooks won't run)".into(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders(f: &str) -> Vec<String> {
        vec![f.to_string()]
    }

    /// The owner's rule (16 Nov 2026): only the program's own screen, word
    /// for word, is answered on a standing choice. The recording is the
    /// real Claude Code 2.1.323, replayed through spandeck's own terminal.
    #[test]
    fn the_recorded_claude_code_screen_is_read_exactly_and_its_own_choice_is_to_leave() {
        let bytes = include_bytes!("../../terminal/recordings/claude-code-tui.110x36.bin");
        // Up to the first key the recording pressed: the screen as it
        // first stands.
        let first = bytes
            .windows(5)
            .position(|w| w == b"\x1b[>0q")
            .expect("the recording's first frame");
        let lines = replayed(&bytes[..first], 110, 36);
        let folder = "/tmp/claude-1000/-home-user00-code-mterm/f79fb1f0-280a-43b3-9357-1471bf3cdc3f/scratchpad/rec2/demo2";
        assert_eq!(
            read(AgentKind::Claude, &lines, &folders(folder)),
            Some(Seen {
                level: Level::Exact,
                focus: Some(Focus::Exit)
            }),
            "{lines:#?}"
        );
        // What this file draws for the stand-in agent is that screen.
        let drawn = drawn::claude(folder, Focus::Exit, 110, &[]);
        let text = |l: &[String]| {
            l.iter()
                .map(|l| l.trim_end().to_string())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
        };
        assert_eq!(text(&lines), text(&drawn));
        // Named for another folder, it is not the pane's own prompt.
        assert_eq!(
            read(AgentKind::Claude, &lines, &folders("/somewhere/else"))
                .unwrap()
                .level,
            Level::Choices
        );
    }

    /// A terminal small enough for this test: the recording's first frame
    /// only moves the cursor by column and by line.
    fn replayed(bytes: &[u8], cols: usize, rows: usize) -> Vec<String> {
        let text = String::from_utf8_lossy(bytes);
        let mut grid = vec![vec![' '; cols]; rows];
        let (mut r, mut c) = (0usize, 0usize);
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\r' => c = 0,
                '\n' => r += 1,
                '\x1b' => match chars.next() {
                    Some('[') => {
                        let mut arg = String::new();
                        let mut fin = ' ';
                        for x in chars.by_ref() {
                            if x.is_ascii_alphabetic() {
                                fin = x;
                                break;
                            }
                            arg.push(x);
                        }
                        if fin == 'G' {
                            c = arg.parse::<usize>().unwrap_or(1) - 1;
                        }
                    }
                    Some(']') => {
                        // A link's address: up to the bell.
                        for x in chars.by_ref() {
                            if x == '\x07' {
                                break;
                            }
                        }
                    }
                    _ => {}
                },
                ch => {
                    grid[r][c] = ch;
                    c += 1;
                }
            }
        }
        grid.into_iter()
            .map(|l| l.into_iter().collect::<String>())
            .collect()
    }

    /// The rows of a real Antigravity 1.5.9 pane at its trust question
    /// (`pane.text`, 32 rows, 16 Nov 2026), each on the row it was on.
    fn real_antigravity(status: &str) -> Vec<String> {
        let mut rows = vec![String::new(); 32];
        for (at, text) in [
            (0, "Accessing workspace:"),
            (2, "/tmp/sd-trust-2687471/a1"),
            (4, "Do you trust the contents of this project?"),
            (
                6,
                "Antigravity CLI requires permission to read, edit, and execute files here.",
            ),
            (8, "> Yes, I trust this folder"),
            (9, "  No, exit"),
            (11, "  ↑/↓ Navigate · enter Confirm"),
            (12, status),
        ] {
            rows[at] = text.to_string();
        }
        rows
    }

    /// Found with the real Antigravity 1.5.9 (16 Nov 2026): a ticked
    /// folder was not answered, because the program draws its model at
    /// the right edge of the row under its hint and the screen was held to
    /// end at the hint. That one row is the program's own; anything else
    /// under the hint is still not the screen.
    #[test]
    fn the_real_antigravity_screen_with_its_status_row_is_read_exactly() {
        let f = folders("/tmp/sd-trust-2687471/a1");
        let exact = Some(Seen {
            level: Level::Exact,
            focus: Some(Focus::Trust),
        });
        let seen = "                                                                                                Gemini 4 Carbon · medium";
        assert_eq!(
            read(AgentKind::Antigravity, &real_antigravity(seen), &f),
            exact
        );
        // Another model, another effort: the row is known by its shape.
        let other = format!("{:>120}", "Gemini 3.8 Flash (Medium) · high");
        assert_eq!(
            read(AgentKind::Antigravity, &real_antigravity(&other), &f),
            exact
        );
        // What this file draws for the stand-in agent is that screen.
        let text = |l: &[String]| {
            l.iter()
                .map(|l| l.trim_end().to_string())
                .collect::<Vec<_>>()
        };
        let drawn = drawn::antigravity("/tmp/sd-trust-2687471/a1", Focus::Trust);
        assert_eq!(text(&real_antigravity(seen))[..13], text(&drawn)[..]);

        // Everything else under the hint leaves the two choices readable
        // (the owner's one tap) and is never exact.
        let choices = Some(Seen {
            level: Level::Choices,
            focus: Some(Focus::Trust),
        });
        let level = |rows: &[String]| read(AgentKind::Antigravity, rows, &f);
        for odd in [
            // At the left edge: text, a prompt, a choice.
            "Gemini 4 Carbon · medium".to_string(),
            "> ".to_string(),
            format!("{:>120}", "> Yes, I trust this folder"),
            format!("{:>120}", "Allow calling this tool?"),
            format!("{:>120}", "1. Yes"),
            format!(
                "{:>130}",
                "a row that is far too long to be the name of a model and its effort"
            ),
        ] {
            let got = level(&real_antigravity(&odd));
            assert_ne!(got, exact, "{odd:?}");
            assert!(got.is_none_or(|s| s.level < Level::Exact), "{odd:?}");
        }
        // Not on the row straight under the hint.
        let mut apart = real_antigravity("");
        apart[13] = seen.to_string();
        assert_ne!(level(&apart), exact);
        // Two rows under the hint.
        let mut two = real_antigravity(seen);
        two[13] = seen.to_string();
        assert_ne!(level(&two), exact);
        // The status row does not make another folder's screen, or a
        // reworded one, exact; its choices are still readable.
        assert_eq!(
            read(
                AgentKind::Antigravity,
                &real_antigravity(seen),
                &folders("/elsewhere")
            ),
            choices
        );
        let mut reworded = real_antigravity(seen);
        reworded[4] = "Do you trust the files in this folder?".into();
        assert_eq!(level(&reworded), choices);
        // Claude Code draws no such row: one under its hint is not its screen.
        let mut claude = drawn::claude("/x", Focus::Trust, 110, &[]);
        claude.push(seen.to_string());
        assert_ne!(
            read(AgentKind::Claude, &claude, &folders("/x")).map(|s| s.level),
            Some(Level::Exact)
        );
    }

    /// The boundary: what is and is not the question, per program.
    #[test]
    fn only_a_programs_own_whole_screen_for_the_panes_folder_is_exact() {
        let f = "/home/me/code/new";
        let exact = |focus| {
            Some(Seen {
                level: Level::Exact,
                focus: Some(focus),
            })
        };
        // Each program's screen, at more than one width, and both ways
        // round.
        for cols in [60, 110, 200] {
            for focus in [Focus::Exit, Focus::Trust] {
                let lines = drawn::claude(f, focus, cols, &[]);
                assert_eq!(read(AgentKind::Claude, &lines, &folders(f)), exact(focus));
            }
        }
        for focus in [Focus::Exit, Focus::Trust] {
            // 1.1.27 as the codelab shows it, without the status row.
            assert_eq!(
                read(
                    AgentKind::Antigravity,
                    &drawn::antigravity_with(f, focus, None),
                    &folders(f)
                ),
                exact(focus)
            );
            let lines = drawn::antigravity(f, focus);
            assert_eq!(
                read(AgentKind::Antigravity, &lines, &folders(f)),
                exact(focus)
            );
            // One program's screen in another program's pane is not it.
            assert_eq!(
                read(AgentKind::Claude, &lines, &folders(f)).map(|s| s.level),
                Some(Level::Mentioned)
            );
        }
        // A path too long for the line is the same path.
        let long = format!("/home/me/{}", "deep/".repeat(40));
        let mut lines = drawn::antigravity(&long, Focus::Trust);
        let (a, b) = long.split_at(70);
        lines.splice(2..3, [a.to_string(), b.to_string()]);
        assert_eq!(
            read(AgentKind::Antigravity, &lines, &folders(&long)),
            exact(Focus::Trust)
        );

        let level = |kind, lines: &[String]| read(kind, lines, &folders(f)).map(|s| s.level);
        // Claude Code saying what the folder's settings would switch on:
        // the owner reads that; it is never answered for them.
        let warned = drawn::claude(
            f,
            Focus::Exit,
            110,
            &[
                "This folder pre-approves 3 tool permissions in .claude/settings.json:",
                "  Bash(curl *), Bash(sh *), WebFetch",
                "These will apply without asking. Only proceed if you trust this configuration.",
            ],
        );
        assert_eq!(level(AgentKind::Claude, &warned), Some(Level::Choices));
        // A version that words the question differently.
        let mut reworded = drawn::antigravity(f, Focus::Trust);
        reworded[4] = "Do you trust the files in this folder?".into();
        assert_eq!(
            level(AgentKind::Antigravity, &reworded),
            Some(Level::Choices)
        );
        // Something printed before it, or after it: not the start screen.
        let mut after = drawn::antigravity(f, Focus::Trust);
        after.push("> ".into());
        assert_eq!(
            level(AgentKind::Antigravity, &after),
            Some(Level::Mentioned)
        );
        let mut before = vec!["I ran the installer; it printed:".to_string()];
        before.extend(drawn::antigravity(f, Focus::Trust));
        assert_eq!(level(AgentKind::Antigravity, &before), Some(Level::Choices));
        // Both choices marked, or neither: unreadable.
        let mut both = drawn::antigravity(f, Focus::Trust);
        both[9] = "> No, exit".into();
        assert_eq!(level(AgentKind::Antigravity, &both), Some(Level::Mentioned));
        // The words in an agent's reply are not the screen.
        let said: Vec<String> = [
            "● The prompt you saw says \"Yes, I trust this folder\" and \"No, exit\".",
            "",
            "❯ ",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(level(AgentKind::Claude, &said), None);
        // Codex's review of hooks is not a folder's trust, for any program.
        for kind in [AgentKind::Codex, AgentKind::Claude, AgentKind::Antigravity] {
            assert_eq!(level(kind, &drawn::codex_hooks()), None);
        }
        // Codex's own folder question has no captured shape: it is only
        // ever mentioned, and so never answered.
        let codex: Vec<String> = ["Trust this folder?", "Trust and continue"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(level(AgentKind::Codex, &codex), Some(Level::Mentioned));
        // A permission prompt.
        let perm: Vec<String> = ["Do you want to proceed?", "❯ 1. Yes", "  2. No"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(level(AgentKind::Claude, &perm), None);
    }
}
