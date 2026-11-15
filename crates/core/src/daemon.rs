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
use spandeck_terminal::{
    sys, ExitInfo, Pane, PaneEvent, PaneHandoff, ScreenInfo, Signal, SpawnSpec,
};
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
    /// Sub-agents the agent itself has running (its own, not spandeck helpers).
    #[serde(default)]
    pub subagents: Vec<SubAgent>,
    /// The program the pane was asked to run instead of its agent's usual
    /// one (tests and custom launchers), so a restart uses it again. Only
    /// meaningful when `launcher_recorded`: older panes did not keep it.
    #[serde(default)]
    pub launcher: Option<String>,
    #[serde(default)]
    pub launcher_recorded: bool,
    /// The program was started into a conversation saved before (a restart,
    /// a recovery, or a new pane asked to continue the last one), not a new
    /// one.
    #[serde(default)]
    pub resumed: bool,
}

/// What `Daemon::last_session` found.
#[derive(Debug, Clone, Default)]
pub struct LastSession {
    /// The conversation a new pane would continue.
    pub session: Option<spandeck_agents::sessions::SavedSession>,
    /// A more recent one was passed over: a live pane here is in it.
    pub newer_in_use: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubAgent {
    /// What the agent calls it: "Explore", "general-purpose", and so on.
    pub kind: String,
    pub since_ms: u64,
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
            "subagents": m.subagents, "resumed": m.resumed,
            "last_output_ms": self.term.last_output_ms(), "pid": self.term.pid(),
            "program": m.spec.program,
            "icon": spandeck_agents::icon(m.agent, &m.spec.program),
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
    /// Continue the last conversation this agent saved for the folder, or
    /// start a new one when there is none. `resume` wins; a shell ignores it.
    pub resume_last: bool,
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
    /// The pane host this daemon's panes live in, where one is used.
    pub(crate) host: Mutex<Option<Arc<spandeck_terminal::HostClient>>>,
    pub(crate) seq: AtomicU64,
    pub fleet: crate::fleet::FleetState,
    /// The fleet key, and requests to be let in.
    pub trust: Arc<crate::trust::TrustState>,
    pub web_addrs: Mutex<Vec<String>>,
    pub vault: crate::vault::VaultState,
    /// Held while the deck document is read, changed and written back.
    pub(crate) deck_lock: Mutex<()>,
    /// The same for the look document.
    pub(crate) look_lock: Mutex<()>,
    /// The same for the owner's preferences.
    pub(crate) prefs_lock: Mutex<()>,
    /// Stamps seen in the fleet, and when this station last looked at it.
    pub(crate) sync: crate::sync::SyncState,
    /// Held while a pane that continues the last conversation is started, so
    /// two of them asked for at once do not both choose the same one.
    pub(crate) resume_lock: Mutex<()>,
    /// Agent programs: versions read, updates running, restarts owed.
    pub(crate) agents: crate::agents::AgentsState,
    pub(crate) prices: crate::prices::PricesState,
    /// A machine being added to the fleet from the Control Deck.
    pub(crate) adding: crate::add::AddState,
    /// Told when the owner asked for this station to be stopped (when it
    /// was removed from the fleet with "also stop spandeck there").
    pub quit: tokio::sync::Notify,
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
        // The state folder holds the tokens and the fleet key; the config
        // folder can hold credentials for MCP servers. Windows protects a
        // file through its folder, so both are made private there.
        let _ = sys::private_dir(&paths.state_dir);
        if sys::WINDOWS {
            let _ = sys::private_dir(&paths.config_dir);
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
            // What the image before this one called its program. One updated
            // by 0.4.1 or earlier may name a release unpacked under the state
            // directory, which is not what this station starts from.
            .or_else(|| {
                std::env::var_os("SPANDECK_BIN")
                    .map(|bin| paths.started_from(std::path::Path::new(&bin)))
            })
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .map(|me| paths.started_from(&me))
            })
            .unwrap_or_else(|| PathBuf::from("spandeck"));
        let home = crate::paths::home_dir();
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
            host: Mutex::new(None),
            seq: AtomicU64::new(0),
            web_addrs: Mutex::new(vec![]),
            vault: Default::default(),
            deck_lock: Mutex::new(()),
            look_lock: Mutex::new(()),
            prefs_lock: Mutex::new(()),
            sync: Default::default(),
            resume_lock: Mutex::new(()),
            agents: Default::default(),
            prices: Default::default(),
            adding: Default::default(),
            quit: tokio::sync::Notify::new(),
        }));
        daemon.prices_load();
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
            if std::fs::hard_link(&daemon.exe, &link).is_err() && sys::WINDOWS {
                // Windows will not let the installed file be written over
                // while it runs, so without a link a copy is what going back needs.
                let _ = std::fs::copy(&daemon.exe, &link);
            }
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
                        // Antigravity's hooks cover the turn but not its
                        // prompts, so there both are listened to.
                        if let Ok(p) = d.pane(&id) {
                            let (hooked, agent) = {
                                let m = p.meta.lock().unwrap();
                                (m.hooks_seen, m.agent)
                            };
                            if !hooked || agent == AgentKind::Antigravity {
                                let reason = (state == AgentState::NeedsYou)
                                    .then(|| "waiting for you".to_string());
                                d.set_state(&p, state, reason);
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
        self.pump_agent_restarts();
        // Agent versions for the fleet view: soon after start, then now and
        // again. Asking for `agents.status` reads them at once.
        if n % 1200 == 20 {
            self.agents_refresh();
        }
        // The price list: soon after start, then looked at every minute and
        // fetched when it is a day old.
        if n % 120 == 4 {
            self.prices_refresh();
        }
        if n.is_multiple_of(2) {
            let d = self.clone();
            let changed = tokio::task::spawn_blocking(move || {
                d.archive.poll();
                d.archive.poll_usage(now_ms())
            })
            .await;
            self.usage_changed(&changed.unwrap_or_default());
            self.pump_mail();
        }
        self.pump_relay(n).await;
        self.pump_recipes().await;
        if n.is_multiple_of(4) {
            self.pump_notifications().await;
        }
        if n % 20 == 1 {
            self.discover_peers().await;
            // The vault: the whole chain when the daemon starts, then the
            // address, the lock and the token every ten seconds.
            self.vault_check(n == 1).await;
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
            .map(|p| self.pane_view(p))
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
        let cwd = opts.cwd.clone().unwrap_or_else(|| self.home.clone());
        let _one_at_a_time = opts.resume_last.then(|| self.resume_lock.lock().unwrap());
        let mut resume = opts.resume.clone();
        let mut new_session = None;
        let mut agent_env = vec![];
        let (program, args) = match (adapter(agent), &opts.program) {
            (Some(a), _) => {
                let cfg = self
                    .cfg
                    .agents
                    .get(agent.as_str())
                    .cloned()
                    .unwrap_or_default();
                let (wiring, env) = self.integration(agent, &id, &token, opts.project.as_deref());
                agent_env = env;
                let mut extra = cfg.args.clone();
                extra.extend(wiring);
                extra.extend(opts.extra_args.iter().cloned());
                extra.extend(opts.args.iter().cloned());
                if resume.is_none() && opts.resume_last {
                    resume = self.last_session(agent, &cwd).session.map(|s| s.id);
                }
                if resume.is_none() {
                    // An agent that cannot report its session id is started
                    // under one chosen here, so it can be resumed later.
                    new_session = a.session_prefix().map(|prefix| {
                        format!(
                            "{prefix}{}",
                            ulid::Ulid::generate().to_string().to_lowercase()
                        )
                    });
                }
                let launch = a.launch(&LaunchOpts {
                    resume: resume.clone(),
                    prompt: opts.prompt.clone(),
                    extra_args: extra,
                    program: opts.program.clone().or(cfg.program),
                    session: new_session.clone(),
                });
                use crate::agents::Found;
                let found = self.program_for_start(agent, &launch.program);
                // What is true of it, for a pane that cannot be started.
                let what = match &found {
                    Found::Broken { path, problem } => format!(
                        "{} is installed on {} but does not start ({} said: {})",
                        agent.display(),
                        self.station,
                        path.display(),
                        problem
                    ),
                    _ => format!(
                        "{} is not installed on {} (there is no {} in the folders spandeck looks in)",
                        agent.display(),
                        self.station,
                        launch.program
                    ),
                };
                let broken = match &found {
                    Found::Broken { path, problem } => Some((path.as_path(), problem.as_str())),
                    _ => None,
                };
                match (&found, self.agent_install_line(agent)) {
                    (Found::Run(path), _) => (path.display().to_string(), launch.args),
                    // Nothing to reinstall it with: it is started as it is,
                    // and says for itself what is wrong.
                    (Found::Broken { path, .. }, None) => {
                        (path.display().to_string(), launch.args)
                    }
                    // Installing a program changes the station, and an
                    // agent never gets to do that: only a pane the owner
                    // asked for installs one.
                    (_, Some(_)) if opts.parent_pane.is_some() => {
                        return Err(invalid(format!(
                            "{what}. A pane an agent asks for does not install software: the owner starts a {} pane once, which installs it",
                            agent.display()
                        )))
                    }
                    // Not installed here, or installed and not working: the
                    // pane installs it first, where the user can watch, and
                    // then starts it.
                    (_, Some(install)) => self.install_then_launch(
                        agent,
                        &launch.program,
                        &install,
                        broken,
                        launch.args,
                    ),
                    (_, None) => {
                        return Err(invalid(format!(
                            "{what}, and spandeck knows no install command for it. Install it yourself, or set [agents.{}] program or install_cmd in config.toml",
                            agent.as_str()
                        )))
                    }
                }
            }
            (None, Some(p)) => (p.clone(), opts.args.clone()),
            (None, None) => {
                agent = AgentKind::Shell;
                sys::default_shell()
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

        if !cwd.is_dir() {
            return Err(invalid(format!("{} is not a directory", cwd.display())));
        }
        let mut env = opts.env.clone();
        env.extend(agent_env);
        let exe_dir = self.exe.parent().map(PathBuf::from).unwrap_or_default();
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
                crate::paths::path_with(&path, &[exe_dir]),
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
        // A name belongs to one live pane in its project (or among the panes
        // with no project). A name the caller chose is refused when taken; a
        // name nobody chose gets a number. A pane being brought back under
        // its own id (restart, recovery) is never refused: it gets a number.
        let project = opts.project.as_deref();
        let name = match &opts.name {
            Some(name) if opts.id.is_none() => {
                if name_taken(&panes, name, project, &id) {
                    return Err(name_in_use(name, project, &self.station));
                }
                name.clone()
            }
            chosen => {
                let base = chosen.clone().unwrap_or_else(|| {
                    if agent.is_agent() {
                        agent.as_str().to_string()
                    } else {
                        "shell".to_string()
                    }
                });
                let mut name = base.clone();
                let mut n = 1;
                while name_taken(&panes, &name, project, &id) {
                    n += 1;
                    name = format!("{base}-{n}");
                }
                name
            }
        };
        let sink = self.make_sink(&id, tx.clone());
        let term = match self.pane_host() {
            Some(host) => {
                let host = host.map_err(|e| invalid(format!("the pane host is not there: {e}")))?;
                Pane::spawn_on(&host, &id, &spec, sink)
            }
            None => Pane::spawn(&spec, sink),
        }
        .map_err(|e| invalid(format!("could not start {}: {e}", spec.program)))?;
        let now = now_ms();
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
            session_id: resume.clone().or(new_session),
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
            subagents: vec![],
            launcher: opts.program.clone(),
            launcher_recorded: true,
            resumed: resume.is_some(),
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
            json!({"name": m.name, "agent": m.agent, "resumed": resume.is_some(), "resume_last": opts.resume_last}),
        );
        Ok(entry)
    }

    /// Give a pane another name. Refused when a live pane in the same project
    /// (or, with no project, another pane with none) already has it.
    pub fn rename_pane(&self, id: &str, name: &str) -> Result<Arc<PaneEntry>> {
        let pane = self.pane(id)?;
        let name = name.trim();
        if name.is_empty() {
            return Err(invalid("a pane needs a name"));
        }
        {
            // Held alone across the check and the change, as when a pane is
            // started, so two renames cannot both find the name free.
            #[allow(clippy::readonly_write_lock)]
            let panes = self.panes.write().unwrap();
            let project = pane.meta.lock().unwrap().project.clone();
            if name_taken(&panes, name, project.as_deref(), &pane.id) {
                return Err(name_in_use(name, project.as_deref(), &self.station));
            }
            pane.meta.lock().unwrap().name = name.to_string();
        }
        self.save_pane(&pane);
        self.emit("pane.updated", Some(&pane.id), None, json!({}));
        Ok(pane)
    }

    /// Arguments and environment that wire an agent to this daemon at
    /// launch, so a pane spandeck starts works with no global install.
    fn integration(
        &self,
        agent: AgentKind,
        pane: &str,
        token: &str,
        project: Option<&str>,
    ) -> (Vec<String>, Vec<(String, String)>) {
        let bin = self.exe.display().to_string();
        let setup = self.setup();
        // The fleet-wide agent setup: model defaults and shared MCP servers.
        let mut args = setup.launch_args(agent);
        let mut env = vec![];
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
            AgentKind::OpenCode => {
                // OpenCode takes settings for one process from its
                // environment, and only a private server reads them: its
                // background service does not see this pane's environment.
                args.push("--standalone".to_string());
                env.push((
                    "OPENCODE_CONFIG_CONTENT".to_string(),
                    self.opencode_config(&setup, pane, token, project)
                        .to_string(),
                ));
            }
            AgentKind::Antigravity => {
                // Antigravity has no per-launch setting for MCP servers or
                // hooks. It loads plugins from its config folder, so spandeck
                // keeps one there, in a folder of its own.
                for problem in self.antigravity_plugin(&setup) {
                    tracing::warn!(%problem, "antigravity plugin");
                }
                // Report state to the terminal (OSC 7501) without asking it
                // first whether it understands.
                env.push(("AGY_CLI_OSC7501".to_string(), "on".to_string()));
            }
            _ => {}
        }
        (args, env)
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

    /// The last conversation `agent` saved for a folder, from the agent's own
    /// files, for a new pane that is to continue it. Never one a live pane
    /// on this station is in: the next most recent instead, or none.
    pub fn last_session(&self, agent: AgentKind, cwd: &std::path::Path) -> LastSession {
        use spandeck_agents::sessions::{self, SavedSession};
        let live: Vec<String> = self
            .list_panes()
            .iter()
            .filter_map(|p| {
                let m = p.meta.lock().unwrap();
                m.exit.is_none().then(|| m.session_id.clone()).flatten()
            })
            .collect();
        let passed = std::cell::Cell::new(false);
        let taken = |id: &str| {
            let held = live.iter().any(|l| l == id);
            passed.set(passed.get() || held);
            held
        };
        let folder = sessions::Folder::of(cwd);
        let session = match agent {
            // The test agent keeps Claude Code's layout.
            AgentKind::Claude | AgentKind::Fake => {
                sessions::claude_last(&sessions::claude_projects_dir(&self.home), &folder, &taken)
            }
            AgentKind::Codex => {
                sessions::codex_last(&sessions::codex_sessions_dir(&self.home), &folder, &taken)
            }
            AgentKind::OpenCode => spandeck_archive::usage::opencode_last(
                &spandeck_archive::usage::opencode_db(&self.home),
                &|dir| folder.is(dir),
                &taken,
            )
            .map(|(id, last_used_ms)| SavedSession {
                id,
                path: None,
                last_used_ms,
            }),
            AgentKind::Antigravity | AgentKind::Shell => None,
        };
        LastSession {
            session,
            newer_in_use: passed.get(),
        }
    }

    /// Start a pane's program again in the same pane: same name, folder,
    /// project and role. An agent goes back to its conversation unless
    /// `fresh` is set or there is none to go back to.
    pub fn restart_pane(&self, id: &str, fresh: bool, by: &str) -> Result<Arc<PaneEntry>> {
        let old = self.pane(id)?;
        let meta = old.meta();
        let resume = if fresh { None } else { self.resumable(&meta) };
        if meta.exit.is_none() {
            let _ = old.term.signal(Signal::Hangup);
            let _ = old.term.signal(Signal::Kill);
            // The old program's exit must be seen before the pane is reused,
            // or it would be taken for the new one's.
            let start = std::time::Instant::now();
            while old.meta().exit.is_none() && start.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        self.cancel_pane_approvals(&old.id);
        // The new program takes the old one's place, under the same id.
        old.term.forget();
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
        self.keep_owner_marks(&p, &meta);
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
            let _ = p.term.signal(Signal::Hangup);
            let p2 = p.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                if p2.term.exit_info().is_none() {
                    let _ = p2.term.signal(Signal::Kill);
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
            let _ = p.term.signal(Signal::Hangup);
            let _ = p.term.signal(Signal::Kill);
            // Keep the pane until its program has been collected. Let go of
            // at once, its reader stops first, and the ended program stays
            // in the process table for as long as the daemon runs.
            let held = p.clone();
            std::thread::spawn(move || {
                for _ in 0..150 {
                    if held.term.exit_info().is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
        }
        p.term.forget();
        self.panes.write().unwrap().remove(&p.id);
        let _ = self.db.with(|c| {
            c.execute(
                "UPDATE panes SET closed=?2 WHERE id=?1 AND closed IS NULL",
                rusqlite::params![p.id, now_ms() as i64],
            )
        });
        self.cancel_pane_approvals(&p.id);
        self.look_forget_pane(&p.id, by);
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
                    .unwrap_or_else(|| sys::keep_awake_cmd(&self.exe));
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

    /// Is this station's pane host in use? Always on Windows; on Unix only
    /// when `SPANDECK_PANE_HOST=1` asks for it (panes are otherwise in the
    /// daemon's own process, kept through an upgrade by exec in place).
    fn uses_pane_host() -> bool {
        sys::PANE_HOST || std::env::var("SPANDECK_PANE_HOST").is_ok_and(|v| v == "1")
    }

    /// The pane host, started if it is not running. `None`: this station
    /// keeps its panes in the daemon's own process.
    pub(crate) fn pane_host(&self) -> Option<std::io::Result<Arc<spandeck_terminal::HostClient>>> {
        if !Self::uses_pane_host() {
            return None;
        }
        let mut held = self.host.lock().unwrap();
        if let Some(host) = held.as_ref().filter(|h| h.alive()) {
            return Some(Ok(host.clone()));
        }
        let pipe = self.paths.host_socket();
        let program = self.exe.with_file_name(format!(
            "spandeck-pane-host{}",
            std::env::consts::EXE_SUFFIX
        ));
        let connect = || spandeck_terminal::host::connect_or_start(&pipe, &program);
        let host = connect().and_then(|host| {
            // A host from an older spandeck with no pane in it is replaced,
            // so the host is renewed whenever that costs nothing. One with
            // panes stays: the protocol only grows, and this daemon speaks
            // every earlier one.
            let idle = host.list().map(|l| l.is_empty()).unwrap_or(false);
            if host.info.version != VERSION && idle {
                let _ = host.shutdown();
                std::thread::sleep(Duration::from_millis(400));
                return connect();
            }
            Ok(host)
        });
        match host {
            Ok(host) => {
                tracing::info!(pid = host.info.pid, version = %host.info.version, protocol = host.info.protocol, "pane host");
                *held = Some(host.clone());
                Some(Ok(host))
            }
            Err(e) => Some(Err(e)),
        }
    }

    /// The station is stopping for good (not being replaced): end the panes
    /// the pane host holds, and the host.
    pub fn stop_pane_host(&self) {
        if let Some(host) = self.host.lock().unwrap().take() {
            let _ = host.shutdown();
        }
    }

    /// What the Control Deck is told about the pane host.
    fn pane_host_info(&self) -> Value {
        match self.host.lock().unwrap().as_ref() {
            Some(h) => json!({
                "version": h.info.version, "protocol": h.info.protocol, "pid": h.info.pid,
                // Older than this daemon: renewed by itself once it holds no pane.
                "outdated": h.info.version != VERSION,
            }),
            None => Value::Null,
        }
    }

    /// Panes the pane host still holds after this daemon's predecessor went
    /// without a handoff (it crashed, or was ended). Each is taken over with
    /// its screen rebuilt from the output the host kept. Returns their ids.
    fn adopt_from_host(&self, rows: &[(String, String, String, Option<i64>)]) -> Vec<String> {
        let Some(Ok(host)) = self.pane_host() else {
            return vec![];
        };
        let held = host.list().unwrap_or_default();
        let mut adopted = vec![];
        for h in &held {
            let row = rows.iter().find(|r| r.0 == h.id);
            let meta = row.and_then(|r| serde_json::from_str::<PaneMeta>(&r.1).ok());
            let (Some(row), Some(meta), None) = (row, meta, h.exit) else {
                // Not a pane this station knows as open, or its program has
                // ended: nothing to go back to.
                if !self.panes.read().unwrap().contains_key(&h.id) {
                    let _ = host.forget(&h.id);
                }
                continue;
            };
            if self.panes.read().unwrap().contains_key(&h.id) {
                continue;
            }
            let (tx, _) = broadcast::channel(1024);
            match Pane::adopt_on(&host, &h.id, None, self.make_sink(&h.id, tx.clone())) {
                Ok(term) => {
                    let entry = Arc::new(PaneEntry {
                        id: h.id.clone(),
                        token: row.2.clone(),
                        term,
                        tx,
                        meta: Mutex::new(meta.clone()),
                    });
                    self.panes.write().unwrap().insert(h.id.clone(), entry);
                    self.record(
                        "pane.kept",
                        Some(&h.id),
                        meta.project.as_deref(),
                        meta.task.as_deref(),
                        None,
                        json!({"why": "daemon restart", "pid": h.pid}),
                    );
                    adopted.push(h.id.clone());
                }
                Err(e) => {
                    tracing::warn!(pane = %h.id, error = %e, "a pane in the pane host could not be taken over");
                    let _ = host.forget(&h.id);
                }
            }
        }
        adopted
    }

    /// After a crash or reboot: agents resume their conversation by session
    /// id. Where a pane host is used, panes it still holds are simply taken
    /// over as they are.
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
        let kept = self.adopt_from_host(&rows);
        for (id, meta, token, pid) in rows {
            // Already in service: kept by the pane host, or adopted from a handoff.
            if kept.contains(&id) || self.panes.read().unwrap().contains_key(&id) {
                continue;
            }
            // A child left over from a handoff that did not complete: its
            // terminal is lost, so end it before resuming the conversation.
            if let Some(pid) = pid.map(|p| p as i32).filter(|p| *p > 1) {
                sys::end_leftover_child(pid);
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
                    self.keep_owner_marks(&p, &meta);
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

    /// A pane started again under its old id keeps what the owner set on
    /// it: paused, muted, watched. A paused pane that came back taking work
    /// would break the Pause guarantee, and did after a daemon crash
    /// (found on 15 Nov 2026).
    fn keep_owner_marks(&self, p: &Arc<PaneEntry>, old: &PaneMeta) {
        if !(old.paused || old.muted || old.watch) {
            return;
        }
        {
            let mut m = p.meta.lock().unwrap();
            m.paused = old.paused;
            m.muted = old.muted;
            m.watch = old.watch;
        }
        self.save_pane(p);
    }

    /// The program a pane was launched with, when it was not the adapter's default
    /// (tests and custom launchers), so a resume uses the same one.
    fn launcher_program(&self, meta: &PaneMeta) -> Option<String> {
        if meta.launcher_recorded {
            return meta.launcher.clone();
        }
        // A pane from before the launcher was kept: work it out.
        let default = adapter(meta.agent)?.launch(&LaunchOpts::default()).program;
        (meta.spec.program != default).then(|| meta.spec.program.clone())
    }

    // ------------------------------------------------------------------ handoff

    /// After an upgrade whose health check failed: hand every terminal back
    /// to the previous binary, the same way they were handed to this one.
    /// Returns only if that could not be done.
    pub fn roll_back(&self, previous: &std::path::Path, reason: &str) -> std::io::Error {
        tracing::error!(reason, previous = %previous.display(), "upgrade failed its health check; going back");
        let state = self.prepare_handoff();
        let file = self.paths.handoff_file();
        if let Err(e) = write_private(&file, &serde_json::to_string(&state).unwrap_or_default()) {
            self.readopt(state);
            return e;
        }
        restore_db_before_upgrade(&self.paths);
        let err = sys::replace_process(
            std::process::Command::new(previous)
                .arg("daemon")
                .arg("--handoff")
                .arg(&file)
                .env_remove("SPANDECK_PREVIOUS_EXE")
                .env("SPANDECK_ROLLED_BACK", reason),
        );
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
        // Unix: the same process (exec in place). Windows: a new process,
        // and the panes are in the pane host.
        if state.version != 1 || (sys::UPGRADE_KEEPS_PID && state.pid != std::process::id()) {
            tracing::error!("handoff state is not for this process; resuming agents by session id");
            return false;
        }
        let total = state.panes.len();
        let mut adopted = 0;
        for hp in state.panes {
            let id = hp.meta.id.clone();
            let (tx, _) = broadcast::channel(1024);
            match self.adopt_pane(&id, &hp.pty, tx.clone()) {
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

    /// Take over one pane a previous image described: from the pane host, or
    /// by the descriptor it left open.
    fn adopt_pane(
        &self,
        id: &str,
        h: &PaneHandoff,
        tx: broadcast::Sender<PaneEvent>,
    ) -> std::io::Result<Pane> {
        let sink = self.make_sink(id, tx);
        match &h.host_id {
            Some(host_id) => {
                let host = self.pane_host().unwrap_or_else(|| {
                    Err(std::io::Error::other("this station uses no pane host"))
                })?;
                Pane::adopt_on(&host, host_id, Some(h), sink)
            }
            // SAFETY: the descriptor was left open across exec by
            // prepare_handoff in a previous image of this same process.
            None => unsafe { Pane::adopt(h, sink) },
        }
    }

    /// Put panes back into service after an exec that did not happen.
    pub fn readopt(&self, state: HandoffState) {
        for hp in state.panes {
            let id = hp.meta.id.clone();
            let (tx, _) = broadcast::channel(1024);
            if let Ok(term) = self.adopt_pane(&id, &hp.pty, tx.clone()) {
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
            // Every terminal is kept through an upgrade or a restart: on Unix
            // by replacing the program in place, on Windows by the pane host.
            "live_handoff": true,
            // It lists requests that wait at other stations (`trust.relay`).
            "trust_relay": true,
            "pane_host": self.pane_host_info(),
            "arch": std::env::consts::ARCH,
            "panes": panes.iter().filter(|p| p.meta.lock().unwrap().exit.is_none()).count(),
            "working": count(AgentState::Working),
            "needs_you": count(AgentState::NeedsYou),
            "keep_awake": self.keep_awake_active(),
            "at_keyboard": self.at_keyboard(),
            "web": self.web_addrs.lock().unwrap().clone(),
            "load": load_average(),
            "macs": crate::manage::mac_addresses(),
            // The fleet's members as this station knows them, and the ones
            // the owner said to forget: every station tells the others, so
            // the list is the same wherever the deck is opened.
            "roster": self.roster(),
            // Where spandeck is on this machine: said before the owner
            // removes a station, since those stay on its disk.
            "dirs": {
                "program": self.exe.display().to_string(),
                "state": self.paths.state_dir.display().to_string(),
                "config": self.paths.config_dir.display().to_string(),
            },
            "service": crate::doctor::service_installed(&self.home),
            "codex_sandbox_blocked": crate::doctor::codex_sandbox_blocked(&self.home),
            "agents": self.agents_brief(),
            "setup": {"updated_ms": self.setup().updated_ms},
            "deck": {"updated_ms": self.deck().updated_ms},
            // Said only by a station that keeps the look document: one that
            // does not say it is never sent a copy.
            "look": {"updated_ms": self.look().updated_ms, "format": crate::look::FORMAT},
            // The same for the owner's preferences.
            "prefs": {"updated_ms": self.prefs().updated_ms, "format": crate::prefs::FORMAT},
            "vault": self.vault_view(),
        })
    }
}

/// Does a live pane other than `except` have this name in this project (or,
/// with no project, among the panes that have none)?
fn name_taken(
    panes: &BTreeMap<String, Arc<PaneEntry>>,
    name: &str,
    project: Option<&str>,
    except: &str,
) -> bool {
    panes.values().any(|p| {
        let m = p.meta.lock().unwrap();
        m.id != except && m.exit.is_none() && m.name == name && m.project.as_deref() == project
    })
}

fn name_in_use(name: &str, project: Option<&str>, station: &str) -> Error {
    let place = match project {
        Some(project) => format!("in project {project}"),
        None => "among the panes with no project".to_string(),
    };
    invalid(format!(
        "a pane called {name:?} is already running {place} on {station}; choose another name"
    ))
}

fn load_average() -> Option<f64> {
    sys::load_average()
}

/// A command and its arguments as one line for the shell that
/// `sys::shell_line` runs: `sh` on Unix, PowerShell on Windows.
pub fn shell_join(argv: &[String]) -> String {
    if sys::WINDOWS {
        // `&` runs a quoted name; a quote inside single quotes is doubled.
        let quoted: Vec<String> = argv
            .iter()
            .map(|a| format!("'{}'", a.replace('\'', "''")))
            .collect();
        format!("& {}", quoted.join(" "))
    } else {
        argv.iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    }
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
    sys::write_private(path, contents.as_bytes())
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
