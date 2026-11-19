//! The folder-trust question (DECISIONS.md, "The folder-trust question").
//!
//! An agent program started in a folder it has not seen stops and asks
//! whether the folder is trusted. The owner can say, where they start the
//! pane, that the answer is yes for that folder and that program; spandeck
//! then answers that one question, once, by pressing the key the owner
//! would press, and nothing else, ever.
//!
//! - **Whose choice.** Only the owner's: the tick in the New pane dialog or
//!   `--trust-folder` (remembered for that folder and program), or a
//!   project the owner switched on. `pane.spawn` is the owner's method;
//!   an agent has no method that sets, reads or uses a choice, and a pane
//!   an agent asks for (`delegate`) is answered only inside a project the
//!   owner switched on.
//! - **Where it is kept.** In this station's own state (folders are a
//!   machine's), never in a project's file: that file travels with the
//!   repository, and a repository must not be able to say it is trusted.
//! - **What is answered.** The program's own screen, word for word, for the
//!   pane's own folder (agents/foldertrust.rs: `Exact`), first seen within
//!   `WINDOW_MS` of the program's start, before any hook of the session.
//!   Where the program's own choice is to leave (Claude Code), the marker
//!   is moved to the trusting choice, the screen is read again, and Enter
//!   is pressed only with the marker seen there (the owner's decision of
//!   16 Nov 2026; `folder_trust.move = false` turns the moving off). Once
//!   per start of the program. Anything else is left to the owner, and
//!   the pane says so.
//! - **The key** is an item of the pane's input like any other
//!   (terminal/input.rs). The programs' own trust files are never written.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spandeck_agents::foldertrust::{self as screen, Focus, Level, Seen};
use spandeck_agents::AgentKind;
use spandeck_terminal::Item;

use crate::daemon::{invalid, Daemon, Error, PaneEntry, PaneMeta, Result};
use crate::paths::now_ms;
use crate::startscreen::{Moving, Stood, Watch, MOVE_MS};
// What this shares with the review of spandeck's hooks (hookstrust.rs):
// the first screen, the settle time, once per start, and what a pane's
// record holds.
pub use crate::startscreen::{Answered, Asks, Choice, SETTLE_MS, WINDOW_MS};

const KEY: &str = "folder_trust";
/// Said in `caps` by a station that has this.
pub const CAP: &str = "folder_trust";
/// A pane's `needs_reason` while the question is on its screen. Why
/// nothing was pressed is in `trust.asks.why`: `not_chosen`,
/// `not_the_known_screen`, `own_choice_is_no`, `switched_off`.
pub const ASKS: &str = "asks whether to trust this folder: answer in the terminal";

/// A folder the owner said a program may be told is trusted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Remembered {
    pub folder: String,
    pub agent: String,
    pub at_ms: u64,
    pub by: String,
}

/// A project in whose folders agents may be started without the question
/// reaching the owner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectRule {
    pub project: String,
    pub at_ms: u64,
    pub by: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Store {
    pub folders: Vec<Remembered>,
    pub projects: Vec<ProjectRule>,
}

/// Panes that may still be at their program's folder-trust question.
pub(crate) type State = crate::startscreen::Watches<Seen>;

/// A folder as the station names it: links resolved when it exists.
pub fn canon(path: &Path) -> String {
    let full = std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();
    // Windows names a resolved path `\\?\C:\…`; a program's screen, and
    // the owner, name it `C:\…`.
    match full.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => rest.to_string(),
        _ => full,
    }
}

fn under(folder: &str, root: &str) -> bool {
    Path::new(folder).starts_with(Path::new(root))
}

impl Daemon {
    fn folder_trust_store(&self) -> Store {
        self.db
            .kv_get(KEY)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn folder_trust_save(&self, store: &Store) {
        self.db
            .kv_set(KEY, &serde_json::to_string(store).unwrap_or_default());
    }

    /// The ways a pane's folder may be written on a program's screen.
    fn folder_names(&self, cwd: &str) -> Vec<String> {
        let mut names = vec![cwd.to_string(), canon(Path::new(cwd))];
        let home = self.home.display().to_string();
        for i in 0..names.len() {
            if let Some(rest) = names[i].strip_prefix(&home) {
                if rest.is_empty() || rest.starts_with(['/', '\\']) {
                    names.push(format!("~{rest}"));
                }
            }
        }
        names.dedup();
        names
    }

    /// The owner's standing choice for starting `agent` in `folder`, if
    /// there is one: the folder itself, or a project switched on whose
    /// folder it is in.
    pub fn folder_trust_standing(
        &self,
        agent: AgentKind,
        folder: &Path,
        project: Option<&str>,
    ) -> Option<&'static str> {
        screen::info(agent)?;
        let store = self.folder_trust_store();
        let folder = canon(folder);
        if store
            .folders
            .iter()
            .any(|r| r.folder == folder && r.agent == agent.as_str())
        {
            return Some("folder");
        }
        let project = project?;
        let on = store.projects.iter().any(|p| p.project == project);
        let root = canon(Path::new(&self.project_path(project)?));
        (on && under(&folder, &root)).then_some("project")
    }

    /// The choice an owner's new pane starts with. `asked`: the tick box,
    /// when the caller sent one; not sent, the standing choice is used as
    /// it is. Nothing is remembered here: see `folder_trust_keep`.
    pub fn folder_trust_at_start(
        &self,
        asked: Option<bool>,
        agent: AgentKind,
        cwd: &Path,
        project: Option<&str>,
        by: &str,
    ) -> Option<Choice> {
        screen::info(agent)?;
        let choice = |why: &str| Choice {
            by: by.to_string(),
            why: why.to_string(),
        };
        match asked {
            Some(true) => Some(choice("folder")),
            Some(false) => None,
            None => self.folder_trust_standing(agent, cwd, project).map(choice),
        }
    }

    /// Once the owner's pane has started: the tick they sent is kept.
    /// Ticked, the folder is remembered for this program; unticked, it is
    /// forgotten.
    pub fn folder_trust_keep(&self, asked: Option<bool>, agent: AgentKind, cwd: &Path, by: &str) {
        if screen::info(agent).is_none() {
            return;
        }
        match asked {
            Some(true) => self.folder_trust_remember(&canon(cwd), agent, by),
            Some(false) => {
                let _ = self.folder_trust_remove(&canon(cwd), agent.as_str(), by);
            }
            None => {}
        }
    }

    /// The choice a helper's pane starts with: only that of a project the
    /// owner switched on. Its folder is one spandeck made for that project
    /// (the project's own, a worktree of it, or a snapshot of it).
    pub fn folder_trust_for_helper(
        &self,
        agent: AgentKind,
        project: &str,
        by: &str,
    ) -> Option<Choice> {
        screen::info(agent)?;
        self.folder_trust_store()
            .projects
            .iter()
            .any(|p| p.project == project)
            .then(|| Choice {
                by: by.to_string(),
                why: "project".to_string(),
            })
    }

    fn folder_trust_remember(&self, folder: &str, agent: AgentKind, by: &str) {
        let _one = self.folder_trust.lock.lock().unwrap();
        let mut store = self.folder_trust_store();
        if store
            .folders
            .iter()
            .any(|r| r.folder == folder && r.agent == agent.as_str())
        {
            return;
        }
        store.folders.push(Remembered {
            folder: folder.to_string(),
            agent: agent.as_str().to_string(),
            at_ms: now_ms(),
            by: by.to_string(),
        });
        self.folder_trust_save(&store);
        self.record(
            "foldertrust.remembered",
            None,
            None,
            None,
            Some(by),
            json!({"folder": folder, "agent": agent}),
        );
    }

    /// `foldertrust.remove`: the owner takes a folder off the list. A pane
    /// started on that choice and not yet asked is no longer answered.
    pub fn folder_trust_remove(&self, folder: &str, agent: &str, by: &str) -> Result<Value> {
        let _one = self.folder_trust.lock.lock().unwrap();
        let mut store = self.folder_trust_store();
        let before = store.folders.len();
        store
            .folders
            .retain(|r| !(r.folder == folder && r.agent == agent));
        let removed = before - store.folders.len();
        if removed > 0 {
            self.folder_trust_save(&store);
            self.record(
                "foldertrust.removed",
                None,
                None,
                None,
                Some(by),
                json!({"folder": folder, "agent": agent}),
            );
        }
        Ok(json!({"removed": removed}))
    }

    /// `foldertrust.project`: the owner's switch for one project, "Agents
    /// may be started in this project's folders without asking me to trust
    /// them". Off until the owner turns it on.
    pub fn folder_trust_project(&self, project: &str, on: bool, by: &str) -> Result<Value> {
        self.project_path(project)
            .ok_or_else(|| Error::NotFound(format!("project {project}")))?;
        let _one = self.folder_trust.lock.lock().unwrap();
        let mut store = self.folder_trust_store();
        let was = store.projects.iter().any(|p| p.project == project);
        if was != on {
            store.projects.retain(|p| p.project != project);
            if on {
                store.projects.push(ProjectRule {
                    project: project.to_string(),
                    at_ms: now_ms(),
                    by: by.to_string(),
                });
            }
            self.folder_trust_save(&store);
            self.record(
                "foldertrust.project",
                None,
                Some(project),
                None,
                Some(by),
                json!({"on": on}),
            );
        }
        Ok(json!({"project": project, "on": on}))
    }

    /// `foldertrust.list`: what the owner has chosen on this station.
    pub fn folder_trust_list(&self) -> Value {
        let store = self.folder_trust_store();
        let folders: Vec<Value> = store
            .folders
            .iter()
            .map(|r| {
                let kind = AgentKind::parse(&r.agent);
                json!({
                    "folder": r.folder, "agent": r.agent,
                    "agent_name": kind.map(|k| k.display()).unwrap_or(&r.agent),
                    "at_ms": r.at_ms, "by": r.by,
                    "there": Path::new(&r.folder).is_dir(),
                })
            })
            .collect();
        let projects: Vec<Value> = store
            .projects
            .iter()
            .map(|p| {
                json!({"project": p.project, "at_ms": p.at_ms, "by": p.by,
                            "path": self.project_path(&p.project)})
            })
            .collect();
        json!({
            "station": self.station, "folders": folders, "projects": projects,
            "programs": programs(),
        })
    }

    /// `foldertrust.choice`: what the New pane dialog shows before the pane
    /// is started: whether this program asks, the folder it would be asked
    /// about, and whether the tick starts on.
    pub fn folder_trust_choice(
        &self,
        agent: AgentKind,
        cwd: &Path,
        project: Option<&str>,
    ) -> Value {
        let Some(info) = screen::info(agent) else {
            return json!({"asks": false});
        };
        let standing = self.folder_trust_standing(agent, cwd, project);
        json!({
            "asks": true,
            "agent": agent, "agent_name": agent.display(),
            "folder": canon(cwd),
            "there": cwd.is_dir(),
            "on": standing.is_some(),
            "why": standing,
            "allows": info.allows, "source": info.source, "reach": info.reach,
            // What the program's own first choice is: where it is to
            // leave, spandeck presses nothing and the owner taps once.
            "pressed": self.folder_trust_would_press(agent),
        })
    }

    /// Would a standing choice be acted on for this program as its screen
    /// first stands? Claude Code's marker starts on "No, exit".
    fn folder_trust_would_press(&self, agent: AgentKind) -> bool {
        self.flag_on("folder_trust", true)
            && (agent != AgentKind::Claude || self.flag_on("folder_trust.move", true))
    }

    /// `trust` in a pane's view.
    pub(crate) fn folder_trust_view(&self, m: &PaneMeta) -> Value {
        if m.trust_choice.is_none() && m.trust_asks.is_none() && m.trust_answered.is_none() {
            return Value::Null;
        }
        json!({
            "chosen": m.trust_choice, "asks": m.trust_asks, "answered": m.trust_answered,
            "folder": m.cwd,
        })
    }

    /// Does the choice a pane was started with still stand? The owner may
    /// have taken the folder off the list, or switched the project off,
    /// since.
    fn folder_trust_stands(&self, m: &PaneMeta, choice: &Choice) -> bool {
        let store = self.folder_trust_store();
        match choice.why.as_str() {
            "folder" => {
                let folder = canon(Path::new(&m.cwd));
                store
                    .folders
                    .iter()
                    .any(|r| r.folder == folder && r.agent == m.agent.as_str())
            }
            "project" => m
                .project
                .as_ref()
                .is_some_and(|project| store.projects.iter().any(|p| &p.project == project)),
            _ => false,
        }
    }

    /// Look at every agent pane that may still be at its first screen.
    /// Returns the panes whose screen is the question, which the ordinary
    /// screen detection leaves alone: what they say comes from here.
    pub(crate) fn folder_trust_look(&self) -> HashSet<String> {
        let mut showing = HashSet::new();
        let now = now_ms();
        let panes = self.list_panes();
        self.folder_trust
            .keep_only(&panes.iter().map(|p| p.id.as_str()).collect());
        for p in panes {
            let m = {
                let m = p.meta.lock().unwrap();
                if !m.agent.is_agent() || m.agent == AgentKind::Fake {
                    continue;
                }
                // Past its first screen, as nearly every pane is: nothing
                // is read, and nothing is copied.
                let started = m.proc_started_ms.unwrap_or(m.created_ms);
                if self.folder_trust.past(&p.id, started) && m.trust_asks.is_none() {
                    continue;
                }
                m.clone()
            };
            if let Some(act) = self.folder_trust_step(&p, &m, now) {
                showing.insert(p.id.clone());
                match act {
                    Act::Wait => {}
                    Act::Answer(choice) => {
                        self.folder_trust_answered(&p, &m, "standing", &choice.by, Some(&choice))
                    }
                    Act::Move => {
                        if let Some(keys) = screen::move_to_trust(m.agent) {
                            let _ = p.term.send(Item::keys(keys));
                        }
                    }
                    Act::Say(asks) => self.folder_trust_say(&p, Some(asks)),
                }
            } else if m.trust_asks.is_some() {
                self.folder_trust_say(&p, None);
            }
        }
        showing
    }

    /// One pane's turn: what to do about its screen, or `None` when its
    /// screen is not the question.
    fn folder_trust_step(&self, p: &PaneEntry, m: &PaneMeta, now: u64) -> Option<Act> {
        let started = m.proc_started_ms.unwrap_or(m.created_ms);
        self.folder_trust
            .turn(&p.id, started, |w| self.folder_trust_decide(p, m, now, w))
    }

    fn folder_trust_decide(
        &self,
        p: &PaneEntry,
        m: &PaneMeta,
        now: u64,
        w: &mut Watch<Seen>,
    ) -> Option<Act> {
        // A hook of the session, an answer already given, a program that
        // ended: the first screen is behind it.
        let behind = m.hooks_seen || m.trust_answered.is_some() || m.exit.is_some();
        let carried = m.trust_asks.as_ref().map(|a| a.since_ms);
        // The program's notice of a newer version comes before its first
        // screen and is the owner's to answer: the question that follows
        // it is still the first.
        if !behind && screen::before_first(m.agent, &p.term.lines()) {
            w.hold(now);
            p.wake.at_screen(now);
            return None;
        }
        let read = || screen::read(m.agent, &p.term.lines(), &self.folder_names(&m.cwd));
        let (seen, since_ms) = match w.look(now, behind, carried, read) {
            Stood::NotIt => return None,
            Stood::Wait => return Some(Act::Wait),
            Stood::Stands(seen, since_ms) => (seen, since_ms),
        };
        let button = seen.level >= Level::Choices;
        let asks = |why: &str| {
            Some(Act::Say(Asks {
                since_ms,
                button,
                why: why.to_string(),
                says: None,
            }))
        };
        let Some(choice) = m
            .trust_choice
            .as_ref()
            .filter(|c| self.folder_trust_stands(m, c))
        else {
            return asks("not_chosen");
        };
        if w.auto_off || !self.flag_on("folder_trust", true) {
            return asks("switched_off");
        }
        if seen.level != Level::Exact {
            return asks("not_the_known_screen");
        }
        match seen.focus {
            Some(Focus::Trust) => {
                // Claimed here, with the watch locked: it is answered once.
                w.closed = true;
                Some(Act::Answer(choice.clone()))
            }
            // The program's own choice is to leave. Its marker is moved,
            // once, unless the owner switched that off.
            Some(Focus::Exit) if self.flag_on("folder_trust.move", true) => match w.moving(now) {
                Moving::Now => Some(Act::Move),
                Moving::Wait => Some(Act::Wait),
                Moving::Failed => asks("own_choice_is_no"),
            },
            _ => asks("own_choice_is_no"),
        }
    }

    /// Is the pane's screen one its program asks something on when it
    /// starts: the notice of a newer version, the folder-trust question,
    /// the review of hooks? Read from the screen as it is now, whatever
    /// any watch of the first screen has decided (one that closed after
    /// its time knows nothing of a question that came late). The mail
    /// nudge asks, and types nothing into such a screen: its Enter would
    /// answer it (messages.rs, `nudge_pane`).
    ///
    /// `greeted`: the program's session has started, or a hook of it ran. Its screen is then
    /// such a question only when it ends with the question's own choices;
    /// before that, any row that begins with one's words is enough.
    pub(crate) fn start_question(&self, p: &PaneEntry, agent: AgentKind, greeted: bool) -> bool {
        use spandeck_agents::hookstrust;
        let lines = p.term.lines();
        if screen::before_first(agent, &lines) {
            return true;
        }
        // The folder's names are not needed: any level is a question.
        let folder = screen::read(agent, &lines, &[]).map(|s| s.level);
        let hooks = hookstrust::read(agent, &lines).map(|s| s.level);
        if greeted {
            folder.is_some_and(|l| l >= Level::Choices)
                || hooks.is_some_and(|l| l >= Level::Choices)
        } else {
            folder.is_some() || hooks.is_some()
        }
    }

    /// Set or clear what a pane says about the question on its screen.
    fn folder_trust_say(&self, p: &PaneEntry, asks: Option<Asks>) {
        self.start_screen_say(p, |m| &mut m.trust_asks, asks, ASKS);
    }

    /// Press the confirming key, once, and put it on record.
    fn folder_trust_answered(
        &self,
        p: &PaneEntry,
        m: &PaneMeta,
        how: &str,
        by: &str,
        choice: Option<&Choice>,
    ) {
        if p.term.send(Item::keys(screen::CONFIRM)).is_err() {
            return;
        }
        {
            let mut now = p.meta.lock().unwrap();
            now.trust_answered = Some(Answered {
                at_ms: now_ms(),
                how: how.to_string(),
                by: by.to_string(),
            });
            now.trust_asks = None;
        }
        self.folder_trust_say(p, None);
        self.save_pane(p);
        let says =
            match how {
                "standing" => format!(
                "spandeck answered {}'s folder-trust question, on the standing choice for this {}",
                m.agent.display(),
                if choice.is_some_and(|c| c.why == "project") { "project" } else { "folder" }
            ),
                _ => format!(
                    "{by} answered {}'s folder-trust question with \"Trust this folder\"",
                    m.agent.display()
                ),
            };
        self.record(
            "pane.trust_answered",
            Some(&p.id),
            m.project.as_deref(),
            m.task.as_deref(),
            Some(if how == "standing" { "spandeck" } else { by }),
            json!({
                "folder": canon(Path::new(&m.cwd)), "agent": m.agent, "how": how,
                "started_by": choice.map(|c| c.by.clone()), "why": choice.map(|c| c.why.clone()),
                "name": m.name, "says": says,
            }),
        );
        self.emit("pane.updated", Some(&p.id), None, json!({}));
    }

    /// `pane.trust_folder`: the owner's own "Trust this folder" on a pane
    /// that shows the question. The marker is moved to the trusting choice
    /// when it is not on it, the screen is read again, and only then is
    /// Enter pressed. `remember`: also keep the choice for this folder and
    /// program.
    pub async fn folder_trust_tap(&self, pane: &str, remember: bool, by: &str) -> Result<Value> {
        let p = self.pane(pane)?;
        let m = p.meta();
        let not_now = || {
            invalid("this pane is not showing a folder-trust question that spandeck can read: answer it in the terminal")
        };
        let names = self.folder_names(&m.cwd);
        let read =
            || screen::read(m.agent, &p.term.lines(), &names).filter(|s| s.level >= Level::Choices);
        // Claimed with the watch locked, so that this and a standing
        // choice cannot both press.
        if !self
            .folder_trust
            .claim(pane, || m.exit.is_none() && read().is_some())
        {
            return Err(not_now());
        }
        let reopen = || self.folder_trust.reopen(pane);
        if read().and_then(|s| s.focus) == Some(Focus::Exit) {
            let keys = screen::move_to_trust(m.agent).ok_or_else(not_now)?;
            p.term.send(Item::keys(keys))?;
            let mut moved = false;
            for _ in 0..(MOVE_MS / 50) {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if read().and_then(|s| s.focus) == Some(Focus::Trust) {
                    moved = true;
                    break;
                }
            }
            if !moved {
                reopen();
                return Err(invalid("the program's marker did not move to \"Yes, I trust this folder\", so nothing was confirmed: answer it in the terminal"));
            }
        }
        if read().and_then(|s| s.focus) != Some(Focus::Trust) {
            reopen();
            return Err(not_now());
        }
        let mut choice = None;
        if remember {
            self.folder_trust_remember(&canon(Path::new(&m.cwd)), m.agent, by);
            let c = Choice {
                by: by.to_string(),
                why: "folder".to_string(),
            };
            p.meta.lock().unwrap().trust_choice = Some(c.clone());
            choice = Some(c);
        }
        self.folder_trust_answered(&p, &m, "tap", by, choice.as_ref());
        Ok(json!({"ok": true, "remembered": remember}))
    }

    /// The folder a new pane would start in, as `pane.spawn` works it out.
    pub(crate) fn spawn_folder(&self, project: Option<&str>, cwd: Option<&str>) -> PathBuf {
        match (cwd.filter(|c| !c.trim().is_empty()), project) {
            (Some(cwd), _) => self.expand_path(Path::new(cwd)),
            (None, Some(project)) => self
                .project_path(project)
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.clone()),
            (None, None) => self.home.clone(),
        }
    }
}

enum Act {
    /// The screen is the question; nothing to do yet.
    Wait,
    Answer(Choice),
    Move,
    Say(Asks),
}

/// The programs the tick box is offered for, and what each one's trust
/// allows, for the list in Agent setup.
fn programs() -> Vec<Value> {
    [AgentKind::Claude, AgentKind::Antigravity, AgentKind::Codex]
        .into_iter()
        .filter_map(|k| {
            screen::info(k).map(|i| {
                json!({"agent": k, "agent_name": k.display(), "allows": i.allows,
                       "source": i.source, "reach": i.reach})
            })
        })
        .collect()
}
