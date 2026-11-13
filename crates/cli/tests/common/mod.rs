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

pub struct Station {
    pub name: String,
    pub root: PathBuf,
    pub port: u16,
    pub child: Option<Child>,
    pub env: Vec<(String, String)>,
    _dir: Option<tempfile::TempDir>,
    scripts: std::cell::Cell<u32>,
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

pub fn wait_for<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(40));
    }
}

impl Station {
    pub fn start(name: &str) -> Station {
        Station::start_with(name, "", &[])
    }

    /// `extra` is appended to config.toml; `env` is added to the daemon's environment.
    pub fn start_with(name: &str, extra: &str, env: &[(&str, &str)]) -> Station {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let port = free_port();
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("fake")).unwrap();
        // Top-level keys first: TOML tables swallow everything after them.
        let (top, tables): (Vec<&str>, Vec<&str>) = {
            let idx = extra.find("\n[").map(|i| i + 1).or_else(|| extra.starts_with('[').then_some(0)).unwrap_or(extra.len());
            (vec![&extra[..idx]], vec![&extra[idx..]])
        };
        std::fs::write(
            root.join("config/config.toml"),
            format!(
                "station = \"{name}\"\nweb_port = {port}\nweb_bind = [\"127.0.0.1\"]\ndiscovery = false\nkeep_awake_cmd = [\"sleep\", \"300\"]\n{}\n[agents.fake]\nprogram = \"{FAKE}\"\n{}\n",
                top.join(""),
                tables.join("")
            ),
        )
        .unwrap();
        let mut s = Station {
            name: name.to_string(),
            root,
            port,
            child: None,
            env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            _dir: Some(dir),
            scripts: std::cell::Cell::new(0),
        };
        s.launch();
        s
    }

    pub fn launch(&mut self) {
        let mut cmd = Command::new(SPANDECK);
        cmd.arg("daemon")
            .env("SPANDECK_STATE_DIR", self.root.join("state"))
            .env("SPANDECK_CONFIG_DIR", self.root.join("config"))
            .env("SPANDECK_FAKE_DIR", self.root.join("fake"))
            .env("HOME", self.root.join("home"))
            .env("SPANDECK_LOG", "warn")
            .env_remove("SPANDECK_SOCKET")
            .env_remove("SPANDECK_TOKEN")
            .env_remove("SPANDECK_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
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
            info["web"].as_array().is_some_and(|w| !w.is_empty()).then_some(())
        });
    }

    pub fn paths(&self) -> Paths {
        Paths { state_dir: self.root.join("state"), config_dir: self.root.join("config") }
    }

    pub fn token(&self) -> String {
        std::fs::read_to_string(self.paths().token_file()).unwrap().trim().to_string()
    }

    pub fn client(&self) -> Result<Client, ClientError> {
        let token = std::fs::read_to_string(self.paths().token_file()).map_err(|e| ClientError { code: "io".into(), message: e.to_string() })?;
        let mut c = Client::connect(&self.root.join("state/spandeckd.sock"), token.trim())?;
        c.set_timeout(Some(Duration::from_secs(60)));
        Ok(c)
    }

    pub fn try_call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        self.client()?.call(method, params)
    }

    pub fn call(&self, method: &str, params: Value) -> Value {
        self.try_call(method, params).unwrap_or_else(|e| panic!("{method} failed: {e}"))
    }

    /// Call the API as a pane (with the pane's own token), the way an agent does.
    pub fn call_as_pane(&self, pane: &str, method: &str, params: Value) -> Result<Value, ClientError> {
        let token: String = {
            let db = rusqlite_token(&self.root.join("state/spandeck.db"), pane);
            db.unwrap_or_else(|| panic!("no token for pane {pane}"))
        };
        let mut c = Client::connect(&self.root.join("state/spandeckd.sock"), &token)?;
        c.set_timeout(Some(Duration::from_secs(60)));
        c.call(method, params)
    }

    pub fn script_file(&self, script: &Value) -> PathBuf {
        let n = self.scripts.get();
        self.scripts.set(n + 1);
        let path = self.root.join(format!("script-{n}.json"));
        std::fs::write(&path, script.to_string()).unwrap();
        path
    }

    /// Start a fake agent pane running `script`. Extra fields go to pane.spawn.
    pub fn fake(&self, script: Value, extra: Value) -> String {
        let path = self.script_file(&script);
        let mut params = json!({"agent": "fake", "args": ["--script", path], "cwd": self.root.join("home")});
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                params[k] = v.clone();
            }
        }
        self.call("pane.spawn", params)["id"].as_str().unwrap().to_string()
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
                last = v["lines"].as_array().map(|a| a.iter().filter_map(|l| l.as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                if last.contains(needle) {
                    return last;
                }
            }
            assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting for {needle:?} in pane {pane}; screen:\n{}", last.trim_end());
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
            assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting for state {state}; pane is {p}\nscreen:\n{}", self.text(pane).trim_end());
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
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn rusqlite_token(db: &Path, pane: &str) -> Option<String> {
    // The sqlite3 CLI keeps the harness free of a second SQLite build.
    let out = Command::new("sqlite3")
        .arg(db)
        .arg(format!("SELECT token FROM panes WHERE id='{}' OR json_extract(meta,'$.name')='{}' ORDER BY created DESC LIMIT 1", pane.replace('\'', ""), pane.replace('\'', "")))
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty()).then_some(t)
}

impl Drop for Station {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            // SIGTERM first so panes are cleaned up with the daemon.
            unsafe { libc_kill(c.id() as i32, 15) };
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(2) {
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

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
