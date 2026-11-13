//! Where spandeck keeps things, and the station's own config file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Paths {
    pub state_dir: PathBuf,
    pub config_dir: PathBuf,
}

impl Paths {
    /// `SPANDECK_STATE_DIR` / `SPANDECK_CONFIG_DIR`, else under the home directory.
    pub fn from_env() -> Paths {
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));
        let state_dir = std::env::var_os("SPANDECK_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state/spandeck"));
        let config_dir = std::env::var_os("SPANDECK_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config/spandeck"));
        Paths {
            state_dir,
            config_dir,
        }
    }

    /// Everything under one directory (tests, and `--home`).
    pub fn at(root: &Path) -> Paths {
        Paths {
            state_dir: root.join("state"),
            config_dir: root.join("config"),
        }
    }

    pub fn socket(&self) -> PathBuf {
        if let Some(p) = std::env::var_os("SPANDECK_SOCKET") {
            return PathBuf::from(p);
        }
        let path = self.state_dir.join("spandeckd.sock");
        // Unix socket paths are limited to about 100 bytes. A state directory
        // deeper than that gets a short, stable stand-in.
        if path.as_os_str().len() <= 100 {
            return path;
        }
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.state_dir.as_os_str().as_encoded_bytes());
        let tag: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        dir.join(format!("spandeck-{tag}.sock"))
    }
    pub fn db(&self) -> PathBuf {
        self.state_dir.join("spandeck.db")
    }
    pub fn archive_db(&self) -> PathBuf {
        self.state_dir.join("archive.db")
    }
    pub fn token_file(&self) -> PathBuf {
        self.state_dir.join("client.token")
    }
    pub fn handoff_file(&self) -> PathBuf {
        self.state_dir.join("handoff.json")
    }
    pub fn drops_dir(&self) -> PathBuf {
        self.state_dir.join("drops")
    }
    pub fn log_file(&self) -> PathBuf {
        self.state_dir.join("spandeckd.log")
    }
    pub fn pid_file(&self) -> PathBuf {
        self.state_dir.join("spandeckd.pid")
    }
    pub fn worktrees_dir(&self) -> PathBuf {
        self.state_dir.join("worktrees")
    }
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn orders_file(&self) -> PathBuf {
        self.config_dir.join("orders.toml")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentLauncher {
    /// Program to run instead of the agent's default (`claude`, `codex`).
    pub program: Option<String>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NotifyConfig {
    /// An ntfy topic URL. Notifications are POSTed there when you are away.
    pub ntfy_url: Option<String>,
    /// Any URL that accepts a JSON POST.
    pub webhook_url: Option<String>,
    /// Which states notify. Default: needs_you, error, done for watched panes.
    pub states: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// This station's name in the fleet. Default: the hostname.
    pub station: String,
    /// Labels such as "gpu", "laptop", "always-on" (standing-order groups).
    pub labels: Vec<String>,
    pub web_port: u16,
    /// Addresses to listen on. Empty: localhost plus the tailnet address.
    pub web_bind: Vec<String>,
    pub keep_awake: bool,
    /// Command that holds the machine awake while it runs. Default per OS.
    pub keep_awake_cmd: Option<Vec<String>>,
    /// You count as "at a keyboard" this long after your last input.
    pub at_keyboard_secs: u64,
    /// How long a permission request waits for a remote answer.
    pub approval_timeout_secs: u64,
    pub notify: NotifyConfig,
    pub agents: BTreeMap<String, AgentLauncher>,
    /// Find other stations through the local Tailscale client.
    pub discovery: bool,
    /// Extra peers as `host:port` (stations discovery cannot see, and tests).
    pub peers: Vec<String>,
    /// Stuck detection thresholds.
    pub stuck_same_failure: u32,
    pub stuck_idle_turns: u32,
    /// Seconds between nudges typed into an idle agent with unread mail.
    pub nudge_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            station: hostname(),
            labels: vec![],
            web_port: 7391,
            web_bind: vec![],
            keep_awake: true,
            keep_awake_cmd: None,
            at_keyboard_secs: 120,
            approval_timeout_secs: 300,
            notify: NotifyConfig::default(),
            agents: BTreeMap::new(),
            discovery: true,
            peers: vec![],
            stuck_same_failure: 3,
            stuck_idle_turns: 3,
            nudge_secs: 60,
        }
    }
}

impl Config {
    pub fn load(paths: &Paths) -> anyhow::Result<Config> {
        let file = paths.config_file();
        match std::fs::read_to_string(&file) {
            Ok(text) => Ok(
                toml::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }
}

pub fn hostname() -> String {
    nix::unistd::gethostname()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|h| h.split('.').next().unwrap_or(&h).to_string())
        .unwrap_or_else(|| "station".into())
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 32 random bytes as hex, from the OS.
pub fn random_token() -> String {
    use std::io::Read;
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("the OS random source is available");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Sortable unique id with a short type prefix: `p-01J…`, `m-01J…`.
pub fn new_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        ulid::Ulid::generate().to_string().to_lowercase()
    )
}
