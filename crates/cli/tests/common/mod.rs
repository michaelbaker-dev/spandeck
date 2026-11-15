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
/// Unix: an upgraded daemon is the same process. Windows: a new one.
pub const UPGRADE_KEEPS_PID: bool = spandeck_terminal::sys::UPGRADE_KEEPS_PID;

pub struct Station {
    pub name: String,
    pub root: PathBuf,
    pub port: u16,
    pub child: Option<Child>,
    pub env: Vec<(String, String)>,
    _dir: Option<tempfile::TempDir>,
    scripts: std::sync::atomic::AtomicU32,
}

/// A port for a test station. Asking the system for "any free port" and then
/// letting go of it lets another test, or a daemon that restarts and listens
/// again, be given the same one: that made tests fail on small CI machines.
/// So each test binary hands out its own run of ports, below the range the
/// system uses for outgoing connections, and never hands one out twice.
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
        let (pa, pb) = (free_port(), free_port());
        let key = shared_key.as_deref();
        let a = Station::start_keyed(
            "alpha",
            pa,
            &format!("peers = [\"127.0.0.1:{pb}\"]\n{extra_a}"),
            key,
        );
        let b = Station::start_keyed("bravo", pb, &format!("peers = [\"127.0.0.1:{pa}\"]\n"), key);
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

    pub fn start_on(name: &str, port: u16, extra: &str, env: &[(&str, &str)]) -> Station {
        let mut s = Station::prepare(name, port, extra, env);
        s.launch();
        s
    }

    fn start_keyed(name: &str, port: u16, extra: &str, key: Option<&str>) -> Station {
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
            env: vec![],
            _dir: dir,
            scripts: std::sync::atomic::AtomicU32::new(0),
        }
    }

    pub fn launch(&mut self) {
        let mut cmd = Command::new(SPANDECK);
        cmd.arg("daemon")
            .env("SPANDECK_STATE_DIR", self.root.join("state"))
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
        self.child = Some(cmd.spawn().unwrap());
        self.wait_ready();
    }

    pub fn wait_ready(&self) {
        // Ready means both the socket and the Control Deck listener are up.
        wait_for("daemon to answer", 15, || {
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
        wait_for("the frozen station to stop answering", 20, || {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", self.port)).ok()?;
            s.set_read_timeout(Some(std::time::Duration::from_millis(500)))
                .ok()?;
            let body = r#"{"method":"station.hello","params":{}}"#;
            write!(
                s,
                "POST /api/peer/rpc HTTP/1.1\r\nhost: station\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .ok()?;
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

fn rusqlite_token(db: &Path, pane: &str) -> Option<String> {
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

    pub struct Page {
        _browser: headless_chrome::Browser,
        pub tab: std::sync::Arc<headless_chrome::Tab>,
    }

    impl Page {
        pub fn open(s: &Station, width: u32, height: u32) -> Page {
            Page::open_as(s, width, height, true)
        }

        /// A browser that has never been given a token for this station.
        pub fn open_signed_out(s: &Station, width: u32, height: u32) -> Page {
            Page::open_as(s, width, height, false)
        }

        fn open_as(s: &Station, width: u32, height: u32, signed_in: bool) -> Page {
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
            let tab = browser.new_tab().unwrap();
            if signed_in {
                tab.navigate_to(&format!("{}#token={}", s.url("/"), s.token()))
                    .unwrap();
                tab.wait_for_element(".topbar").unwrap();
            } else {
                tab.navigate_to(&s.url("/")).unwrap();
                tab.wait_for_element("main.login").unwrap();
            }
            Page {
                _browser: browser,
                tab,
            }
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
            }
        }

        pub fn js(&self, expr: &str) -> Value {
            // Values come back as JSON text, so arrays and objects survive.
            let wrapped = format!("(async () => JSON.stringify(await ({expr})))()");
            let r = self
                .tab
                .evaluate(&wrapped, true)
                .unwrap_or_else(|e| panic!("js failed: {e}\n{expr}"));
            r.value
                .and_then(|v| v.as_str().and_then(|s| serde_json::from_str(s).ok()))
                .unwrap_or(Value::Null)
        }

        pub fn wait(&self, what: &str, expr: &str) -> Value {
            wait_for(what, 20, || {
                let v = self.js(expr);
                (!v.is_null() && v != json!(false) && v != json!("")).then_some(v)
            })
        }

        pub fn goto(&self, hash: &str) {
            self.js(&format!("location.hash = {}", json!(hash)));
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
        pub fn right_click(&self, el: &str) {
            self.wait(
                "something to right-click",
                &format!("(el => el ? (el.dispatchEvent(new MouseEvent('contextmenu', {{bubbles: true, cancelable: true, clientX: 300, clientY: 200}})), true) : false)({el})"),
            );
            self.wait("the menu", "!!document.querySelector('.menu')");
        }

        /// Choose the item of the open menu whose label starts with `label`.
        pub fn choose(&self, label: &str) {
            let done = self.js(&format!(
                "(i => i && !i.disabled ? (i.click(), true) : [...document.querySelectorAll('.menu-item')].map(x => x.innerText))([...document.querySelectorAll('.menu-item')].find(i => i.innerText.startsWith({})))",
                json!(label)
            ));
            assert_eq!(done, json!(true), "no menu item {label:?} to choose");
        }

        /// The text on a pane's terminal as the browser shows it.
        pub fn term_text(&self) -> String {
            self.js("[...document.querySelectorAll('.pane .xterm-rows')].map(r => r.innerText).join('\\n----\\n')").as_str().unwrap_or("").to_string()
        }
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
