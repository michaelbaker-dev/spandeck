//! `spandeck integration install <agent>`: write hook entries into the agent's
//! own config, and remove exactly those entries on uninstall.
//!
//! Every entry this module writes carries a marker comment in its command, so
//! other hooks in the same file are never touched.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::AgentKind;

/// Bump when the set of hooked events or the command line changes.
pub const HOOKS_VERSION: u32 = 1;
const MARKER: &str = "# spandeck-managed v";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub agent: AgentKind,
    pub config_path: PathBuf,
    pub installed: bool,
    pub version: Option<u32>,
    pub outdated: bool,
    /// Something the user still has to do, in plain words.
    pub note: Option<String>,
}

fn events(kind: AgentKind) -> &'static [(&'static str, u64)] {
    // (event, timeout seconds). PermissionRequest holds while a remote answer
    // is awaited; the daemon gives up first and the terminal prompt appears.
    match kind {
        AgentKind::Claude => &[
            ("SessionStart", 10),
            ("UserPromptSubmit", 10),
            ("PreToolUse", 10),
            ("PostToolUse", 10),
            ("PostToolUseFailure", 10),
            ("PermissionRequest", 330),
            ("Notification", 10),
            ("Stop", 10),
            ("StopFailure", 10),
            ("SubagentStart", 10),
            ("SubagentStop", 10),
            ("PreCompact", 10),
            ("PostCompact", 10),
            ("SessionEnd", 5),
        ],
        AgentKind::Codex => &[
            ("SessionStart", 10),
            ("UserPromptSubmit", 10),
            ("PreToolUse", 10),
            ("PostToolUse", 10),
            ("PermissionRequest", 330),
            ("Stop", 10),
            ("Interrupt", 1),
            ("SubagentStart", 10),
            ("SubagentStop", 10),
            ("PreCompact", 10),
            ("PostCompact", 10),
            ("SessionEnd", 1),
        ],
        _ => &[],
    }
}

fn takes_matcher(event: &str) -> bool {
    matches!(
        event,
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PermissionRequest"
    )
}

fn hook_command(kind: AgentKind, bin: &str, windows: bool) -> String {
    if windows {
        // PowerShell: `&` runs a quoted path, a quote inside single quotes
        // is doubled, and `#` starts a comment as it does in sh.
        return format!(
            "& '{}' hook {} {}{}",
            bin.replace('\'', "''"),
            kind.as_str(),
            MARKER,
            HOOKS_VERSION
        );
    }
    format!(
        "'{}' hook {} {}{}",
        bin.replace('\'', "'\\''"),
        kind.as_str(),
        MARKER,
        HOOKS_VERSION
    )
}

/// One hook entry. On Windows, Claude Code runs a hook's command in Git
/// Bash when that is installed and in PowerShell when it is not
/// (code.claude.com/docs/en/hooks, read 14 Nov 2026); no one line is right
/// in both, so the entry names PowerShell, which every Windows has. Codex
/// documents no way to name the shell there: the same line is written and
/// is unverified (DECISIONS.md, "Windows").
fn hook_entry(kind: AgentKind, bin: &str, timeout: u64, windows: bool) -> Value {
    let mut entry =
        json!({"type": "command", "command": hook_command(kind, bin, windows), "timeout": timeout});
    if windows && kind == AgentKind::Claude {
        entry["shell"] = json!("powershell");
    }
    entry
}

/// The `hooks` object for an agent. Also used at launch time (`--settings`)
/// so a pane spandeck starts works with no global install.
pub fn hooks_block(kind: AgentKind, bin: &str) -> Value {
    hooks_block_for(kind, bin, crate::on_windows())
}

fn hooks_block_for(kind: AgentKind, bin: &str, windows: bool) -> Value {
    let mut hooks = Map::new();
    for (event, timeout) in events(kind) {
        let mut group = Map::new();
        if takes_matcher(event) {
            group.insert("matcher".into(), json!("*"));
        }
        group.insert(
            "hooks".into(),
            json!([hook_entry(kind, bin, *timeout, windows)]),
        );
        hooks.insert(event.to_string(), json!([Value::Object(group)]));
    }
    Value::Object(hooks)
}

pub fn config_path(kind: AgentKind, home: &Path) -> Result<PathBuf> {
    Ok(match kind {
        AgentKind::Claude => home.join(".claude").join("settings.json"),
        AgentKind::Codex => home.join(".codex").join("hooks.json"),
        other => bail!("{} has no hook integration", other.display()),
    })
}

fn read_config(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON; not touching it", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_config(path: &Path, value: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let backup = path.with_extension("json.spandeck-bak");
    if path.exists() && !backup.exists() {
        fs::copy(path, &backup)?;
    }
    let tmp = path.with_extension("json.spandeck-tmp");
    fs::write(&tmp, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn marker_version(command: &str) -> Option<u32> {
    let idx = command.find(MARKER)?;
    command[idx + MARKER.len()..].trim().parse().ok()
}

fn is_ours(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| c.contains(MARKER))
}

/// Remove our handlers from a `hooks` object. Returns how many were removed.
fn strip_ours(hooks: &mut Map<String, Value>) -> usize {
    let mut removed = 0;
    let mut empty_events = Vec::new();
    for (event, groups) in hooks.iter_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(handlers) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                let before = handlers.len();
                handlers.retain(|h| !is_ours(h));
                removed += before - handlers.len();
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(|h| h.as_array())
                .is_none_or(|h| !h.is_empty())
        });
        if groups.is_empty() {
            empty_events.push(event.clone());
        }
    }
    for event in empty_events {
        hooks.remove(&event);
    }
    removed
}

pub fn install(kind: AgentKind, home: &Path, bin: &str) -> Result<Status> {
    let path = config_path(kind, home)?;
    let mut config = read_config(&path)?;
    let root = config
        .as_object_mut()
        .context("config root is not an object")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().context("`hooks` is not an object")?;
    strip_ours(hooks);
    let Value::Object(ours) = hooks_block(kind, bin) else {
        unreachable!()
    };
    for (event, groups) in ours {
        let slot = hooks.entry(event).or_insert_with(|| json!([]));
        let slot = slot.as_array_mut().context("hook event is not an array")?;
        slot.extend(groups.as_array().cloned().unwrap_or_default());
    }
    write_config(&path, &config)?;
    status(kind, home)
}

pub fn uninstall(kind: AgentKind, home: &Path) -> Result<Status> {
    let path = config_path(kind, home)?;
    if path.exists() {
        let mut config = read_config(&path)?;
        if let Some(root) = config.as_object_mut() {
            let mut drop_hooks = false;
            if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
                let removed = strip_ours(hooks);
                drop_hooks = removed > 0 && hooks.is_empty();
            }
            if drop_hooks {
                root.remove("hooks");
            }
        }
        write_config(&path, &config)?;
    }
    status(kind, home)
}

pub fn status(kind: AgentKind, home: &Path) -> Result<Status> {
    let path = config_path(kind, home)?;
    let config = read_config(&path)?;
    let mut version: Option<u32> = None;
    if let Some(hooks) = config.get("hooks").and_then(|h| h.as_object()) {
        for groups in hooks.values() {
            for group in groups.as_array().into_iter().flatten() {
                for handler in group
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(v) = handler
                        .get("command")
                        .and_then(|c| c.as_str())
                        .and_then(marker_version)
                    {
                        version = Some(version.map_or(v, |cur| cur.min(v)));
                    }
                }
            }
        }
    }
    let installed = version.is_some();
    let note = match (kind, installed) {
        (AgentKind::Codex, true) => Some(
            "Codex runs a hook only after you trust it: open Codex and run /hooks once."
                .to_string(),
        ),
        _ => None,
    };
    Ok(Status {
        agent: kind,
        config_path: path,
        installed,
        version,
        outdated: version.is_some_and(|v| v < HOOKS_VERSION),
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows: the hook line must run in PowerShell, which Claude Code is
    /// told to use, with a path that has a space or a quote in it (a user
    /// folder such as `C:\Users\Mary O'Neil`), and must still be found
    /// again as ours.
    #[test]
    fn a_windows_hook_names_powershell_and_keeps_the_marker() {
        let bin = r"C:\Users\Mary O'Neil\AppData\Local\Programs\spandeck\spandeck.exe";
        let block = hooks_block_for(AgentKind::Claude, bin, true);
        let entry = &block["PreToolUse"][0]["hooks"][0];
        assert_eq!(entry["shell"], "powershell");
        let command = entry["command"].as_str().unwrap();
        assert_eq!(
            command,
            format!(
                r"& 'C:\Users\Mary O''Neil\AppData\Local\Programs\spandeck\spandeck.exe' hook claude # spandeck-managed v{HOOKS_VERSION}"
            )
        );
        assert!(is_ours(entry));
        assert_eq!(marker_version(command), Some(HOOKS_VERSION));
        // Unix entries are as they were: no shell is named.
        let unix = hooks_block_for(AgentKind::Claude, "/usr/bin/spandeck", false);
        assert!(unix["PreToolUse"][0]["hooks"][0].get("shell").is_none());
    }

    #[test]
    fn install_is_reversible_and_leaves_other_hooks_alone() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = json!({
            "model": "opus",
            "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]}
        });
        fs::write(&path, serde_json::to_string_pretty(&original).unwrap()).unwrap();

        let st = install(AgentKind::Claude, home.path(), "/usr/local/bin/spandeck").unwrap();
        assert!(st.installed && !st.outdated);
        assert_eq!(st.version, Some(HOOKS_VERSION));
        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            after["hooks"]["Stop"].as_array().unwrap().len(),
            2,
            "theirs + ours"
        );
        assert!(after["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("hook claude"));

        // Installing twice does not duplicate.
        install(AgentKind::Claude, home.path(), "/usr/local/bin/spandeck").unwrap();
        let again: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(again, after);

        let st = uninstall(AgentKind::Claude, home.path()).unwrap();
        assert!(!st.installed);
        let restored: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            restored, original,
            "uninstall removes exactly what install wrote"
        );
    }

    #[test]
    fn codex_install_creates_hooks_json_and_says_trust_is_needed() {
        let home = tempfile::tempdir().unwrap();
        let st = install(AgentKind::Codex, home.path(), "spandeck").unwrap();
        assert!(st.installed);
        assert!(st.note.unwrap().contains("/hooks"));
        assert!(home.path().join(".codex/hooks.json").exists());
        let st = uninstall(AgentKind::Codex, home.path()).unwrap();
        assert!(!st.installed);
    }

    #[test]
    fn invalid_json_is_never_overwritten() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();
        assert!(install(AgentKind::Claude, home.path(), "spandeck").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn older_marker_reads_as_outdated() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"spandeck hook claude # spandeck-managed v0"}]}]}}"#).unwrap();
        let st = status(AgentKind::Claude, home.path()).unwrap();
        assert!(st.installed && st.outdated);
    }
}
