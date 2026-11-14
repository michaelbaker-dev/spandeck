//! The agent programs on this station: where each one is, which version it
//! is, installing one that is missing, and updating them.
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
const VERSION_LIMIT: Duration = Duration::from_secs(5);
const UPDATE_LIMIT: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub(crate) struct Probe {
    at: Instant,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}

#[derive(Default)]
pub struct AgentsState {
    probes: Mutex<HashMap<AgentKind, Probe>>,
    updating: Mutex<HashSet<AgentKind>>,
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

    /// Find a program: a path as given, else on PATH, else in the folders
    /// the vendors' installers use.
    pub(crate) fn find_program(&self, name: &str) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let runnable = |p: &PathBuf| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        };
        if name.contains('/') {
            let p = PathBuf::from(name);
            return runnable(&p).then_some(p);
        }
        let path = std::env::var("PATH").unwrap_or_default();
        path.split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .chain(self.agent_dirs())
            .map(|d| d.join(name))
            .find(runnable)
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

    /// The install command for an agent, as one shell line.
    pub(crate) fn agent_install_line(&self, kind: AgentKind) -> Option<String> {
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
        configured.or_else(|| catalogue::program(kind).map(|p| p.install.to_string()))
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

    /// Run a command to its end, or to a time limit, and return whether it
    /// succeeded and what it printed. Blocking.
    fn run_captured(&self, argv: &[String], limit: Duration) -> (bool, String) {
        let Some((program, args)) = argv.split_first() else {
            return (false, "no command".into());
        };
        // Output goes to a file, so a talkative installer cannot fill a pipe.
        let file = self.paths.state_dir.join(format!("{}.out", new_id("run")));
        let open = || std::fs::File::create(&file);
        let (Ok(out), Ok(err)) = (open(), std::fs::OpenOptions::new().append(true).open(&file))
        else {
            return (false, "could not capture the command's output".into());
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
            Err(e) => (false, format!("could not run {program}: {e}")),
            Ok(mut child) => {
                let start = Instant::now();
                let status = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) if start.elapsed() < limit => {
                            std::thread::sleep(Duration::from_millis(20))
                        }
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break None;
                        }
                    }
                };
                let text = plain(&String::from_utf8_lossy(
                    &std::fs::read(&file).unwrap_or_default(),
                ));
                match status {
                    Some(s) => (s.success(), text),
                    None => (
                        false,
                        format!(
                            "{text}\nstopped: it ran longer than {} seconds",
                            limit.as_secs()
                        ),
                    ),
                }
            }
        };
        let _ = std::fs::remove_file(&file);
        result
    }

    /// Where an agent's program is and which version it is. Remembered for a
    /// minute; `max_age` zero reads it again. Blocking when it reads.
    pub(crate) fn agent_probe(&self, kind: AgentKind, max_age: Duration) -> Probe {
        if let Some(p) = self.agents.probes.lock().unwrap().get(&kind) {
            let good_for = if p.path.is_some() {
                max_age
            } else {
                max_age.min(FRESH_MISSING)
            };
            if p.at.elapsed() < good_for {
                return p.clone();
            }
        }
        let path = self.agent_program(kind).and_then(|p| self.find_program(&p));
        let version = path.as_ref().and_then(|path| {
            let configured = self
                .cfg
                .agents
                .get(kind.as_str())
                .and_then(|a| a.version_cmd.clone())
                .filter(|c| !c.is_empty());
            let argv = configured.unwrap_or_else(|| {
                let mut argv = vec![path.display().to_string()];
                let args = catalogue::program(kind)
                    .map(|p| p.version_args)
                    .unwrap_or(&["--version"]);
                argv.extend(args.iter().map(|a| a.to_string()));
                argv
            });
            let (ok, text) = self.run_captured(&argv, VERSION_LIMIT);
            ok.then(|| catalogue::parse_version(&text)).flatten()
        });
        let probe = Probe {
            at: Instant::now(),
            path,
            version,
        };
        self.agents
            .probes
            .lock()
            .unwrap()
            .insert(kind, probe.clone());
        probe
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
        let updating = self.agents.updating.lock().unwrap().clone();
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
                json!({
                    "agent": k, "name": k.display(),
                    "installed": probe.path.is_some(),
                    "path": probe.path, "version": probe.version,
                    "panes": self.live_panes_of(k).len(),
                    "updating": updating.contains(&k),
                    "last_update": last,
                    "can_install": self.agent_install_line(k).is_some(),
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
                Some(json!({"agent": k, "installed": p.path.is_some(), "version": p.version}))
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
        configured.or_else(|| {
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
        let (ok, output) = self.run_captured(argv, UPDATE_LIMIT);
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

    /// What a pane runs when its agent's program is missing: say so, show the
    /// install command, run it where the user can watch, then start the
    /// agent. Ctrl-C skips the install and ends the pane with a plain
    /// message. `$1` name, `$2` program, `$3` folders to look in afterwards,
    /// `$4` the install command; the rest are the agent's own arguments.
    pub(crate) fn install_then_launch(
        &self,
        kind: AgentKind,
        program: &str,
        install: &str,
        args: Vec<String>,
    ) -> (String, Vec<String>) {
        const SCRIPT: &str = r#"name=$1; prog=$2; PATH="$PATH:$3"; cmd=$4; shift 4
printf '%s is not installed on this station (%s was not found).\n' "$name" "$prog"
printf 'spandeck will install it now with:\n\n    %s\n\nPress Ctrl-C to skip.\n\n' "$cmd"
trap 'echo; echo "Skipped: $name was not installed, so this pane has nothing to start. Install it, then restart this pane."; exit 130' INT
sh -c "$cmd" || { echo; echo "The install command failed, so $name was not started. Fix the problem above, then restart this pane."; exit 1; }
trap - INT
if command -v "$prog" >/dev/null 2>&1; then echo; exec "$prog" "$@"; fi
echo; echo "The install command finished, but $prog is still not found. Set [agents.<name>] program in spandeck's config.toml to where it was installed."
exit 127"#;
        // The probe that said "missing" must not outlive the install.
        self.agents.probes.lock().unwrap().remove(&kind);
        let dirs = self
            .agent_dirs()
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
        let mut wrapped = vec![
            "-c".to_string(),
            SCRIPT.to_string(),
            "spandeck-install".to_string(),
            kind.display().to_string(),
            program.to_string(),
            dirs,
            install.to_string(),
        ];
        wrapped.extend(args);
        ("/bin/sh".to_string(), wrapped)
    }
}

fn kinds_name(agent: Option<AgentKind>) -> &'static str {
    agent.map(|k| k.display()).unwrap_or("agents")
}
