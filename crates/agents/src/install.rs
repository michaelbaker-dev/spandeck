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
/// 2 (16 Nov 2026): a hook that can block is given 75 seconds and told so.
/// 3 (16 Nov 2026): Claude Code's question and plan have an entry of their
/// own, with an hour, so that the owner can answer them from the deck.
pub const HOOKS_VERSION: u32 = 3;
/// How the hook command is told how long its agent gives it (seconds): in
/// its environment, set by the installed command line itself. Not an
/// argument: a `spandeck` from before this (after going back a version)
/// would refuse an argument it does not know and exit 2, which Claude Code
/// and Codex read as "block this tool call"; a variable it ignores.
pub const DEADLINE_ENV: &str = "SPANDECK_HOOK_DEADLINE";
/// What a tool-call hook is given since version 2 of the entries. Before:
/// 10 seconds, which a session started before the update keeps.
pub const TOOL_HOOK_SECS: u64 = 75;
pub const PERMISSION_HOOK_SECS: u64 = 330;
/// The tools with which Claude Code asks the owner something. A hook that
/// runs before one of them can answer it (Claude Code's hook reference,
/// "Tools that require user interaction", read 16 Nov 2026): these two get
/// a second entry, beside the one every tool call has.
pub const ASKING_TOOLS: [&str; 2] = ["AskUserQuestion", "ExitPlanMode"];
/// What that entry's hook is given: an hour. It is the most a session will
/// ever wait, since a session keeps the entries it was started with; how
/// long the station really holds a question is its own setting, ten
/// minutes unless the owner says otherwise (core/questions.rs). A person
/// answers these, from a phone, so the 75 seconds of a rule check would
/// put nearly every question back in the terminal. The reference gives a
/// command hook 600 seconds when it names none, and names no upper limit;
/// that an hour is honoured has not been run with the real agent. If it
/// is not, the hook is cancelled early, and the reference says what
/// follows: the call goes on through its normal flow, which for these two
/// is the prompt in the terminal. Nothing answers for the owner.
pub const QUESTION_HOOK_SECS: u64 = 3600;
/// How the hook of that entry is told which one it is: the first entry's
/// hook runs for these tools too (hooks that match run side by side), and
/// only this one may wait for the owner.
pub const ANSWERS_ENV: &str = "SPANDECK_HOOK_ANSWERS";
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
    // PreToolUse is where a rule is enforced: it is given long enough to
    // wait for a spandeckd that is away for a minute and still answer
    // (core/guard.rs). Both agents' documentation says a hook that reaches
    // its timeout is cancelled and the tool call goes ahead, so the hook
    // must answer before it.
    match kind {
        AgentKind::Claude => &[
            ("SessionStart", 10),
            ("UserPromptSubmit", 10),
            ("PreToolUse", TOOL_HOOK_SECS),
            ("PostToolUse", 10),
            ("PostToolUseFailure", 10),
            ("PermissionRequest", PERMISSION_HOOK_SECS),
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
            ("PreToolUse", TOOL_HOOK_SECS),
            ("PostToolUse", 10),
            ("PermissionRequest", PERMISSION_HOOK_SECS),
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

/// Can the agent wait on this hook's answer before it acts? Those hooks
/// are told their timeout, so that they answer before it.
pub fn blocking(event: &str) -> bool {
    matches!(event, "PreToolUse" | "PermissionRequest")
}

/// The timeout the entries of this version give an agent's hook, in
/// seconds: what a session started with them will wait.
pub fn timeout_secs(kind: AgentKind, event: &str) -> Option<u64> {
    let kind = if kind == AgentKind::Fake {
        AgentKind::Claude
    } else {
        kind
    };
    events(kind)
        .iter()
        .find(|(e, _)| *e == event)
        .map(|(_, secs)| *secs)
}

fn hook_command(
    kind: AgentKind,
    bin: &str,
    windows: bool,
    deadline: Option<u64>,
    answers: bool,
) -> String {
    if windows {
        // PowerShell: `&` runs a quoted path, a quote inside single quotes
        // is doubled, and `#` starts a comment as it does in sh.
        let mut told = deadline
            .map(|secs| format!("$env:{DEADLINE_ENV}='{secs}'; "))
            .unwrap_or_default();
        if answers {
            told.push_str(&format!("$env:{ANSWERS_ENV}='1'; "));
        }
        return format!(
            "{told}& '{}' hook {} {}{}",
            bin.replace('\'', "''"),
            kind.as_str(),
            MARKER,
            HOOKS_VERSION
        );
    }
    let mut told = deadline
        .map(|secs| format!("{DEADLINE_ENV}={secs} "))
        .unwrap_or_default();
    if answers {
        told.push_str(&format!("{ANSWERS_ENV}=1 "));
    }
    format!(
        "{told}'{}' hook {} {}{}",
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
fn hook_entry(
    kind: AgentKind,
    bin: &str,
    event: &str,
    timeout: u64,
    windows: bool,
    answers: bool,
) -> Value {
    let deadline = blocking(event).then_some(timeout);
    let mut entry = json!({"type": "command", "command": hook_command(kind, bin, windows, deadline, answers), "timeout": timeout});
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
            json!([hook_entry(kind, bin, event, *timeout, windows, false)]),
        );
        let mut groups = vec![Value::Object(group)];
        // Claude Code's question and plan: a second entry for those two
        // tools by name, with the long timeout. A matcher of letters and
        // `|` is a list of exact names (its hook reference, "Matcher
        // patterns"), and each handler has a timeout of its own.
        if kind == AgentKind::Claude && *event == "PreToolUse" {
            groups.push(json!({
                "matcher": ASKING_TOOLS.join("|"),
                "hooks": [hook_entry(kind, bin, event, QUESTION_HOOK_SECS, windows, true)],
            }));
        }
        hooks.insert(event.to_string(), Value::Array(groups));
    }
    Value::Object(hooks)
}

pub fn config_path(kind: AgentKind, home: &Path) -> Result<PathBuf> {
    Ok(match kind {
        AgentKind::Claude => home.join(".claude").join("settings.json"),
        AgentKind::Codex => home.join(".codex").join("hooks.json"),
        // Not hooks: permission rules for spandeck's own mail tools.
        AgentKind::Antigravity => home
            .join(".gemini")
            .join("antigravity-cli")
            .join("settings.json"),
        other => bail!("{} has no hook integration", other.display()),
    })
}

/// The permission rules `spandeck integration install antigravity` adds to
/// the owner's Antigravity settings, and nothing else: allow, for the four
/// mail tools of spandeck's own MCP server, and for no other tool.
///
/// Why (found 16 Nov 2026): Antigravity asks "Allow calling this tool?"
/// before every call of an MCP tool, so an Antigravity agent could neither
/// read nor send mail without the owner at its terminal. What this rests
/// on, read 16 Nov 2026 and NOT run with a real `agy`:
/// - https://antigravity.google/docs/permissions (CLI): rules are
///   `action(target)` strings in `permissions.allow` / `ask` / `deny` of
///   `~/.gemini/antigravity-cli/settings.json`; "`mcp`: `mcp(server/tool)`,
///   `mcp(server/*)`, or `mcp(*)`"; "Unconfigured `mcp` tools ... default
///   to Ask"; "Deny > Ask > Allow"; "The CLI ignores rules it can't parse".
/// - https://antigravity.google/docs/build-plugins: a plugin's MCP server
///   is "namespaced with the plugin name as `<plugin-name>_<server-key>`",
///   so spandeck's server in spandeck's plugin is `spandeck_spandeck`,
///   which is also the name the real prompt showed.
///
/// A PreToolUse hook answering "allow" was NOT built: the guide shipped
/// in 1.5.9 says `"allow"` "automatically allow[s] the tool execution",
/// the online guide says it "allows the tool call to proceed, delegating
/// to the next hook or default policy" (which for an MCP tool is to ask)
/// and names another value, `"auto_approve"`, that the shipped guide does
/// not have; and neither says what a matcher is matched against for an
/// MCP server's tool.
pub const ANTIGRAVITY_MAIL_RULES: [&str; 4] = [
    "mcp(spandeck_spandeck/inbox)",
    "mcp(spandeck_spandeck/send)",
    "mcp(spandeck_spandeck/reply)",
    "mcp(spandeck_spandeck/ack)",
];

/// The `permissions.allow` list of an Antigravity settings file, made if
/// it is not there. Refused, with nothing changed, when what is there is
/// not what the documentation says it is.
fn antigravity_allow(config: &mut Value) -> Result<&mut Vec<Value>> {
    let root = config
        .as_object_mut()
        .context("the settings file is not a JSON object; not touching it")?;
    let permissions = root.entry("permissions").or_insert_with(|| json!({}));
    let permissions = permissions
        .as_object_mut()
        .context("`permissions` is not an object; not touching it")?;
    permissions
        .entry("allow")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("`permissions.allow` is not a list; not touching it")
}

fn antigravity_install(home: &Path, add: bool) -> Result<Status> {
    let path = config_path(AgentKind::Antigravity, home)?;
    if add || path.exists() {
        let mut config = read_config(&path)?;
        let before = config.clone();
        let allow = antigravity_allow(&mut config)?;
        allow.retain(|r| {
            !r.as_str()
                .is_some_and(|r| ANTIGRAVITY_MAIL_RULES.contains(&r))
        });
        if add {
            allow.extend(ANTIGRAVITY_MAIL_RULES.iter().map(|r| json!(r)));
        } else {
            // Nothing of ours is left behind: an empty list or object that
            // was made for these rules goes with them.
            let empty = allow.is_empty();
            let permissions = config["permissions"].as_object_mut();
            if let (true, Some(p)) = (empty, permissions) {
                p.remove("allow");
                if p.is_empty() {
                    config
                        .as_object_mut()
                        .map(|root| root.remove("permissions"));
                }
            }
        }
        if config != before {
            write_config(&path, &config)?;
        }
    }
    status(AgentKind::Antigravity, home)
}

fn antigravity_status(home: &Path) -> Result<Status> {
    let path = config_path(AgentKind::Antigravity, home)?;
    let config = read_config(&path)?;
    let has = |rule: &str| {
        config
            .pointer("/permissions/allow")
            .and_then(|a| a.as_array())
            .is_some_and(|a| a.iter().any(|r| r.as_str() == Some(rule)))
    };
    let installed = ANTIGRAVITY_MAIL_RULES.iter().all(|r| has(r));
    Ok(Status {
        agent: AgentKind::Antigravity,
        config_path: path,
        installed,
        version: None,
        outdated: false,
        note: Some(if installed {
            "Antigravity no longer asks before spandeck's inbox, send, reply and ack; it asks before every other tool as before. A running Antigravity may need to be started again to read the rules.".to_string()
        } else {
            "Antigravity asks you before each spandeck tool call. `spandeck integration install antigravity` adds allow rules for spandeck's inbox, send, reply and ack to your Antigravity settings, and for nothing else.".to_string()
        }),
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
    // The file keeps who may read it (Antigravity's is the owner's alone).
    if let Ok(meta) = fs::metadata(path) {
        fs::set_permissions(&tmp, meta.permissions())?;
    }
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
    if kind == AgentKind::Antigravity {
        return antigravity_install(home, true);
    }
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
    if kind == AgentKind::Antigravity {
        return antigravity_install(home, false);
    }
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
    if kind == AgentKind::Antigravity {
        return antigravity_status(home);
    }
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
                r"$env:SPANDECK_HOOK_DEADLINE='75'; & 'C:\Users\Mary O''Neil\AppData\Local\Programs\spandeck\spandeck.exe' hook claude # spandeck-managed v{HOOKS_VERSION}"
            )
        );
        assert!(is_ours(entry));
        assert_eq!(marker_version(command), Some(HOOKS_VERSION));
        // The entry that answers a question or a plan says so, and has
        // its hour.
        assert_eq!(
            block["PreToolUse"][1]["hooks"][0]["command"],
            format!(
                r"$env:SPANDECK_HOOK_DEADLINE='3600'; $env:SPANDECK_HOOK_ANSWERS='1'; & 'C:\Users\Mary O''Neil\AppData\Local\Programs\spandeck\spandeck.exe' hook claude # spandeck-managed v{HOOKS_VERSION}"
            )
        );
        assert_eq!(block["PreToolUse"][1]["hooks"][0]["shell"], "powershell");
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

    /// Found with a real Antigravity on 16 Nov 2026: it asked "Allow
    /// calling this tool?" before every spandeck tool call, so its agent
    /// could neither read nor send mail without the owner at its
    /// terminal. The owner's `integration install antigravity` adds allow
    /// rules for spandeck's four mail tools and for nothing else: never a
    /// wildcard, never another server, never another of spandeck's tools
    /// (delegate, the vault). The owner's other settings and rules are
    /// kept, an ask or deny rule of theirs is left to win, and uninstall
    /// takes out exactly what was put in.
    #[test]
    fn antigravity_is_allowed_spandecks_mail_tools_and_nothing_else() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".gemini/antigravity-cli/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let theirs = json!({
            "agentMode": "accept-edits",
            "permissions": {
                "allow": ["command(git status)", "mcp(spandeck_spandeck/inbox)"],
                "ask": ["mcp(spandeck_spandeck/send)"],
                "deny": ["command(sudo)"]
            }
        });
        fs::write(&path, theirs.to_string()).unwrap();
        assert!(
            !status(AgentKind::Antigravity, home.path())
                .unwrap()
                .installed
        );
        let read = || serde_json::from_str::<Value>(&fs::read_to_string(&path).unwrap()).unwrap();
        for _ in 0..2 {
            let st = install(AgentKind::Antigravity, home.path(), "spandeck").unwrap();
            assert!(st.installed && !st.outdated);
            let now = read();
            assert_eq!(
                now["permissions"]["allow"],
                json!([
                    "command(git status)",
                    "mcp(spandeck_spandeck/inbox)",
                    "mcp(spandeck_spandeck/send)",
                    "mcp(spandeck_spandeck/reply)",
                    "mcp(spandeck_spandeck/ack)"
                ])
            );
            assert_eq!(now["permissions"]["ask"], theirs["permissions"]["ask"]);
            assert_eq!(now["permissions"]["deny"], theirs["permissions"]["deny"]);
            assert_eq!(now["agentMode"], json!("accept-edits"));
        }
        for rule in ANTIGRAVITY_MAIL_RULES {
            assert!(
                rule.starts_with("mcp(spandeck_spandeck/") && !rule.contains('*'),
                "{rule}"
            );
        }
        uninstall(AgentKind::Antigravity, home.path()).unwrap();
        assert_eq!(
            read()["permissions"]["allow"],
            json!(["command(git status)"])
        );
        // A file with nothing in it but our rules is left with nothing.
        fs::write(&path, "{}").unwrap();
        install(AgentKind::Antigravity, home.path(), "spandeck").unwrap();
        uninstall(AgentKind::Antigravity, home.path()).unwrap();
        assert_eq!(read(), json!({}));
        // Not what the documentation says it is: refused, and not touched.
        let odd = "{\"permissions\": {\"allow\": \"everything\"}}";
        fs::write(&path, odd).unwrap();
        assert!(install(AgentKind::Antigravity, home.path(), "spandeck").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), odd);
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

    /// REVIEW2 blocker 8: PreToolUse was installed with a 10 second
    /// timeout, so a hook that waits for spandeckd and then refuses never
    /// got to refuse: the agent cancels it first and runs the tool. The
    /// entry gives it 75 seconds and the command tells the hook so; a hook
    /// that cannot block is told nothing. The variable is set by the
    /// command line, never passed as an argument: a `spandeck` from before
    /// this would exit 2 on an argument it does not know, and exit 2
    /// blocks the tool call.
    #[test]
    fn a_hook_that_can_block_is_given_time_and_told_how_much() {
        for kind in [AgentKind::Claude, AgentKind::Codex] {
            let block = hooks_block_for(kind, "/usr/local/bin/spandeck", false);
            let entry = |event: &str| block[event][0]["hooks"][0].clone();
            assert_eq!(entry("PreToolUse")["timeout"], 75);
            assert_eq!(
                entry("PreToolUse")["command"],
                format!(
                    "SPANDECK_HOOK_DEADLINE=75 '/usr/local/bin/spandeck' hook {} # spandeck-managed v3",
                    kind.as_str()
                )
            );
            assert_eq!(entry("PermissionRequest")["timeout"], 330);
            assert!(entry("PermissionRequest")["command"]
                .as_str()
                .unwrap()
                .starts_with("SPANDECK_HOOK_DEADLINE=330 '"));
            assert_eq!(
                entry("PostToolUse")["command"],
                format!(
                    "'/usr/local/bin/spandeck' hook {} # spandeck-managed v3",
                    kind.as_str()
                )
            );
            for event in ["PreToolUse", "PermissionRequest", "Stop"] {
                let e = entry(event);
                assert!(is_ours(&e));
                assert_eq!(marker_version(e["command"].as_str().unwrap()), Some(3));
                assert_eq!(timeout_secs(kind, event), e["timeout"].as_u64());
            }
        }
        // Version 1's entries are found again, as outdated, and replaced.
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"'/usr/local/bin/spandeck' hook claude # spandeck-managed v1","timeout":10}]}]}}"#).unwrap();
        assert!(status(AgentKind::Claude, home.path()).unwrap().outdated);
        let st = install(AgentKind::Claude, home.path(), "/usr/local/bin/spandeck").unwrap();
        assert!(st.installed && !st.outdated);
        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(after["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"], 75);
    }

    /// Mobile plan, phase M3: a question or a plan waits for a person, far
    /// longer than a rule check. Claude Code's two asking tools get an entry
    /// of their own, by name, with an hour, and its hook is told that it is
    /// the one that may wait for the owner; the entry every tool call has is
    /// unchanged, so a rule is still checked within its 75 seconds. Codex,
    /// whose hooks cannot answer a question, gets no such entry. Uninstall
    /// takes both away.
    #[test]
    fn a_question_and_a_plan_have_an_entry_of_their_own() {
        let block = hooks_block_for(AgentKind::Claude, "/usr/local/bin/spandeck", false);
        let groups = block["PreToolUse"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["matcher"], "*");
        assert_eq!(groups[0]["hooks"][0]["timeout"], 75);
        assert_eq!(groups[1]["matcher"], "AskUserQuestion|ExitPlanMode");
        assert_eq!(groups[1]["hooks"][0]["timeout"], 3600);
        assert_eq!(
            groups[1]["hooks"][0]["command"],
            "SPANDECK_HOOK_DEADLINE=3600 SPANDECK_HOOK_ANSWERS=1 '/usr/local/bin/spandeck' hook claude # spandeck-managed v3"
        );
        assert!(is_ours(&groups[1]["hooks"][0]));
        let codex = hooks_block_for(AgentKind::Codex, "/usr/local/bin/spandeck", false);
        assert_eq!(codex["PreToolUse"].as_array().unwrap().len(), 1);
        for event in ["PermissionRequest", "PostToolUse", "Stop"] {
            assert_eq!(block[event].as_array().unwrap().len(), 1, "{event}");
        }
        let home = tempfile::tempdir().unwrap();
        install(AgentKind::Claude, home.path(), "/usr/local/bin/spandeck").unwrap();
        let st = uninstall(AgentKind::Claude, home.path()).unwrap();
        assert!(!st.installed);
        let left: Value = serde_json::from_str(
            &fs::read_to_string(home.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(left, json!({}));
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
