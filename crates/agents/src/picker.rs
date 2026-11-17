//! A list a program draws on its terminal to choose from (DECISIONS.md, "No
//! prompt on the terminal is invisible in Chat", 17 Nov 2026).
//!
//! The owner: "When I type /model in Claude it gives me a list to pick
//! from, I see that list in terminal but not in chat ... Sometime an agent
//! might give a multiple choice picker for questions it has, I'm worried
//! that these won't show up in chat like the model command". Such a list
//! is drawn by the program and is in no session file, so a conversation
//! read from that file shows nothing of it.
//!
//! This reads the bottom of a pane's screen as such a list **by its
//! structure, not its words**, so that a list nobody has captured is still
//! read. The structure, from the screens captured from the real Claude
//! Code 2.1.326 on 17 Nov 2026 (`/model`, `/config`, `/resume`,
//! `/permissions`: the fixtures below, the session titles of `/resume`
//! replaced with made-up ones) and the ones already in the repository
//! (the folder-trust question, "Switch model?"):
//!
//! - the screen ends with **a row of key names** ("Enter to set as default
//!   · s to use this session only · Esc to cancel"), which may be two rows;
//! - above it, rows one under another in one column are the choices, and
//!   **exactly one carries the program's marker** (`❯`, `›`), set in from
//!   the left edge (the marker at the very edge is the program's own
//!   message box, never a list);
//! - a choice may be numbered ("1."), may have a second column (a value, a
//!   description), a row of words under it, or be two rows with an empty
//!   row between entries; `✔` marks the one in use and `▸` one that opens
//!   another list;
//! - "↑ N more" / "↓ N more" say the list goes on; a row beginning `⌕` is a
//!   field to type a filter in; a few rows may stand between the list and
//!   the key names (the effort row of `/model`);
//! - the rows of words above, up to the program's rule, are its title.
//!
//! A screen that ends with key names and has no marked row (`/permissions`,
//! whose focus begins on its tabs) is read as "showing something", with no
//! choices. Nothing here presses a key.
//!
//! Not captured, so known only as far as this structure goes: any list of
//! Codex, Antigravity (whose marker is `>`, at the edge: not read here) or
//! OpenCode.

use serde::{Deserialize, Serialize};

use crate::foldertrust::clean;

/// One row to choose.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub text: String,
    /// Its second column, or the rows of words under it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The program's marker (its cursor) is on this row.
    pub marked: bool,
    /// Where several may be chosen: whether this one is ticked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    /// The program's own number for it ("3.").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u32>,
    /// The program marks it as the one in use (`✔`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub current: bool,
    /// It opens another list (`▸`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub submenu: bool,
}

/// What kind of thing the list decides, as far as its words say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// A choice among things: a model, a setting, a session.
    Plain,
    /// It grants something: trust in a folder or in hooks, a permission,
    /// a sign-in, a way round the permissions. Never answered from a
    /// friendly row: the owner decides with the whole screen in view.
    Grants,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Picker {
    /// The first row of words above the list.
    pub title: String,
    /// The rows of words under the title.
    pub about: String,
    /// The title row's parts, when it is several set apart by blanks
    /// ("Settings  Status   Config"): the program's tabs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<String>,
    /// Empty: the program shows something with key names under it and no
    /// marked row.
    pub choices: Vec<Choice>,
    /// Several may be ticked (`[x]`, `[ ]`).
    pub multi: bool,
    /// The program says there are rows above, or below, what it shows.
    pub more_above: bool,
    pub more_below: bool,
    /// Rows between the list and the key names.
    pub footer: Vec<String>,
    /// The row of key names under the list.
    pub hint: String,
    /// A field to type a filter in, with the words it shows.
    pub free_text: Option<String>,
    pub class: Class,
    /// The structure is whole: choices in one column, exactly one marked.
    /// Without that the deck offers no rows, only the screen.
    pub exact: bool,
}

const MARKERS: [char; 4] = ['❯', '›', '▶', '>'];
const MAX_CHOICES: usize = 40;

/// Words that make a screen one that grants something. Compared without
/// regard to case, in its title, its tabs and its choices.
const GRANTS: &[&str] = &[
    "trust",
    "allow",
    "permission",
    "bypass",
    "dangerous",
    "log in",
    "login",
    "sign in",
    "sign-in",
    "api key",
    "do you want to proceed",
    "do you want to make this edit",
    "do you want to create",
    "would you like to proceed",
    "approve",
    "deny",
    "hooks need review",
];

/// A row of key names: what a program writes under a list.
fn is_hint(row: &str) -> bool {
    let low = row.to_lowercase();
    let keys = [
        "enter", "esc", "↑", "↓", "space", "tab", "navigate", "confirm", "select", "cancel",
        "ctrl+",
    ];
    let named = keys.iter().filter(|k| low.contains(*k)).count();
    if low.contains("interrupt") {
        return false;
    }
    if named >= 3 && row.chars().count() <= 200 {
        return true;
    }
    row.chars().count() <= 200
        && low.contains(" to ")
        && (named >= 2 || (named >= 1 && row.matches(" · ").count() >= 2))
        || (named >= 2 && row.contains(" · "))
}

/// "↑ 3 more", "↓ 31 more": the program's word that its list goes on.
/// `Some(true)`: below.
fn more(text: &str) -> Option<bool> {
    let low = text.to_lowercase();
    let rest = text.strip_prefix(['↑', '↓', '▲', '▼', '…'])?;
    (text.chars().count() <= 30 && (rest.trim().is_empty() || low.ends_with("more")))
        .then_some(!text.starts_with(['↑', '▲']))
}

struct Row {
    /// The column its words begin in; for a marked row, after its marker.
    indent: usize,
    marked: bool,
    /// Its marker is at the very left edge: where a program's own message
    /// box has its mark too.
    edge: bool,
    checked: Option<bool>,
    text: String,
}

fn row(line: &str) -> Row {
    let line: String = line
        .chars()
        .filter(|c| !('\u{2500}'..='\u{257f}').contains(c))
        .collect();
    let line = line.trim_end();
    let mut indent = line.chars().take_while(|c| *c == ' ').count();
    let mut rest = line.trim_start();
    let mut marked = false;
    let mut edge = false;
    if let Some(after) = rest.strip_prefix(MARKERS) {
        if after.starts_with(' ') {
            marked = true;
            edge = indent == 0;
            indent += 1 + after.chars().take_while(|c| *c == ' ').count();
            rest = after.trim_start();
        }
    }
    let mut checked = None;
    for (mark, on) in [
        ("[x]", true),
        ("[X]", true),
        ("[✓]", true),
        ("[ ]", false),
        ("◉", true),
        ("◯", false),
        ("☑", true),
        ("☐", false),
    ] {
        if let Some(after) = rest.strip_prefix(mark) {
            if after.starts_with(' ') {
                checked = Some(on);
                rest = after.trim_start();
                break;
            }
        }
    }
    Row {
        indent,
        marked,
        edge,
        checked,
        text: rest.to_string(),
    }
}

/// A choice's words: its number, the mark of the one in use, the mark of
/// a list behind it, and its second column.
fn choice(r: &Row) -> Choice {
    let mut c = Choice {
        marked: r.marked,
        checked: r.checked,
        ..Default::default()
    };
    let mut text = r.text.as_str();
    if let Some((n, rest)) = text.split_once('.') {
        if !n.is_empty()
            && n.len() <= 2
            && n.bytes().all(|b| b.is_ascii_digit())
            && rest.starts_with(' ')
        {
            c.number = n.parse().ok();
            text = rest.trim_start();
        }
    }
    if let Some(rest) = text.strip_prefix('▸') {
        c.submenu = true;
        text = rest.trim_start();
    }
    let (name, detail) = match text.find("  ") {
        Some(at) => (&text[..at], text[at..].trim()),
        None => (text, ""),
    };
    let name = match name.strip_suffix('✔') {
        Some(n) => {
            c.current = true;
            n.trim_end()
        }
        None => name,
    };
    let (name, detail) = match (name.strip_suffix("(current)"), detail) {
        (Some(n), _) => {
            c.current = true;
            (n.trim_end(), detail)
        }
        (None, "(current)") => {
            c.current = true;
            (name, "")
        }
        _ => (name, detail),
    };
    c.text = name.to_string();
    c.detail = detail.to_string();
    c
}

/// Read the bottom of a screen as a list to choose from, as something
/// shown with key names under it, or as neither.
pub fn read(lines: &[String]) -> Option<Picker> {
    let mut end = lines.iter().rposition(|l| !clean(l).is_empty())? + 1;
    // (A program's status row at the right edge, under everything, as
    // Antigravity draws its model: not part of what is shown.)
    if status_row(&lines[end - 1]) {
        end = lines[..end - 1]
            .iter()
            .rposition(|l| !clean(l).is_empty())?
            + 1;
    }
    let lines = &lines[..end];
    // The key names: the last row, and the row above when they wrap.
    let mut at = lines.len() - 1;
    let mut hint = clean(&lines[at]);
    if !is_hint(&hint) {
        return None;
    }
    if at > 0 && is_hint(&clean(&lines[at - 1])) {
        at -= 1;
        hint = format!("{} {hint}", clean(&lines[at]));
    }
    let rows: Vec<Row> = lines[..at].iter().map(|l| row(l)).collect();
    let blank = |i: usize| rows[i].text.is_empty();
    // Nothing marked, or a mark at the edge that is no list (the
    // program's own message box, a line of its history): what it shows
    // with key names under it, with no rows.
    let shown = |hint: String| -> Option<Picker> {
        // Nothing is marked. A screen the program says Esc leaves is
        // something it is showing; anything else is not read.
        let low = hint.to_lowercase();
        if !low.contains("esc") && !low.contains("enter") {
            return None;
        }
        let (title, about, free_text, tabs) = title_above(lines, at, false);
        let class = class_of(&format!("{title} {about}"));
        Some(Picker {
            tabs,
            title,
            about,
            choices: vec![],
            multi: false,
            more_above: false,
            more_below: false,
            footer: vec![],
            hint,
            free_text,
            class,
            exact: false,
        })
    };
    let Some(marker_at) = rows.iter().rposition(|r| r.marked) else {
        return shown(hint);
    };
    let edge = rows[marker_at].edge;
    let list = (|| -> Option<Picker> {
        let hint = hint.clone();
        let column = rows[marker_at].indent;
        let (mut more_above, mut more_below, mut grouped) = (false, false, false);
        let part = |i: usize| {
            !blank(i)
                && !rows[i].marked
                && rows[i].indent >= column
                && more(&rows[i].text).is_none()
        };
        let mut first = marker_at;
        while first > 0 {
            let i = first - 1;
            if part(i) {
                first = i;
            } else if blank(i) && i > 0 && part(i - 1) && !rows[marker_at].edge {
                grouped = true;
                first = i - 1;
            } else {
                more_above = !blank(i) && more(&rows[i].text) == Some(false);
                break;
            }
        }
        let mut last = marker_at;
        while last + 1 < rows.len() {
            let i = last + 1;
            if part(i) {
                last = i;
            } else if blank(i) && i + 1 < rows.len() && part(i + 1) && !rows[marker_at].edge {
                grouped = true;
                last = i + 1;
            } else {
                break;
            }
        }
        // Under the list: its "more" row, and a few rows before the key names.
        let mut footer = vec![];
        for (n, r) in rows.iter().enumerate().skip(last + 1) {
            if r.marked {
                return None;
            }
            if r.text.is_empty() {
                continue;
            }
            if n == last + 1 && more(&r.text) == Some(true) {
                more_below = true;
            } else {
                footer.push(r.text.clone());
            }
        }
        if footer.len() > 4 {
            return None;
        }
        let mut choices: Vec<Choice> = vec![];
        let mut fresh = true;
        for r in &rows[first..=last] {
            if r.text.is_empty() {
                fresh = true;
                continue;
            }
            // An entry begins after an empty row where entries are set apart
            // by them, and at the list's own column where they are not.
            let begins = if grouped {
                fresh
            } else {
                r.indent == column || choices.is_empty()
            };
            fresh = false;
            if begins {
                choices.push(choice(r));
            } else if let Some(c) = choices.last_mut() {
                if !c.detail.is_empty() {
                    c.detail.push(' ');
                }
                c.detail.push_str(&r.text);
            }
        }
        if choices.len() < 2
            || choices.len() > MAX_CHOICES
            || choices.iter().filter(|c| c.marked).count() != 1
        {
            return None;
        }
        if choices
            .iter()
            .any(|c| c.text.is_empty() || c.text.chars().count() > 160)
        {
            return None;
        }
        let (title, about, free_text, tabs) =
            title_above(lines, first.saturating_sub(usize::from(more_above)), true);
        let multi = choices.iter().all(|c| c.checked.is_some());
        if !multi {
            choices.iter_mut().for_each(|c| c.checked = None);
        }
        let said = format!(
            "{title} {about} {}",
            choices
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        Some(Picker {
            tabs,
            title,
            about,
            choices,
            multi,
            more_above,
            more_below,
            footer,
            hint,
            free_text,
            class: class_of(&said),
            exact: true,
        })
    })();
    match list {
        None if edge => shown(hint),
        other => other,
    }
}

/// A short row set against the right edge, with nothing that could be a
/// choice or a question in it: a program's status row.
fn status_row(line: &str) -> bool {
    let text = line.trim();
    let before = line.chars().take_while(|c| *c == ' ').count();
    let len = text.chars().count();
    (1..=40).contains(&len) && before >= len && before >= 20 && !text.contains(['>', '❯', '›', '?'])
}

/// Is a program showing something of its own that is not its ordinary
/// wait for a message, though nothing on it can be read as a list? Only
/// what is known from a real screen of that program:
///
/// - **Antigravity 1.5.9** (captured 17 Nov 2026): its message box is a
///   row `>` between two rules, and when it waits for a message nothing
///   is under the box but its status row. Anything else under it is
///   something it shows.
/// - **OpenCode 2.3.0** (captured the same day): its windows have their
///   title on a row that ends, far to the right, with `esc`.
///
/// Claude Code's and Codex's waiting screens are not captured, so for
/// them this says nothing: their lists are read by structure (`read`).
pub fn showing(agent: crate::AgentKind, lines: &[String]) -> bool {
    use crate::AgentKind;
    let Some(end) = lines.iter().rposition(|l| !clean(l).is_empty()) else {
        return false;
    };
    match agent {
        AgentKind::Antigravity => {
            let rule =
                |l: &String| l.trim().chars().count() >= 20 && l.trim().chars().all(|c| c == '─');
            let boxed = (1..end).rev().find(|i| {
                (lines[*i].trim_end() == ">" || lines[*i].starts_with("> "))
                    && rule(&lines[i - 1])
                    && lines.get(i + 1).is_some_and(rule)
            });
            // The row straight under the box is the program's own: its
            // status at the right, and when it waits, a hint at the left on
            // the same row ("? for shortcuts", "esc to cancel"). Found with
            // the real 1.5.10, 17 Nov 2026: an idle pane was taken for one
            // showing something, and messages to it were held.
            let own_row = |l: &String| {
                status_row(l) || {
                    let t = l.trim_end();
                    let gap = t.find("          ");
                    gap.is_some_and(|g| {
                        let (left, right) = (t[..g].trim(), t[g..].trim());
                        !left.is_empty()
                            && left.chars().count() <= 32
                            && !right.is_empty()
                            && right.chars().count() <= 40
                            && !right.contains("     ")
                    })
                }
            };
            match boxed {
                Some(i) => lines[i + 2..=end].iter().enumerate().any(|(n, l)| {
                    !clean(l).is_empty() && !(n == 0 && own_row(l)) && !status_row(l)
                }),
                None => false,
            }
        }
        AgentKind::OpenCode => lines[..=end].iter().any(|l| {
            let t = l.trim_end();
            t.ends_with("   esc")
                && t.trim_start()
                    .chars()
                    .next()
                    .is_some_and(char::is_alphabetic)
        }),
        _ => false,
    }
}

/// Is the bottom of the screen a program's ordinary wait for a message?
/// `None`: this program's waiting screen is not known. From the real
/// screens captured on 17 Nov 2026, empty and with words typed in:
///
/// - **Claude Code 2.1.326**: its message box is a row beginning `❯`
///   between two rules, with its mode row under it. (What it draws under
///   the box while a command is being typed is still its wait.)
/// - **Codex 0.169.0**: a row beginning `› ` at the left edge, and under
///   it at most its two status rows.
///
/// When a pane that has shown this once shows something else while it is
/// not in a turn, the program is showing something of its own (the
/// station, picker.rs). Antigravity and OpenCode are told by `showing`.
pub fn waiting(agent: crate::AgentKind, lines: &[String]) -> Option<bool> {
    use crate::AgentKind;
    let rows: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    let tail = &rows[rows.len().saturating_sub(14)..];
    Some(match agent {
        AgentKind::Claude => {
            let rule =
                |l: &&String| l.trim().chars().count() >= 20 && l.trim().chars().all(|c| c == '─');
            // The bottom-most rule that closes a box whose first row
            // begins with the program's mark.
            (1..tail.len()).rev().any(|close| {
                rule(&tail[close])
                    && (0..close)
                        .rev()
                        .take(12)
                        .find(|i| rule(&tail[*i]))
                        .is_some_and(|open| open + 1 < close && tail[open + 1].starts_with('❯'))
            })
        }
        AgentKind::Codex => tail
            .iter()
            .rposition(|l| l.starts_with("› ") || l.as_str() == "›")
            .is_some_and(|at| tail.len() - at - 1 <= 3),
        _ => return None,
    })
}

fn class_of(said: &str) -> Class {
    let said = said.to_lowercase();
    if GRANTS.iter().any(|w| said.contains(w)) {
        Class::Grants
    } else {
        Class::Plain
    }
}

/// The rows of words above row `top`, up to the program's rule: the
/// title, what is said under it, and a field to type in.
/// `nearest`: only the rows of words nearest above (to the first empty
/// row over them): a list's own title, where the program draws no rule.
fn title_above(
    lines: &[String],
    top: usize,
    nearest: bool,
) -> (String, String, Option<String>, Vec<String>) {
    let mut words: Vec<String> = vec![];
    let mut tabs: Vec<String> = vec![];
    let mut field = None;
    for line in lines[..top].iter().rev().take(12) {
        // A rule: the edge of what the program is showing.
        if line.trim_start().starts_with(['▔', '▁', '━', '═']) {
            break;
        }
        let text = clean(line);
        if text.is_empty() {
            if nearest && !words.is_empty() {
                break;
            }
            continue;
        }
        if let Some(shown) = text.strip_prefix('⌕') {
            field = Some(shown.trim().to_string());
            continue;
        }
        if matches!(text.as_str(), "Search" | "Search:") {
            field = Some("Search".to_string());
            continue;
        }
        if !text.chars().any(char::is_alphanumeric) || words.len() == 4 {
            break;
        }
        tabs = text
            .split("  ")
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect();
        words.insert(0, text.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    let title = words.first().cloned().unwrap_or_default();
    let about = words.get(1..).map(|w| w.join(" ")).unwrap_or_default();
    if tabs.len() < 3 || tabs.iter().any(|t| t.chars().count() > 24) {
        tabs.clear();
    }
    (title, about, field, tabs)
}

/// A list as a program draws one, for the stand-in agent and the tests,
/// in the layout of Claude Code's own. `window`: how many it shows at a
/// time, from `top`. A choice: its words, its second column, its tick.
pub fn drawn(
    title: &str,
    choices: &[(String, String, Option<bool>)],
    marked: usize,
    top: usize,
    window: usize,
    hint: &str,
) -> Vec<String> {
    let mut out = vec![
        "▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔".to_string(),
        format!("   {title}"),
        String::new(),
    ];
    let end = (top + window).min(choices.len());
    if top > 0 {
        out.push(format!("     ↑ {top} more"));
    }
    let wide = choices
        .iter()
        .map(|c| c.0.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    for (i, (text, detail, checked)) in choices.iter().enumerate().take(end).skip(top) {
        let tick = match checked {
            Some(true) => "[x] ",
            Some(false) => "[ ] ",
            None => "",
        };
        let row = format!(
            "   {} {tick}{text:<wide$}{detail}",
            if i == marked { "❯" } else { " " }
        );
        out.push(row.trim_end().to_string());
    }
    if end < choices.len() {
        out.push(format!("     ↓ {} more", choices.len() - end));
    }
    out.push(String::new());
    out.push(format!("   {hint}"));
    out
}

/// Claude Code 2.1.326's own screens, as captured on 17 Nov 2026 in a
/// pane of 120 columns: the rows from its rule down. (Of `/resume`, the
/// sessions' titles are made up; everything else is as it was.)
pub mod real {
    pub const MODEL: &str = "▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔ tmux focus-events off · add 'set -g focus-events on' to ~/.tmux.conf and reattach for focus tracking ▔
   Select model
   Switch between Claude models. Your pick becomes the default for new sessions. For other or older models, use
   --model.

     1.  Default (recommended)  Opus 5.5 · Best for everyday, complex tasks
     2.  Opus 5.5               For complex work and everyday tasks
     3.  Fable 5.2              For your toughest challenges
     4.  Sonnet 5.5             Most efficient for simpler tasks
   ❯ 5.  Haiku 5.5 ✔            Fastest for quick answers
     ▸   More models

   ◐ Medium effort (default) ←/→ to adjust

   Use /fast to turn on fast mode (Opus 5.5).

   Enter to set as default · s to use this session only · Esc to cancel";
    pub const CONFIG: &str = "  ⎿  Kept model as Haiku 5.5
▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔
   Settings  Status   Config   Usage   Stats

   ╭──────────────────────────────────────╮
   │ ⌕ Search settings…                   │
   ╰──────────────────────────────────────╯

   ❯ Auto-compact                                    on
     Idle compaction                                 on
     Switch models when a message is flagged         Ask each time ›
     Thinking mode                                   on
     Thinking can't be turned off for Haiku 5.5
     Session recap                                   on
     Dynamic workflow size                           medium (default)
     ↓ 31 more

   Type to filter · Enter/↓ to select · ↑ to tabs · Esc to clear";
    pub const RESUME: &str = "▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔
   Resume session (1 of 18)
   ╭──────────────────────────────────────╮
   │ ⌕ Search…                            │
   ╰──────────────────────────────────────╯
     marsh

   ❯ Heron counts for the marsh survey  ⧉ 2
     28 seconds ago · main · 86.5MB

     Fix the tally script
     12 hours ago · main · 411.7KB

     Reed beds, east bank
     1 day ago · main · 621KB
     ↓ 13 more


     Ctrl+A to show all projects · Ctrl+B to only show current branch · Ctrl+W to show all worktrees · Space to";
    /// Codex 0.169.0, `/model`: its marker is at the left edge.
    pub const CODEX_MODEL: &str = "  Select Model and Effort


› 1. GPT-6.1-Sol (current)  Latest workhorse model for coding and everyday work.
  2. GPT-6-Astra            Frontier intelligence for the most demanding work.
  3. GPT-6.1 Terra          GPT-6.1 Terra Codex model.
  4. GPT-6-Sol              Previous generation workhorse model.
  5. GPT-6-Luna             Fast and affordable model for easier tasks.
  6. GPT-5.6-Sol            Older generation workhorse model.
  7. GPT-5.6-Terra          Older balanced model for straightforward work.
  8. GPT-5.6-Luna           Older fast and efficient model.

  enter select · esc back";
    /// Antigravity 1.5.9, `/model`, from its message box down.
    pub const ANTIGRAVITY_MODEL: &str = "────────────────────────────────────────────────────────────
>
────────────────────────────────────────────────────────────
Switch Model

  Search:
          ────────────────────

> Gemini 4 Carbon       (current)
  Gemini 4 Argon
  Gemini 4 Hydrogen
  Orchestrator (Carbon)
  Orchestrator (Argon)
  Gemini 3.8 Flash
  Claude Opus 5.5
  Claude Sonnet 5.5
  GPT-OSS 120B (Medium)

  Effort  ◂        ●━━━━━━━━━━━━━━◉──────────────○        ▸
                  low          medium          high
            Balanced speed and reasoning quality for most tasks

Keyboard: ↑/↓ Navigate  ←/→ Effort  enter Select  esc Go Back

                                                                                                Gemini 4 Carbon · medium";
    /// Antigravity 1.5.9 waiting for a message: the same box, and its
    /// status row. (The words typed in it are made up.)
    pub const ANTIGRAVITY_IDLE: &str = "  The nests are counted.

────────────────────────────────────────────────────────────
> count the egrets too
────────────────────────────────────────────────────────────

                                                                                                Gemini 4 Carbon · medium";
    /// OpenCode 2.3.0, `/models`: a window over its screen; the row in
    /// use has `●`, and its highlight is a colour, which no row carries.
    pub const OPENCODE_MODELS: &str = "                                  Select model                                     esc

                                  Search

                                  Big Pickle                                      Free
                                ● DeepSeek V4.1 Flash Free                        Free
                                  GLM-5.3-Flash Free                              Free
                       ┃          Hy4 flash Free                                  Free
                       ┃  Ask     Iceman Free                                     Free
                       ╹▀▀▀▀▀▀    Flint 40B Free                                  Free    ▀▀▀▀▀▀▀▀
                       ~/code/    Nemotron 3.5 Super VL Free                      Free    commands
                                  Beam Free                                       Free

                                  View all integrations ctrl+a  Favorite ctrl+f





  ⊙ 1 MCP /mcps                                                                                                  2.3.0";
    /// Claude Code 2.1.326 waiting for a message, and with words typed
    /// in its box (the words are made up).
    pub const CLAUDE_IDLE: &str = "● Nine herons, counted.

────────────────────────────────────────────────────────────
❯\u{a0}Try \"edit tally.py to…\"
────────────────────────────────────────────────────────────
  ⏵⏵ auto mode on (shift+tab to cycle)";
    pub const CLAUDE_DRAFT: &str = "────────────────────────────────────────────────────────────
❯\u{a0}count the egrets too
────────────────────────────────────────────────────────────
  ⏵⏵ auto mode on (shift+tab to cycle)";
    /// Codex 0.169.0 waiting for a message, and with words typed in.
    pub const CODEX_IDLE: &str = "  >_ OpenAI Codex (v0.169.0)
     ~/marsh


› Ask Codex to do anything

  GPT-6.1-Sol high · ~/marsh
  ← for agents · ? for shortcuts                                                              ⚠ 2 warnings · f2 to view";
    pub const CODEX_DRAFT: &str = "› count the egrets too

  GPT-6.1-Sol high · ~/marsh
                                                                                              ⚠ 2 warnings · f2 to view";
    /// OpenCode 2.3.0 waiting for a message.
    pub const OPENCODE_IDLE: &str = "                       ┃
                       ┃  Ask anything… \"Fix broken tests\"
                       ┃
                       ┃  Build · DeepSeek V4.1 Flash Free OpenCode · high
                       ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
                       ~/marsh:main      shift+tab agents  ctrl+p commands



  ⊙ 1 MCP /mcps                                                                                                  2.3.0";
    pub const PERMISSIONS: &str = "❯ /resume
  ⎿  Resume cancelled

▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔
   Permissions  Recently blocked   Allow   Ask   Deny   Auto mode   Workspace

   Claude Code won't ask before using allowed tools.
   ╭──────────────────────────────────────╮
   │ ⌕ Search…                            │
   ╰──────────────────────────────────────╯

     Add a new rule…
     mcp__spandeck

   ←/→ to switch · ↓ to select · Esc to cancel";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real Antigravity 1.5.10, idle (17 Nov 2026): under its box is one
    /// row, a hint at the left and its status at the right. That is the
    /// program waiting for a message, and nothing it shows.
    #[test]
    fn antigravity_waiting_for_a_message_shows_nothing() {
        let rule = "\u{2500}".repeat(120);
        let screen = |under: &str| -> Vec<String> {
            vec![
                " \u{2503} Auto is now the default agent mode".to_string(),
                " \u{2503} esc to dismiss".to_string(),
                rule.clone(),
                ">".to_string(),
                rule.clone(),
                under.to_string(),
            ]
        };
        let idle = format!(
            "? for shortcuts{}Gemini 4 Carbon \u{b7} medium",
            " ".repeat(81)
        );
        assert!(!showing(crate::AgentKind::Antigravity, &screen(&idle)));
        let working = format!(
            "esc to cancel{}Gemini 4 Carbon \u{b7} medium",
            " ".repeat(83)
        );
        assert!(!showing(crate::AgentKind::Antigravity, &screen(&working)));
        // Something else under the box is still something it shows.
        let mut menu = screen("  Allow calling this tool?");
        menu.push("  > Yes".to_string());
        assert!(showing(crate::AgentKind::Antigravity, &menu));
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    /// The owner's report of 17 Nov 2026: "When I type /model in Claude it
    /// gives me a list to pick from, I see that list in terminal but not
    /// in chat". The real program's four screens are read by their
    /// structure, each as what it is.
    #[test]
    fn claude_codes_own_lists_are_read_as_captured() {
        let p = read(&lines(real::MODEL)).expect("/model");
        assert_eq!(
            (p.title.as_str(), p.exact, p.class, p.multi, p.more_below),
            ("Select model", true, Class::Plain, false, false)
        );
        assert!(
            p.about
                .starts_with("Switch between Claude models. Your pick becomes the default"),
            "{}",
            p.about
        );
        let rows: Vec<_> = p
            .choices
            .iter()
            .map(|c| (c.number, c.text.as_str(), c.marked, c.current, c.submenu))
            .collect();
        assert_eq!(
            rows,
            vec![
                (Some(1), "Default (recommended)", false, false, false),
                (Some(2), "Opus 5.5", false, false, false),
                (Some(3), "Fable 5.2", false, false, false),
                (Some(4), "Sonnet 5.5", false, false, false),
                (Some(5), "Haiku 5.5", true, true, false),
                (None, "More models", false, false, true),
            ]
        );
        assert_eq!(p.choices[1].detail, "For complex work and everyday tasks");
        assert_eq!(
            p.footer,
            [
                "◐ Medium effort (default) ←/→ to adjust",
                "Use /fast to turn on fast mode (Opus 5.5)."
            ]
        );
        assert_eq!(
            p.hint,
            "Enter to set as default · s to use this session only · Esc to cancel"
        );

        let p = read(&lines(real::CONFIG)).expect("/config");
        assert_eq!(
            (
                p.title.as_str(),
                p.free_text.as_deref(),
                p.more_below,
                p.exact
            ),
            (
                "Settings Status Config Usage Stats",
                Some("Search settings…"),
                true,
                true
            )
        );
        assert_eq!(
            (
                p.choices[0].text.as_str(),
                p.choices[0].detail.as_str(),
                p.choices[0].marked
            ),
            ("Auto-compact", "on", true)
        );
        assert!(p
            .choices
            .iter()
            .any(|c| c.text == "Session recap" && c.detail == "on"));
        assert_eq!(p.tabs, ["Settings", "Status", "Config", "Usage", "Stats"]);

        // Entries of two rows, an empty row between them.
        let p = read(&lines(real::RESUME)).expect("/resume");
        assert_eq!(
            (p.title.as_str(), p.more_below, p.free_text.as_deref()),
            ("Resume session (1 of 18)", true, Some("Search…"))
        );
        let marked = p.choices.iter().find(|c| c.marked).unwrap();
        assert_eq!(
            (marked.text.as_str(), marked.detail.as_str()),
            (
                "Heron counts for the marsh survey",
                "⧉ 2 28 seconds ago · main · 86.5MB"
            )
        );
        assert!(p
            .choices
            .iter()
            .any(|c| c.text == "Reed beds, east bank" && c.detail == "1 day ago · main · 621KB"));

        // Nothing is marked, and it is about permissions: something
        // shown, with no rows to tap.
        let p = read(&lines(real::PERMISSIONS)).expect("/permissions");
        assert_eq!(
            (p.choices.len(), p.exact, p.class),
            (0, false, Class::Grants)
        );
        assert_eq!(
            p.title,
            "Permissions Recently blocked Allow Ask Deny Auto mode Workspace"
        );
        // The screens already in the repository: lists that grant, but
        // for "Switch model?".
        use crate::foldertrust::{drawn as trust, Focus};
        let p = read(&trust::claude("/home/ada/marsh", Focus::Exit, 110, &[]))
            .expect("the folder-trust question");
        assert_eq!((p.class, p.choices.len()), (Class::Grants, 2));
        let p = read(&crate::models::switch_drawn("Sonnet 5.5", true)).expect("Switch model?");
        assert_eq!(
            (p.class, p.choices[0].number, p.choices[0].marked),
            (Class::Plain, Some(1), true)
        );
    }

    /// The owner, 17 Nov 2026: "Confirm the picker works on any agent at
    /// anytime a multiple choice would come up for any reason". It did
    /// not: Codex's and Antigravity's `/model` lists have their marker at
    /// the left edge, and OpenCode's has none. As captured from the three.
    #[test]
    fn the_other_programs_lists_are_read_as_captured() {
        use crate::AgentKind;
        let p = read(&lines(real::CODEX_MODEL)).expect("Codex /model");
        assert_eq!(
            (
                p.title.as_str(),
                p.choices.len(),
                p.exact,
                p.class,
                p.hint.as_str()
            ),
            (
                "Select Model and Effort",
                8,
                true,
                Class::Plain,
                "enter select · esc back"
            )
        );
        let c = &p.choices[0];
        assert_eq!(
            (
                c.number,
                c.text.as_str(),
                c.marked,
                c.current,
                c.detail.as_str()
            ),
            (
                Some(1),
                "GPT-6.1-Sol",
                true,
                true,
                "Latest workhorse model for coding and everyday work."
            )
        );
        assert_eq!(p.choices[7].text, "GPT-5.6-Luna");

        let p = read(&lines(real::ANTIGRAVITY_MODEL)).expect("Antigravity /model");
        assert_eq!(
            (
                p.title.as_str(),
                p.choices.len(),
                p.free_text.as_deref(),
                p.exact
            ),
            ("Switch Model", 9, Some("Search"), true)
        );
        assert_eq!(
            (
                p.choices[0].text.as_str(),
                p.choices[0].marked,
                p.choices[0].current,
                p.choices[8].text.as_str()
            ),
            ("Gemini 4 Carbon", true, true, "GPT-OSS 120B (Medium)")
        );
        assert_eq!(p.footer.len(), 3);
        assert!(p.hint.starts_with("Keyboard: ↑/↓ Navigate"), "{}", p.hint);
        // Waiting for a message, with words typed in its box: nothing.
        assert_eq!(read(&lines(real::ANTIGRAVITY_IDLE)), None);
        assert!(!showing(
            AgentKind::Antigravity,
            &lines(real::ANTIGRAVITY_IDLE)
        ));
        assert!(showing(
            AgentKind::Antigravity,
            &lines(real::ANTIGRAVITY_MODEL)
        ));

        // OpenCode's has no marker a row carries: not a list, but known
        // to be something it shows.
        assert_eq!(read(&lines(real::OPENCODE_MODELS)), None);
        assert!(showing(AgentKind::OpenCode, &lines(real::OPENCODE_MODELS)));
        assert!(!showing(
            AgentKind::OpenCode,
            &lines("  The nests are counted.\n\n  ┃  Ask anything\n\n  ⊙ 1 MCP /mcps      2.3.0")
        ));
        // Nothing is said of a program whose waiting screen is not known.
        assert!(
            !showing(AgentKind::Claude, &lines(real::MODEL))
                && !showing(AgentKind::Codex, &lines(real::CODEX_MODEL))
        );
        // Codex's own message box, its mark at the same edge: not a list.
        assert_eq!(read(&lines("  The nests are counted.\n  1. east\n  2. west\n\n› Ask Codex to do anything\n\n  gpt-6.1-sol · 97% left · ~/marsh")), None);
    }

    /// Run with the four real programs on 17 Nov 2026: each one's ordinary
    /// wait for a message, empty and with words typed in, is nothing (an
    /// idle Antigravity pane had been taken for one showing something,
    /// and every message to it was held); and what is not that wait is
    /// known not to be, for Claude Code and Codex too.
    #[test]
    fn every_programs_wait_for_a_message_is_nothing() {
        use crate::AgentKind;
        for (agent, screen) in [
            (AgentKind::Claude, real::CLAUDE_IDLE),
            (AgentKind::Claude, real::CLAUDE_DRAFT),
            (AgentKind::Codex, real::CODEX_IDLE),
            (AgentKind::Codex, real::CODEX_DRAFT),
            (AgentKind::OpenCode, real::OPENCODE_IDLE),
            (AgentKind::Antigravity, real::ANTIGRAVITY_IDLE),
        ] {
            assert_eq!(read(&lines(screen)), None, "{agent:?}\n{screen}");
            assert!(!showing(agent, &lines(screen)), "{agent:?}\n{screen}");
            if let Some(waits) = waiting(agent, &lines(screen)) {
                assert!(waits, "{agent:?}\n{screen}");
            }
        }
        // Its own lists and screens are not its wait.
        for (agent, screen) in [
            (AgentKind::Claude, real::MODEL),
            (AgentKind::Claude, real::CONFIG),
            (AgentKind::Claude, real::PERMISSIONS),
            (AgentKind::Codex, "  Hooks\n\n  Event          Active/Installed\n  PreToolUse     2/2\n  Stop           1/1"),
        ] {
            assert_eq!(waiting(agent, &lines(screen)), Some(false), "{agent:?}\n{screen}");
        }
        assert_eq!(
            waiting(AgentKind::OpenCode, &lines(real::OPENCODE_IDLE)),
            None
        );
    }

    /// The same report's other half: what is not such a list is not read
    /// as one, or the owner would be told an agent waits when it does not.
    #[test]
    fn what_is_not_a_list_to_choose_from_is_not_read_as_one() {
        let three: Vec<_> = [("Fable", "hardest"), ("Opus", "complex"), ("Haiku", "")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string(), None))
            .collect();
        let hint = "Enter to confirm · Esc to cancel";
        // A window on a longer list, where several may be ticked.
        let many: Vec<_> = (1..=9)
            .map(|n| (format!("session {n}"), String::new(), Some(n % 2 == 0)))
            .collect();
        let p = read(&drawn(
            "Pick sessions",
            &many,
            4,
            2,
            4,
            "Space to toggle · Enter to confirm · Esc to cancel",
        ))
        .unwrap();
        assert_eq!(
            (p.choices.len(), p.more_above, p.more_below, p.multi),
            (4, true, true, true)
        );
        assert_eq!(
            (
                p.choices[2].marked,
                p.choices[2].checked,
                p.choices[1].checked
            ),
            (true, Some(false), Some(true))
        );
        assert!(read(&drawn("Select", &three, 1, 0, 10, hint)).is_some());
        for not in [
            // A list in an agent's reply: no marker, no key names.
            "Here are the options:\n  1. Fable\n  2. Opus\n  3. Haiku".to_string(),
            // The program's own message box with words in it, a reply
            // above and its status row under: its mark is at the edge.
            "  The models are:\n  Fable\n  Opus\n\n❯ which one is fastest\n\n  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt · ctrl+t to hide".to_string(),
            // The same list with the message box under it: it is over.
            format!("{}\n\n❯ ", drawn("Select", &three, 1, 0, 10, hint).join("\n")),
            "He said:\n> first\n> second".to_string(),
            "| name | size |\n| a | 1 |\n| b | 2 |".to_string(),
            "@@ -1,2 +1,2 @@\n-old\n+new\n context".to_string(),
            // One choice; two marked; a list with no key names under it.
            "   Title\n\n   ❯ Only one\n\n   Enter to confirm · Esc to cancel".to_string(),
            "   Title\n\n   ❯ One\n   ❯ Two\n\n   Enter to confirm · Esc to cancel".to_string(),
            "   Resume which?\n\n   ❯ yesterday\n     last week".to_string(),
            // An agent at work: its status row names keys, nothing is marked.
            "  Counting the herons\n\n✻ Working… (esc to interrupt · ↓ 2.3k tokens · ctrl+t to hide)".to_string(),
        ] {
            assert_eq!(read(&lines(&not)), None, "{not:?}");
        }
        // Words that grant make it one that grants, whatever its shape.
        let p = read(&lines(
            "   Allow calling this tool?\n\n   ❯ Yes\n     No\n\n   ↑/↓ Navigate · enter Confirm",
        ))
        .unwrap();
        assert_eq!(p.class, Class::Grants);
    }
}
