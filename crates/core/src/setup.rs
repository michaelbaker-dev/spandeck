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
use crate::paths::now_ms;

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

    /// Where each agent looks for skills on this station.
    fn skill_dirs(&self) -> Vec<(AgentKind, PathBuf)> {
        let mut dirs = vec![
            (AgentKind::Claude, self.home.join(".claude/skills")),
            // From Codex's documentation; not checked against a real Codex here.
            (AgentKind::Codex, self.home.join(".codex/skills")),
            // A folder of spandeck's own, named to OpenCode at launch.
            (AgentKind::OpenCode, self.opencode_skills_dir()),
        ];
        // Only once an Antigravity pane has been started here.
        let plugin = self.antigravity_plugin_dir();
        if plugin.join(MARKER).exists() {
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
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(setup).map_err(|e| invalid(e.to_string()))?)?;
        }
        std::fs::rename(&tmp, self.setup_file())?;
        let mut problems = self.apply_skills(setup);
        // Antigravity reads the shared MCP servers from spandeck's plugin.
        if self.antigravity_plugin_dir().join(MARKER).exists() {
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
    pub async fn setup_set(&self, mut setup: AgentSetup, by: &str, fleet: bool) -> Result<Value> {
        setup.updated_ms = now_ms();
        setup.updated_by = format!("{by} at {}", self.station);
        let mut results = vec![self.setup_apply(&setup, by)?];
        if fleet {
            for peer in self.fleet.peers() {
                if peer.state != "online" {
                    // It catches up by itself when it is next seen.
                    results.push(json!({"station": peer.station, "ok": false, "pending": true, "why": "unreachable now; it will get this setup when it is back"}));
                    continue;
                }
                let r = self
                    .fleet
                    .call(&peer.station, "setup.push", json!({"setup": setup}))
                    .await;
                results.push(match r {
                    Ok(v) => v,
                    Err(e) => json!({"station": peer.station, "ok": false, "why": e}),
                });
            }
        }
        Ok(json!({"setup": setup, "stations": results}))
    }

    /// Another station offering its copy. Accepted only if it is newer.
    pub fn setup_push(&self, setup: AgentSetup, from: &str) -> Result<Value> {
        let mine = self.setup();
        if setup.updated_ms <= mine.updated_ms {
            return Ok(
                json!({"station": self.station, "ok": true, "kept": "this station's copy is as new or newer", "updated_ms": mine.updated_ms}),
            );
        }
        self.setup_apply(&setup, from)
    }

    /// Called after discovery: bring peers with an older setup up to date.
    pub(crate) async fn setup_sync(&self) {
        let mine = self.setup();
        if mine.updated_ms == 0 {
            return;
        }
        for peer in self.fleet.peers() {
            let theirs = peer
                .info
                .pointer("/setup/updated_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if peer.state == "online" && theirs < mine.updated_ms {
                let _ = self
                    .fleet
                    .call(&peer.station, "setup.push", json!({"setup": mine}))
                    .await;
            }
        }
    }

    pub fn setup_view(&self) -> Value {
        let setup = self.setup();
        let agents: Vec<Value> = spandeck_agents::catalogue::CATALOGUE
            .iter()
            .map(|entry| {
                let k = entry.kind;
                let panes: Vec<Value> = self
                    .list_panes()
                    .iter()
                    .filter(|p| { let m = p.meta.lock().unwrap(); m.agent == k && m.exit.is_none() })
                    .map(|p| { let m = p.meta.lock().unwrap(); json!({"id": m.id, "name": m.name, "state": m.state, "needs_reason": m.needs_reason}) })
                    .collect();
                let version = self.agent_probe(k, std::time::Duration::from_secs(60)).version;
                json!({"agent": k, "name": k.display(), "installed_version": version, "running": panes})
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
            for peer in self
                .fleet
                .peers()
                .into_iter()
                .filter(|p| p.state == "online")
            {
                let r = self
                    .fleet
                    .call(&peer.station, "rpc.forward", json!({"method": "agents.command", "params": {"command": command, "agent": agent, "force": force}}))
                    .await;
                stations.push(match r {
                    Ok(v) => v["stations"][0].clone(),
                    Err(e) => json!({"station": peer.station, "error": e}),
                });
            }
        }
        Ok(json!({"command": command, "stations": stations}))
    }
}
