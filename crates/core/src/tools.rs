//! Tools on this station: installing what the tools document names for it,
//! saying how that went, and handing an agent only what is ready.
//!
//! Each station acts for itself. The document (tooldoc.rs) says which
//! tools, where, and which version; this station reads it when it starts
//! and when it changes, and installs what is for it and not here yet. No
//! station installs on another, and nothing here waits for another
//! station. An install is a job this station keeps, one at a time per
//! tool, with the same words and the same shape as an agent program's job
//! (agents.rs): the owner sees the same on every device, whenever they
//! look.
//!
//! What is run comes from the catalogue (toolcat.rs), or from this
//! station's own `[tools.<id>]` config. Nothing here takes a command or an
//! address from a caller or from another station: the document names a
//! catalogue entry and a version.
//!
//! Where things are, under this station's state directory:
//!   tools/runtime/node-<version>/     the Node spandeck downloaded
//!   tools/<name>/<version>/           one installed version of a tool
//!   tools/<name>/data/                what the tool keeps between versions
//!                                     (Playwright: its browsers)
//! A version is ready when `.spandeck-ready` is in its folder: written
//! only after the tool was started once. Nothing is installed globally and
//! nothing is written outside this folder.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spandeck_agents::{catalogue, AgentKind};
use spandeck_terminal::sys;

use crate::agents::{last_line, minutes, tail, Ended, VERSION_LIMIT};
use crate::daemon::{invalid, shell_join, Daemon, Result};
use crate::latest::Seen;
use crate::paths::{new_id, now_ms};
use crate::setup::{AgentSetup, McpServer};
use crate::toolcat::{self, Entry};
use crate::tooldoc::Tool;

/// What a station says it has (`station.info` caps): the tools document,
/// `tools.status` and its jobs. A station without it "needs a newer
/// spandeck".
pub const CAP: &str = "tools";
const READY: &str = ".spandeck-ready";
/// What a command printed is kept to this many characters (its end).
const OUTPUT_KEPT: usize = 6000;
/// The agents a tool can be given to.
const KINDS: [AgentKind; 4] = [
    AgentKind::Claude,
    AgentKind::Codex,
    AgentKind::OpenCode,
    AgentKind::Antigravity,
];

/// An install of one tool on this station, running or ended. There is one
/// per tool: the newest. The same shape as an agent program's job.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ToolJob {
    pub id: String,
    /// `install`, `upgrade` or `rollback`.
    pub action: String,
    pub tool: String,
    /// `running` or `done`.
    pub state: String,
    /// While it runs: `runtime` (spandeck's Node), `package`, `setup`
    /// (what the tool fetches for itself), `starting` (started once).
    pub step: String,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// Who made the change this station is acting on: "you at alpha".
    pub by: String,
    /// What is run now, as the owner would type it.
    pub command: Option<String>,
    /// The last line it has printed so far.
    pub last_line: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    /// `installed`, `upgraded`, `rolled_back`, `failed`, `needs` (it
    /// installed, and needs something this station lacks), `interrupted`.
    pub outcome: Option<String>,
    /// For a failure: why, in the program's own line.
    pub reason: Option<String>,
    /// How it ended, in the words the owner reads: "Installed 0.0.85".
    pub says: Option<String>,
    /// What the commands printed (its end, bounded).
    pub output: Option<String>,
    /// Panes restarted on the new version, and those that will be when
    /// their turn is over.
    pub restarted: Vec<String>,
    pub restart_when_idle: Vec<String>,
    pub restart: bool,
    pub limit_secs: u64,
}

impl ToolJob {
    fn running(&self) -> bool {
        self.state == "running"
    }

    fn brief(&self) -> Value {
        json!({"id": self.id, "action": self.action, "tool": self.tool, "started_ms": self.started_ms, "by": self.by, "state": self.state, "to": self.to})
    }
}

#[derive(Default)]
pub struct ToolsState {
    /// One change to the document at a time.
    pub(crate) doc_lock: Mutex<()>,
    /// The newest job of each tool, by name. One runs at a time per tool.
    jobs: Mutex<HashMap<String, ToolJob>>,
    /// What each catalogue entry's registry last answered, by entry id.
    latest: Mutex<HashMap<String, Seen>>,
    checking: Mutex<HashSet<String>>,
    /// One download of the runtime at a time.
    runtime_lock: Mutex<()>,
}

/// Why an install did not end in a tool that is ready.
struct Fail {
    /// It installed, and does not start here: something the station lacks.
    needs: bool,
    line: String,
}

fn failed(line: impl Into<String>) -> Fail {
    Fail {
        needs: false,
        line: line.into(),
    }
}

/// What a pane is started with, as this station has it now.
#[derive(Default)]
pub(crate) struct Wiring {
    /// The servers an agent is given: the agent setup's, the custom tools
    /// in scope here, and the catalogue tools that are ready here.
    pub servers: BTreeMap<String, McpServer>,
    /// Tools ticked "without asking", with the agents each is for.
    allow: Vec<(String, Vec<String>)>,
    /// Tools chosen for this station that a pane started now does not
    /// get, the agents each is for, and why.
    without: Vec<(String, Vec<String>, String)>,
}

fn applies(agents: &[String], kind: AgentKind) -> bool {
    agents.is_empty() || agents.iter().any(|a| a == kind.as_str())
}

impl Wiring {
    /// The tools this agent may use without the owner being asked.
    pub fn allowed(&self, kind: AgentKind) -> Vec<&str> {
        self.allow
            .iter()
            .filter(|(name, agents)| applies(agents, kind) && self.servers.contains_key(name))
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// For a pane's record: the tools it was started without, and why.
    pub fn without(&self, kind: AgentKind) -> Vec<Value> {
        self.without
            .iter()
            .filter(|(_, agents, _)| applies(agents, kind))
            .map(|(name, _, why)| json!({"tool": name, "why": why}))
            .collect()
    }

    pub fn with(&self, kind: AgentKind) -> Vec<&str> {
        self.servers
            .iter()
            .filter(|(_, s)| applies(&s.agents, kind))
            .map(|(name, _)| name.as_str())
            .collect()
    }
}

fn job_key(name: &str) -> String {
    format!("tools.job.{name}")
}

fn current_key(name: &str) -> String {
    format!("tools.current.{name}")
}

fn latest_key(id: &str) -> String {
    format!("tools.latest.{id}")
}

/// `upgrade` or `rollback`, by which of the two versions is the later.
fn change_kind(from: Option<&str>, to: &str) -> &'static str {
    match from {
        None => "install",
        Some(f) if f == to => "install",
        Some(f) if catalogue::is_newer(to, f) == Some(false) => "rollback",
        Some(_) => "upgrade",
    }
}

impl Daemon {
    fn tools_root(&self) -> PathBuf {
        self.paths.state_dir.join("tools")
    }

    fn tool_home(&self, name: &str) -> PathBuf {
        self.tools_root().join(name)
    }

    fn tool_dir(&self, name: &str, version: &str) -> PathBuf {
        self.tool_home(name).join(version)
    }

    fn tool_data(&self, name: &str) -> PathBuf {
        self.tool_home(name).join("data")
    }

    fn tool_runtime_dir(&self) -> PathBuf {
        self.tools_root()
            .join("runtime")
            .join(format!("node-{}", toolcat::NODE_VERSION))
    }

    fn tool_cfg(&self, tool: &Tool) -> Option<&crate::paths::ToolLauncher> {
        self.cfg.tools.get(tool.catalogue.as_deref()?)
    }

    /// The program a catalogue tool is started with: spandeck's own Node,
    /// by its whole path.
    fn tool_runtime_program(&self, tool: &Tool) -> PathBuf {
        match self.tool_cfg(tool).and_then(|c| c.runtime.clone()) {
            Some(program) => PathBuf::from(program),
            None => toolcat::node_files(&self.tool_runtime_dir()).0,
        }
    }

    /// Is this version of a tool installed here and known to start?
    fn tool_ready(&self, name: &str, tool: &Tool, version: &str) -> bool {
        let dir = self.tool_dir(name, version);
        let entry = tool.catalogue.as_deref().and_then(toolcat::entry);
        toolcat::version_ok(version)
            && dir.join(READY).is_file()
            && entry.is_some_and(|e| dir.join(e.entry).is_file())
    }

    /// The versions of a tool in its folder that are ready.
    fn tool_on_disk(&self, name: &str) -> Vec<String> {
        let mut versions: Vec<String> = std::fs::read_dir(self.tool_home(name))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join(READY).is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|v| toolcat::version_ok(v))
            .collect();
        versions.sort_by(|a, b| match catalogue::is_newer(a, b) {
            Some(true) => std::cmp::Ordering::Greater,
            _ if a == b => std::cmp::Ordering::Equal,
            _ => std::cmp::Ordering::Less,
        });
        versions
    }

    /// The version this station last made the one its panes get.
    fn tool_current(&self, name: &str) -> Option<String> {
        self.db
            .kv_get(&current_key(name))
            .filter(|v| toolcat::version_ok(v))
    }

    /// The version a pane started now is given: the one pinned for this
    /// station when it is ready, else the one it ran before while that is
    /// still on the disk (an upgrade that is still installing, or that
    /// failed, takes nothing away). None: the tool is not given at all.
    fn tool_wired(&self, name: &str, tool: &Tool) -> Option<String> {
        if !tool.is_catalogue() || !tool.in_scope(&self.station) {
            return None;
        }
        let want = tool.pinned(&self.station)?;
        if self.tool_ready(name, tool, want) {
            return Some(want.to_string());
        }
        self.tool_current(name)
            .filter(|v| self.tool_ready(name, tool, v))
    }

    /// A catalogue tool as an agent is given it: spandeck's Node and the
    /// tool's entry file by their whole paths, and the catalogue's
    /// arguments.
    fn tool_server(&self, name: &str, tool: &Tool, version: &str) -> Option<McpServer> {
        let entry = toolcat::entry(tool.catalogue.as_deref()?)?;
        let mut args = vec![self
            .tool_dir(name, version)
            .join(entry.entry)
            .display()
            .to_string()];
        args.extend(entry.args.iter().map(|a| a.to_string()));
        if let Some(arg) = entry.output_arg {
            args.push(arg.to_string());
            args.push(self.tool_data(name).join("output").display().to_string());
        }
        let mut env = BTreeMap::new();
        if let Some(var) = entry.data_env {
            env.insert(var.to_string(), self.tool_data(name).display().to_string());
        }
        Some(McpServer {
            command: Some(self.tool_runtime_program(tool).display().to_string()),
            args,
            env,
            url: None,
            agents: tool.agents.clone(),
        })
    }

    /// Where a catalogue tool stands on this station: `ready`,
    /// `installing`, `failed` or `needs`, and the line that says why.
    fn tool_state(&self, name: &str, tool: &Tool) -> (&'static str, Option<String>) {
        let job = self.tools.jobs.lock().unwrap().get(name).cloned();
        if job.as_ref().is_some_and(|j| j.running()) {
            return ("installing", None);
        }
        let Some(want) = tool.pinned(&self.station) else {
            return ("failed", Some("no version is named for it".into()));
        };
        if self.tool_ready(name, tool, want) {
            return ("ready", None);
        }
        let id = tool.catalogue.as_deref().unwrap_or_default();
        if toolcat::entry(id).is_none() {
            return (
                "needs",
                Some(format!(
                    "this station's spandeck does not know the tool `{id}`: it needs a newer spandeck"
                )),
            );
        }
        match job {
            Some(j) if j.to.as_deref() == Some(want) => match j.outcome.as_deref() {
                Some("needs") => ("needs", j.reason),
                Some("failed") | Some("interrupted") => ("failed", j.reason),
                _ => ("installing", None),
            },
            // Its job starts in a moment (`tools_reconcile`).
            _ => ("installing", None),
        }
    }

    /// What a pane started now is given, and what it is not and why.
    pub(crate) fn tools_wiring(&self, setup: &AgentSetup) -> Wiring {
        let mut w = Wiring {
            servers: setup.mcp_servers.clone(),
            ..Default::default()
        };
        for (name, tool) in &self.tools_doc().tools {
            // The document's entry is the one that counts.
            w.servers.remove(name);
            if !tool.in_scope(&self.station) {
                continue;
            }
            let server = if tool.is_catalogue() {
                self.tool_wired(name, tool)
                    .and_then(|v| self.tool_server(name, tool, &v))
            } else {
                Some(tool.as_server())
            };
            match server {
                Some(server) => {
                    w.servers.insert(name.clone(), server);
                    if tool.allow_without_asking {
                        w.allow.push((name.clone(), tool.agents.clone()));
                    }
                }
                None => {
                    let why = match self.tool_state(name, tool) {
                        ("failed", line) => format!(
                            "it could not be installed on {}: {}",
                            self.station,
                            line.unwrap_or_default()
                        ),
                        ("needs", line) => format!(
                            "it needs something {} lacks: {}",
                            self.station,
                            line.unwrap_or_default()
                        ),
                        _ => format!("it is still being installed on {}", self.station),
                    };
                    w.without.push((name.clone(), tool.agents.clone(), why));
                }
            }
        }
        w
    }

    /// The agent setup with this station's tools in place of its list of
    /// MCP servers: what every agent's wiring is written from.
    pub(crate) fn with_tools(&self, mut setup: AgentSetup) -> AgentSetup {
        setup.mcp_servers = self.tools_wiring(&setup).servers;
        setup
    }

    // ------------------------------------------------------------- jobs

    fn tool_job_save(&self, job: &ToolJob) {
        if let Ok(text) = serde_json::to_string(job) {
            self.db.kv_set(&job_key(&job.tool), &text);
        }
    }

    /// Start a job for a tool, unless one is running: then that one is the
    /// answer, and nothing is started.
    fn tool_job_begin(
        &self,
        name: &str,
        action: &str,
        by: &str,
    ) -> std::result::Result<ToolJob, Box<ToolJob>> {
        let mut jobs = self.tools.jobs.lock().unwrap();
        if let Some(running) = jobs.get(name).filter(|j| j.running()) {
            return Err(Box::new(running.clone()));
        }
        let job = ToolJob {
            id: new_id("job"),
            action: action.to_string(),
            tool: name.to_string(),
            state: "running".into(),
            step: "runtime".into(),
            started_ms: now_ms(),
            by: by.to_string(),
            limit_secs: self.job_limit().as_secs(),
            ..Default::default()
        };
        jobs.insert(name.to_string(), job.clone());
        drop(jobs);
        self.tool_job_save(&job);
        Ok(job)
    }

    fn tool_job_change(&self, name: &str, change: impl FnOnce(&mut ToolJob)) -> Option<ToolJob> {
        let mut jobs = self.tools.jobs.lock().unwrap();
        let job = jobs.get_mut(name)?;
        change(job);
        Some(job.clone())
    }

    fn tool_job_end(&self, name: &str, change: impl FnOnce(&mut ToolJob)) -> Option<ToolJob> {
        let job = self.tool_job_change(name, |j| {
            change(j);
            j.state = "done".into();
            j.step = String::new();
            j.command = None;
            j.last_line = None;
            j.ended_ms = Some(now_ms());
        })?;
        self.tool_job_save(&job);
        self.emit(
            "tools.status",
            None,
            None,
            json!({"tool": name, "station": self.station}),
        );
        Some(job)
    }

    /// At start: the jobs as they were stored (one that was running when
    /// spandeck stopped is over, and says so), and then what the document
    /// names for this station that is not here is installed. Nothing waits
    /// for it.
    pub(crate) fn tools_start(&self) {
        for name in self.tools_doc().tools.keys() {
            let Some(mut job) = self
                .db
                .kv_get(&job_key(name))
                .and_then(|t| serde_json::from_str::<ToolJob>(&t).ok())
            else {
                continue;
            };
            if job.running() {
                let why = format!(
                    "interrupted by a restart of spandeck on {}: it is started again",
                    self.station
                );
                job.state = "done".into();
                job.step = String::new();
                job.ended_ms = Some(now_ms());
                job.outcome = Some("interrupted".into());
                job.says = Some(format!("Not known whether it finished: {why}"));
                job.reason = Some(why);
                self.tool_job_save(&job);
            }
            self.tools.jobs.lock().unwrap().insert(name.clone(), job);
        }
        self.tools_reconcile(None);
    }

    /// The document changed, here or at another station.
    pub(crate) fn tools_changed(&self) {
        self.tools_reconcile(None);
        self.tools_rewire();
    }

    /// Antigravity reads its servers from spandeck's plugin, not from a
    /// pane's command line: it is written again when what is ready here
    /// changes (by the station of the account only; setup.rs).
    fn tools_rewire(&self) {
        let d = self.clone();
        std::thread::spawn(move || d.antigravity_refresh());
    }

    /// For every tool the document names for this station: start its
    /// install if the version pinned here is not ready, and make that
    /// version the one panes get if it is. A tool whose install failed is
    /// not tried again by itself (the same failure, and its download,
    /// again and again): the owner retries it (`retry`, from
    /// `tools.install`), or changes its version. One that was interrupted
    /// by a restart is tried again.
    pub(crate) fn tools_reconcile(&self, retry: Option<&str>) {
        let doc = self.tools_doc();
        for (name, tool) in &doc.tools {
            if !tool.is_catalogue() || !tool.in_scope(&self.station) {
                continue;
            }
            let Some(want) = tool.pinned(&self.station).map(String::from) else {
                continue;
            };
            let last = self.tools.jobs.lock().unwrap().get(name).cloned();
            if last.as_ref().is_some_and(|j| j.running()) {
                continue;
            }
            let current = self.tool_current(name);
            let by = match retry {
                Some(r) if r == name => "you".to_string(),
                _ => doc.updated_by.clone(),
            };
            if self.tool_ready(name, tool, &want) {
                if current.as_deref() != Some(want.as_str()) {
                    self.tool_switch(name, tool, current, &want, &by);
                }
                continue;
            }
            if tool.catalogue.as_deref().and_then(toolcat::entry).is_none() {
                continue;
            }
            let failed_before = last.is_some_and(|j| {
                j.to.as_deref() == Some(want.as_str())
                    && matches!(j.outcome.as_deref(), Some("failed") | Some("needs"))
            });
            if failed_before && retry != Some(name.as_str()) {
                continue;
            }
            self.tool_install_start(name, tool, current, &want, &by);
        }
    }

    /// The version pinned here is already on the disk and ready (it ran
    /// here before): it is the one panes get from now, at once.
    fn tool_switch(&self, name: &str, tool: &Tool, from: Option<String>, to: &str, by: &str) {
        let Some(from) = from.filter(|f| self.tool_ready(name, tool, f)) else {
            // Nothing ran here before it: there is no change to report.
            self.db.kv_set(&current_key(name), to);
            return;
        };
        let action = change_kind(Some(&from), to);
        if self.tool_job_begin(name, action, by).is_err() {
            return;
        }
        self.tool_job_change(name, |j| {
            j.step = "starting".into();
            j.from = Some(from.clone());
            j.to = Some(to.to_string());
            j.restart = tool.restart;
        });
        let (d, name, tool, to) = (self.clone(), name.to_string(), tool.clone(), to.to_string());
        let runtime = tokio::runtime::Handle::try_current().ok();
        std::thread::spawn(move || {
            let _in_runtime = runtime.as_ref().map(|h| h.enter());
            d.tool_finish(&name, &tool, Some(from), &to, Ok(()), String::new());
        });
    }

    fn tool_install_start(
        &self,
        name: &str,
        tool: &Tool,
        from: Option<String>,
        to: &str,
        by: &str,
    ) {
        let from = from.filter(|f| self.tool_ready(name, tool, f));
        let action = change_kind(from.as_deref(), to);
        if self.tool_job_begin(name, action, by).is_err() {
            return;
        }
        self.tool_job_change(name, |j| {
            j.from = from.clone();
            j.to = Some(to.to_string());
            j.restart = tool.restart;
        });
        self.emit(
            "tools.status",
            None,
            None,
            json!({"tool": name, "station": self.station}),
        );
        let (d, name, tool, to) = (self.clone(), name.to_string(), tool.clone(), to.to_string());
        let runtime = tokio::runtime::Handle::try_current().ok();
        std::thread::spawn(move || {
            let _in_runtime = runtime.as_ref().map(|h| h.enter());
            let mut output = String::new();
            let result = d.tool_install_steps(&name, &tool, &to, runtime.as_ref(), &mut output);
            d.tool_finish(&name, &tool, from, &to, result, output);
        });
    }

    /// One command of an install: run in the tool's folder with what is
    /// left of the job's time, its last line shown on the job as it goes.
    #[allow(clippy::too_many_arguments)]
    fn tool_run(
        &self,
        name: &str,
        step: &str,
        argv: &[String],
        cwd: &Path,
        env: &[(String, String)],
        deadline: Instant,
        output: &mut String,
    ) -> std::result::Result<(), String> {
        let shown = shell_join(argv);
        self.tool_job_change(name, |j| {
            j.step = step.to_string();
            j.command = Some(shown.clone());
            j.last_line = None;
        });
        let left = deadline.saturating_duration_since(Instant::now());
        let ran = self.run_watched_in(argv, left, true, Some(cwd), env, &|text| {
            let line = last_line(text);
            self.tool_job_change(name, |j| j.last_line = line);
        });
        output.push_str(&format!("$ {shown}\n{}\n", ran.text.trim_end()));
        match ran.ended {
            Ended::Exited { ok: true } => Ok(()),
            Ended::NoAnswer => Err(format!(
                "it was still running after {} and was stopped",
                minutes(self.job_limit())
            )),
            _ => Err(catalogue::error_line(&ran.text)
                .unwrap_or_else(|| "it exited with an error and printed nothing".into())),
        }
    }

    /// An install, start to finish, on its own thread: spandeck's Node,
    /// the package, what the tool fetches for itself, and the tool started
    /// once. The folder is marked ready only after that last step.
    fn tool_install_steps(
        &self,
        name: &str,
        tool: &Tool,
        version: &str,
        runtime: Option<&tokio::runtime::Handle>,
        output: &mut String,
    ) -> std::result::Result<(), Fail> {
        let id = tool.catalogue.clone().unwrap_or_default();
        let entry =
            toolcat::entry(&id).ok_or_else(|| failed(format!("no catalogue entry called {id}")))?;
        let cfg = self.tool_cfg(tool).cloned().unwrap_or_default();
        let deadline = Instant::now() + self.job_limit();
        let dir = self.tool_dir(name, version);
        let data = self.tool_data(name);
        // A folder left by an install that did not finish is not built on.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::create_dir_all(&data))
            .map_err(|e| failed(format!("could not make {}: {e}", dir.display())))?;
        let mut env = vec![];
        if let Some(var) = entry.data_env {
            env.push((var.to_string(), data.display().to_string()));
        }
        let check: Option<Vec<String>> = if let Some(install) = cfg.install_cmd.clone() {
            // This station's own config says how (tests, unusual setups).
            let mut env = env.clone();
            for (k, v) in [
                ("SPANDECK_TOOL_NAME", name.to_string()),
                ("SPANDECK_TOOL_VERSION", version.to_string()),
                ("SPANDECK_TOOL_DIR", dir.display().to_string()),
                ("SPANDECK_TOOL_DATA", data.display().to_string()),
            ] {
                env.push((k.to_string(), v));
            }
            self.tool_run(name, "package", &install, &dir, &env, deadline, output)
                .map_err(failed)?;
            cfg.check_cmd.clone()
        } else {
            let (node, npm) = match &cfg.runtime {
                Some(program) => (PathBuf::from(program), None),
                None => {
                    let (node, npm) = self.tool_runtime(name, runtime, deadline, output)?;
                    (node, Some(npm))
                }
            };
            let node_s = node.display().to_string();
            // What npm or the tool starts in turn finds this Node first.
            if let Some(bin) = node.parent() {
                let path = crate::paths::path_with(
                    &std::env::var("PATH").unwrap_or_default(),
                    &self.agent_dirs(),
                );
                let sep = if sys::WINDOWS { ';' } else { ':' };
                env.push(("PATH".into(), format!("{}{sep}{path}", bin.display())));
            }
            let mut install = vec![node_s.clone()];
            match npm {
                Some(npm) => install.push(npm.display().to_string()),
                // A runtime of the station's own choosing brings its npm.
                None => install = vec!["npm".to_string()],
            }
            install.extend(
                [
                    "install",
                    "--prefix",
                    &dir.display().to_string(),
                    "--cache",
                    &self
                        .tools_root()
                        .join("runtime")
                        .join("npm-cache")
                        .display()
                        .to_string(),
                    // The package and what it depends on have no install
                    // scripts (toolcat.rs): none is run.
                    "--ignore-scripts",
                    "--no-audit",
                    "--no-fund",
                    "--no-update-notifier",
                    &format!("{}@{version}", entry.package),
                ]
                .map(String::from),
            );
            self.tool_run(name, "package", &install, &dir, &env, deadline, output)
                .map_err(failed)?;
            for step in entry.after_install {
                let mut argv = vec![node_s.clone()];
                argv.extend(step.node_args.iter().map(|a| a.to_string()));
                self.tool_run(name, "setup", &argv, &dir, &env, deadline, output)
                    .map_err(failed)?;
            }
            cfg.check_cmd.clone().or_else(|| {
                entry.start_check.as_ref().map(|step| {
                    let mut argv = vec![node_s.clone()];
                    argv.extend(step.node_args.iter().map(|a| a.to_string()));
                    argv
                })
            })
        };
        if !dir.join(entry.entry).is_file() {
            return Err(failed(format!(
                "the install finished, and there is no {} in {}",
                entry.entry,
                dir.display()
            )));
        }
        // Started once, as the last step. If it does not start, the tool is
        // not ready: what it said is what this station lacks.
        if let Some(argv) = check {
            self.tool_run(name, "starting", &argv, &dir, &env, deadline, output)
                .map_err(|line| Fail { needs: true, line })?;
        }
        let marker =
            json!({"tool": name, "catalogue": id, "version": version, "ready_ms": now_ms()});
        std::fs::write(dir.join(READY), marker.to_string())
            .map_err(|e| failed(format!("could not write in {}: {e}", dir.display())))?;
        Ok(())
    }

    /// spandeck's own Node, downloaded once per version into the state
    /// folder: the archive nodejs.org publishes for this kind of machine,
    /// taken over HTTPS, refused unless its SHA-256 is the one fixed in the
    /// code, and unpacked with `tar` (which Linux, macOS and Windows 10 and
    /// later all have, and which the release archives are unpacked with).
    fn tool_runtime(
        &self,
        name: &str,
        runtime: Option<&tokio::runtime::Handle>,
        deadline: Instant,
        output: &mut String,
    ) -> std::result::Result<(PathBuf, PathBuf), Fail> {
        let _one = self.tools.runtime_lock.lock().unwrap();
        let dir = self.tool_runtime_dir();
        let (node, npm) = toolcat::node_files(&dir);
        if !(node.is_file() && npm.is_file()) {
            let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
            let archive = toolcat::node_archive(os, arch).ok_or_else(|| Fail {
                needs: true,
                line: format!(
                    "nodejs.org publishes no Node {} for this kind of machine ({os}, {arch})",
                    toolcat::NODE_VERSION
                ),
            })?;
            let shown = format!("download {}", archive.url);
            self.tool_job_change(name, |j| {
                j.step = "runtime".into();
                j.command = Some(shown.clone());
            });
            output.push_str(&format!("$ {shown}\n"));
            let runtime =
                runtime.ok_or_else(|| failed("this station cannot make the download now"))?;
            let left = deadline.saturating_duration_since(Instant::now());
            let bytes = runtime
                .block_on(fetch(&archive.url, left, &|got, of| {
                    let line = match of {
                        Some(of) => format!(
                            "Downloading Node {}: {} of {} MB",
                            toolcat::NODE_VERSION,
                            got >> 20,
                            of >> 20
                        ),
                        None => format!(
                            "Downloading Node {}: {} MB",
                            toolcat::NODE_VERSION,
                            got >> 20
                        ),
                    };
                    self.tool_job_change(name, |j| j.last_line = Some(line));
                }))
                .map_err(failed)?;
            toolcat::verify_sha256(&bytes, archive.sha256).map_err(|why| {
                failed(format!(
                    "the Node archive from nodejs.org was not used: {why}"
                ))
            })?;
            output.push_str(&format!(
                "{} bytes, SHA-256 {} as fixed in spandeck\n",
                bytes.len(),
                archive.sha256
            ));
            let parent = dir.parent().unwrap_or(&dir).to_path_buf();
            let unpack = parent.join(new_id("unpack"));
            let file = unpack.join(if archive.zip {
                "node.zip"
            } else {
                "node.tar.gz"
            });
            let made = std::fs::create_dir_all(&unpack).and_then(|_| std::fs::write(&file, &bytes));
            drop(bytes);
            let unpacked = made
                .map_err(|e| format!("could not write {}: {e}", file.display()))
                .and_then(|_| {
                    // The tar Windows ships (bsdtar) reads .zip as well.
                    let argv = [
                        "tar",
                        if archive.zip { "-xf" } else { "-xzf" },
                        &file.display().to_string(),
                        "-C",
                        &unpack.display().to_string(),
                    ]
                    .map(String::from);
                    self.tool_run(name, "runtime", &argv, &unpack, &[], deadline, output)
                })
                .and_then(|_| {
                    let _ = std::fs::remove_dir_all(&dir);
                    std::fs::rename(unpack.join(&archive.folder), &dir)
                        .map_err(|e| format!("the Node archive did not unpack as expected: {e}"))
                });
            let _ = std::fs::remove_dir_all(&unpack);
            unpacked.map_err(failed)?;
        }
        // It is there: does it start on this machine?
        let ran = self.run_captured(
            &[node.display().to_string(), "--version".to_string()],
            VERSION_LIMIT,
        );
        if !ran.ok() {
            return Err(Fail {
                needs: true,
                line: format!(
                    "spandeck's Node ({}) does not start on this station: {}",
                    toolcat::NODE_VERSION,
                    catalogue::error_line(&ran.text).unwrap_or_else(|| "it said nothing".into())
                ),
            });
        }
        Ok((node, npm))
    }

    /// A job is over. A version that is ready becomes the one panes get;
    /// the versions before the previous one are removed, so going back one
    /// is at once; after a change of version the panes of the agents it is
    /// for are started again as after an agent's update (an idle one now,
    /// another when its turn is over), unless the change said not to.
    fn tool_finish(
        &self,
        name: &str,
        tool: &Tool,
        from: Option<String>,
        to: &str,
        result: std::result::Result<(), Fail>,
        output: String,
    ) {
        let action = change_kind(from.as_deref(), to);
        let ok = result.is_ok();
        let changed = ok && from.as_deref().is_some_and(|f| f != to);
        let (mut restarted, mut waiting) = (vec![], vec![]);
        if ok {
            self.db.kv_set(&current_key(name), to);
            for v in self.tool_on_disk(name) {
                if v != to && Some(v.as_str()) != from.as_deref() {
                    let _ = std::fs::remove_dir_all(self.tool_dir(name, &v));
                }
            }
            if changed && tool.restart {
                for kind in KINDS.into_iter().filter(|k| applies(&tool.agents, *k)) {
                    let (now, later) = self.restart_panes_of(kind, "tool update");
                    restarted.extend(now);
                    waiting.extend(later);
                }
            }
        }
        let arrow = format!("{} → {to}", from.as_deref().unwrap_or("?"));
        let (outcome, reason, says) = match (&result, action) {
            (Ok(()), "upgrade") => ("upgraded", None, format!("Upgraded {arrow}")),
            (Ok(()), "rollback") => ("rolled_back", None, format!("Rolled back {arrow}")),
            (Ok(()), _) => ("installed", None, format!("Installed {to}")),
            (Err(f), _) if f.needs => (
                "needs",
                Some(f.line.clone()),
                format!("Needs something this station lacks: {}", f.line),
            ),
            (Err(f), "upgrade") => (
                "failed",
                Some(f.line.clone()),
                format!("Could not upgrade: {}", f.line),
            ),
            (Err(f), "rollback") => (
                "failed",
                Some(f.line.clone()),
                format!("Could not roll back: {}", f.line),
            ),
            (Err(f), _) => (
                "failed",
                Some(f.line.clone()),
                format!("Could not install: {}", f.line),
            ),
        };
        let job = self.tool_job_end(name, |j| {
            j.outcome = Some(outcome.into());
            j.reason = reason.clone();
            j.says = Some(says.clone());
            j.output = Some(tail(&output, OUTPUT_KEPT));
            j.restarted = restarted.clone();
            j.restart_when_idle = waiting.clone();
        });
        self.record(
            "tools.installed",
            None,
            None,
            None,
            job.as_ref().map(|j| j.by.as_str()),
            json!({
                "tool": name, "catalogue": tool.catalogue, "station": self.station,
                "ok": ok, "action": action, "outcome": outcome, "from": from, "to": to,
                "reason": reason, "says": says, "job": job.as_ref().map(|j| j.id.clone()),
                "message": tail(&output, 600), "at_ms": now_ms(),
                "restarted": restarted, "restart_when_idle": waiting,
            }),
        );
        if ok {
            self.antigravity_refresh();
        } else {
            self.notify(
                "error",
                None,
                None,
                &format!("The tool {name} is not ready on {}", self.station),
                &says,
            );
        }
    }

    // ----------------------------------------------------------- calls

    /// `tools.install`: try again here. While a job for the tool runs,
    /// nothing more is started: the answer names the running job.
    pub fn tools_install(&self, name: &str) -> Result<Value> {
        let doc = self.tools_doc();
        let tool = doc
            .tools
            .get(name)
            .ok_or_else(|| invalid(format!("there is no tool called {name}")))?;
        if !tool.is_catalogue() {
            return Err(invalid(format!(
                "{name} is a custom tool: spandeck does not install it"
            )));
        }
        if !tool.in_scope(&self.station) {
            return Err(invalid(format!(
                "{name} is not chosen for {}: widen its scope first",
                self.station
            )));
        }
        let running = |d: &Daemon| {
            d.tools
                .jobs
                .lock()
                .unwrap()
                .get(name)
                .filter(|j| j.running())
                .cloned()
        };
        let base = json!({"station": self.station, "tool": name});
        let with = |extra: Value| {
            let mut out = base.clone();
            for (k, v) in extra.as_object().into_iter().flatten() {
                out[k] = v.clone();
            }
            out
        };
        if let Some(job) = running(self) {
            return Ok(with(json!({"started": false, "running": job.brief()})));
        }
        let want = tool.pinned(&self.station).unwrap_or_default();
        if self.tool_ready(name, tool, want) {
            return Ok(with(json!({"started": false, "ready": want})));
        }
        if let ("needs", Some(line)) = self.tool_state(name, tool) {
            if tool.catalogue.as_deref().and_then(toolcat::entry).is_none() {
                return Err(invalid(line));
            }
        }
        self.tools_reconcile(Some(name));
        match running(self) {
            Some(job) => Ok(with(json!({"started": true, "job": job.id, "to": job.to}))),
            None => Ok(with(json!({"started": false}))),
        }
    }

    /// `tools.status`: every tool of the document, and every MCP server of
    /// the agent setup, as it stands on this station. It reads this
    /// station's disk and waits for nothing else.
    pub fn tools_status(&self) -> Value {
        let doc = self.tools_doc();
        let setup = self.setup();
        let jobs = self.tools.jobs.lock().unwrap().clone();
        let panes = |agents: &[String]| -> usize {
            KINDS
                .into_iter()
                .filter(|k| applies(agents, *k))
                .map(|k| self.live_panes_of(k).len())
                .sum()
        };
        let custom = |name: &str, tool: &Tool, from_setup: bool| -> Value {
            let (state, line, path) = match &tool.command {
                // An address is not asked whether it answers.
                None => ("ready", None, None),
                Some(command) => match self.find_program(command) {
                    Some(path) => ("ready", None, Some(path)),
                    None => (
                        "needs",
                        Some(format!(
                            "there is no `{command}` on {} (in the folders spandeck looks in)",
                            self.station
                        )),
                        None,
                    ),
                },
            };
            json!({
                "name": name, "title": name, "kind": "custom", "from_setup": from_setup,
                "chosen": true, "state": state, "line": line, "path": path,
                // A custom tool is given to agents wherever it is chosen,
                // found or not, as an MCP server of the agent setup is.
                "wired": true, "checked": tool.command.is_some(),
                "allow_without_asking": tool.allow_without_asking,
                "panes": panes(&tool.agents),
            })
        };
        let mut rows = vec![];
        for (name, tool) in &doc.tools {
            if !tool.in_scope(&self.station) {
                rows.push(json!({
                    "name": name, "kind": tool.kind, "catalogue": tool.catalogue,
                    "title": crate::tooldoc::tool_json(name, tool, false)["title"],
                    "from_setup": false, "chosen": false, "state": "not_chosen", "wired": false,
                    "on_disk": self.tool_on_disk(name),
                }));
                continue;
            }
            if !tool.is_catalogue() {
                rows.push(custom(name, tool, false));
                continue;
            }
            let (state, line) = self.tool_state(name, tool);
            let wanted = tool.pinned(&self.station);
            let wired = self.tool_wired(name, tool);
            rows.push(json!({
                "name": name, "kind": "catalogue", "catalogue": tool.catalogue,
                "title": crate::tooldoc::tool_json(name, tool, false)["title"],
                "from_setup": false, "chosen": true,
                "state": state, "line": line,
                // The version pinned for this station, and the one a pane
                // started now is given (null: none).
                "wanted": wanted,
                "version": (state == "ready").then_some(wanted).flatten(),
                "wired": wired.is_some(), "wired_version": wired,
                "own_pin": tool.station_versions.contains_key(&self.station),
                "job": jobs.get(name),
                "latest": self.tool_latest_view(tool, wanted),
                "on_disk": self.tool_on_disk(name),
                "allow_without_asking": tool.allow_without_asking,
                "panes": panes(&tool.agents),
            }));
        }
        for (name, server) in &setup.mcp_servers {
            if !doc.tools.contains_key(name) {
                rows.push(custom(name, &Tool::from_server(server), true));
            }
        }
        json!({
            "station": self.station,
            "updated_ms": doc.updated_ms,
            "checks_off": !self.latest_on(),
            "runtime": {
                "node": toolcat::NODE_VERSION,
                "installed": toolcat::node_files(&self.tool_runtime_dir()).0.is_file(),
            },
            "tools": rows,
        })
    }

    /// `tools.catalogue`: what spandeck can install, and with what.
    pub fn tools_catalogue(&self) -> Value {
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        let tools: Vec<Value> = toolcat::all()
            .iter()
            .map(|e| {
                let seen = self.tool_latest_seen(e.id);
                json!({
                    "id": e.id, "name": e.name, "gives": e.gives, "package": e.package,
                    "needs": e.needs, "tested": e.tested, "args": e.args, "source": e.source,
                    "latest": seen.and_then(|s| s.version),
                })
            })
            .collect();
        json!({
            "station": self.station,
            "tools": tools,
            "runtime": {
                "node": toolcat::NODE_VERSION,
                "platform": toolcat::node_platform(os, arch),
                "installed": toolcat::node_files(&self.tool_runtime_dir()).0.is_file(),
            },
        })
    }

    // --------------------------------------------------- is it the latest

    fn tool_latest_seen(&self, id: &str) -> Option<Seen> {
        if let Some(seen) = self.tools.latest.lock().unwrap().get(id) {
            return Some(seen.clone());
        }
        let seen: Seen = serde_json::from_str(&self.db.kv_get(&latest_key(id))?).ok()?;
        self.tools
            .latest
            .lock()
            .unwrap()
            .insert(id.to_string(), seen.clone());
        Some(seen)
    }

    fn tool_latest_keep(&self, id: &str, seen: &Seen) {
        if let Ok(text) = serde_json::to_string(seen) {
            self.db.kv_set(&latest_key(id), &text);
        }
        self.tools
            .latest
            .lock()
            .unwrap()
            .insert(id.to_string(), seen.clone());
    }

    fn tool_latest_fresh(&self, id: &str) -> Option<Seen> {
        self.tool_latest_seen(id).filter(|s| {
            let keep = if s.version.is_some() {
                crate::latest::KEEP_MS
            } else {
                crate::latest::RETRY_MS
            };
            now_ms().saturating_sub(s.checked_ms) < keep
        })
    }

    /// Ask the registry, now: a plain GET of a public address, which says
    /// nothing of the owner, the station or a project.
    async fn tool_latest_ask(&self, entry: &Entry) -> Seen {
        let url = self
            .cfg
            .tools
            .get(entry.id)
            .and_then(|c| c.latest_url.clone())
            .unwrap_or_else(|| entry.latest.0.to_string());
        let mut seen = Seen {
            checked_ms: now_ms(),
            method: "registry".into(),
            asked: crate::latest::host_of(&url),
            ..Default::default()
        };
        let version = crate::latest::get(&url).await.and_then(|text| {
            serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.pointer(entry.latest.1)
                        .and_then(|v| v.as_str().map(String::from))
                })
                .as_deref()
                .and_then(catalogue::published_version)
                .filter(|v| toolcat::version_ok(v))
                .ok_or_else(|| format!("{} did not answer with a version", seen.asked))
        });
        match version {
            Ok(v) => seen.version = Some(v),
            Err(why) => seen.why = Some(why.chars().take(300).collect()),
        }
        seen
    }

    /// The newest version, for a change the owner is making now: what was
    /// answered in the last three hours, else asked now. None when checks
    /// are off on this station or the registry did not answer.
    pub(crate) async fn tool_latest_now(&self, entry: &Entry) -> Option<String> {
        if let Some(v) = self.tool_latest_fresh(entry.id).and_then(|s| s.version) {
            return Some(v);
        }
        if !self.latest_on() {
            return None;
        }
        let seen = self.tool_latest_ask(entry).await;
        self.tool_latest_keep(entry.id, &seen);
        seen.version
    }

    /// `tools.check`: ask the registry about one tool, or every catalogue
    /// tool of the document, where what is known is older than three hours
    /// (`again`: whatever is known). Answers at once with which were
    /// asked; `tools.status` carries the answers. Asked only when the
    /// owner asks, and never with `[agents] check_latest = false`.
    pub fn tools_check(&self, name: Option<&str>, again: bool) -> Value {
        if !self.latest_on() {
            return json!({"station": self.station, "off": true, "asking": []});
        }
        let runtime = tokio::runtime::Handle::try_current().ok();
        let mut asking = vec![];
        for (tool_name, tool) in &self.tools_doc().tools {
            if name.is_some_and(|n| n != tool_name) {
                continue;
            }
            let Some(entry) = tool.catalogue.as_deref().and_then(toolcat::entry) else {
                continue;
            };
            if (!again && self.tool_latest_fresh(entry.id).is_some())
                || !self
                    .tools
                    .checking
                    .lock()
                    .unwrap()
                    .insert(entry.id.to_string())
            {
                continue;
            }
            asking.push(json!(tool_name));
            let (d, runtime) = (self.clone(), runtime.clone());
            std::thread::spawn(move || {
                let seen = match &runtime {
                    Some(runtime) => runtime.block_on(d.tool_latest_ask(entry)),
                    None => Seen {
                        checked_ms: now_ms(),
                        why: Some("this station cannot make the call now".into()),
                        ..Default::default()
                    },
                };
                d.tool_latest_keep(entry.id, &seen);
                d.tools.checking.lock().unwrap().remove(entry.id);
                d.emit(
                    "tools.status",
                    None,
                    None,
                    json!({"catalogue": entry.id, "station": d.station}),
                );
            });
        }
        json!({"station": self.station, "off": false, "asking": asking})
    }

    /// The `latest` of a row in `tools.status`, against the version pinned
    /// for this station: `latest`, `available` (with the version),
    /// `unknown` (with why), `not_checked` or `off`.
    fn tool_latest_view(&self, tool: &Tool, wanted: Option<&str>) -> Value {
        let Some(id) = tool.catalogue.as_deref() else {
            return Value::Null;
        };
        if !self.latest_on() {
            return json!({"state": "off"});
        }
        let checking = self.tools.checking.lock().unwrap().contains(id);
        let Some(seen) = self.tool_latest_seen(id) else {
            return json!({"state": "not_checked", "checking": checking});
        };
        let (state, why) = match (&seen.version, wanted) {
            (Some(published), Some(wanted)) => match catalogue::is_newer(published, wanted) {
                Some(true) => ("available", None),
                Some(false) => ("latest", None),
                None if published == wanted => ("latest", None),
                None => (
                    "unknown",
                    Some(format!(
                        "{wanted} and the published {published} cannot be compared"
                    )),
                ),
            },
            (Some(_), None) => ("unknown", Some("no version is pinned here".to_string())),
            (None, _) => ("unknown", seen.why.clone()),
        };
        json!({
            "state": state, "version": seen.version, "why": why,
            "checked_ms": seen.checked_ms, "asked": seen.asked, "checking": checking,
        })
    }
}

/// One download over HTTPS through spandeck's own client (`http.rs`), with
/// what has arrived so far said as it goes.
async fn fetch(
    url: &str,
    limit: Duration,
    progress: &dyn Fn(u64, Option<u64>),
) -> std::result::Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        return Err(format!("{url} is not an https address: not downloaded"));
    }
    let host = crate::latest::host_of(url);
    let http = crate::http::builder()
        .connect_timeout(Duration::from_secs(10))
        .user_agent("spandeck")
        .build()
        .map_err(|e| e.to_string())?;
    let whole = async {
        let mut res = http.get(url).send().await.map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                format!("no route to {host}")
            } else {
                format!("{host}: {}", e.without_url())
            }
        })?;
        if !res.status().is_success() {
            return Err(format!("{host} answered {}", res.status().as_u16()));
        }
        let of = res.content_length();
        let mut bytes = Vec::with_capacity(of.unwrap_or(0).min(1 << 28) as usize);
        let mut said = Instant::now();
        while let Some(chunk) = res.chunk().await.map_err(|e| {
            format!(
                "the download from {host} did not finish: {}",
                e.without_url()
            )
        })? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > (1 << 30) {
                return Err(format!("{host} sent more than an archive of Node is"));
            }
            if said.elapsed() >= Duration::from_millis(500) {
                said = Instant::now();
                progress(bytes.len() as u64, of);
            }
        }
        Ok(bytes)
    };
    match tokio::time::timeout(limit, whole).await {
        Ok(r) => r,
        Err(_) => Err(format!(
            "the download from {host} was still running after {} and was stopped",
            minutes(limit)
        )),
    }
}
