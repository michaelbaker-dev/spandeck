//! Agent setup: one list of MCP servers, skills and default model settings,
//! edited in the Control Deck and applied to every agent on every station.
//!
//! Each station keeps its own copy (`agent-setup.json`) and applies it in the
//! way each agent understands. Stations converge by pushing the newer copy to
//! a peer that has an older one: last edit wins. This is the small, early part
//! of the common config; the rest stays in v1.5.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spandeck_agents::{AgentKind, AgentState};

use crate::daemon::{invalid, Daemon, Result};

const MARKER: &str = ".spandeck-managed";
/// The skill every agent in a spandeck pane gets: how to use spandeck's tools.
const SPANDECK_SKILL: &str = include_str!("../../../skills/spandeck/SKILL.md");

fn write_if_changed(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|now| now == content) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServer {
    /// A local server: the program and its arguments.
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Or a remote server.
    pub url: Option<String>,
    /// Which agents get it. Empty: all of them.
    pub agents: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Skill {
    pub description: String,
    /// The body of SKILL.md (without front matter; spandeck writes that).
    pub content: String,
    pub agents: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentDefaults {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSetup {
    pub updated_ms: u64,
    pub updated_by: String,
    pub mcp_servers: BTreeMap<String, McpServer>,
    pub skills: BTreeMap<String, Skill>,
    /// Keyed by agent: "claude", "codex", "antigravity".
    pub defaults: BTreeMap<String, AgentDefaults>,
    /// Which stations' changes this copy holds (sync.rs).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub changes: crate::sync::Changes,
}

fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "spandeck"
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn applies(agents: &[String], kind: AgentKind) -> bool {
    agents.is_empty() || agents.iter().any(|a| a == kind.as_str())
}

impl AgentSetup {
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !crate::sync::changes_ok(&self.changes) {
            return Err("the record of changes is not one".into());
        }
        for (name, server) in &self.mcp_servers {
            if !name_ok(name) {
                return Err(format!(
                    "MCP server name {name:?}: use letters, digits, - and _ (and not \"spandeck\")"
                ));
            }
            match (&server.command, &server.url) {
                (Some(c), None) if !c.trim().is_empty() => {}
                (None, Some(u)) if u.starts_with("https://") || u.starts_with("http://") => {}
                _ => {
                    return Err(format!(
                        "MCP server {name}: give either a command or an http(s) URL"
                    ))
                }
            }
        }
        for (name, skill) in &self.skills {
            if !name_ok(name) {
                return Err(format!("skill name {name:?}: use letters, digits, - and _"));
            }
            if skill.content.trim().is_empty() || skill.description.trim().is_empty() {
                return Err(format!(
                    "skill {name}: needs a description and some content"
                ));
            }
        }
        for (agent, d) in &self.defaults {
            if !matches!(agent.as_str(), "claude" | "codex" | "antigravity") {
                return Err(format!(
                    "defaults are per agent (claude, codex, antigravity), not {agent:?}"
                ));
            }
            for v in [&d.model, &d.effort, &d.permission_mode]
                .into_iter()
                .flatten()
            {
                if v.is_empty()
                    || !v.chars().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '[' | ']')
                    })
                {
                    return Err(format!("{agent}: {v:?} is not a valid setting value"));
                }
            }
            // A word Codex has no flag for would be dropped without a word.
            if let ("codex", Some(m)) = (agent.as_str(), d.permission_mode.as_deref()) {
                if !matches!(m, "auto" | "never" | "on-request") {
                    return Err(format!(
                        "codex: the permission mode is auto, on-request or never, not {m:?}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The MCP servers for an agent, in Claude Code's `mcpServers` shape.
    pub fn mcp_json(&self, kind: AgentKind) -> serde_json::Map<String, Value> {
        self.mcp_servers
            .iter()
            .filter(|(_, s)| applies(&s.agents, kind))
            .map(|(name, s)| {
                let v = match (&s.command, &s.url) {
                    (Some(command), _) => json!({"command": command, "args": s.args, "env": s.env}),
                    (None, Some(url)) => json!({"type": "http", "url": url}),
                    _ => json!({}),
                };
                (name.clone(), v)
            })
            .collect()
    }

    /// Launch arguments that apply this setup to a new agent pane.
    pub fn launch_args(&self, kind: AgentKind) -> Vec<String> {
        let d = self
            .defaults
            .get(kind.as_str())
            .cloned()
            .unwrap_or_default();
        let mut args = vec![];
        match kind {
            AgentKind::Claude => {
                for (flag, v) in [
                    ("--model", &d.model),
                    ("--effort", &d.effort),
                    ("--permission-mode", &d.permission_mode),
                ] {
                    if let Some(v) = v {
                        args.push(flag.to_string());
                        args.push(v.clone());
                    }
                }
            }
            // `agy --help` 1.5.9: --model, --effort, --mode.
            AgentKind::Antigravity => {
                for (flag, v) in [
                    ("--model", &d.model),
                    ("--effort", &d.effort),
                    ("--mode", &d.permission_mode),
                ] {
                    if let Some(v) = v {
                        args.push(flag.to_string());
                        args.push(v.clone());
                    }
                }
            }
            AgentKind::Codex => {
                // `codex --help` 0.169.0. "auto" is Codex's own review of
                // what it would otherwise ask the owner, inside its sandbox
                // for the folder: what Claude Code calls auto mode. (Until
                // 18 Nov 2026 this setting was dropped for Codex, and a
                // Codex pane asked the owner about each step.)
                match d.permission_mode.as_deref() {
                    Some("auto") => args.push("--approve-for-me".to_string()),
                    Some(m @ ("never" | "on-request")) => {
                        args.extend(["--ask-for-approval".to_string(), m.to_string()])
                    }
                    _ => {}
                }
                if let Some(m) = &d.model {
                    args.extend(["-m".to_string(), m.clone()]);
                }
                if let Some(e) = &d.effort {
                    args.extend(["-c".to_string(), format!("model_reasoning_effort=\"{e}\"")]);
                }
                for (name, s) in self
                    .mcp_servers
                    .iter()
                    .filter(|(_, s)| applies(&s.agents, kind))
                {
                    let key = format!("mcp_servers.{name}");
                    match (&s.command, &s.url) {
                        (Some(command), _) => {
                            args.extend([
                                "-c".to_string(),
                                format!("{key}.command={}", json!(command)),
                            ]);
                            args.extend([
                                "-c".to_string(),
                                format!("{key}.args={}", json!(s.args)),
                            ]);
                            for (k, v) in &s.env {
                                args.extend([
                                    "-c".to_string(),
                                    format!("{key}.env.{k}={}", json!(v)),
                                ]);
                            }
                        }
                        (None, Some(url)) => {
                            args.extend(["-c".to_string(), format!("{key}.url={}", json!(url))])
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        args
    }
}

impl Daemon {
    fn setup_file(&self) -> PathBuf {
        self.paths.config_dir.join("agent-setup.json")
    }

    pub fn setup(&self) -> AgentSetup {
        std::fs::read(self.setup_file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Does this station write into the agents' own settings under the
    /// user's home folder? Only the station of the account does (the usual
    /// state directory, with the login service), or one whose config says
    /// `agent_home = true`.
    ///
    /// What happened (16 Nov 2026): a throwaway station, started for a test
    /// under its own state and config directories but with the owner's
    /// real HOME, rewrote `~/.gemini/config/plugins/spandeck` to point at
    /// its own binary and would have removed the setup's skills it did
    /// not list. A second station has no business in those folders: what
    /// its panes need goes on their own command line or environment, or
    /// in a folder under its own state directory.
    pub fn owns_agent_home(&self) -> bool {
        self.cfg
            .agent_home
            .unwrap_or_else(|| self.paths.is_the_station(&self.home))
    }

    /// Why this station left the agents' settings alone, for a problem
    /// list, a refusal and the doctor.
    pub(crate) fn not_owning_words(&self) -> String {
        format!(
            "{} does not write into the agent settings in your home folder: it is not the station that starts at login from {} (a trial, a test or a second station). To let it, put `agent_home = true` in {}",
            self.station,
            spandeck_terminal::sys::default_dirs(&self.home).0.display(),
            self.paths.config_file().display()
        )
    }

    /// An Antigravity pane on a station that may not write Antigravity's
    /// plugin has spandeck's tools only if the station of the account has
    /// put the plugin there. Otherwise: what to tell the owner.
    pub(crate) fn antigravity_without_tools(&self) -> Option<String> {
        if self.owns_agent_home() || self.antigravity_plugin_dir().join(MARKER).exists() {
            return None;
        }
        Some("started without spandeck's tools: Antigravity loads them only from a plugin in your home folder, which this station does not write (see `spandeck doctor`)".to_string())
    }

    /// Where each agent looks for skills on this station: the folders this
    /// station may write.
    fn skill_dirs(&self) -> Vec<(AgentKind, PathBuf)> {
        let mut dirs = vec![];
        let owns = self.owns_agent_home();
        if owns {
            dirs.push((AgentKind::Claude, self.home.join(".claude/skills")));
            // From Codex's documentation; not checked against a real Codex here.
            dirs.push((AgentKind::Codex, self.home.join(".codex/skills")));
        }
        // A folder of spandeck's own, named to OpenCode at launch.
        dirs.push((AgentKind::OpenCode, self.opencode_skills_dir()));
        // Only once an Antigravity pane has been started here.
        let plugin = self.antigravity_plugin_dir();
        if owns && plugin.join(MARKER).exists() {
            dirs.push((AgentKind::Antigravity, plugin.join("skills")));
        }
        dirs
    }

    fn opencode_skills_dir(&self) -> PathBuf {
        self.paths.state_dir.join("agent-skills").join("opencode")
    }

    /// spandeck's plugin in Antigravity's own plugin folder. Antigravity
    /// loads every plugin placed there (its plugins guide; `agy plugin list`
    /// showed this one enabled with its skill, MCP server and hooks).
    fn antigravity_plugin_dir(&self) -> PathBuf {
        self.home.join(".gemini/config/plugins/spandeck")
    }

    /// The spandeck skill, in a folder of skills spandeck manages.
    fn write_spandeck_skill(dir: &std::path::Path) -> std::io::Result<()> {
        let skill = dir.join("spandeck");
        write_if_changed(&skill.join("SKILL.md"), SPANDECK_SKILL)?;
        write_if_changed(
            &skill.join(MARKER),
            "Installed by spandeck. It is rewritten when a pane starts.\n",
        )
    }

    /// OpenCode's settings for one pane, in the form `OPENCODE_CONFIG_CONTENT`
    /// takes: spandeck's MCP server speaking for this pane, the fleet's
    /// shared servers, and where the skills are. (`opencode debug config
    /// --standalone` on 2.3.0 accepts exactly these keys.)
    pub(crate) fn opencode_config(
        &self,
        setup: &AgentSetup,
        pane: &str,
        token: &str,
        project: Option<&str>,
    ) -> Value {
        let skills = self.opencode_skills_dir();
        if let Err(e) = Self::write_spandeck_skill(&skills) {
            tracing::warn!(error = %e, "could not write the spandeck skill for OpenCode");
        }
        let mut env = serde_json::Map::new();
        for (k, v) in [
            ("SPANDECK_PANE", pane.to_string()),
            ("SPANDECK_TOKEN", token.to_string()),
            ("SPANDECK_SOCKET", self.paths.socket().display().to_string()),
            (
                "SPANDECK_STATE_DIR",
                self.paths.state_dir.display().to_string(),
            ),
            (
                "SPANDECK_CONFIG_DIR",
                self.paths.config_dir.display().to_string(),
            ),
            ("SPANDECK_STATION", self.station.clone()),
        ] {
            env.insert(k.into(), json!(v));
        }
        if let Some(project) = project {
            env.insert("SPANDECK_PROJECT".into(), json!(project));
        }
        let mut servers = serde_json::Map::new();
        for (name, s) in setup
            .mcp_servers
            .iter()
            .filter(|(_, s)| applies(&s.agents, AgentKind::OpenCode))
        {
            let v = match (&s.command, &s.url) {
                (Some(command), _) => {
                    let mut argv = vec![command.clone()];
                    argv.extend(s.args.iter().cloned());
                    json!({"type": "local", "command": argv, "environment": s.env, "enabled": true})
                }
                (None, Some(url)) => json!({"type": "remote", "url": url, "enabled": true}),
                _ => continue,
            };
            servers.insert(name.clone(), v);
        }
        servers.insert(
            "spandeck".into(),
            json!({"type": "local", "command": [self.exe.display().to_string(), "mcp"], "environment": env, "enabled": true}),
        );
        json!({"mcp": servers, "skills": {"paths": [skills.display().to_string()]}})
    }

    /// Write, or bring up to date, spandeck's Antigravity plugin: its MCP
    /// server and the fleet's shared ones, the hooks that report the
    /// conversation and the turn, and the skills. Returns what went wrong.
    pub(crate) fn antigravity_plugin(&self, setup: &AgentSetup) -> Vec<String> {
        if !self.owns_agent_home() {
            return vec![self.not_owning_words()];
        }
        let dir = self.antigravity_plugin_dir();
        if dir.exists() && !dir.join(MARKER).exists() {
            return vec![format!(
                "{} exists and was not made by spandeck; left alone, so Antigravity panes have no spandeck tools",
                dir.display()
            )];
        }
        let bin = self.exe.display().to_string();
        let mut servers = serde_json::Map::new();
        for (name, s) in setup
            .mcp_servers
            .iter()
            .filter(|(_, s)| applies(&s.agents, AgentKind::Antigravity))
        {
            let v = match (&s.command, &s.url) {
                (Some(command), _) => json!({"command": command, "args": s.args, "env": s.env}),
                (None, Some(url)) => json!({"url": url}),
                _ => continue,
            };
            servers.insert(name.clone(), v);
        }
        servers.insert("spandeck".into(), json!({"command": bin, "args": ["mcp"]}));
        let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default() + "\n";
        let files = [
            (
                "plugin.json",
                pretty(&json!({"name": "spandeck", "version": crate::daemon::VERSION})),
            ),
            ("mcp_config.json", pretty(&json!({"mcpServers": servers}))),
            (
                "hooks.json",
                pretty(&spandeck_agents::antigravity::hooks_json(&bin)),
            ),
            (
                MARKER,
                "Made by spandeck and rewritten when an Antigravity pane starts. Remove the folder to remove it.\n".to_string(),
            ),
        ];
        let mut problems = vec![];
        for (name, content) in files {
            if let Err(e) = write_if_changed(&dir.join(name), &content) {
                problems.push(format!("could not write {}: {e}", dir.join(name).display()));
            }
        }
        if let Err(e) = Self::write_spandeck_skill(&dir.join("skills")) {
            problems.push(format!("could not write the spandeck skill: {e}"));
        }
        problems.extend(self.apply_skills(setup));
        problems
    }

    /// Write the setup's skills where each agent finds them, and remove the
    /// ones spandeck wrote earlier that are no longer listed. Skills the user
    /// installed some other way are never touched.
    fn apply_skills(&self, setup: &AgentSetup) -> Vec<String> {
        let mut problems = vec![];
        // Skills for the agents that read them from the home folder are
        // the station of the account's to write, and to remove.
        if !self.owns_agent_home()
            && setup.skills.values().any(|s| {
                [AgentKind::Claude, AgentKind::Codex, AgentKind::Antigravity]
                    .iter()
                    .any(|k| applies(&s.agents, *k))
            })
        {
            problems.push(format!(
                "skills were not written for Claude Code, Codex or Antigravity: {}",
                self.not_owning_words()
            ));
        }
        for (kind, dir) in self.skill_dirs() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    let ours = e.path().join(MARKER).exists();
                    let wanted = setup
                        .skills
                        .get(&name)
                        .is_some_and(|s| applies(&s.agents, kind));
                    // "spandeck" is spandeck's own skill, not one from the setup.
                    if ours && !wanted && name != "spandeck" {
                        let _ = std::fs::remove_file(e.path().join("SKILL.md"));
                        let _ = std::fs::remove_file(e.path().join(MARKER));
                        let _ = std::fs::remove_dir(e.path());
                    }
                }
            }
            for (name, skill) in setup
                .skills
                .iter()
                .filter(|(_, s)| applies(&s.agents, kind))
            {
                let sdir = dir.join(name);
                if sdir.exists() && !sdir.join(MARKER).exists() {
                    problems.push(format!("{}: a skill called {name} already exists and was not installed by spandeck; left alone", kind.display()));
                    continue;
                }
                let body = format!(
                    "---\nname: {name}\ndescription: {}\n---\n\n{}\n",
                    skill.description.replace('\n', " "),
                    skill.content.trim_end()
                );
                let res = std::fs::create_dir_all(&sdir)
                    .and_then(|_| std::fs::write(sdir.join("SKILL.md"), body))
                    .and_then(|_| {
                        std::fs::write(
                            sdir.join(MARKER),
                            "Installed by spandeck agent setup. Edit it in the Control Deck.\n",
                        )
                    });
                if let Err(e) = res {
                    problems.push(format!(
                        "{}: could not write skill {name}: {e}",
                        kind.display()
                    ));
                }
            }
        }
        problems
    }

    /// Save a setup on this station and apply it.
    pub fn setup_apply(&self, setup: &AgentSetup, by: &str) -> Result<Value> {
        setup.validate().map_err(invalid)?;
        std::fs::create_dir_all(&self.paths.config_dir)?;
        let tmp = self.setup_file().with_extension("json.tmp");
        // MCP server settings can carry credentials: readable by the user only.
        spandeck_terminal::sys::write_private(
            &tmp,
            &serde_json::to_vec_pretty(setup).map_err(|e| invalid(e.to_string()))?,
        )?;
        std::fs::rename(&tmp, self.setup_file())?;
        let mut problems = self.apply_skills(setup);
        // Antigravity reads the shared MCP servers from spandeck's plugin.
        if self.owns_agent_home() && self.antigravity_plugin_dir().join(MARKER).exists() {
            problems.extend(self.antigravity_plugin(setup));
            problems.sort();
            problems.dedup();
        }
        self.record(
            "setup.applied",
            None,
            None,
            None,
            Some(by),
            json!({"mcp_servers": setup.mcp_servers.len(), "skills": setup.skills.len(), "problems": problems}),
        );
        Ok(
            json!({"station": self.station, "ok": true, "updated_ms": setup.updated_ms, "problems": problems}),
        )
    }

    /// Edit the setup here and push it to every station.
    ///
    /// What arrives is the whole setup as an editor changed it, with the
    /// stamp of the copy the editor opened. If the setup here has been
    /// changed since (at this station or another), saving would undo that
    /// change, so it is refused and the owner opens the setup again. A
    /// caller that sends no stamp (0) replaces whatever is here, as before.
    pub async fn setup_set(&self, mut setup: AgentSetup, by: &str, fleet: bool) -> Result<Value> {
        let mine = self.setup();
        if setup.updated_ms != 0 && setup.updated_ms < mine.updated_ms {
            return Err(invalid(format!(
                "not saved: the agent setup was changed ({}) after you opened it. Open it again and make your change there.",
                mine.updated_by
            )));
        }
        setup.updated_ms = self.sync_stamp("setup", mine.updated_ms);
        // The record is this station's, not the editor's.
        setup.changes = mine.changes.clone();
        self.sync_mark(&mut setup.changes, mine.updated_ms, setup.updated_ms);
        setup.updated_by = format!("{by} at {}", self.station);
        let mut results = vec![self.setup_apply(&setup, by)?];
        if fleet {
            // Every station at the same time, each within its own five
            // seconds: the answer waits for the slowest, not for the sum.
            let peers = self.fleet.peers();
            let setup = &setup;
            let sends = peers.iter().map(|peer| async move {
                if peer.state != "online" {
                    // It catches up by itself when it is next seen.
                    return json!({"station": peer.station, "ok": false, "pending": true, "why": "unreachable now; it will get this setup when it is back"});
                }
                let r = self
                    .fleet
                    .call(&peer.station, "setup.push", json!({"setup": setup}))
                    .await;
                let result = match &r {
                    // It kept its own copy: this setup is not on it. Said as
                    // not saved, never as saved.
                    Ok(v) if v.get("kept").is_some() => json!({
                        "station": peer.station, "ok": false, "kept": true,
                        "why": if v["updated_ms"].as_u64() > Some(setup.updated_ms) {
                            "it has a newer setup, which replaces this one"
                        } else {
                            "it kept the setup it has"
                        },
                    }),
                    Ok(v) => v.clone(),
                    Err(e) => json!({"station": peer.station, "ok": false, "why": e}),
                };
                self.sync_sent("setup", setup.updated_ms, &peer.station, r)
                    .await;
                result
            });
            results.extend(crate::fleet::futures_join_all(sends).await);
        }
        // If a station's newer copy replaced it, say what is here now.
        Ok(json!({"setup": self.setup(), "stations": results}))
    }

    /// Another station offering its copy. Accepted only if it is newer.
    pub fn setup_push(&self, setup: AgentSetup, from: &str) -> Result<Value> {
        self.sync_saw("setup", setup.updated_ms);
        let mine = self.setup();
        if setup.updated_ms <= mine.updated_ms {
            return Ok(
                json!({"station": self.station, "ok": true, "kept": "this station's copy is as new or newer", "updated_ms": mine.updated_ms}),
            );
        }
        self.sync_replaced("setup", &mine.changes, &setup.changes);
        let out = self.setup_apply(&setup, from)?;
        self.emit(
            "setup.updated",
            None,
            None,
            json!({"updated_ms": setup.updated_ms}),
        );
        Ok(out)
    }

    /// In a station's round: bring it up to date if its setup is older.
    pub(crate) async fn setup_offer(&self, peer: &crate::fleet::Peer, mine: &AgentSetup) {
        let theirs = peer
            .info
            .pointer("/setup/updated_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if mine.updated_ms == 0 || peer.state != "online" || theirs >= mine.updated_ms {
            return;
        }
        let r = self
            .fleet
            .call(&peer.station, "setup.push", json!({"setup": mine}))
            .await;
        self.sync_sent("setup", mine.updated_ms, &peer.station, r)
            .await;
    }

    pub fn setup_view(&self) -> Value {
        let setup = self.setup();
        let agents: Vec<Value> = spandeck_agents::catalogue::all()
            .iter()
            .map(|entry| {
                let k = entry.kind;
                let panes: Vec<Value> = self
                    .list_panes()
                    .iter()
                    .filter(|p| { let m = p.meta.lock().unwrap(); m.agent == k && m.exit.is_none() })
                    .map(|p| { let m = p.meta.lock().unwrap(); json!({"id": m.id, "name": m.name, "state": m.state, "needs_reason": m.needs_reason}) })
                    .collect();
                let probe = self.agent_probe(k, std::time::Duration::from_secs(60));
                json!({"agent": k, "name": k.display(), "installed_version": probe.version, "health": probe.health, "running": panes})
            })
            .collect();
        json!({"station": self.station, "setup": setup, "agents": agents})
    }

    /// Type a command (for example `/compact` or `/model opus`) into running
    /// agents: one pane, every agent on this station, or the whole fleet.
    /// Agents that are mid-turn are skipped unless `force` is set.
    pub async fn agents_command(
        &self,
        command: &str,
        pane: Option<&str>,
        agent: Option<AgentKind>,
        fleet: bool,
        force: bool,
        by: &str,
    ) -> Result<Value> {
        let command = command.trim();
        if command.is_empty() || command.contains('\n') || command.len() > 500 {
            return Err(invalid("a command is one short line"));
        }
        let mut sent = vec![];
        let mut skipped = vec![];
        let targets: Vec<_> = match pane {
            Some(p) => vec![self.pane(p)?],
            None => self.list_panes(),
        };
        for p in targets {
            let m = p.meta();
            if m.exit.is_some() || !m.agent.is_agent() || agent.is_some_and(|a| a != m.agent) {
                continue;
            }
            if m.state == AgentState::Working && !force {
                skipped
                    .push(json!({"pane": m.id, "name": m.name, "why": "in the middle of a turn"}));
                continue;
            }
            // A question of the program's own is on its screen: a command
            // typed now would be taken as its answer (modelswitch.rs).
            if let Some(asks) = self.prompt_up(&p) {
                skipped.push(json!({"pane": m.id, "name": m.name, "why": format!("asking {asks} in its terminal")}));
                continue;
            }
            match self.send_text(&m.id, command, true) {
                Ok(()) => sent.push(json!({"pane": m.id, "name": m.name})),
                Err(e) => skipped.push(json!({"pane": m.id, "name": m.name, "why": e.to_string()})),
            }
        }
        self.record(
            "agents.command",
            pane,
            None,
            None,
            Some(by),
            json!({"command": command, "sent": sent.len(), "skipped": skipped.len()}),
        );
        let mut stations = vec![json!({"station": self.station, "sent": sent, "skipped": skipped})];
        if fleet && pane.is_none() {
            // Every station at the same time: one that does not answer
            // holds up none of the others.
            let peers = self.fleet.peers();
            let calls = peers
                .iter()
                .filter(|p| p.state == "online")
                .map(|peer| async move {
                    let r = self
                        .fleet
                        .call(&peer.station, "rpc.forward", json!({"method": "agents.command", "params": {"command": command, "agent": agent, "force": force}}))
                        .await;
                    match r {
                        Ok(v) => v["stations"][0].clone(),
                        Err(e) => json!({"station": peer.station, "error": e}),
                    }
                });
            stations.extend(crate::fleet::futures_join_all(calls).await);
        }
        Ok(json!({"command": command, "stations": stations}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bug that happened (18 Nov 2026): the permission mode set for an
    /// agent was passed to Claude Code and Antigravity and dropped for
    /// Codex, so a Codex pane asked the owner about each step it took.
    #[test]
    fn the_permission_mode_set_for_codex_reaches_codex() {
        let with = |mode: &str| {
            let mut s = AgentSetup::default();
            s.defaults.insert(
                "codex".into(),
                AgentDefaults {
                    permission_mode: Some(mode.into()),
                    ..Default::default()
                },
            );
            s
        };
        assert_eq!(
            with("auto").launch_args(AgentKind::Codex),
            ["--approve-for-me"]
        );
        assert_eq!(
            with("never").launch_args(AgentKind::Codex),
            ["--ask-for-approval", "never"]
        );
        assert!(with("auto").validate().is_ok());
        // A word Codex has no flag for is refused, not dropped.
        assert!(with("acceptEdits").validate().is_err());
        // Nothing set: Codex starts as its own settings say.
        assert!(AgentSetup::default()
            .launch_args(AgentKind::Codex)
            .is_empty());
    }
}
