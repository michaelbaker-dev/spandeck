//! The pair recipe: a builder and a critic in one project, working through
//! stages that each end with the critic's written verdict (the owner's test
//! of 18 Nov 2026, set up by hand then). It writes how they work
//! (`PROCESS.md`, from `pair-process.md` beside this file) and what they
//! build (`BRIEF.md`), defines the panes, starts them, and sends each its
//! role as a message from the owner, once: the role's prompt from the role
//! library (roledoc.rs), filled, and after it this recipe's own line. With
//! `user`, a third pane stands in for the person the thing is made for.
//!
//! The panes work in the project's folder, the same working tree: the
//! critic runs what the builder wrote as it stands, and writes only under
//! `reviews/`. That is a rule of `PROCESS.md`, not something enforced.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spandeck_agents::AgentKind;

use super::ProjectRecipe;
use crate::daemon::{invalid, Caller, Daemon, Result};
use crate::git;
use crate::paths::now_ms;
use crate::project::{PaneSlot, ProjectFile, Role};

/// How the two work. Edited in that one file.
const PROCESS: &str = include_str!("pair-process.md");
/// What `BRIEF.md` says until the owner writes the brief.
const NO_BRIEF: &str = include_str!("pair-brief.md");
/// The line of `pair-brief.md` by which a `BRIEF.md` is known to be still
/// that file. While it is there, nobody is told to start.
const NO_BRIEF_MARK: &str =
    "Nothing is written here yet, so the builder and the critic have not been told to start.";
/// A brief is a page or a few, not an upload.
const BRIEF_MAX: usize = 256 * 1024;

pub const BUILDER: &str = "builder";
pub const CRITIC: &str = "critic";
pub const USER: &str = "user";
/// Minutes a stage, unless the owner says (`--budget`).
pub const STAGE_MINUTES: u32 = 20;

pub(super) const RECIPE: ProjectRecipe = ProjectRecipe {
    name: "pair",
    summary: "A builder and a critic in one folder. The builder writes; the critic uses \
              each slice as its user would, holds it against the brief and reviews it, and \
              each stage ends with its written verdict inside a budget. Writes PROCESS.md \
              and BRIEF.md. With --user [<agent>] a third pane stands in for the person it \
              is made for; --budget <minutes> is the time a stage gets (default 20).",
    roles: &[(BUILDER, "claude"), (CRITIC, "codex")],
};

/// What this recipe adds to a pane's role, in the same message.
pub fn recipe_line(role: &str) -> String {
    if role == USER {
        "Read BRIEF.md and PROCESS.md in this folder. Then wait: the builder asks you to \
         look after each slice is committed. Work as PROCESS.md says, without waiting for me."
            .to_string()
    } else {
        "Read BRIEF.md and PROCESS.md in this folder, then start stage 0. Work with the \
         other agents as PROCESS.md says, without waiting for me."
            .to_string()
    }
}

/// How long one look of the `user` pane gets, for a stage of `minutes`.
fn look_minutes(minutes: u32) -> u32 {
    (minutes / 2).clamp(5, 15)
}

/// The budget line a pair's pane is given in its role, when the owner
/// named a budget for the pair. Without one, the role's own stands.
pub fn budget_line(role: &str, minutes: u32) -> String {
    if role == USER {
        format!(
            "About {} minutes a look; three things.",
            look_minutes(minutes)
        )
    } else {
        format!("One round of review a stage unless there is a blocker; {minutes} minutes a stage.")
    }
}

/// `PROCESS.md` for this project, these programs and this budget.
pub fn process_text(
    project: &str,
    builder: AgentKind,
    critic: AgentKind,
    user: Option<AgentKind>,
    minutes: u32,
) -> String {
    let (user_line, user_stage) = match user {
        Some(user) => (
            format!(
                "- **user** ({}) stands in for the person this is made for. It uses what was built, never reads\n  the code, and writes `reviews/USER-<n>.md`.",
                user.display()
            ),
            format!(
                "   After each slice is committed the builder asks the **user** pane to look (about {} minutes). Its three things are answered by the builder by number, like findings: fixed, or declined with the reason.\n",
                look_minutes(minutes)
            ),
        ),
        None => (
            "- There is no `user` pane: the critic's first look at each slice stands in for one.".to_string(),
            String::new(),
        ),
    };
    PROCESS
        .replace("{{user}}", &user_line)
        .replace("{{user_stage}}\n", &user_stage)
        .replace("{{stage_minutes}}", &minutes.to_string())
        .replace("{{project}}", project)
        .replace("{{builder}}", builder.display())
        .replace("{{critic}}", critic.display())
}

/// Does this `BRIEF.md` say what to build? Not when it is empty, or still
/// the file this recipe wrote in its place.
fn is_brief(text: &str) -> bool {
    !text.trim().is_empty() && !text.contains(NO_BRIEF_MARK)
}

pub struct PairOpts {
    pub path: PathBuf,
    pub name: Option<String>,
    pub builder: Option<String>,
    pub critic: Option<String>,
    /// A third pane, `user`: `Some("")` for the builder's program.
    pub user: Option<String>,
    /// Minutes a stage. None: `STAGE_MINUTES`, and each role's own budget.
    pub budget: Option<u32>,
    pub brief: Option<String>,
    pub open: bool,
    /// The owner's word on the folder's trust (`--trust-folder`,
    /// `--no-trust-folder`). None: on for a folder this recipe makes.
    pub trust: Option<bool>,
}

fn program(role: &str, named: Option<&str>, default: &str) -> Result<AgentKind> {
    let name = named.unwrap_or(default);
    AgentKind::parse(name)
        .filter(|k| k.is_agent())
        .ok_or_else(|| {
            invalid(format!(
                "the {role} is an agent program spandeck knows (claude, codex, opencode, antigravity), not {name:?}"
            ))
        })
}

/// Write a file that is not there. False: it was, and is as it was.
fn write_new(path: &Path, text: &str) -> Result<bool> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            f.write_all(text.as_bytes())?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// The first commit of a repository that has none: the files this recipe
/// wrote and no others, whatever else is in the folder or staged. Where
/// the repository has no author configured, the commit is spandeck's; no
/// configuration is written, here or in the owner's home.
fn first_commit(dir: &Path, files: &[&str]) -> std::result::Result<String, String> {
    let mut add = vec!["add", "--"];
    add.extend(files);
    git::git(dir, &add)?;
    let configured = |key: &str| git::git(dir, &["config", key]).is_ok_and(|v| !v.is_empty());
    // Nobody is at the daemon's terminal to unlock a signing key.
    let mut commit = vec!["-c", "commit.gpgsign=false"];
    if !(configured("user.name") && configured("user.email")) {
        commit.extend([
            "-c",
            "user.name=spandeck",
            "-c",
            "user.email=spandeck@localhost",
        ]);
    }
    commit.extend([
        "commit",
        "-q",
        "-m",
        "Set up the builder and critic pair",
        "--",
    ]);
    commit.extend(files);
    git::git(dir, &commit)?;
    git::head(dir).ok_or_else(|| "the commit was not made".to_string())
}

impl Daemon {
    /// Set a folder up for a builder and a critic. Run again on the same
    /// folder it writes only what is missing and sends only what was not
    /// sent.
    pub(super) async fn pair_setup(&self, caller: &Caller, opts: PairOpts) -> Result<Value> {
        let by = caller.who.as_str();
        let builder = program(BUILDER, opts.builder.as_deref(), "claude")?;
        let critic = program(CRITIC, opts.critic.as_deref(), "codex")?;
        let user = match opts.user.as_deref() {
            None => None,
            Some(named) => Some(program(
                USER,
                Some(named).filter(|n| !n.is_empty()),
                builder.as_str(),
            )?),
        };
        if opts.budget.is_some_and(|m| !(5..=240).contains(&m)) {
            return Err(invalid(
                "the budget is the minutes a stage gets: from 5 to 240",
            ));
        }
        let brief = opts.brief.filter(|b| !b.trim().is_empty());
        if brief.as_ref().is_some_and(|b| b.len() > BRIEF_MAX) {
            return Err(invalid("a brief is at most 256 KB"));
        }
        let mut notes: Vec<String> = vec![];

        // The project: its folder (made, as a git repository, if it is not
        // there), its file, and its place in this station's list.
        let slot = |role: &str, agent: AgentKind| PaneSlot {
            name: role.into(),
            agent: agent.as_str().into(),
            role: Some(role.into()),
            args: vec![],
            program: None,
            cwd: None,
        };
        let had_file = ProjectFile::load(&self.expand_path(&opts.path)).is_ok();
        // A folder that is not there, or holds nothing, is made here: it
        // will hold only what this recipe writes.
        let fresh = std::fs::read_dir(self.expand_path(&opts.path))
            .map_or(true, |mut d| d.next().is_none());
        let view = self.project_new(
            &opts.path,
            opts.name,
            [
                (BUILDER, Some(builder)),
                (CRITIC, Some(critic)),
                (USER, user),
            ]
            .into_iter()
            .filter_map(|(role, agent)| Some(slot(role, agent?)))
            .collect(),
            by,
        )?;
        let name = view["name"].as_str().unwrap_or_default().to_string();
        let dir = PathBuf::from(view["path"].as_str().unwrap_or_default());
        if !git::is_repo(&dir) {
            if let Err(e) = git::git(&dir, &["init", "-q"]) {
                notes.push(format!("This folder is not a git repository and could not be made one ({e}). The builder is told to commit: run `git init` there."));
            }
        }

        // A project that was already defined here keeps its panes; these
        // are added if it has none by those names. The roles are placed in
        // the project's own folder: a role with no entry is given a
        // worktree of its own when an agent delegates to it.
        let mut file = ProjectFile::load(&dir)?;
        let before = file.clone();
        for (role, agent) in [
            (BUILDER, Some(builder)),
            (CRITIC, Some(critic)),
            (USER, user),
        ] {
            let Some(agent) = agent else { continue };
            if !file.panes.iter().any(|p| p.name == role) {
                file.panes.push(slot(role, agent));
            }
            let agent = file
                .panes
                .iter()
                .find(|p| p.name == role)
                .map(|p| p.agent.clone())
                .unwrap_or_default();
            file.roles.entry(role.into()).or_insert(Role {
                agent,
                placement: "same-tree".into(),
                can_delegate: false,
                args: vec![],
                program: None,
            });
        }
        if file != before {
            file.save(&dir)?;
        }
        let mut written: Vec<&str> = vec![];
        if !had_file || file != before {
            written.push(".spandeck/project.toml");
        }
        // The programs the panes really are, which an older project file
        // may have chosen otherwise.
        let mut kind = |role: &str, asked: AgentKind| {
            let slot = file.panes.iter().find(|p| p.name == role);
            let kind = slot
                .and_then(|p| AgentKind::parse(&p.agent))
                .unwrap_or(asked);
            if kind != asked {
                notes.push(format!(
                    "This project already has a pane named {role}, which runs {}. It was kept.",
                    kind.display()
                ));
            }
            kind
        };
        let (builder, critic) = (kind(BUILDER, builder), kind(CRITIC, critic));
        let user = user.map(|u| kind(USER, u));
        let minutes = opts.budget.unwrap_or(STAGE_MINUTES);

        // The two files. One that is there is the owner's, and is kept.
        let mut kept: Vec<&str> = vec![];
        if write_new(
            &dir.join("PROCESS.md"),
            &process_text(&name, builder, critic, user, minutes),
        )? {
            written.push("PROCESS.md");
        } else {
            kept.push("PROCESS.md");
            notes.push("PROCESS.md was already there and was kept as it is.".into());
        }
        let brief_file = dir.join("BRIEF.md");
        let there = std::fs::read_to_string(&brief_file).ok();
        match (&there, &brief) {
            // Somebody's brief: never replaced.
            (Some(text), given) if is_brief(text) => {
                kept.push("BRIEF.md");
                notes.push(
                    if given.is_some() {
                        "BRIEF.md was already there and was kept as it is. The brief given now was not used."
                    } else {
                        "BRIEF.md was already there and was kept as it is."
                    }
                    .into(),
                );
            }
            // No file, or one that says nothing yet: the brief goes in.
            (_, Some(given)) => {
                let mut text = given.trim_end().to_string();
                text.push('\n');
                std::fs::write(&brief_file, text)?;
                written.push("BRIEF.md");
            }
            (None, None) => {
                std::fs::write(&brief_file, NO_BRIEF.replace("{{project}}", &name))?;
                written.push("BRIEF.md");
            }
            (Some(_), None) => kept.push("BRIEF.md"),
        }
        let has_brief = std::fs::read_to_string(&brief_file).is_ok_and(|t| is_brief(&t));

        // A repository with no commit gets its first: what was written now.
        let mut commit = None;
        if git::is_repo(&dir) && git::head(&dir).is_none() && !written.is_empty() {
            match first_commit(&dir, &written) {
                Ok(id) => commit = Some(id),
                Err(e) => notes.push(format!("The files were written but not committed: {e}")),
            }
        }

        self.db.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO recipes(id,kind,created,updated,project,state,data) VALUES(?1,'pair',?2,?2,?3,'set_up','{\"sent\":{}}')",
                rusqlite::params![row_id(&name), now_ms() as i64, name],
            )
        })?;
        // The third pane and the budget, said now or when it was first run.
        if user.is_some() {
            self.db.with(|c| {
                c.execute(
                    "UPDATE recipes SET data=json_set(data, '$.user', 1) WHERE id=?1",
                    [row_id(&name)],
                )
            })?;
        }
        if let Some(minutes) = opts.budget {
            self.db.with(|c| {
                c.execute(
                    "UPDATE recipes SET data=json_set(data, '$.budget', ?2) WHERE id=?1",
                    rusqlite::params![row_id(&name), minutes],
                )
            })?;
        }
        self.record(
            "recipe.started",
            None,
            Some(&name),
            None,
            Some(by),
            json!({"id": row_id(&name), "kind": "pair", "written": written, "kept": kept}),
        );

        // The folder's trust (DECISIONS.md, "A pair's folder is trusted
        // when the recipe made it"): the project's switch, turned on at
        // the owner's word, or without it for a folder made here that is
        // a repository of its own (Claude Code keeps its trust for the
        // whole repository a folder is in).
        let own_root = git::toplevel(&dir).is_none_or(|top| {
            crate::foldertrust::canon(Path::new(&top)) == crate::foldertrust::canon(&dir)
        });
        let was_on = self.folder_trust_list()["projects"]
            .as_array()
            .is_some_and(|l| l.iter().any(|p| p["project"] == json!(name)));
        let trusted = opts.trust.unwrap_or(was_on || (fresh && own_root));
        if trusted != was_on {
            self.folder_trust_project(&name, trusted, by)?;
        }
        match (opts.trust, trusted, was_on) {
            (Some(true), ..) => notes.push("Folder trust is on for this project, as you asked: an agent that asks whether to trust this folder is answered yes.".into()),
            (None, true, false) => notes.push("This folder was made now and holds only what was written here, so folder trust is on for this project: an agent that asks whether to trust it is answered yes (--no-trust-folder leaves it to you).".into()),
            (None, false, _) => notes.push(
                if fresh {
                    "This folder is inside another git repository, which an agent's trust would cover, so each agent will ask you whether to trust it (--trust-folder answers for you)."
                } else {
                    "This folder was already there, so each agent will ask you whether to trust it (--trust-folder answers for you)."
                }
                .into(),
            ),
            _ => {}
        }

        let mut view = if opts.open {
            self.project_open(&name, by).map_err(|e| {
                invalid(format!(
                    "{name} is set up at {}, but its panes could not be started: {e}",
                    dir.display()
                ))
            })?
        } else {
            self.project_view(&name)?
        };
        let roles = self
            .pair_roles(caller, &name)
            .await
            .unwrap_or("waiting for the panes");
        let next = match roles {
            "waiting for a brief" => Some(format!(
                "Write what is to be built in {}, then run: spandeck project open {name}",
                brief_file.display()
            )),
            "waiting for the panes" => Some(format!(
                "Each agent is sent its role when its pane is running: spandeck project open {name}"
            )),
            _ => None,
        };
        view["recipe"] = json!({
            "name": "pair", "written": written, "kept": kept, "commit": commit,
            "brief": has_brief, "roles": roles, "notes": notes, "next": next,
            "folder_trust": trusted,
        });
        Ok(view)
    }

    /// What a pane of a pair is told beside its role: this recipe's line,
    /// and the budget the owner named for the pair. None: the pane is not
    /// one of a pair's.
    pub(crate) fn recipe_role_words(&self, meta: &crate::daemon::PaneMeta) -> Option<RoleWords> {
        let project = meta.project.as_deref()?;
        let role = meta.role.as_deref()?;
        if meta.name != role || !matches!(role, BUILDER | CRITIC | USER) {
            return None;
        }
        let (user, budget): (bool, Option<u32>) = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT json_extract(data, '$.user') IS NOT NULL, json_extract(data, '$.budget') FROM recipes WHERE id=?1 AND kind='pair'",
                    [row_id(project)],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
            })
            .ok()?;
        if role == USER && !user {
            return None;
        }
        Some(RoleWords {
            line: Some(recipe_line(role)),
            budget: budget.map(|m| budget_line(role, m)),
        })
    }

    /// Is this pane's role still this recipe's to send (a pair's pane that
    /// has not been told to start: there may be no brief yet)?
    pub(crate) fn recipe_sends_role(&self, meta: &crate::daemon::PaneMeta) -> bool {
        if self.recipe_role_words(meta).is_none() {
            return false;
        }
        let (Some(project), Some(role)) = (meta.project.as_deref(), meta.role.as_deref()) else {
            return false;
        };
        self.db
            .read(|c| {
                c.query_row(
                    "SELECT json_extract(data, '$.sent.' || ?2) IS NULL FROM recipes WHERE id=?1 AND kind='pair'",
                    rusqlite::params![row_id(project), role],
                    |r| r.get(0),
                )
            })
            .unwrap_or(false)
    }

    /// Send each of a pair's agents its role, if it has not been sent,
    /// a brief is written and its pane is running. None: this project is
    /// not a pair. Called when the recipe is run and whenever the project
    /// is opened, so a brief written later starts the work.
    pub(crate) async fn pair_roles(&self, caller: &Caller, project: &str) -> Option<&'static str> {
        let id = row_id(project);
        let sent = |role: &str| -> Option<bool> {
            self.db
                .with(|c| {
                    c.query_row(
                        "SELECT json_extract(data, '$.sent.' || ?2) IS NOT NULL FROM recipes WHERE id=?1 AND kind='pair'",
                        rusqlite::params![id, role],
                        |r| r.get(0),
                    )
                })
                .ok()
        };
        let with_user = self
            .db
            .with(|c| {
                c.query_row(
                    "SELECT json_extract(data, '$.user') IS NOT NULL FROM recipes WHERE id=?1 AND kind='pair'",
                    [&id],
                    |r| r.get::<_, bool>(0),
                )
            })
            .ok()?;
        let mut todo = vec![];
        for role in [BUILDER, CRITIC, USER] {
            if role == USER && !with_user {
                continue;
            }
            if !sent(role)? {
                todo.push(role);
            }
        }
        if todo.is_empty() {
            return Some("sent before");
        }
        let path = PathBuf::from(self.project_path(project)?);
        if !std::fs::read_to_string(path.join("BRIEF.md")).is_ok_and(|t| is_brief(&t)) {
            return Some("waiting for a brief");
        }
        let mut waiting = false;
        for role in todo {
            let to = format!("{project}/{role}");
            if self.pane(&to).is_err() {
                waiting = true;
                continue;
            }
            // The pane is marked first, so that it is not also sent its
            // role as any pane started with one is (roledoc.rs).
            if let Ok(pane) = self.pane(&to) {
                pane.meta.lock().unwrap().role_sent_ms = now_ms();
            }
            // Marked as sent before it is sent, and only by whoever marks
            // it first: two runs at once send one message.
            let mine = self
                .db
                .with(|c| {
                    c.execute(
                        "UPDATE recipes SET updated=?3, data=json_set(data, '$.sent.' || ?2, ?3) WHERE id=?1 AND json_extract(data, '$.sent.' || ?2) IS NULL",
                        rusqlite::params![id, role, now_ms() as i64],
                    )
                })
                .is_ok_and(|n| n == 1);
            if !mine {
                continue;
            }
            // The role's prompt from the library and this recipe's line, as
            // one message from the owner (roledoc.rs).
            let sent = match self
                .role_send(&to, &caller.who, crate::roledoc::Send::Recipe)
                .await
            {
                Ok(v) if v["sent"] == json!(true) => Ok(()),
                Ok(v) => Err(v["why"].as_str().unwrap_or("not sent").to_string()),
                Err(e) => Err(e.to_string()),
            };
            if let Err(e) = sent {
                // Not sent: it is tried again at the next opening.
                tracing::warn!(project, role, "pair: the role was not sent: {e}");
                let _ = self.db.with(|c| {
                    c.execute(
                        "UPDATE recipes SET data=json_remove(data, '$.sent.' || ?2) WHERE id=?1",
                        rusqlite::params![id, role],
                    )
                });
                waiting = true;
            }
        }
        if waiting {
            return Some("waiting for the panes");
        }
        let _ = self.db.with(|c| {
            c.execute(
                "UPDATE recipes SET state='started', updated=?2 WHERE id=?1",
                rusqlite::params![id, now_ms() as i64],
            )
        });
        Some("sent")
    }

    /// A project that is removed is no longer a pair: one made later under
    /// its name is told its roles.
    pub(crate) fn pair_forget(&self, project: &str) {
        let _ = self.db.with(|c| {
            c.execute(
                "DELETE FROM recipes WHERE id=?1 AND kind='pair'",
                [row_id(project)],
            )
        });
    }
}

/// What a recipe adds to a role's message.
pub(crate) struct RoleWords {
    pub line: Option<String>,
    pub budget: Option<String>,
}

/// One record a project, under the recipes: its name is its key.
fn row_id(project: &str) -> String {
    format!("pair:{project}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::Authority;

    /// Acceptance: the text the two agents work from names the project and
    /// both programs, begins with a stage that finds out what can be run
    /// (lesson 1 of the owner's run of 18 Nov 2026) and promises no browser;
    /// and the file that stands in for a brief is not taken for one.
    #[test]
    fn the_process_is_filled_in_and_begins_with_the_tools_stage() {
        let text = process_text("shop", AgentKind::Claude, AgentKind::Codex, None, 20);
        assert!(!text.contains("{{"), "{text}");
        for want in [
            "project `shop`",
            "**builder** (Claude Code)",
            "**critic** (Codex)",
            "0. **Tools**",
            "there may be no browser",
            "**20 minutes a stage**",
            "There is no `user` pane",
            "4. **Sign-off**",
        ] {
            assert!(text.contains(want), "{want}");
        }
        assert!(text.find("0. **Tools**") < text.find("1. **Design**"));
        // With the third pane and the owner's budget.
        let three = process_text(
            "shop",
            AgentKind::Claude,
            AgentKind::Codex,
            Some(AgentKind::Claude),
            30,
        );
        assert!(!three.contains("{{"), "{three}");
        for want in [
            "**user** (Claude Code)",
            "**30 minutes a stage**",
            "asks the **user** pane to look (about 15 minutes)",
        ] {
            assert!(three.contains(want), "{want}");
        }
        assert!(!is_brief(&NO_BRIEF.replace("{{project}}", "shop")));
        assert!(!NO_BRIEF.replace("{{project}}", "shop").contains("{{"));
        assert!(!is_brief(" \n") && is_brief("# Brief\nA shop.\n"));
    }

    /// Rule 7: setting a project up, with or without a recipe, and opening
    /// one (which sends a pair's roles) are the owner's. No agent, no other
    /// station and no read-only caller has either.
    #[test]
    fn only_the_owner_sets_a_project_up() {
        for m in ["project.new", "project.open", "project.recipes"] {
            for who in [Authority::Agent, Authority::Peer, Authority::ReadOnly] {
                assert!(!crate::rpc::allowed(who, m), "{who:?} {m}");
            }
            assert!(crate::rpc::allowed(Authority::Owner, m), "{m}");
        }
    }
}
