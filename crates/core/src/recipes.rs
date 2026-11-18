//! Recipes: named multi-agent patterns. The first is the review loop: one
//! agent writes, another reviews a snapshot, and they repeat until the review
//! is clean, a limit is hit, or the reviewer repeats itself.
//!
//! A project recipe is a named way to set a project up (`project.new` with
//! `recipe`). There is one, the pair of a builder and a critic (`pair.rs`).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::{invalid, Caller, Daemon, Error, Result};
use crate::git::head;
use crate::paths::{new_id, now_ms};
use crate::tasks::NewTask;

mod pair;

/// A named way to set a project up: its panes, and the files they work from.
pub struct ProjectRecipe {
    pub name: &'static str,
    pub summary: &'static str,
    /// Each role's pane, and the agent program it runs unless another is named.
    pub roles: &'static [(&'static str, &'static str)],
}

/// Every project recipe there is. A new one is a line here and an arm in
/// `project_recipe`.
pub const PROJECT_RECIPES: &[ProjectRecipe] = &[pair::RECIPE];

/// The list, as `project.recipes` answers it.
pub fn project_recipes() -> Value {
    PROJECT_RECIPES
        .iter()
        .map(|r| {
            let roles: Vec<Value> = r
                .roles
                .iter()
                .map(|(role, agent)| json!({"role": role, "agent": agent}))
                .collect();
            json!({"name": r.name, "summary": r.summary, "roles": roles})
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewLoop {
    pub project: String,
    pub author_pane: String,
    pub reviewer_role: String,
    pub round: u32,
    pub max_rounds: u32,
    /// Stands in for a cost cap until usage data exists (v1.5): the whole loop
    /// stops and asks after this many minutes.
    pub max_minutes: u64,
    pub started_ms: u64,
    /// reviewing | fixing | awaiting_signoff | done | stalled | limit | failed | cancelled
    pub phase: String,
    pub task: Option<String>,
    pub commit: Option<String>,
    /// Fingerprints of the findings raised in each finished round.
    pub history: Vec<Vec<String>>,
    pub last_findings: Vec<String>,
    pub note: Option<String>,
}

/// Two findings are "the same" when their words match, whatever the punctuation.
pub fn fingerprint(finding: &str) -> String {
    finding
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Stall check: has the reviewer raised any of these findings before?
pub fn repeated(history: &[Vec<String>], findings: &[String]) -> Option<String> {
    findings
        .iter()
        .find(|f| {
            let fp = fingerprint(f);
            !fp.is_empty() && history.iter().any(|round| round.contains(&fp))
        })
        .cloned()
}

impl Daemon {
    /// Set a project up by a recipe: `project.new` with `recipe`.
    pub async fn project_recipe(
        &self,
        caller: &Caller,
        recipe: &str,
        params: &Value,
    ) -> Result<Value> {
        let text = |key: &str| params.get(key).and_then(|v| v.as_str()).map(String::from);
        match recipe {
            "pair" => {
                let path = text("path").ok_or_else(|| invalid("missing parameter: path"))?;
                self.pair_setup(
                    caller,
                    pair::PairOpts {
                        path: path.into(),
                        name: text("name"),
                        builder: text(pair::BUILDER),
                        critic: text(pair::CRITIC),
                        brief: text("brief"),
                        open: params.get("open").and_then(|v| v.as_bool()) == Some(true),
                        trust: params.get("trust_folder").and_then(|v| v.as_bool()),
                    },
                )
                .await
            }
            _ => Err(invalid(format!(
                "there is no recipe called {recipe:?}; there is: {}",
                PROJECT_RECIPES
                    .iter()
                    .map(|r| r.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    fn recipe_save(&self, id: &str, r: &ReviewLoop) {
        let active = matches!(
            r.phase.as_str(),
            "reviewing" | "fixing" | "awaiting_signoff"
        );
        let _ = self.db.with(|c| {
            c.execute(
                "UPDATE recipes SET updated=?2, state=?3, data=?4 WHERE id=?1",
                rusqlite::params![
                    id,
                    now_ms() as i64,
                    if active { "active" } else { r.phase.as_str() },
                    serde_json::to_string(r).unwrap_or_default()
                ],
            )
        });
        self.emit(
            "recipe.updated",
            Some(&r.author_pane),
            Some(&r.project),
            json!({"id": id, "phase": r.phase, "round": r.round, "note": r.note}),
        );
    }

    /// The review loops (`recipe.list`). A pair's record is not among them:
    /// the Control Deck draws every row here as a loop with rounds.
    pub fn recipes(&self, project: Option<&str>) -> Vec<Value> {
        self.db
            .read(|c| {
                let mut stmt = c.prepare("SELECT id,kind,created,updated,state,data FROM recipes WHERE kind='review_loop' AND (?1 IS NULL OR project=?1) ORDER BY created DESC LIMIT 50")?;
                let rows = stmt.query_map([project], |r| {
                    let data: String = r.get(5)?;
                    Ok(json!({
                        "id": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "created": r.get::<_, i64>(2)?,
                        "updated": r.get::<_, i64>(3)?, "state": r.get::<_, String>(4)?,
                        "data": serde_json::from_str::<Value>(&data).unwrap_or(Value::Null),
                    }))
                })?;
                rows.collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn active_recipe(&self, project: &str) -> Option<String> {
        self.db
            .read(|c| {
                c.query_row(
                    "SELECT id FROM recipes WHERE project=?1 AND state='active' LIMIT 1",
                    [project],
                    |r| r.get(0),
                )
            })
            .ok()
    }

    /// Start a review loop on the author's current commit.
    pub fn review_start(
        &self,
        caller: &Caller,
        author_pane: &str,
        reviewer_role: Option<&str>,
    ) -> Result<Value> {
        let author = self.pane(author_pane)?;
        let meta = author.meta();
        let project = meta
            .project
            .clone()
            .ok_or_else(|| invalid("the author's pane is not in a project"))?;
        if self.active_recipe(&project).is_some() {
            return Err(invalid("a review loop is already running in this project"));
        }
        let (path, file) = self.project_file(&project)?;
        let commit =
            head(&path).ok_or_else(|| invalid("nothing to review: the project has no commits"))?;
        let id = new_id("r");
        let mut r = ReviewLoop {
            project: project.clone(),
            author_pane: author.id.clone(),
            reviewer_role: reviewer_role.unwrap_or("reviewer").to_string(),
            round: 1,
            max_rounds: file.review.max_rounds.unwrap_or(3),
            max_minutes: file.review.max_minutes.unwrap_or(90),
            started_ms: now_ms(),
            phase: "reviewing".into(),
            task: None,
            commit: Some(commit),
            history: vec![],
            last_findings: vec![],
            note: None,
        };
        self.db.with(|c| {
            c.execute(
                "INSERT INTO recipes(id,kind,created,updated,project,state,data) VALUES(?1,'review_loop',?2,?2,?3,'active','{}')",
                rusqlite::params![id, now_ms() as i64, project],
            )
        })?;
        if let Err(e) = self.review_round(&id, &mut r) {
            r.phase = "failed".into();
            r.note = Some(e.to_string());
            self.recipe_save(&id, &r);
            return Err(e);
        }
        self.recipe_save(&id, &r);
        self.record(
            "recipe.started",
            Some(&author.id),
            Some(&project),
            None,
            Some(&caller.who),
            json!({"id": id, "kind": "review_loop"}),
        );
        Ok(json!({"id": id, "recipe": r}))
    }

    fn review_round(&self, id: &str, r: &mut ReviewLoop) -> Result<()> {
        let commit = r.commit.clone().unwrap_or_default();
        let goal = format!(
            "Review round {} of {}: review the code at commit {} for bugs, regressions and missed requirements. \
             Run `git log -5 --stat` and `git show {}` to see what changed. Report each problem as one finding with file and line; report zero findings if the change is good.",
            r.round,
            r.max_rounds,
            &commit[..commit.len().min(12)],
            &commit[..commit.len().min(12)],
        );
        let recipe_caller = Caller {
            authority: crate::daemon::Authority::Owner,
            pane: None,
            client: "recipe".into(),
            who: format!("recipe:{id}"),
        };
        let out = self.delegate(
            &recipe_caller,
            &r.reviewer_role,
            &goal,
            "Every finding names a file and what is wrong.",
            Some(&r.project),
            Some(id),
        )?;
        let task = out["task"].as_str().unwrap_or_default().to_string();
        // The recipe, not a pane, is waiting for this result.
        let _ = self.db.with(|c| {
            c.execute(
                "UPDATE tasks SET requester=?2 WHERE id=?1",
                rusqlite::params![task, format!("recipe:{id}")],
            )
        });
        r.task = Some(task);
        r.phase = "reviewing".into();
        Ok(())
    }

    pub fn review_cancel(&self, id: &str, by: &str) -> Result<()> {
        let mut r = self.recipe_load(id)?;
        r.phase = "cancelled".into();
        r.note = Some(format!("cancelled by {by}"));
        self.recipe_save(id, &r);
        Ok(())
    }

    fn recipe_load(&self, id: &str) -> Result<ReviewLoop> {
        let data: String = self
            .db
            .read(|c| c.query_row("SELECT data FROM recipes WHERE id=?1", [id], |r| r.get(0)))
            .map_err(|_| Error::NotFound(format!("recipe {id}")))?;
        serde_json::from_str(&data).map_err(|e| invalid(e.to_string()))
    }

    fn stop_and_ask(&self, id: &str, r: &mut ReviewLoop, phase: &str, note: String) {
        r.phase = phase.into();
        r.note = Some(note.clone());
        self.recipe_save(id, r);
        self.notify(
            "needs_you",
            Some(&r.author_pane),
            Some(&r.project),
            "Review loop stopped and needs you",
            &note,
        );
        self.record(
            "recipe.stopped",
            Some(&r.author_pane),
            Some(&r.project),
            None,
            None,
            json!({"id": id, "phase": phase, "note": note}),
        );
    }

    /// Advance every active review loop by at most one step.
    pub(crate) async fn pump_recipes(&self) {
        let active: Vec<String> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare("SELECT id FROM recipes WHERE state='active'")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect()
            })
            .unwrap_or_default();
        for id in active {
            let Ok(mut r) = self.recipe_load(&id) else {
                continue;
            };
            if matches!(r.phase.as_str(), "reviewing" | "fixing")
                && now_ms().saturating_sub(r.started_ms) > r.max_minutes * 60_000
            {
                let m = r.max_minutes;
                self.stop_and_ask(
                    &id,
                    &mut r,
                    "limit",
                    format!("The loop has run for more than {m} minutes."),
                );
                continue;
            }
            match r.phase.as_str() {
                "reviewing" => {
                    let Some(task) = r.task.clone() else { continue };
                    let Ok(t) = self.task_get(&task) else {
                        continue;
                    };
                    match t["state"].as_str() {
                        Some("completed") => {
                            let findings: Vec<String> = t["outputs"]["findings"]
                                .as_array()
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|f| f.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default();
                            self.review_done(&id, &mut r, findings).await;
                        }
                        Some("failed" | "cancelled") => {
                            self.stop_and_ask(
                                &id,
                                &mut r,
                                "failed",
                                "The reviewer ended without a result.".into(),
                            );
                        }
                        _ => {}
                    }
                }
                "fixing" => {
                    let Some(task) = r.task.clone() else { continue };
                    let Ok(t) = self.task_get(&task) else {
                        continue;
                    };
                    match t["state"].as_str() {
                        Some("completed") => {
                            let path = self.project_path(&r.project).unwrap_or_default();
                            r.commit = head(Path::new(&path));
                            r.round += 1;
                            if let Err(e) = self.review_round(&id, &mut r) {
                                self.stop_and_ask(
                                    &id,
                                    &mut r,
                                    "failed",
                                    format!("Could not start the next review: {e}"),
                                );
                            } else {
                                self.recipe_save(&id, &r);
                            }
                        }
                        Some("failed" | "cancelled") => {
                            self.stop_and_ask(
                                &id,
                                &mut r,
                                "failed",
                                "The author ended without fixing the findings.".into(),
                            );
                        }
                        _ => {}
                    }
                }
                "awaiting_signoff" => {
                    if let Ok(s) = self.signoff_status(&r.project) {
                        if s["valid"].as_bool() == Some(true)
                            && s["commit"].as_str() == r.commit.as_deref()
                        {
                            r.phase = "done".into();
                            r.note = Some("signed off".into());
                            self.recipe_save(&id, &r);
                            self.record(
                                "recipe.finished",
                                Some(&r.author_pane),
                                Some(&r.project),
                                None,
                                None,
                                json!({"id": id, "rounds": r.round}),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    async fn review_done(&self, id: &str, r: &mut ReviewLoop, findings: Vec<String>) {
        r.last_findings = findings.clone();
        if findings.is_empty() {
            r.phase = "awaiting_signoff".into();
            r.note = Some(format!("Review clean after {} round(s). Waiting for your sign-off before anything is pushed.", r.round));
            self.recipe_save(id, r);
            self.notify(
                "needs_you",
                Some(&r.author_pane),
                Some(&r.project),
                "Review is clean: ready for your sign-off",
                r.note.as_deref().unwrap_or(""),
            );
            return;
        }
        // Stall check: the same finding twice means the loop is not converging.
        if let Some(same) = repeated(&r.history, &findings) {
            self.stop_and_ask(
                id,
                r,
                "stalled",
                format!("The reviewer raised the same finding twice: {same}"),
            );
            return;
        }
        r.history
            .push(findings.iter().map(|f| fingerprint(f)).collect());
        if r.round >= r.max_rounds {
            let (rounds, n) = (r.max_rounds, findings.len());
            self.stop_and_ask(
                id,
                r,
                "limit",
                format!("{rounds} review rounds are done and {n} finding(s) remain."),
            );
            return;
        }
        // Hand the findings to the author as a task in its own pane.
        let goal = format!(
            "Fix the {} finding(s) from review round {} and commit the fix.",
            findings.len(),
            r.round
        );
        let task = match self.task_create(NewTask {
            goal: goal.clone(),
            acceptance: "Every finding is fixed or answered, and the fix is committed.".into(),
            role: Some("author".into()),
            project: Some(r.project.clone()),
            pane: Some(r.author_pane.clone()),
            requester: Some(format!("recipe:{id}")),
            recipe: Some(id.to_string()),
            state: Some("running".into()),
            ..Default::default()
        }) {
            Ok(t) => t,
            Err(e) => {
                self.stop_and_ask(id, r, "failed", e.to_string());
                return;
            }
        };
        if let Ok(p) = self.pane(&r.author_pane) {
            p.meta.lock().unwrap().task = Some(task.clone());
            self.save_pane(&p);
        }
        let mut body = format!("{goal}\n");
        for f in &findings {
            body.push_str(&format!("- {f}\n"));
        }
        body.push_str("\nWhen the fix is committed, report with the spandeck `task_result` tool (or run: spandeck task result --summary \"...\"). The reviewer then looks again.");
        let recipe_caller = Caller {
            authority: crate::daemon::Authority::Owner,
            pane: None,
            client: "recipe".into(),
            who: format!("recipe:{id}"),
        };
        if let Err(e) = self
            .msg_send(
                &recipe_caller,
                &r.author_pane,
                "task",
                &body,
                json!([]),
                None,
                Some(task.clone()),
            )
            .await
        {
            self.stop_and_ask(id, r, "failed", format!("Could not reach the author: {e}"));
            return;
        }
        r.task = Some(task);
        r.phase = "fixing".into();
        r.note = Some(format!(
            "Round {}: {} finding(s) sent to the author.",
            r.round,
            findings.len()
        ));
        self.recipe_save(id, r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_finding_twice_is_a_stall_whatever_the_punctuation() {
        let history = vec![vec![fingerprint("src/a.rs:10 — off-by-one in the loop")]];
        assert!(repeated(
            &history,
            &["src/a.rs:10 off by one in the loop.".to_string()]
        )
        .is_some());
        assert!(repeated(&history, &["src/b.rs:4 missing null check".to_string()]).is_none());
        assert!(repeated(&[], &["anything".to_string()]).is_none());
    }
}
