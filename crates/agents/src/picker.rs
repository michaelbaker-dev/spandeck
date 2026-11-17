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

const MARKERS: [char; 3] = ['❯', '›', '▶'];
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
    if let Some(after) = rest.strip_prefix(MARKERS) {
        // (At the very edge it is the program's own message box.)
        if after.starts_with(' ') && indent >= 1 {
            marked = true;
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
    c.text = name.to_string();
    c.detail = detail.to_string();
    c
}

/// Read the bottom of a screen as a list to choose from, as something
/// shown with key names under it, or as neither.
pub fn read(lines: &[String]) -> Option<Picker> {
    let end = lines.iter().rposition(|l| !clean(l).is_empty())? + 1;
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
    let Some(marker_at) = rows.iter().rposition(|r| r.marked) else {
        // Nothing is marked. A screen the program says Esc leaves is
        // something it is showing; anything else is not read.
        let low = hint.to_lowercase();
        if ![
            "esc to cancel",
            "esc to clear",
            "esc to close",
            "esc to go back",
        ]
        .iter()
        .any(|w| low.contains(w))
        {
            return None;
        }
        let (title, about, free_text, tabs) = title_above(lines, at);
        let class = class_of(&format!("{title} {about}"));
        return Some(Picker {
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
        });
    };
    let column = rows[marker_at].indent;
    let (mut more_above, mut more_below, mut grouped) = (false, false, false);
    let part = |i: usize| {
        !blank(i) && !rows[i].marked && rows[i].indent >= column && more(&rows[i].text).is_none()
    };
    let mut first = marker_at;
    while first > 0 {
        let i = first - 1;
        if part(i) {
            first = i;
        } else if blank(i) && i > 0 && part(i - 1) {
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
        } else if blank(i) && i + 1 < rows.len() && part(i + 1) {
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
        title_above(lines, first.saturating_sub(usize::from(more_above)));
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
fn title_above(lines: &[String], top: usize) -> (String, String, Option<String>, Vec<String>) {
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
            continue;
        }
        if let Some(shown) = text.strip_prefix('⌕') {
            field = Some(shown.trim().to_string());
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
