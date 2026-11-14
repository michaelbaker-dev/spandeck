//! `spandeck doctor`: is this station set up to work?

use serde_json::{json, Value};
use spandeck_agents::{install, AgentKind};

use crate::daemon::Daemon;
use crate::db::Db;

/// A program's own word for its version. It gets no terminal and nothing to
/// read, and three seconds of our attention: on a Mac, `codex --version` run
/// from the daemon never returned, and the station check hung with it.
fn version_of(program: &str) -> Option<String> {
    use std::io::Read;
    let mut child = std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < std::time::Duration::from_secs(3) => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            _ => {
                // Stop waiting, but never kill it: a program asked for its
                // version may be in the middle of updating itself, and one
                // killed there can be left without its own binary.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return None;
            }
        }
    };
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    status
        .success()
        .then(|| out.lines().next().unwrap_or("").trim().to_string())
}

impl Daemon {
    pub fn doctor(&self) -> Value {
        let mut checks: Vec<Value> = vec![];
        let mut add = |name: &str, ok: bool, required: bool, detail: String| {
            checks.push(json!({"name": name, "ok": ok, "required": required, "detail": detail}));
        };
        add(
            "daemon",
            true,
            true,
            format!(
                "spandeckd {} running as pid {}",
                crate::daemon::VERSION,
                std::process::id()
            ),
        );
        let schema = self.db.schema_version();
        add(
            "database",
            schema == Db::LATEST_SCHEMA,
            true,
            format!("schema {schema} of {}", Db::LATEST_SCHEMA),
        );
        if codex_sandbox_blocked(&self.home) {
            add(
                "Codex sandbox",
                false,
                false,
                "Codex cannot run commands here: this system blocks the sandbox Codex uses. Run `spandeck doctor --fix` once (it asks for your password)".into(),
            );
        }
        let service = service_installed(&self.home);
        add(
            "service",
            service,
            false,
            if service {
                "spandeckd starts at login".into()
            } else {
                "spandeckd does not start at login: after a reboot this station stays down until someone starts it. Run `spandeck service --install`".into()
            },
        );
        add(
            "git",
            version_of("git").is_some(),
            true,
            version_of("git").unwrap_or_else(|| {
                "git not found: worktrees, checkpoints and sign-offs need it".into()
            }),
        );

        let me = self.fleet.tailnet_self();
        add(
            "tailscale",
            me.is_some(),
            false,
            match &me {
                Some(m) => format!(
                    "logged in as {} ({})",
                    m.login,
                    m.ips.first().cloned().unwrap_or_default()
                ),
                None => {
                    "Tailscale is not running or not logged in: this station cannot see the fleet"
                        .into()
                }
            },
        );
        let web = self.web_addrs.lock().unwrap().clone();
        add(
            "control deck",
            !web.is_empty(),
            false,
            if web.is_empty() {
                "the web listener is not running".into()
            } else {
                format!("serving on {}", web.join(", "))
            },
        );

        for kind in spandeck_agents::catalogue::CATALOGUE.iter().map(|p| p.kind) {
            let program = self
                .agent_program(kind)
                .unwrap_or_else(|| kind.as_str().to_string());
            let probe = self.agent_probe(kind, std::time::Duration::from_secs(60));
            add(
                &format!("{} installed", kind.display()),
                probe.path.is_some(),
                false,
                match (&probe.path, &probe.version) {
                    (Some(path), Some(v)) => format!("{v} at {}", path.display()),
                    (Some(path), None) => format!("{} (version not read)", path.display()),
                    (None, _) => {
                        format!("`{program}` not found; a pane started for it installs it first")
                    }
                },
            );
            if let Ok(st) = install::status(kind, &self.home) {
                let detail = match (st.installed, st.outdated, kind) {
                    (true, true, _) => "hooks installed but out of date: run `spandeck integration install`".to_string(),
                    (true, false, _) => format!("hooks installed (v{}){}", st.version.unwrap_or(0), st.note.map(|n| format!("; {n}")).unwrap_or_default()),
                    (false, _, AgentKind::Claude) => "not installed globally; panes spandeck starts are hooked at launch anyway".to_string(),
                    (false, _, _) => "not installed: run `spandeck integration install codex`, then trust the hooks in Codex with /hooks".to_string(),
                };
                add(
                    &format!("{} hooks", kind.display()),
                    st.installed && !st.outdated || kind == AgentKind::Claude,
                    false,
                    detail,
                );
            }
        }
        let keep = self
            .cfg
            .keep_awake_cmd
            .clone()
            .map(|c| c.first().cloned().unwrap_or_default())
            .unwrap_or_else(|| {
                if cfg!(target_os = "macos") {
                    "caffeinate".into()
                } else {
                    "systemd-inhibit".into()
                }
            });
        let found = std::process::Command::new("sh")
            .args(["-c", &format!("command -v {keep}")])
            .output()
            .is_ok_and(|o| o.status.success());
        add(
            "keep-awake",
            found || !self.cfg.keep_awake,
            false,
            if found {
                format!("{keep} available")
            } else {
                format!("{keep} not found: the station may sleep while agents work")
            },
        );

        let ok = checks
            .iter()
            .all(|c| c["ok"] == json!(true) || c["required"] == json!(false));
        json!({"ok": ok, "station": self.station, "checks": checks})
    }
}

/// Whether this station is set to start spandeckd at login.
pub fn service_installed(home: &std::path::Path) -> bool {
    service_definition(std::path::Path::new("spandeck"), home)
        .0
        .exists()
}

/// A systemd user unit (Linux) or launchd agent (macOS) that starts spandeckd at login.
pub fn service_definition(
    exe: &std::path::Path,
    home: &std::path::Path,
) -> (std::path::PathBuf, String, String) {
    // A login service starts with a bare PATH. Give it the one the user has
    // now, so the daemon finds the agent programs (`claude`, `codex`) they do.
    let mut dirs: Vec<String> = vec![home.join(".local/bin").display().to_string()];
    if let Some(dir) = exe.parent().filter(|d| !d.as_os_str().is_empty()) {
        dirs.push(dir.display().to_string());
    }
    dirs.extend(
        std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(String::from),
    );
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| {
        d.starts_with('/') && !d.contains(['"', '<', '>', '&', '\n', '%']) && seen.insert(d.clone())
    });
    for d in ["/usr/local/bin", "/usr/bin", "/bin"] {
        if seen.insert(d.to_string()) {
            dirs.push(d.to_string());
        }
    }
    let search = dirs.join(":");
    let exe = exe.display();
    if cfg!(target_os = "macos") {
        let path = home.join("Library/LaunchAgents/dev.spandeck.spandeckd.plist");
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>dev.spandeck.spandeckd</string>
  <key>ProgramArguments</key><array><string>{exe}</string><string>daemon</string></array>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>{search}</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#
        );
        let enable = format!("launchctl bootstrap gui/$(id -u) {}", path.display());
        (path, body, enable)
    } else {
        let path = home.join(".config/systemd/user/spandeckd.service");
        let body = format!(
            "[Unit]\nDescription=spandeck daemon\nAfter=network-online.target\n\n[Service]\nEnvironment=\"PATH={search}\"\nExecStart={exe} daemon\nRestart=on-failure\nRestartSec=2\n# An upgrade replaces the process image in place; do not kill panes with it.\nKillMode=process\n\n[Install]\nWantedBy=default.target\n"
        );
        (path, body, "systemctl --user daemon-reload && systemctl --user enable --now spandeckd && loginctl enable-linger $USER".to_string())
    }
}

/// Where the profile that lets Codex's sandbox work is installed.
pub const CODEX_SANDBOX_PROFILE_PATH: &str = "/etc/apparmor.d/spandeck-codex-bwrap";

/// AppArmor profiles for bubblewrap, the program Codex builds its sandbox
/// with: the system's copy, which Codex uses when there is one, and Codex's own.
pub const CODEX_SANDBOX_PROFILE: &str = r#"# Installed by `spandeck doctor --fix`. Version 2.
# Codex runs each command in a sandbox built with bubblewrap. Ubuntu stops
# unprivileged programs from setting up such a sandbox unless a profile allows
# it. These allow it for bubblewrap only: the system's copy, which Codex uses
# when it is installed, and the copy that ships with Codex.
abi <abi/4.0>,
include <tunables/global>

profile spandeck-bwrap /usr/bin/bwrap flags=(unconfined) {
  userns,
}

profile spandeck-codex-bwrap /home/*/.codex/packages/*/releases/*/codex-resources/bwrap flags=(unconfined) {
  userns,
}
"#;

/// On Ubuntu 23.10 and later, Codex cannot run any command until its sandbox
/// program is allowed to use user namespaces. True when that is the case
/// here and the fix has not been installed. It needs root, once.
pub fn codex_sandbox_blocked(home: &std::path::Path) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let restricted =
        std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
            .is_ok_and(|v| v.trim() == "1");
    // An older version of the profile covered only Codex's own copy, which
    // Codex does not use when the system has one: that still counts as blocked.
    let installed = std::fs::read_to_string(CODEX_SANDBOX_PROFILE_PATH).unwrap_or_default();
    restricted && home.join(".codex/packages").is_dir() && installed != CODEX_SANDBOX_PROFILE
}
