//! Adding a machine to the fleet from the Control Deck.
//!
//! The owner picks a machine under their Tailscale login that is not a station
//! yet. This station connects to it over SSH, runs the install script from
//! the README there, starts spandeck, and has it ask to join this fleet the
//! way `spandeck fleet --join` does. The owner then approves the code as for
//! any other station.
//!
//! The password, when one is needed, is used for that one connection. It is
//! never written to disk, a log, an event, an error or a command line: ssh
//! asks for it through an askpass helper (this same binary), which is handed
//! it once over a private socket.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::daemon::{invalid, Daemon, Error, Result};
use crate::fleet::{tailscale_status, Peer};
use crate::paths::{new_id, now_ms, random_token};

/// The script the README tells a person to run. The same text is sent to the
/// machine being added, so the two cannot drift apart.
const INSTALL_SH: &str = include_str!("../../../scripts/install.sh");

/// How long one add may take from connecting to the join request.
const ADD_TIMEOUT: Duration = Duration::from_secs(600);
/// How long an approved station has to show up.
const SEEN_TIMEOUT: Duration = Duration::from_secs(120);
const KEPT_LINES: usize = 300;

/// A password held for one connection. Wiped when dropped. Deliberately has
/// no `Debug`, `Display`, `Clone` or `Serialize`.
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(text: String) -> Secret {
        Secret(text.into_bytes())
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// How a newly installed station gets into the fleet. Today there is one
/// way: it asks, and the owner approves the code it shows. A single-use
/// ticket made here and handed to the new station would be a second variant:
/// it changes `join_lines` and skips the wait for approval, nothing else.
#[derive(Clone, Copy)]
enum JoinBy {
    Code,
}

#[derive(Clone)]
struct Job {
    id: String,
    machine: String,
    name: String,
    address: String,
    os: String,
    user: String,
    by: String,
    /// connecting, installing, starting, joining, approve, joined: in
    /// progress. needs_password, failed, done: finished.
    stage: &'static str,
    problem: Option<(&'static str, String)>,
    lines: VecDeque<String>,
    code: Option<String>,
    request: Option<String>,
    station: Option<String>,
    started_ms: u64,
    cancel: Arc<tokio::sync::Notify>,
}

impl Job {
    fn running(&self) -> bool {
        !matches!(self.stage, "needs_password" | "failed" | "done")
    }

    fn view(&self) -> Value {
        json!({
            "id": self.id, "machine": self.machine, "name": self.name, "address": self.address,
            "os": self.os, "user": self.user, "by": self.by, "stage": self.stage,
            "running": self.running(),
            "problem": self.problem.as_ref().map(|(kind, text)| json!({"kind": kind, "text": text})),
            "lines": self.lines, "code": self.code, "request": self.request,
            "station": self.station, "started_ms": self.started_ms,
        })
    }
}

#[derive(Default)]
pub struct AddState {
    job: Mutex<Option<Job>>,
}

struct Candidate {
    id: String,
    name: String,
    os: String,
    address: String,
    tailscale_ssh: bool,
}

/// Tailscale's own address ranges. An address outside them is not a tailnet
/// address, whatever the status output says.
fn is_tailscale_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64,
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

fn host_of(addr: &str) -> &str {
    addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr)
}

/// Mac and Linux machines under the owner's login that are online and are
/// not stations. Phones, tagged machines, shared machines and Windows are out.
fn candidates(status: &Value, stations: &[Peer]) -> Vec<Candidate> {
    let Some(me) = status.pointer("/Self/UserID").and_then(|u| u.as_i64()) else {
        return vec![];
    };
    let text = |v: &Value, k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let truthy = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    let filled = |v: &Value, k: &str| {
        v.get(k)
            .and_then(|x| x.as_array())
            .is_some_and(|a| !a.is_empty())
    };
    let mut out = vec![];
    for peer in status
        .get("Peer")
        .and_then(|p| p.as_object())
        .into_iter()
        .flat_map(|m| m.values())
    {
        let os = text(peer, "OS");
        if peer.get("UserID").and_then(|u| u.as_i64()) != Some(me)
            || !truthy(peer, "Online")
            || truthy(peer, "ShareeNode")
            || filled(peer, "Tags")
            || !matches!(os.as_str(), "macOS" | "linux")
        {
            continue;
        }
        let ips: Vec<String> = peer
            .get("TailscaleIPs")
            .and_then(|a| a.as_array())
            .into_iter()
            .flatten()
            .filter_map(|i| i.as_str().map(String::from))
            .collect();
        let dns = text(peer, "DNSName").trim_end_matches('.').to_lowercase();
        let short = dns.split('.').next().unwrap_or_default().to_string();
        let host = text(peer, "HostName");
        let name = if short.is_empty() {
            host.clone()
        } else {
            short.clone()
        };
        // Already a station: something at its address answers as one, or a
        // station carries its name (a station is named after its machine).
        let is_station = stations.iter().any(|s| {
            let at = host_of(&s.addr).to_lowercase();
            ips.contains(&at)
                || (!dns.is_empty() && at == dns)
                || [&short, &host]
                    .iter()
                    .any(|n| !n.is_empty() && n.eq_ignore_ascii_case(&s.station))
        });
        let address = ips
            .iter()
            .filter_map(|i| i.parse::<IpAddr>().ok())
            .filter(|ip| is_tailscale_address(*ip))
            .min_by_key(|ip| ip.is_ipv6());
        let (Some(address), false) = (address, is_station) else {
            continue;
        };
        let id = text(peer, "ID");
        if id.is_empty() || name.is_empty() {
            continue;
        }
        out.push(Candidate {
            id,
            name,
            os: if os == "macOS" { "macOS" } else { "Linux" }.to_string(),
            address: address.to_string(),
            tailscale_ssh: filled(peer, "sshHostKeys"),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Only text reaches the Control Deck: no escape sequences, no endless lines.
fn clean_line(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .take(600)
        .collect()
}

/// What became of the password: none was given, ssh asked for it and got
/// it, or the other machine refused without ever asking.
#[derive(Clone, Copy)]
enum Password {
    None,
    Asked,
    NeverAsked,
}

/// Why ssh itself gave up, from what it printed.
fn ssh_problem(said: &[String], password: Password) -> &'static str {
    let all = said.join("\n").to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| all.contains(n));
    if has(&[
        "host key verification failed",
        "remote host identification has changed",
    ]) {
        "host_key"
    } else if has(&["permission denied", "too many authentication failures"]) {
        match password {
            Password::None => "needs_password",
            Password::Asked => "wrong_password",
            Password::NeverAsked => "no_password_login",
        }
    } else if has(&["connection refused"]) {
        "ssh_off"
    } else if has(&[
        "timed out",
        "no route to host",
        "host is down",
        "network is unreachable",
    ]) {
        "unreachable"
    } else {
        "connect"
    }
}

impl Daemon {
    fn known_hosts_file(&self) -> PathBuf {
        self.paths.state_dir.join("known_hosts")
    }

    fn add_candidates_now(&self) -> Option<Vec<Candidate>> {
        let status = tokio::task::block_in_place(tailscale_status)?;
        let mut stations = self.fleet.peers();
        stations.extend(self.known_stations());
        Some(candidates(&status, &stations))
    }

    /// Whether this station may add machines: the config says so, or the
    /// owner turned it on from the Control Deck (kept in the station's state).
    pub fn add_enabled(&self) -> bool {
        self.cfg.add_machines || self.db.kv_get("add.enabled").as_deref() == Some("1")
    }

    /// The owner's switch in the Control Deck. Only the owner reaches this
    /// (it is on no agent or station list and is never forwarded), so the
    /// click is the owner saying so, as the line in config.toml was.
    pub fn add_enable(&self, on: bool, by: &str) -> Value {
        self.db.kv_set("add.enabled", if on { "1" } else { "0" });
        self.record(
            "fleet.add_enabled",
            None,
            None,
            None,
            Some(by),
            json!({"on": on, "station": self.station}),
        );
        json!({"enabled": self.add_enabled(), "by_config": self.cfg.add_machines})
    }

    /// What the picker shows.
    pub fn add_candidates(&self) -> Value {
        let found = self.add_candidates_now();
        json!({
            "enabled": self.add_enabled(),
            "by_config": self.cfg.add_machines,
            "station": self.station,
            "tailscale": found.is_some(),
            "machines": found.unwrap_or_default().iter().map(|c| json!({
                "id": c.id, "name": c.name, "os": c.os, "address": c.address,
                "tailscale_ssh": c.tailscale_ssh,
            })).collect::<Vec<_>>(),
        })
    }

    pub fn add_status(&self) -> Value {
        self.adding
            .job
            .lock()
            .unwrap()
            .as_ref()
            .map(|j| j.view())
            .unwrap_or(Value::Null)
    }

    /// Change the job, if it is still the one named, and tell the deck.
    fn add_update(&self, id: &str, f: impl FnOnce(&mut Job)) -> bool {
        let stage = {
            let mut job = self.adding.job.lock().unwrap();
            match job.as_mut().filter(|j| j.id == id) {
                Some(j) => {
                    f(j);
                    j.stage
                }
                None => return false,
            }
        };
        self.emit(
            "fleet.add.updated",
            None,
            None,
            json!({"id": id, "stage": stage}),
        );
        true
    }

    fn add_stage(&self, id: &str) -> Option<&'static str> {
        self.adding
            .job
            .lock()
            .unwrap()
            .as_ref()
            .filter(|j| j.id == id)
            .map(|j| j.stage)
    }

    /// The audit record: who added which machine as which user, and how it
    /// went. Never the password.
    fn add_audit(&self, id: &str, outcome: &str) {
        let Some(j) = self
            .adding
            .job
            .lock()
            .unwrap()
            .clone()
            .filter(|j| j.id == id)
        else {
            return;
        };
        self.record(
            "fleet.add",
            None,
            None,
            None,
            Some(&j.by),
            json!({"machine": j.name, "address": j.address, "user": j.user,
                "outcome": outcome, "station": j.station}),
        );
    }

    fn add_fail(&self, id: &str, kind: &'static str) {
        let station = self.station.clone();
        let known = self.known_hosts_file().display().to_string();
        let changed = self.add_update(id, |j| {
            let name = &j.name;
            let text = match kind {
                "ssh_off" => format!("SSH is not turned on on {name}: on a Mac turn on Remote Login in System Settings, General, Sharing; on Linux install and start the SSH server; or turn on Tailscale SSH there; then try again."),
                "wrong_password" => format!("{name} did not accept that user name and password: check both and try again."),
                "no_password_login" => format!("{name} refused without asking for a password, so it does not take passwords over SSH: turn on password login or Tailscale SSH there, or give this station's SSH key access, then try again."),
                "unreachable" => format!("{name} did not answer, so it is probably asleep or off: wake it, check that Tailscale is connected there, and try again."),
                "host_key" => format!("{name} answered with a different SSH host key than the one saved for it before, so nothing was sent: if that machine was reinstalled, delete the line for {} from {known} and try again; if it was not, do not connect.", j.address),
                "unsupported" => format!("spandeck has no release for {name}'s system: it runs on macOS and Linux, on Apple silicon, arm64 and x86-64."),
                "network" => format!("{name} could not reach GitHub to download spandeck: check its internet connection and try again."),
                "install" => format!("The install script failed on {name}: its last lines are below, and the install command in the README, run on that machine, shows the same."),
                "start" => format!("spandeck was installed on {name} but did not start: on that machine run `spandeck daemon` to see why."),
                "join" => format!("spandeck is running on {name} but could not ask to join this fleet: on that machine run `spandeck fleet --join {station}`."),
                "refused" => format!("You refused {name}, so spandeck is installed there but it is not in this fleet: to ask again, run `spandeck fleet --join {station}` on it."),
                "expired" => format!("Nobody approved {name} in time: run `spandeck fleet --join {station}` on it to get a new code."),
                "not_seen" => format!("{name} was approved but has not appeared yet: it should show up by itself, and if it does not, check that spandeck is still running there."),
                "timeout" => format!("Adding {name} took more than ten minutes and was stopped: try again."),
                "no_ssh" => "This station could not run its SSH command: install the OpenSSH client here (or fix add_machine_cmd in config.toml) and try again.".to_string(),
                _ => format!("This station could not connect to {name} over SSH: the last lines below say why."),
            };
            j.stage = "failed";
            j.problem = Some((kind, text));
        });
        if changed {
            self.add_audit(id, &format!("failed: {kind}"));
        }
    }

    /// Start adding a machine. Returns at once; progress arrives as events
    /// and is read with `fleet.add_status`.
    pub fn add_start(
        &self,
        machine: &str,
        user: &str,
        password: Option<Secret>,
        by: &str,
    ) -> Result<Value> {
        if !self.add_enabled() {
            return Err(Error::Denied(
                "adding a machine is off on this station. Turn it on from the Add a machine tile, or put add_machines = true in its config.toml.".into(),
            ));
        }
        // The user name goes on a command line: letters, digits, dot, dash
        // and underscore only, and never a leading dash.
        let user = user.trim();
        if user.is_empty()
            || user.len() > 64
            || user.starts_with('-')
            || !user
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(invalid(
                "that is not a user name: use the short name of the account on that machine",
            ));
        }
        // The machine's name inside a quoted shell word on the other side.
        if !self
            .station
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(invalid(
                "this station's name has characters that cannot be sent to another machine; rename it in config.toml",
            ));
        }
        // The address is the one Tailscale gives for the machine picked. A
        // caller names a machine, never an address.
        let found = self
            .add_candidates_now()
            .ok_or_else(|| invalid("Tailscale is not answering on this station"))?;
        let cand = found.into_iter().find(|c| c.id == machine).ok_or_else(|| {
            Error::NotFound(
                "that machine is not online under your Tailscale login, or is already a station"
                    .into(),
            )
        })?;
        let job = Job {
            id: new_id("add"),
            machine: cand.id.clone(),
            name: cand.name.clone(),
            address: cand.address.clone(),
            os: cand.os.clone(),
            user: user.to_string(),
            by: by.to_string(),
            stage: "connecting",
            problem: None,
            lines: VecDeque::new(),
            code: None,
            request: None,
            station: None,
            started_ms: now_ms(),
            cancel: Arc::new(tokio::sync::Notify::new()),
        };
        {
            let mut slot = self.adding.job.lock().unwrap();
            if let Some(j) = slot.as_ref().filter(|j| j.running()) {
                return Err(invalid(format!(
                    "{} is being added now: wait for it, or cancel it first",
                    j.name
                )));
            }
            *slot = Some(job.clone());
        }
        self.record(
            "fleet.add",
            None,
            None,
            None,
            Some(by),
            json!({"machine": job.name, "address": job.address, "user": job.user,
                "outcome": "started", "with_password": password.is_some()}),
        );
        let (me, view) = (self.clone(), job.view());
        tokio::spawn(async move { me.add_run(job, password).await });
        Ok(view)
    }

    /// Stop an add that is running, and clear one that has ended.
    pub fn add_cancel(&self, by: &str) -> Value {
        let Some(job) = self.adding.job.lock().unwrap().take() else {
            return json!({"cancelled": false});
        };
        if job.running() {
            // notify_one leaves a permit, so a task that is not waiting at
            // this instant still stops.
            job.cancel.notify_one();
            self.record(
                "fleet.add",
                None,
                None,
                None,
                Some(by),
                json!({"machine": job.name, "address": job.address, "user": job.user,
                    "outcome": "cancelled", "at": job.stage}),
            );
        }
        self.emit(
            "fleet.add.updated",
            None,
            None,
            json!({"id": job.id, "stage": "cleared"}),
        );
        json!({"cancelled": job.running()})
    }

    /// The owner answered a join request. If it is the one this add is
    /// waiting on, move on.
    pub(crate) fn add_decided(&self, code: &str, approved: bool) {
        let id = {
            let job = self.adding.job.lock().unwrap();
            match job.as_ref() {
                Some(j) if j.stage == "approve" && j.code.as_deref() == Some(code) => j.id.clone(),
                _ => return,
            }
        };
        if approved {
            self.add_update(&id, |j| j.stage = "joined");
        } else {
            self.add_fail(&id, "refused");
        }
    }

    async fn add_run(self, job: Job, password: Option<Secret>) {
        let id = job.id.clone();
        let code = match self.add_install(&job, password, JoinBy::Code).await {
            Ok(code) => code,
            Err("cancelled") => return,
            Err("needs_password") => {
                if self.add_update(&id, |j| j.stage = "needs_password") {
                    self.add_audit(&id, "needs a password");
                }
                return;
            }
            Err(kind) => return self.add_fail(&id, kind),
        };
        // The code came over SSH from the other machine, so it is only
        // believed if this station holds a request that shows the same one.
        let Some((request, station, expires_ms)) = self.trust_station_request(&code) else {
            return self.add_fail(&id, "join");
        };
        self.add_update(&id, |j| {
            j.stage = "approve";
            j.code = Some(code);
            j.request = Some(request);
            j.station = Some(station.clone());
        });
        self.add_audit(&id, "installed; waiting for approval");
        let mut seen_by = None;
        loop {
            tokio::select! {
                _ = job.cancel.notified() => return,
                _ = tokio::time::sleep(Duration::from_millis(700)) => {}
            }
            match self.add_stage(&id) {
                Some("approve") if now_ms() > expires_ms => return self.add_fail(&id, "expired"),
                Some("approve") => {}
                Some("joined") => {
                    let by = *seen_by.get_or_insert(tokio::time::Instant::now() + SEEN_TIMEOUT);
                    self.discover_peers().await;
                    if self
                        .fleet
                        .peer(&station)
                        .is_some_and(|p| p.state == "online")
                    {
                        if self.add_update(&id, |j| j.stage = "done") {
                            self.add_audit(&id, "joined");
                            self.emit("fleet.updated", None, None, json!({}));
                        }
                        return;
                    }
                    if tokio::time::Instant::now() > by {
                        return self.add_fail(&id, "not_seen");
                    }
                }
                _ => return,
            }
        }
    }

    /// The command that reaches the machine. `add_machine_cmd` in the config
    /// replaces it. The script arrives on its standard input.
    fn add_command(&self, job: &Job) -> Vec<String> {
        let known = self.known_hosts_file().display().to_string();
        let template: Vec<String> = self.cfg.add_machine_cmd.clone().unwrap_or_else(|| {
            [
                "ssh",
                // The owner's own ssh config could send the connection
                // somewhere else, or switch host key checking off.
                "-F",
                "none",
                "-T",
                "-x",
                // First contact saves the key; a different key later is refused.
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "UserKnownHostsFile={known_hosts}",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
                "-o",
                "HashKnownHosts=no",
                "-o",
                "UpdateHostKeys=no",
                "-o",
                "ConnectTimeout=12",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "NumberOfPasswordPrompts=1",
                "-l",
                "{user}",
                "{host}",
                "sh",
                "-s",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        });
        template
            .iter()
            .map(|a| {
                a.replace("{user}", &job.user)
                    .replace("{host}", &job.address)
                    .replace("{known_hosts}", &known)
            })
            .collect()
    }

    /// What runs on the machine: the README's install script, then start at
    /// login, then the join request. Lines that begin `@spandeck` tell this
    /// station how far it got. It only uses commands every release has, since
    /// the release it installs may be older than this station.
    fn add_script(&self, join: JoinBy) -> String {
        let releases = match &self.cfg.update.url {
            Some(url) if !url.contains('\'') => {
                format!("SPANDECK_RELEASES='{}'\n", url.trim_end_matches('/'))
            }
            _ => String::new(),
        };
        let join_lines = match join {
            JoinBy::Code => format!(
                r#"out=$("$bin" --json fleet --join '{station}') || {{ printf '%s\n' "$out"; fail join; }}
code=$(printf '%s\n' "$out" | sed -n 's/.*"code": *"\([A-Z0-9]*\)".*/\1/p' | head -n 1)
[ -n "$code" ] || fail join
say "code $code""#,
                station = self.station
            ),
        };
        format!(
            r#"main() {{
set -u
say() {{ printf '@spandeck %s\n' "$*"; }}
fail() {{ say "fail $1"; exit 1; }}
say step install
case "$(uname -s)" in
  Darwin | Linux) ;;
  *) echo "This machine runs $(uname -s). spandeck runs on macOS and Linux only."; fail unsupported ;;
esac
case "$(uname -m)" in
  arm64 | aarch64 | x86_64 | amd64) ;;
  *) echo "This machine's processor is $(uname -m). There is no spandeck release for it."; fail unsupported ;;
esac
command -v curl >/dev/null 2>&1 || {{ echo "curl is not installed on this machine."; fail install; }}
{releases}SPANDECK_RELEASES=${{SPANDECK_RELEASES:-https://github.com/michaelbaker-dev/spandeck/releases}}
export SPANDECK_RELEASES
curl -fsSI -o /dev/null --max-time 20 "$SPANDECK_RELEASES/latest" || {{ echo "This machine could not reach $SPANDECK_RELEASES"; fail network; }}
sh -s <<'SPANDECK_INSTALL_SH' || fail install
{INSTALL_SH}
SPANDECK_INSTALL_SH
bin=${{SPANDECK_BIN_DIR:-$HOME/.local/bin}}/spandeck
say step start
"$bin" service --install >/dev/null || fail start
started=no
case "$(uname -s)" in
  Darwin)
    launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/dev.spandeck.spandeckd.plist" 2>/dev/null && started=yes ;;
  *)
    systemctl --user daemon-reload 2>/dev/null && systemctl --user enable --now spandeckd 2>/dev/null && started=yes
    loginctl enable-linger "$(id -un)" 2>/dev/null || true ;;
esac
if [ "$started" = no ]; then
  echo "The login service could not be started from here, so spandeck was started directly. It starts by itself at the next login."
  (nohup "$bin" daemon >/dev/null 2>&1 &)
fi
n=0
until "$bin" status >/dev/null 2>&1; do
  n=$((n + 1))
  [ "$n" -le 30 ] || {{ echo "spandeck was installed but did not start."; fail start; }}
  sleep 1
done
echo "spandeck is running."
say step join
{join_lines}
}}
main </dev/null
"#
        )
    }

    /// Connect, install, start and ask to join. `Ok` is the join code the new
    /// station shows; `Err` is the kind of problem.
    async fn add_install(
        &self,
        job: &Job,
        password: Option<Secret>,
        join: JoinBy,
    ) -> std::result::Result<String, &'static str> {
        let id = job.id.as_str();
        let had_password = password.is_some();
        let argv = self.add_command(job);
        let Some((program, args)) = argv.split_first() else {
            return Err("no_ssh");
        };
        // ssh asks for the password by running a helper. The helper is this
        // binary; it gets the password over this socket, once, and only for
        // a password prompt. Nothing secret is in ssh's arguments or
        // environment: the nonce only proves the helper is ssh's.
        let sock = self
            .paths
            .socket()
            .with_file_name(format!("a{}.sock", &random_token()[..8]));
        let _ = std::fs::remove_file(&sock);
        let nonce = random_token();
        let listener = tokio::net::UnixListener::bind(&sock).map_err(|_| "no_ssh")?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
        }
        struct Tidy(PathBuf, tokio::task::JoinHandle<()>);
        impl Drop for Tidy {
            fn drop(&mut self) {
                // Ends the helper's listener, which drops (and wipes) a
                // password ssh never asked for.
                self.1.abort();
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let asked = Arc::new(AtomicBool::new(false));
        let _tidy = Tidy(
            sock.clone(),
            tokio::spawn(askpass_serve(
                listener,
                nonce.clone(),
                password,
                asked.clone(),
            )),
        );

        let mut child = tokio::process::Command::new(program)
            .args(args)
            .env("SSH_ASKPASS", &self.exe)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("SPANDECK_ASKPASS_SOCK", &sock)
            .env("SPANDECK_ASKPASS_NONCE", &nonce)
            .env_remove("SPANDECK_TOKEN")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "no_ssh")?;
        if let Some(mut stdin) = child.stdin.take() {
            let script = self.add_script(join);
            tokio::spawn(async move {
                let _ = stdin.write_all(script.as_bytes()).await;
                let _ = stdin.shutdown().await;
            });
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(bool, String)>();
        fn pump<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
            from: Option<R>,
            is_err: bool,
            tx: tokio::sync::mpsc::UnboundedSender<(bool, String)>,
        ) {
            let Some(from) = from else { return };
            tokio::spawn(async move {
                let mut reader = BufReader::new(from);
                let mut raw = vec![];
                while matches!(reader.read_until(b'\n', &mut raw).await, Ok(n) if n > 0) {
                    if tx.send((is_err, clean_line(&raw))).is_err() {
                        break;
                    }
                    raw.clear();
                }
            });
        }
        pump(child.stdout.take(), false, tx.clone());
        pump(child.stderr.take(), true, tx);

        let deadline = tokio::time::Instant::now() + ADD_TIMEOUT;
        let (mut code, mut failed, mut said) = (None::<String>, None::<&'static str>, vec![]);
        let mut missing = false;
        loop {
            let (is_err, line) = tokio::select! {
                next = rx.recv() => match next { Some(l) => l, None => break },
                // Dropping the child on the way out kills it.
                _ = job.cancel.notified() => return Err("cancelled"),
                _ = tokio::time::sleep_until(deadline) => return Err("timeout"),
            };
            if let (false, Some(mark)) = (is_err, line.strip_prefix("@spandeck ")) {
                match mark.split_once(' ').unwrap_or((mark, "")) {
                    ("step", step) => {
                        let stage = match step {
                            "install" => "installing",
                            "start" => "starting",
                            "join" => "joining",
                            _ => continue,
                        };
                        self.add_update(id, |j| j.stage = stage);
                    }
                    ("code", c) => code = Some(c.to_string()),
                    ("fail", kind) => {
                        failed = Some(match kind {
                            "unsupported" => "unsupported",
                            "network" => "network",
                            "start" => "start",
                            "join" => "join",
                            _ => "install",
                        })
                    }
                    _ => {}
                }
                continue;
            }
            if line.trim().is_empty() {
                continue;
            }
            if is_err {
                said.push(line.clone());
            }
            // The download answering "not found": no release for that system.
            missing |= line.contains("returned error: 404");
            {
                let mut slot = self.adding.job.lock().unwrap();
                let Some(j) = slot.as_mut().filter(|j| j.id == id) else {
                    return Err("cancelled");
                };
                if j.lines.len() >= KEPT_LINES {
                    j.lines.pop_front();
                }
                j.lines.push_back(line.clone());
            }
            self.emit(
                "fleet.add.output",
                None,
                None,
                json!({"id": id, "line": line}),
            );
        }
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .ok()
            .and_then(|s| s.ok());
        if let Some(kind) = failed {
            return Err(if kind == "install" && missing {
                "unsupported"
            } else {
                kind
            });
        }
        if !status.is_some_and(|s| s.success()) {
            let password = match (had_password, asked.load(Ordering::SeqCst)) {
                (false, _) => Password::None,
                (true, true) => Password::Asked,
                (true, false) => Password::NeverAsked,
            };
            return Err(ssh_problem(&said, password));
        }
        code.filter(|c| (4..=12).contains(&c.len()) && c.chars().all(|c| c.is_ascii_alphanumeric()))
            .ok_or("join")
    }
}

/// Answer ssh's askpass helper. The password goes out once, for a prompt
/// that asks for a password, to a helper that knows the nonce. A second
/// prompt (the first password was wrong) gets nothing, so ssh gives up
/// instead of trying again.
async fn askpass_serve(
    listener: tokio::net::UnixListener,
    nonce: String,
    mut password: Option<Secret>,
    asked: Arc<AtomicBool>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read.take(4096)).lines();
        let two = tokio::time::timeout(Duration::from_secs(5), async {
            let nonce = lines.next_line().await.ok().flatten()?;
            let prompt = lines.next_line().await.ok().flatten()?;
            Some((nonce, prompt))
        })
        .await;
        let Ok(Some((theirs, prompt))) = two else {
            continue;
        };
        if !crate::rpc::constant_eq(&theirs, &nonce) {
            continue;
        }
        let prompt = prompt.to_lowercase();
        if prompt.contains("password") && !prompt.contains("passphrase") {
            if let Some(secret) = password.take() {
                asked.store(true, Ordering::SeqCst);
                let _ = write.write_all(&secret.0).await;
            }
        }
        let _ = write.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The host key rule: the address ssh is given must be a Tailscale
    // address. A bug here would let a status line point ssh anywhere.
    #[test]
    fn only_tailscale_addresses_are_connected_to() {
        for yes in [
            "100.64.0.1",
            "100.101.102.103",
            "100.127.255.254",
            "fd7a:115c:a1e0::1",
        ] {
            assert!(is_tailscale_address(yes.parse().unwrap()), "{yes}");
        }
        for no in [
            "100.63.0.1",
            "100.128.0.1",
            "127.0.0.1",
            "192.168.1.5",
            "10.0.0.1",
            "fd7b::1",
            "::1",
        ] {
            assert!(!is_tailscale_address(no.parse().unwrap()), "{no}");
        }
    }

    // Each failure the tile must explain is told apart by what ssh printed.
    #[test]
    fn what_ssh_printed_says_which_problem_it_was() {
        let p = |text: &str, pw: bool| {
            let pw = if pw { Password::Asked } else { Password::None };
            ssh_problem(&[text.to_string()], pw)
        };
        assert_eq!(
            ssh_problem(
                &["Permission denied (publickey).".to_string()],
                Password::NeverAsked
            ),
            "no_password_login"
        );
        assert_eq!(
            p(
                "ssh: connect to host 100.64.0.2 port 22: Connection refused",
                false
            ),
            "ssh_off"
        );
        assert_eq!(
            p(
                "ssh: connect to host 100.64.0.2 port 22: Operation timed out",
                false
            ),
            "unreachable"
        );
        assert_eq!(
            p(
                "ssh: connect to host 100.64.0.2 port 22: No route to host",
                true
            ),
            "unreachable"
        );
        assert_eq!(
            p(
                "me@100.64.0.2: Permission denied (publickey,password).",
                false
            ),
            "needs_password"
        );
        assert_eq!(
            p(
                "me@100.64.0.2: Permission denied (publickey,password).",
                true
            ),
            "wrong_password"
        );
        assert_eq!(
            p("@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @", true),
            "host_key"
        );
        assert_eq!(p("Host key verification failed.", false), "host_key");
        assert_eq!(
            p(
                "kex_exchange_identification: read: Connection reset by peer",
                false
            ),
            "connect"
        );
    }
}
