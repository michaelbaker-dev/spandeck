//! `spandeck doctor`: is this station set up to work?

use serde_json::{json, Value};
use spandeck_agents::{install, AgentKind};

use crate::agents::{Ended, Health};
use crate::daemon::Daemon;
use crate::db::Db;

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
        // Asked without a terminal and given three seconds: on a Mac a
        // program asked its version from the daemon once never returned, and
        // the station check hung with it. It is not killed, and not answering
        // is not the same as not being there.
        let git = self.run_captured(
            &["git".to_string(), "--version".to_string()],
            std::time::Duration::from_secs(3),
        );
        let (git_ok, git_detail) = match git.ended {
            Ended::Exited { ok: true } => {
                (true, git.text.lines().next().unwrap_or("").trim().to_string())
            }
            Ended::NotRun { missing: true } => (
                false,
                "git is not installed: worktrees, checkpoints and sign-offs need it".into(),
            ),
            Ended::NoAnswer => (
                true,
                "git is installed but did not answer `git --version` within 3 seconds, so whether it works is not known".into(),
            ),
            Ended::Exited { ok: false } | Ended::NotRun { .. } => (
                false,
                format!(
                    "git is installed but does not start: {}",
                    spandeck_agents::catalogue::error_line(&git.text).unwrap_or_default()
                ),
            ),
        };
        add("git", git_ok, true, git_detail);

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
        // This machine's Tailscale key: when it lapses the station drops off
        // the tailnet, and nothing else says so beforehand.
        if let Some(t) = self.tailnet.get() {
            let now = crate::paths::now_ms();
            if let Some(me) = t.devices.iter().find(|d| d.me) {
                let (ok, detail) = match me.expires_ms {
                    None => (true, "this machine's key never expires".to_string()),
                    Some(e) if me.expired || e <= now => (
                        false,
                        format!("this machine's key expired on {} (UTC): sign in to Tailscale again here", crate::tailnet::utc_date(e)),
                    ),
                    Some(e) => {
                        let days = crate::tailnet::days_left(e, now);
                        (
                            days > 14,
                            format!(
                                "this machine's key expires in {days} day{} (on {}, UTC){}",
                                if days == 1 { "" } else { "s" },
                                crate::tailnet::utc_date(e),
                                if days > 14 { "" } else { ": sign in to Tailscale again here, or choose \"Disable key expiry\" for it in Tailscale's admin console" }
                            ),
                        )
                    }
                };
                add("tailscale key", ok, false, detail);
            }
        }
        // Stations whose clocks are far from this one's: past two minutes
        // their calls are refused, and they list each other as unreachable.
        for p in self.fleet.peers() {
            let Some(c) = self.perf.clock_of(&p.station) else {
                continue;
            };
            if c.diff_ms.unsigned_abs() < 30_000 {
                continue;
            }
            let words = crate::perf::clock_words(c.diff_ms);
            let which = if c.diff_ms > 0 { "ahead of" } else { "behind" };
            add(
                &format!("clock ({})", p.station),
                !c.refused(),
                false,
                if c.refused() {
                    format!("{}'s clock is {words} {which} this station's: calls between them are refused (the limit is 2 minutes), so each lists the other as unreachable. Set both clocks from the network", p.station)
                } else {
                    format!("{}'s clock is {words} {which} this station's: past 2 minutes, calls between them are refused", p.station)
                },
            );
        }
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

        for kind in spandeck_agents::catalogue::all().iter().map(|p| p.kind) {
            let program = self
                .agent_program(kind)
                .unwrap_or_else(|| kind.as_str().to_string());
            let probe = self.agent_probe(kind, std::time::Duration::from_secs(60));
            let at = probe
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            add(
                &format!("{} installed", kind.display()),
                matches!(probe.health, Health::Ok | Health::NoAnswer),
                false,
                match (probe.health, &probe.version) {
                    (Health::Ok, Some(v)) => format!("{v} at {at}"),
                    (Health::Ok, None) => {
                        format!("{at} (it answered, but its version could not be read)")
                    }
                    (Health::Missing, _) => format!(
                        "not installed (there is no `{program}` in the folders spandeck looks in); a pane started for it installs it first"
                    ),
                    (Health::Broken, _) => format!(
                        "installed at {at} but it does not start: {}. A pane started for it offers to reinstall it, or use Reinstall under Agent programs in the Control Deck",
                        probe.problem.clone().unwrap_or_default()
                    ),
                    (Health::NoAnswer, _) => format!(
                        "installed at {at}; it did not answer when asked its version within {} seconds, so whether it works is not known",
                        crate::agents::VERSION_LIMIT.as_secs()
                    ),
                },
            );
            if let Ok(st) = install::status(kind, &self.home) {
                let detail = match (st.installed, st.outdated, kind) {
                    (true, true, _) => format!("hooks installed but out of date (v{}): a rule check is cut off after 10 seconds. Run `spandeck integration install {}`{}", st.version.unwrap_or(0), kind.as_str(), if kind == AgentKind::Codex { ", then trust the hooks again in Codex with /hooks" } else { "" }),
                    (true, false, _) => format!("hooks installed (v{}){}", st.version.unwrap_or(0), st.note.map(|n| format!("; {n}")).unwrap_or_default()),
                    (false, _, AgentKind::Claude) => "not installed globally; panes spandeck starts are hooked at launch anyway".to_string(),
                    (false, _, _) => "not installed: run `spandeck integration install codex`, then trust the hooks in Codex with /hooks".to_string(),
                };
                add(
                    &format!("{} hooks", kind.display()),
                    st.installed && !st.outdated || (kind == AgentKind::Claude && !st.installed),
                    false,
                    detail,
                );
            }
        }
        // The vault, as the last check left it (`vault.check` asks again).
        let vault = self.vault_view();
        let url = vault["url"].as_str().unwrap_or("");
        let broken = vault["checks"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["ok"] == json!(false))
            .and_then(|c| c["detail"].as_str())
            .unwrap_or("");
        let (ok, detail) = match vault["health"].as_str().unwrap_or("") {
            "off" => (true, "turned off in the config".to_string()),
            "none" => (
                true,
                "no AgentVault found on your tailnet (spandeck works without one)".to_string(),
            ),
            "ok" => (
                true,
                format!("connected to {url}; agents can use your keys through it"),
            ),
            "not_connected" => (false, format!("AgentVault found at {url}, and this station is not connected to it: connect it in the Control Deck, under Vault")),
            "problem" => (false, broken.to_string()),
            _ => (true, "not checked yet".to_string()),
        };
        add("vault", ok, false, detail);

        let keep = self
            .cfg
            .keep_awake_cmd
            .clone()
            .map(|c| c.first().cloned().unwrap_or_default())
            .unwrap_or_else(|| {
                spandeck_terminal::sys::keep_awake_cmd(&self.exe)
                    .first()
                    .cloned()
                    .unwrap_or_default()
            });
        // On Windows the default is spandeck itself, found by its path.
        let found = self.find_program(&keep).is_some();
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

/// How to undo `spandeck service --install` on this kind of machine.
pub struct ServiceRemoval {
    /// The definition `service --install` wrote. Deleted after `before`.
    pub file: std::path::PathBuf,
    /// Shell lines that turn the login service off while its file exists.
    pub before: Vec<String>,
    /// Shell lines for after the file is gone.
    pub after: Vec<String>,
    /// A line that may end the daemon itself, so it is run last, detached.
    pub last: Vec<String>,
}

/// Stop spandeckd starting at login: the reverse of `service_definition`.
/// Nothing here stops the daemon that is running, except `last` on macOS,
/// where taking the job out of launchd ends it. On Linux and Windows the
/// daemon then ends by itself with success, which neither restarts.
pub fn service_removal(home: &std::path::Path) -> ServiceRemoval {
    let (file, _, _) = service_definition(std::path::Path::new("spandeck"), home);
    let line = |s: &str| vec![s.to_string()];
    let (before, after, last) = if spandeck_terminal::sys::WINDOWS {
        (
            line(&format!("schtasks.exe /Delete /TN {WINDOWS_TASK} /F")),
            vec![],
            vec![],
        )
    } else if cfg!(target_os = "macos") {
        (
            vec![],
            vec![],
            line("launchctl bootout gui/$(id -u)/dev.spandeck.spandeckd"),
        )
    } else {
        (
            line("systemctl --user disable spandeckd"),
            line("systemctl --user daemon-reload"),
            vec![],
        )
    };
    ServiceRemoval {
        file,
        before,
        after,
        last,
    }
}

/// The name of the Scheduled Task that starts spandeckd at log on (Windows).
pub const WINDOWS_TASK: &str = "spandeckd";

/// Windows: a per-user Scheduled Task that runs at log on. The file is the
/// PowerShell script that registers it; running it is the enable step.
/// A Windows Service is not used: it would run without the user's profile,
/// PATH and the agents' logins. The task runs `spandeckd.exe`, which starts
/// the daemon with no console window; without it (a build from source that
/// did not install it) the task runs `spandeck.exe daemon` in a window.
fn windows_task_definition(
    exe: &std::path::Path,
    home: &std::path::Path,
) -> (std::path::PathBuf, String, String) {
    let (_, config) = spandeck_terminal::sys::default_dirs(home);
    let path = config.join("start-at-login.ps1");
    let quote = |p: &std::path::Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
    let launcher = exe.with_file_name("spandeckd.exe");
    let action = if launcher.is_file() {
        format!("New-ScheduledTaskAction -Execute {}", quote(&launcher))
    } else {
        format!(
            "New-ScheduledTaskAction -Execute {} -Argument 'daemon'",
            quote(exe)
        )
    };
    let body = format!(
        r#"# Written by `spandeck service --install`. Registers the task that starts
# spandeckd when you log on. Run it again after moving spandeck.
$ErrorActionPreference = 'Stop'
$me = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = {action}
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $me
# No time limit (the default stops a task after three days), start again if
# it fails, and do not stop for a laptop on battery.
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -MultipleInstances IgnoreNew
$principal = New-ScheduledTaskPrincipal -UserId $me -LogonType Interactive -RunLevel Limited
Register-ScheduledTask -TaskName '{WINDOWS_TASK}' -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Description 'spandeck daemon' -Force | Out-Null
Start-ScheduledTask -TaskName '{WINDOWS_TASK}'
"#
    );
    let enable = format!(
        "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
        path.display()
    );
    (path, body, enable)
}

/// A systemd user unit (Linux), launchd agent (macOS) or Scheduled Task
/// (Windows) that starts spandeckd at login: where its definition is
/// written, the definition, and the command that turns it on.
pub fn service_definition(
    exe: &std::path::Path,
    home: &std::path::Path,
) -> (std::path::PathBuf, String, String) {
    if spandeck_terminal::sys::WINDOWS {
        return windows_task_definition(exe, home);
    }
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
