//! Scenario harness: a real spandeckd in a temp directory, driven through its
//! real socket, with scriptable fake agents in its panes.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spandeck_core::client::{Client, ClientError};
use spandeck_core::Paths;

pub const SPANDECK: &str = env!("CARGO_BIN_EXE_spandeck");
pub const FAKE: &str = env!("CARGO_BIN_EXE_spandeck-fake-agent");
/// The program the Windows log-on task runs: it keeps a station running.
pub const SPANDECKD: &str = env!("CARGO_BIN_EXE_spandeckd");
/// Unix: an upgraded daemon is the same process. Windows: a new one.
pub const UPGRADE_KEEPS_PID: bool = spandeck_terminal::sys::UPGRADE_KEEPS_PID;

pub struct Station {
    pub name: String,
    pub root: PathBuf,
    pub port: u16,
    pub child: Option<Child>,
    /// `spandeckd`, for a station started as the log-on task starts one.
    /// The daemon is then not this test's child.
    pub supervisor: Option<Child>,
    pub env: Vec<(String, String)>,
    _dir: Option<tempfile::TempDir>,
    scripts: std::sync::atomic::AtomicU32,
}

/// A port for a test station. Asking the system for "any free port" and then
/// letting go of it lets another test, or a daemon that restarts and listens
/// again, be given the same one: that made tests fail on small CI machines.
/// So each test binary hands out its own run of ports, below the range the
/// system uses for outgoing connections, and never hands one out twice.
/// Config for a test station that is to write the agents' settings under
/// its home folder (a throwaway one: `Station::command` sets HOME). Without
/// it a test station writes nothing there, like any station that is not
/// the one of its account.
#[allow(dead_code)]
pub const AGENT_HOME: &str = "agent_home = true\n";

pub fn free_port() -> u16 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let base = 20000 + (std::process::id() * 131) % 9000;
    loop {
        let port = (base + NEXT.fetch_add(1, Ordering::SeqCst) % 1000) as u16;
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

/// Like `wait_for`, and on a timeout says what `context` returns.
pub fn wait_for_or<T>(
    what: &str,
    secs: u64,
    context: impl Fn() -> String,
    mut f: impl FnMut() -> Option<T>,
) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if start.elapsed() > Duration::from_secs(secs) {
            panic!("timed out waiting for {what}\n{}", context());
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

pub fn wait_for<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(40));
    }
}

impl Station {
    /// A station of the test fleet on a port the test chose, with the
    /// peer addresses the test chose (a proxy's, say). Nothing is waited
    /// for.
    pub fn in_fleet(name: &str, port: u16, peers: &[u16], extra: &str) -> Station {
        let peers: Vec<String> = peers.iter().map(|p| format!("\"127.0.0.1:{p}\"")).collect();
        Station::start_keyed(
            name,
            port,
            &format!("peers = [{}]\n{extra}", peers.join(", ")),
            Some(&"5a".repeat(32)),
        )
    }

    pub fn start(name: &str) -> Station {
        Station::start_with(name, "", &[])
    }

    /// `extra` is appended to config.toml; `env` is added to the daemon's environment.
    pub fn start_with(name: &str, extra: &str, env: &[(&str, &str)]) -> Station {
        Station::start_on(name, free_port(), extra, env)
    }

    /// Two stations on this machine that list each other as peers and are in
    /// one fleet: they were given the same fleet key before they started.
    pub fn pair() -> (Station, Station) {
        let (a, b) = Station::strangers(Some("5a".repeat(32)));
        (a, b)
    }

    /// Two stations that can see each other. With no key given, each starts
    /// its own fleet, as two fresh installs would.
    pub fn strangers(shared_key: Option<String>) -> (Station, Station) {
        Station::two(shared_key, "")
    }

    /// A linked pair, with extra config for the first station.
    pub fn pair_with(extra_a: &str) -> (Station, Station) {
        Station::two(Some("5a".repeat(32)), extra_a)
    }

    /// A linked pair whose stations were both told, in their config, to
    /// write the agents' settings under their (throwaway) home folders, as
    /// the station of an account does.
    pub fn pair_told() -> (Station, Station) {
        Station::two_with(Some("5a".repeat(32)), AGENT_HOME, AGENT_HOME)
    }

    /// Three stations in one fleet: alpha, bravo and charlie.
    pub fn three() -> (Station, Station, Station) {
        let ports = [free_port(), free_port(), free_port()];
        let key = "5a".repeat(32);
        let start = |i: usize, name: &str| {
            let others: Vec<String> = (0..3)
                .filter(|j| *j != i)
                .map(|j| format!("\"127.0.0.1:{}\"", ports[j]))
                .collect();
            Station::start_keyed(
                name,
                ports[i],
                &format!("peers = [{}]\n", others.join(", ")),
                Some(&key),
            )
        };
        let (a, b, c) = (start(0, "alpha"), start(1, "bravo"), start(2, "charlie"));
        for s in [&a, &b, &c] {
            wait_for("the three stations to find each other", 20, || {
                let v = s.call("fleet.refresh", json!({}));
                (v["stations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|x| x["state"] == json!("online"))
                    .count()
                    == 3)
                    .then_some(())
            });
        }
        (a, b, c)
    }

    /// Stations that list each other as peers. Those given the same key are
    /// one fleet; one given none starts a fleet of its own. Nothing is
    /// waited for: the test says what it expects each to see.
    pub fn several(stations: &[(&str, Option<&str>)]) -> Vec<Station> {
        let ports: Vec<u16> = stations.iter().map(|_| free_port()).collect();
        stations
            .iter()
            .enumerate()
            .map(|(i, (name, key))| {
                let others: Vec<String> = ports
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, port)| format!("\"127.0.0.1:{port}\""))
                    .collect();
                Station::start_keyed(
                    name,
                    ports[i],
                    &format!("peers = [{}]\n", others.join(", ")),
                    *key,
                )
            })
            .collect()
    }

    /// Stations of one fleet that list each other as peers, each with its
    /// own extra config and environment (given its own folder). Nothing is
    /// waited for: the test says what it expects each to see.
    pub fn fleet(
        stations: &[&str],
        each: impl Fn(&str, &Path) -> (String, Vec<(String, String)>),
    ) -> Vec<Station> {
        let ports: Vec<u16> = stations.iter().map(|_| free_port()).collect();
        stations
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let others: Vec<String> = ports
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, port)| format!("\"127.0.0.1:{port}\""))
                    .collect();
                let mut s = Station::prepare(name, ports[i], "", &[]);
                let (extra, env) = each(name, &s.root);
                let root = s.root.clone();
                let dir = s._dir.take();
                s = Station::prepare_in(
                    name,
                    ports[i],
                    &format!("peers = [{}]\n{extra}", others.join(", ")),
                    root,
                    dir,
                );
                s.env = env;
                std::fs::create_dir_all(s.root.join("state")).unwrap();
                std::fs::write(s.root.join("state/fleet.key"), "5a".repeat(32)).unwrap();
                s.launch();
                s
            })
            .collect()
    }

    /// How this station sees another one, after looking again.
    pub fn sees(&self, other: &str) -> Option<String> {
        self.call("fleet.refresh", json!({}))["stations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["station"] == json!(other))
            .and_then(|x| x["state"].as_str().map(String::from))
    }

    fn two(shared_key: Option<String>, extra_a: &str) -> (Station, Station) {
        Station::two_with(shared_key, extra_a, "")
    }

    fn two_with(shared_key: Option<String>, extra_a: &str, extra_b: &str) -> (Station, Station) {
        let (pa, pb) = (free_port(), free_port());
        let key = shared_key.as_deref();
        let a = Station::start_keyed(
            "alpha",
            pa,
            &format!("peers = [\"127.0.0.1:{pb}\"]\n{extra_a}"),
            key,
        );
        let b = Station::start_keyed(
            "bravo",
            pb,
            &format!("peers = [\"127.0.0.1:{pa}\"]\n{extra_b}"),
            key,
        );
        let want = if key.is_some() {
            "online"
        } else {
            "not_linked"
        };
        for s in [&a, &b] {
            wait_for("the stations to find each other", 20, || {
                let v = s.call("fleet.refresh", json!({}));
                (v["stations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|x| x["state"] == json!("online") || x["state"] == json!(want))
                    .count()
                    == 2)
                    .then_some(())
            });
        }
        (a, b)
    }

    /// A station that is ready to be started (`launch`) and has not been:
    /// for a test that puts something in its home folder first.
    #[allow(dead_code)]
    pub fn not_started(name: &str, extra: &str) -> Station {
        Station::prepare(name, free_port(), extra, &[])
    }

    pub fn start_on(name: &str, port: u16, extra: &str, env: &[(&str, &str)]) -> Station {
        let mut s = Station::prepare(name, port, extra, env);
        s.launch();
        s
    }

    /// A station of the test fleet (the key every `pair` and `three` has)
    /// on a port and with a config of the caller's choosing: for a fleet
    /// in which one station reaches another through something in between.
    #[allow(dead_code)]
    pub fn keyed_on(name: &str, port: u16, extra: &str) -> Station {
        Station::start_keyed(name, port, extra, Some(&"5a".repeat(32)))
    }

    pub fn start_keyed(name: &str, port: u16, extra: &str, key: Option<&str>) -> Station {
        let mut s = Station::prepare(name, port, extra, &[]);
        if let Some(key) = key {
            std::fs::create_dir_all(s.root.join("state")).unwrap();
            std::fs::write(s.root.join("state/fleet.key"), key).unwrap();
        }
        s.launch();
        s
    }

    fn prepare(name: &str, port: u16, extra: &str, env: &[(&str, &str)]) -> Station {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut s = Station::prepare_in(name, port, extra, root, Some(dir));
        s.env = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        s
    }

    fn prepare_in(
        name: &str,
        port: u16,
        extra: &str,
        root: PathBuf,
        dir: Option<tempfile::TempDir>,
    ) -> Station {
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("fake")).unwrap();
        // Top-level keys first: TOML tables swallow everything after them.
        // Tables that begin with `[agents.fake]` add to the fake agent's own.
        let (top, tables): (Vec<&str>, Vec<&str>) = {
            let idx = extra
                .starts_with('[')
                .then_some(0)
                .or_else(|| extra.find("\n[").map(|i| i + 1))
                .unwrap_or(extra.len());
            (vec![&extra[..idx]], vec![&extra[idx..]])
        };
        // No test fetches the real price list: a test that wants prices
        // names a local one (`fake_prices`).
        let prices = if extra.contains("prices.") {
            ""
        } else {
            "prices.auto = false\n"
        };
        // A test that stands in for the `tailscale` command says so itself.
        let discovery = if extra.contains("discovery =") {
            ""
        } else {
            "discovery = false\n"
        };
        // A test of where the station listens says so itself.
        let bind = if extra.contains("web_bind =") {
            ""
        } else {
            "web_bind = [\"127.0.0.1\"]\n"
        };
        // A program that only waits stands in for the keep-awake helper.
        let wait = if spandeck_terminal::sys::WINDOWS {
            r#"["ping", "-n", "300", "127.0.0.1"]"#
        } else {
            r#"["sleep", "300"]"#
        };
        std::fs::write(
            root.join("config/config.toml"),
            format!(
                "station = \"{name}\"\nweb_port = {port}\n{bind}{discovery}keep_awake_cmd = {wait}\n{prices}{}\n[agents.fake]\nprogram = '{FAKE}'\n{}\n",
                top.join(""),
                tables
                    .join("")
                    .strip_prefix("[agents.fake]\n")
                    .unwrap_or(&tables.join(""))
            ),
        )
        .unwrap();
        Station {
            name: name.to_string(),
            root,
            port,
            child: None,
            supervisor: None,
            env: vec![],
            _dir: dir,
            scripts: std::sync::atomic::AtomicU32::new(0),
        }
    }

    pub fn launch(&mut self) {
        let mut cmd = self.command(SPANDECK);
        cmd.arg("daemon");
        self.child = Some(cmd.spawn().unwrap());
        self.wait_ready();
    }

    /// A station started the way the Windows log-on task starts one:
    /// `spandeckd` starts the daemon and keeps it running.
    pub fn start_supervised(name: &str, env: &[(&str, &str)]) -> Station {
        Station::start_supervised_by(name, "", env, Path::new(SPANDECKD))
    }

    /// The same, by a copy of `spandeckd` somewhere else (an installed
    /// one, with `spandeck` beside it), with `extra` added to config.toml.
    pub fn start_supervised_by(
        name: &str,
        extra: &str,
        env: &[(&str, &str)],
        spandeckd: &Path,
    ) -> Station {
        let mut s = Station::prepare(name, free_port(), extra, env);
        let program = spandeckd.to_string_lossy().to_string();
        s.supervisor = Some(s.command(&program).spawn().unwrap());
        s.wait_ready();
        s
    }

    /// The pid of the daemon that answers now.
    pub fn daemon_pid(&self) -> u32 {
        self.call("station.info", json!({}))["pid"]
            .as_u64()
            .unwrap() as u32
    }

    /// End a process this station started, without warning (Windows).
    pub fn end_process(pid: u32) {
        let out = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output()
            .unwrap();
        assert!(out.status.success(), "taskkill {pid}: {out:?}");
    }

    /// A program of this station's: its folders, and nothing of the
    /// machine's own.
    pub fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env("SPANDECK_STATE_DIR", self.root.join("state"))
            .env("SPANDECK_CONFIG_DIR", self.root.join("config"))
            .env("SPANDECK_FAKE_DIR", self.root.join("fake"))
            .env("HOME", self.root.join("home"))
            // Windows: the home folder is the profile.
            .env("USERPROFILE", self.root.join("home"))
            .env("SPANDECK_LOG", "warn")
            // Where a pane host is used (Windows): one whose daemon a test
            // ended does not wait ten minutes for another.
            .env("SPANDECK_HOST_LINGER", "5")
            .env_remove("SPANDECK_SOCKET")
            .env_remove("SPANDECK_TOKEN")
            .env_remove("SPANDECK_PANE")
            // Codex's settings are the throwaway home's, never the ones a
            // developer's own shell points at.
            .env_remove("CODEX_HOME")
            // Nor is OpenCode's database: the throwaway home has its own.
            .env_remove("XDG_DATA_HOME")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        // A test must never start the owner's own agent programs. On
        // Windows the test machine's PATH has their folders; without them a
        // test whose stand-in is missing fails instead of starting one.
        if spandeck_terminal::sys::WINDOWS {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let kept = std::env::split_paths(&path).filter(|d| {
                let d = d.to_string_lossy().to_lowercase();
                ![
                    r"\.local\bin",
                    r"\openai\codex",
                    r"\agy\",
                    r"\npm",
                    r"\.opencode",
                ]
                .iter()
                .any(|agent| d.contains(agent))
            });
            cmd.env("PATH", std::env::join_paths(kept).unwrap());
            // The folders the catalogue looks in beyond PATH hang off these.
            cmd.env("LOCALAPPDATA", self.root.join("home"));
            cmd.env("APPDATA", self.root.join("home"));
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }

    pub fn wait_ready(&self) {
        // Ready means both the socket and the Control Deck listener are up.
        // (Thirty seconds: fifteen was passed now and then on a Mac running
        // every test at once.)
        wait_for("daemon to answer", 30, || {
            let info = self.try_call("station.info", json!({})).ok()?;
            info["web"]
                .as_array()
                .is_some_and(|w| !w.is_empty())
                .then_some(())
        });
    }

    pub fn paths(&self) -> Paths {
        Paths {
            state_dir: self.root.join("state"),
            config_dir: self.root.join("config"),
        }
    }

    pub fn token(&self) -> String {
        std::fs::read_to_string(self.paths().token_file())
            .unwrap()
            .trim()
            .to_string()
    }

    pub fn client(&self) -> Result<Client, ClientError> {
        let token =
            std::fs::read_to_string(self.paths().token_file()).map_err(|e| ClientError {
                code: "io".into(),
                message: e.to_string(),
            })?;
        let mut c = Client::connect(&self.paths().socket(), token.trim())?;
        c.set_timeout(Some(Duration::from_secs(60)));
        Ok(c)
    }

    pub fn try_call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        self.client()?.call(method, params)
    }

    pub fn call(&self, method: &str, params: Value) -> Value {
        self.try_call(method, params)
            .unwrap_or_else(|e| panic!("{method} failed: {e}"))
    }

    /// Call the API as a pane (with the pane's own token), the way an agent does.
    pub fn call_as_pane(
        &self,
        pane: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, ClientError> {
        let token: String = {
            let db = rusqlite_token(&self.root.join("state/spandeck.db"), pane);
            db.unwrap_or_else(|| panic!("no token for pane {pane}"))
        };
        let mut c = Client::connect(&self.paths().socket(), &token)?;
        c.set_timeout(Some(Duration::from_secs(60)));
        c.call(method, params)
    }

    /// A pane's own token, as the station's database has it.
    pub fn pane_token(&self, pane: &str) -> String {
        rusqlite_token(&self.root.join("state/spandeck.db"), pane)
            .unwrap_or_else(|| panic!("no token for pane {pane}"))
    }

    pub fn script_file(&self, script: &Value) -> PathBuf {
        let n = self
            .scripts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = self.root.join(format!("script-{n}.json"));
        std::fs::write(&path, script.to_string()).unwrap();
        path
    }

    /// Start a fake agent pane running `script`. Extra fields go to pane.spawn.
    pub fn fake(&self, script: Value, extra: Value) -> String {
        let path = self.script_file(&script);
        let mut params =
            json!({"agent": "fake", "args": ["--script", path], "cwd": self.root.join("home")});
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                params[k] = v.clone();
            }
        }
        self.call("pane.spawn", params)["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    pub fn pane(&self, pane: &str) -> Value {
        self.call("pane.get", json!({"pane": pane}))
    }

    pub fn text(&self, pane: &str) -> String {
        self.call("pane.text", json!({"pane": pane, "history": 500}))["lines"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn wait_text(&self, pane: &str, needle: &str) -> String {
        let mut last = String::new();
        let start = Instant::now();
        loop {
            if let Ok(v) = self.try_call("pane.text", json!({"pane": pane, "history": 500})) {
                last = v["lines"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|l| l.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                if last.contains(needle) {
                    return last;
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "timed out waiting for {needle:?} in pane {pane}; screen:\n{}",
                last.trim_end()
            );
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    pub fn wait_state(&self, pane: &str, state: &str) -> Value {
        let start = Instant::now();
        loop {
            let p = self.pane(pane);
            if p["state"] == json!(state) {
                return p;
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "timed out waiting for state {state}; pane is {p}\nscreen:\n{}",
                self.text(pane).trim_end()
            );
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    pub fn log(&self, kind: &str) -> Vec<Value> {
        self.call("log.query", json!({"kind": kind}))
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    /// Freeze or thaw the daemon: to its peers it looks unreachable, while
    /// its panes keep running.
    pub fn signal(&self, sig: i32) {
        kill_pid(self.pid() as i32, sig);
    }

    /// After a freezing signal: wait until the daemon really does not
    /// answer. The signal takes hold a moment after `kill` returns, and a
    /// call made in that moment is still answered (a stop was, once, on a
    /// busy machine).
    pub fn wait_frozen(&self) {
        use std::io::{Read, Write};
        // Frozen means: no byte comes back in half a second. A connection
        // that is not taken, or a request that cannot be written, is the
        // same thing (macOS does not take a connection for a stopped
        // process the way Linux does).
        wait_for("the frozen station to stop answering", 20, || {
            let half = std::time::Duration::from_millis(500);
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.port));
            let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, half) else {
                return Some(());
            };
            let _ = s.set_read_timeout(Some(half));
            let _ = s.set_write_timeout(Some(half));
            let body = r#"{"method":"station.hello","params":{}}"#;
            if write!(
                s,
                "POST /api/peer/rpc HTTP/1.1\r\nhost: station\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .is_err()
            {
                return Some(());
            }
            match s.read(&mut [0u8; 16]) {
                Ok(n) if n > 0 => None,
                _ => Some(()),
            }
        });
    }

    pub fn kill9(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn git_repo(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "test@example.com"]);
        git(&dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "first"]);
        dir
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A pane's record is written a moment after the pane is started (the
/// station does not wait for its database), so it is looked for again
/// for a few seconds.
fn rusqlite_token(db: &Path, pane: &str) -> Option<String> {
    let start = Instant::now();
    loop {
        match rusqlite_token_now(db, pane) {
            None if start.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            found => return found,
        }
    }
}

fn rusqlite_token_now(db: &Path, pane: &str) -> Option<String> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    conn.query_row(
        "SELECT token FROM panes WHERE id=?1 OR json_extract(meta,'$.name')=?1 ORDER BY created DESC LIMIT 1",
        [pane],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .filter(|t| !t.is_empty())
}

impl Drop for Station {
    fn drop(&mut self) {
        // A failing test shows what its station had to say.
        if std::thread::panicking() {
            if let Ok(log) = std::fs::read_to_string(self.root.join("state/spandeckd.log")) {
                let tail: Vec<&str> = log.lines().rev().take(40).collect();
                eprintln!("---- last lines of {}'s log ----", self.name);
                for line in tail.into_iter().rev() {
                    eprintln!("{}", line.chars().take(400).collect::<String>());
                }
            }
        }
        // First the program that would start an ended daemon again.
        if let Some(mut c) = self.supervisor.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        // Windows: an upgrade in the test left a daemon that is not our child.
        if spandeck_terminal::sys::WINDOWS {
            let now = self.try_call("station.info", json!({})).ok();
            if let Some(pid) = now.and_then(|i| i["pid"].as_u64()) {
                if self.child.as_ref().is_none_or(|c| u64::from(c.id()) != pid) {
                    let _ = Command::new("taskkill")
                        .args(["/PID", &pid.to_string(), "/F"])
                        .output();
                }
            }
        }
        if let Some(mut c) = self.child.take() {
            // SIGTERM first so panes are cleaned up with the daemon. (On
            // Windows the pane host ends them a few seconds after it.)
            if !spandeck_terminal::sys::WINDOWS {
                kill_pid(c.id() as i32, 15);
            }
            let start = Instant::now();
            while !spandeck_terminal::sys::WINDOWS && start.elapsed() < Duration::from_secs(2) {
                if let Ok(Some(_)) = c.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Send a signal to a process, with the system's own `kill`. Windows has no
/// signals: a test that needs one is marked ignored there, with its reason.
pub fn kill_pid(pid: i32, sig: i32) {
    if spandeck_terminal::sys::WINDOWS {
        panic!("this test sends a signal, which Windows does not have");
    }
    let _ = Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .status();
}

pub mod browser {
    use super::*;
    use std::time::Duration;

    // ------------------------------------------------------------ browser smoke

    fn chrome() -> Option<std::path::PathBuf> {
        if let Some(p) = std::env::var_os("SPANDECK_CHROME") {
            return Some(p.into());
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let mut found = vec![];
        for base in [
            format!("{home}/.cache/ms-playwright"),
            format!("{home}/Library/Caches/ms-playwright"),
        ] {
            for entry in std::fs::read_dir(&base).into_iter().flatten().flatten() {
                for sub in std::fs::read_dir(entry.path())
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    for name in [
                        "chrome-headless-shell",
                        "chrome",
                        "Chromium.app/Contents/MacOS/Chromium",
                    ] {
                        let p = sub.path().join(name);
                        if p.is_file() {
                            found.push(p);
                        }
                    }
                }
            }
        }
        found.sort();
        found.pop().or_else(|| {
            [
                "/usr/bin/google-chrome",
                "/usr/bin/chromium",
                "/usr/bin/chromium-browser",
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            ]
            .iter()
            .map(std::path::PathBuf::from)
            .find(|p| p.is_file())
        })
    }

    /// A terminal's history on a phone (phonescroll.rs, and the scenario in
    /// phase5.rs): the terminal on the phone's screen.
    pub const T: &str = "document.querySelector('.phone-pane .pane')?.paneTerm";

    /// The text of the row of history at the top of what is in sight.
    pub const TOP_ROW: &str = "(t => { const m = t._metrics(), rows = t.hist.el.querySelectorAll('.hist-row'); return rows[Math.round((t.body.scrollTop - m.histTop) / (t.hist.th * t._hs))]?.textContent; })";

    /// Counts what the terminal draws and what happens to a finger's moves,
    /// from now on: `window.seen`.
    pub const WATCH: &str = "(t => { const seen = window.seen = { drawn: 0, changed: 0, moves: [] };
        new MutationObserver(l => { seen.changed += l.length; }).observe(t.body.querySelector('.xterm-rows'), { subtree: true, childList: true, characterData: true, attributes: true });
        t.term.onRender(() => { seen.drawn += 1; });
        addEventListener('touchmove', e => seen.moves.push(e), true);
        return true; })";

    pub struct Page {
        _browser: headless_chrome::Browser,
        pub tab: std::sync::Arc<headless_chrome::Tab>,
        wire: std::sync::Mutex<Option<Wire>>,
        /// What was right-clicked last, while its menu is the one open.
        menu_of: std::sync::Mutex<Option<String>>,
    }

    /// This page's own connection to the browser: what `Page` asks of the
    /// page, and the keys it types, go over it. The `headless_chrome` crate
    /// answers a call only after a tenth of a second or two (its reader
    /// holds the connection's lock while it waits 100 ms for the browser to
    /// say something, so a call waits for the lock and its answer waits for
    /// the reader): a scenario that asked the page a hundred things, or
    /// typed twenty letters, spent ten seconds on that. Chrome allows a
    /// second connection to a tab, and on this one an answer is read as
    /// soon as it arrives. Starting the browser, navigation, screenshots
    /// and the list of tabs are still the crate's.
    struct Wire {
        socket: tokio_tungstenite::tungstenite::WebSocket<std::net::TcpStream>,
        next: u64,
    }

    impl Wire {
        fn open(browser: &headless_chrome::Browser, tab: &headless_chrome::Tab) -> Wire {
            // ws://127.0.0.1:port/devtools/browser/… is the browser's; a
            // tab's is /devtools/page/<its id> at the same address.
            let ws = browser.get_ws_url();
            let at = ws
                .strip_prefix("ws://")
                .and_then(|r| r.split('/').next())
                .unwrap_or_else(|| panic!("the browser's address: {ws}"))
                .to_string();
            let stream = std::net::TcpStream::connect(&at).unwrap();
            stream.set_nodelay(true).unwrap();
            // A page that never answers fails its test; it does not hang it.
            stream
                .set_read_timeout(Some(Duration::from_secs(120)))
                .unwrap();
            let url = format!("ws://{at}/devtools/page/{}", tab.get_target_id());
            let (socket, _) = tokio_tungstenite::tungstenite::client(url, stream)
                .unwrap_or_else(|e| panic!("the page's own connection: {e}"));
            Wire { socket, next: 0 }
        }

        /// One call of the DevTools protocol: its result, or what the
        /// browser said instead.
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            use tokio_tungstenite::tungstenite::Message;
            self.next += 1;
            let call = json!({"id": self.next, "method": method, "params": params});
            self.socket
                .send(Message::text(call.to_string()))
                .map_err(|e| e.to_string())?;
            loop {
                let m = self.socket.read().map_err(|e| e.to_string())?;
                let Message::Text(text) = m else { continue };
                let Ok(mut v) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                if v["id"] != json!(self.next) {
                    continue;
                }
                if let Some(e) = v.get("error") {
                    return Err(e["message"].as_str().unwrap_or("an error").to_string());
                }
                return Ok(v["result"].take());
            }
        }
    }

    /// A key of a US keyboard as the DevTools protocol names it: its code
    /// and number, and the text it types. The same as the `headless_chrome`
    /// crate sent for it (its table is Puppeteer's, and private), for the
    /// keys the scenarios type; another key is added here when one needs it.
    fn key_of(key: &str) -> (String, u32, String) {
        if key == "Enter" {
            return ("Enter".into(), 13, "\r".into());
        }
        let c = key.chars().next().unwrap_or(' ');
        let (code, number) = match c {
            'a'..='z' => (format!("Key{}", c.to_ascii_uppercase()), c as u32 - 32),
            '0'..='9' => (format!("Digit{c}"), c as u32),
            ' ' => ("Space".into(), 32),
            '-' => ("NumpadSubtract".into(), 109),
            '+' => ("NumpadAdd".into(), 107),
            '$' => ("Digit4".into(), 52),
            '(' => ("Digit9".into(), 57),
            ')' => ("Digit0".into(), 48),
            _ => panic!("a key the harness has no entry for: {key:?} (add it to `key_of`)"),
        };
        (code, number, key.to_string())
    }

    impl Page {
        pub fn open(s: &Station, width: u32, height: u32) -> Page {
            Page::open_as(s, width, height, true)
        }

        /// A browser that has never been given a token for this station.
        pub fn open_signed_out(s: &Station, width: u32, height: u32) -> Page {
            Page::open_as(s, width, height, false)
        }

        /// The station's deck, reached at another port of this machine:
        /// through something that stands in for the link between them.
        pub fn open_through(s: &Station, port: u16, width: u32, height: u32) -> Page {
            Page::open_at(&format!("http://127.0.0.1:{port}/"), s, width, height, true)
        }

        fn open_as(s: &Station, width: u32, height: u32, signed_in: bool) -> Page {
            Page::open_at(&s.url("/"), s, width, height, signed_in)
        }

        /// A browser at any address (a station's HTTPS deck, say), holding
        /// `token` as a browser signed in from another device holds its
        /// own. Nothing is waited for but the page's own script.
        pub fn open_as_device(at: &str, token: &str, width: u32, height: u32, spki: &str) -> Page {
            let page = Page::bare(width, height, spki);
            page.tab.navigate_to(at).unwrap();
            page.wait("the page's script", "!!document.getElementById('app')?.api");
            page.js(&format!(
                "localStorage.setItem('spandeck.token', '{token}')"
            ));
            page.tab.reload(false, None).unwrap();
            page.wait("the deck", "!!document.querySelector('.topbar')");
            page
        }

        /// `spki`: the key of the certificates the test's stations serve.
        fn bare(width: u32, height: u32, spki: &str) -> Page {
            let trust =
                std::ffi::OsString::from(format!("--ignore-certificate-errors-spki-list={spki}"));
            let path = chrome()
                .expect("no Chrome found: set SPANDECK_CHROME to a Chrome or Chromium binary");
            let browser = headless_chrome::Browser::new(
                headless_chrome::LaunchOptions::default_builder()
                    .path(Some(path))
                    .window_size(Some((width, height)))
                    .sandbox(false)
                    // The stations' certificates are made in the test:
                    // theirs are taken, by their key, and no other.
                    .ignore_certificate_errors(false)
                    .args(vec![trust.as_os_str()])
                    .idle_browser_timeout(Duration::from_secs(120))
                    .build()
                    .unwrap(),
            )
            .unwrap();
            let tab = (0..3)
                .find_map(|_| browser.new_tab().ok())
                .expect("the browser gave no tab in three tries");
            Page {
                _browser: browser,
                tab,
                wire: std::sync::Mutex::new(None),
                menu_of: std::sync::Mutex::new(None),
            }
        }

        fn open_at(at: &str, s: &Station, width: u32, height: u32, signed_in: bool) -> Page {
            let path = chrome()
                .expect("no Chrome found: set SPANDECK_CHROME to a Chrome or Chromium binary");
            let browser = headless_chrome::Browser::new(
                headless_chrome::LaunchOptions::default_builder()
                    .path(Some(path))
                    .window_size(Some((width, height)))
                    .sandbox(false)
                    .idle_browser_timeout(Duration::from_secs(120))
                    .build()
                    .unwrap(),
            )
            .unwrap();
            // A browser that has only just started may not answer in the
            // library's own wait (seen once on a CI Mac runner): asked again.
            let tab = (0..3)
                .find_map(|_| browser.new_tab().ok())
                .expect("the browser gave no tab in three tries");
            let page = Page {
                _browser: browser,
                tab,
                wire: std::sync::Mutex::new(None),
                menu_of: std::sync::Mutex::new(None),
            };
            if signed_in {
                page.tab
                    .navigate_to(&format!("{at}#token={}", s.token()))
                    .unwrap();
                page.wait("the deck", "!!document.querySelector('.topbar')");
            } else {
                page.tab.navigate_to(at).unwrap();
                page.wait("the sign-in page", "!!document.querySelector('main.login')");
            }
            page
        }

        /// The window this page opened at an address holding `at`, once
        /// the browser has it.
        pub fn opened(&self, at: &str) -> Page {
            let tab = wait_for("the window the page opened", 20, || {
                let tabs = self._browser.get_tabs().lock().unwrap().clone();
                tabs.into_iter()
                    .find(|t| !std::sync::Arc::ptr_eq(t, &self.tab) && t.get_url().contains(at))
            });
            Page {
                _browser: self._browser.clone(),
                tab,
                wire: std::sync::Mutex::new(None),
                menu_of: std::sync::Mutex::new(None),
            }
        }

        /// The pane on screen on a computer, as its terminal. An
        /// agent's pane opens as its conversation ("Chat default for
        /// agents terminal for terminals", the owner, 16 Nov 2026), and
        /// "Terminal" on its bar is one click.
        pub fn terminal_face(&self) {
            self.wait(
                "the pane as its terminal",
                "(b => b ? (b.click(), false) : !!document.querySelector('.pane .xterm-rows'))([...document.querySelectorAll('.chat-pane .chat-switch button')].find(b => b.innerText === 'Terminal'))",
            );
        }

        pub fn js(&self, expr: &str) -> Value {
            // Values come back as JSON text, so arrays and objects survive.
            let wrapped = format!("(async () => JSON.stringify(await ({expr})))()");
            let params = json!({"expression": wrapped, "awaitPromise": true});
            match self.call("Runtime.evaluate", params) {
                // A page that threw has no value: null.
                Ok(v) => v["result"]["value"]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(Value::Null),
                // The page went elsewhere while it was asked (a sign-in that
                // loads it again, say): there is no answer, as from a page
                // that threw, and a `wait` asks the new page.
                Err(e) if e.contains("navigated or closed") || e.contains("context") => Value::Null,
                Err(e) => panic!("js failed: {e}\n{expr}"),
            }
        }

        /// One call of the DevTools protocol, for what `js` cannot do (the
        /// size of the window as a device has it).
        pub fn protocol(&self, method: &str, params: Value) -> Value {
            self.call(method, params)
                .unwrap_or_else(|e| panic!("{method} failed: {e}"))
        }

        fn call(&self, method: &str, params: Value) -> Result<Value, String> {
            let mut wire = self.wire.lock().unwrap();
            wire.get_or_insert_with(|| Wire::open(&self._browser, &self.tab))
                .call(method, params)
        }

        /// Press and let go of one key: a letter, a digit, a sign, "Enter".
        pub fn press_key(&self, key: &str) {
            let (code, number, text) = key_of(key);
            for kind in ["keyDown", "keyUp"] {
                self.call(
                    "Input.dispatchKeyEvent",
                    json!({"type": kind, "key": key, "text": text, "code": code,
                        "windowsVirtualKeyCode": number, "nativeVirtualKeyCode": number}),
                )
                .unwrap_or_else(|e| panic!("the key {key:?}: {e}"));
            }
        }

        /// From now on this page is as large as a phone's screen, whatever
        /// the browser's window. (A full Chrome, as the CI runners have, is
        /// never narrower than 500 px and gives the page less than the
        /// height asked for: "390 × 844" was 500 × 644 there, and a
        /// terminal fitted to it had 63 columns where a test expected
        /// fewer than 60.)
        pub fn phone_screen(&self) {
            self.protocol(
                "Emulation.setDeviceMetricsOverride",
                json!({"width": 390, "height": 844, "deviceScaleFactor": 1, "mobile": false}),
            );
            self.wait(
                "a phone's screen",
                "innerWidth === 390 && innerHeight === 844",
            );
        }

        /// From now on this page has a touch screen.
        pub fn touches(&self) {
            self.protocol(
                "Emulation.setTouchEmulationEnabled",
                json!({"enabled": true, "maxTouchPoints": 1}),
            );
        }

        /// A finger put down so that its way has the middle of `el` (an
        /// expression for an element) at its own middle, moved by (dx, dy)
        /// in `steps` moves and lifted; with no way to go, a tap. These are
        /// the browser's own touches (`touches` first): it scrolls with
        /// them, or hands them to the page, as it would a finger's. An
        /// event the page makes up itself scrolls nothing. The finger
        /// stays on the element: a way longer than the element is cut to
        /// what fits (a finger put down beside a terminal is not a drag
        /// over it, which is what happened on a screen shorter than the
        /// one the test was written on).
        pub fn touch_drag(&self, el: &str, dx: f64, dy: f64, steps: u32) {
            let at = self.js(&format!(
                "(r => [r.left + r.width / 2, r.top + r.height / 2, r.width, r.height])({el}.getBoundingClientRect())"
            ));
            let fits = |way: f64, room: f64| {
                if way.abs() > room - 16.0 {
                    (room - 16.0).max(0.0) / way.abs()
                } else {
                    1.0
                }
            };
            let part = fits(dx, at[2].as_f64().unwrap()).min(fits(dy, at[3].as_f64().unwrap()));
            let (dx, dy) = (dx * part, dy * part);
            let (x, y) = (
                at[0].as_f64().unwrap() - dx / 2.0,
                at[1].as_f64().unwrap() - dy / 2.0,
            );
            let point = |i: u32| {
                let part = f64::from(i) / f64::from(steps);
                json!([{"x": x + dx * part, "y": y + dy * part}])
            };
            self.protocol(
                "Input.dispatchTouchEvent",
                json!({"type": "touchStart", "touchPoints": point(0)}),
            );
            for i in 1..=steps {
                self.protocol(
                    "Input.dispatchTouchEvent",
                    json!({"type": "touchMove", "touchPoints": point(i)}),
                );
                std::thread::sleep(Duration::from_millis(12));
            }
            self.protocol(
                "Input.dispatchTouchEvent",
                json!({"type": "touchEnd", "touchPoints": []}),
            );
        }

        /// Type `text` on the keyboard, a key at a time, into whatever has
        /// the keyboard.
        pub fn type_str(&self, text: &str) {
            for c in text.chars() {
                self.press_key(&c.to_string());
            }
        }

        pub fn wait(&self, what: &str, expr: &str) -> Value {
            wait_for_or(
                what,
                20,
                || self.showed(),
                || {
                    let v = self.js(expr);
                    (!v.is_null() && v != json!(false) && v != json!("")).then_some(v)
                },
            )
        }

        /// Where the page is and the start of what it shows, for a wait
        /// that ran out: several of those were reported with nothing to
        /// find their cause by.
        fn showed(&self) -> String {
            let seen = "JSON.stringify([location.href.replace(/token=[^&]*/, 'token=…'), document.body ? document.body.innerText.slice(0, 1500) : ''])";
            let params = json!({"expression": seen});
            match self.call("Runtime.evaluate", params) {
                Ok(v) => match serde_json::from_str::<Value>(
                    v["result"]["value"].as_str().unwrap_or("[]"),
                ) {
                    Ok(v) => format!(
                        "the page was at {} and showed:\n{}",
                        v[0].as_str().unwrap_or("?"),
                        v[1].as_str().unwrap_or("")
                    ),
                    Err(_) => "the page did not say what it showed".to_string(),
                },
                Err(e) => format!("the page did not answer: {e}"),
            }
        }

        pub fn goto(&self, hash: &str) {
            self.js(&format!("location.hash = {}", json!(hash)));
        }

        /// Load the page again, and come back once the page that was there
        /// is gone: what is asked next is asked of the new one. (A reload
        /// takes a moment to begin; until it does, the old page still
        /// answers, and a click on it is lost with it.)
        pub fn reload(&self) {
            self.js("(() => { window.__before = true; location.reload(); return true; })()");
            self.wait(
                "the page after a reload",
                "!window.__before && !!document.body",
            );
        }

        /// Open another station's Control Deck in the same browser.
        pub fn visit(&self, s: &Station) {
            self.tab
                .navigate_to(&format!("{}#token={}", s.url("/"), s.token()))
                .unwrap();
            self.wait(
                "the other station's deck",
                &format!(
                    "location.port === '{}' && !!document.querySelector('.topbar') && !location.hash.includes('token')",
                    s.port
                ),
            );
        }

        /// Make the page redraw for new data, as it does every few seconds,
        /// and come back once it has: a change to `pane` is what it hears of.
        pub fn redraw(&self, s: &Station, pane: &str) {
            let draws = "Number(document.getElementById('app').dataset.draws || 0)";
            let before = self.js(draws).as_u64().unwrap_or(0);
            let muted = s.pane(pane)["muted"] == json!(true);
            s.call("pane.set", json!({"pane": pane, "muted": !muted}));
            self.wait("a redraw", &format!("{draws} > {before}"));
            // What a redraw does to the keyboard, it does a moment later.
            std::thread::sleep(Duration::from_millis(150));
        }

        /// Right-click the element `el` (a JS expression) and wait for its menu.
        /// The page closes a menu whenever it is drawn again, which it is
        /// for every change it hears of (the change the step before made,
        /// say): a menu that went that way before it was seen is opened
        /// again. Three runs in sixteen failed here on two processors once
        /// the page was asked without the tenth of a second between.
        pub fn right_click(&self, el: &str) {
            let began = Instant::now();
            loop {
                self.wait(
                    "something to right-click",
                    &format!("(el => el ? (el.dispatchEvent(new MouseEvent('contextmenu', {{bubbles: true, cancelable: true, clientX: 300, clientY: 200}})), true) : false)({el})"),
                );
                let asked = Instant::now();
                let menu = || self.js("!!document.querySelector('.menu')") == json!(true);
                while !menu() && asked.elapsed() < Duration::from_secs(1) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                if menu() {
                    break;
                }
                assert!(
                    began.elapsed() < Duration::from_secs(20),
                    "timed out waiting for the menu\n{}",
                    self.showed()
                );
            }
            *self.menu_of.lock().unwrap() = Some(el.to_string());
        }

        /// Choose the item of the open menu whose label starts with `label`.
        /// A menu that a redraw took away between the right-click and this
        /// is opened again; a menu that is there without the item fails at
        /// once.
        pub fn choose(&self, label: &str) {
            let began = Instant::now();
            loop {
                let done = self.js(&format!(
                    "(i => i && !i.disabled ? (i.click(), true) : [...document.querySelectorAll('.menu-item')].map(x => x.innerText))([...document.querySelectorAll('.menu-item')].find(i => i.innerText.startsWith({})))",
                    json!(label)
                ));
                let opener = self.menu_of.lock().unwrap().take();
                if done == json!(true) {
                    return;
                }
                match opener {
                    Some(el) if done == json!([]) && began.elapsed() < Duration::from_secs(20) => {
                        self.right_click(&el)
                    }
                    _ => assert_eq!(done, json!(true), "no menu item {label:?} to choose"),
                }
            }
        }

        /// The text on a pane's terminal as the browser shows it.
        pub fn term_text(&self) -> String {
            self.js("[...document.querySelectorAll('.pane .xterm-rows')].map(r => r.innerText).join('\\n----\\n')").as_str().unwrap_or("").to_string()
        }
    }
}

/// A link that can be stalled or slowed, and a Control Deck's socket as a
/// browser tab holds one: what the scenario tests of the deck's connection
/// share (heartbeat.rs, typed.rs).
pub mod wire {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    /// A link that can go half-open, as the audit's proxy did: while it is
    /// stalled nothing passes in either direction and nothing is closed (what
    /// was on its way waits, and is delivered when the link returns; a new
    /// connection is taken and then hears nothing). It can also be slow: every
    /// piece is held `delay` ms, each way, without holding up the one behind it
    /// for longer than that. A request is passed on under the station's own
    /// address (a station serves only the hosts that are its own).
    pub struct Link {
        pub port: u16,
        delay: Arc<AtomicU64>,
        stalled: Arc<AtomicBool>,
    }

    impl Link {
        pub fn to(station: u16) -> Link {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (delay, stalled) = (
                Arc::new(AtomicU64::new(0)),
                Arc::new(AtomicBool::new(false)),
            );
            let (d, s) = (delay.clone(), stalled.clone());
            std::thread::spawn(move || {
                for from in listener.incoming().flatten() {
                    let Ok(onward) = TcpStream::connect(("127.0.0.1", station)) else {
                        continue;
                    };
                    let _ = (from.set_nodelay(true), onward.set_nodelay(true));
                    let (here, there) = (
                        format!("127.0.0.1:{port}").into_bytes(),
                        format!("127.0.0.1:{station}").into_bytes(),
                    );
                    let pairs = [
                        (from.try_clone().unwrap(), onward.try_clone().unwrap(), true),
                        (onward, from, false),
                    ];
                    for (mut src, mut dst, outward) in pairs {
                        let (here, there) = (here.clone(), there.clone());
                        let (delay, stalled) = (d.clone(), s.clone());
                        let (tx, rx) = mpsc::channel::<(Instant, Vec<u8>)>();
                        std::thread::spawn(move || {
                            let mut buf = [0u8; 32 * 1024];
                            while let Ok(n) = src.read(&mut buf) {
                                if n == 0 {
                                    break;
                                }
                                let due = Instant::now()
                                    + Duration::from_millis(delay.load(Ordering::SeqCst));
                                let mut bytes = buf[..n].to_vec();
                                // Only a request's head holds the address as
                                // text: what a WebSocket client sends is masked.
                                while let Some(at) = bytes
                                    .windows(here.len())
                                    .position(|w| outward && w == here.as_slice())
                                {
                                    bytes.splice(at..at + here.len(), there.iter().copied());
                                }
                                if tx.send((due, bytes)).is_err() {
                                    break;
                                }
                            }
                        });
                        std::thread::spawn(move || {
                            for (due, bytes) in rx {
                                std::thread::sleep(due.saturating_duration_since(Instant::now()));
                                while stalled.load(Ordering::SeqCst) {
                                    std::thread::sleep(Duration::from_millis(5));
                                }
                                if dst.write_all(&bytes).is_err() {
                                    break;
                                }
                            }
                            let _ = dst.shutdown(std::net::Shutdown::Both);
                        });
                    }
                }
            });
            Link {
                port,
                delay,
                stalled,
            }
        }

        pub fn stall(&self, on: bool) {
            self.stalled.store(on, Ordering::SeqCst);
        }

        pub fn slow(&self, ms: u64) {
            self.delay.store(ms, Ordering::SeqCst);
        }
    }

    type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// What a page has been sent so far.
    #[derive(Default)]
    pub struct Seen {
        pub answers: Vec<(u64, Result<Value, Value>)>,
        pub output: Vec<(u64, Vec<u8>)>,
        /// Messages on a channel that are not output.
        pub said: Vec<Value>,
        pub pongs: Vec<Value>,
        pub closed: bool,
    }

    /// A signed-in Control Deck, as a browser tab is one.
    pub struct Deck {
        sink: tokio::sync::Mutex<futures_util::stream::SplitSink<Socket, Message>>,
        pub seen: Arc<Mutex<Seen>>,
        pub hello: Value,
        id: AtomicU64,
    }

    impl Deck {
        pub async fn open(s: &Station, port: u16, client: &str) -> Deck {
            Deck::open_as(s, port, json!({"client": client, "caps": ["busy"]})).await
        }

        /// Signed in with what `auth` says besides the token: the client id,
        /// and what a page of today adds (`instance`, `caps`).
        pub async fn open_as(s: &Station, port: u16, mut auth: Value) -> Deck {
            let url = format!("ws://127.0.0.1:{port}/api/ws");
            let (ws, _) = tokio_tungstenite::connect_async_with_config(url, None, true)
                .await
                .unwrap();
            let (mut sink, mut stream) = ws.split();
            auth["token"] = json!(s.token());
            let auth = json!({"auth": auth});
            sink.send(Message::Text(auth.to_string().into()))
                .await
                .unwrap();
            let hello = match stream.next().await {
                Some(Ok(Message::Text(t))) => serde_json::from_str::<Value>(&t).unwrap(),
                other => panic!("no hello: {other:?}"),
            };
            assert_eq!(hello["auth"]["ok"], json!(true), "{hello}");
            let seen: Arc<Mutex<Seen>> = Arc::default();
            let into = seen.clone();
            tokio::spawn(async move {
                while let Some(Ok(m)) = stream.next().await {
                    let mut seen = into.lock().unwrap();
                    match m {
                        Message::Binary(b) if b.len() >= 5 => {
                            let chan = u32::from_le_bytes([b[1], b[2], b[3], b[4]]) as u64;
                            seen.output.push((chan, b[5..].to_vec()));
                        }
                        Message::Text(t) => {
                            let v: Value = serde_json::from_str(&t).unwrap();
                            match v["id"].as_u64() {
                                Some(id) => {
                                    let answer = match v.get("error") {
                                        Some(e) => Err(e.clone()),
                                        None => Ok(v["result"].clone()),
                                    };
                                    seen.answers.push((id, answer));
                                }
                                None if v.get("pong").is_some() => seen.pongs.push(v),
                                None if v.get("chan").is_some() => seen.said.push(v),
                                None => {}
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
                into.lock().unwrap().closed = true;
            });
            Deck {
                sink: tokio::sync::Mutex::new(sink),
                seen,
                hello,
                id: AtomicU64::new(0),
            }
        }

        pub async fn raw(&self, text: Value) {
            let mut sink = self.sink.lock().await;
            let _ = sink.send(Message::Text(text.to_string().into())).await;
        }

        /// Sent exactly as written (a ping is known by how it begins).
        pub async fn text(&self, text: String) {
            let mut sink = self.sink.lock().await;
            let _ = sink.send(Message::Text(text.into())).await;
        }

        /// What was said on a channel of things typed that were not taken.
        pub fn dropped(&self, chan: u64) -> Vec<Value> {
            let seen = self.seen.lock().unwrap();
            seen.said
                .iter()
                .filter(|v| v["chan"] == json!(chan) && v.get("input_dropped").is_some())
                .cloned()
                .collect()
        }

        pub async fn ask(&self, method: &str, params: Value) -> u64 {
            let id = 1 + self.id.fetch_add(1, Ordering::SeqCst);
            self.raw(json!({"id": id, "method": method, "params": params}))
                .await;
            id
        }

        pub async fn until<T>(&self, what: &str, secs: u64, f: impl Fn(&Seen) -> Option<T>) -> T {
            let began = Instant::now();
            loop {
                if let Some(v) = f(&self.seen.lock().unwrap()) {
                    return v;
                }
                assert!(began.elapsed() < Duration::from_secs(secs), "no {what}");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }

        pub async fn answer(&self, id: u64) -> Result<Value, Value> {
            self.until(&format!("answer to request {id}"), 30, |seen| {
                seen.answers
                    .iter()
                    .find(|(i, _)| *i == id)
                    .map(|(_, a)| a.clone())
            })
            .await
        }

        pub async fn attach(&self, params: Value) -> Result<u64, Value> {
            let id = self.ask("pane.attach", params).await;
            self.answer(id).await.map(|v| v["chan"].as_u64().unwrap())
        }

        pub async fn key(&self, chan: u64, data: &str) {
            self.raw(
                json!({"id": null, "method": "pane.input", "params": {"chan": chan, "data": data}}),
            )
            .await;
        }

        /// Wait until the pane's output on this channel has shown `needle`.
        pub async fn shows(&self, chan: u64, needle: &str) {
            self.until(&format!("{needle} on channel {chan}"), 20, |seen| {
                let all: Vec<u8> = seen
                    .output
                    .iter()
                    .filter(|(c, _)| *c == chan)
                    .flat_map(|(_, b)| b.clone())
                    .collect();
                String::from_utf8_lossy(&all).contains(needle).then_some(())
            })
            .await
        }

        /// What was said on a channel that it is unreachable, if anything.
        pub fn unreachable(&self, chan: u64) -> Option<Value> {
            unreachable_in(&self.seen.lock().unwrap(), chan)
        }
    }

    pub fn unreachable_in(seen: &Seen, chan: u64) -> Option<Value> {
        seen.said
            .iter()
            .find(|v| v["chan"] == json!(chan) && v["unreachable"] == json!(true))
            .cloned()
    }
}

/// A stand-in for AgentVault that follows the contract in
/// docs/specs/spandeck-agentvault.md, for scenario tests.
pub mod fake_vault {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, Method, StatusCode};
    use axum::response::{Html, IntoResponse};
    use axum::routing::{any, get, post, put};
    use axum::{Json, Router};
    use serde_json::{json, Value};

    #[derive(Default)]
    pub struct Tok {
        pub station: String,
        pub rotation: Option<u64>,
        pub retired: bool,
        pub overdue: bool,
        /// The owner untied the token from its machine at the vault.
        pub unpinned: bool,
        /// A rotation's new token that has not been used yet.
        pub unused_new: Option<String>,
        /// The token this one replaces once it is first used.
        pub replaces: Option<String>,
    }

    #[derive(Default)]
    pub struct Inner {
        pub unlocked: bool,
        pub service: bool,
        pub requests: HashMap<String, (String, String, String, bool)>, // id -> (code, station, status, collected)
        pub tokens: HashMap<String, Tok>,
        pub calls: Vec<Value>,
        pub reports: Vec<String>,
        pub issued: Vec<String>,
        /// Every token any caller has shown this vault, at whatever address.
        pub presented: Vec<String>,
        pub port: u16,
        /// What the vault says its version is, when a test wants to choose.
        pub version: Option<String>,
        n: u32,
    }

    #[derive(Clone)]
    pub struct FakeVault {
        pub port: u16,
        pub inner: Arc<Mutex<Inner>>,
        /// Set when the vault has moved away from this address.
        gone: Arc<std::sync::atomic::AtomicBool>,
    }

    type S = Arc<Mutex<Inner>>;

    fn bearer(h: &HeaderMap) -> String {
        h.get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("")
            .to_string()
    }

    /// Ok(token) when it may be used. Using a rotation's new token retires the old one.
    fn auth(
        s: &mut Inner,
        h: &HeaderMap,
        allow_overdue: bool,
    ) -> Result<String, (StatusCode, Json<Value>)> {
        let t = bearer(h);
        s.presented.push(t.clone());
        let Some(tok) = s.tokens.get(&t) else {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unknown token"})),
            ));
        };
        if tok.retired {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "token was replaced", "code": "token_replaced"})),
            ));
        }
        if tok.overdue && !allow_overdue {
            return Err((
                StatusCode::FORBIDDEN,
                Json(
                    json!({"error": "this token is overdue for rotation; rotate it first", "code": "token_overdue"}),
                ),
            ));
        }
        if let Some(old) = tok.replaces.clone() {
            if let Some(o) = s.tokens.get_mut(&old) {
                o.retired = true;
                o.unused_new = None;
            }
            s.tokens.get_mut(&t).unwrap().replaces = None;
        }
        Ok(t)
    }

    async fn status(State(s): State<S>) -> Json<Value> {
        let s = s.lock().unwrap();
        if s.service {
            Json(
                json!({"service": "agentvault", "version": s.version.as_deref().unwrap_or("0.1.0-fake"), "initialized": true, "sealed": !s.unlocked}),
            )
        } else {
            Json(json!({"version": "something else"}))
        }
    }

    async fn open_request(State(s): State<S>, Json(body): Json<Value>) -> Json<Value> {
        let mut s = s.lock().unwrap();
        s.n += 1;
        let id = format!("req{}", s.n);
        let code = format!("code-{}-secret", s.n);
        let station = body["station"].as_str().unwrap_or("?").to_string();
        // One pending request per station: a new one replaces it.
        s.requests
            .retain(|_, r| !(r.1 == station && r.2 == "pending"));
        s.requests
            .insert(id.clone(), (code.clone(), station, "pending".into(), false));
        Json(
            json!({"request_id": id, "approve_url": format!("http://127.0.0.1:{}/#/approve/{id}", s.port), "collect_code": code, "expires_at": 0}),
        )
    }

    async fn approve_page(Path(id): Path<String>, State(s): State<S>) -> impl IntoResponse {
        let s = s.lock().unwrap();
        let station = s.requests.get(&id).map(|r| r.1.clone()).unwrap_or_default();
        Html(format!("<!doctype html><title>AgentVault</title><h1>Approve a token for station {station}?</h1><p id=req>{id}</p>"))
    }

    async fn collect(
        Path(id): Path<String>,
        State(s): State<S>,
        h: HeaderMap,
    ) -> (StatusCode, Json<Value>) {
        let mut s = s.lock().unwrap();
        let code = h
            .get("x-collect-code")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let Some(req) = s.requests.get(&id).cloned() else {
            return (StatusCode::NOT_FOUND, Json(json!({"status": "expired"})));
        };
        if req.0 != code {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "wrong collect code"})),
            );
        }
        match (req.2.as_str(), req.3) {
            ("approved", false) => {
                s.n += 1;
                let token = format!("avt_station_{}_{}", req.1, s.n);
                // Approving replaces that station's earlier token.
                for t in s.tokens.values_mut().filter(|t| t.station == req.1) {
                    t.retired = true;
                }
                s.tokens.insert(
                    token.clone(),
                    Tok {
                        station: req.1.clone(),
                        ..Default::default()
                    },
                );
                s.issued.push(token.clone());
                s.requests.get_mut(&id).unwrap().3 = true;
                (
                    StatusCode::OK,
                    Json(
                        json!({"token": token, "scopes": ["list", "proxy"], "secrets": ["*"], "proxy_get_only": true}),
                    ),
                )
            }
            ("approved", true) => (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "no such request; it may have expired, so ask again"})),
            ),
            (other, _) => (StatusCode::OK, Json(json!({"status": other}))),
        }
    }

    async fn token_self(State(s): State<S>, h: HeaderMap) -> (StatusCode, Json<Value>) {
        let mut s = s.lock().unwrap();
        match auth(&mut s, &h, false) {
            Ok(t) => {
                let tok = &s.tokens[&t];
                (
                    StatusCode::OK,
                    Json(
                        json!({"name": format!("spandeck:{}", tok.station), "nodes": if tok.unpinned { vec![] } else { vec!["this-machine"] },
                            "scopes": ["list", "proxy"], "proxy_get_only": true, "rotation": {"every_hours": tok.rotation}}),
                    ),
                )
            }
            Err(e) => e,
        }
    }

    async fn rotate(State(s): State<S>, h: HeaderMap) -> (StatusCode, Json<Value>) {
        let mut s = s.lock().unwrap();
        let t = match auth(&mut s, &h, true) {
            Ok(t) => t,
            Err(e) => return e,
        };
        s.n += 1;
        let (station, rotation) = (s.tokens[&t].station.clone(), s.tokens[&t].rotation);
        let new = format!("avt_station_{station}_{}", s.n);
        // A repeat swap with the old token discards the unused new one.
        if let Some(prev) = s.tokens.get_mut(&t).unwrap().unused_new.take() {
            s.tokens.remove(&prev);
        }
        s.tokens.insert(
            new.clone(),
            Tok {
                station,
                rotation,
                replaces: Some(t.clone()),
                ..Default::default()
            },
        );
        let old = s.tokens.get_mut(&t).unwrap();
        old.unused_new = Some(new.clone());
        old.overdue = false;
        s.issued.push(new.clone());
        (StatusCode::OK, Json(json!({"token": new})))
    }

    async fn set_rotation(
        State(s): State<S>,
        h: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let mut s = s.lock().unwrap();
        let t = match auth(&mut s, &h, false) {
            Ok(t) => t,
            Err(e) => return e,
        };
        let hours = body["every_hours"].as_u64();
        if hours == Some(0) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "minimum 1 hour"})),
            );
        }
        s.tokens.get_mut(&t).unwrap().rotation = hours;
        (StatusCode::OK, Json(json!({"every_hours": hours})))
    }

    async fn secrets(State(s): State<S>, h: HeaderMap) -> (StatusCode, Json<Value>) {
        let mut s = s.lock().unwrap();
        if !s.unlocked {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    json!({"error": "vault is sealed: sign in to the web interface to unlock it"}),
                ),
            );
        }
        if let Err(e) = auth(&mut s, &h, false) {
            return e;
        }
        (
            StatusCode::OK,
            Json(json!({"secrets": [
                {"name": "OPENAI_API_KEY", "description": "OpenAI key for the demo project", "provider": "openai", "risk": "medium"},
                {"name": "OPENAI_ORG", "description": "OpenAI organisation id", "provider": "openai", "risk": "low"},
                {"name": "GITHUB_TOKEN", "description": "GitHub token", "provider": "github", "risk": "high"}
            ]})),
        )
    }

    async fn secret(
        Path(name): Path<String>,
        State(s): State<S>,
        h: HeaderMap,
    ) -> (StatusCode, Json<Value>) {
        let mut s = s.lock().unwrap();
        if let Err(e) = auth(&mut s, &h, false) {
            return e;
        }
        (
            StatusCode::OK,
            Json(
                json!({"name": name, "description": "a key", "provider": "openai", "risk": "medium"}),
            ),
        )
    }

    async fn proxy(
        Path((name, path)): Path<(String, String)>,
        State(s): State<S>,
        method: Method,
        h: HeaderMap,
        body: String,
    ) -> (StatusCode, Json<Value>) {
        let mut s = s.lock().unwrap();
        if !s.unlocked {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    json!({"error": "vault is sealed: sign in to the web interface to unlock it"}),
                ),
            );
        }
        let t = match auth(&mut s, &h, false) {
            Ok(t) => t,
            Err(e) => return e,
        };
        let actor = h
            .get("x-agentvault-actor")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        s.calls.push(json!({"name": name, "path": path, "method": method.as_str(), "actor": actor, "token": t, "body": body}));
        (
            StatusCode::OK,
            Json(json!({"provider_says": "ok", "data": [{"id": "model-a"}], "key_used": name})),
        )
    }

    async fn report(
        State(s): State<S>,
        h: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let mut s = s.lock().unwrap();
        if let Err(e) = auth(&mut s, &h, false) {
            return e;
        }
        s.reports
            .push(body["value"].as_str().unwrap_or("").to_string());
        // Always the same reply: a report never says whether it matched.
        (StatusCode::ACCEPTED, Json(json!({"received": true})))
    }

    impl FakeVault {
        pub fn start() -> FakeVault {
            FakeVault::serve(
                Arc::new(Mutex::new(Inner {
                    unlocked: true,
                    service: true,
                    ..Default::default()
                })),
                0,
            )
        }

        /// The vault moves to another port, as when its machine is renamed:
        /// the same vault, with the tokens it issued. Nothing answers as a
        /// vault at the old address any more.
        pub fn moved(&self, port: u16) -> FakeVault {
            self.gone.store(true, std::sync::atomic::Ordering::SeqCst);
            FakeVault::serve(self.inner.clone(), port)
        }

        fn serve(inner: S, port: u16) -> FakeVault {
            let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            inner.lock().unwrap().port = port;
            let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let left = gone.clone();
            let app = Router::new()
                .route("/v1/status", get(status))
                .route("/v1/token-requests", post(open_request))
                .route("/v1/token-requests/{id}", get(collect))
                .route("/approve/{id}", get(approve_page))
                .route("/v1/tokens/self", get(token_self))
                .route("/v1/tokens/self/rotate", post(rotate))
                .route("/v1/tokens/self/rotation", put(set_rotation))
                .route("/v1/secrets", get(secrets))
                .route("/v1/secrets/{name}", get(secret))
                .route("/v1/proxy/{name}/{*path}", any(proxy))
                .route("/v1/attention/report", post(report))
                .layer(axum::middleware::from_fn(
                    move |req: axum::extract::Request, next: axum::middleware::Next| {
                        let left = left.clone();
                        async move {
                            if left.load(std::sync::atomic::Ordering::SeqCst) {
                                StatusCode::NOT_FOUND.into_response()
                            } else {
                                next.run(req).await
                            }
                        }
                    },
                ))
                .with_state(inner.clone());
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    let _ = axum::serve(listener, app).await;
                });
            });
            FakeVault { port, inner, gone }
        }

        pub fn url(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        /// Whether anything answers as a vault at this address.
        pub fn answer(&self, yes: bool) {
            self.gone.store(!yes, std::sync::atomic::Ordering::SeqCst);
        }

        /// The config lines that point a station at this vault.
        pub fn config(&self) -> String {
            format!("\n[vault]\nurl = \"{}\"\n", self.url())
        }

        /// Config for a station that looks for its vault, as one on a
        /// tailnet does, among the addresses given.
        pub fn search(ports: &[u16]) -> String {
            let list: Vec<String> = ports
                .iter()
                .map(|p| format!("\"http://127.0.0.1:{p}\""))
                .collect();
            format!("\n[vault]\nsearch = [{}]\n", list.join(", "))
        }

        /// The owner approves (or denies) every pending request on the vault's page.
        pub fn decide(&self, approve: bool) -> usize {
            let mut s = self.inner.lock().unwrap();
            let mut n = 0;
            for r in s.requests.values_mut().filter(|r| r.2 == "pending") {
                r.2 = if approve {
                    "approved".into()
                } else {
                    "denied".into()
                };
                n += 1;
            }
            n
        }

        pub fn calls(&self) -> Vec<Value> {
            self.inner.lock().unwrap().calls.clone()
        }
        pub fn reports(&self) -> Vec<String> {
            self.inner.lock().unwrap().reports.clone()
        }
        pub fn issued(&self) -> Vec<String> {
            self.inner.lock().unwrap().issued.clone()
        }
        pub fn set_unlocked(&self, unlocked: bool) {
            self.inner.lock().unwrap().unlocked = unlocked;
        }
        /// The live (not retired) tokens of a station.
        pub fn live_tokens(&self, station: &str) -> Vec<String> {
            let s = self.inner.lock().unwrap();
            let mut v: Vec<String> = s
                .tokens
                .iter()
                .filter(|(_, t)| t.station == station && !t.retired)
                .map(|(k, _)| k.clone())
                .collect();
            v.sort();
            v
        }
        pub fn with_tokens(&self, station: &str, f: impl Fn(&mut Tok)) {
            let mut s = self.inner.lock().unwrap();
            for t in s
                .tokens
                .values_mut()
                .filter(|t| t.station == station && !t.retired)
            {
                f(t);
            }
        }
    }
}

/// A stand-in for the project's release page: one release, holding the same
/// binary the tests run, so an update can be exercised end to end.
#[allow(dead_code)]
pub mod fake_releases {
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::extract::{Path, State};
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use sha2::{Digest, Sha256};

    pub const TAG: &str = "v9.9.9";

    pub struct Releases {
        pub port: u16,
    }

    impl Releases {
        pub fn url(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        /// `honest`: whether the published checksum matches the file.
        pub fn start(honest: bool) -> Releases {
            let target = format!(
                "{}-{}",
                std::env::consts::ARCH,
                if cfg!(target_os = "macos") {
                    "apple-darwin"
                } else {
                    "unknown-linux-gnu"
                }
            );
            let name = format!("spandeck-{TAG}-{target}");
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join(&name)).unwrap();
            // A link, not a copy: the binary is large.
            let bin = dir.path().join(&name).join("spandeck");
            if std::fs::hard_link(super::SPANDECK, &bin).is_err() {
                std::fs::copy(super::SPANDECK, &bin).unwrap();
            }
            // A release carries the short command too; the install script
            // expects it. A second link, which tar stores as a link.
            std::fs::hard_link(&bin, dir.path().join(&name).join("sd")).unwrap();
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("tar -cf - {name} | gzip -1 > {name}.tar.gz"))
                .current_dir(dir.path())
                .status()
                .unwrap();
            assert!(out.success());
            let archive = std::fs::read(dir.path().join(format!("{name}.tar.gz"))).unwrap();
            let mut sum: String = Sha256::digest(&archive)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if !honest {
                sum = sum.chars().rev().collect();
            }
            let mut files: HashMap<String, Vec<u8>> = HashMap::new();
            files.insert(
                format!("{name}.tar.gz.sha256"),
                format!("{sum}  {name}.tar.gz\n").into_bytes(),
            );
            files.insert(format!("{name}.tar.gz"), archive);
            let files = Arc::new(files);

            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let app = Router::new()
                .route(
                    "/latest",
                    get(|| async {
                        (
                            StatusCode::FOUND,
                            [(header::LOCATION, format!("/tag/{TAG}"))],
                        )
                    }),
                )
                .route("/tag/{tag}", get(|| async { "a release" }))
                .route(
                    "/download/{tag}/{file}",
                    get(
                        |State(files): State<Arc<HashMap<String, Vec<u8>>>>,
                         Path((tag, file)): Path<(String, String)>| async move {
                            match files.get(&file).filter(|_| tag == TAG) {
                                Some(bytes) => bytes.clone().into_response(),
                                None => StatusCode::NOT_FOUND.into_response(),
                            }
                        },
                    ),
                )
                .with_state(files);
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    let _ = axum::serve(listener, app).await;
                });
            });
            Releases { port }
        }
    }
}

/// A stand-in for the public model price list: the fixture catalogue, served
/// on localhost. No test reaches the real one.
pub mod fake_prices {
    use std::io::{Read, Write};

    pub const CATALOGUE: &str = include_str!("../../../core/fixtures/prices/models-dev.json");

    pub struct Prices {
        pub port: u16,
    }

    impl Prices {
        pub fn url(&self) -> String {
            format!("http://127.0.0.1:{}/api.json", self.port)
        }

        /// `prices.url` for a station's config.
        pub fn config(&self) -> String {
            format!("prices.url = \"{}\"\n", self.url())
        }

        pub fn start() -> Prices {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                for mut s in listener.incoming().flatten() {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf);
                    let _ = write!(
                        s,
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{CATALOGUE}",
                        CATALOGUE.len()
                    );
                }
            });
            Prices { port }
        }
    }
}
