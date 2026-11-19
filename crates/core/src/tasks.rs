//! Task records, delegation, workspaces, checkpoints and sign-offs.
//!
//! Guarantees (Tasks, Workspaces):
//! - A task record exists for delegated and recipe work only.
//! - An agent going quiet is not completion: only an explicit result or a
//!   sign-off completes a task.
//! - A helper that writes gets its own worktree and branch. After a crash the
//!   worktree and branch are kept and the task is failed with resume info.
//! - A sign-off names the commit reviewed and goes stale if the code changes.

use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};
use spandeck_agents::{AgentKind, ToolCall};

use crate::daemon::{invalid, Caller, Daemon, Error, PaneMeta, Result, SpawnOpts};
use crate::git::{git, head, is_dirty, is_repo};
use crate::hooks::truncate;
use crate::orders::{Block, Layer};
use crate::paths::{new_id, now_ms};

pub const STATES: &[&str] = &[
    "queued",
    "running",
    "waiting_for_approval",
    "blocked",
    "completed",
    "failed",
    "cancelled",
];

/// The task state machine. Completed and cancelled are final; a failed task
/// can be resumed.
pub fn transition_allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("queued", "running" | "cancelled" | "failed")
            | (
                "running",
                "waiting_for_approval" | "blocked" | "completed" | "failed" | "cancelled"
            )
            | ("waiting_for_approval", "running" | "cancelled" | "failed")
            | ("blocked", "running" | "cancelled" | "failed")
            | ("failed", "running")
    )
}

fn row_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let j = |i: usize| -> rusqlite::Result<Value> {
        let s: String = r.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or(Value::Null))
    };
    Ok(json!({
        "id": r.get::<_, String>(0)?, "created": r.get::<_, i64>(1)?, "updated": r.get::<_, i64>(2)?,
        "state": r.get::<_, String>(3)?, "goal": r.get::<_, String>(4)?, "acceptance": r.get::<_, String>(5)?,
        "role": r.get::<_, Option<String>>(6)?, "agent": r.get::<_, Option<String>>(7)?,
        "station": r.get::<_, String>(8)?, "project": r.get::<_, Option<String>>(9)?,
        "pane": r.get::<_, Option<String>>(10)?, "requester": r.get::<_, Option<String>>(11)?,
        "workspace": j(12)?, "dependencies": j(13)?, "outputs": j(14)?, "review": j(15)?, "resume": j(16)?,
        "recipe": r.get::<_, Option<String>>(17)?, "parent": r.get::<_, Option<String>>(18)?,
    }))
}

const TASK_COLS: &str = "id,created,updated,state,goal,acceptance,role,agent,station,project,pane,requester,workspace,deps,outputs,review,resume,recipe,parent";

#[derive(Debug, Clone, Default)]
pub struct NewTask {
    pub goal: String,
    pub acceptance: String,
    pub role: Option<String>,
    pub agent: Option<String>,
    pub project: Option<String>,
    pub pane: Option<String>,
    pub requester: Option<String>,
    pub workspace: Value,
    pub deps: Vec<String>,
    pub recipe: Option<String>,
    pub parent: Option<String>,
    pub state: Option<String>,
}

impl Daemon {
    pub fn task_create(&self, t: NewTask) -> Result<String> {
        if t.goal.trim().is_empty() {
            return Err(invalid("a task needs a goal"));
        }
        let id = new_id("t");
        let now = now_ms() as i64;
        let state = t.state.clone().unwrap_or_else(|| "queued".into());
        self.db.with(|c| {
            c.execute(
                "INSERT INTO tasks(id,created,updated,state,goal,acceptance,role,agent,station,project,pane,requester,workspace,deps,recipe,parent)
                 VALUES(?1,?2,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                rusqlite::params![
                    id, now, state, t.goal, t.acceptance, t.role, t.agent, self.station, t.project, t.pane, t.requester,
                    if t.workspace.is_null() { "{}".to_string() } else { t.workspace.to_string() },
                    json!(t.deps).to_string(), t.recipe, t.parent
                ],
            )
        })?;
        self.record(
            "task.created",
            t.pane.as_deref(),
            t.project.as_deref(),
            Some(&id),
            t.requester.as_deref(),
            json!({"goal": truncate(&t.goal, 200), "role": t.role, "state": state}),
        );
        Ok(id)
    }

    pub fn task_get(&self, id: &str) -> Result<Value> {
        self.db
            .read(|c| {
                c.query_row(
                    &format!("SELECT {TASK_COLS} FROM tasks WHERE id=?1"),
                    [id],
                    row_task,
                )
            })
            .map_err(|_| Error::NotFound(format!("task {id}")))
    }

    pub fn tasks(&self, project: Option<&str>, active_only: bool, limit: u32) -> Vec<Value> {
        self.db
            .read(|c| {
                let mut stmt = c.prepare(&format!(
                    "SELECT {TASK_COLS} FROM tasks WHERE (?1 IS NULL OR project=?1)
                     AND (?2=0 OR state NOT IN ('completed','failed','cancelled')) ORDER BY created DESC LIMIT ?3"
                ))?;
                let rows = stmt.query_map(rusqlite::params![project, active_only as i64, limit], row_task)?;
                rows.collect()
            })
            .unwrap_or_default()
    }

    /// Move a task to a new state, merging `patch` into its JSON fields
    /// (outputs, review, resume, workspace).
    pub fn task_set_state(&self, id: &str, to: &str, patch: Value, by: &str) -> Result<Value> {
        let task = self.task_get(id)?;
        let from = task["state"].as_str().unwrap_or("");
        if from != to && !transition_allowed(from, to) {
            return Err(invalid(format!("a {from} task cannot become {to}")));
        }
        let merged = |field: &str| -> String {
            let mut cur = task[field].clone();
            if let (Some(obj), Some(add)) = (
                cur.as_object_mut(),
                patch.get(field).and_then(|p| p.as_object()),
            ) {
                for (k, v) in add {
                    obj.insert(k.clone(), v.clone());
                }
            }
            cur.to_string()
        };
        self.db.with(|c| {
            c.execute(
                "UPDATE tasks SET state=?2, updated=?3, outputs=?4, review=?5, resume=?6, workspace=?7,
                   pane=COALESCE(?8, pane) WHERE id=?1",
                rusqlite::params![
                    id, to, now_ms() as i64, merged("outputs"), merged("review"), merged("resume"), merged("workspace"),
                    patch.get("pane").and_then(|p| p.as_str())
                ],
            )
        })?;
        self.record(
            "task.state",
            task["pane"].as_str(),
            task["project"].as_str(),
            Some(id),
            Some(by),
            json!({"from": from, "to": to, "goal": truncate(task["goal"].as_str().unwrap_or(""), 120)}),
        );
        self.task_get(id)
    }

    // ------------------------------------------------------------- workspaces

    /// Prepare where a helper works. Returns {kind, path, branch?, commit}.
    fn prepare_workspace(&self, project_path: &Path, task: &str, placement: &str) -> Result<Value> {
        let commit = head(project_path);
        match placement {
            "same-tree" => Ok(
                json!({"kind": "same-tree", "path": project_path.display().to_string(), "commit": commit}),
            ),
            "snapshot" | "worktree" => {
                if !is_repo(project_path) {
                    return Err(invalid(
                        "this placement needs the project to be a git repository",
                    ));
                }
                let commit = commit
                    .ok_or_else(|| invalid("the project has no commits yet; commit first"))?;
                let dir = self.paths.worktrees_dir().join(task);
                std::fs::create_dir_all(self.paths.worktrees_dir())?;
                let dir_s = dir.display().to_string();
                let branch = format!("spandeck/{task}");
                let res = if placement == "snapshot" {
                    git(
                        project_path,
                        &["worktree", "add", "--detach", &dir_s, &commit],
                    )
                } else {
                    git(
                        project_path,
                        &["worktree", "add", "-b", &branch, &dir_s, &commit],
                    )
                };
                res.map_err(|e| invalid(format!("could not create the workspace: {e}")))?;
                Ok(json!({
                    "kind": placement, "path": dir_s, "commit": commit,
                    "branch": (placement == "worktree").then_some(branch),
                }))
            }
            other => Err(invalid(format!(
                "unknown placement {other}; use same-tree, snapshot or worktree"
            ))),
        }
    }

    /// One Git snapshot of the working tree at task start, without touching it.
    fn checkpoint(
        &self,
        path: &Path,
        project: Option<&str>,
        task: Option<&str>,
        pane: Option<&str>,
    ) -> Option<String> {
        if !is_repo(path) {
            return None;
        }
        let sha = git(path, &["stash", "create", "spandeck checkpoint"])
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| head(path))?;
        let id = new_id("c");
        let ref_name = format!("refs/spandeck/checkpoints/{}", task.unwrap_or(&id));
        let _ = git(path, &["update-ref", &ref_name, &sha]);
        let _ = self.db.with(|c| {
            c.execute(
                "INSERT INTO checkpoints(id,ts,project,task,pane,path,commit_sha,ref_name) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                rusqlite::params![id, now_ms() as i64, project, task, pane, path.display().to_string(), sha, ref_name],
            )
        });
        Some(sha)
    }

    /// Read-only diff of what an agent changed: since its task's checkpoint,
    /// or against the last commit for a pane with no task.
    pub fn diff(&self, pane: &str) -> Result<Value> {
        let p = self.pane(pane)?;
        let meta = p.meta();
        let (path, base): (String, Option<String>) = meta
            .task
            .as_ref()
            .and_then(|t| {
                self.db
                    .read(|c| {
                        c.query_row(
                            "SELECT path, commit_sha FROM checkpoints WHERE task=?1 ORDER BY ts DESC LIMIT 1",
                            [t],
                            |r| Ok((r.get::<_, String>(0)?, Some(r.get::<_, String>(1)?))),
                        )
                    })
                    .ok()
            })
            .unwrap_or((meta.cwd.clone(), None));
        let dir = Path::new(&path);
        if !is_repo(dir) {
            return Err(invalid("this pane is not in a git repository"));
        }
        let base = base.or_else(|| head(dir)).unwrap_or_else(|| "HEAD".into());
        let stat = git(dir, &["diff", "--stat", &base]).unwrap_or_default();
        let mut diff = git(dir, &["diff", "--no-color", &base]).unwrap_or_default();
        let truncated = diff.len() > 400_000;
        if truncated {
            let mut cut = 400_000;
            while !diff.is_char_boundary(cut) {
                cut -= 1;
            }
            diff.truncate(cut);
        }
        let untracked: Vec<String> = git(dir, &["ls-files", "--others", "--exclude-standard"])
            .unwrap_or_default()
            .lines()
            .take(200)
            .map(|s| s.to_string())
            .collect();
        Ok(json!({
            "pane": meta.id, "task": meta.task, "path": path, "base": base, "head": head(dir),
            "stat": stat, "diff": diff, "truncated": truncated, "untracked": untracked,
        }))
    }

    // -------------------------------------------------------------- delegation

    /// One agent hands work to another. spandeck starts a fresh pane with the
    /// brief as its first prompt; the report comes back to the requester's inbox.
    pub fn delegate(
        &self,
        caller: &Caller,
        role: &str,
        goal: &str,
        acceptance: &str,
        project: Option<&str>,
        recipe: Option<&str>,
    ) -> Result<Value> {
        let requester_meta: Option<PaneMeta> = caller
            .pane
            .as_ref()
            .and_then(|id| self.pane(id).ok())
            .map(|p| p.meta());
        let project = project
            .map(|s| s.to_string())
            .or_else(|| requester_meta.as_ref().and_then(|m| m.project.clone()))
            .ok_or_else(|| invalid("delegation needs a project (the calling pane has none)"))?;
        let (project_path, file) = self.project_file(&project)?;
        // Helpers cannot delegate further unless their role allows it.
        if let Some(m) = &requester_meta {
            if m.parent_pane.is_some() || m.task.is_some() {
                let allowed = m.role.as_deref().is_some_and(|r| file.role(r).can_delegate);
                if !allowed {
                    return Err(Error::Denied(
                        "helpers cannot delegate further (depth 1) unless their role allows it"
                            .into(),
                    ));
                }
            }
        }
        let mut settings = file.role(role);
        // A role the project's file says nothing of runs the program the
        // role library names for it, if it names one.
        if !file.roles.contains_key(role) {
            if let Some(agent) = self.roles_doc().role(role).and_then(|(r, _)| r.agent) {
                settings.agent = agent;
            }
        }
        let agent = AgentKind::parse(&settings.agent)
            .filter(|a| a.is_agent())
            .ok_or_else(|| {
                invalid(format!(
                    "role {role} names agent {:?}; use claude or codex",
                    settings.agent
                ))
            })?;
        let requester = requester_meta
            .as_ref()
            .map(|m| format!("{project}/{}", m.name))
            .unwrap_or_else(|| "you".to_string());

        let task = self.task_create(NewTask {
            goal: goal.to_string(),
            acceptance: acceptance.to_string(),
            role: Some(role.to_string()),
            agent: Some(agent.as_str().to_string()),
            project: Some(project.clone()),
            requester: Some(requester.clone()),
            recipe: recipe.map(|s| s.to_string()),
            parent: requester_meta.as_ref().and_then(|m| m.task.clone()),
            ..Default::default()
        })?;
        let workspace = match self.prepare_workspace(&project_path, &task, &settings.placement) {
            Ok(w) => w,
            Err(e) => {
                let _ = self.task_set_state(
                    &task,
                    "failed",
                    json!({"resume": {"error": e.to_string()}}),
                    "spandeck",
                );
                return Err(e);
            }
        };
        let ws_path = PathBuf::from(workspace["path"].as_str().unwrap_or_default());
        let checkpoint = self.checkpoint(&ws_path, Some(&project), Some(&task), None);

        let where_ = match workspace["kind"].as_str() {
            Some("snapshot") => format!(
                "a frozen snapshot of the project at commit {} (read it; changes here are thrown away)",
                workspace["commit"].as_str().map(|c| &c[..c.len().min(12)]).unwrap_or("?")
            ),
            Some("worktree") => format!(
                "your own worktree on branch {} (commit your work there; the requester integrates it)",
                workspace["branch"].as_str().unwrap_or("?")
            ),
            _ => "the project's working tree, shared with the requester (read and test; do not edit files unless the task says so)".to_string(),
        };
        let brief = format!(
            "You are a spandeck helper with the role \"{role}\", working for {requester} in project \"{project}\".\n\
             Task {task}: {goal}\n\
             {}\
             Workspace: {where_}.\n\
             When you are done, report with the spandeck `task_result` tool (or run: spandeck task result --summary \"...\"). \
             List each problem you found as a separate finding; report no findings if there are none. \
             Your report goes to {requester}'s inbox. Stopping without a report does not complete the task.",
            if acceptance.trim().is_empty() { String::new() } else { format!("Acceptance criteria: {acceptance}\n") },
        );
        let short = &task[task.len().saturating_sub(5)..];
        // The task names its pane, and is running, before the pane's
        // program starts. It used to be said after the pane's record was
        // written (a wait of up to a second) and its instructions were: a
        // helper that reported before that was refused ("only the pane
        // working on a task can report its result"), and one that had also
        // ended by then left a task that stayed running with nobody on it.
        // A review loop waited on such a task for good (seen once in CI,
        // on a machine with a slow disk).
        let pane_id = new_id("p");
        self.task_set_state(
            &task,
            "running",
            json!({"workspace": workspace, "pane": pane_id, "resume": {"checkpoint": checkpoint}}),
            &caller.who,
        )?;
        let pane = self.spawn_pane(SpawnOpts {
            id: Some(pane_id),
            name: Some(format!("{role}-{short}")),
            agent: Some(agent),
            program: settings.program.clone(),
            extra_args: settings.args.clone(),
            cwd: Some(ws_path.clone()),
            project: Some(project.clone()),
            role: Some(role.to_string()),
            task: Some(task.clone()),
            parent_pane: requester_meta.as_ref().map(|m| m.id.clone()),
            prompt: Some(brief),
            // A helper's folder-trust question is answered only in a
            // project the owner switched that on for; nothing a caller
            // of `delegate` sends has a say in it.
            trust_choice: self.folder_trust_for_helper(agent, &project, &requester),
            // And its review of hooks only on the owner's standing choice
            // for this station, as it is.
            hooks_trust_choice: self.hooks_trust_at_start(None, agent, &requester),
            ..Default::default()
        });
        let pane = match pane {
            Ok(p) => {
                // The helper's record is written before the requester is
                // told of it, when the database answers within a second.
                self.pane_saved(&p.id);
                p
            }
            Err(e) => {
                let _ = self.task_set_state(
                    &task,
                    "failed",
                    json!({"workspace": workspace, "resume": {"error": e.to_string()}}),
                    "spandeck",
                );
                return Err(e);
            }
        };
        self.write_instructions(&pane.meta());
        Ok(json!({"task": task, "pane": pane.id, "workspace": workspace}))
    }

    /// The explicit result that completes (or fails) a task.
    pub async fn task_result(
        &self,
        caller: &Caller,
        task: Option<&str>,
        summary: &str,
        report: Option<&str>,
        findings: Vec<String>,
        failed: bool,
    ) -> Result<Value> {
        let pane_meta = caller
            .pane
            .as_ref()
            .and_then(|id| self.pane(id).ok())
            .map(|p| p.meta());
        let task_id = task
            .map(|s| s.to_string())
            .or_else(|| pane_meta.as_ref().and_then(|m| m.task.clone()))
            .ok_or_else(|| invalid("this pane has no task to report on"))?;
        let t = self.task_get(&task_id)?;
        if let Some(m) = &pane_meta {
            if t["pane"].as_str() != Some(m.id.as_str()) {
                return Err(Error::Denied(
                    "only the pane working on a task can report its result".into(),
                ));
            }
        }
        if summary.trim().is_empty() {
            return Err(invalid("a result needs a summary"));
        }
        let ws = t["workspace"]["path"].as_str().map(PathBuf::from);
        let commit = ws.as_deref().and_then(head);
        let dir = self.paths.state_dir.join("reports");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!("{task_id}.md"));
        let mut body = format!(
            "# Task {task_id}\n\n**Goal:** {}\n\n**Summary:** {summary}\n",
            t["goal"].as_str().unwrap_or("")
        );
        if !findings.is_empty() {
            body.push_str("\n## Findings\n\n");
            for f in &findings {
                body.push_str(&format!("- {f}\n"));
            }
        }
        if let Some(r) = report {
            body.push_str(&format!("\n## Report\n\n{r}\n"));
        }
        std::fs::write(&file, &body)?;
        let state = if failed { "failed" } else { "completed" };
        let updated = self.task_set_state(
            &task_id,
            state,
            json!({"outputs": {
                "summary": summary, "findings": findings, "report_file": file.display().to_string(),
                "commit": commit, "branch": t["workspace"]["branch"],
            }}),
            &caller.who,
        )?;
        // A fix task borrowed the author's pane: give the pane back.
        if let Some(m) = &pane_meta {
            if m.parent_pane.is_none() && m.task.as_deref() == Some(task_id.as_str()) {
                if let Ok(p) = self.pane(&m.id) {
                    p.meta.lock().unwrap().task = None;
                    self.save_pane(&p);
                }
            }
        }
        if let Some(project) = t["project"].as_str() {
            for f in &findings {
                let _ = self.note_add(
                    project,
                    "finding",
                    pane_meta.as_ref().map(|m| m.name.as_str()),
                    Some(&task_id),
                    f,
                );
            }
        }
        // The summary lands in the requester's inbox.
        if let Some(requester) = t["requester"]
            .as_str()
            .filter(|r| !r.starts_with("recipe:"))
        {
            let mut text = format!("Task {task_id} {state}: {summary}");
            if !findings.is_empty() {
                text.push_str(&format!("\nFindings ({}):\n", findings.len()));
                for f in &findings {
                    text.push_str(&format!("- {f}\n"));
                }
            }
            text.push_str(&format!("\nFull report: {}", file.display()));
            if let Some(b) = t["workspace"]["branch"].as_str() {
                text.push_str(&format!("\nBranch to integrate: {b}"));
            }
            let _ = self
                .msg_send(
                    caller,
                    requester,
                    "result",
                    &text,
                    json!([{"file": file.display().to_string()}]),
                    Some(task_id.clone()),
                    Some(task_id.clone()),
                )
                .await;
        }
        Ok(updated)
    }

    /// A pane ended. If it was working on a task, the task did not complete:
    /// it failed, and keeps what is needed to resume.
    pub(crate) fn on_pane_exit_tasks(&self, meta: &PaneMeta) {
        let Some(task) = &meta.task else { return };
        let Ok(t) = self.task_get(task) else { return };
        if !matches!(
            t["state"].as_str(),
            Some("running" | "blocked" | "waiting_for_approval" | "queued")
        ) {
            return;
        }
        let _ = self.task_set_state(
            task,
            "failed",
            json!({"resume": {
                "reason": "the agent's process ended before it reported a result",
                "session_id": meta.session_id, "pane": meta.id, "exit": meta.exit,
                "worktree": t["workspace"]["path"], "branch": t["workspace"]["branch"],
            }}),
            "spandeck",
        );
        self.notify(
            "error",
            Some(&meta.id),
            meta.project.as_deref(),
            &format!("Task failed: {} ended early", meta.name),
            "Its worktree and branch were kept; the task has resume information.",
        );
    }

    // ---------------------------------------------------------------- sign-off

    /// Sign off the project's current commit.
    pub fn signoff(
        &self,
        project: &str,
        task: Option<&str>,
        by: &str,
        note: &str,
    ) -> Result<Value> {
        let path = PathBuf::from(
            self.project_path(project)
                .ok_or_else(|| Error::NotFound(format!("project {project}")))?,
        );
        let commit = head(&path).ok_or_else(|| invalid("the project has no commit to sign off"))?;
        if is_dirty(&path) {
            return Err(invalid(
                "there are uncommitted changes; a sign-off names a commit, so commit first",
            ));
        }
        let id = new_id("s");
        self.db.with(|c| {
            c.execute(
                "INSERT INTO signoffs(id,ts,project,task,commit_sha,by,note) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![id, now_ms() as i64, project, task, commit, by, note],
            )
        })?;
        self.record(
            "signoff.given",
            None,
            Some(project),
            task,
            Some(by),
            json!({"commit": commit}),
        );
        if let Some(task) = task {
            if let Ok(t) = self.task_get(task) {
                if matches!(
                    t["state"].as_str(),
                    Some("running" | "waiting_for_approval")
                ) {
                    let _ = self.task_set_state(
                        task,
                        "completed",
                        json!({"review": {"signed_off": commit, "by": by}}),
                        by,
                    );
                }
            }
        }
        self.signoff_status(project)
    }

    /// The latest sign-off and whether it still covers the code.
    pub fn signoff_status(&self, project: &str) -> Result<Value> {
        let path = PathBuf::from(
            self.project_path(project)
                .ok_or_else(|| Error::NotFound(format!("project {project}")))?,
        );
        self.signoff_status_at(project, &path)
    }

    fn signoff_status_at(&self, project: &str, path: &Path) -> Result<Value> {
        let head_now = head(path);
        let dirty = is_dirty(path);
        let last: Option<(String, i64, String, String)> = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT commit_sha, ts, by, note FROM signoffs WHERE project=?1 ORDER BY ts DESC, rowid DESC LIMIT 1",
                    [project],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
            })
            .ok();
        Ok(match last {
            Some((commit, ts, by, note)) => {
                let stale = head_now.as_deref() != Some(commit.as_str()) || dirty;
                json!({
                    "signed": true, "commit": commit, "ts": ts, "by": by, "note": note, "head": head_now, "dirty": dirty,
                    "stale": stale, "valid": !stale,
                    "why_stale": if !stale { None } else if dirty { Some("uncommitted changes since the sign-off") } else { Some("the code changed since the sign-off") },
                })
            }
            None => {
                json!({"signed": false, "valid": false, "stale": false, "head": head_now, "dirty": dirty})
            }
        })
    }

    /// Your sign-off before anything is pushed: refuse `git push` when the
    /// project asks for it (or a review loop is running) and the commit being
    /// pushed has no valid sign-off.
    pub(crate) fn push_guard(&self, meta: &PaneMeta, tool: &ToolCall) -> Option<Block> {
        let cmd = tool.command.as_deref()?;
        let project = meta.project.as_deref()?;
        let re =
            Regex::new(r"(^|[;&|(]\s*|\s)git(\s+-[^\s]+(\s+[^\s-][^\s]*)?)*\s+push(\s|$)").ok()?;
        if !re.is_match(cmd) {
            return None;
        }
        let wants = self
            .project_file(project)
            .map(|(_, f)| f.review.signoff_before_push)
            .unwrap_or(false)
            || self.active_recipe(project).is_some();
        if !wants {
            return None;
        }
        let status = self.signoff_status_at(project, Path::new(&meta.cwd)).ok()?;
        if status["valid"].as_bool() == Some(true) {
            return None;
        }
        let why = if status["signed"].as_bool() == Some(true) {
            "the last sign-off is stale because the code changed after it"
        } else {
            "this commit has no sign-off yet"
        };
        Some(Block {
            order: "signoff-before-push".into(),
            layer: Layer::Project,
            reason: format!(
                "Blocked by spandeck: this project needs the user's sign-off before anything is pushed, and {why}. Do not retry; tell the user the work is ready for sign-off."
            ),
        })
    }
}
