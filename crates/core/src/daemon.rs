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
    sys, ExitInfo, InputKind, Item, Mark, Pane, PaneEvent, PaneHandoff, ScreenInfo, Signal,
    SpawnSpec,
};
use tokio::sync::{broadcast, mpsc};

use crate::db::Db;
use crate::paths::{new_id, now_ms, random_token, Config, Paths};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Bumped when stations can no longer talk to an older peer.
pub const PROTOCOL: u32 = 2;

/// Enter follows a paste, and the mail nudge, this long after the text:
/// agents that detect pastes need the Enter as a write of its own, a
/// moment later.
pub const PASTE_ENTER: Duration = Duration::from_millis(60);

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
    /// A stop that could not reach its program: it is not reading input.
    #[error("{0}")]
    NotReading(String),
    /// A second try of a change, at a station that cannot tell whether
    /// the first was carried out (rids.rs). Nothing was done now.
    #[error("{0}")]
    OutcomeUnknown(String),
    /// A request id used again for a different request.
    #[error("{0}")]
    RidReused(String),
    /// The error first given to a request, given again to its second try.
    #[error("{message}")]
    Replayed { code: &'static str, message: String },
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
            Error::NotReading(_) => "not_reading",
            Error::OutcomeUnknown(_) => "outcome_unknown",
            Error::RidReused(_) => "rid_reused",
            Error::Replayed { code, .. } => code,
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
    /// When this pane was sent its role's prompt (roledoc.rs). 0: never.
    /// Kept through a restart into the same conversation, so a pane is
    /// sent its role once.
    #[serde(default)]
    pub role_sent_ms: u64,
    pub agent: AgentKind,
    pub spec: SpawnSpec,
    pub cwd: String,
    pub created_ms: u64,
    pub state: AgentState,
    pub state_since_ms: u64,
    /// When the pane last went from not busy to working or waiting for
    /// the owner: what a stop's cut-off is compared with for an agent that
    /// has no prompt hook (stops.rs). 0 in a record from before it was kept.
    #[serde(default)]
    pub busy_since_ms: u64,
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
    /// Those of them its hooks said were over, with the program's own id
    /// (subagents.rs): the newest few, each for the session it was in.
    #[serde(default)]
    pub subagents_over: Vec<crate::subagents::SubOver>,
    /// How hard the model is asked to think, as the agent's hooks last
    /// said. `None`: it has not said.
    #[serde(default)]
    pub effort: Option<String>,
    /// When the agent's hooks last said its effort.
    #[serde(default)]
    pub effort_at_ms: u64,
    /// What the program last printed when a command set its model, and
    /// its effort, kept with the pane so that it is still known when the
    /// program is started again (conversation.rs, `model_now`).
    #[serde(default)]
    pub model_said: Option<crate::conversation::Said>,
    #[serde(default)]
    pub effort_said: Option<crate::conversation::Said>,
    /// When, and where in its conversation, the pane was started again on
    /// `model_args` at the owner's choice.
    #[serde(default)]
    pub model_chosen: Option<crate::conversation::Said>,
    /// A question of the program's own on its screen that spandeck knows
    /// ("Switch model?": modelswitch.rs). `None` when there is none.
    #[serde(default)]
    pub prompt: Option<crate::modelswitch::Prompt>,
    /// The flags the pane's program is started with for its model and
    /// effort, when the owner chose them for this pane alone
    /// (`pane.model` with `only_pane`): kept, so a later restart starts it
    /// the same way.
    #[serde(default)]
    pub model_args: Vec<String>,
    /// The program was started to review for itself what it would
    /// otherwise ask the owner (Codex's `--approve-for-me`): its requests
    /// for permission are its own to answer, not the owner's (hooks.rs).
    #[serde(default)]
    pub own_review: bool,
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
    /// When the pane's program was started, by this station's clock. Kept
    /// beside the pid so that, after the daemon ended without stopping its
    /// panes, what the program left running is told from an unrelated
    /// process that was given the same pid since. `None`: a pane from
    /// before this was kept, whose leftovers are never touched.
    #[serde(default)]
    pub proc_started_ms: Option<u64>,
    /// Why the pane is listed though it has no program: `restart` when it
    /// ended with a daemon that went away and could not be started again.
    #[serde(default)]
    pub ended_by: Option<String>,
    #[serde(default)]
    pub ended_ms: u64,
    /// Listed and not started again: the station restarted three times in
    /// ten minutes. Its record stays open, so a later start tries again.
    #[serde(default)]
    pub held_back: bool,
    /// Stopped, and staying stopped: no mail reaches it in any form until
    /// the owner releases it (holds.rs). Apart from `paused`, and from
    /// `held_restored`: clearing one never clears another.
    #[serde(default)]
    pub held_stopped: Option<crate::holds::HeldStopped>,
    /// Started again after the station restarted, and waiting for the
    /// owner before it takes mail (holds.rs).
    #[serde(default)]
    pub held_restored: Option<crate::holds::HeldRestored>,
    /// The owner's standing choice this pane was started with: if its
    /// program asks whether to trust the folder, the answer is yes
    /// (foldertrust.rs). Kept through a restart of the program.
    #[serde(default)]
    pub trust_choice: Option<crate::foldertrust::Choice>,
    /// The program's folder-trust question was answered, by whom and how.
    #[serde(default)]
    pub trust_answered: Option<crate::foldertrust::Answered>,
    /// The question is on the screen now and nothing was pressed.
    #[serde(default)]
    pub trust_asks: Option<crate::foldertrust::Asks>,
    /// The owner's standing choice this pane was started with: if its
    /// program asks to review hooks and they are all spandeck's own, they
    /// are trusted (hookstrust.rs). Kept through a restart of the program.
    #[serde(default)]
    pub hooks_trust_choice: Option<crate::startscreen::Choice>,
    /// The program's review of hooks was answered, by whom and how.
    #[serde(default)]
    pub hooks_trust_answered: Option<crate::startscreen::Answered>,
    /// The review is on the screen now and nothing was pressed.
    #[serde(default)]
    pub hooks_trust_asks: Option<crate::startscreen::Asks>,
    /// What of stuck detection the owner said is fine for this pane
    /// (stuck.rs): quiet until its session changes or a file is changed.
    #[serde(default)]
    pub stuck_fine: crate::stuck::Fine,
}

/// What came of stopping one pane.
#[derive(Debug, Clone, Copy)]
pub struct Stopped {
    /// The terminal took the interrupt key. False: the program is not
    /// reading; it was not stopped, and is never said to be.
    pub delivered: bool,
    /// Things sent to the pane before the stop that were thrown away.
    pub discarded: usize,
    /// The pane's record, with its mark, is written.
    pub saved: bool,
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
    /// The program's own id for it, when its hooks give one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

pub struct PaneEntry {
    pub id: String,
    pub token: String,
    pub term: Pane,
    pub tx: broadcast::Sender<PaneEvent>,
    pub meta: Mutex<PaneMeta>,
    /// Whose turn it is to look at this pane's mail (wake.rs).
    pub wake: crate::wake::PaneWake,
    /// "Needs you" as its screen said it (`detect_screens`): the state the
    /// pane had before, and the reason given, so that it is taken back
    /// when the screen no longer shows it and nothing else has spoken.
    pub screen_needs: Mutex<Option<(AgentState, String)>>,
    /// What was sent to this pane and what it printed after each
    /// (screentalk.rs): its Chat face when it has no transcript.
    pub talk: Mutex<crate::screentalk::Talk>,
    /// The clients that have this pane's terminal open to size it (attached
    /// in control), and how many attachments each has. A phone asks before
    /// it fits a pane to itself: it takes the size from nobody.
    pub holders: Mutex<std::collections::BTreeMap<String, u32>>,
}

/// One attachment in control, for as long as it is kept.
pub struct Holding {
    pane: Arc<PaneEntry>,
    client: String,
}

impl Drop for Holding {
    fn drop(&mut self) {
        let mut h = self.pane.holders.lock().unwrap();
        if let Some(n) = h.get_mut(&self.client) {
            *n -= 1;
            if *n == 0 {
                h.remove(&self.client);
            }
        }
    }
}

impl PaneMeta {
    /// The pane's state, with "needs you" for what its program shows on
    /// its screen (picker.rs) read as the state that is under it: a pane
    /// that waits at a list was not in a turn.
    pub(crate) fn state_before_prompt(&self) -> AgentState {
        match (&self.prompt, self.state) {
            (Some(_), AgentState::NeedsYou) => AgentState::Idle,
            (_, state) => state,
        }
    }
}

impl PaneEntry {
    /// `client` has this pane's terminal open in control until what is
    /// returned is dropped.
    pub fn hold(self: &Arc<Self>, client: &str) -> Holding {
        *self
            .holders
            .lock()
            .unwrap()
            .entry(client.to_string())
            .or_insert(0) += 1;
        Holding {
            pane: self.clone(),
            client: client.to_string(),
        }
    }

    pub fn meta(&self) -> PaneMeta {
        self.meta.lock().unwrap().clone()
    }

    pub fn view(&self, station: &str) -> Value {
        let m = self.meta();
        let info = self.term.info();
        let mut view = json!({
            "id": m.id, "station": station, "address": format!("{}@{}", m.id, station),
            "name": m.name, "project": m.project, "role": m.role,
            "agent": m.agent, "agent_name": m.agent.display(), "cwd": m.cwd,
            "created_ms": m.created_ms, "state": m.state, "state_since_ms": m.state_since_ms,
            "needs_reason": m.needs_reason, "tool": m.tool, "tool_since_ms": m.tool_since_ms,
            "session_id": m.session_id, "title": if m.title.is_empty() { info.title.clone() } else { m.title.clone() },
            "exit": m.exit, "exited": m.exit.is_some(), "paused": m.paused, "task": m.task,
            "parent_pane": m.parent_pane, "hooks_seen": m.hooks_seen, "muted": m.muted,
            "prompt": m.prompt,
            "watch": m.watch, "slot": m.slot, "last_message": m.last_message,
            "cols": info.cols, "rows": info.rows, "controller": m.controller,
            "holders": self.holders.lock().unwrap().keys().collect::<Vec<_>>(),
            "subagents": m.subagents, "resumed": m.resumed,
            "ended_by": m.ended_by, "ended_ms": m.ended_ms, "held_back": m.held_back,
            "last_output_ms": self.term.last_output_ms(), "pid": self.term.pid(),
            "program": m.spec.program,
            "icon": spandeck_agents::icon(m.agent, &m.spec.program),
        });
        // Said to be stuck, and by what: a deck shows a notice the owner
        // can dismiss, not a prompt in the terminal (stuck.rs).
        view["stuck"] = crate::stuck::view(&m);
        view
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
    /// When the pane this one takes the place of was sent its role: set
    /// by whoever restarts a pane into its conversation, never by a caller.
    #[serde(skip)]
    pub role_sent_ms: u64,
    pub task: Option<String>,
    pub parent_pane: Option<String>,
    pub resume: Option<String>,
    /// Continue the last conversation this agent saved for the folder, or
    /// start a new one when there is none. `resume` wins; a shell ignores it.
    pub resume_last: bool,
    pub prompt: Option<String>,
    pub slot: Option<String>,
    pub extra_args: Vec<String>,
    /// The program's own flags for a model and an effort chosen for this
    /// pane alone (kept on the pane's record).
    pub model_args: Vec<String>,
    /// The New pane dialog's tick, "Trust this folder for <agent> if it
    /// asks", as the caller sent it. Read by `pane.spawn` for the owner
    /// only, and by nothing else (foldertrust.rs).
    pub trust_folder: Option<bool>,
    /// The standing choice the pane starts with. Never read from a
    /// caller's parameters: whoever starts the pane works it out.
    #[serde(skip)]
    pub trust_choice: Option<crate::foldertrust::Choice>,
    /// The New pane dialog's tick, "Trust spandeck's own hooks in <agent>
    /// if it asks", as the caller sent it. Read by `pane.spawn` for the
    /// owner only, and by nothing else (hookstrust.rs).
    pub trust_hooks: Option<bool>,
    /// The choice the pane starts with. Never read from a caller's
    /// parameters: whoever starts the pane works it out.
    #[serde(skip)]
    pub hooks_trust_choice: Option<crate::startscreen::Choice>,
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
    Osc7501(String, AgentState, Option<String>),
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
    /// The stops this station issued, and the ones it applied (stops.rs).
    pub stops: crate::stops::StopsState,
    /// The binary panes call back into (`spandeck hook`, `spandeck mcp`).
    pub exe: PathBuf,
    pub home: PathBuf,
    pub(crate) msg_tx: mpsc::UnboundedSender<PaneMsg>,
    /// What agents asked the owner, and the hooks waiting for an answer.
    pub(crate) approvals: crate::approvals::ApprovalBook,
    /// client id -> last input time: who is "at a keyboard".
    pub(crate) presence: Mutex<HashMap<String, u64>>,
    /// When each client last had its typing into a pane written to the log.
    pub(crate) typed: Mutex<HashMap<String, u64>>,
    /// Panes that may still be at their program's folder-trust question.
    pub(crate) folder_trust: crate::foldertrust::State,
    /// Panes that may still be at their program's review of hooks.
    pub(crate) hooks_trust: crate::hookstrust::State,
    pub(crate) keep_awake: Mutex<Option<std::process::Child>>,
    /// pane -> (session, line): how far the owner has read each pane's
    /// conversation. Kept in the key-value table; this is its copy in memory.
    pub(crate) read_marks: Mutex<HashMap<String, (String, u64)>>,
    /// pane -> the session found for it among its program's own files
    /// (discover.rs), and when it was last looked for.
    pub(crate) finds: Mutex<crate::discover::Finds>,
    /// Set while this image is being replaced: no helper is started then.
    pub(crate) keep_awake_held: std::sync::atomic::AtomicBool,
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
    /// What panes say at a glance, and its sampling (glance.rs).
    pub(crate) glance: crate::glance::GlanceState,
    /// Tools: the jobs that install them here (tools.rs).
    pub(crate) tools: crate::tools::ToolsState,
    /// The role library (roledoc.rs).
    pub(crate) roles: crate::roledoc::RolesState,
    pub(crate) prices: crate::prices::PricesState,
    /// What the release host last said of the newest spandeck (newrelease.rs).
    pub(crate) newrelease: Arc<crate::newrelease::State>,
    /// A machine being added to the fleet from the Control Deck.
    pub(crate) adding: crate::add::AddState,
    /// A release being supplied to, or taken from, another station.
    pub(crate) held: Arc<crate::supply_fleet::Held>,
    /// Links: stations reached over the owner's own ssh (link/mod.rs).
    pub(crate) link: crate::link::LinkState,
    /// Told when the owner asked for this station to be stopped (when it
    /// was removed from the fleet with "also stop spandeck there").
    pub quit: tokio::sync::Notify,
    /// Counters for the Performance page. Measured, never acted on.
    pub perf: Arc<crate::perf::Perf>,
    /// What the local Tailscale client last said of key expiry.
    pub tailnet: crate::tailnet::TailnetState,
    /// The HTTPS certificate, and how things stand with it (https.rs).
    pub https: Arc<crate::https::HttpsState>,
    /// The switches later steps read (`[flags]` in the config, and what the
    /// owner set with `station.flags`), and what the last start found.
    pub(crate) restore: crate::restore::RestoreState,
    /// The owner has been told that this station cannot write its database.
    pub(crate) db_told: std::sync::atomic::AtomicBool,
    /// The schedule of rounds with the other stations (rounds.rs).
    pub rounds: Arc<crate::rounds::Rounds>,
    /// The senders of messages for other stations, one per station.
    pub relay: crate::relay::Relay,
    /// Requests that carry an id: done once, and answered again (rids.rs).
    pub rids: Arc<crate::rids::Rids>,
    /// The file explorer: uploads in pieces, copies, zips (files.rs).
    pub(crate) files: crate::files::FilesState,
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
    /// Requests to join or sign in that were waiting for the owner. They
    /// hold the hash of what the asker collects with, never the key. A
    /// daemon from before this neither writes nor reads them.
    #[serde(default)]
    pub trust_requests: Vec<Value>,
    /// What the database had not taken when the image was replaced (it was
    /// locked, or the disk was full): pane records, stops, answers to
    /// approvals, entries of the ship's log. The next image writes them. A
    /// daemon from before this neither writes nor reads them.
    #[serde(default)]
    pub unwritten: crate::db::Unwritten,
    /// For how long this image kept request ids (rids.rs), when it did:
    /// with `unwritten`, its records of what was done are the next
    /// image's. A daemon from before this neither writes nor reads it, so
    /// an image that takes over from one starts counting from nothing.
    #[serde(default)]
    pub rids_kept_ms: Option<u64>,
}

/// A running pane, as a name is looked up among them.
pub(crate) struct Named {
    pub id: String,
    pub name: String,
    pub project: Option<String>,
}

/// Which running pane a name means: its place in `panes`, `None` when no
/// pane has the name, or, when several have it, the full address of each
/// to choose from (`project/name@station`).
///
/// `own` is the project of the pane that asks (`None`: the owner outside
/// every pane, or a pane of no project). A bare name is first a pane of
/// that project: every project has a `builder` and a `critic`, and an
/// agent that writes to `critic` means its own. Only when its project has
/// none is the name looked for on the whole station, as `name` or as
/// `project/name`.
pub(crate) fn by_name(
    panes: &[Named],
    name: &str,
    own: Option<&str>,
    station: &str,
) -> std::result::Result<Option<usize>, Vec<String>> {
    let pick = |found: Vec<usize>| match found[..] {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        _ => {
            let address = |p: &Named| match &p.project {
                Some(project) => format!("{project}/{}@{station}", p.name),
                None => format!("{}@{station}", p.name),
            };
            let mut all: Vec<String> = found
                .iter()
                .map(|i| {
                    let mine = address(&panes[*i]);
                    let shared = found.iter().any(|j| j != i && address(&panes[*j]) == mine);
                    // Two panes one address would name: each by its id.
                    if shared {
                        format!("{}@{station}", panes[*i].id)
                    } else {
                        mine
                    }
                })
                .collect();
            all.sort();
            Err(all)
        }
    };
    let among = |keep: &dyn Fn(&Named) -> bool| -> Vec<usize> {
        (0..panes.len()).filter(|i| keep(&panes[*i])).collect()
    };
    if let Some(own) = own {
        let ours = among(&|p| p.name == name && p.project.as_deref() == Some(own));
        if !ours.is_empty() {
            return pick(ours);
        }
    }
    pick(among(&|p| {
        p.name == name
            || p.project
                .as_ref()
                .is_some_and(|project| format!("{project}/{}", p.name) == name)
    }))
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
        let perf = Arc::new(crate::perf::Perf::default());
        let daemon = Daemon(Arc::new(Inner {
            fleet: crate::fleet::FleetState::new(&cfg, trust.clone(), perf.clone()),
            perf,
            tailnet: Default::default(),
            https: Default::default(),
            restore: Default::default(),
            db_told: Default::default(),
            rounds: crate::rounds::Rounds::new(crate::fleet::ROUND_DEADLINE),
            relay: crate::relay::Relay::new(crate::relay::MessageStore::new(
                db.clone(),
                &cfg.station,
            )),
            rids: crate::rids::Rids::new(db.clone()),
            files: crate::files::FilesState::new(&paths.state_dir.join("files")),
            approvals: crate::approvals::ApprovalBook::new(db.clone(), &cfg.station),
            trust,
            stops: crate::stops::StopsState::new(db.clone(), &cfg.station),
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
            presence: Mutex::new(HashMap::new()),
            typed: Mutex::new(HashMap::new()),
            folder_trust: Default::default(),
            hooks_trust: Default::default(),
            keep_awake: Mutex::new(None),
            read_marks: Mutex::new(HashMap::new()),
            finds: Mutex::new(HashMap::new()),
            keep_awake_held: std::sync::atomic::AtomicBool::new(false),
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
            glance: Default::default(),
            tools: Default::default(),
            roles: Default::default(),
            prices: Default::default(),
            newrelease: Default::default(),
            adding: Default::default(),
            held: Default::default(),
            link: Default::default(),
            quit: tokio::sync::Notify::new(),
        }));
        daemon.prices_load();
        daemon.agents_jobs_load();
        let _ = std::fs::write(daemon.paths.pid_file(), std::process::id().to_string());

        let adopted = match opts.handoff {
            Some(file) => daemon.adopt_handoff(&file),
            None => false,
        };
        daemon.flags_load();
        // Before anything is restored or served: is this station's name
        // the one it last ran under (rename.rs)?
        daemon.rename_started();
        if !adopted {
            // Restore first, serve afterwards: no deck ever sees this
            // station with its panes missing.
            let start = daemon.restore_begin();
            let found = daemon.recover(&start);
            daemon.note_started(found.resumed);
            daemon.restore_report(&start, &found, if start.clean { "stop" } else { "crash" });
        }
        daemon.restore_started(adopted);
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
        // What the tools document names for this station and is not here
        // yet is installed, off to one side: nothing waits for it.
        daemon.tools_start();
        // A task and a port on 127.0.0.1 for each `[[link]]`. With none,
        // nothing is started and nothing listens.
        if !daemon.cfg.link.is_empty() {
            daemon.link_start();
        }
        Ok(daemon)
    }

    fn spawn_tasks(&self, mut msg_rx: mpsc::UnboundedReceiver<PaneMsg>) {
        let d = self.clone();
        tokio::spawn(async move {
            while let Some(msg) = msg_rx.recv().await {
                match msg {
                    PaneMsg::Term(id, ev) => d.on_pane_event(&id, ev),
                    PaneMsg::Osc7501(id, state, said) => {
                        // Read when present; hooks stay the primary signal.
                        // Antigravity's hooks cover the turn but not its
                        // prompts, so there both are listened to.
                        if let Ok(p) = d.pane(&id) {
                            let (hooked, agent) = {
                                let m = p.meta.lock().unwrap();
                                (m.hooks_seen, m.agent)
                            };
                            if !hooked || agent == AgentKind::Antigravity {
                                // What the program said with it is the
                                // reason, and is written down: without it
                                // nobody can tell why a pane waits.
                                let reason = (state == AgentState::NeedsYou).then(|| match &said {
                                    Some(said) => format!("waiting for you: {said}"),
                                    None => "waiting for you".to_string(),
                                });
                                let news = reason.is_some()
                                    && p.meta.lock().unwrap().needs_reason != reason;
                                if news {
                                    tracing::info!(
                                        pane = %id,
                                        said = said.as_deref().unwrap_or(""),
                                        "the program reports it waits for the owner (OSC 7501)"
                                    );
                                }
                                d.set_state(&p, state, reason);
                            }
                        }
                    }
                }
            }
        });
        if self.flag_on(crate::loops::SWITCH, true) {
            self.spawn_loops();
        } else {
            // The switch `split_tick` off: everything periodic in one loop,
            // each thing awaited in turn, as before.
            let d = self.clone();
            tokio::spawn(async move {
                let mut n: u64 = 0;
                let period = Duration::from_millis(500);
                let mut tick = tokio::time::interval(period);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut last: Option<std::time::Instant> = None;
                loop {
                    tick.tick().await;
                    n += 1;
                    let began = std::time::Instant::now();
                    if let Some(last) = last {
                        d.perf
                            .tick_late
                            .record(began.duration_since(last).saturating_sub(period));
                    }
                    last = Some(began);
                    d.tick(n).await;
                    d.perf.tick_work.record(began.elapsed());
                }
            });
        }
        // An approval that was waiting when the last daemon went is left
        // for its hook to find again (approvals.rs). One whose hook has
        // not come back by now never will: it leaves the list.
        let d = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(crate::approvals::REATTACH_WAIT).await;
            d.expire_detached_approvals();
        });
        // Stops that wait for a station: a task of their own, so a slow
        // tick cannot hold one up. Every two seconds, and at once when the
        // list of stations changes (a station that was away is back).
        let d = self.clone();
        tokio::spawn(async move {
            let mut events = d.bus.subscribe();
            let mut every = tokio::time::interval(Duration::from_secs(2));
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = every.tick() => {}
                    ev = events.recv() => match ev {
                        Ok(ev) if ev.kind != "fleet.updated" => continue,
                        Err(broadcast::error::RecvError::Closed) => return,
                        _ => {}
                    },
                }
                d.pump_stops().await;
            }
        });
    }

    /// Each kind of periodic work in a loop of its own (loops.rs). The
    /// loops marked local wait for no other machine: a station that is
    /// slow or away cannot make them late. Whatever is for another station
    /// is done in that station's own round, which the `fleet` loop only
    /// starts and never waits for (rounds.rs).
    fn spawn_loops(&self) {
        use crate::loops::{spawn, Loop};
        let ms = Duration::from_millis;
        let local = |name, every: u64| Loop {
            name,
            every: ms(every),
            first: Duration::ZERO,
            local: true,
        };
        let remote = |name, every: u64, first: u64| Loop {
            name,
            every: ms(every),
            first: ms(first),
            local: false,
        };
        let d = self.clone();
        spawn(&self.perf, local("screens", 500), move |_| {
            d.detect_screens();
            d.update_keep_awake();
            d.pump_agent_restarts();
            Box::pin(async {})
        });
        let d = self.clone();
        spawn(&self.perf, local("archive", 1000), move |_| {
            let d = d.clone();
            Box::pin(async move {
                let a = d.clone();
                let changed = tokio::task::spawn_blocking(move || {
                    a.archive.poll();
                    a.archive.poll_usage(now_ms())
                })
                .await;
                d.usage_changed(&changed.unwrap_or_default());
            })
        });
        let d = self.clone();
        spawn(&self.perf, local("mail", 1000), move |_| {
            d.pump_mail();
            d.db_health();
            Box::pin(async {})
        });
        let d = self.clone();
        spawn(&self.perf, local("recipes", 500), move |_| {
            let d = d.clone();
            Box::pin(async move { d.pump_recipes().await })
        });
        // Every ten seconds: when this daemon was last running, for the
        // next start. Now and again: the price list (looked at every
        // minute, fetched when a day old), agent versions and ended panes
        // (every ten minutes); each of those works on a thread of its own.
        let d = self.clone();
        spawn(&self.perf, local("upkeep", 10_000), move |n| {
            d.restore_alive();
            // Each pane's guard, on a thread of its own (it reads the
            // orders files): what a hook does while this daemon is away,
            // and by its age, for how long it has been (guard.rs).
            let g = d.clone();
            tokio::task::spawn_blocking(move || g.guards_refresh());
            if n % 6 == 1 {
                d.prices_refresh();
            }
            if n % 60 == 2 {
                d.agents_refresh();
            }
            if n % 60 == 3 {
                d.prune_ended();
            }
            // Is a newer spandeck published? Looked at five minutes after
            // the start and then every hour; asked when the last answer
            // is a day old (newrelease.rs).
            if n % 360 == 30 {
                d.update_check(false);
            }
            // The explorer's leftovers (files.rs): finished jobs and
            // unread zips every minute; old trash and part files nothing
            // writes to at the start and every ten minutes.
            if n % 6 == 4 {
                let f = d.clone();
                tokio::task::spawn_blocking(move || f.files_sweep(n % 60 == 4));
            }
            Box::pin(async {})
        });
        // From here on, work that waits for another machine. Each loop can
        // only make itself late.
        let d = self.clone();
        spawn(&self.perf, remote("relay", 500, 500), move |_| {
            d.pump_relay();
            Box::pin(async {})
        });
        let d = self.clone();
        spawn(&self.perf, remote("notifications", 2000, 2000), move |_| {
            let d = d.clone();
            Box::pin(async move { d.pump_notifications().await })
        });
        let d = self.clone();
        spawn(&self.perf, remote("fleet", 10_000, 500), move |_| {
            let d = d.clone();
            Box::pin(async move { d.look_begin().await })
        });
        // The vault: the whole chain once the fleet has been looked at
        // (the vault may be at another station), then the address, the
        // lock and the token every ten seconds.
        let d = self.clone();
        spawn(&self.perf, remote("vault", 10_000, 600), move |n| {
            let d = d.clone();
            Box::pin(async move {
                if n == 1 {
                    d.rounds.settle(true, crate::fleet::LOOK_WAIT).await;
                }
                d.vault_check(n == 1).await;
                d.vault_report_archive().await;
            })
        });
    }

    /// The switch `split_tick` off: one loop for everything.
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
            self.db_health();
        }
        self.pump_relay();
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
        // For the next start: when this one was last running.
        if n % 20 == 3 {
            self.restore_alive();
            let g = self.clone();
            tokio::task::spawn_blocking(move || g.guards_refresh());
        }
        if n % 1200 == 40 {
            self.prune_ended();
        }
        if n % 7200 == 600 {
            self.update_check(false);
        }
        // Answers kept for a second try go after two minutes.
        if n % 20 == 7 {
            self.rids.prune(now_ms());
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

    /// The owner is told, once, when this station cannot write its
    /// database (writes have failed for 30 seconds, or 1,024 changes of
    /// state wait), and the ship's log says when it can again. Looked at
    /// every second.
    fn db_health(&self) {
        let healthy = self.db.healthy();
        if self.db_told.swap(!healthy, Ordering::Relaxed) == !healthy {
            return;
        }
        let stats = self.db.stats();
        self.record(
            "station.database",
            None,
            None,
            None,
            None,
            json!({"writes": healthy, "waiting": stats.waiting, "log_waiting": stats.log_waiting,
                   "log_dropped": stats.log_dropped, "error": stats.last_error}),
        );
        if !healthy {
            self.notify(
                "station.database",
                None,
                None,
                &format!("{} cannot write its database", self.station),
                &format!(
                    "{}. Stop, Pause, Kill, typing and approvals still work, and what they change is kept in memory ({} changes now) and written when the database takes it; new messages, tasks and panes are refused until then. If spandeck is stopped first, what is in memory is lost. Check the disk the state folder is on.",
                    stats.last_error.as_deref().unwrap_or("Too many changes are waiting to be written"),
                    stats.waiting
                ),
            );
        }
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
        self.pane_from(None, id)
    }

    /// The pane `id` means to the caller in pane `from` (`None`: the owner,
    /// outside every pane): an id, `id@station` for this station, or a
    /// name. A name is looked for among the caller's own project's panes
    /// first (`by_name`).
    pub fn pane_from(&self, from: Option<&str>, id: &str) -> Result<Arc<PaneEntry>> {
        let panes = self.panes.read().unwrap();
        if let Some(p) = panes.get(id) {
            return Ok(p.clone());
        }
        // `x@station`, by this station's name or one it had before.
        let bare = match crate::relay::split_address(id) {
            (name, Some(station)) if self.is_here(station) => name,
            _ => id,
        };
        if let Some(p) = panes.get(bare) {
            return Ok(p.clone());
        }
        let own = from
            .and_then(|f| panes.get(f))
            .and_then(|p| p.meta.lock().unwrap().project.clone());
        let running: Vec<Named> = panes
            .values()
            .filter_map(|p| {
                let m = p.meta.lock().unwrap();
                m.exit.is_none().then(|| Named {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    project: m.project.clone(),
                })
            })
            .collect();
        match by_name(&running, bare, own.as_deref(), &self.station) {
            Ok(Some(i)) => Ok(panes[&running[i].id].clone()),
            Ok(None) => Err(Error::NoPane(id.to_string())),
            Err(addresses) => Err(invalid(format!(
                "more than one pane is called {bare}: {}. Use one of these addresses",
                addresses.join(", ")
            ))),
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
                    if let Some((state, said)) = spandeck_agents::detect::parse_osc7501_why(&bytes)
                    {
                        let _ = msg_tx.send(PaneMsg::Osc7501(id.clone(), state, said));
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
        // The tools it is started with, and those chosen for this station
        // that it is started without, with why (tools.rs).
        let (mut tools_with, mut tools_without) = (vec![], vec![]);
        let mut own_review = false;
        let (program, args) = match (adapter(agent), &opts.program) {
            (Some(a), _) => {
                let cfg = self
                    .cfg
                    .agents
                    .get(agent.as_str())
                    .cloned()
                    .unwrap_or_default();
                let (wiring, env, tools) = self.integration(
                    agent,
                    &id,
                    &token,
                    opts.project.as_deref(),
                    opts.role.as_deref(),
                );
                agent_env = env;
                tools_with = tools.with(agent).into_iter().map(String::from).collect();
                tools_without = tools.without(agent);
                let mut extra = cfg.args.clone();
                extra.extend(wiring);
                extra.extend(opts.extra_args.iter().cloned());
                extra.extend(opts.model_args.iter().cloned());
                extra.extend(opts.args.iter().cloned());
                own_review = extra.iter().any(|a| a == "--approve-for-me");
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
        // Taken before the program exists: what it writes first (a session
        // file, discover.rs) is never dated before its pane's start.
        let starting = now_ms();
        // `[pane_env]` (paneenv.rs) is given to the process and is not in
        // the pane's record, which is kept in the database: a value may be
        // a credential. It comes first, so that what the pane is started
        // with and what spandeck itself sets win over it. With no
        // `[pane_env]` the process is started from the record's own spec.
        let with_pane_env = match self.pane_env() {
            vars if vars.is_empty() => None,
            mut vars => {
                vars.extend(spec.env.iter().cloned());
                Some(SpawnSpec {
                    env: vars,
                    ..spec.clone()
                })
            }
        };
        let started_with = with_pane_env.as_ref().unwrap_or(&spec);
        let term = match self.pane_host() {
            Some(host) => {
                let host = host.map_err(|e| invalid(format!("the pane host is not there: {e}")))?;
                Pane::spawn_on(&host, &id, started_with, sink)
            }
            None => Pane::spawn(started_with, sink),
        }
        .map_err(|e| invalid(format!("could not start {}: {e}", spec.program)))?;
        let now = now_ms();
        let meta = PaneMeta {
            id: id.clone(),
            name,
            project: opts.project.clone(),
            role: opts.role.clone(),
            role_sent_ms: opts.role_sent_ms,
            agent,
            spec,
            cwd: cwd.display().to_string(),
            created_ms: now,
            state: AgentState::Idle,
            state_since_ms: now,
            busy_since_ms: 0,
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
            subagents_over: vec![],
            effort: None,
            effort_at_ms: 0,
            model_said: None,
            effort_said: None,
            model_chosen: None,
            prompt: None,
            model_args: opts.model_args.clone(),
            own_review,
            launcher: opts.program.clone(),
            launcher_recorded: true,
            resumed: resume.is_some(),
            proc_started_ms: Some(starting),
            ended_by: None,
            ended_ms: 0,
            held_back: false,
            held_stopped: None,
            held_restored: None,
            trust_choice: opts.trust_choice.clone(),
            trust_answered: None,
            trust_asks: None,
            hooks_trust_choice: opts.hooks_trust_choice.clone(),
            hooks_trust_answered: None,
            hooks_trust_asks: None,
            stuck_fine: Default::default(),
        };
        let entry = Arc::new(PaneEntry {
            id: id.clone(),
            token,
            term,
            tx,
            meta: Mutex::new(meta),
            wake: Default::default(),
            screen_needs: Default::default(),
            talk: Default::default(),
            holders: Default::default(),
        });
        panes.insert(id.clone(), entry.clone());
        drop(panes);
        if let Some(prompt) = &opts.prompt {
            self.screen_started_with(&entry, prompt);
        }
        self.save_pane(&entry);
        // What its hooks do if this daemon is away (guard.rs): written
        // before the agent can have made its first tool call.
        self.guard_write(&entry);
        let m = entry.meta();
        self.record(
            "pane.created",
            Some(&id),
            m.project.as_deref(),
            m.task.as_deref(),
            None,
            json!({"name": m.name, "agent": m.agent, "resumed": resume.is_some(), "resume_last": opts.resume_last, "tools": tools_with, "tools_without": tools_without}),
        );
        if m.agent == AgentKind::Antigravity {
            if let Some(words) = self.antigravity_without_tools() {
                self.notify(
                    "agent",
                    Some(&id),
                    m.project.as_deref(),
                    &format!("{} has no spandeck tools", m.name),
                    &format!("It was {words}. {}.", self.not_owning_words()),
                );
            }
        }
        // A pane started with a role of the library is sent it, once.
        self.role_at_start(&m);
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
        role: Option<&str>,
    ) -> (Vec<String>, Vec<(String, String)>, crate::tools::Wiring) {
        let bin = self.exe.display().to_string();
        let mut setup = self.setup();
        // A role of the library may ask for a model, an effort and a
        // permission mode of its own: over the agent setup's, for this pane.
        self.role_defaults(role, agent, &mut setup);
        // The servers a pane is given: the agent setup's, the custom tools
        // chosen for this station, and the catalogue tools that are ready
        // here now. One that is still installing is left out, and the
        // pane's record says so.
        let tools = self.tools_wiring(&setup);
        setup.mcp_servers = tools.servers.clone();
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
                // A tool the owner ticked "without asking": the same rule,
                // for that one server and no other.
                for name in tools.allowed(agent) {
                    args.push(format!("mcp__{name}"));
                }
                // Files the owner attaches to a message are kept by the
                // station outside the pane's folder. Found with the real
                // Claude Code, 17 Nov 2026: it stopped to ask "Allow this
                // read outside the working directories?" for each one.
                // Reading the pane's own folder of attached files is
                // allowed by a rule (adding the folder as a working
                // directory makes Claude Code ask to trust it at every start).
                let attached = self.paths.drops_dir().join(pane).join("attached");
                args.push(format!("Read(/{}/**)", attached.display()));
                // What a tool of spandeck's wrote for the agent (its
                // browser's screenshots) is kept outside the pane's folder
                // too. Found with the real Claude Code, 18 Nov 2026: three
                // agents stopped to ask before reading a screenshot they
                // had just taken.
                for dir in tools.outputs(agent) {
                    args.push(format!("Read(/{}/**)", dir.display()));
                }
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
        match agent {
            // A tool the owner ticked "without asking": the setting
            // spandeck's own server has, for that one server and no other.
            AgentKind::Codex => {
                for name in tools.allowed(agent) {
                    args.extend([
                        "-c".to_string(),
                        format!("mcp_servers.{name}.default_tools_approval_mode=\"approve\""),
                    ]);
                }
            }
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
                // hooks. It loads plugins from its config folder (the
                // user's, or a project's own `.agents/`), so spandeck keeps
                // one in the user's, in a folder of its own. Only the
                // station of the account writes there: another station
                // starts Antigravity with whatever plugin that one keeps,
                // or with none, and says so (`antigravity_without_tools`).
                if self.owns_agent_home() {
                    for problem in self.antigravity_plugin(&setup) {
                        tracing::warn!(%problem, "antigravity plugin");
                    }
                }
                // Report state to the terminal (OSC 7501) without asking it
                // first whether it understands.
                env.push(("AGY_CLI_OSC7501".to_string(), "on".to_string()));
            }
            _ => {}
        }
        (args, env, tools)
    }

    /// Is this pane's record written? Waits for the database for at most a
    /// second. For what the owner is answered about (a pane started,
    /// closed, paused): in ordinary running the record is written before
    /// the answer, as it always was; when the database is locked the answer
    /// says `saved: false` and the record is written when it can be.
    pub(crate) fn pane_saved(&self, id: &str) -> bool {
        self.db.saved_within(
            &[
                format!("pane:{id}"),
                format!("pane-closed:{id}"),
                format!("pane-unlisted:{id}"),
            ],
            crate::db::ACK_WAIT,
        )
    }

    pub(crate) fn save_pane(&self, p: &PaneEntry) {
        let meta = p.meta();
        // A pane held back after three restarts keeps an open record, so a
        // later start tries it again; one that ended with a restart keeps
        // the time it ended, which is what its seven days are counted from.
        let closed: Option<i64> = match (meta.held_back, meta.ended_ms) {
            (true, _) => None,
            (false, 0) => meta.exit.map(|_| now_ms() as i64),
            (false, at) => Some(at as i64),
        };
        // Kept until it is written, the newest state of each pane; nothing
        // waits for it (db.rs). Pause and holds are part of the record.
        self.db.put(crate::db::Op::Pane {
            id: meta.id.clone(),
            meta: serde_json::to_string(&meta).unwrap_or_default(),
            token: p.token.clone(),
            pid: Some(p.term.pid() as i64),
            created: meta.created_ms as i64,
            closed,
        });
    }

    fn on_pane_event(&self, id: &str, ev: PaneEvent) {
        let Ok(p) = self.pane(id) else { return };
        match ev {
            PaneEvent::Title(title) => {
                p.meta.lock().unwrap().title = title.clone();
                self.emit("pane.title", Some(id), None, json!({"title": title}));
            }
            PaneEvent::Exited(info) => {
                // The end of a program this pane no longer runs: it was
                // started again under the same id, and the old one's end
                // came late (a real Codex with commands left running held
                // its terminal open past the wait, 18 Nov 2026, and the
                // pane started again was listed as exited while it ran).
                if p.term.exit_info().is_none() {
                    return;
                }
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
        let worked_ms;
        let (old, meta) = {
            let mut m = p.meta.lock().unwrap();
            if m.exit.is_some() {
                return false;
            }
            if m.state == state && m.needs_reason == reason {
                return false;
            }
            let old = m.state;
            worked_ms = now_ms().saturating_sub(m.state_since_ms);
            m.state = state;
            m.needs_reason = reason.clone();
            if old != state {
                m.state_since_ms = now_ms();
                let busy = |s| matches!(s, AgentState::Working | AgentState::NeedsYou);
                if busy(state) && !busy(old) {
                    m.busy_since_ms = m.state_since_ms;
                }
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
        // An agent's turn ended, and no transcript of its own says so: said
        // in the conversation made from its screen (screentalk.rs). A
        // state that only flickered is no turn.
        if old == AgentState::Working
            && matches!(state, AgentState::Idle | AgentState::Done)
            && meta.agent.is_agent()
            && meta.transcript_path.is_none()
            && self.found_session(&p.id).is_none()
            && worked_ms >= 1000
        {
            self.screen_turn_ended(p, worked_ms);
        }
        if old != state {
            // An agent that has just gone idle is looked at again the
            // moment it has been idle long enough to be told of its mail.
            if matches!(state, AgentState::Idle | AgentState::Done) && meta.agent.is_agent() {
                self.wake_mail_at(&p.id, meta.state_since_ms + crate::wake::IDLE_MS + 1);
            }
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

    /// The observer of screens. For an agent's pane whose hooks have not
    /// reported it is the fallback for every state. For one whose hooks do
    /// report it says one thing only: that the program sits at a screen
    /// this adapter knows by name and that no hook tells of (a sign-in, a
    /// trust question, a question asked in the terminal, a permission
    /// prompt of the program's own). It only observes, and answers none.
    fn detect_screens(&self) {
        // A program's folder-trust question is read first, and what such
        // a pane says comes from there (foldertrust.rs).
        let mut trust = self.folder_trust_look();
        // And its review of hooks (hookstrust.rs).
        trust.extend(self.hooks_trust_look());
        // And Claude Code's "Switch model?" (modelswitch.rs).
        trust.extend(self.model_switch_look());
        // And any list a program draws to choose from (picker.rs).
        let lists = self.picker_look(&trust);
        trust.extend(lists);
        // What follows such a screen is given time to be drawn before a
        // mail nudge is typed (wake.rs, `starting`).
        let now = now_ms();
        for id in &trust {
            if let Ok(p) = self.pane(id) {
                p.wake.at_screen(now);
            }
        }
        for p in self.list_panes() {
            let (agent, hooks_seen, exited, state, reason) = {
                let m = p.meta.lock().unwrap();
                (
                    m.agent,
                    m.hooks_seen,
                    m.exit.is_some(),
                    m.state,
                    m.needs_reason.clone(),
                )
            };
            if exited || trust.contains(&p.id) {
                continue;
            }
            let Some(a) = adapter(agent) else { continue };
            let seen = a.detect_screen_why(&p.term.lines());
            let quiet = now_ms().saturating_sub(p.term.last_output_ms()) > 1500;
            let mut said = p.screen_needs.lock().unwrap();
            // Something else has spoken since the screen did: a hook, the
            // owner's answer. What the screen said is forgotten.
            if said.as_ref().is_some_and(|(_, why)| {
                state != AgentState::NeedsYou || reason.as_ref() != Some(why)
            }) {
                *said = None;
            }
            if hooks_seen {
                match seen {
                    Some((AgentState::NeedsYou, Some(why))) => {
                        if state != AgentState::NeedsYou {
                            *said = Some((state, why.clone()));
                            drop(said);
                            self.set_state(&p, AgentState::NeedsYou, Some(why));
                        }
                    }
                    // The screen is gone and no hook has said what came
                    // next: the pane is what it was before.
                    _ if quiet => {
                        if let Some((before, _)) = said.take() {
                            drop(said);
                            self.set_state(&p, before, None);
                        }
                    }
                    _ => {}
                }
                continue;
            }
            match seen {
                Some((now, why)) => {
                    let why = (now == AgentState::NeedsYou)
                        .then(|| why.unwrap_or_else(|| "waiting at a prompt".to_string()));
                    if let (Some(why), true) = (&why, state != AgentState::NeedsYou) {
                        *said = Some((AgentState::Idle, why.clone()));
                    }
                    drop(said);
                    self.set_state(&p, now, why);
                }
                None => {
                    // Screen shows nothing recognisable: working -> done,
                    // and a prompt that is gone no longer needs the owner.
                    let gone = quiet && said.take().is_some();
                    drop(said);
                    if gone {
                        self.set_state(&p, AgentState::Idle, None);
                    } else if state == AgentState::Working && quiet {
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
        self.send_text_from(id, text, enter, None)
    }

    /// Send a line of text to a pane as one item of its input, its Enter
    /// included: nothing else sent to the pane can land between the two,
    /// and nobody waits for the pause before the Enter. `from`: the client
    /// that sent it, told if a stop throws it away.
    pub fn send_text_from(
        &self,
        id: &str,
        text: &str,
        enter: bool,
        from: Option<&str>,
    ) -> Result<()> {
        let p = self.pane(id)?;
        p.wake.typed_by(false);
        p.term.send(Item {
            kind: InputKind::Text,
            bytes: text.as_bytes().to_vec(),
            // Agents with paste detection need the Enter as its own write.
            enter_after: enter.then_some(Duration::from_millis(30)),
            from: from.map(String::from),
        })?;
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
        self.restart_pane_in(id, None, fresh, by)
    }

    /// May this pane's program be started again in `cwd`? The folder as
    /// it will be recorded, or why not.
    ///
    /// What records a pane's folder: the pane's own record (what a reload
    /// and a restart of the station read), and for a pane that fills a
    /// place in a project, the project's file, which names the place's
    /// folder as one inside the project's. So a project's pane moves
    /// inside its project and nowhere else; and a helper on a task stays
    /// in the folder the task was given, where its diff and its sign-off
    /// are read.
    fn move_target(&self, meta: &PaneMeta, cwd: &std::path::Path) -> Result<PathBuf> {
        let name = &meta.name;
        if !cwd.is_absolute() {
            return Err(invalid(format!(
                "{} is not an absolute path on {}",
                cwd.display(),
                self.station
            )));
        }
        let real = std::fs::canonicalize(cwd)
            .ok()
            .filter(|p| p.is_dir())
            .ok_or_else(|| {
                invalid(format!(
                    "{} is not a folder on {}",
                    cwd.display(),
                    self.station
                ))
            })?;
        if let Some(task) = &meta.task {
            return Err(invalid(format!(
                "{name} is doing task {task} in the folder the task was given ({}): its changes and its sign-off are read there, so it is not moved. Start a pane in {} instead",
                meta.cwd,
                cwd.display()
            )));
        }
        if let Some(project) = &meta.project {
            let root = self
                .project_path(project)
                .map(PathBuf::from)
                .ok_or_else(|| Error::NotFound(format!("project {project}")))?;
            let root_real = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
            if !real.starts_with(&root_real) {
                return Err(invalid(format!(
                    "{name} is a pane of the project {project}, whose folder is {}: a project's pane can move to a folder inside its project, not out of it (its place in the project, its orders and the project's record go by that folder). Start a pane in {} instead",
                    root.display(),
                    cwd.display()
                )));
            }
        }
        // As it was given: the folder's own spelling, which is what the
        // pane's program is started in and what the pane then shows.
        Ok(cwd.to_path_buf())
    }

    /// A project's file names the folder of each of its places: a pane
    /// that fills one and was moved is started there again when the
    /// project is next opened.
    fn move_slot(&self, meta: &PaneMeta, cwd: &std::path::Path) {
        let (Some(project), Some(slot)) = (&meta.project, &meta.slot) else {
            return;
        };
        let Ok((root, mut file)) = self.project_file(project) else {
            return;
        };
        let root_real = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let cwd_real = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let Ok(sub) = cwd_real.strip_prefix(&root_real) else {
            return;
        };
        let sub = (!sub.as_os_str().is_empty()).then(|| sub.to_string_lossy().into_owned());
        let Some(place) = file.panes.iter_mut().find(|p| &p.name == slot) else {
            return;
        };
        if place.cwd != sub {
            place.cwd = sub;
            if let Err(e) = file.save(&root) {
                tracing::warn!(project = %project, error = %e, "the pane's new folder was not written to the project's file");
            }
        }
    }

    /// `restart_pane`, and with `cwd` the "Move agent here" of the file
    /// explorer: the program is started again in that folder, under the
    /// same pane, as a new conversation (an agent's conversation belongs
    /// to its folder; the one it was in stays in the archive).
    pub fn restart_pane_in(
        &self,
        id: &str,
        cwd: Option<&std::path::Path>,
        fresh: bool,
        by: &str,
    ) -> Result<Arc<PaneEntry>> {
        let old = self.pane(id)?;
        let meta = old.meta();
        // Looked at before anything is ended: a move that is refused
        // leaves the pane running as it was.
        let moved = match cwd {
            Some(cwd) => Some(self.move_target(&meta, cwd)?),
            None => None,
        };
        let resume = if fresh || moved.is_some() {
            None
        } else {
            self.resumable(&meta)
        };
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
        let p = self
            .spawn_pane(SpawnOpts {
                id: Some(old.id.clone()),
                name: Some(meta.name.clone()),
                agent: Some(meta.agent),
                program: self.launcher_program(&meta),
                cwd: Some(moved.clone().unwrap_or_else(|| PathBuf::from(&meta.cwd))),
                cols: Some(meta.spec.cols),
                rows: Some(meta.spec.rows),
                project: meta.project.clone(),
                role: meta.role.clone(),
                // A new conversation has not heard its role.
                role_sent_ms: if resume.is_some() {
                    meta.role_sent_ms
                } else {
                    0
                },
                task: meta.task.clone(),
                parent_pane: meta.parent_pane.clone(),
                model_args: meta.model_args.clone(),
                resume: resume.clone(),
                slot: meta.slot.clone(),
                // The same rule for the program started again, and only
                // for a pane that was started with the choice. In another
                // folder, the owner's standing choice for that folder.
                trust_choice: match &moved {
                    Some(cwd) => self.folder_trust_at_start(
                        None,
                        meta.agent,
                        cwd,
                        meta.project.as_deref(),
                        by,
                    ),
                    None => meta.trust_choice.clone(),
                },
                hooks_trust_choice: meta.hooks_trust_choice.clone(),
                ..Default::default()
            })
            .inspect_err(|_| {
                // A pane listed after a restart stays listed when it cannot
                // be brought back (its folder is gone, say), with the reason
                // the caller is given; it is not lost from the list.
                if meta.ended_by.is_some() || meta.held_back {
                    self.panes
                        .write()
                        .unwrap()
                        .insert(old.id.clone(), old.clone());
                }
            })?;
        self.keep_owner_marks(&p, &meta);
        // What is known of its model goes with the pane: what its program
        // printed for a command, and what the owner chose for this pane.
        {
            let mut m = p.meta.lock().unwrap();
            m.model_said = meta.model_said.clone();
            m.effort_said = meta.effort_said.clone();
            m.model_chosen = meta.model_chosen.clone();
        }
        self.write_instructions(&p.meta());
        if let Some(cwd) = &moved {
            self.move_slot(&meta, cwd);
        }
        self.record(
            "pane.restarted",
            Some(&p.id),
            meta.project.as_deref(),
            meta.task.as_deref(),
            Some(by),
            match &moved {
                Some(cwd) => json!({"resumed": false, "session_id": null,
                                    "moved_from": meta.cwd, "cwd": cwd.display().to_string()}),
                None => json!({"resumed": resume.is_some(), "session_id": resume}),
            },
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
        self.paste_from(id, text, enter, None)
    }

    /// `paste`, as one item of the pane's input (its Enter included), from
    /// a client that is told if a stop throws it away. A stop that lands
    /// while a marked paste is being written closes it first.
    pub fn paste_from(&self, id: &str, text: &str, enter: bool, from: Option<&str>) -> Result<()> {
        let p = self.pane(id)?;
        let text = text.replace("\r\n", "\n");
        // A paste cannot carry the marker that ends a paste.
        let text = text.replace("\x1b[201~", "").replace("\x1b[200~", "");
        let bracketed = p.term.bracketed_paste();
        let bytes = if bracketed {
            format!("\x1b[200~{}\x1b[201~", text.replace('\n', "\r")).into_bytes()
        } else if p.meta().agent.is_agent() {
            text.split('\n')
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
                .into_bytes()
        } else {
            text.replace('\n', "\r").into_bytes()
        };
        p.wake.typed_by(false);
        p.term.send(Item {
            kind: InputKind::Paste { bracketed },
            bytes,
            enter_after: enter.then_some(PASTE_ENTER),
            from: from.map(String::from),
        })?;
        Ok(())
    }

    /// Keys as they were typed, as one item of the pane's input.
    pub fn type_keys(&self, id: &str, data: &[u8], from: Option<&str>) -> Result<()> {
        let p = self.pane(id)?;
        p.term.send(Item {
            kind: InputKind::Keys,
            bytes: data.to_vec(),
            enter_after: None,
            from: from.map(String::from),
        })?;
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

    /// Pause: no new work is delivered; the current turn finishes. It
    /// holds from the moment it is set in memory. Returns whether it is
    /// saved too: false when the database could not be written within a
    /// second (it is kept and written when it can be; if this station ends
    /// before that, the pane comes back not paused).
    pub fn pause(&self, id: &str, paused: bool, by: &str) -> Result<bool> {
        let p = self.pane(id)?;
        let meta = {
            let mut m = p.meta.lock().unwrap();
            m.paused = paused;
            m.clone()
        };
        if paused {
            // A nudge on its way and not yet typed is taken back: the mail
            // pump's look and its typing cannot pass a Pause (input.rs).
            p.term.withdraw(InputKind::Nudge);
        }
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
        Ok(self.pane_saved(&p.id))
    }

    /// Stop: the agent's current turn is interrupted; the process stays.
    /// The stop's place in the pane's input is now. Waits up to two
    /// seconds for the terminal to take the key: not for a thread that
    /// must not wait.
    pub fn stop(&self, id: &str, by: &str) -> Result<Stopped> {
        let mark = self.pane(id)?.term.mark_stop();
        self.stop_at(id, by, mark, true)
    }

    /// Stop a pane at the place its stop was read (`Pane::mark_stop`).
    ///
    /// What was sent to the pane before that and not started is thrown
    /// away whole and its senders are told; a paste being written is closed
    /// and cut; then the agent's interrupt keys are written; what was typed
    /// after the stop was read is kept. An agent's pane is marked stopped
    /// first, so that from this moment no mail reaches it until the owner
    /// releases it (holds.rs). A program that is not reading cannot be
    /// given the key: `delivered` is false, the pane says so, and nothing
    /// reports it as stopped. `ack`: wait (a second at most) for the
    /// pane's record to be written, for an answer that says `saved`.
    pub fn stop_at(&self, id: &str, by: &str, mark: Mark, ack: bool) -> Result<Stopped> {
        let p = self.pane(id)?;
        let meta = p.meta();
        if meta.exit.is_some() {
            return Ok(Stopped {
                delivered: true,
                discarded: 0,
                saved: true,
            });
        }
        let held = self.hold_stopped(&p, by);
        let keys: &[u8] = adapter(meta.agent)
            .map(|a| a.interrupt_keys())
            .unwrap_or(b"\x03");
        let done = p.term.interrupt(mark, keys);
        self.tell_discarded(&p.id, &done.discarded);
        if held && !done.delivered {
            if let Some(h) = p.meta.lock().unwrap().held_stopped.as_mut() {
                h.reached = false;
            }
            self.save_pane(&p);
        }
        if done.delivered {
            self.cancel_pane_approvals(&p.id);
        }
        self.record(
            if done.delivered {
                "pane.stopped"
            } else {
                "pane.stop_not_reading"
            },
            Some(&p.id),
            meta.project.as_deref(),
            meta.task.as_deref(),
            Some(by),
            json!({"discarded": done.discarded.len(), "held": held}),
        );
        if held {
            self.emit("pane.updated", Some(&p.id), None, json!({}));
        }
        let saved = !held || !ack || self.pane_saved(&p.id);
        // An interrupt while the model is thinking fires no hook at all. Confirm
        // from the screen: once the agent no longer shows itself as busy, the
        // turn is over. Only for a key the terminal took.
        if let Some(a) = adapter(meta.agent).filter(|_| done.delivered) {
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
        Ok(Stopped {
            delivered: done.delivered,
            discarded: done.discarded.len(),
            saved,
        })
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
        self.db.put(crate::db::Op::PaneClosed {
            id: p.id.clone(),
            at: now_ms() as i64,
        });
        self.cancel_pane_approvals(&p.id);
        self.conversation_forget(&p.id);
        self.look_forget_pane(&p.id, by);
        let m = p.meta();
        self.forget_ended(&m);
        self.return_mail(&m);
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
        if !self.cfg.keep_awake || self.keep_awake_held.load(Ordering::SeqCst) {
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

    /// End the keep-awake helper. Before this program image is replaced: the
    /// next image does not know the helper, and each upgrade used to leave
    /// one running (39 on one station after a day of upgrades). The next
    /// image, or this one if the swap fails, starts its own when needed.
    pub fn stop_keep_awake(&self) {
        // The periodic check must not start another one before the swap: on
        // a slow machine it did, and that one was left behind as before.
        self.keep_awake_held.store(true, Ordering::SeqCst);
        if let Some(mut child) = self.keep_awake.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// The swap did not happen: this image carries on and may hold the
    /// machine awake again.
    pub fn resume_keep_awake(&self) {
        self.keep_awake_held.store(false, Ordering::SeqCst);
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
                        wake: Default::default(),
                        screen_needs: Default::default(),
                        talk: Default::default(),
                        holders: Default::default(),
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
    ///
    /// Nothing is left wrong behind it (DECISIONS.md, "After a restart"): a
    /// task whose agent went with the daemon is failed with its resume
    /// information; what a pane that is started again left running is
    /// ended; a pane that cannot be started again stays listed as ended;
    /// approvals nobody can answer any more are expired. With `start.hold`
    /// (the third start in ten minutes that followed no clean stop) no
    /// agent is started: each is listed, and waits for the owner.
    fn recover(&self, start: &crate::restore::StartInfo) -> crate::restore::Recovery {
        let mut found = crate::restore::Recovery::default();
        let rows: Vec<(String, String, String, Option<i64>)> = self
            .db
            .read(|c| {
                let mut stmt =
                    c.prepare("SELECT id, meta, token, pid FROM panes WHERE closed IS NULL")?;
                let rows =
                    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                rows.collect()
            })
            .unwrap_or_default();
        let mut kept = self.adopt_from_host(&rows);
        found.kept = kept.len();
        // What runs on this machine now: read once, and only when a pane
        // that is not kept needs it.
        let mut running: Option<Vec<sys::Proc>> = None;
        for (id, meta, token, pid) in rows {
            // Already in service: kept by the pane host, or adopted from a handoff.
            if kept.contains(&id) || self.panes.read().unwrap().contains_key(&id) {
                kept.push(id);
                continue;
            }
            // A child left over from a handoff that did not complete: its
            // terminal is lost, so end it before resuming the conversation.
            if let Some(pid) = pid.map(|p| p as i32).filter(|p| *p > 1) {
                sys::end_leftover_child(pid);
            }
            let Ok(mut meta) = serde_json::from_str::<PaneMeta>(&meta) else {
                continue;
            };
            // Held back before an upgrade whose handoff lost other panes:
            // still held back, and listed again once this is done.
            if meta.held_back && start.handoff {
                continue;
            }
            found.panes += 1;
            // Held back at an earlier start: looked at again as it was.
            if meta.held_back {
                meta.held_back = false;
                meta.exit = None;
            }
            if self.fail_task_for_restart(&meta) {
                found.tasks_failed += 1;
            }
            // What the pane's program left running in no pane, if anything.
            let left = crate::restore::leftovers(
                running.get_or_insert_with(sys::processes),
                pid,
                meta.proc_started_ms,
                start.last_alive_ms,
                now_ms(),
            );
            let resumable =
                meta.agent.is_agent() && meta.session_id.is_some() && meta.exit.is_none();
            if !resumable || start.hold {
                // Not started again, so what it left running is the
                // owner's to judge (a job detached on purpose): listed in
                // the notice and not touched.
                found.note_left(&left);
                if resumable {
                    found.held += 1;
                } else if meta.agent.is_agent() {
                    found.closed_agents += 1;
                } else {
                    found.closed_shells += 1;
                }
                self.list_ended(meta, token, resumable);
                continue;
            }
            // Started again: the old program's leftovers would run beside
            // the new one, in no pane. Their whole session is ended.
            sys::end_processes(&left.iter().map(|p| p.pid).collect::<Vec<_>>());
            found.leftovers_ended += left.len();
            let opts = SpawnOpts {
                id: Some(id.clone()),
                token: Some(token.clone()),
                name: Some(meta.name.clone()),
                agent: Some(meta.agent),
                program: self.launcher_program(&meta),
                cwd: Some(PathBuf::from(&meta.cwd)),
                cols: Some(meta.spec.cols),
                rows: Some(meta.spec.rows),
                project: meta.project.clone(),
                role: meta.role.clone(),
                role_sent_ms: meta.role_sent_ms,
                task: meta.task.clone(),
                parent_pane: meta.parent_pane.clone(),
                model_args: meta.model_args.clone(),
                resume: self.resumable(&meta),
                slot: meta.slot.clone(),
                trust_choice: meta.trust_choice.clone(),
                hooks_trust_choice: meta.hooks_trust_choice.clone(),
                ..Default::default()
            };
            match self.spawn_pane(opts) {
                Ok(p) => {
                    found.resumed += 1;
                    let why = match (start.handoff, start.clean) {
                        (true, _) => "upgrade",
                        (false, true) => "stop",
                        (false, false) => "crash",
                    };
                    self.keep_marks(&p, &meta, Some(why));
                    if p.meta.lock().unwrap().held_restored.is_some() {
                        found.mail_held += 1;
                    }
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
                    found.closed_agents += 1;
                    self.list_ended(meta, token, false);
                }
            }
        }
        found.approvals_expired = self.expire_ghost_approvals(&kept);
        found
    }

    /// A pane started again under its old id keeps what the owner set on
    /// it: paused, muted, watched. A paused pane that came back taking work
    /// would break the Pause guarantee, and did after a daemon crash
    /// (found on 15 Nov 2026).
    fn keep_owner_marks(&self, p: &Arc<PaneEntry>, old: &PaneMeta) {
        self.keep_marks(p, old, None);
    }

    /// `keep_owner_marks`, and for a pane the station itself started again
    /// after a restart, the mark that makes it wait for the owner before
    /// it takes mail (`restored`; holds.rs). The marks are apart: each is
    /// carried as it was, and none stands for another.
    fn keep_marks(&self, p: &Arc<PaneEntry>, old: &PaneMeta, restored: Option<&str>) {
        let restored = restored
            .filter(|_| old.agent.is_agent() && self.holds_in_force().restored)
            .map(|why| crate::holds::HeldRestored {
                at_ms: now_ms(),
                why: why.to_string(),
            })
            .or_else(|| old.held_restored.clone());
        if !(old.paused
            || old.muted
            || old.watch
            || old.held_stopped.is_some()
            || restored.is_some())
        {
            return;
        }
        {
            let mut m = p.meta.lock().unwrap();
            m.paused = old.paused;
            m.muted = old.muted;
            m.watch = old.watch;
            m.held_stopped = old.held_stopped.clone();
            m.held_restored = restored;
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
            trust_requests: self.trust.requests_for_handoff(),
            rids_kept_ms: self.rids.kept_for().map(|d| d.as_millis() as u64),
            // Last: every pane is still now. The writer is given two
            // seconds, then stopped; what it could not write goes with the
            // panes, so nothing is written behind the next image's back.
            unwritten: {
                // No request with an id is begun from here on: its mark
                // would be written behind the next image's back.
                self.rids.freeze(true);
                self.db.drain(Duration::from_secs(2))
            },
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
        // What the image before could not write is written by this one,
        // and before anything this one changes.
        self.db.restore(state.unwritten);
        // And with them its records of the requests it carried out.
        if let Some(ms) = state.rids_kept_ms {
            self.rids.kept_before(Duration::from_millis(ms));
        }
        // Whoever was waiting to be let in is still waiting.
        self.trust.adopt_requests(&state.trust_requests);
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
                        wake: Default::default(),
                        screen_needs: Default::default(),
                        talk: Default::default(),
                        holders: Default::default(),
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
            // Whatever did not survive resumes by session id, and the owner
            // is told as after any other restart. This is not a start that
            // counts towards holding sessions back.
            let start = crate::restore::StartInfo::handoff(now_ms());
            let found = self.recover(&start);
            self.restore_report(&start, &found, "upgrade");
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
        // The writer was stopped for the image that did not come: it still
        // holds everything, and writes again.
        self.db.resume();
        self.rids.freeze(false);
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
                    wake: Default::default(),
                    screen_needs: Default::default(),
                    talk: Default::default(),
                    holders: Default::default(),
                });
                self.panes.write().unwrap().insert(id, entry);
            }
        }
    }

    /// What this station can do that a station one version behind cannot,
    /// said in `station.hello` and `station.info`. Another station uses an
    /// ability only towards a station that announces it.
    pub fn caps(&self) -> Vec<&'static str> {
        let mut caps = vec!["task_continue", "station_flags"];
        // A layout's Files and Watch leaves are kept whole (project.rs).
        caps.push(crate::project::VIEW_CAP);
        // A tool step that sent files to the owner carries them (`sent`).
        caps.push(crate::conversation::SENT_CAP);
        // `pane.pick` chooses a row of a screen that grants (picker.rs),
        // unless the owner switched that off at this station.
        if self.flag_on(crate::picker::GRANT_CAP, true) {
            caps.push(crate::picker::GRANT_CAP);
        }
        // One answer for everything a deck shows of this station.
        if self.flag_on(crate::snapshot::CAP, true) {
            caps.push(crate::snapshot::CAP);
        }
        // Holds, and `pane.release`: offered by a deck only for a station
        // that says so.
        if self.flag_on("holds", true) {
            caps.push(crate::holds::CAP);
        }
        // The folder-trust tick, the one tap and the list: a deck offers
        // them only for a station that says so.
        caps.push(crate::foldertrust::CAP);
        // An update or install of an agent program is a job this station
        // keeps (`agents.status` rows carry `job` and `latest`), and
        // `agents.check` asks whether each is the newest.
        caps.push(crate::agents::CAP);
        // A pane's branch, processor, memory and mail, and a held answer
        // about its agent program (`pane.glance`, glance.rs).
        caps.push(crate::glance::CAP);
        // The tools document, `tools.status` and the jobs that install a
        // tool here: a station without it needs a newer spandeck.
        caps.push(crate::tools::CAP);
        // The role library, the `roles.` calls, and a pane sent its role.
        caps.push(crate::roledoc::CAP);
        // The tick for spandeck's own hooks, its switch and the one tap.
        caps.push(crate::hookstrust::CAP);
        // A question and a plan are held for the owner, and
        // `question.answer` answers them.
        caps.push(crate::questions::CAP);
        if self.flag_on(crate::stops::CAP, true) {
            caps.push(crate::stops::CAP);
        }
        // It supplies a release to a station of its fleet that cannot
        // download one (supply_fleet.rs).
        caps.push(crate::supply_fleet::CAP);
        // The owner's file explorer: the `files.` methods (files.rs).
        caps.push(crate::files::CAP);
        // It can be either side of a link (link/mod.rs).
        caps.push(crate::link::CAP);
        // Request ids are kept: a change may be tried again by its id.
        if self.flag_on(crate::rids::FLAG, true) {
            caps.push(crate::rids::CAP);
        }
        // A page's `{"ping": n}` is answered, and connections are pinged.
        if self.flag_on(crate::beat::FLAG, true) {
            caps.push(crate::beat::CAP);
        }
        // An attachment is named by the page, and a page that comes back
        // replaces its own earlier connection.
        if self.flag_on(crate::typed::NAMES_FLAG, true) {
            caps.push(crate::typed::NAMES_CAP);
        }
        // What is typed with a stamp is refused when it is late. The
        // stamp is read against the pings, so only where those are answered.
        if self.flag_on(crate::typed::INPUT_FLAG, true) && self.flag_on(crate::beat::FLAG, true) {
            caps.push(crate::typed::INPUT_CAP);
        }
        // A recording is turned into text here, by the owner's program.
        if self.cfg.speech.configured().is_some() {
            caps.push(crate::speech::CAP);
        }
        // The deck is also served over HTTPS, at `https.name` and `port`.
        if self.https.serving() {
            caps.push(crate::https::CAP);
        }
        // A signed-in browser's page loaded from another member's deck may
        // connect here (`page_failover`, the web crate's `deck_origin`).
        if self.flag_on(crate::https::VIA_FLAG, true) {
            caps.push(crate::https::VIA_CAP);
        }
        // It knows of renames: its own, the others', and a name given to a
        // station as it is let in (rename.rs).
        if self.rename_on() {
            caps.push(crate::names::CAP);
        }
        caps
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
            // It answers `perf.get` and `tailnet.report`. Nothing those
            // say is in this answer, which an agent may read.
            "perf": 1,
            // It lists requests that wait at other stations (`trust.relay`).
            "trust_relay": true,
            // What this station can be asked for that an older one cannot.
            "caps": self.caps(),
            "pane_host": self.pane_host_info(),
            "arch": std::env::consts::ARCH,
            "panes": panes.iter().filter(|p| p.meta.lock().unwrap().exit.is_none()).count(),
            "working": count(AgentState::Working),
            "needs_you": count(AgentState::NeedsYou),
            "keep_awake": self.keep_awake_active(),
            "at_keyboard": self.at_keyboard(),
            "web": self.web_addrs.lock().unwrap().clone(),
            // The deck over HTTPS: whether, at which name and port, and if
            // not, why. Nothing of the key, and not when it runs out.
            "https": self.https_brief(),
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
            // What this station's release host said of the newest spandeck
            // (newrelease.rs): a public version number, or why not.
            "update": self.update_brief(),
            "setup": {"updated_ms": self.setup().updated_ms},
            "deck": {"updated_ms": self.deck().updated_ms},
            // Said only by a station that keeps the look document: one that
            // does not say it is never sent a copy.
            "look": {"updated_ms": self.look().updated_ms, "format": crate::look::FORMAT},
            // The same for the owner's preferences.
            "prefs": {"updated_ms": self.prefs().updated_ms, "format": crate::prefs::FORMAT},
            // And for the tools document (tooldoc.rs).
            "tools": {"updated_ms": self.tools_doc().updated_ms, "format": crate::tooldoc::FORMAT},
            // And for the role library (roledoc.rs).
            "roles": {"updated_ms": self.roles_doc().updated_ms, "format": crate::roledoc::FORMAT},
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason for this test: `spandeck send critic "…"` from a pane of
    /// `hero3` failed because `pairtest` on the same station had a
    /// `critic` too (19 Nov 2026). A bare name is the asking pane's own
    /// project's pane first; the owner outside any pane, with several to
    /// choose from, is given their full addresses.
    #[test]
    fn a_bare_name_is_a_pane_of_the_askers_own_project_first() {
        let pane = |id: &str, name: &str, project: Option<&str>| Named {
            id: id.into(),
            name: name.into(),
            project: project.map(String::from),
        };
        let panes = [
            pane("p-1", "critic", Some("pairtest")),
            pane("p-2", "builder", Some("hero3")),
            pane("p-3", "critic", Some("hero3")),
            pane("p-4", "user", Some("pairtest")),
            pane("p-5", "shell", None),
            pane("p-6", "shell", None),
        ];
        let find = |name: &str, own: Option<&str>| by_name(&panes, name, own, "spark2");
        assert_eq!(find("critic", Some("hero3")), Ok(Some(2)));
        assert_eq!(find("critic", Some("pairtest")), Ok(Some(0)));
        // None of that name in its own project: the station's, if one.
        assert_eq!(find("user", Some("hero3")), Ok(Some(3)));
        assert_eq!(find("pairtest/critic", Some("hero3")), Ok(Some(0)));
        assert_eq!(find("nobody", Some("hero3")), Ok(None));
        // The owner, and a pane of a project that has no critic.
        let both = vec![
            "hero3/critic@spark2".to_string(),
            "pairtest/critic@spark2".to_string(),
        ];
        assert_eq!(find("critic", None), Err(both.clone()));
        assert_eq!(find("critic", Some("other")), Err(both));
        assert_eq!(find("builder", None), Ok(Some(1)));
        // Two panes that one address would name are told apart by id.
        assert_eq!(
            find("shell", None),
            Err(vec!["p-5@spark2".to_string(), "p-6@spark2".to_string()])
        );
    }
}
