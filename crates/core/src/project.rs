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

/// One arrangement of a project's panes on screen, saved with the project so
/// it is the same on every device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Layout {
    pub name: String,
    /// Draw lines between panes whose agents are talking.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lines: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<LayoutNode>,
    /// Panes of the tree that are not on screen for now, each named as its
    /// leaf in the tree is. The tree keeps their places and its split sizes,
    /// so showing one puts it back where it was. A name that is not in the
    /// tree (any more) means nothing, and goes at the Control Deck's next save.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<LayoutNode>,
}

/// A pane, or two parts side by side (`row`) or one above the other (`col`).
/// A leaf names any pane: one with a name in a project, or one with no
/// project by its id; on this station or another.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged, try_from = "RawNode")]
pub enum LayoutNode {
    Pane {
        /// The pane's name in its project.
        pane: String,
        /// Another project's pane shown here as a guest. Empty: the layout's
        /// own project.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        /// Empty: the station the layout is stored on. A layout kept by
        /// the whole fleet names the station of each leaf (see deck.rs).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        station: Option<String>,
    },
    /// A pane that is in no project.
    PaneId {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        station: Option<String>,
    },
    Split {
        dir: String,
        /// The first part's share, in thousandths.
        ratio: u16,
        a: Box<LayoutNode>,
        b: Box<LayoutNode>,
    },
}

/// A node as written. Which kind it is follows from the keys it has, so a
/// leaf that names a pane both ways is refused instead of half read.
#[derive(Deserialize)]
struct RawNode {
    pane: Option<String>,
    id: Option<String>,
    project: Option<String>,
    station: Option<String>,
    dir: Option<String>,
    ratio: Option<u16>,
    a: Option<Box<LayoutNode>>,
    b: Option<Box<LayoutNode>>,
}

impl TryFrom<RawNode> for LayoutNode {
    type Error = String;
    fn try_from(n: RawNode) -> std::result::Result<LayoutNode, String> {
        let split = n.dir.is_some() || n.ratio.is_some() || n.a.is_some() || n.b.is_some();
        match (n.pane, n.id, split) {
            (None, None, true) => match (n.dir, n.ratio, n.a, n.b) {
                (Some(dir), Some(ratio), Some(a), Some(b)) => {
                    Ok(LayoutNode::Split { dir, ratio, a, b })
                }
                _ => Err("a split needs dir, ratio, a and b".into()),
            },
            (Some(pane), None, false) => Ok(LayoutNode::Pane {
                pane,
                project: n.project,
                station: n.station,
            }),
            (None, Some(_), false) if n.project.is_some() => {
                Err("a pane named by id is in no project; name it by pane and project".into())
            }
            (None, Some(id), false) => Ok(LayoutNode::PaneId {
                id,
                station: n.station,
            }),
            _ => Err(
                "a layout part is a split, or one pane: by pane (a name) or by id, not both".into(),
            ),
        }
    }
}

impl LayoutNode {
    /// The number of panes, or why this is not a layout.
    fn check(&self, depth: u32) -> std::result::Result<u32, String> {
        if depth > 8 {
            return Err("a layout is at most 8 splits deep".into());
        }
        let station_ok = |station: &Option<String>| match station.as_deref() {
            Some(s) if !valid_name(s) => Err(format!("{s:?} is not a station name")),
            _ => Ok(1),
        };
        match self {
            LayoutNode::Pane {
                pane,
                project,
                station,
            } => {
                if !valid_name(pane) || project.as_deref().is_some_and(|p| !valid_name(p)) {
                    return Err(format!("{pane:?} is not a pane name"));
                }
                station_ok(station)
            }
            LayoutNode::PaneId { id, station } => {
                if !valid_name(id) {
                    return Err(format!("{id:?} is not a pane id"));
                }
                station_ok(station)
            }
            LayoutNode::Split { dir, ratio, a, b } => {
                if dir != "row" && dir != "col" {
                    return Err("a split is a row or a col".into());
                }
                if !(50..=950).contains(ratio) {
                    return Err("a split's ratio is between 50 and 950 thousandths".into());
                }
                Ok(a.check(depth + 1)? + b.check(depth + 1)?)
            }
        }
    }
}

impl Layout {
    /// Why this cannot be saved, if it cannot.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !valid_name(&self.name) {
            return Err("layout names use letters, digits, - _ . only".into());
        }
        if let Some(tree) = &self.tree {
            if tree.check(0)? > 16 {
                return Err("a layout holds at most 16 panes".into());
            }
        }
        if self.hidden.len() > 16 {
            return Err("a layout hides at most 16 panes".into());
        }
        for leaf in &self.hidden {
            if matches!(leaf, LayoutNode::Split { .. }) {
                return Err("only a pane can be hidden, not a split".into());
            }
            leaf.check(0)?;
        }
        Ok(())
    }
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
    #[serde(default, rename = "layout", skip_serializing_if = "Vec::is_empty")]
    pub layouts: Vec<Layout>,
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

pub(crate) fn valid_name(name: &str) -> bool {
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
            .read(|c| {
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
                    layouts: vec![],
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
        self.links_forget(name);
        self.pair_forget(name);
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
            .read(|c| {
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

    /// Save (or replace) one of a project's layouts in its project file.
    pub fn project_layout_save(&self, project: &str, layout: Layout, by: &str) -> Result<Value> {
        layout.validate().map_err(invalid)?;
        let (path, mut file) = self.project_file(project)?;
        match file.layouts.iter_mut().find(|l| l.name == layout.name) {
            Some(existing) => {
                if *existing == layout {
                    return Ok(
                        json!({"project": project, "layout": layout.name, "changed": false}),
                    );
                }
                *existing = layout.clone();
            }
            None => {
                if file.layouts.len() >= 24 {
                    return Err(invalid("a project holds at most 24 layouts"));
                }
                file.layouts.push(layout.clone());
            }
        }
        file.save(&path)?;
        self.record(
            "project.layout",
            None,
            Some(project),
            None,
            Some(by),
            json!({"layout": layout.name, "saved": true}),
        );
        self.emit("project.updated", None, Some(project), json!({}));
        Ok(json!({"project": project, "layout": layout.name, "changed": true}))
    }

    pub fn project_layout_remove(&self, project: &str, name: &str, by: &str) -> Result<Value> {
        let (path, mut file) = self.project_file(project)?;
        let before = file.layouts.len();
        file.layouts.retain(|l| l.name != name);
        if file.layouts.len() == before {
            return Err(Error::NotFound(format!("layout {name} in {project}")));
        }
        file.save(&path)?;
        self.look_forget_layout(&format!("{}/{project}/{name}", self.station), by);
        self.record(
            "project.layout",
            None,
            Some(project),
            None,
            Some(by),
            json!({"layout": name, "removed": true}),
        );
        self.emit("project.updated", None, Some(project), json!({}));
        Ok(json!({"project": project, "removed": name}))
    }

    pub fn project_view(&self, name: &str) -> Result<Value> {
        let (path, open): (String, i64) = self
            .db
            .read(|c| {
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
            .map(|p| self.pane_view(p))
            .collect();
        Ok(json!({
            "name": name, "station": self.station, "address": format!("{name}@{}", self.station),
            "path": path, "open": open != 0,
            "definition": file.as_ref().ok(),
            "problem": file.as_ref().err().map(|e| e.to_string()),
            "branch": crate::git::git(Path::new(&path), &["rev-parse", "--abbrev-ref", "HEAD"]).ok(),
            "panes": panes,
            "links": self.links_of(name).into_iter()
                .map(|(with, station)| json!({"with": with, "station": station}))
                .collect::<Vec<_>>(),
        }))
    }

    /// The last pane that filled a slot, for restoring its conversation.
    fn last_slot_meta(&self, project: &str, slot: &str) -> Option<PaneMeta> {
        let metas: Vec<String> = self
            .db
            .read(|c| {
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
                .and_then(|m| self.resumable(m));
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
                // A launcher has no tick box: the owner's standing choice
                // for the folder, or for this project, is what counts.
                trust_choice: self.folder_trust_at_start(None, agent, &cwd, Some(name), by),
                hooks_trust_choice: self.hooks_trust_at_start(None, agent, by),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A layout holds any pane (the owner's rule, 14 Nov 2026), and project
    /// files written before that rule still load and are written back as
    /// they were.
    #[test]
    fn a_project_file_keeps_old_layout_leaves_and_takes_the_new_ones() {
        let old = "name = \"api\"\n\n[review]\nsignoff_before_push = false\n\n[recall]\ncross_project = false\n\n[vault]\n\n\
                   [[layout]]\nname = \"build\"\n\n[layout.tree]\ndir = \"row\"\nratio = 600\n\n\
                   [layout.tree.a]\npane = \"dev\"\n\n[layout.tree.b]\npane = \"logs\"\nproject = \"ops\"\n";
        let file: ProjectFile = toml::from_str(old).unwrap();
        assert_eq!(toml::to_string_pretty(&file).unwrap(), old);

        let new = "name = \"api\"\n[[layout]]\nname = \"all\"\n[layout.tree]\ndir = \"col\"\nratio = 500\n\
                   [layout.tree.a]\npane = \"dev\"\nproject = \"ops\"\nstation = \"spark2\"\n\
                   [layout.tree.b]\nid = \"p-01abc\"\nstation = \"spark2\"\n";
        let file: ProjectFile = toml::from_str(new).unwrap();
        assert_eq!(file.layouts[0].validate(), Ok(()));
        let again: ProjectFile = toml::from_str(&toml::to_string_pretty(&file).unwrap()).unwrap();
        assert_eq!(again, file);
        let Some(LayoutNode::Split { a, b, .. }) = &file.layouts[0].tree else {
            panic!("not a split: {file:?}");
        };
        assert_eq!(
            (&**a, &**b),
            (
                &LayoutNode::Pane {
                    pane: "dev".into(),
                    project: Some("ops".into()),
                    station: Some("spark2".into())
                },
                &LayoutNode::PaneId {
                    id: "p-01abc".into(),
                    station: Some("spark2".into())
                }
            )
        );

        // A leaf names its pane one way.
        let both = "name = \"api\"\n[[layout]]\nname = \"x\"\n[layout.tree]\npane = \"dev\"\nid = \"p-1\"\n";
        assert!(toml::from_str::<ProjectFile>(both).is_err());
    }

    /// Hidden panes are saved with the layout (the owner's request, 15 Nov
    /// 2026): they come back from a project file as they went in, a file
    /// without them is written as it was before, and what is not a pane, has
    /// a bad name or is too many is refused.
    #[test]
    fn a_layouts_hidden_panes_are_saved_with_it_and_checked() {
        let text = "name = \"api\"\n[[layout]]\nname = \"all\"\n[layout.tree]\ndir = \"col\"\nratio = 600\n\
                    [layout.tree.a]\npane = \"dev\"\n[layout.tree.b]\nid = \"p-01abc\"\nstation = \"spark2\"\n\
                    [[layout.hidden]]\nid = \"p-01abc\"\nstation = \"spark2\"\n";
        let file: ProjectFile = toml::from_str(text).unwrap();
        let layout = &file.layouts[0];
        assert_eq!(layout.validate(), Ok(()));
        assert_eq!(
            layout.hidden,
            vec![LayoutNode::PaneId {
                id: "p-01abc".into(),
                station: Some("spark2".into())
            }]
        );
        let written = toml::to_string_pretty(&file).unwrap();
        assert_eq!(toml::from_str::<ProjectFile>(&written).unwrap(), file);
        // As the Control Deck sends it, and as the deck document holds it.
        let sent = serde_json::to_value(layout).unwrap();
        assert_eq!(
            sent["hidden"],
            serde_json::json!([{"id": "p-01abc", "station": "spark2"}])
        );
        assert_eq!(&serde_json::from_value::<Layout>(sent).unwrap(), layout);

        let mut bad = layout.clone();
        bad.hidden = vec![layout.tree.clone().unwrap()];
        assert!(bad.validate().is_err(), "a split cannot be hidden");
        bad.hidden = vec![layout.hidden[0].clone(); 17];
        assert!(bad.validate().is_err(), "more than a layout holds");
        bad.hidden = vec![LayoutNode::PaneId {
            id: "<b>x</b>".into(),
            station: None,
        }];
        assert!(bad.validate().is_err(), "a hidden pane's name is checked");
    }
}
