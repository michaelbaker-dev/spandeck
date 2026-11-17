//! The agent programs on this station: where each one is, whether it
//! answers, which version it is, installing one that is missing or does not
//! start, and updating them.
//!
//! What to run comes from the catalogue in `spandeck-agents`, or from this
//! station's own `[agents.<name>]` config. Nothing here takes a command from
//! a caller: a caller names an agent, never a program.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spandeck_agents::{adapter, catalogue, AgentKind, AgentState, LaunchOpts};

use spandeck_terminal::sys;

use crate::daemon::{invalid, shell_join, Daemon, Result};
use crate::paths::{new_id, now_ms};

/// How long a version that was read stays good.
const FRESH: Duration = Duration::from_secs(60);
/// A missing program is looked for again sooner: it may be installing.
const FRESH_MISSING: Duration = Duration::from_secs(5);
/// A pane start trusts a program that answered this recently: asking again
/// on every start would slow each one, and the tick asks every few minutes.
const FRESH_FOR_START: Duration = Duration::from_secs(600);
/// What a station says it has (`station.info` caps): jobs and `latest`
/// in `agents.status`, and `agents.check`.
pub const CAP: &str = "agent_jobs";
pub(crate) const VERSION_LIMIT: Duration = Duration::from_secs(5);
/// An updater or installer is given this long (`[flags]
/// agents.job_limit_secs` changes it): generous, since an installer
/// downloads a few hundred megabytes. After it the job ends as failed, with
/// what was printed, and the row's buttons come back.
const JOB_LIMIT_SECS: u64 = 900;
/// `agents.status` waits this long for a version it has never read. Less
/// than the five seconds a call from another station is given: a program
/// that is slow to say its version must not cost a station its whole row
/// of answers.
const STATUS_WAIT: Duration = Duration::from_millis(3000);
/// What an updater printed is kept to this many characters (its end).
const OUTPUT_KEPT: usize = 6000;
/// Copies of one program that are asked before giving up.
const MAX_COPIES: usize = 4;

/// What was found when spandeck looked for an agent's program. The owner's
/// Mac had a `codex` whose real binary was gone (it was called installed),
/// and earlier one that never answered (it was called not found).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// There, and it answered when asked its version.
    Ok,
    /// Not in any folder spandeck looks in.
    Missing,
    /// There, but it exits with an error or cannot be run at all.
    Broken,
    /// There, but it did not answer in time. Unknown, not broken.
    NoAnswer,
}

#[derive(Clone)]
pub(crate) struct Probe {
    at: Instant,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub health: Health,
    /// The program's own error line when it is broken.
    pub problem: Option<String>,
}

/// How a command spandeck ran came to an end.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    Exited {
        ok: bool,
    },
    /// Still running at the time limit.
    NoAnswer,
    /// It could not be started. `missing`: there is no such program.
    NotRun {
        missing: bool,
    },
}

pub(crate) struct Ran {
    pub ended: Ended,
    pub text: String,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.ended == Ended::Exited { ok: true }
    }
}

/// One way to install an agent on this station.
pub(crate) struct InstallMethod {
    pub id: String,
    pub label: String,
    /// One shell line.
    pub command: String,
    pub update: Option<String>,
    /// Whether its tool (`npm`, `brew`) is on this station.
    pub available: bool,
}

/// What a pane is started with.
pub(crate) enum Found {
    Run(PathBuf),
    Missing,
    Broken { path: PathBuf, problem: String },
}

/// An update or an install of one program on this station, running or
/// ended. There is one per program: the newest. The owner, 17 Nov 2026: "I
/// picked update all on m4-MacBook, no feedback, don't know if it's doing
/// it, if it worked"; and "I can't press update twice in a row, it should
/// grey out until the update process is complete". The station holds this,
/// so every deck and the CLI show the same, whenever they look.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: String,
    /// `update` or `install`.
    pub action: String,
    pub agent: String,
    /// `running` or `done`.
    pub state: String,
    /// While it runs: `looking` (reading the version first) or `running`.
    pub step: String,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// Who asked: "you", "you (via alpha)".
    pub by: String,
    /// What is run, as the owner would type it.
    pub command: Option<String>,
    /// The last line the command has printed so far.
    pub last_line: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    /// `updated`, `latest` (it ran and the version is what it was),
    /// `installed`, `failed`, `interrupted`.
    pub outcome: Option<String>,
    /// For a failure: why, in one line.
    pub reason: Option<String>,
    /// What the command printed (its end, bounded).
    pub output: Option<String>,
    /// Panes restarted on the new version, and those that will be when
    /// their turn is over.
    pub restarted: Vec<String>,
    pub restart_when_idle: Vec<String>,
    /// Whether panes are restarted after a change of version.
    pub restart: bool,
    /// The "update all" this was part of.
    pub batch: Option<String>,
    pub method: Option<String>,
    pub limit_secs: u64,
}

impl Job {
    fn running(&self) -> bool {
        self.state == "running"
    }

    /// For a caller told "this is already running".
    fn brief(&self) -> Value {
        json!({"id": self.id, "action": self.action, "agent": self.agent, "started_ms": self.started_ms, "by": self.by, "state": self.state})
    }
}

#[derive(Default)]
pub struct AgentsState {
    probes: Mutex<HashMap<AgentKind, Probe>>,
    /// Programs whose version is being read off to one side.
    probing: Mutex<HashSet<AgentKind>>,
    /// The newest job of each program. One runs at a time per program.
    jobs: Mutex<HashMap<AgentKind, Job>>,
    /// Panes to restart on the new version once their turn is over.
    restart_when_idle: Mutex<HashSet<String>>,
    /// What each program's release source last answered (latest.rs).
    pub(crate) latest: Mutex<HashMap<AgentKind, crate::latest::Seen>>,
    pub(crate) checking: Mutex<HashSet<AgentKind>>,
}

fn job_key(kind: AgentKind) -> String {
    format!("agents.job.{}", kind.as_str())
}

/// The last line of what a command has printed, for a row on a screen.
fn last_line(text: &str) -> Option<String> {
    // A progress bar redraws one line with carriage returns: its last state.
    let line = text.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(200).collect())
}

/// In the middle of something the owner would lose by a restart.
fn busy(state: AgentState) -> bool {
    matches!(state, AgentState::Working | AgentState::NeedsYou)
}

/// Text a program printed, without the escape sequences that colour it.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e.is_ascii_alphabetic() || e == '\x07' {
                    break;
                }
            }
        } else if c == '\r' {
            // A line redrawn in place (a progress bar): each state a line.
            if chars.peek() != Some(&'\n') {
                out.push('\n');
            }
        } else if c == '\n' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

fn tail(text: &str, max: usize) -> String {
    let text = text.trim();
    let n = text.chars().count();
    if n <= max {
        text.to_string()
    } else {
        format!("…{}", text.chars().skip(n - max).collect::<String>())
    }
}

impl Daemon {
    /// Folders agent programs are installed in, besides PATH.
    pub(crate) fn agent_dirs(&self) -> Vec<PathBuf> {
        catalogue::well_known_dirs()
            .iter()
            .filter_map(|d| expand_dir(d, &self.home))
            .collect()
    }

    /// PATH with those folders added, for a command spandeck runs itself.
    fn agent_path_env(&self) -> String {
        crate::paths::path_with(
            &std::env::var("PATH").unwrap_or_default(),
            &self.agent_dirs(),
        )
    }

    /// Every copy of a program, in the order they are looked for: a path as
    /// given, else on PATH, else in the folders the vendors' installers use.
    /// Two names for one file count once.
    pub(crate) fn find_programs(&self, name: &str) -> Vec<PathBuf> {
        // A name stands for one file on Unix; on Windows for `name.exe`,
        // `name.cmd` and so on, in PATHEXT's order.
        let names = sys::executable_names(name);
        if is_path(name) {
            return std::iter::once(name.to_string())
                .chain(names)
                .map(PathBuf::from)
                .find(runnable)
                .into_iter()
                .collect();
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut seen = HashSet::new();
        std::env::split_paths(&path)
            .filter(|d| !d.as_os_str().is_empty())
            .chain(self.agent_dirs())
            .flat_map(|d| names.iter().map(|n| d.join(n)).collect::<Vec<_>>())
            .filter(runnable)
            .filter(|p| seen.insert(std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())))
            .collect()
    }

    /// The first copy of a program that exists. Whether it works is not asked.
    pub(crate) fn find_program(&self, name: &str) -> Option<PathBuf> {
        self.find_programs(name).into_iter().next()
    }

    /// The program an agent is started with on this station.
    pub(crate) fn agent_program(&self, kind: AgentKind) -> Option<String> {
        let configured = self
            .cfg
            .agents
            .get(kind.as_str())
            .and_then(|a| a.program.clone());
        configured.or_else(|| Some(adapter(kind)?.launch(&LaunchOpts::default()).program))
    }

    /// The ways an agent can be installed on this station. `install_cmd` in
    /// the station's config wins over everything and is then the only way.
    pub(crate) fn agent_methods(&self, kind: AgentKind) -> Vec<InstallMethod> {
        let configured = self
            .cfg
            .agents
            .get(kind.as_str())
            .and_then(|a| a.install_cmd.as_ref())
            .filter(|c| !c.is_empty())
            .map(|c| shell_join(c));
        if let Some(command) = configured {
            return vec![InstallMethod {
                id: "config".into(),
                label: "This station's config.toml".into(),
                command,
                update: None,
                available: true,
            }];
        }
        let methods = catalogue::program(kind).map(|p| p.methods).unwrap_or(&[]);
        methods
            .iter()
            .map(|m| InstallMethod {
                id: m.id.into(),
                label: m.label.into(),
                command: m.install.into(),
                update: m.update.map(String::from),
                available: m.tool.is_none_or(|t| self.find_program(t).is_some()),
            })
            .collect()
    }

    /// The way the owner chose for this agent on this station, while its
    /// tool is still here.
    fn agent_method_chosen(&self, kind: AgentKind) -> Option<InstallMethod> {
        let id = self.db.kv_get(&method_key(kind))?;
        self.agent_methods(kind)
            .into_iter()
            .find(|m| m.id == id && m.available)
    }

    pub(crate) fn agent_method_chosen_id(&self, kind: AgentKind) -> Option<String> {
        self.agent_method_chosen(kind).map(|m| m.id)
    }

    /// The way an agent is installed here: the owner's choice, else the first
    /// (the vendor's own script).
    pub(crate) fn agent_method(&self, kind: AgentKind) -> Option<InstallMethod> {
        self.agent_method_chosen(kind)
            .or_else(|| self.agent_methods(kind).into_iter().find(|m| m.available))
    }

    /// The install command for an agent, as one shell line.
    pub(crate) fn agent_install_line(&self, kind: AgentKind) -> Option<String> {
        self.agent_method(kind).map(|m| m.command)
    }

    /// Every agent this station keeps track of: the catalogue, and any agent
    /// whose config says how to read its version or update it.
    pub(crate) fn agent_kinds(&self) -> Vec<AgentKind> {
        let mut kinds: Vec<AgentKind> = catalogue::all().iter().map(|p| p.kind).collect();
        for (name, a) in &self.cfg.agents {
            let described = a.version_cmd.is_some() || a.update_cmd.is_some();
            if let Some(k) = AgentKind::parse(name).filter(|k| adapter(*k).is_some()) {
                if described && !kinds.contains(&k) {
                    kinds.push(k);
                }
            }
        }
        kinds
    }

    /// Run a command to its end, or to a time limit, and return how it ended
    /// and what it printed. Blocking.
    pub(crate) fn run_captured(&self, argv: &[String], limit: Duration) -> Ran {
        // A quick question (a version) that gets no answer is left to
        // finish by itself: the program may be updating itself, and
        // killing it there can leave it without its binary.
        self.run_watched(argv, limit, limit >= Duration::from_secs(60), &|_| {})
    }

    /// `run_captured`, saying whether a command still running at the limit
    /// is stopped (a long job that overran: an install, an update) or left
    /// alone, and showing `watch` what it has printed so far, twice a
    /// second.
    pub(crate) fn run_watched(
        &self,
        argv: &[String],
        limit: Duration,
        stop_at_limit: bool,
        watch: &dyn Fn(&str),
    ) -> Ran {
        let not_run = |text: String| Ran {
            ended: Ended::NotRun { missing: false },
            text,
        };
        let Some((program, args)) = argv.split_first() else {
            return not_run("no command".into());
        };
        // Output goes to a file, so a talkative installer cannot fill a pipe.
        let file = self.paths.state_dir.join(format!("{}.out", new_id("run")));
        let open = || std::fs::File::create(&file);
        let (Ok(out), Ok(err)) = (open(), std::fs::OpenOptions::new().append(true).open(&file))
        else {
            return not_run("could not capture the command's output".into());
        };
        let child = std::process::Command::new(program)
            .args(args)
            .env("PATH", self.agent_path_env())
            .current_dir(&self.home)
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn();
        let result = match child {
            Err(e) => Ran {
                ended: Ended::NotRun {
                    missing: e.kind() == std::io::ErrorKind::NotFound && !is_path(program),
                },
                text: format!("could not run {program}: {e}"),
            },
            Ok(mut child) => {
                let start = Instant::now();
                let mut looked = Instant::now();
                let status = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) if start.elapsed() < limit => {
                            std::thread::sleep(Duration::from_millis(20));
                            if looked.elapsed() >= Duration::from_millis(500) {
                                looked = Instant::now();
                                watch(&plain(&read_end(&file, 4096)));
                            }
                        }
                        _ => {
                            if !stop_at_limit {
                                std::thread::spawn(move || {
                                    let _ = child.wait();
                                });
                            } else {
                                let _ = child.kill();
                                let _ = child.wait();
                            }
                            break None;
                        }
                    }
                };
                let text = plain(&String::from_utf8_lossy(
                    &std::fs::read(&file).unwrap_or_default(),
                ));
                match status {
                    Some(s) => Ran {
                        ended: Ended::Exited { ok: s.success() },
                        text,
                    },
                    None => Ran {
                        ended: Ended::NoAnswer,
                        text: format!(
                            "{text}\nno answer after {} seconds{}",
                            limit.as_secs(),
                            if stop_at_limit { ": stopped" } else { "" }
                        ),
                    },
                }
            }
        };
        let _ = std::fs::remove_file(&file);
        result
    }

    /// Ask one copy of an agent's program for its version.
    fn probe_copy(&self, kind: AgentKind, path: &std::path::Path) -> Probe {
        let configured = self
            .cfg
            .agents
            .get(kind.as_str())
            .and_then(|a| a.version_cmd.clone())
            .filter(|c| !c.is_empty());
        let mut probe = Probe {
            at: Instant::now(),
            path: Some(path.to_path_buf()),
            version: None,
            health: Health::Ok,
            problem: None,
        };
        let argv = match (configured, catalogue::program(kind)) {
            (Some(argv), _) => argv,
            (None, Some(p)) => {
                let mut argv = vec![path.display().to_string()];
                argv.extend(p.version_args.iter().map(|a| a.to_string()));
                argv
            }
            // Nothing says how to ask this one: being there is all we know.
            (None, None) => return probe,
        };
        let ran = self.run_captured(&argv, VERSION_LIMIT);
        match ran.ended {
            Ended::Exited { ok: true } => probe.version = catalogue::parse_version(&ran.text),
            Ended::NoAnswer => probe.health = Health::NoAnswer,
            Ended::Exited { ok: false } | Ended::NotRun { .. } => {
                probe.health = Health::Broken;
                probe.problem = Some(
                    catalogue::error_line(&ran.text)
                        .unwrap_or_else(|| "it exited with an error and printed nothing".into()),
                );
            }
        }
        probe.at = Instant::now();
        probe
    }

    /// What was last read of an agent's program, however long ago.
    pub(crate) fn agent_probe_last(&self, kind: AgentKind) -> Option<Probe> {
        self.agents.probes.lock().unwrap().get(&kind).cloned()
    }

    /// The same, when it is still good for `max_age`.
    fn agent_probe_fresh(&self, kind: AgentKind, max_age: Duration) -> Option<Probe> {
        let p = self.agent_probe_last(kind)?;
        let good_for = match p.health {
            // A program that did not answer is not asked again at once:
            // it is never killed, so each asking leaves one more running.
            Health::Ok | Health::NoAnswer => max_age,
            // Missing or broken may be in the middle of an install.
            Health::Missing | Health::Broken => max_age.min(FRESH_MISSING),
        };
        (p.at.elapsed() < good_for && p.path.as_ref().is_none_or(runnable)).then_some(p)
    }

    /// Where an agent's program is, whether it answers and which version it
    /// is. When there are several copies, the first that answers is used,
    /// not the first that exists. Remembered; `max_age` zero asks again.
    /// Blocking when it asks.
    pub(crate) fn agent_probe(&self, kind: AgentKind, max_age: Duration) -> Probe {
        if let Some(p) = self.agent_probe_fresh(kind, max_age) {
            return p;
        }
        let copies = self
            .agent_program(kind)
            .map(|p| self.find_programs(&p))
            .unwrap_or_default();
        let mut best: Option<Probe> = None;
        for path in copies.iter().take(MAX_COPIES) {
            let probe = self.probe_copy(kind, path);
            let better = match (&best, probe.health) {
                (_, Health::Ok) | (None, _) => true,
                // One that may yet answer is used before one that failed.
                (Some(b), Health::NoAnswer) => b.health == Health::Broken,
                _ => false,
            };
            if better {
                best = Some(probe);
            }
            if best.as_ref().is_some_and(|b| b.health == Health::Ok) {
                break;
            }
        }
        let probe = best.unwrap_or(Probe {
            at: Instant::now(),
            path: None,
            version: None,
            health: Health::Missing,
            problem: None,
        });
        self.agents
            .probes
            .lock()
            .unwrap()
            .insert(kind, probe.clone());
        probe
    }

    /// What a pane for this agent is started with. Cheap: a program that
    /// answered in the last ten minutes is not asked again.
    pub(crate) fn program_for_start(&self, kind: AgentKind, program: &str) -> Found {
        let cfg = self.cfg.agents.get(kind.as_str());
        let can_ask = catalogue::program(kind).is_some()
            || cfg.is_some_and(|a| a.version_cmd.as_ref().is_some_and(|c| !c.is_empty()));
        // A pane given a program of its own is not this station's agent
        // program: it is only looked for.
        if !can_ask || self.agent_program(kind).as_deref() != Some(program) {
            return match self.find_program(program) {
                Some(path) => Found::Run(path),
                None => Found::Missing,
            };
        }
        let probe = blocking(|| self.agent_probe(kind, FRESH_FOR_START));
        match (probe.health, probe.path) {
            (Health::Broken, Some(path)) => Found::Broken {
                path,
                problem: probe.problem.unwrap_or_default(),
            },
            (_, Some(path)) => Found::Run(path),
            (_, None) => Found::Missing,
        }
    }

    fn live_panes_of(&self, kind: AgentKind) -> Vec<std::sync::Arc<crate::daemon::PaneEntry>> {
        self.list_panes()
            .into_iter()
            .filter(|p| {
                let m = p.meta.lock().unwrap();
                m.agent == kind && m.exit.is_none()
            })
            .collect()
    }

    /// The versions `agents.status` shows. One that was read lately is
    /// used as it is; the others are read again, side by side, and waited
    /// for a few seconds at most. A program that has not answered by then
    /// is shown as it was last seen and marked `reading`: the caller looks
    /// again. (Each was read in turn before, five seconds each at most,
    /// which is longer than a call from another station is given: one slow
    /// program cost a station every row.)
    fn probes_for_status(&self) -> Vec<(AgentKind, Probe, bool)> {
        let kinds = self.agent_kinds();
        let mut stale = vec![];
        for &k in &kinds {
            if self.agent_probe_fresh(k, FRESH).is_some() {
                continue;
            }
            stale.push((k, self.agent_probe_last(k).map(|p| p.at)));
            if self.agents.probing.lock().unwrap().insert(k) {
                let d = self.clone();
                std::thread::spawn(move || {
                    d.agent_probe(k, FRESH);
                    d.agents.probing.lock().unwrap().remove(&k);
                });
            }
        }
        let read_since = |(k, was): &(AgentKind, Option<Instant>)| {
            self.agent_probe_last(*k).map(|p| p.at) != *was
        };
        let start = Instant::now();
        while start.elapsed() < STATUS_WAIT && !stale.iter().all(read_since) {
            std::thread::sleep(Duration::from_millis(15));
        }
        kinds
            .into_iter()
            .map(|k| {
                let reading = stale.iter().any(|s| s.0 == k && !read_since(s));
                let probe = self.agent_probe_last(k).unwrap_or_else(|| {
                    // Never read, and still being asked: there or not is
                    // all that is known.
                    let path = self.agent_program(k).and_then(|p| self.find_program(&p));
                    Probe {
                        at: Instant::now(),
                        health: if path.is_some() {
                            Health::NoAnswer
                        } else {
                            Health::Missing
                        },
                        path,
                        version: None,
                        problem: None,
                    }
                });
                (k, probe, reading)
            })
            .collect()
    }

    /// `agents.status`: every agent this station knows, installed or not,
    /// with its job (an update or install, running or ended) and whether
    /// its version is the newest published. Blocking for a few seconds at
    /// most, when a version has to be read.
    pub fn agents_status(&self) -> Value {
        let updates = self.db.log_query(None, Some("agents.updated"), 0, 200);
        let installs = self.db.log_query(None, Some("agents.installed"), 0, 200);
        let jobs = self.agents.jobs.lock().unwrap().clone();
        let rows: Vec<Value> = self
            .probes_for_status()
            .into_iter()
            .map(|(k, probe, reading)| {
                let last = updates
                    .iter()
                    .find(|u| u["data"]["agent"] == json!(k))
                    .map(|u| {
                        let d = &u["data"];
                        json!({"at_ms": u["ts"], "ok": d["ok"], "from": d["from"], "to": d["to"], "message": d["message"]})
                    });
                let last_install = installs
                    .iter()
                    .find(|u| u["data"]["agent"] == json!(k))
                    .map(|u| {
                        let d = &u["data"];
                        json!({"at_ms": u["ts"], "ok": d["ok"], "method": d["method"], "to": d["to"], "message": d["message"]})
                    });
                let methods: Vec<Value> = self
                    .agent_methods(k)
                    .iter()
                    .map(|m| json!({"id": m.id, "label": m.label, "command": m.command, "available": m.available}))
                    .collect();
                let method = self.agent_method(k).map(|m| m.id);
                let job = jobs.get(&k);
                let running = |action: &str| job.is_some_and(|j| j.running() && j.action == action);
                let update_command = probe
                    .path
                    .as_ref()
                    .and_then(|path| self.agent_update_shown(k, path));
                json!({
                    "agent": k, "name": k.display(),
                    "installed": probe.path.is_some(),
                    "health": probe.health, "problem": probe.problem,
                    "path": probe.path, "version": probe.version,
                    // The version is being read again: look again shortly.
                    "reading": reading,
                    "panes": self.live_panes_of(k).len(),
                    "updating": running("update"),
                    "installing": running("install"),
                    "job": job,
                    "latest": self.latest_view(k, &probe),
                    "last_update": last,
                    "last_install": last_install,
                    "can_install": method.is_some(),
                    "method": method,
                    "methods": methods,
                    "can_update": update_command.is_some() || self.agent_update_argv(k, &PathBuf::new()).is_some(),
                    "update_command": update_command,
                    "by_itself": catalogue::program(k).map(|p| p.by_itself),
                })
            })
            .collect();
        json!(rows)
    }

    /// The short list in `station.info`. Never waits: it is what was last
    /// read, and empty until the first reading.
    pub(crate) fn agents_brief(&self) -> Value {
        let probes = self.agents.probes.lock().unwrap();
        let rows: Vec<Value> = self
            .agent_kinds()
            .into_iter()
            .filter_map(|k| {
                let p = probes.get(&k)?;
                Some(
                    json!({"agent": k, "installed": p.path.is_some(), "health": p.health, "version": p.version}),
                )
            })
            .collect();
        json!(rows)
    }

    /// Read every agent's version again, off the event loop.
    pub(crate) fn agents_refresh(&self) {
        let d = self.clone();
        tokio::task::spawn_blocking(move || {
            for k in d.agent_kinds() {
                d.agent_probe(k, FRESH);
            }
        });
    }

    fn agent_update_argv(&self, kind: AgentKind, program: &std::path::Path) -> Option<Vec<String>> {
        let configured = self
            .cfg
            .agents
            .get(kind.as_str())
            .and_then(|a| a.update_cmd.clone())
            .filter(|c| !c.is_empty());
        // A copy the owner installed with npm or Homebrew is updated the
        // same way, where the vendor documents how.
        let by_method = || {
            let line = self.agent_method_chosen(kind)?.update?;
            Some(sys::shell_line(&line))
        };
        configured.or_else(by_method).or_else(|| {
            let p = catalogue::program(kind)?;
            let mut argv = vec![program.display().to_string()];
            argv.extend(p.update_args.iter().map(|a| a.to_string()));
            Some(argv)
        })
    }

    /// The update command as the owner would type it: `claude update`,
    /// `npm install -g …`. For the row, while it runs.
    fn agent_update_shown(&self, kind: AgentKind, program: &std::path::Path) -> Option<String> {
        let configured = self
            .cfg
            .agents
            .get(kind.as_str())
            .and_then(|a| a.update_cmd.clone())
            .filter(|c| !c.is_empty());
        if let Some(argv) = configured {
            return Some(shell_join(&argv));
        }
        if let Some(line) = self.agent_method_chosen(kind).and_then(|m| m.update) {
            return Some(line);
        }
        let p = catalogue::program(kind)?;
        let name = program
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.program.to_string());
        Some(
            std::iter::once(name.as_str())
                .chain(p.update_args.iter().copied())
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    /// How long an updater or installer may run.
    fn job_limit(&self) -> Duration {
        let secs = self
            .flag("agents.job_limit_secs")
            .and_then(|v| v.as_u64())
            .filter(|s| *s > 0)
            .unwrap_or(JOB_LIMIT_SECS);
        Duration::from_secs(secs)
    }

    fn job_save(&self, kind: AgentKind, job: &Job) {
        if let Ok(text) = serde_json::to_string(job) {
            self.db.kv_set(&job_key(kind), &text);
        }
    }

    /// Start a job for a program, unless one is running: then that one is
    /// the answer, and nothing is started. One at a time per program.
    fn job_begin(
        &self,
        kind: AgentKind,
        action: &str,
        by: &str,
    ) -> std::result::Result<Job, Box<Job>> {
        let mut jobs = self.agents.jobs.lock().unwrap();
        if let Some(running) = jobs.get(&kind).filter(|j| j.running()) {
            return Err(Box::new(running.clone()));
        }
        let job = Job {
            id: new_id("job"),
            action: action.to_string(),
            agent: kind.as_str().to_string(),
            state: "running".into(),
            step: "looking".into(),
            started_ms: now_ms(),
            by: by.to_string(),
            limit_secs: self.job_limit().as_secs(),
            ..Default::default()
        };
        jobs.insert(kind, job.clone());
        drop(jobs);
        self.job_save(kind, &job);
        Ok(job)
    }

    fn job_change(&self, kind: AgentKind, change: impl FnOnce(&mut Job)) -> Option<Job> {
        let mut jobs = self.agents.jobs.lock().unwrap();
        let job = jobs.get_mut(&kind)?;
        change(job);
        Some(job.clone())
    }

    /// A job is over: its record says how, and is stored.
    fn job_end(&self, kind: AgentKind, change: impl FnOnce(&mut Job)) -> Option<Job> {
        let job = self.job_change(kind, |j| {
            change(j);
            j.state = "done".into();
            j.step = String::new();
            j.last_line = None;
            j.ended_ms = Some(now_ms());
        })?;
        self.job_save(kind, &job);
        self.emit(
            "agents.status",
            None,
            None,
            json!({"agent": kind, "station": self.station}),
        );
        Some(job)
    }

    /// At start: the jobs as they were stored. One that was running when
    /// spandeck stopped is over, and says so: nothing is left "running"
    /// that no one is running.
    pub(crate) fn agents_jobs_load(&self) {
        for kind in self.agent_kinds() {
            let Some(mut job) = self
                .db
                .kv_get(&job_key(kind))
                .and_then(|t| serde_json::from_str::<Job>(&t).ok())
            else {
                continue;
            };
            if job.running() {
                job.state = "done".into();
                job.step = String::new();
                job.ended_ms = Some(now_ms());
                job.outcome = Some("interrupted".into());
                job.reason = Some(format!(
                    "interrupted by a restart of spandeck on {}: whether the command finished is not known. The version shown is what the program says now.",
                    self.station
                ));
                self.job_save(kind, &job);
            }
            self.agents.jobs.lock().unwrap().insert(kind, job);
        }
    }

    /// `agents.update`: start updating one agent, or every installed one,
    /// and return at once with what was started. It waits for nothing: the
    /// job reads the version itself, and `agents.status` says how it goes.
    /// (It read each version first before, a few seconds each: over a link
    /// to another station the caller gave up before the answer, and was
    /// told nothing it could use.) A program whose job is running is not
    /// started a second time: the answer names the running job.
    pub fn agents_update(
        &self,
        agent: Option<AgentKind>,
        restart: bool,
        by: &str,
    ) -> Result<Value> {
        let kinds = match agent {
            Some(k) if self.agent_kinds().contains(&k) => vec![k],
            Some(k) => {
                return Err(invalid(format!(
                    "spandeck does not know how to update {}",
                    k.display()
                )))
            }
            None => self.agent_kinds(),
        };
        let all = agent.is_none();
        let batch = all.then(|| new_id("batch"));
        let runtime = tokio::runtime::Handle::try_current().ok();
        let (mut started, mut skipped) = (vec![], vec![]);
        for k in kinds {
            let last = self.agent_probe_last(k);
            // What is known without asking the program anything: whether
            // it is there at all is a look at the disk.
            let there = last.as_ref().is_some_and(|p| p.path.is_some())
                || self
                    .agent_program(k)
                    .is_some_and(|p| self.find_program(&p).is_some());
            let why = match &last {
                _ if !there => Some("not installed on this station"),
                Some(p) if p.health == Health::Broken && all => {
                    Some("installed but it does not start: reinstall it")
                }
                _ if self.agent_update_argv(k, &PathBuf::new()).is_none() => {
                    Some("no update command is known for it")
                }
                _ => None,
            };
            if let Some(why) = why {
                skipped.push(json!({"agent": k, "name": k.display(), "why": why}));
                continue;
            }
            let job = match self.job_begin(k, "update", by) {
                Ok(job) => job,
                Err(running) => {
                    let why = match running.action.as_str() {
                        "install" => "an install is already running",
                        _ => "an update is already running",
                    };
                    skipped.push(json!({"agent": k, "name": k.display(), "why": why, "job": running.brief()}));
                    continue;
                }
            };
            let from = last.and_then(|p| p.version);
            self.job_change(k, |j| {
                j.from = from.clone();
                j.restart = restart;
                j.batch = batch.clone();
            });
            started.push(json!({"agent": k, "name": k.display(), "from": from, "job": job.id}));
            self.emit(
                "agents.updating",
                None,
                None,
                json!({"agent": k, "station": self.station}),
            );
            let (d, by, runtime) = (self.clone(), by.to_string(), runtime.clone());
            std::thread::spawn(move || {
                let _in_runtime = runtime.as_ref().map(|h| h.enter());
                d.agent_update_run(k, restart, &by);
            });
        }
        if !all && started.is_empty() {
            // Already running: the caller is told of that job, not refused.
            if let Some(running) = skipped.first().and_then(|s| s.get("job")).cloned() {
                return Ok(
                    json!({"station": self.station, "started": [], "skipped": skipped, "running": running, "restart": restart}),
                );
            }
            let why = skipped[0]["why"].as_str().unwrap_or("it cannot be updated");
            return Err(invalid(format!("{}: {why}", kinds_name(agent))));
        }
        Ok(
            json!({"station": self.station, "started": started, "skipped": skipped, "restart": restart, "batch": batch}),
        )
    }

    /// One update, start to finish, on its own thread.
    fn agent_update_run(&self, kind: AgentKind, restart: bool, by: &str) {
        let probe = self.agent_probe(kind, Duration::ZERO);
        let from = probe.version.clone();
        let cannot = match (&probe.path, probe.health) {
            (None, _) => Some("it is not installed on this station".to_string()),
            // Its updater is the program itself, which does not start.
            (_, Health::Broken) => Some(format!(
                "it is installed but does not start ({}): reinstall it",
                probe.problem.as_deref().unwrap_or("it said nothing")
            )),
            _ => None,
        };
        let path = probe.path.clone().unwrap_or_default();
        let argv = self.agent_update_argv(kind, &path);
        let (ran, argv) = match (cannot, argv) {
            (Some(why), _) => (Err(why), None),
            (None, None) => (Err("no update command is known for it".to_string()), None),
            (None, Some(argv)) => {
                let shown = self.agent_update_shown(kind, &path);
                self.job_change(kind, |j| {
                    j.step = "running".into();
                    j.command = shown;
                    j.from = from.clone();
                });
                let limit = self.job_limit();
                let ran = self.run_watched(&argv, limit, true, &|text| {
                    let line = last_line(text);
                    self.job_change(kind, |j| j.last_line = line);
                });
                (Ok((ran, limit)), Some(argv))
            }
        };
        let (ok, output, reason) = match ran {
            Err(why) => (false, String::new(), Some(why)),
            Ok((ran, limit)) => {
                let reason =
                    match ran.ended {
                        Ended::Exited { ok: true } => None,
                        Ended::NoAnswer => Some(format!(
                            "it was still running after {} and was stopped",
                            minutes(limit)
                        )),
                        _ => Some(catalogue::error_line(&ran.text).unwrap_or_else(|| {
                            "it exited with an error and printed nothing".into()
                        })),
                    };
                (ran.ok(), ran.text, reason)
            }
        };
        let to = match argv {
            Some(_) => self.agent_probe(kind, Duration::ZERO).version,
            None => from.clone(),
        };
        let changed = ok && to.is_some() && to != from;
        let (mut restarted, mut waiting) = (vec![], vec![]);
        if changed && restart {
            for p in self.live_panes_of(kind) {
                if busy(p.meta.lock().unwrap().state) {
                    // Not interrupted: it restarts when this turn is over.
                    self.agents
                        .restart_when_idle
                        .lock()
                        .unwrap()
                        .insert(p.id.clone());
                    waiting.push(p.id.clone());
                } else if self.restart_pane(&p.id, false, "agent update").is_ok() {
                    restarted.push(p.id.clone());
                }
            }
        }
        let outcome = match (ok, changed) {
            (false, _) => "failed",
            (true, true) => "updated",
            // The updater ran and the version is what it was: the
            // program's own answer to "is there anything newer?".
            (true, false) => "latest",
        };
        let job = self.job_end(kind, |j| {
            j.outcome = Some(outcome.into());
            j.reason = reason.clone();
            j.from = from.clone();
            j.to = to.clone();
            j.output = Some(tail(&output, OUTPUT_KEPT));
            j.restarted = restarted.clone();
            j.restart_when_idle = waiting.clone();
        });
        let message = tail(&output, 600);
        self.record(
            "agents.updated",
            None,
            None,
            None,
            Some(by),
            json!({
                "agent": kind, "name": kind.display(), "station": self.station,
                "ok": ok, "from": from, "to": to, "changed": changed, "message": message,
                "outcome": outcome, "reason": reason, "job": job.map(|j| j.id),
                "at_ms": now_ms(), "restarted": restarted, "restart_when_idle": waiting,
            }),
        );
        if !ok {
            self.notify(
                "error",
                None,
                None,
                &format!("Updating {} failed on {}", kind.display(), self.station),
                &reason.unwrap_or_else(|| tail(&output, 300)),
            );
        }
    }

    /// `agents.install`: install an agent's program again, or for the first
    /// time, with the way the owner chose, and remember that choice for this
    /// station. Returns at once with what was started. The caller names an
    /// agent and a way; the command comes from the catalogue or this
    /// station's config. While a job for the program runs, nothing more is
    /// started: the answer names the running job.
    pub fn agents_install(&self, kind: AgentKind, method: Option<&str>, by: &str) -> Result<Value> {
        let methods = self.agent_methods(kind);
        if !self.agent_kinds().contains(&kind) || methods.is_empty() {
            return Err(invalid(format!(
                "spandeck knows no install command for {}. Set [agents.{}] install_cmd in config.toml",
                kind.display(),
                kind.as_str()
            )));
        }
        let chosen = match method {
            Some(id) => methods.into_iter().find(|m| m.id == id).ok_or_else(|| {
                invalid(format!(
                    "{} cannot be installed with `{id}` on {}",
                    kind.display(),
                    self.station
                ))
            })?,
            None => self
                .agent_method(kind)
                .ok_or_else(|| invalid("no way to install it is available on this station"))?,
        };
        if !chosen.available {
            return Err(invalid(format!(
                "{} is not on {}, so {} cannot be installed with it",
                chosen.label,
                self.station,
                kind.display()
            )));
        }
        let job = match self.job_begin(kind, "install", by) {
            Ok(job) => job,
            Err(running) => {
                return Ok(json!({
                    "station": self.station, "agent": kind, "name": kind.display(),
                    "started": false, "running": running.brief(),
                }))
            }
        };
        if chosen.id != "config" {
            self.db.kv_set(&method_key(kind), &chosen.id);
        }
        let from = self.agent_probe_last(kind).and_then(|p| p.version);
        self.job_change(kind, |j| {
            j.step = "running".into();
            j.command = Some(chosen.command.clone());
            j.method = Some(chosen.id.clone());
            j.from = from.clone();
        });
        self.emit(
            "agents.installing",
            None,
            None,
            json!({"agent": kind, "station": self.station}),
        );
        let started = json!({
            "station": self.station, "agent": kind, "name": kind.display(),
            "method": chosen.id, "command": chosen.command, "from": from,
            "started": true, "job": job.id,
        });
        let runtime = tokio::runtime::Handle::try_current().ok();
        let (d, by) = (self.clone(), by.to_string());
        std::thread::spawn(move || {
            let _in_runtime = runtime.as_ref().map(|h| h.enter());
            let argv = sys::shell_line(&chosen.command);
            let limit = d.job_limit();
            let ran = d.run_watched(&argv, limit, true, &|text| {
                let line = last_line(text);
                d.job_change(kind, |j| j.last_line = line);
            });
            // Whether it worked is what the program says afterwards, not the
            // installer's exit code.
            let after = d.agent_probe(kind, Duration::ZERO);
            let ok = after.health == Health::Ok;
            let reason = (!ok).then(|| match (&after.problem, ran.ended) {
                (Some(p), _) => format!("it still does not start: {p}"),
                (None, Ended::NoAnswer) => format!(
                    "the install was still running after {} and was stopped",
                    minutes(limit)
                ),
                (None, _) if after.health == Health::NoAnswer => {
                    "it was installed, and did not answer when asked its version".to_string()
                }
                (None, _) => catalogue::error_line(&ran.text).unwrap_or_else(|| {
                    "the install command printed nothing, and the program is still not there".into()
                }),
            });
            let job = d.job_end(kind, |j| {
                j.outcome = Some(if ok { "installed" } else { "failed" }.into());
                j.reason = reason.clone();
                j.to = after.version.clone();
                j.output = Some(tail(&ran.text, OUTPUT_KEPT));
            });
            d.record(
                "agents.installed",
                None,
                None,
                None,
                Some(&by),
                json!({
                    "agent": kind, "name": kind.display(), "station": d.station,
                    "method": chosen.id, "command": chosen.command, "ok": ok,
                    "health": after.health, "problem": after.problem,
                    "from": from, "to": after.version,
                    "reason": reason, "job": job.map(|j| j.id),
                    "message": tail(&ran.text, 600), "at_ms": now_ms(),
                }),
            );
            if matches!(after.health, Health::Missing | Health::Broken) {
                let why = match &after.problem {
                    Some(p) => format!("It still does not start: {p}"),
                    None => tail(&ran.text, 300),
                };
                d.notify(
                    "error",
                    None,
                    None,
                    &format!(
                        "Installing {} did not work on {}",
                        kind.display(),
                        d.station
                    ),
                    &why,
                );
            }
        });
        Ok(started)
    }

    /// Restart the panes that were busy when their agent was updated, each
    /// as soon as it is idle. Called on every tick.
    pub(crate) fn pump_agent_restarts(&self) {
        let waiting: Vec<String> = {
            let w = self.agents.restart_when_idle.lock().unwrap();
            if w.is_empty() {
                return;
            }
            w.iter().cloned().collect()
        };
        for id in waiting {
            let state = self.pane(&id).ok().and_then(|p| {
                let m = p.meta.lock().unwrap();
                m.exit.is_none().then_some(m.state)
            });
            if state.is_some_and(busy) {
                continue;
            }
            self.agents.restart_when_idle.lock().unwrap().remove(&id);
            if state.is_some() {
                let d = self.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = d.restart_pane(&id, false, "agent update");
                });
            }
        }
    }

    /// What a pane runs when its agent's program is missing, or is there but
    /// does not start: say what was found, show the install command, run it
    /// where the user can watch, then start the agent. Ctrl-C skips the
    /// install and ends the pane with a plain message. `$1` name, `$2`
    /// program, `$3` folders to look in afterwards, `$4` the install command,
    /// `$5` the copy that does not start and `$6` what it said (both empty
    /// when none was found), `$7` the arguments that ask its version (empty:
    /// only look for it); the rest are the agent's own arguments.
    pub(crate) fn install_then_launch(
        &self,
        kind: AgentKind,
        program: &str,
        install: &str,
        broken: Option<(&std::path::Path, &str)>,
        args: Vec<String>,
    ) -> (String, Vec<String>) {
        const SCRIPT: &str = r#"name=$1; prog=$2; PATH="$PATH:$3"; cmd=$4; found=$5; problem=$6; ask=$7; shift 7
did=installed; [ -n "$found" ] && did=reinstalled
trap 'echo; echo "Skipped: $name was not $did, so this pane has nothing to start. Install it, then restart this pane."; exit 130' INT
if [ -n "$found" ]; then
  printf '%s is installed on this station but does not start.\n\n    found:   %s\n    it said: %s\n\n' "$name" "$found" "$problem"
  printf 'spandeck will reinstall it now with:\n\n    %s\n\nPress Ctrl-C to skip.\n\n' "$cmd"
else
  printf '%s is not installed on this station (there is no %s in the folders spandeck looks in).\n' "$name" "$prog"
  printf 'spandeck will install it now with:\n\n    %s\n\nPress Ctrl-C to skip.\n\n' "$cmd"
fi
sh -c "$cmd" </dev/tty; status=$?
trap - INT
# Whether it worked is judged by the program answering, not by the
# installer's exit code: Codex's installer ends by offering to start Codex
# and reports failure when that last step cannot run, after a good install.
# The first copy that answers is started, so a broken copy earlier on the
# path does not hide the one just installed. A copy that is slow to answer
# is started too, and is never stopped: slow is not broken.
tmp=$(mktemp "${TMPDIR:-/tmp}/spandeck-ask.XXXXXX" 2>/dev/null || echo "/tmp/spandeck-ask.$$"); seen=; said=
try() {
  c=$1; shift
  [ -f "$c" ] && [ -x "$c" ] || return 0
  if [ -n "$ask" ]; then
    rm -f "$tmp.rc"
    ( "$c" $ask >"$tmp" 2>&1 </dev/null; echo $? >"$tmp.rc" ) &
    n=0; while [ ! -s "$tmp.rc" ] && [ "$n" -lt 50 ]; do sleep 0.1; n=$((n+1)); done
    if [ -s "$tmp.rc" ] && [ "$(cat "$tmp.rc")" != 0 ]; then
      if [ -z "$seen" ]; then seen=$c; said=$(grep -i -m 1 error "$tmp" || tail -n 1 "$tmp"); fi
      return 0
    fi
  fi
  rm -f "$tmp" "$tmp.rc"; echo; exec "$c" "$@"
}
case $prog in
  */*) try "$prog" "$@" ;;
  *) oldifs=$IFS; IFS=:; for d in $PATH; do IFS=$oldifs; try "$d/$prog" "$@"; done; IFS=$oldifs ;;
esac
rm -f "$tmp" "$tmp.rc"; echo
if [ -n "$seen" ]; then
  printf 'The install command ran, but %s still does not start.\n\n    found:   %s\n    it said: %s\n\nFix the problem above, then restart this pane.\n' "$name" "$seen" "$said"
  exit 1
fi
if [ "$status" -ne 0 ]; then echo "The install command failed, so $name was not started. Fix the problem above, then restart this pane."; exit 1; fi
echo "The install command finished, but there is still no $prog in the folders spandeck looks in. Set [agents.<name>] program in spandeck's config.toml to where it was installed."
exit 127"#;
        // The probe that said "missing" or "broken" must not outlive the install.
        self.agents.probes.lock().unwrap().remove(&kind);
        let dirs = crate::paths::path_with("", &self.agent_dirs());
        // `path_with` starts from an empty PATH, which leaves a separator in front.
        let dirs = dirs.trim_start_matches([':', ';']).to_string();
        let configured_ask = self
            .cfg
            .agents
            .get(kind.as_str())
            .is_some_and(|a| a.version_cmd.is_some());
        let ask = match catalogue::program(kind) {
            Some(p) if !configured_ask => p.version_args.join(" "),
            _ => String::new(),
        };
        let (found, problem) = match broken {
            Some((path, problem)) => (path.display().to_string(), plain(problem)),
            None => Default::default(),
        };
        if sys::WINDOWS {
            // The same steps, done by spandeck itself: there is no `sh`.
            let mut wrapped = vec![
                "install-then-launch".to_string(),
                kind.display().to_string(),
                program.to_string(),
                dirs,
                install.to_string(),
                found,
                problem,
                ask,
            ];
            wrapped.extend(args);
            return (self.exe.display().to_string(), wrapped);
        }
        let mut wrapped = vec![
            "-c".to_string(),
            SCRIPT.to_string(),
            "spandeck-install".to_string(),
            kind.display().to_string(),
            program.to_string(),
            dirs,
            install.to_string(),
            found,
            problem,
            ask,
        ];
        wrapped.extend(args);
        ("/bin/sh".to_string(), wrapped)
    }
}

/// The end of a file a command is writing its output to.
fn read_end(file: &std::path::Path, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut text = Vec::new();
    if let Ok(mut f) = std::fs::File::open(file) {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let _ = f.seek(SeekFrom::Start(len.saturating_sub(max)));
        let _ = f.read_to_end(&mut text);
    }
    String::from_utf8_lossy(&text).into_owned()
}

fn method_key(kind: AgentKind) -> String {
    format!("agents.method.{}", kind.as_str())
}

// Takes `&PathBuf` because it is used as a filter over lists of them.
#[allow(clippy::ptr_arg)]
fn runnable(p: &PathBuf) -> bool {
    sys::is_executable(p)
}

/// Does this name say where the program is, or only what it is called?
fn is_path(name: &str) -> bool {
    name.contains('/') || (sys::WINDOWS && (name.contains('\\') || name.contains(':')))
}

/// A folder from the catalogue: `~/…` is under the home folder, and
/// `%NAME%\…` under the folder that variable names (left out when it is not set).
fn expand_dir(dir: &str, home: &std::path::Path) -> Option<PathBuf> {
    if let Some(rest) = dir.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    if let Some((var, rest)) = dir.strip_prefix('%').and_then(|d| d.split_once('%')) {
        let base = std::env::var_os(var).filter(|v| !v.is_empty())?;
        return Some(PathBuf::from(base).join(rest.trim_start_matches(['\\', '/'])));
    }
    Some(PathBuf::from(dir))
}

/// `spandeck install-then-launch`: what a Windows pane runs when its agent's
/// program is missing or does not start. The Unix pane runs the shell script
/// in `install_then_launch`; these are the same steps and the same words.
/// Arguments: name, program, folders to look in afterwards, the install
/// command, the copy that does not start and what it said (both empty when
/// none was found), the arguments that ask its version (empty: only look
/// for it); the rest are the agent's own arguments. Returns the exit code.
pub fn install_then_launch_main(argv: &[String]) -> i32 {
    let [name, prog, dirs, cmd, found, problem, ask, rest @ ..] = argv else {
        eprintln!("install-then-launch: not enough arguments");
        return 64;
    };
    // Ctrl-C ends the installer, not this program, which says what happened.
    sys::ignore_interrupt();
    if found.is_empty() {
        println!("{name} is not installed on this station (there is no {prog} in the folders spandeck looks in).");
        println!("spandeck will install it now with:\n\n    {cmd}\n\nPress Ctrl-C to skip.\n");
    } else {
        println!("{name} is installed on this station but does not start.\n\n    found:   {found}\n    it said: {problem}\n");
        println!("spandeck will reinstall it now with:\n\n    {cmd}\n\nPress Ctrl-C to skip.\n");
    }
    let line = sys::shell_line(cmd);
    let status = std::process::Command::new(&line[0])
        .args(&line[1..])
        .status();
    if sys::interrupted() {
        let did = if found.is_empty() {
            "installed"
        } else {
            "reinstalled"
        };
        println!("\nSkipped: {name} was not {did}, so this pane has nothing to start. Install it, then restart this pane.");
        return 130;
    }
    // Whether it worked is judged by the program answering, not by the
    // installer's exit code. The first copy that answers is started; one
    // that is slow to answer is started too, and never stopped.
    let ask: Vec<&str> = ask.split_whitespace().collect();
    let names = sys::executable_names(prog);
    let candidates: Vec<PathBuf> = if is_path(prog) {
        std::iter::once(prog.clone())
            .chain(names)
            .map(PathBuf::from)
            .collect()
    } else {
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path)
            .chain(std::env::split_paths(dirs))
            .filter(|d| !d.as_os_str().is_empty())
            .flat_map(|d| names.iter().map(|n| d.join(n)).collect::<Vec<_>>())
            .collect()
    };
    let mut seen: Option<(PathBuf, String)> = None;
    for c in candidates.iter().filter(|c| runnable(c)) {
        if !ask.is_empty() {
            let answer = std::process::Command::new(c)
                .args(&ask)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn();
            let failed = match answer {
                Err(e) => Some(e.to_string()),
                Ok(mut child) => {
                    let start = Instant::now();
                    loop {
                        match child.try_wait() {
                            Ok(Some(s)) if s.success() => break None,
                            Ok(Some(_)) => {
                                let out = child.wait_with_output().ok();
                                let text = out
                                    .map(|o| {
                                        format!(
                                            "{}\n{}",
                                            String::from_utf8_lossy(&o.stdout),
                                            String::from_utf8_lossy(&o.stderr)
                                        )
                                    })
                                    .unwrap_or_default();
                                break Some(catalogue::error_line(&text).unwrap_or_default());
                            }
                            // Slow is not broken: it is started, and left alone.
                            _ if start.elapsed() > VERSION_LIMIT => break None,
                            _ => std::thread::sleep(Duration::from_millis(100)),
                        }
                    }
                }
            };
            if let Some(said) = failed {
                seen.get_or_insert((c.clone(), said));
                continue;
            }
        }
        println!();
        return match std::process::Command::new(c).args(rest).status() {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                println!("{} could not be started: {e}", c.display());
                1
            }
        };
    }
    println!();
    if let Some((path, said)) = seen {
        println!("The install command ran, but {name} still does not start.\n\n    found:   {}\n    it said: {said}\n\nFix the problem above, then restart this pane.", path.display());
        return 1;
    }
    if !status.is_ok_and(|s| s.success()) {
        println!("The install command failed, so {name} was not started. Fix the problem above, then restart this pane.");
        return 1;
    }
    println!("The install command finished, but there is still no {prog} in the folders spandeck looks in. Set [agents.<name>] program in spandeck's config.toml to where it was installed.");
    127
}

/// Run something that may wait a few seconds without holding up the event
/// loop it was called from.
fn blocking<R>(f: impl FnOnce() -> R) -> R {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(h) if h.runtime_flavor() == RuntimeFlavor::MultiThread => tokio::task::block_in_place(f),
        _ => f(),
    }
}

/// A time limit in words: "15 minutes", "2 seconds".
fn minutes(limit: Duration) -> String {
    match limit.as_secs() {
        s if s >= 120 => format!("{} minutes", s / 60),
        s => format!("{s} seconds"),
    }
}

fn kinds_name(agent: Option<AgentKind>) -> &'static str {
    agent.map(|k| k.display()).unwrap_or("agents")
}
