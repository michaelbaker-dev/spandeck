//! The daemon: panes, the event bus, the ship's log.

use std::collections::{BTreeMap, HashMap};
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spandeck_agents::{adapter, install, kind_from_command, AgentKind, AgentState, LaunchOpts};
use spandeck_archive::Archive;
use spandeck_terminal::{ExitInfo, Pane, PaneEvent, PaneHandoff, ScreenInfo, Signal, SpawnSpec};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::db::Db;
use crate::paths::{new_id, now_ms, random_token, Config, Paths};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Bumped when stations can no longer talk to an older peer.
pub const PROTOCOL: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no such pane: {0}")]
    NoPane(String),
    #[error("not allowed: {0}")]
    Denied(String),
    #[error("{0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::NoPane(_) | Error::NotFound(_) => "not_found",
            Error::Denied(_) => "denied",
            Error::Invalid(_) => "invalid",
            _ => "internal",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

/// One event on the bus. The Control Deck is drawn from these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusEvent {
    pub seq: u64,
    pub ts: u64,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub data: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnStats {
    pub started_ms: u64,
    pub tool_calls: u32,
    pub file_changes: u32,
    /// The failing command seen last, and how many times in a row.
    pub fail_cmd: Option<String>,
    pub fail_streak: u32,
    /// Consecutive busy turns that changed no files.
    pub idle_turns: u32,
}

/// Everything about a pane except its terminal. Persisted so a pane can be
/// resumed after a crash or reboot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    pub agent: AgentKind,
    pub spec: SpawnSpec,
    pub cwd: String,
    pub created_ms: u64,
    pub state: AgentState,
    pub state_since_ms: u64,
    /// Why the pane needs you, in plain words.
    #[serde(default)]
    pub needs_reason: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub tool_since_ms: u64,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub exit: Option<ExitInfo>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub task: Option<String>,
    /// The pane that delegated to this one.
    #[serde(default)]
    pub parent_pane: Option<String>,
    #[serde(default)]
    pub hooks_seen: bool,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub watch: bool,
    /// Launcher slot in the project file, so reopening restores this pane.
    #[serde(default)]
    pub slot: Option<String>,
    #[serde(default)]
    pub last_message: Option<String>,
    #[serde(default)]
    pub turn: TurnStats,
    /// Client that owns this pane's size; others letterbox.
    #[serde(default)]
    pub controller: Option<String>,
}

pub struct PaneEntry {
    pub id: String,
    pub token: String,
    pub term: Pane,
    pub tx: broadcast::Sender<PaneEvent>,
    pub meta: Mutex<PaneMeta>,
}

impl PaneEntry {
    pub fn meta(&self) -> PaneMeta {
        self.meta.lock().unwrap().clone()
    }

    pub fn view(&self, station: &str) -> Value {
        let m = self.meta();
        let info = self.term.info();
        json!({
            "id": m.id, "station": station, "address": format!("{}@{}", m.id, station),
            "name": m.name, "project": m.project, "role": m.role,
            "agent": m.agent, "agent_name": m.agent.display(), "cwd": m.cwd,
            "created_ms": m.created_ms, "state": m.state, "state_since_ms": m.state_since_ms,
            "needs_reason": m.needs_reason, "tool": m.tool, "tool_since_ms": m.tool_since_ms,
            "session_id": m.session_id, "title": if m.title.is_empty() { info.title.clone() } else { m.title.clone() },
            "exit": m.exit, "exited": m.exit.is_some(), "paused": m.paused, "task": m.task,
            "parent_pane": m.parent_pane, "hooks_seen": m.hooks_seen, "muted": m.muted,
            "watch": m.watch, "slot": m.slot, "last_message": m.last_message,
            "cols": info.cols, "rows": info.rows, "controller": m.controller,
            "last_output_ms": self.term.last_output_ms(), "pid": self.term.pid(),
            "program": m.spec.program,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SpawnOpts {
    pub id: Option<String>,
    pub token: Option<String>,
    pub name: Option<String>,
    pub program: Option<String>,
    pub args: Vec<String>,
    pub agent: Option<AgentKind>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub project: Option<String>,
    pub role: Option<String>,
    pub task: Option<String>,
    pub parent_pane: Option<String>,
    pub resume: Option<String>,
    pub prompt: Option<String>,
    pub slot: Option<String>,
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// The user at a Control Deck or CLI: full authority on this station.
    Owner,
    ReadOnly,
    /// An agent in a pane: only what its pane and task grant. Never administer.
    Agent,
    /// Another station under the same login, relaying for its own callers.
    Peer,
}

#[derive(Debug, Clone)]
pub struct Caller {
    pub authority: Authority,
    pub pane: Option<String>,
    /// Stable id of the connected client (presence, size control).
    pub client: String,
    /// For the ship's log: "you", a pane id, or a station name.
    pub who: String,
}

impl Caller {
    pub fn owner() -> Caller {
        Caller {
            authority: Authority::Owner,
            pane: None,
            client: "local".into(),
            who: "you".into(),
        }
    }
    pub fn is_owner(&self) -> bool {
        self.authority == Authority::Owner
    }
}

#[derive(Debug, Clone)]
pub struct ApprovalAnswer {
    pub allow: bool,
    pub reason: Option<String>,
}

pub(crate) enum PaneMsg {
    Term(String, PaneEvent),
    Osc7501(String, AgentState),
}

pub struct Inner {
    pub paths: Paths,
    pub cfg: Config,
    pub db: Arc<Db>,
    pub archive: Archive,
    pub station: String,
    pub client_token: String,
    pub bus: broadcast::Sender<BusEvent>,
    pub panes: RwLock<BTreeMap<String, Arc<PaneEntry>>>,
    pub started_ms: u64,
    /// The binary panes call back into (`spandeck hook`, `spandeck mcp`).
    pub exe: PathBuf,
    pub home: PathBuf,
    pub(crate) msg_tx: mpsc::UnboundedSender<PaneMsg>,
    pub(crate) approvals: Mutex<HashMap<String, oneshot::Sender<ApprovalAnswer>>>,
    /// client id -> last input time: who is "at a keyboard".
    pub(crate) presence: Mutex<HashMap<String, u64>>,
    /// When each client last had its typing into a pane written to the log.
    pub(crate) typed: Mutex<HashMap<String, u64>>,
    pub(crate) keep_awake: Mutex<Option<std::process::Child>>,
    pub(crate) seq: AtomicU64,
    pub fleet: crate::fleet::FleetState,
    /// The fleet key, and requests to be let in.
    pub trust: Arc<crate::trust::TrustState>,
    pub web_addrs: Mutex<Vec<String>>,
    pub vault: crate::vault::VaultState,
}

#[derive(Clone)]
pub struct Daemon(pub Arc<Inner>);

impl Deref for Daemon {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

#[derive(Default)]
pub struct StartOpts {
    /// State left by the previous image of this process (upgrade handoff).
    pub handoff: Option<PathBuf>,
    pub exe: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
pub struct HandoffPane {
    pub meta: PaneMeta,
    pub token: String,
    pub pty: PaneHandoff,
}

#[derive(Serialize, Deserialize)]
pub struct HandoffState {
    pub version: u32,
    pub from_version: String,
    pub pid: u32,
    pub prev_exe: PathBuf,
    pub panes: Vec<HandoffPane>,
}

impl Daemon {
    pub fn start(paths: Paths, cfg: Config, opts: StartOpts) -> anyhow::Result<Daemon> {
        std::fs::create_dir_all(&paths.state_dir)?;
        std::fs::create_dir_all(&paths.config_dir)?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&paths.state_dir, std::fs::Permissions::from_mode(0o700));
        }
        let db = Arc::new(Db::open(&paths.db())?);
        let archive = Archive::open(&paths.archive_db())?;
        let client_token = match std::fs::read_to_string(paths.token_file()) {
            Ok(t) if t.trim().len() >= 32 => t.trim().to_string(),
            _ => {
                let t = random_token();
                write_private(&paths.token_file(), &t)?;
                t
            }
        };
        let (bus, _) = broadcast::channel(4096);
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let exe = opts
            .exe
            .or_else(|| std::env::var_os("SPANDECK_BIN").map(PathBuf::from))
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| PathBuf::from("spandeck"));
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
        let station = cfg.station.clone();
        let trust = Arc::new(crate::trust::TrustState::load(&paths)?);
        let daemon = Daemon(Arc::new(Inner {
            fleet: crate::fleet::FleetState::new(&cfg, trust.clone()),
            trust,
            paths,
            cfg,
            db,
            archive,
            station,
            client_token,
            bus,
            panes: RwLock::new(BTreeMap::new()),
            started_ms: now_ms(),
            exe,
            home,
            msg_tx,
            approvals: Mutex::new(HashMap::new()),
            presence: Mutex::new(HashMap::new()),
            typed: Mutex::new(HashMap::new()),
            keep_awake: Mutex::new(None),
            seq: AtomicU64::new(0),
            web_addrs: Mutex::new(vec![]),
            vault: Default::default(),
        }));
        let _ = std::fs::write(daemon.paths.pid_file(), std::process::id().to_string());

        let adopted = match opts.handoff {
            Some(file) => daemon.adopt_handoff(&file),
            None => false,
        };
        if !adopted {
            daemon.recover();
        }
        // Keep hold of this binary. A hard link costs nothing and survives the
        // file at its path being replaced by an installer; where a link is not
        // possible (another filesystem), rollback uses the path as it is then.
        {
            let link = daemon.paths.running_exe();
            if let Some(dir) = link.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::remove_file(&link);
            let _ = std::fs::hard_link(&daemon.exe, &link);
        }
        daemon.spawn_tasks(msg_rx);
        Ok(daemon)
    }

    fn spawn_tasks(&self, mut msg_rx: mpsc::UnboundedReceiver<PaneMsg>) {
        let d = self.clone();
        tokio::spawn(async move {
            while let Some(msg) = msg_rx.recv().await {
                match msg {
                    PaneMsg::Term(id, ev) => d.on_pane_event(&id, ev),
                    PaneMsg::Osc7501(id, state) => {
                        // Read when present; hooks stay the primary signal.
                        if let Ok(p) = d.pane(&id) {
                            if !p.meta.lock().unwrap().hooks_seen {
                                d.set_state(&p, state, None);
                            }
                        }
                    }
                }
            }
        });
        let d = self.clone();
        tokio::spawn(async move {
            let mut n: u64 = 0;
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                n += 1;
                d.tick(n).await;
            }
        });
    }

    async fn tick(&self, n: u64) {
        self.detect_screens();
        self.update_keep_awake();
        if n.is_multiple_of(2) {
            let d = self.clone();
            let _ = tokio::task::spawn_blocking(move || d.archive.poll()).await;
            self.pump_mail();
        }
        self.pump_relay(n).await;
        self.pump_recipes().await;
        if n.is_multiple_of(4) {
            self.pump_notifications().await;
        }
        if n % 20 == 1 {
            self.discover_peers().await;
            self.vault_discover().await;
        }
        if n % 20 == 11 {
            self.vault_maintain().await;
            self.vault_report_archive().await;
        }
        if n % 4 == 2 {
            self.pump_stops().await;
        }
    }

    // ---------------------------------------------------------------- events

    pub fn emit(&self, kind: &str, pane: Option<&str>, project: Option<&str>, data: Value) {
        let ev = BusEvent {
            seq: self.seq.fetch_add(1, Ordering::SeqCst) + 1,
            ts: now_ms(),
            kind: kind.to_string(),
            pane: pane.map(|s| s.to_string()),
            project: project.map(|s| s.to_string()),
            data,
        };
        let _ = self.bus.send(ev);
    }

    /// Emit and write to the ship's log.
    pub fn record(
        &self,
        kind: &str,
        pane: Option<&str>,
        project: Option<&str>,
        task: Option<&str>,
        actor: Option<&str>,
        data: Value,
    ) {
        let ulid = format!(
            "{}-{}",
            ulid::Ulid::generate().to_string().to_lowercase(),
            self.station
        );
        self.db
            .log(&ulid, now_ms(), kind, pane, project, task, actor, &data);
        self.emit(kind, pane, project, data);
    }

    // ----------------------------------------------------------------- panes

    pub fn pane(&self, id: &str) -> Result<Arc<PaneEntry>> {
        let panes = self.panes.read().unwrap();
        if let Some(p) = panes.get(id) {
            return Ok(p.clone());
        }
        // Accept `id@station` for this station, and a unique name.
        let bare = id.strip_suffix(&format!("@{}", self.station)).unwrap_or(id);
        if let Some(p) = panes.get(bare) {
            return Ok(p.clone());
        }
        let mut by_name = panes.values().filter(|p| {
            let m = p.meta.lock().unwrap();
            m.exit.is_none()
                && (m.name == bare
                    || m.project
                        .as_ref()
                        .is_some_and(|proj| format!("{proj}/{}", m.name) == bare))
        });
        match (by_name.next(), by_name.next()) {
            (Some(p), None) => Ok(p.clone()),
            (Some(_), Some(_)) => Err(invalid(format!(
                "more than one pane is called {bare}; use its id"
            ))),
            _ => Err(Error::NoPane(id.to_string())),
        }
    }

    pub fn pane_by_token(&self, token: &str) -> Option<Arc<PaneEntry>> {
        self.panes
            .read()
            .unwrap()
            .values()
            .find(|p| p.token == token)
            .cloned()
    }

    pub fn list_panes(&self) -> Vec<Arc<PaneEntry>> {
        self.panes.read().unwrap().values().cloned().collect()
    }

    pub fn pane_views(&self) -> Vec<Value> {
        self.list_panes()
            .iter()
            .map(|p| p.view(&self.station))
            .collect()
    }

    fn make_sink(&self, id: &str, tx: broadcast::Sender<PaneEvent>) -> spandeck_terminal::Sink {
        let msg_tx = self.msg_tx.clone();
        let id = id.to_string();
        Box::new(move |ev| match ev {
            PaneEvent::Output(bytes) => {
                if bytes.windows(7).any(|w| w == b"\x1b]7501;") {
                    if let Some(state) = spandeck_agents::detect::parse_osc7501(&bytes) {
                        let _ = msg_tx.send(PaneMsg::Osc7501(id.clone(), state));
                    }
                }
                // Never blocks: a subscriber that falls behind is told it lagged
                // and resyncs from a snapshot.
                let _ = tx.send(PaneEvent::Output(bytes));
            }
            other => {
                let _ = tx.send(other.clone());
                let _ = msg_tx.send(PaneMsg::Term(id.clone(), other));
            }
        })
    }

    pub fn spawn_pane(&self, opts: SpawnOpts) -> Result<Arc<PaneEntry>> {
        let id = opts.id.clone().unwrap_or_else(|| new_id("p"));
        let token = opts.token.clone().unwrap_or_else(random_token);
        let cols = opts.cols.unwrap_or(120).clamp(2, 500);
        let rows = opts.rows.unwrap_or(32).clamp(1, 300);

        let mut agent = opts.agent.unwrap_or_else(|| {
            opts.program
                .as_deref()
                .map(kind_from_command)
                .unwrap_or(AgentKind::Shell)
        });
        let (program, args) = match (adapter(agent), &opts.program) {
            (Some(a), _) => {
                let cfg = self
                    .cfg
                    .agents
                    .get(agent.as_str())
                    .cloned()
                    .unwrap_or_default();
                let mut extra = cfg.args.clone();
                extra.extend(self.integration_args(agent));
                extra.extend(opts.extra_args.iter().cloned());
                extra.extend(opts.args.iter().cloned());
                let launch = a.launch(&LaunchOpts {
                    resume: opts.resume.clone(),
                    prompt: opts.prompt.clone(),
                    extra_args: extra,
                    program: opts.program.clone().or(cfg.program),
                });
                (launch.program, launch.args)
            }
            (None, Some(p)) => (p.clone(), opts.args.clone()),
            (None, None) => {
                agent = AgentKind::Shell;
                (
                    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
                    vec![],
                )
            }
        };

        // Codex cannot run a single command where the system blocks its
        // sandbox. The fix needs a password once, so it is offered in the
        // pane itself, before Codex starts; Codex starts either way.
        let (program, args) = if agent == AgentKind::Codex
            && crate::doctor::codex_sandbox_blocked(&self.home)
        {
            let mut wrapped = vec![
                    "-c".to_string(),
                    // Ctrl-C at the password prompt must skip the fix, not end the
                // pane: the shell catches it, and Codex starts regardless.
                "trap : INT; \"$0\" fix-codex-sandbox || { echo; echo 'Skipped. Codex will start, but it cannot run commands until you run: spandeck doctor --fix'; sleep 2; }; trap - INT; exec \"$@\"".to_string(),
                    self.exe.display().to_string(),
                    program,
                ];
            wrapped.extend(args);
            ("/bin/sh".to_string(), wrapped)
        } else {
            (program, args)
        };

        let cwd = opts.cwd.clone().unwrap_or_else(|| self.home.clone());
        if !cwd.is_dir() {
            return Err(invalid(format!("{} is not a directory", cwd.display())));
        }
        let mut env = opts.env.clone();
        let exe_dir = self
            .exe
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let path = std::env::var("PATH").unwrap_or_default();
        env.extend([
            ("SPANDECK_PANE".to_string(), id.clone()),
            ("SPANDECK_TOKEN".to_string(), token.clone()),
            (
                "SPANDECK_SOCKET".to_string(),
                self.paths.socket().display().to_string(),
            ),
            ("SPANDECK_BIN".to_string(), self.exe.display().to_string()),
            ("SPANDECK_STATION".to_string(), self.station.clone()),
            (
                "PATH".to_string(),
                if exe_dir.is_empty() {
                    path.clone()
                } else {
                    format!("{path}:{exe_dir}")
                },
            ),
        ]);
        if let Some(project) = &opts.project {
            env.push(("SPANDECK_PROJECT".to_string(), project.clone()));
        }
        // A daemon started from inside an agent session must not pass that
        // session's markers on: the agents in its panes are not its children.
        let env_remove = [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SSE_PORT",
            "CODEX_SANDBOX",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let spec = SpawnSpec {
            program,
            args,
            cwd: Some(cwd.clone()),
            env,
            env_remove,
            cols,
            rows,
        };

        let (tx, _) = broadcast::channel(1024);
        // An agent can call in the moment it starts, with its pane's token.
        // Hold the pane table from before the process exists until the pane
        // is in it, so that first call waits instead of being refused.
        // (It was refused, now and then, and the agent's session id was lost.)
        let mut panes = self.panes.write().unwrap();
        let term = Pane::spawn(&spec, self.make_sink(&id, tx.clone()))
            .map_err(|e| invalid(format!("could not start {}: {e}", spec.program)))?;
        let now = now_ms();
        let name = opts.name.clone().unwrap_or_else(|| {
            if agent.is_agent() {
                agent.as_str().to_string()
            } else {
                "shell".to_string()
            }
        });
        let meta = PaneMeta {
            id: id.clone(),
            name,
            project: opts.project.clone(),
            role: opts.role.clone(),
            agent,
            spec,
            cwd: cwd.display().to_string(),
            created_ms: now,
            state: AgentState::Idle,
            state_since_ms: now,
            needs_reason: None,
            tool: None,
            tool_since_ms: 0,
            session_id: opts.resume.clone(),
            transcript_path: None,
            title: String::new(),
            exit: None,
            paused: false,
            task: opts.task.clone(),
            parent_pane: opts.parent_pane.clone(),
            hooks_seen: false,
            muted: false,
            watch: false,
            slot: opts.slot.clone(),
            last_message: None,
            turn: TurnStats::default(),
            controller: None,
        };
        let entry = Arc::new(PaneEntry {
            id: id.clone(),
            token,
            term,
            tx,
            meta: Mutex::new(meta),
        });
        panes.insert(id.clone(), entry.clone());
        drop(panes);
        self.save_pane(&entry);
        let m = entry.meta();
        self.record(
            "pane.created",
            Some(&id),
            m.project.as_deref(),
            m.task.as_deref(),
            None,
            json!({"name": m.name, "agent": m.agent, "resumed": opts.resume.is_some()}),
        );
        Ok(entry)
    }

    /// Arguments that wire an agent to this daemon at launch, so a pane
    /// spandeck starts works with no global install.
    fn integration_args(&self, agent: AgentKind) -> Vec<String> {
        let bin = self.exe.display().to_string();
        let setup = self.setup();
        // The fleet-wide agent setup: model defaults and shared MCP servers.
        let mut args = setup.launch_args(agent);
        match agent {
            AgentKind::Claude => {
                let installed = install::status(agent, &self.home)
                    .map(|s| s.installed)
                    .unwrap_or(false);
                if !installed {
                    args.push("--settings".to_string());
                    args.push(json!({"hooks": install::hooks_block(agent, &bin)}).to_string());
                }
                let mut servers = setup.mcp_json(agent);
                servers.insert("spandeck".into(), json!({"command": bin, "args": ["mcp"]}));
                args.push("--mcp-config".to_string());
                args.push(json!({"mcpServers": servers}).to_string());
                // spandeck's own tools are not asked about one by one: what an
                // agent may do with them is decided by spandeck, and logged.
                args.push("--allowedTools".to_string());
                args.push("mcp__spandeck".to_string());
            }
            AgentKind::Codex => args.extend([
                "-c".to_string(),
                format!(
                    "mcp_servers.spandeck.command={}",
                    serde_json::to_string(&bin).unwrap_or_default()
                ),
                "-c".to_string(),
                "mcp_servers.spandeck.args=[\"mcp\"]".to_string(),
                // Codex starts MCP servers with a bare environment. Without
                // these the server cannot tell which pane it speaks for.
                "-c".to_string(),
                "mcp_servers.spandeck.default_tools_approval_mode=\"approve\"".to_string(),
                "-c".to_string(),
                "mcp_servers.spandeck.env_vars=[\"SPANDECK_PANE\",\"SPANDECK_TOKEN\",\"SPANDECK_SOCKET\",\"SPANDECK_STATE_DIR\",\"SPANDECK_CONFIG_DIR\",\"SPANDECK_STATION\",\"SPANDECK_PROJECT\"]".to_string(),
            ]),
            _ => {}
        }
        args
    }

    pub(crate) fn save_pane(&self, p: &PaneEntry) {
        let meta = p.meta();
        let closed: Option<i64> = meta.exit.map(|_| now_ms() as i64);
        let r = self.db.with(|c| {
            c.execute(
                "INSERT INTO panes(id,meta,token,pid,created,closed) VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(id) DO UPDATE SET meta=excluded.meta, pid=excluded.pid, closed=excluded.closed",
                rusqlite::params![
                    meta.id,
                    serde_json::to_string(&meta).unwrap_or_default(),
                    p.token,
                    p.term.pid(),
                    meta.created_ms as i64,
                    closed
                ],
            )
        });
        if let Err(e) = r {
            tracing::warn!(error = %e, "pane record write failed");
        }
    }

    fn on_pane_event(&self, id: &str, ev: PaneEvent) {
        let Ok(p) = self.pane(id) else { return };
        match ev {
            PaneEvent::Title(title) => {
                p.meta.lock().unwrap().title = title.clone();
                self.emit("pane.title", Some(id), None, json!({"title": title}));
            }
            PaneEvent::Exited(info) => {
                let meta = {
                    let mut m = p.meta.lock().unwrap();
                    m.exit = Some(info);
                    m.state = AgentState::Idle;
                    m.state_since_ms = now_ms();
                    m.tool = None;
                    m.clone()
                };
                self.save_pane(&p);
                self.cancel_pane_approvals(id);
                self.record(
                    "pane.exited",
                    Some(id),
                    meta.project.as_deref(),
                    meta.task.as_deref(),
                    None,
                    json!({"exit": info}),
                );
                self.on_pane_exit_tasks(&meta);
            }
            PaneEvent::Bell => self.emit("pane.bell", Some(id), None, json!({})),
            PaneEvent::Output(_) => {}
        }
    }

    /// Change a pane's agent state. Returns true when it changed.
    pub fn set_state(&self, p: &PaneEntry, state: AgentState, reason: Option<String>) -> bool {
        let (old, meta) = {
            let mut m = p.meta.lock().unwrap();
            if m.exit.is_some() {
                return false;
            }
            if m.state == state && m.needs_reason == reason {
                return false;
            }
            let old = m.state;
            m.state = state;
            m.needs_reason = reason.clone();
            if old != state {
                m.state_since_ms = now_ms();
            }
            (old, m.clone())
        };
        self.save_pane(p);
        self.record(
            "pane.state",
            Some(&p.id),
            meta.project.as_deref(),
            meta.task.as_deref(),
            None,
            json!({"state": state, "from": old, "reason": reason, "name": meta.name}),
        );
        if old != state {
            self.notify_state(&meta, old, state);
            // A task follows its agent: stuck on the user is blocked, and
            // working again is running. Waiting on an approval is set where
            // the approval is asked for, and wins over blocked.
            if let Some(task) = &meta.task {
                match state {
                    AgentState::NeedsYou | AgentState::Error => {
                        self.task_follow(task, "blocked", reason.as_deref())
                    }
                    AgentState::Working => self.task_follow(task, "running", None),
                    _ => {}
                }
            }
        }
        true
    }

    /// Move a task to a state its agent has put it in, where the task's
    /// state machine allows it. A finished task is never moved.
    pub(crate) fn task_follow(&self, task: &str, to: &str, why: Option<&str>) {
        let Ok(now) = self.task_get(task) else {
            return;
        };
        let from = now["state"].as_str().unwrap_or("");
        if from == to || !crate::tasks::transition_allowed(from, to) {
            return;
        }
        // Blocked never replaces waiting for approval: that is the more exact answer.
        let _ = self.task_set_state(
            task,
            to,
            json!({"resume": {"waiting_on": why}}),
            "the agent's state",
        );
    }

    /// Fallback observer for agent panes whose hooks have not reported.
    fn detect_screens(&self) {
        for p in self.list_panes() {
            let (agent, hooks_seen, exited) = {
                let m = p.meta.lock().unwrap();
                (m.agent, m.hooks_seen, m.exit.is_some())
            };
            if exited || hooks_seen {
                continue;
            }
            let Some(a) = adapter(agent) else { continue };
            let lines = p.term.lines();
            match a.detect_screen(&lines) {
                Some(state) => {
                    let reason =
                        (state == AgentState::NeedsYou).then(|| "waiting at a prompt".to_string());
                    self.set_state(&p, state, reason);
                }
                None => {
                    // Screen shows nothing recognisable: working -> done.
                    let was_working = p.meta.lock().unwrap().state == AgentState::Working;
                    if was_working && now_ms().saturating_sub(p.term.last_output_ms()) > 1500 {
                        self.set_state(&p, AgentState::Done, None);
                    }
                }
            }
        }
    }

    /// A late joiner's view: a snapshot and a stream that starts exactly after it.
    pub fn attach(
        &self,
        id: &str,
    ) -> Result<(Vec<u8>, ScreenInfo, broadcast::Receiver<PaneEvent>)> {
        let p = self.pane(id)?;
        Ok(p.term
            .with_screen(|screen| (screen.snapshot_ansi(2000), screen.info(), p.tx.subscribe())))
    }

    pub fn send_text(&self, id: &str, text: &str, enter: bool) -> Result<()> {
        let p = self.pane(id)?;
        p.term.write(text.as_bytes())?;
        if enter {
            // Agents with paste detection need the Enter as its own write.
            std::thread::sleep(Duration::from_millis(30));
            p.term.write(b"\r")?;
        }
        Ok(())
    }

    /// The session to resume for a pane that ran before, if there is a
    /// conversation to go back to. Claude Code saves nothing until the first
    /// message, and refuses to resume a session it never saved, so a session
    /// that was opened and never used starts afresh.
    pub(crate) fn resumable(&self, meta: &PaneMeta) -> Option<String> {
        let id = meta.session_id.clone()?;
        if meta.agent == AgentKind::Claude {
            let saved = meta
                .transcript_path
                .as_deref()
                .and_then(|p| std::fs::metadata(p).ok())
                .is_some_and(|m| m.len() > 0);
            if !saved {
                return None;
            }
        }
        Some(id)
    }

    /// Start a pane's program again in the same pane: same name, folder,
    /// project and role. An agent goes back to its conversation unless
    /// `fresh` is set or there is none to go back to.
    pub fn restart_pane(&self, id: &str, fresh: bool, by: &str) -> Result<Arc<PaneEntry>> {
        let old = self.pane(id)?;
        let meta = old.meta();
        let resume = if fresh { None } else { self.resumable(&meta) };
        if meta.exit.is_none() {
            let _ = old.term.signal(Signal::SIGHUP);
            let _ = old.term.signal(Signal::SIGKILL);
            // The old program's exit must be seen before the pane is reused,
            // or it would be taken for the new one's.
            let start = std::time::Instant::now();
            while old.meta().exit.is_none() && start.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        self.cancel_pane_approvals(&old.id);
        self.panes.write().unwrap().remove(&old.id);
        let p = self.spawn_pane(SpawnOpts {
            id: Some(old.id.clone()),
            name: Some(meta.name.clone()),
            agent: Some(meta.agent),
            program: self.launcher_program(&meta),
            cwd: Some(PathBuf::from(&meta.cwd)),
            cols: Some(meta.spec.cols),
            rows: Some(meta.spec.rows),
            project: meta.project.clone(),
            role: meta.role.clone(),
            task: meta.task.clone(),
            parent_pane: meta.parent_pane.clone(),
            resume: resume.clone(),
            slot: meta.slot.clone(),
            ..Default::default()
        })?;
        self.write_instructions(&p.meta());
        self.record(
            "pane.restarted",
            Some(&p.id),
            meta.project.as_deref(),
            meta.task.as_deref(),
            Some(by),
            json!({"resumed": resume.is_some(), "session_id": resume}),
        );
        self.emit("pane.updated", Some(&p.id), None, json!({}));
        Ok(p)
    }

    /// A folder as the user typed it: `~` and `~/x` are under the home folder,
    /// and a path with no leading `/` is too.
    pub fn expand_path(&self, path: &std::path::Path) -> PathBuf {
        let text = path.to_string_lossy();
        let text = text.trim();
        if text == "~" {
            self.home.clone()
        } else if let Some(rest) = text.strip_prefix("~/") {
            self.home.join(rest)
        } else if path.is_absolute() {
            PathBuf::from(text)
        } else {
            self.home.join(text)
        }
    }

    /// Paste text into a pane the way a terminal does. A program that asked
    /// for pastes to be marked (Claude Code, Codex, modern shells) gets the
    /// text as one paste, line breaks kept. Otherwise an agent gets it as one
    /// line, since each line break would send a half-finished message, and a
    /// shell gets it as typed.
    pub fn paste(&self, id: &str, text: &str, enter: bool) -> Result<()> {
        let p = self.pane(id)?;
        let text = text.replace("\r\n", "\n");
        // A paste cannot carry the marker that ends a paste.
        let text = text.replace("\x1b[201~", "").replace("\x1b[200~", "");
        if p.term.bracketed_paste() {
            p.term
                .write(format!("\x1b[200~{}\x1b[201~", text.replace('\n', "\r")).as_bytes())?;
        } else if p.meta().agent.is_agent() {
            let one_line = text
                .split('\n')
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            p.term.write(one_line.as_bytes())?;
        } else {
            p.term.write(text.replace('\n', "\r").as_bytes())?;
        }
        if enter {
            std::thread::sleep(Duration::from_millis(60));
            p.term.write(b"\r")?;
        }
        Ok(())
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16, client: &str) -> Result<()> {
        let p = self.pane(id)?;
        p.term.resize(cols.clamp(2, 500), rows.clamp(1, 300))?;
        p.meta.lock().unwrap().controller = Some(client.to_string());
        self.emit(
            "pane.resized",
            Some(&p.id),
            None,
            json!({"cols": cols, "rows": rows, "controller": client}),
        );
        Ok(())
    }

    /// Pause: no new work is delivered; the current turn finishes.
    pub fn pause(&self, id: &str, paused: bool, by: &str) -> Result<()> {
        let p = self.pane(id)?;
        let meta = {
            let mut m = p.meta.lock().unwrap();
            m.paused = paused;
            m.clone()
        };
        self.save_pane(&p);
        self.record(
            if paused {
                "pane.paused"
            } else {
                "pane.unpaused"
            },
            Some(&p.id),
            meta.project.as_deref(),
            meta.task.as_deref(),
            Some(by),
            json!({}),
        );
        Ok(())
    }

    /// Stop: the agent's current turn is interrupted; the process stays.
    pub fn stop(&self, id: &str, by: &str) -> Result<()> {
        let p = self.pane(id)?;
        let meta = p.meta();
        if meta.exit.is_some() {
            return Ok(());
        }
        let keys: &[u8] = adapter(meta.agent)
            .map(|a| a.interrupt_keys())
            .unwrap_or(b"\x03");
        p.term.write(keys)?;
        self.cancel_pane_approvals(&p.id);
        self.record(
            "pane.stopped",
            Some(&p.id),
            meta.project.as_deref(),
            meta.task.as_deref(),
            Some(by),
            json!({}),
        );
        // An interrupt while the model is thinking fires no hook at all. Confirm
        // from the screen: once the agent no longer shows itself as busy, the
        // turn is over.
        if let Some(a) = adapter(meta.agent) {
            let d = self.clone();
            let id = p.id.clone();
            tokio::spawn(async move {
                for _ in 0..40 {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    let Ok(p) = d.pane(&id) else { return };
                    let state = p.meta.lock().unwrap().state;
                    if !matches!(state, AgentState::Working | AgentState::NeedsYou) {
                        return;
                    }
                    if a.detect_screen(&p.term.lines()) != Some(AgentState::Working) {
                        p.meta.lock().unwrap().tool = None;
                        d.set_state(&p, AgentState::Idle, None);
                        return;
                    }
                }
            });
        }
        Ok(())
    }

    /// Kill: the process ends.
    pub fn kill(&self, id: &str, by: &str) -> Result<()> {
        let p = self.pane(id)?;
        let meta = p.meta();
        if meta.exit.is_none() {
            let _ = p.term.signal(Signal::SIGHUP);
            let p2 = p.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                if p2.term.exit_info().is_none() {
                    let _ = p2.term.signal(Signal::SIGKILL);
                }
            });
        }
        self.record(
            "pane.killed",
            Some(&p.id),
            meta.project.as_deref(),
            meta.task.as_deref(),
            Some(by),
            json!({}),
        );
        Ok(())
    }

    /// Forget an exited pane (or kill and forget a running one).
    pub fn close(&self, id: &str, by: &str) -> Result<()> {
        let p = self.pane(id)?;
        if p.meta.lock().unwrap().exit.is_none() {
            let _ = p.term.signal(Signal::SIGHUP);
            let _ = p.term.signal(Signal::SIGKILL);
        }
        self.panes.write().unwrap().remove(&p.id);
        let _ = self.db.with(|c| {
            c.execute(
                "UPDATE panes SET closed=?2 WHERE id=?1 AND closed IS NULL",
                rusqlite::params![p.id, now_ms() as i64],
            )
        });
        self.cancel_pane_approvals(&p.id);
        let m = p.meta();
        self.record(
            "pane.closed",
            Some(&p.id),
            m.project.as_deref(),
            m.task.as_deref(),
            Some(by),
            json!({}),
        );
        Ok(())
    }

    /// Image and file drop: write the bytes on this station, type the path.
    pub fn drop_file(&self, id: &str, name: &str, bytes: &[u8]) -> Result<String> {
        let p = self.pane(id)?;
        let clean: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let clean = clean.trim_start_matches('.').to_string();
        let clean = if clean.is_empty() {
            "file".to_string()
        } else {
            clean
        };
        if bytes.len() > 64 << 20 {
            return Err(invalid("file is larger than 64 MB"));
        }
        let dir = self.paths.drops_dir().join(&p.id);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}-{clean}", now_ms()));
        std::fs::write(&path, bytes)?;
        let shown = path.display().to_string();
        p.term
            .write(format!("{} ", shell_quote(&shown)).as_bytes())?;
        self.record(
            "pane.file_dropped",
            Some(&p.id),
            None,
            None,
            None,
            json!({"name": clean, "bytes": bytes.len()}),
        );
        Ok(shown)
    }

    // ------------------------------------------------------------ who was here

    /// The ship's log says who opened a pane's terminal.
    pub fn note_attach(&self, pane: &str, who: &str, client: &str, control: bool) {
        self.record(
            "pane.attached",
            Some(pane),
            None,
            None,
            Some(who),
            json!({"client": client, "mode": if control { "control" } else { "watch" }}),
        );
    }

    /// The ship's log says who typed into a pane: once when they start, and
    /// again after five quiet minutes. Never what they typed.
    pub fn note_input(&self, pane: &str, who: &str, client: &str) {
        let key = format!("{pane}\u{0}{client}");
        let now = now_ms();
        {
            let mut typed = self.typed.lock().unwrap();
            let last = typed.insert(key, now).unwrap_or(0);
            typed.retain(|_, t| now.saturating_sub(*t) < 3_600_000);
            if now.saturating_sub(last) < 300_000 {
                return;
            }
        }
        self.record(
            "pane.typed",
            Some(pane),
            None,
            None,
            Some(who),
            json!({"client": client}),
        );
    }

    // ---------------------------------------------------------------- presence

    pub fn touch_presence(&self, client: &str) {
        self.presence
            .lock()
            .unwrap()
            .insert(client.to_string(), now_ms());
    }

    /// Is the user at a keyboard on any full client right now?
    pub fn at_keyboard(&self) -> bool {
        let cutoff = now_ms().saturating_sub(self.cfg.at_keyboard_secs * 1000);
        self.presence.lock().unwrap().values().any(|t| *t >= cutoff)
    }

    // --------------------------------------------------------------- keep-awake

    fn update_keep_awake(&self) {
        if !self.cfg.keep_awake {
            return;
        }
        let working = self.cfg.stay_awake
            || self.list_panes().iter().any(|p| {
                let m = p.meta.lock().unwrap();
                m.exit.is_none() && m.state == AgentState::Working
            });
        let mut guard = self.keep_awake.lock().unwrap();
        if let Some(child) = guard.as_mut() {
            if matches!(child.try_wait(), Ok(Some(_))) {
                *guard = None;
            }
        }
        match (working, guard.is_some()) {
            (true, false) => {
                let cmd = self
                    .cfg
                    .keep_awake_cmd
                    .clone()
                    .unwrap_or_else(default_keep_awake_cmd);
                if let Some((prog, args)) = cmd.split_first() {
                    match std::process::Command::new(prog)
                        .args(args)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                    {
                        Ok(child) => {
                            self.emit("station.keep_awake", None, None, json!({"active": true}));
                            *guard = Some(child);
                        }
                        Err(e) => tracing::debug!(error = %e, "keep-awake command unavailable"),
                    }
                }
            }
            (false, true) => {
                if let Some(mut child) = guard.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                    self.emit("station.keep_awake", None, None, json!({"active": false}));
                }
            }
            _ => {}
        }
    }

    pub fn keep_awake_active(&self) -> bool {
        self.keep_awake.lock().unwrap().is_some()
    }

    // ------------------------------------------------------------ crash recovery

    /// After a crash or reboot: agents resume their conversation by session id.
    fn recover(&self) {
        let rows: Vec<(String, String, String, Option<i64>)> = self
            .db
            .with(|c| {
                let mut stmt =
                    c.prepare("SELECT id, meta, token, pid FROM panes WHERE closed IS NULL")?;
                let rows =
                    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                rows.collect()
            })
            .unwrap_or_default();
        for (id, meta, token, pid) in rows {
            // A child left over from a handoff that did not complete: its
            // terminal is lost, so end it before resuming the conversation.
            if let Some(pid) = pid.map(|p| p as i32).filter(|p| *p > 1) {
                if is_our_child(pid) {
                    let _ =
                        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), Signal::SIGKILL);
                    let _ = nix::sys::wait::waitpid(nix::unistd::Pid::from_raw(pid), None);
                }
            }
            let Ok(meta) = serde_json::from_str::<PaneMeta>(&meta) else {
                continue;
            };
            let resumable =
                meta.agent.is_agent() && meta.session_id.is_some() && meta.exit.is_none();
            if !resumable {
                let _ = self.db.with(|c| {
                    c.execute(
                        "UPDATE panes SET closed=?2 WHERE id=?1",
                        rusqlite::params![id, now_ms() as i64],
                    )
                });
                continue;
            }
            let opts = SpawnOpts {
                id: Some(id.clone()),
                token: Some(token),
                name: Some(meta.name.clone()),
                agent: Some(meta.agent),
                program: self.launcher_program(&meta),
                cwd: Some(PathBuf::from(&meta.cwd)),
                cols: Some(meta.spec.cols),
                rows: Some(meta.spec.rows),
                project: meta.project.clone(),
                role: meta.role.clone(),
                task: meta.task.clone(),
                parent_pane: meta.parent_pane.clone(),
                resume: self.resumable(&meta),
                slot: meta.slot.clone(),
                ..Default::default()
            };
            match self.spawn_pane(opts) {
                Ok(p) => {
                    self.record(
                        "pane.resumed",
                        Some(&p.id),
                        meta.project.as_deref(),
                        meta.task.as_deref(),
                        None,
                        json!({"session_id": meta.session_id, "why": "daemon restart"}),
                    );
                }
                Err(e) => {
                    tracing::warn!(pane = %id, error = %e, "could not resume pane");
                    let _ = self.db.with(|c| {
                        c.execute(
                            "UPDATE panes SET closed=?2 WHERE id=?1",
                            rusqlite::params![id, now_ms() as i64],
                        )
                    });
                }
            }
        }
    }

    /// The program a pane was launched with, when it was not the adapter's default
    /// (tests and custom launchers), so a resume uses the same one.
    fn launcher_program(&self, meta: &PaneMeta) -> Option<String> {
        let default = adapter(meta.agent)?.launch(&LaunchOpts::default()).program;
        (meta.spec.program != default).then(|| meta.spec.program.clone())
    }

    // ------------------------------------------------------------------ handoff

    /// After an upgrade whose health check failed: hand every terminal back
    /// to the previous binary, the same way they were handed to this one.
    /// Returns only if that could not be done.
    pub fn roll_back(&self, previous: &std::path::Path, reason: &str) -> std::io::Error {
        use std::os::unix::process::CommandExt;
        tracing::error!(reason, previous = %previous.display(), "upgrade failed its health check; going back");
        let state = self.prepare_handoff();
        let file = self.paths.handoff_file();
        if let Err(e) = write_private(&file, &serde_json::to_string(&state).unwrap_or_default()) {
            self.readopt(state);
            return e;
        }
        restore_db_before_upgrade(&self.paths);
        let err = std::process::Command::new(previous)
            .arg("daemon")
            .arg("--handoff")
            .arg(&file)
            .env_remove("SPANDECK_PREVIOUS_EXE")
            .env("SPANDECK_ROLLED_BACK", reason)
            .exec();
        let _ = std::fs::remove_file(&file);
        self.readopt(state);
        err
    }

    /// Said once by the binary that was gone back to.
    pub fn note_rolled_back(&self, reason: &str) {
        self.record(
            "station.rolled_back",
            None,
            None,
            None,
            None,
            json!({"reason": reason, "version": VERSION}),
        );
        self.notify(
            "station.rolled_back",
            None,
            None,
            &format!("The upgrade of {} was undone", self.station),
            &format!("The new version did not start properly ({reason}). This station is back on {VERSION} and every terminal was kept."),
        );
    }

    /// Describe every live pane for the next process image. Readers stop here.
    pub fn prepare_handoff(&self) -> HandoffState {
        let mut panes = vec![];
        for p in self.list_panes() {
            if p.meta.lock().unwrap().exit.is_some() {
                continue;
            }
            match p.term.prepare_handoff(2000) {
                Ok(pty) => panes.push(HandoffPane {
                    meta: p.meta(),
                    token: p.token.clone(),
                    pty,
                }),
                Err(e) => {
                    tracing::error!(pane = %p.id, error = %e, "pane could not be prepared for handoff")
                }
            }
        }
        HandoffState {
            version: 1,
            from_version: VERSION.to_string(),
            pid: std::process::id(),
            prev_exe: self.exe.clone(),
            panes,
        }
    }

    /// Adopt panes described by a previous image. Returns false when the state
    /// was unusable, and the caller falls back to resume by session id.
    fn adopt_handoff(&self, file: &std::path::Path) -> bool {
        let state: HandoffState = match std::fs::read(file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(s) => s,
            None => {
                tracing::error!("handoff state unreadable; resuming agents by session id");
                return false;
            }
        };
        let _ = std::fs::remove_file(file);
        if state.version != 1 || state.pid != std::process::id() {
            tracing::error!("handoff state is not for this process; resuming agents by session id");
            return false;
        }
        let total = state.panes.len();
        let mut adopted = 0;
        for hp in state.panes {
            let id = hp.meta.id.clone();
            let (tx, _) = broadcast::channel(1024);
            // SAFETY: the descriptor was left open across exec by prepare_handoff
            // in the previous image of this same process.
            match unsafe { Pane::adopt(&hp.pty, self.make_sink(&id, tx.clone())) } {
                Ok(term) => {
                    let entry = Arc::new(PaneEntry {
                        id: id.clone(),
                        token: hp.token,
                        term,
                        tx,
                        meta: Mutex::new(hp.meta),
                    });
                    self.panes.write().unwrap().insert(id, entry.clone());
                    self.save_pane(&entry);
                    adopted += 1;
                }
                Err(e) => tracing::error!(pane = %id, error = %e, "pane lost in handoff"),
            }
        }
        self.record(
            "station.upgraded",
            None,
            None,
            None,
            None,
            json!({"from": state.from_version, "to": VERSION, "panes": total, "adopted": adopted}),
        );
        if adopted < total {
            // Whatever did not survive resumes by session id.
            self.recover();
        }
        true
    }

    /// Put panes back into service after an exec that did not happen.
    pub fn readopt(&self, state: HandoffState) {
        for hp in state.panes {
            let id = hp.meta.id.clone();
            let (tx, _) = broadcast::channel(1024);
            // SAFETY: descriptors created by prepare_handoff in this process.
            if let Ok(term) = unsafe { Pane::adopt(&hp.pty, self.make_sink(&id, tx.clone())) } {
                let entry = Arc::new(PaneEntry {
                    id: id.clone(),
                    token: hp.token,
                    term,
                    tx,
                    meta: Mutex::new(hp.meta),
                });
                self.panes.write().unwrap().insert(id, entry);
            }
        }
    }

    pub fn info(&self) -> Value {
        let panes = self.list_panes();
        let count = |s: AgentState| {
            panes
                .iter()
                .filter(|p| {
                    let m = p.meta.lock().unwrap();
                    m.exit.is_none() && m.state == s
                })
                .count()
        };
        json!({
            "station": self.station,
            "version": VERSION,
            "protocol": PROTOCOL,
            "schema": self.db.schema_version(),
            "pid": std::process::id(),
            "started_ms": self.started_ms,
            "labels": self.cfg.labels,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "panes": panes.iter().filter(|p| p.meta.lock().unwrap().exit.is_none()).count(),
            "working": count(AgentState::Working),
            "needs_you": count(AgentState::NeedsYou),
            "keep_awake": self.keep_awake_active(),
            "at_keyboard": self.at_keyboard(),
            "web": self.web_addrs.lock().unwrap().clone(),
            "load": load_average(),
            "macs": crate::manage::mac_addresses(),
            "service": crate::doctor::service_installed(&self.home),
            "codex_sandbox_blocked": crate::doctor::codex_sandbox_blocked(&self.home),
            "setup": {"updated_ms": self.setup().updated_ms},
            "vault": self.vault_view(),
        })
    }
}

fn default_keep_awake_cmd() -> Vec<String> {
    let v: &[&str] = if cfg!(target_os = "macos") {
        &["caffeinate", "-i"]
    } else {
        &[
            "systemd-inhibit",
            "--what=sleep:idle",
            "--who=spandeck",
            "--why=agents are working",
            "sleep",
            "infinity",
        ]
    };
    v.iter().map(|s| s.to_string()).collect()
}

fn load_average() -> Option<f64> {
    let mut avg = [0f64; 1];
    // SAFETY: valid pointer to one f64.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 1) };
    (n == 1).then_some(avg[0])
}

fn is_our_child(pid: i32) -> bool {
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
    matches!(
        waitpid(nix::unistd::Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)),
        Ok(WaitStatus::StillAlive)
    )
}

pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+=:@%".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())
}

/// Put back the database saved before an upgrade that changed its schema, so
/// the previous binary can open it. Changes made since are lost.
pub fn restore_db_before_upgrade(paths: &Paths) {
    let saved = paths.db_before_upgrade();
    if !saved.exists() {
        return;
    }
    let db = paths.db();
    for ext in ["-wal", "-shm"] {
        let mut side = db.clone().into_os_string();
        side.push(ext);
        let _ = std::fs::remove_file(side);
    }
    if let Err(e) = std::fs::rename(&saved, &db) {
        tracing::error!(error = %e, "could not put back the database saved before the upgrade");
    }
}
