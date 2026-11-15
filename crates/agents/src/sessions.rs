//! The last conversation an agent saved for a folder, read from the agent's
//! own files, so one started outside spandeck counts too.
//!
//! - Claude Code keeps one file per conversation, `<session>.jsonl`, in
//!   `<config>/projects/<folder name>/`. The folder name is the working
//!   folder with every character that is not an ASCII letter or digit
//!   replaced by `-` (layout seen on 2.1.x, 15 Nov 2026). Its sub-agents
//!   write under `<session>/subagents/`, one level down, and are not read.
//! - Codex keeps `sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`; the first
//!   line (`session_meta`) records the id and the folder (shape seen on a
//!   real file, 15 Nov 2026).
//! - OpenCode keeps its sessions in a database: see
//!   `spandeck_archive::usage::opencode_last`.
//! - Antigravity: no dependable record of which folder a conversation
//!   belongs to is known, so none is ever found.
//!
//! Everything here only reads.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;

/// A conversation an agent can go back to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSession {
    /// What the agent's resume flag takes.
    pub id: String,
    /// The file it is kept in, for an agent that keeps one per conversation.
    pub path: Option<PathBuf>,
    pub last_used_ms: u64,
}

/// Where Claude Code keeps its conversations.
pub fn claude_projects_dir(home: &Path) -> PathBuf {
    env_dir("CLAUDE_CONFIG_DIR")
        .unwrap_or_else(|| home.join(".claude"))
        .join("projects")
}

/// Where Codex keeps its conversations.
pub fn codex_sessions_dir(home: &Path) -> PathBuf {
    env_dir("CODEX_HOME")
        .unwrap_or_else(|| home.join(".codex"))
        .join("sessions")
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// A folder under the names an agent may have recorded it by: as it was
/// given, and with links resolved (what a program started there is told).
#[derive(Debug, Clone)]
pub struct Folder {
    names: Vec<String>,
    windows: bool,
}

impl Folder {
    pub fn of(path: &Path) -> Folder {
        let mut names = vec![];
        if let Ok(real) = std::fs::canonicalize(path) {
            names.push(real.to_string_lossy().into_owned());
        }
        names.push(path.to_string_lossy().into_owned());
        Folder::named(names, crate::on_windows())
    }

    fn named(names: Vec<String>, windows: bool) -> Folder {
        let mut out: Vec<String> = vec![];
        for name in names {
            let name = plain(&name, windows);
            if !out.contains(&name) {
                out.push(name);
            }
        }
        Folder {
            names: out,
            windows,
        }
    }

    /// Is `recorded`, a folder as an agent wrote it down, this folder?
    pub fn is(&self, recorded: &str) -> bool {
        let recorded = key(&plain(recorded, self.windows), self.windows);
        self.names.iter().any(|n| key(n, self.windows) == recorded)
    }
}

/// A path without Windows' long-path prefix (`\\?\C:\x`, which resolving a
/// path adds there and no agent writes down) and without a trailing
/// separator.
fn plain(path: &str, windows: bool) -> String {
    let mut p = path.to_string();
    if windows {
        if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
            p = format!(r"\\{rest}");
        } else if let Some(rest) = p.strip_prefix(r"\\?\") {
            p = rest.to_string();
        }
    }
    let seps: &[char] = if windows { &['\\', '/'] } else { &['/'] };
    let trimmed = p.trim_end_matches(seps);
    // A root (`/`, `C:\`) keeps its separator.
    if trimmed.is_empty() || (windows && trimmed.ends_with(':')) {
        p
    } else {
        trimmed.to_string()
    }
}

/// What two spellings of one folder have in common: on Windows, case and
/// the direction of the separators do not tell folders apart.
fn key(path: &str, windows: bool) -> String {
    if windows {
        path.replace('/', "\\").to_lowercase()
    } else {
        path.to_string()
    }
}

/// The name of Claude Code's directory for a working folder. `None` for a
/// folder whose name is too long: Claude Code shortens those and adds a
/// hash, which is not reproduced here.
pub fn claude_folder_name(folder: &str) -> Option<String> {
    let mut name = String::with_capacity(folder.len());
    for c in folder.chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else {
            // Claude Code replaces UTF-16 units, so a character outside the
            // basic plane becomes two dashes.
            for _ in 0..c.len_utf16() {
                name.push('-');
            }
        }
    }
    (name.len() <= 200).then_some(name)
}

fn modified_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ClaudeLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "isSidechain")]
    side: bool,
}

/// Does a Claude Code session file hold a conversation? Claude Code refuses
/// to resume one that has no message of the main conversation in it (a file
/// of summaries or snapshots only). Looks at the start of the file only.
fn claude_has_conversation(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut reader = BufReader::new(file.take(8 << 20));
    let mut line = Vec::new();
    for _ in 0..400 {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(n) if n > 0 => {}
            _ => return false,
        }
        if let Ok(l) = serde_json::from_slice::<ClaudeLine>(&line) {
            if !l.side && matches!(l.kind.as_deref(), Some("user" | "assistant")) {
                return true;
            }
        }
    }
    false
}

/// Claude Code's most recently used conversation for `folder` that `taken`
/// does not claim. `projects` is `claude_projects_dir`.
pub fn claude_last(
    projects: &Path,
    folder: &Folder,
    taken: &dyn Fn(&str) -> bool,
) -> Option<SavedSession> {
    let mut files: Vec<(u64, String, PathBuf)> = vec![];
    for name in &folder.names {
        let Some(dir) = claude_folder_name(name) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(projects.join(dir)) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let Some(name) = e.file_name().to_str().map(String::from) else {
                continue;
            };
            // `agent-*.jsonl` beside the sessions is how older versions kept
            // a sub-agent's side conversation.
            let Some(id) = name
                .strip_suffix(".jsonl")
                .filter(|id| !id.is_empty() && !id.starts_with("agent-"))
            else {
                continue;
            };
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() || meta.len() == 0 || files.iter().any(|f| f.1 == id) {
                continue;
            }
            files.push((modified_ms(&meta), id.to_string(), e.path()));
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    files
        .into_iter()
        .find(|(_, id, path)| !taken(id) && claude_has_conversation(path))
        .map(|(last_used_ms, id, path)| SavedSession {
            id,
            path: Some(path),
            last_used_ms,
        })
}

/// How many of Codex's newest session files are opened to find one for a
/// folder. A conversation older than that many others is not found.
pub const CODEX_FILES_READ: usize = 200;

#[derive(Deserialize, Default)]
#[serde(default)]
struct CodexMeta {
    id: Option<String>,
    cwd: Option<String>,
    /// "cli", "vscode", "exec"; an object for a conversation Codex started
    /// itself (a sub-agent), which is not the user's to go back to.
    source: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CodexFirstLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<CodexMeta>,
}

/// The names in `dir`, last first. Codex's year, month, day and file names
/// all sort by time.
fn names_newest_first(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort_by(|a, b| b.cmp(a));
    names
}

/// A Codex session file's id, if it is a conversation of the user's in
/// `folder` with something said in it.
fn codex_session_in(path: &Path, folder: &Folder) -> Option<String> {
    let file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut reader = BufReader::new(file.take(4 << 20));
    let mut line = Vec::new();
    let n = reader.read_until(b'\n', &mut line).ok()? as u64;
    // Only the first line, or a first line still being written: nothing to
    // go back to yet.
    if line.last() != Some(&b'\n') || len <= n {
        return None;
    }
    let first: CodexFirstLine = serde_json::from_slice(&line).ok()?;
    if first.kind.as_deref() != Some("session_meta") {
        return None;
    }
    let meta = first.payload?;
    if meta.source.as_ref().is_some_and(|s| s.is_object()) || !folder.is(meta.cwd.as_deref()?) {
        return None;
    }
    meta.id.filter(|id| !id.is_empty())
}

/// Codex's most recently used conversation for `folder` that `taken` does
/// not claim, among the `CODEX_FILES_READ` most recently started.
/// `sessions` is `codex_sessions_dir`.
pub fn codex_last(
    sessions: &Path,
    folder: &Folder,
    taken: &dyn Fn(&str) -> bool,
) -> Option<SavedSession> {
    let mut read = 0;
    let mut found: Vec<SavedSession> = vec![];
    'walk: for year in names_newest_first(sessions) {
        let year = sessions.join(year);
        for month in names_newest_first(&year) {
            let month = year.join(month);
            for day in names_newest_first(&month) {
                let day = month.join(day);
                for name in names_newest_first(&day) {
                    if !(name.starts_with("rollout-") && name.ends_with(".jsonl")) {
                        continue;
                    }
                    if read == CODEX_FILES_READ {
                        break 'walk;
                    }
                    read += 1;
                    let path = day.join(name);
                    let Some(id) = codex_session_in(&path, folder) else {
                        continue;
                    };
                    let last_used_ms = std::fs::metadata(&path)
                        .map(|m| modified_ms(&m))
                        .unwrap_or(0);
                    found.push(SavedSession {
                        id,
                        path: Some(path),
                        last_used_ms,
                    });
                }
            }
        }
    }
    // Going back to a conversation adds to its first file, so the last one
    // used need not be the last one started. (A stable sort: of two used at
    // the same moment, the one started later.)
    found.sort_by_key(|s| std::cmp::Reverse(s.last_used_ms));
    found.into_iter().find(|s| !taken(&s.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn write(path: &Path, text: &str, age_secs: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        let when = SystemTime::now() - Duration::from_secs(age_secs);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    const SAID: &str = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n";

    /// The owner's request (15 Nov 2026): a new pane continues the last
    /// conversation in its folder "if it's there", and "if it's not then it
    /// just start a new one". For Claude Code that means the newest session
    /// file it would accept: not an empty one, not a sub-agent's, not one
    /// with no message, not another folder's, and not one a pane is in.
    #[test]
    fn claude_last_is_the_newest_conversation_it_would_resume_in_that_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let work = tmp.path().join("my.repo");
        std::fs::create_dir_all(&work).unwrap();
        let folder = Folder::of(&work);
        let none = |_: &str| false;
        assert_eq!(claude_last(&projects, &folder, &none), None);

        let real = std::fs::canonicalize(&work).unwrap();
        let dir = projects.join(
            claude_folder_name(&plain(&real.to_string_lossy(), crate::on_windows())).unwrap(),
        );
        write(&dir.join("old.jsonl"), SAID, 3000);
        write(&dir.join("recent.jsonl"), SAID, 2000);
        write(&dir.join("never-used.jsonl"), "", 10);
        write(
            &dir.join("no-message.jsonl"),
            "{\"type\":\"summary\",\"summary\":\"x\"}\n",
            20,
        );
        write(
            &dir.join("side.jsonl"),
            "{\"type\":\"user\",\"isSidechain\":true}\n",
            30,
        );
        write(&dir.join("agent-a1.jsonl"), SAID, 40);
        write(&dir.join("recent/subagents/agent-b2.jsonl"), SAID, 50);
        write(&projects.join("-somewhere-else/other.jsonl"), SAID, 60);

        let found = claude_last(&projects, &folder, &none).unwrap();
        assert_eq!(found.id, "recent");
        assert_eq!(found.path, Some(dir.join("recent.jsonl")));
        let age = now_ms() - found.last_used_ms;
        assert!((1_990_000..2_100_000).contains(&age), "{age}");
        // A live pane is in the newest: the next one, then none.
        let next = claude_last(&projects, &folder, &|id| id == "recent").unwrap();
        assert_eq!(next.id, "old");
        assert_eq!(claude_last(&projects, &folder, &|_| true), None);
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// Claude Code's folder names, and the folders of a Windows station:
    /// the same request, for the machines the owner runs (`C:\Users\me\x`
    /// is `C--Users-me-x` there, and Codex records `C:\Users\me\x`).
    #[test]
    fn folders_are_named_and_compared_the_way_each_system_writes_them() {
        assert_eq!(
            claude_folder_name("/home/me/code/my.repo_v2").unwrap(),
            "-home-me-code-my-repo-v2"
        );
        assert_eq!(
            claude_folder_name(r"C:\Users\me\x").unwrap(),
            "C--Users-me-x"
        );
        assert_eq!(claude_folder_name("/a/é/😀").unwrap(), "-a-----");
        assert_eq!(claude_folder_name(&"/x".repeat(150)), None);

        let win = Folder::named(vec![r"\\?\C:\Users\Me\proj".into()], true);
        assert_eq!(win.names, [r"C:\Users\Me\proj"]);
        assert!(win.is(r"c:\users\me\proj\"));
        assert!(win.is("C:/Users/Me/proj"));
        assert!(!win.is(r"C:\Users\Me\proj2"));
        let unix = Folder::named(vec!["/home/me/proj/".into(), "/home/me/proj".into()], false);
        assert_eq!(unix.names, ["/home/me/proj"]);
        assert!(unix.is("/home/me/proj") && !unix.is("/home/me/Proj"));
        assert!(Folder::named(vec!["/".into()], false).is("/"));
    }

    fn rollout(id: &str, cwd: &str, source: &str, said: bool) -> String {
        let meta = format!(
            "{{\"timestamp\":\"t\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":{},\"source\":{source},\"base_instructions\":{{\"text\":\"long\"}}}}}}\n",
            serde_json::to_string(cwd).unwrap()
        );
        if said {
            meta + "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n"
        } else {
            meta
        }
    }

    /// The same request, for Codex: the conversation last used in the
    /// folder, read from each file's first line, and never more than a
    /// fixed number of files however long the history is.
    #[test]
    fn codex_last_is_the_conversation_last_used_in_that_folder_within_a_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let work = tmp.path().join("repo");
        std::fs::create_dir_all(&work).unwrap();
        let folder = Folder::of(&work);
        let here = work.to_string_lossy().into_owned();
        let none = |_: &str| false;
        assert_eq!(codex_last(&sessions, &folder, &none), None);

        let cli = "\"cli\"";
        let file = |day: &str, name: &str| sessions.join(day).join(format!("rollout-{name}.jsonl"));
        // Started first, used last: going back to it added to its file.
        write(
            &file("2026/11/13", "a"),
            &rollout("first", &here, cli, true),
            100,
        );
        write(
            &file("2026/11/14", "b"),
            &rollout("second", &here, cli, true),
            500,
        );
        write(
            &file("2026/11/14", "c"),
            &rollout("elsewhere", "/other", cli, true),
            5,
        );
        write(
            &file("2026/11/15", "d"),
            &rollout("unused", &here, cli, false),
            5,
        );
        write(
            &file("2026/11/15", "e"),
            &rollout("helper", &here, "{\"subagent\":\"review\"}", true),
            5,
        );
        write(&file("2026/11/15", "f"), "not json\n{}\n", 5);

        let found = codex_last(&sessions, &folder, &none).unwrap();
        assert_eq!(found.id, "first");
        assert_eq!(found.path, Some(file("2026/11/13", "a")));
        assert_eq!(
            codex_last(&sessions, &folder, &|id| id == "first")
                .unwrap()
                .id,
            "second"
        );

        // A long history of other folders: the search stops at its bound.
        for i in 0..CODEX_FILES_READ {
            write(
                &file("2026/11/16", &format!("{i:04}")),
                &rollout(&format!("x{i}"), "/other", cli, true),
                1,
            );
        }
        assert_eq!(codex_last(&sessions, &folder, &none), None);
    }
}
