//! Where spandeck keeps things, and the station's own config file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use spandeck_terminal::sys;

#[derive(Debug, Clone)]
pub struct Paths {
    pub state_dir: PathBuf,
    pub config_dir: PathBuf,
}

impl Paths {
    /// `SPANDECK_STATE_DIR` / `SPANDECK_CONFIG_DIR`, else under the home
    /// directory, where this system keeps such things.
    pub fn from_env() -> Paths {
        let (state, config) = sys::default_dirs(&home_dir());
        let state_dir = std::env::var_os("SPANDECK_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or(state);
        let config_dir = std::env::var_os("SPANDECK_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or(config);
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

    /// The program this station is started from at login, for a daemon whose
    /// own image is a release unpacked under the state directory. A daemon
    /// started by an update from 0.4.0 or earlier is not told that path, so it
    /// is worked out: the place the installer puts the program. Only for the
    /// station that has the login service and the usual state directory; a
    /// trial or a test under another directory keeps the image it runs.
    pub fn started_from(&self, running: &Path) -> PathBuf {
        self.started_from_in(&home_dir(), running)
    }

    /// `started_from` for a given home directory. Whether the state directory
    /// came from `SPANDECK_STATE_DIR` does not matter: a running daemon always
    /// sets that for its children and for the image that replaces it, so
    /// asking for it to be unset (as 0.4.1 did) meant this never applied.
    pub fn started_from_in(&self, home: &Path, running: &Path) -> PathBuf {
        if !running.starts_with(&self.state_dir) {
            return running.to_path_buf();
        }
        let installed = if sys::WINDOWS {
            self.state_dir
                .parent()
                .map(|local| local.join("Programs").join("spandeck").join("spandeck.exe"))
        } else {
            Some(home.join(".local/bin/spandeck"))
        };
        match installed {
            Some(p) if p.is_file() && self.is_the_station(home) => p,
            _ => running.to_path_buf(),
        }
    }

    /// Is this THE station of this user account: the one kept in the usual
    /// state directory, with the login service? A trial, a test or a second
    /// station kept under another directory is not, whatever its HOME is.
    /// Only the station of the account writes anything into the agents'
    /// own settings under the user's home folder (DECISIONS.md, "Only the
    /// station of the account writes the agents' settings in your home").
    pub fn is_the_station(&self, home: &Path) -> bool {
        sys::default_dirs(home).0 == self.state_dir && crate::doctor::service_installed(home)
    }

    pub fn socket(&self) -> PathBuf {
        if let Some(p) = std::env::var_os("SPANDECK_SOCKET") {
            return PathBuf::from(p);
        }
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.state_dir.as_os_str().as_encoded_bytes());
        let tag: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        // Windows: a named pipe, which is not a file. One per state folder.
        if let Some(pipe) = sys::local::pipe_name(&tag) {
            return pipe;
        }
        let path = self.state_dir.join("spandeckd.sock");
        // Unix socket paths are limited to about 100 bytes. A state directory
        // deeper than that gets a short, stable stand-in.
        if path.as_os_str().len() <= 100 {
            return path;
        }
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        dir.join(format!("spandeck-{tag}.sock"))
    }
    /// Where the pane host listens (DECISIONS.md, "Windows"): the local
    /// socket's name with `-host` after it.
    pub fn host_socket(&self) -> PathBuf {
        let mut name = self.socket().into_os_string();
        name.push("-host");
        PathBuf::from(name)
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
    /// A link to the binary this daemon is running, kept so an upgrade can go
    /// back to it even after the file at its path has been replaced.
    pub fn running_exe(&self) -> PathBuf {
        self.state_dir.join("bin").join("running")
    }
    pub fn previous_exe(&self) -> PathBuf {
        self.state_dir.join("bin").join("previous")
    }
    /// The database as it was just before an upgrade that changes its schema.
    pub fn db_before_upgrade(&self) -> PathBuf {
        self.state_dir.join("spandeck.db.before-upgrade")
    }
    pub fn handoff_file(&self) -> PathBuf {
        self.state_dir.join("handoff.json")
    }
    pub fn drops_dir(&self) -> PathBuf {
        self.state_dir.join("drops")
    }
    /// A recording being transcribed (speech.rs): there only meanwhile.
    pub fn speech_dir(&self) -> PathBuf {
        self.state_dir.join("speech")
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
    /// The model price list as last fetched.
    pub fn prices_file(&self) -> PathBuf {
        self.state_dir.join("prices.json")
    }
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn orders_file(&self) -> PathBuf {
        self.config_dir.join("orders.toml")
    }
    /// Where each pane's guard is kept: what its hooks do while the
    /// daemon is away (guard.rs).
    pub fn guard_dir(&self) -> PathBuf {
        self.state_dir.join("panes")
    }
    /// A pane's guard. The pane's id comes from a hook's environment: it
    /// is cut to letters, digits, `-` and `_`, so it is never a path.
    pub fn guard_file(&self, pane: &str) -> PathBuf {
        let name: String = pane
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .take(96)
            .collect();
        self.guard_dir().join(format!("{name}.guard"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentLauncher {
    /// Program to run instead of the agent's default (`claude`, `codex`).
    pub program: Option<String>,
    pub args: Vec<String>,
    /// For tests and unusual setups: the command that installs the program
    /// when it is missing, instead of the vendor's own installer.
    pub install_cmd: Option<Vec<String>>,
    /// The command that updates it, instead of the program's own.
    pub update_cmd: Option<Vec<String>>,
    /// The command that prints its version, instead of `<program> --version`.
    pub version_cmd: Option<Vec<String>>,
    /// Where the newest published version is read, instead of the address
    /// in the catalogue (for a mirror, and for tests). Read the same way.
    pub latest_url: Option<String>,
    /// A command that prints the newest published version, instead of
    /// anything in the catalogue.
    pub latest_cmd: Option<Vec<String>>,
}

/// `[agents]`: a table per agent (`[agents.claude]`), and beside them the
/// one switch `check_latest`.
#[derive(Debug, Clone, Default)]
pub struct Agents {
    /// `check_latest = false`: this station never asks a release host
    /// which version of an agent program is the newest.
    pub check_latest: Option<bool>,
    launchers: BTreeMap<String, AgentLauncher>,
}

impl std::ops::Deref for Agents {
    type Target = BTreeMap<String, AgentLauncher>;
    fn deref(&self) -> &Self::Target {
        &self.launchers
    }
}

impl<'a> IntoIterator for &'a Agents {
    type Item = (&'a String, &'a AgentLauncher);
    type IntoIter = std::collections::btree_map::Iter<'a, String, AgentLauncher>;
    fn into_iter(self) -> Self::IntoIter {
        self.launchers.iter()
    }
}

impl<'de> Deserialize<'de> for Agents {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw: BTreeMap<String, toml::Value> = Deserialize::deserialize(d)?;
        let mut agents = Agents::default();
        for (name, value) in raw {
            if name == "check_latest" {
                agents.check_latest =
                    Some(value.as_bool().ok_or_else(|| {
                        D::Error::custom("[agents] check_latest is true or false")
                    })?);
            } else {
                let launcher = value
                    .try_into()
                    .map_err(|e| D::Error::custom(format!("[agents.{name}]: {e}")))?;
                agents.launchers.insert(name, launcher);
            }
        }
        Ok(agents)
    }
}

impl Serialize for Agents {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(None)?;
        // A plain value before the tables, as TOML wants.
        if let Some(on) = self.check_latest {
            map.serialize_entry("check_latest", &on)?;
        }
        for (name, launcher) in &self.launchers {
            map.serialize_entry(name, launcher)?;
        }
        map.end()
    }
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
    /// HTTPS on the station's Tailscale name (`[web]`, https.rs).
    pub web: WebConfig,
    /// Speech to text by a program of the owner's (`[speech]`, speech.rs).
    pub speech: SpeechConfig,
    pub keep_awake: bool,
    /// Stay awake whenever spandeckd runs, not only while an agent works, so
    /// the station can be reached from a phone at any time.
    pub stay_awake: bool,
    /// Command that holds the machine awake while it runs. Default per OS.
    pub keep_awake_cmd: Option<Vec<String>>,
    /// You count as "at a keyboard" this long after your last input.
    pub at_keyboard_secs: u64,
    /// How long a permission request waits for a remote answer.
    pub approval_timeout_secs: u64,
    pub notify: NotifyConfig,
    pub agents: Agents,
    /// Find other stations through the local Tailscale client.
    pub discovery: bool,
    /// Extra peers as `host:port` (stations discovery cannot see, and tests).
    pub peers: Vec<String>,
    /// Stuck detection thresholds.
    pub stuck_same_failure: u32,
    pub stuck_idle_turns: u32,
    /// Seconds between nudges typed into an idle agent with unread mail.
    pub nudge_secs: u64,
    /// May this station write into the agents' own settings under the
    /// user's home folder (Antigravity's plugin, skills for Claude Code
    /// and Codex, `spandeck integration install`)? Not set: only the
    /// station of this account does, the one in the usual state directory
    /// with the login service. `true` tells another station to (the owner
    /// said so, in this file); `false` stops the usual one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_home: Option<bool>,
    /// AgentVault: normally empty, because spandeck finds the vault by itself.
    pub vault: crate::vault::VaultConfig,
    /// Let this station start another station's daemon over SSH. Off by
    /// default: it means this station can run a command on that machine.
    pub remote_start: bool,
    /// The command used for that. `{host}` and `{script}` are filled in.
    /// Default: `tailscale ssh {host} sh -c {script}`.
    pub remote_start_cmd: Option<Vec<String>>,
    /// Let the owner add a machine to the fleet from this station's Control
    /// Deck: this station connects to it over SSH and installs spandeck
    /// there. Off by default, and separate from `remote_start`.
    pub add_machines: bool,
    /// The command that reaches the machine being added, instead of ssh.
    /// `{user}`, `{host}` and `{known_hosts}` are filled in; the install
    /// script arrives on its standard input.
    pub add_machine_cmd: Option<Vec<String>>,
    /// Where wake-on-LAN packets are sent. Default: 255.255.255.255.
    pub wake_broadcast: Option<String>,
    pub update: UpdateConfig,
    /// Model list prices for cost estimates: fetched automatically.
    pub prices: crate::prices::PricesConfig,
    /// Switches: `[flags]`, each a true/false, a whole number or a short
    /// text (`holds.restored = false`). Read at start; the owner can also
    /// set one while the station runs (`station.flags`), which wins.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub flags: BTreeMap<String, toml::Value>,
}

/// `[speech]`: a recording from the deck is turned into text by a program
/// on this station (speech.rs). Off until `command` is set.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SpeechConfig {
    /// The program and its arguments, run as written (no shell). `{file}`
    /// is the recording's path; without it the path is the last argument.
    /// It prints the text. Example, whisper.cpp:
    /// `["whisper-cli", "-m", "/models/ggml-large-v3-turbo.bin", "-nt", "-np", "-f", "{file}"]`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// The longest recording a deck makes for this station.
    pub max_seconds: u64,
    /// The largest recording taken.
    pub max_bytes: usize,
    /// How long the program is given before it is ended.
    pub timeout_secs: u64,
}

impl Default for SpeechConfig {
    fn default() -> Self {
        SpeechConfig {
            command: None,
            max_seconds: 60,
            max_bytes: 4 << 20,
            timeout_secs: 60,
        }
    }
}

/// `[web]`: the Control Deck over HTTPS (https.rs).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct WebConfig {
    /// Not set: on wherever Tailscale gives this machine a certificate.
    /// `false`: off, and the copy of the key is deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub https: Option<bool>,
    /// The port TLS is answered on, beside `web_port`.
    pub https_port: u16,
    /// Answer a browser that asks the plain port for the page with a
    /// redirect to the HTTPS address. Off: the deck offers it instead.
    pub redirect: bool,
    /// The command that prints the certificate and its key, instead of
    /// `tailscale cert`. `{name}` is filled in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_cmd: Option<Vec<String>>,
    /// The name the certificate is for, instead of the machine's name on
    /// the tailnet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        WebConfig {
            https: None,
            https_port: 7392,
            redirect: false,
            cert_cmd: None,
            name: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct UpdateConfig {
    /// Where releases are downloaded from. Default: the project's GitHub releases.
    pub url: Option<String>,
    /// After an update proves healthy, replace the binary this station was
    /// started from, so a reboot starts the new version.
    pub in_place: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        UpdateConfig {
            url: None,
            in_place: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            station: hostname(),
            labels: vec![],
            web_port: 7391,
            web_bind: vec![],
            web: WebConfig::default(),
            speech: SpeechConfig::default(),
            keep_awake: true,
            stay_awake: false,
            keep_awake_cmd: None,
            at_keyboard_secs: 120,
            approval_timeout_secs: 300,
            notify: NotifyConfig::default(),
            agents: Agents::default(),
            discovery: true,
            peers: vec![],
            stuck_same_failure: 3,
            stuck_idle_turns: 3,
            nudge_secs: 60,
            agent_home: None,
            vault: Default::default(),
            remote_start: false,
            remote_start_cmd: None,
            add_machines: false,
            add_machine_cmd: None,
            wake_broadcast: None,
            update: UpdateConfig::default(),
            prices: Default::default(),
            flags: BTreeMap::new(),
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
    sys::hostname()
        .map(|h| h.split('.').next().unwrap_or(&h).to_string())
        .unwrap_or_else(|| "station".into())
}

/// The user's home folder (`$HOME`; `%USERPROFILE%` on Windows).
pub fn home_dir() -> PathBuf {
    std::env::home_dir()
        .filter(|h| !h.as_os_str().is_empty())
        .unwrap_or_else(std::env::temp_dir)
}

/// A PATH value with more folders at its end, joined this system's way.
pub fn path_with(path: &str, more: &[PathBuf]) -> String {
    let dirs = std::env::split_paths(path)
        .chain(more.iter().filter(|d| !d.as_os_str().is_empty()).cloned());
    std::env::join_paths(dirs)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

pub fn now_ms() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    // Debug builds only, for the test of two stations whose clocks differ:
    // this station's clock, moved by so many milliseconds.
    if cfg!(debug_assertions) {
        static MOVED: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
        let by = *MOVED.get_or_init(|| {
            std::env::var("SPANDECK_TEST_CLOCK_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        });
        return now.saturating_add_signed(by);
    }
    now
}

/// 32 random bytes as hex, from the OS.
pub fn random_token() -> String {
    let mut buf = [0u8; 32];
    sys::random_bytes(&mut buf).expect("the OS random source is available");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Sortable unique id with a short type prefix: `p-01J…`, `m-01J…`.
pub fn new_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        ulid::Ulid::generate().to_string().to_lowercase()
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The bug that happened (15 Nov 2026): LLM-Server was updated from a
    /// release and kept naming the unpacked release as its program, so the
    /// program started at login was never replaced. The daemon always runs
    /// with SPANDECK_STATE_DIR set, and the rule had asked for it to be unset.
    #[test]
    fn a_station_updated_from_a_release_still_names_the_program_it_starts_from() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let paths = Paths {
            state_dir: sys::default_dirs(home).0,
            config_dir: sys::default_dirs(home).1,
        };
        let release = paths.state_dir.join("releases/v9/spandeck-v9/spandeck");
        let installed = home.join(".local/bin/spandeck");
        // No program installed, no login service: a trial keeps its image.
        assert_eq!(paths.started_from_in(home, &release), release);
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, "").unwrap();
        assert_eq!(paths.started_from_in(home, &release), release);
        let service = crate::doctor::service_definition(Path::new("spandeck"), home).0;
        std::fs::create_dir_all(service.parent().unwrap()).unwrap();
        std::fs::write(&service, "").unwrap();
        assert_eq!(paths.started_from_in(home, &release), installed);
        // And that station, alone, is the station of this account: the
        // one that may write the agents' settings in the home folder.
        assert!(paths.is_the_station(home));
        // A program outside the state directory is what it says it is.
        let built = home.join("code/target/release/spandeck");
        assert_eq!(paths.started_from_in(home, &built), built);
        // A station kept somewhere else is a trial or a test.
        let other = Paths {
            state_dir: home.join("elsewhere"),
            config_dir: home.join("elsewhere-config"),
        };
        let image = other.state_dir.join("releases/v9/spandeck");
        assert_eq!(other.started_from_in(home, &image), image);
        assert!(!other.is_the_station(home));
    }
}
