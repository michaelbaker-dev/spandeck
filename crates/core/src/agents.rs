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

use crate::daemon::{invalid, shell_quote, Daemon, Result};
use crate::paths::{new_id, now_ms};

/// How long a version that was read stays good.
const FRESH: Duration = Duration::from_secs(60);
/// A missing program is looked for again sooner: it may be installing.
const FRESH_MISSING: Duration = Duration::from_secs(5);
/// A pane start trusts a program that answered this recently: asking again
/// on every start would slow each one, and the tick asks every few minutes.
const FRESH_FOR_START: Duration = Duration::from_secs(600);
pub(crate) const VERSION_LIMIT: Duration = Duration::from_secs(5);
const UPDATE_LIMIT: Duration = Duration::from_secs(600);
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

#[derive(Default)]
pub struct AgentsState {
    probes: Mutex<HashMap<AgentKind, Probe>>,
    updating: Mutex<HashSet<AgentKind>>,
    installing: Mutex<HashSet<AgentKind>>,
    /// Panes to restart on the new version once their turn is over.
    restart_when_idle: Mutex<HashSet<String>>,
}

/// In the middle of something the owner would lose by a restart.
fn busy(state: AgentState) -> bool {
    matches!(state, AgentState::Working | AgentState::NeedsYou)
}

/// Text a program printed, without the escape sequences that colour it.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e.is_ascii_alphabetic() || e == '\x07' {
                    break;
                }
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
        catalogue::WELL_KNOWN_DIRS
            .iter()
            .map(|d| match d.strip_prefix("~/") {
                Some(rest) => self.home.join(rest),
                None => PathBuf::from(d),
            })
            .collect()
    }

    /// PATH with those folders added, for a command spandeck runs itself.
    fn agent_path_env(&self) -> String {
        let mut path = std::env::var("PATH").unwrap_or_default();
        for d in self.agent_dirs() {
            path.push(':');
            path.push_str(&d.display().to_string());
        }
        path
    }

    /// Every copy of a program, in the order they are looked for: a path as
    /// given, else on PATH, else in the folders the vendors' installers use.
    /// Two names for one file count once.
    pub(crate) fn find_programs(&self, name: &str) -> Vec<PathBuf> {
        if name.contains('/') {
            let p = PathBuf::from(name);
            return if runnable(&p) { vec![p] } else { vec![] };
        }
        let path = std::env::var("PATH").unwrap_or_default();
        let mut seen = HashSet::new();
        path.split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .chain(self.agent_dirs())
            .map(|d| d.join(name))
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
            .map(|c| {
                c.iter()
                    .map(|a| shell_quote(a))
                    .collect::<Vec<_>>()
                    .join(" ")
            });
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
        let mut kinds: Vec<AgentKind> = catalogue::CATALOGUE.iter().map(|p| p.kind).collect();
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
                    missing: e.kind() == std::io::ErrorKind::NotFound && !program.contains('/'),
                },
                text: format!("could not run {program}: {e}"),
            },
            Ok(mut child) => {
                let start = Instant::now();
                let status = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) if start.elapsed() < limit => {
                            std::thread::sleep(Duration::from_millis(20))
                        }
                        _ => {
                            // A quick question (a version) that gets no answer
                            // is left to finish by itself: the program may be
                            // updating itself, and killing it there can leave
                            // it without its binary. Only a long job that
                            // overran (an install, an update) is stopped.
                            if limit < Duration::from_secs(60) {
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
                            if limit < Duration::from_secs(60) {
                                ""
                            } else {
                                ": stopped"
                            }
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

    /// Where an agent's program is, whether it answers and which version it
    /// is. When there are several copies, the first that answers is used,
    /// not the first that exists. Remembered; `max_age` zero asks again.
    /// Blocking when it asks.
    pub(crate) fn agent_probe(&self, kind: AgentKind, max_age: Duration) -> Probe {
        if let Some(p) = self.agents.probes.lock().unwrap().get(&kind) {
            let good_for = match p.health {
                // A program that did not answer is not asked again at once:
                // it is never killed, so each asking leaves one more running.
                Health::Ok | Health::NoAnswer => max_age,
                // Missing or broken may be in the middle of an install.
                Health::Missing | Health::Broken => max_age.min(FRESH_MISSING),
            };
            if p.at.elapsed() < good_for && p.path.as_ref().is_none_or(runnable) {
                return p.clone();
            }
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

    /// `agents.status`: every agent this station knows, installed or not.
    /// Blocking when a version has to be read.
    pub fn agents_status(&self) -> Value {
        let updates = self.db.log_query(None, Some("agents.updated"), 0, 200);
        let installs = self.db.log_query(None, Some("agents.installed"), 0, 200);
        let updating = self.agents.updating.lock().unwrap().clone();
        let installing = self.agents.installing.lock().unwrap().clone();
        let rows: Vec<Value> = self
            .agent_kinds()
            .into_iter()
            .map(|k| {
                let probe = self.agent_probe(k, FRESH);
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
                json!({
                    "agent": k, "name": k.display(),
                    "installed": probe.path.is_some(),
                    "health": probe.health, "problem": probe.problem,
                    "path": probe.path, "version": probe.version,
                    "panes": self.live_panes_of(k).len(),
                    "updating": updating.contains(&k),
                    "installing": installing.contains(&k),
                    "last_update": last,
                    "last_install": last_install,
                    "can_install": method.is_some(),
                    "method": method,
                    "methods": methods,
                    "can_update": self.agent_update_argv(k, &PathBuf::new()).is_some(),
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
            Some(vec!["sh".to_string(), "-c".to_string(), line])
        };
        configured.or_else(by_method).or_else(|| {
            let p = catalogue::program(kind)?;
            let mut argv = vec![program.display().to_string()];
            argv.extend(p.update_args.iter().map(|a| a.to_string()));
            Some(argv)
        })
    }

    /// `agents.update`: start updating one agent, or every installed one, and
    /// return at once with what was started. Blocking only to read versions.
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
        let runtime = tokio::runtime::Handle::try_current().ok();
        let (mut started, mut skipped) = (vec![], vec![]);
        for k in kinds {
            let mut skip =
                |why: &str| skipped.push(json!({"agent": k, "name": k.display(), "why": why}));
            let probe = self.agent_probe(k, Duration::ZERO);
            let Some(path) = probe.path.clone() else {
                skip("not installed on this station");
                continue;
            };
            if probe.health == Health::Broken {
                // Its updater is the program itself, which does not start.
                skip("installed but it does not start: reinstall it");
                continue;
            }
            let Some(argv) = self.agent_update_argv(k, &path) else {
                skip("no update command is known for it");
                continue;
            };
            if !self.agents.updating.lock().unwrap().insert(k) {
                skip("an update is already running");
                continue;
            }
            started.push(json!({"agent": k, "name": k.display(), "from": probe.version}));
            self.emit(
                "agents.updating",
                None,
                None,
                json!({"agent": k, "station": self.station}),
            );
            let (d, by, from, runtime) =
                (self.clone(), by.to_string(), probe.version, runtime.clone());
            std::thread::spawn(move || {
                let _in_runtime = runtime.as_ref().map(|h| h.enter());
                d.agent_update_run(k, &argv, from, restart, &by);
                d.agents.updating.lock().unwrap().remove(&k);
                d.emit(
                    "agents.status",
                    None,
                    None,
                    json!({"agent": k, "station": d.station}),
                );
            });
        }
        if agent.is_some() && started.is_empty() {
            let why = skipped[0]["why"].as_str().unwrap_or("it cannot be updated");
            return Err(invalid(format!("{}: {why}", kinds_name(agent))));
        }
        Ok(
            json!({"station": self.station, "started": started, "skipped": skipped, "restart": restart}),
        )
    }

    /// One update, start to finish, on its own thread.
    fn agent_update_run(
        &self,
        kind: AgentKind,
        argv: &[String],
        from: Option<String>,
        restart: bool,
        by: &str,
    ) {
        let ran = self.run_captured(argv, UPDATE_LIMIT);
        let (ok, output) = (ran.ok(), ran.text);
        let to = self.agent_probe(kind, Duration::ZERO).version;
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
                "at_ms": now_ms(), "restarted": restarted, "restart_when_idle": waiting,
            }),
        );
        if !ok {
            self.notify(
                "error",
                None,
                None,
                &format!("Updating {} failed on {}", kind.display(), self.station),
                &tail(&output, 300),
            );
        }
    }

    /// `agents.install`: install an agent's program again, or for the first
    /// time, with the way the owner chose, and remember that choice for this
    /// station. Returns at once with what was started. The caller names an
    /// agent and a way; the command comes from the catalogue or this
    /// station's config.
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
        if !self.agents.updating.lock().unwrap().insert(kind) {
            return Err(invalid(format!(
                "{}: an install or update is already running",
                kind.display()
            )));
        }
        self.agents.installing.lock().unwrap().insert(kind);
        if chosen.id != "config" {
            self.db.kv_set(&method_key(kind), &chosen.id);
        }
        let before = self.agent_probe(kind, FRESH);
        self.emit(
            "agents.installing",
            None,
            None,
            json!({"agent": kind, "station": self.station}),
        );
        let started = json!({
            "station": self.station, "agent": kind, "name": kind.display(),
            "method": chosen.id, "command": chosen.command, "from": before.version,
        });
        let runtime = tokio::runtime::Handle::try_current().ok();
        let (d, by) = (self.clone(), by.to_string());
        std::thread::spawn(move || {
            let _in_runtime = runtime.as_ref().map(|h| h.enter());
            let argv = ["sh".to_string(), "-c".to_string(), chosen.command.clone()];
            let ran = d.run_captured(&argv, UPDATE_LIMIT);
            // Whether it worked is what the program says afterwards, not the
            // installer's exit code.
            let after = d.agent_probe(kind, Duration::ZERO);
            let ok = after.health == Health::Ok;
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
                    "from": before.version, "to": after.version,
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
            d.agents.installing.lock().unwrap().remove(&kind);
            d.agents.updating.lock().unwrap().remove(&kind);
            d.emit(
                "agents.status",
                None,
                None,
                json!({"agent": kind, "station": d.station}),
            );
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
        let dirs = self
            .agent_dirs()
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
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

fn method_key(kind: AgentKind) -> String {
    format!("agents.method.{}", kind.as_str())
}

fn runnable(p: &PathBuf) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
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

fn kinds_name(agent: Option<AgentKind>) -> &'static str {
    agent.map(|k| k.display()).unwrap_or("agents")
}
