//! Projects: a name, a folder, and the panes to start in it. The definition
//! lives in the repo as `.spandeck/project.toml`, so it travels through git.
//! Projects are addressed as project@station; nothing is replicated.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spandeck_agents::AgentKind;

use crate::daemon::{invalid, Daemon, Error, PaneMeta, Result, SpawnOpts};
use crate::paths::now_ms;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaneSlot {
    /// Slot name, unique in the project. Also the pane's name.
    pub name: String,
    /// claude, codex, shell (or fake in tests).
    #[serde(default = "default_agent")]
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Sub-directory of the project to start in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

fn default_agent() -> String {
    "shell".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Role {
    #[serde(default = "default_agent")]
    pub agent: String,
    /// same-tree | snapshot | worktree
    #[serde(default = "default_placement")]
    pub placement: String,
    #[serde(default)]
    pub can_delegate: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
}

fn default_placement() -> String {
    "same-tree".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ReviewSettings {
    /// Refuse `git push` until the current commit has a sign-off.
    #[serde(default)]
    pub signoff_before_push: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_minutes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RecallSettings {
    /// Let agents in this project search other projects' history.
    #[serde(default)]
    pub cross_project: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct VaultSettings {
    /// Vault keys this project's agents may use, as names or patterns
    /// (`OPENAI_*`). Empty: none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectFile {
    pub name: String,
    #[serde(default, rename = "pane", skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<PaneSlot>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, Role>,
    #[serde(default)]
    pub review: ReviewSettings,
    #[serde(default)]
    pub recall: RecallSettings,
    #[serde(default)]
    pub vault: VaultSettings,
}

impl ProjectFile {
    pub fn load(dir: &Path) -> Result<ProjectFile> {
        let file = dir.join(".spandeck/project.toml");
        let text = std::fs::read_to_string(&file).map_err(|_| {
            Error::NotFound(format!("{} has no .spandeck/project.toml", dir.display()))
        })?;
        toml::from_str(&text).map_err(|e| invalid(format!("{}: {e}", file.display())))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let sd = dir.join(".spandeck");
        std::fs::create_dir_all(&sd)?;
        let text = toml::to_string_pretty(self).map_err(|e| invalid(e.to_string()))?;
        std::fs::write(sd.join("project.toml"), text)?;
        Ok(())
    }

    /// The role's settings, with defaults for the built-in role names.
    pub fn role(&self, name: &str) -> Role {
        self.roles.get(name).cloned().unwrap_or_else(|| Role {
            agent: "claude".into(),
            placement: match name {
                "reviewer" | "security" => "snapshot",
                "tester" => "same-tree",
                _ => "worktree",
            }
            .into(),
            can_delegate: false,
            args: vec![],
            program: None,
        })
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.')
}

impl Daemon {
    pub fn project_path(&self, name: &str) -> Option<String> {
        self.db
            .with(|c| {
                c.query_row("SELECT path FROM projects WHERE name=?1", [name], |r| {
                    r.get(0)
                })
            })
            .ok()
    }

    pub fn project_file(&self, name: &str) -> Result<(PathBuf, ProjectFile)> {
        let path = self
            .project_path(name)
            .ok_or_else(|| Error::NotFound(format!("project {name}")))?;
        let path = PathBuf::from(path);
        let file = ProjectFile::load(&path)?;
        Ok((path, file))
    }

    /// Create a project in a folder (writing its definition) or register a
    /// folder that already has one.
    pub fn project_new(
        &self,
        dir: &Path,
        name: Option<String>,
        panes: Vec<PaneSlot>,
        by: &str,
    ) -> Result<Value> {
        // `~` means the home folder, as in a shell.
        let dir = self.expand_path(dir);
        if dir.exists() && !dir.is_dir() {
            return Err(invalid(format!(
                "{} is a file, not a folder",
                dir.display()
            )));
        }
        // A folder that is not there yet is made, as a new git repository:
        // helpers that write get their own worktree, which needs one.
        let created = !dir.exists();
        if created {
            std::fs::create_dir_all(&dir)
                .map_err(|e| invalid(format!("could not create {}: {e}", dir.display())))?;
            let _ = std::process::Command::new("git")
                .arg("init")
                .arg("-q")
                .arg(&dir)
                .status();
        }
        let out = self.project_new_in(&dir, name, panes, by);
        // Nothing is left behind by a project that could not be made.
        if out.is_err() && created {
            let _ = std::fs::remove_dir_all(&dir);
        }
        out
    }

    fn project_new_in(
        &self,
        dir: &Path,
        name: Option<String>,
        panes: Vec<PaneSlot>,
        by: &str,
    ) -> Result<Value> {
        let dir = dir.canonicalize()?;
        let file = match ProjectFile::load(&dir) {
            Ok(existing) if name.as_ref().is_none_or(|n| *n == existing.name) => existing,
            Ok(existing) => {
                return Err(invalid(format!(
                    "{} already defines project {}; open that, or change its project.toml",
                    dir.display(),
                    existing.name
                )))
            }
            Err(_) => {
                let name = name.unwrap_or_else(|| {
                    dir.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "project".into())
                });
                if !valid_name(&name) {
                    return Err(invalid("project names use letters, digits, - _ . only"));
                }
                let mut seen = std::collections::BTreeSet::new();
                for p in &panes {
                    if !valid_name(&p.name) || !seen.insert(p.name.clone()) {
                        return Err(invalid(format!(
                            "pane name {:?} is used twice or is not a name (letters, digits, - _ . only). Give each pane its own name.",
                            p.name
                        )));
                    }
                    if AgentKind::parse(&p.agent).is_none() {
                        return Err(invalid(format!(
                            "unknown agent {:?}; use claude, codex or shell",
                            p.agent
                        )));
                    }
                }
                let panes = if panes.is_empty() {
                    vec![PaneSlot {
                        name: "shell".into(),
                        agent: "shell".into(),
                        role: None,
                        args: vec![],
                        program: None,
                        cwd: None,
                    }]
                } else {
                    panes
                };
                let file = ProjectFile {
                    name,
                    panes,
                    roles: BTreeMap::new(),
                    review: Default::default(),
                    recall: Default::default(),
                    vault: Default::default(),
                };
                file.save(&dir)?;
                file
            }
        };
        if let Some(other) = self
            .project_path(&file.name)
            .filter(|p| Path::new(p) != dir)
        {
            return Err(invalid(format!(
                "a project called {} already exists at {other}",
                file.name
            )));
        }
        self.db.with(|c| {
            c.execute(
                "INSERT INTO projects(name,path,added) VALUES(?1,?2,?3) ON CONFLICT(name) DO UPDATE SET path=excluded.path",
                rusqlite::params![file.name, dir.display().to_string(), now_ms() as i64],
            )
        })?;
        self.record(
            "project.added",
            None,
            Some(&file.name),
            None,
            Some(by),
            json!({"path": dir.display().to_string()}),
        );
        self.project_view(&file.name)
    }

    pub fn project_remove(&self, name: &str, by: &str) -> Result<()> {
        self.project_close(name, by)?;
        self.db
            .with(|c| c.execute("DELETE FROM projects WHERE name=?1", [name]))?;
        self.record(
            "project.removed",
            None,
            Some(name),
            None,
            Some(by),
            json!({}),
        );
        Ok(())
    }

    pub fn projects(&self) -> Vec<Value> {
        let names: Vec<String> = self
            .db
            .with(|c| {
                let mut stmt = c.prepare("SELECT name FROM projects ORDER BY name")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect()
            })
            .unwrap_or_default();
        names
            .iter()
            .filter_map(|n| self.project_view(n).ok())
            .collect()
    }

    pub fn project_view(&self, name: &str) -> Result<Value> {
        let (path, open): (String, i64) = self
            .db
            .with(|c| {
                c.query_row(
                    "SELECT path, open FROM projects WHERE name=?1",
                    [name],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
            })
            .map_err(|_| Error::NotFound(format!("project {name}")))?;
        let file = ProjectFile::load(Path::new(&path));
        let panes: Vec<Value> = self
            .list_panes()
            .iter()
            .filter(|p| p.meta.lock().unwrap().project.as_deref() == Some(name))
            .map(|p| p.view(&self.station))
            .collect();
        Ok(json!({
            "name": name, "station": self.station, "address": format!("{name}@{}", self.station),
            "path": path, "open": open != 0,
            "definition": file.as_ref().ok(),
            "problem": file.as_ref().err().map(|e| e.to_string()),
            "branch": crate::git::git(Path::new(&path), &["rev-parse", "--abbrev-ref", "HEAD"]).ok(),
            "panes": panes,
        }))
    }

    /// The last pane that filled a slot, for restoring its conversation.
    fn last_slot_meta(&self, project: &str, slot: &str) -> Option<PaneMeta> {
        let metas: Vec<String> = self
            .db
            .with(|c| {
                let mut stmt =
                    c.prepare("SELECT meta FROM panes ORDER BY created DESC LIMIT 500")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect()
            })
            .unwrap_or_default();
        metas
            .iter()
            .filter_map(|m| serde_json::from_str::<PaneMeta>(m).ok())
            .find(|m| m.project.as_deref() == Some(project) && m.slot.as_deref() == Some(slot))
    }

    /// Open a project: start every slot that is not already running. An agent
    /// slot that ran before resumes its conversation by session id.
    pub fn project_open(&self, name: &str, by: &str) -> Result<Value> {
        let (path, file) = self.project_file(name)?;
        let running: Vec<String> = self
            .list_panes()
            .iter()
            .filter_map(|p| {
                let m = p.meta.lock().unwrap();
                (m.project.as_deref() == Some(name) && m.exit.is_none())
                    .then(|| m.slot.clone())
                    .flatten()
            })
            .collect();
        let mut started = vec![];
        for slot in &file.panes {
            if running.contains(&slot.name) {
                continue;
            }
            let agent = AgentKind::parse(&slot.agent).ok_or_else(|| {
                invalid(format!(
                    "pane {}: unknown agent {:?}",
                    slot.name, slot.agent
                ))
            })?;
            let previous = self.last_slot_meta(name, &slot.name);
            let resume = previous
                .as_ref()
                .filter(|m| m.agent == agent && agent.is_agent())
                .and_then(|m| m.session_id.clone());
            let cwd = match &slot.cwd {
                Some(sub) => path.join(sub),
                None => path.clone(),
            };
            // Forget the old record so pane ids stay unique per live pane.
            if let Some(prev) = &previous {
                self.panes
                    .write()
                    .unwrap()
                    .retain(|id, p| !(id == &prev.id && p.meta.lock().unwrap().exit.is_some()));
            }
            let p = self.spawn_pane(SpawnOpts {
                name: Some(slot.name.clone()),
                agent: Some(agent),
                program: slot.program.clone(),
                extra_args: slot.args.clone(),
                cwd: Some(cwd),
                project: Some(name.to_string()),
                role: slot.role.clone(),
                resume: resume.clone(),
                slot: Some(slot.name.clone()),
                ..Default::default()
            })?;
            self.write_instructions(&p.meta());
            started.push(json!({"pane": p.id, "slot": slot.name, "resumed": resume.is_some()}));
        }
        self.db
            .with(|c| c.execute("UPDATE projects SET open=1 WHERE name=?1", [name]))?;
        self.record(
            "project.opened",
            None,
            Some(name),
            None,
            Some(by),
            json!({"started": started}),
        );
        let mut view = self.project_view(name)?;
        view["started"] = json!(started);
        Ok(view)
    }

    /// Close a project: end its panes. Their session ids are kept for reopening.
    pub fn project_close(&self, name: &str, by: &str) -> Result<()> {
        let ids: Vec<String> = self
            .list_panes()
            .iter()
            .filter(|p| p.meta.lock().unwrap().project.as_deref() == Some(name))
            .map(|p| p.id.clone())
            .collect();
        for id in ids {
            self.close(&id, by)?;
        }
        self.db
            .with(|c| c.execute("UPDATE projects SET open=0 WHERE name=?1", [name]))?;
        self.record(
            "project.closed",
            None,
            Some(name),
            None,
            Some(by),
            json!({}),
        );
        Ok(())
    }
}
