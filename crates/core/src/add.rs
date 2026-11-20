//! Adding a machine to the fleet from the Control Deck.
//!
//! The owner picks a machine under their Tailscale login that is not a station
//! yet. This station connects to it over SSH, runs the install script from
//! the README there, starts spandeck, and has it ask to join this fleet the
//! way `spandeck fleet --join` does. The owner then approves the code as for
//! any other station.
//!
//! Two more things go the same way, over the same connection and under the
//! same switch (15 Nov 2026). A station that is running but is not in this
//! fleet is added without the owner going to it: nothing is installed, it
//! is only made to ask (`add_station_start`). And spandeck is uninstalled
//! from a station (`uninstall_start`): the station there closes its panes,
//! stops and takes its login service out, as it does when it is removed
//! with "also stop spandeck there", and then the programs are deleted.
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
use crate::supply::{self, Force, NoSupply, Source, Supplied};

/// The script the README tells a person to run. The same text is sent to the
/// machine being added, so the two cannot drift apart.
pub(crate) const INSTALL_SH: &str = include_str!("../../../scripts/install.sh");
/// The same for a Windows machine.
const INSTALL_PS1: &str = include_str!("../../../scripts/install.ps1");

/// How long one add may take from connecting to the join request, not
/// counting the time a release is being fetched here and sent (below).
const ADD_TIMEOUT: Duration = Duration::from_secs(600);
/// A release that is being sent over SSH: this station has this long to get
/// it (a download here, or its own files), the sending may take this long
/// in all, and is stopped when the session takes nothing for this long. So
/// a slow link is given time while it moves: 18 MB in 50 minutes is 6 KB a
/// second.
const SUPPLY_PREPARE: Duration = Duration::from_secs(420);
const SUPPLY_TIMEOUT: Duration = Duration::from_secs(3000);
const SUPPLY_STALL: Duration = Duration::from_secs(120);
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

/// Why a job stopped, in the dialog's words: what did not work (`text`),
/// what had worked before that (`worked`), and the one thing to do next.
#[derive(Clone)]
struct Problem {
    kind: &'static str,
    text: String,
    worked: Option<String>,
    next: Option<String>,
}

#[derive(Clone)]
struct Job {
    id: String,
    /// "add": install on a machine and have it join. "join": a station that
    /// is running already is made to ask. "uninstall": spandeck is taken
    /// off a station.
    kind: &'static str,
    /// Uninstall: also delete the station's history, archive and settings.
    erase: bool,
    /// The fleet-mates the station says it brings, once it has asked.
    with: Vec<String>,
    /// Join: whether the station there is new enough to bring its mates.
    brings_mates: bool,
    /// The join step again, for a machine spandeck was installed on by the
    /// try before this one: nothing is downloaded or installed.
    rejoin: bool,
    /// The version of spandeck that machine says it runs, once it runs.
    version: Option<String>,
    /// What spandeck there said when it could not ask to join. Text from
    /// another machine: shown as text.
    why: Option<String>,
    /// The owner asked by hand for the release to be sent over SSH.
    send: Force,
    /// What is happening that the steps do not say, in the owner's words:
    /// "monitoring cannot reach GitHub: sending spandeck from spark2…".
    note: Option<String>,
    /// Why there was nothing to send, in the owner's words.
    told: Option<String>,
    machine: String,
    name: String,
    address: String,
    os: String,
    user: String,
    by: String,
    /// connecting, installing, starting, joining, approve, joined,
    /// stopping, removing: in progress. needs_password, failed, done:
    /// finished.
    stage: &'static str,
    problem: Option<Problem>,
    lines: VecDeque<String>,
    code: Option<String>,
    request: Option<String>,
    station: Option<String>,
    started_ms: u64,
    cancel: Arc<tokio::sync::Notify>,
}

impl Job {
    fn new(kind: &'static str, name: &str, address: &str, os: &str, user: &str, by: &str) -> Job {
        Job {
            id: new_id("add"),
            kind,
            erase: false,
            with: vec![],
            brings_mates: false,
            rejoin: false,
            version: None,
            why: None,
            send: Force::No,
            note: None,
            told: None,
            machine: String::new(),
            name: name.to_string(),
            address: address.to_string(),
            os: os.to_string(),
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
        }
    }

    fn running(&self) -> bool {
        !matches!(self.stage, "needs_password" | "failed" | "done")
    }

    /// Whether "Try again" can go straight to the join step: spandeck was
    /// installed and started there by this job (or the one it repeats), and
    /// what failed came after that.
    fn can_ask_again(&self) -> bool {
        let Some(problem) = self.problem.as_ref().filter(|_| self.stage == "failed") else {
            return self.rejoin && self.stage == "needs_password";
        };
        match self.kind {
            "add" => matches!(problem.kind, "join" | "refused" | "expired"),
            "join" if self.rejoin => problem.kind != "not_running",
            _ => false,
        }
    }

    /// The ship's log entry this job writes.
    fn log_kind(&self) -> &'static str {
        match self.kind {
            "uninstall" => "fleet.uninstall",
            _ => "fleet.add",
        }
    }

    fn view(&self) -> Value {
        json!({
            "id": self.id, "kind": self.kind, "erase": self.erase, "with": self.with,
            "brings_mates": self.brings_mates, "rejoin": self.rejoin, "version": self.version,
            "can_ask_again": self.can_ask_again(), "note": self.note,
            "machine": self.machine, "name": self.name, "address": self.address,
            "os": self.os, "user": self.user, "by": self.by, "stage": self.stage,
            "running": self.running(),
            "problem": self.problem.as_ref().map(|p| json!({"kind": p.kind, "text": p.text,
                "worked": p.worked, "next": p.next})),
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

/// Mac, Linux and Windows machines under the owner's login that are online
/// and are not stations. Phones, tagged machines and shared machines are out.
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
            || !matches!(os.as_str(), "macOS" | "linux" | "windows")
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
            os: match os.as_str() {
                "macOS" => "macOS",
                "windows" => "Windows",
                _ => "Linux",
            }
            .to_string(),
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
            j.log_kind(),
            None,
            None,
            None,
            Some(&j.by),
            json!({"machine": j.name, "address": j.address, "user": j.user,
                "outcome": outcome, "station": j.station}),
        );
    }

    fn add_fail(&self, id: &str, kind: &'static str) {
        // The stations of this fleet, this one first: any of them can be
        // asked, and the words name them all.
        let mut fleet = vec![self.station.clone()];
        fleet.extend(self.fleet_members().into_iter().map(|p| p.station));
        let stations = fleet.join(", ");
        let known = self.known_hosts_file().display().to_string();
        let changed = self.add_update(id, |j| {
            let name = &j.name;
            let windows = j.os == "Windows";
            let uninstall = j.kind == "uninstall";
            // What had worked when the join step failed, and what to do.
            let (mut worked, mut next) = (None, None);
            if matches!(kind, "join" | "refused" | "expired") {
                worked = Some(match (j.kind, &j.version) {
                    ("add", Some(v)) => format!("spandeck {v} was installed on {name} and is running."),
                    ("add", None) => format!("spandeck was installed on {name} and is running."),
                    (_, Some(v)) => format!("spandeck {v} is installed on {name} and is running."),
                    _ => format!("spandeck is running on {name}."),
                });
                let command = format!("Or on that machine: `spandeck fleet --join <any station of your fleet>` (its stations: {stations}).");
                next = Some(if j.kind == "add" || j.rejoin {
                    format!("Try again: it only asks again, and nothing is downloaded or installed a second time. {command}")
                } else {
                    format!("Try again. {command}")
                });
            }
            let text = match kind {
                "not_running" => format!("spandeck's command was not found on {name}, or spandeck is not answering there: the last lines below say which. Start it on that machine (`spandeck daemon`), or use Add a machine to install it again."),
                "stop" => format!("spandeck on {name} did not stop when it was asked to, so nothing was deleted there: the last lines below say why. Try again, or stop it on that machine."),
                "remove" => format!("spandeck was stopped on {name} and no longer starts at login, but its programs could not all be deleted: the last lines below say which."),
                "timeout" if uninstall => format!("Uninstalling spandeck on {name} took more than ten minutes and was stopped: try again."),
                "ssh_off" if windows => format!("The OpenSSH server is not installed or not running on {name}: on that machine open Settings, System, Optional features, add OpenSSH Server, then in a PowerShell window run as administrator: Start-Service sshd; Set-Service sshd -StartupType Automatic. Then try again."),
                "blocked" => format!("Windows on {name} refused to run spandeck: Smart App Control (or another application control policy) blocks programs it does not know, and spandeck's releases are not signed. spandeck was downloaded there but cannot run until that is turned off in Windows Security, App & browser control, Smart App Control settings; Windows does not let it be turned on again without a reset, so that is yours to decide."),
                "start" if windows => format!("spandeck was installed on {name} but did not start: it starts when you log on there, so log on at that machine (a Windows station runs only while you are logged on), or run `spandeck daemon` there to see why."),
                "no_password_login" if windows => format!("{name} refused without asking for a password, so its OpenSSH server does not take passwords: allow PasswordAuthentication in C:\\ProgramData\\ssh\\sshd_config there, or give this station's SSH key access (for an administrator account that is C:\\ProgramData\\ssh\\administrators_authorized_keys), then try again."),
                "ssh_off" => format!("SSH is not turned on on {name}: on a Mac turn on Remote Login in System Settings, General, Sharing; on Linux install and start the SSH server; or turn on Tailscale SSH there; then try again."),
                "wrong_password" => format!("{name} did not accept that user name and password: check both and try again."),
                "no_password_login" => format!("{name} refused without asking for a password, so it does not take passwords over SSH: turn on password login or Tailscale SSH there, or give this station's SSH key access, then try again."),
                "unreachable" => format!("{name} did not answer, so it is probably asleep or off: wake it, check that Tailscale is connected there, and try again."),
                "host_key" => format!("{name} answered with a different SSH host key than the one saved for it before, so nothing was sent: if that machine was reinstalled, delete the line for {} from {known} and try again; if it was not, do not connect.", j.address),
                "unsupported" => format!("spandeck has no release for {name}'s system: it runs on macOS and Linux on Apple silicon, arm64 and x86-64, and on Windows on x86-64."),
                "network" => format!("{name} could not reach GitHub to download spandeck: check its internet connection and try again."),
                // Nothing to send, a release that is not there, a download
                // here that failed its check: the words were made when it
                // happened (`no_supply_words`).
                "no_release" | "no_build" | "supply_checksum" | "own_copy" if j.told.is_some() => j.told.clone().unwrap_or_default(),
                "transfer" => format!("What {} sent did not arrive whole on {name}, so nothing was installed there: the last lines below say what arrived. Try again.", self.station),
                "stalled" => format!("Sending spandeck to {name} over SSH stopped: nothing moved for two minutes, or it took more than fifty. Nothing was installed there. Try again when the connection to {name} is better."),
                "supply" => format!("{name} asked {} to send spandeck over SSH and was not answered: the last lines below say why. Try again.", self.station),
                "not_fetched" => format!("{} did not have the release to send to {name} within seven minutes (its own download was too slow), so nothing was sent or installed. Try again.", self.station),
                "install" => format!("The install script failed on {name}: its last lines are below, and the install command in the README, run on that machine, shows the same."),
                "start" => format!("spandeck was installed on {name} but did not start: on that machine run `spandeck daemon` to see why."),
                "join" => join_failure(name, &stations, j.why.as_deref()),
                "refused" => format!("You refused {name}, so it is not in your fleet."),
                "expired" => format!("Nobody approved {name} in time, so it is not in your fleet."),
                "not_seen" => format!("{name} was approved but has not appeared yet: it should show up by itself, and if it does not, check that spandeck is still running there."),
                "timeout" => format!("Adding {name} took more than ten minutes (not counting the time a release was being sent to it) and was stopped: try again."),
                "no_ssh" => "This station could not run its SSH command: install the OpenSSH client here (or fix add_machine_cmd in config.toml) and try again.".to_string(),
                _ => format!("This station could not connect to {name} over SSH: the last lines below say why."),
            };
            j.stage = "failed";
            j.problem = Some(Problem {
                kind,
                text,
                worked,
                next,
            });
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
        send: Force,
        by: &str,
    ) -> Result<Value> {
        let user = self.add_checks(user)?;
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
        let job = Job::new("add", &cand.name, &cand.address, &cand.os, &user, by);
        self.add_begin(
            Job {
                machine: cand.id.clone(),
                send,
                ..job
            },
            password,
        )
    }

    /// "Try again" after the join step failed (the owner's report of 17 Nov
    /// 2026: it downloaded and installed everything a second time, or, once
    /// this station had seen spandeck running there, said the machine was
    /// "already a station"). spandeck is installed and running on that
    /// machine, so this connects to the address the last job had, which was
    /// checked then, and only has spandeck there ask again.
    pub fn add_again_start(&self, user: &str, password: Option<Secret>, by: &str) -> Result<Value> {
        let user = self.add_checks(user)?;
        let last = self
            .adding
            .job
            .lock()
            .unwrap()
            .clone()
            .filter(|j| j.can_ask_again())
            .ok_or_else(|| {
                invalid("there is no machine here whose request to join can be made again: use Add a machine")
            })?;
        let job = Job::new("join", &last.name, &last.address, &last.os, &user, by);
        self.add_begin(
            Job {
                machine: last.machine,
                rejoin: true,
                version: last.version,
                // What was installed is a release that brings its mates.
                brings_mates: true,
                ..job
            },
            password,
        )
    }

    /// What every connection to another machine needs first: the owner's
    /// switch is on, and the names that go on a command line are plain.
    fn add_checks(&self, user: &str) -> Result<String> {
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
        Ok(user.to_string())
    }

    /// A station this one can see, as a machine to connect to: its address
    /// and what it runs. The caller names a station, never an address; the
    /// address is the one the station was found at, and it must be a
    /// Tailscale address or one the owner wrote in this station's config.
    fn station_target(&self, station: &str) -> Result<(Peer, String, String)> {
        let peer = self
            .fleet
            .peer(station)
            .ok_or_else(|| Error::NotFound(format!("no station called {station} is known here")))?;
        if peer.state == "unreachable" {
            return Err(invalid(format!(
                "{station} is not answering, so it cannot be reached over SSH either: wake it or start it first"
            )));
        }
        let host = host_of(&peer.addr)
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let tailnet = host.parse::<IpAddr>().is_ok_and(is_tailscale_address);
        if !tailnet && !self.cfg.peers.contains(&peer.addr) {
            return Err(invalid(format!(
                "{station} was found at {host}, which is not a Tailscale address, so this station does not connect to it"
            )));
        }
        // What it runs: it says so itself; an older one does not, and then
        // Tailscale does.
        let said = peer.info["os"].as_str().map(str::to_lowercase);
        let os = said.or_else(|| {
            let status = tokio::task::block_in_place(tailscale_status)?;
            status
                .get("Peer")?
                .as_object()?
                .values()
                .find(|p| {
                    p["TailscaleIPs"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|i| i.as_str() == Some(host.as_str())))
                })?
                .get("OS")?
                .as_str()
                .map(str::to_lowercase)
        });
        let os = match os.as_deref() {
            Some("macos") => "macOS",
            Some("windows") => "Windows",
            _ => "Linux",
        };
        Ok((peer, host, os.to_string()))
    }

    /// "Add X to this fleet": a station that is running but is not in this
    /// fleet is made to ask to join it, over SSH, so the owner does not go
    /// to that machine. Nothing is installed. The code it shows is approved
    /// here as for any station, and its fleet-mates come with it.
    pub fn add_station_start(
        &self,
        station: &str,
        user: &str,
        password: Option<Secret>,
        by: &str,
    ) -> Result<Value> {
        let user = self.add_checks(user)?;
        let (peer, address, os) = self.station_target(station)?;
        if peer.state != "not_linked" {
            return Err(invalid(format!("{station} is already in this fleet")));
        }
        let job = Job::new("join", station, &address, &os, &user, by);
        self.add_begin(
            Job {
                station: Some(station.to_string()),
                brings_mates: peer.info["merge"] == json!(true),
                ..job
            },
            password,
        )
    }

    /// "Uninstall spandeck on X": for a station of this fleet, first the
    /// fleet removal (its panes closed, the fleet's key replaced, the
    /// station forgotten), exactly as "Remove" does; then, over SSH, the
    /// station there stops, takes its login service out, and its programs
    /// are deleted. With `erase` its history, archive and settings go too.
    /// The owner's project folders are never touched.
    pub async fn uninstall_start(
        &self,
        station: &str,
        user: &str,
        password: Option<Secret>,
        erase: bool,
        caller: &crate::daemon::Caller,
    ) -> Result<Value> {
        let user = self.add_checks(user)?;
        if station == self.station {
            return Err(invalid(
                "this is the station you are using, and it cannot uninstall itself: do it from another station",
            ));
        }
        // Where to connect is settled before the station is forgotten.
        let (in_fleet, address, os) = match self.station_target(station) {
            Ok((peer, address, os)) => (peer.state != "not_linked", address, os),
            // The try before this one took the station out of the fleet and
            // forgot it, and then asked for a password: until the next look
            // lists that machine again, nothing is known of it here. The
            // password is for that job, so this try goes to the address that
            // one had, which was checked then. (Found when the deck began to
            // show the password box at once: a password entered within a
            // fraction of a second was refused with "no station called X is
            // known here".)
            Err(unknown) => {
                let asked = self.adding.job.lock().unwrap();
                match asked.as_ref().filter(|j| {
                    j.kind == "uninstall"
                        && j.stage == "needs_password"
                        && j.station.as_deref() == Some(station)
                }) {
                    Some(j) => (false, j.address.clone(), j.os.clone()),
                    None => return Err(unknown),
                }
            }
        };
        self.add_slot_free()?;
        let removed = if in_fleet {
            Some(self.fleet_remove(station, false, caller).await?)
        } else {
            None
        };
        let job = Job::new("uninstall", station, &address, &os, &user, &caller.who);
        let view = self.add_begin(
            Job {
                station: Some(station.to_string()),
                erase,
                ..job
            },
            password,
        )?;
        Ok(json!({"job": view, "removed": removed}))
    }

    fn add_slot_free(&self) -> Result<()> {
        match self
            .adding
            .job
            .lock()
            .unwrap()
            .as_ref()
            .filter(|j| j.running())
        {
            Some(j) => Err(invalid(format!(
                "{} is being worked on now: wait for it, or cancel it first",
                j.name
            ))),
            None => Ok(()),
        }
    }

    /// Take the one slot and start the job.
    fn add_begin(&self, job: Job, password: Option<Secret>) -> Result<Value> {
        {
            let mut slot = self.adding.job.lock().unwrap();
            if let Some(j) = slot.as_ref().filter(|j| j.running()) {
                return Err(invalid(format!(
                    "{} is being worked on now: wait for it, or cancel it first",
                    j.name
                )));
            }
            *slot = Some(job.clone());
        }
        self.record(
            job.log_kind(),
            None,
            None,
            None,
            Some(&job.by),
            json!({"machine": job.name, "address": job.address, "user": job.user,
                "outcome": "started", "with_password": password.is_some(),
                "what": job.kind, "erase": job.erase}),
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
                job.log_kind(),
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
        if job.kind == "uninstall" {
            // It is gone: it leaves the list, and is a machine to add again.
            self.discover_peers().await;
            if self.add_update(&id, |j| j.stage = "done") {
                self.add_audit(
                    &id,
                    if job.erase {
                        "uninstalled; everything erased"
                    } else {
                        "uninstalled"
                    },
                );
                self.emit("fleet.updated", None, None, json!({}));
            }
            return;
        }
        // The code came over SSH from the other machine, so it is only
        // believed if this station holds a request that shows the same one.
        let Some((request, station, with, expires_ms)) = self.trust_station_request(&code) else {
            return self.add_fail(&id, "join");
        };
        self.add_update(&id, |j| {
            j.stage = "approve";
            j.code = Some(code);
            j.request = Some(request);
            j.station = Some(station.clone());
            j.with = with;
        });
        self.add_audit(
            &id,
            if job.kind == "join" {
                "asked to join; waiting for approval"
            } else {
                "installed; waiting for approval"
            },
        );
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
                if spandeck_terminal::sys::WINDOWS {
                    "GlobalKnownHostsFile=NUL"
                } else {
                    "GlobalKnownHostsFile=/dev/null"
                },
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
            ]
            .iter()
            .map(|s| s.to_string())
            // What reads the script there. Windows: PowerShell reads all of
            // standard input, then runs it. The line has no `$` and no
            // quote of its own inside, so it means the same whether that
            // machine's OpenSSH hands it to cmd.exe (the default) or to
            // PowerShell.
            .chain(if job.os == "Windows" {
                vec![
                    "powershell".to_string(),
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-Command".to_string(),
                    // An install may be sent the release after its script,
                    // so its script must not be read to the end of input.
                    if job.kind == "add" {
                        WINDOWS_READS_A_LINE
                    } else {
                        WINDOWS_READS_STDIN
                    }
                    .to_string(),
                ]
            } else {
                vec!["sh".to_string(), "-s".to_string()]
            })
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
    ///
    /// A machine that cannot download the release (it has no curl, the
    /// release page does not answer it, or the download does not finish or
    /// does not match its checksum) asks this station for it and is sent it
    /// in this same session (supply.rs). The script is not read by its
    /// shell from the session's standard input (`supply::unix_boot_line`
    /// starts it), which is the script's file descriptor 3 until the
    /// install is over.
    fn add_script(&self, join: JoinBy, os: &str, send: Force) -> String {
        if os == "Windows" {
            return windows_script(
                &self.join_candidates(),
                self.cfg.update.url.as_deref(),
                join,
                send,
            );
        }
        unix_script(
            &self.join_candidates(),
            self.cfg.update.url.as_deref(),
            join,
            send,
        )
    }
}

fn unix_script(
    ask: &[(String, Option<String>)],
    releases: Option<&str>,
    join: JoinBy,
    send: Force,
) -> String {
    {
        let releases = match releases {
            Some(url) if !url.contains('\'') => {
                format!("SPANDECK_RELEASES='{}'\n", url.trim_end_matches('/'))
            }
            _ => String::new(),
        };
        let join_lines = unix_join_lines(ask, join);
        let receive = supply::unix_receive_lines();
        let asked = if send == Force::No { "" } else { "asked" };
        format!(
            r#"main() {{
set -u
say() {{ printf '@spandeck %s\n' "$*"; }}
fail() {{ say "fail $1"; exit 1; }}
say step install
case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-gnu ;;
  *) echo "This machine runs $(uname -s). spandeck runs on macOS and Linux only."; fail unsupported ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) echo "This machine's processor is $(uname -m). There is no spandeck release for it."; fail unsupported ;;
esac
{releases}SPANDECK_RELEASES=${{SPANDECK_RELEASES:-https://github.com/michaelbaker-dev/spandeck/releases}}
export SPANDECK_RELEASES
# The README's install script, as it is there.
spandeck_install() {{
sh -s <<'SPANDECK_INSTALL_SH'
{INSTALL_SH}
SPANDECK_INSTALL_SH
}}
{receive}
# Why the release has to be sent, if it has to be.
why={asked}
if [ -n "$why" ]; then
  :
elif ! command -v curl >/dev/null 2>&1; then
  echo "curl is not installed on this machine."
  why=nocurl
elif ! curl -fsSI -o /dev/null --max-time 20 "$SPANDECK_RELEASES/latest"; then
  echo "This machine could not reach $SPANDECK_RELEASES"
  why=network
else
  spandeck_install 3<&-
  case $? in
    0) ;;
    75) why=download ;;
    *) fail install ;;
  esac
fi
if [ -n "$why" ]; then
  spandeck_receive "$arch-$os" "$why" || fail supply
  SPANDECK_ARCHIVE=$supplied SPANDECK_ARCHIVE_SHA256=$supplied_sum SPANDECK_VERSION=$supplied_version
  export SPANDECK_ARCHIVE SPANDECK_ARCHIVE_SHA256 SPANDECK_VERSION
  spandeck_install 3<&- || fail install
  rm -rf "$supplied_dir"
  supplied_dir=
fi
# Nothing more is read from the session: what is started below must not hold it.
exec 3<&-
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
# A station that was already running here from an earlier install goes on
# running the program it was started from: it is brought to the one just
# installed, in place. (The owner's Mac, 17 Nov 2026: 0.4.11 was installed
# beside a running 0.4.10, which went on answering, and could not see the fleet.)
have=$("$bin" --version 2>/dev/null | sed -n 's/^spandeck \([0-9][0-9.]*\).*/\1/p' | head -n 1)
runs=$("$bin" status 2>/dev/null | sed -n 's/.*spandeck \([0-9][0-9.]*\).*/\1/p' | head -n 1)
if [ -n "$have" ] && [ -n "$runs" ] && [ "$have" != "$runs" ]; then
  echo "spandeck $runs was already running here: bringing it to $have."
  "$bin" upgrade "$bin" >/dev/null 2>&1 || echo "It could not be brought to $have: it still runs $runs."
  n=0
  until [ "$("$bin" status 2>/dev/null | sed -n 's/.*spandeck \([0-9][0-9.]*\).*/\1/p' | head -n 1)" = "$have" ]; do
    n=$((n + 1))
    [ "$n" -le 30 ] || break
    sleep 1
  done
fi
echo "spandeck is running."
say "version $("$bin" --version 2>/dev/null | head -n 1)"
say step join
{join_lines}
}}
main 3<&0 </dev/null; exit $?
"#
        )
    }
}

impl Daemon {
    /// What runs on a station that is to ask to join this fleet: nothing is
    /// installed. The command is looked for where the installer puts it,
    /// then on the PATH, then where a build from source leaves it.
    fn join_script(&self, join: JoinBy, os: &str) -> String {
        let ask = self.join_candidates();
        if os == "Windows" {
            let join_lines = windows_join_lines(&ask, join);
            return format!(
                r#"$ErrorActionPreference = 'Continue'
function Say($m) {{ Write-Output "@spandeck $m" }}
function Fail($kind) {{ Say "fail $kind"; exit 1 }}
Say 'step join'
$binDir = if ($env:SPANDECK_BIN_DIR) {{ $env:SPANDECK_BIN_DIR }} else {{ Join-Path $env:LOCALAPPDATA 'Programs\spandeck' }}
$bin = Join-Path $binDir 'spandeck.exe'
if (-not (Test-Path $bin)) {{
    $found = Get-Command spandeck.exe -ErrorAction SilentlyContinue
    if ($found) {{ $bin = $found.Source }} else {{ Write-Output 'The spandeck command was not found on this machine.'; Fail 'not_running' }}
}}
$ok = $false
try {{ & $bin status *> $null; $ok = ($LASTEXITCODE -eq 0) }} catch {{ Write-Output "$_" }}
if (-not $ok) {{ Write-Output 'spandeck is not answering on this machine. It runs only while you are logged on there.'; Fail 'not_running' }}
{join_lines}
exit 0
"#
            );
        }
        let join_lines = unix_join_lines(&ask, join);
        format!(
            r#"main() {{
set -u
say() {{ printf '@spandeck %s\n' "$*"; }}
fail() {{ say "fail $1"; exit 1; }}
say step join
bin=
for at in "${{SPANDECK_BIN_DIR:-$HOME/.local/bin}}/spandeck" "$(command -v spandeck 2>/dev/null)" "$HOME/.cargo/bin/spandeck"; do
  if [ -n "$at" ] && [ -x "$at" ]; then bin=$at; break; fi
done
[ -n "$bin" ] || {{ echo "The spandeck command was not found on this machine."; fail not_running; }}
"$bin" status >/dev/null 2>&1 || {{ echo "spandeck is not answering on this machine."; fail not_running; }}
say "version $("$bin" --version 2>/dev/null | head -n 1)"
{join_lines}
}}
main </dev/null
"#
        )
    }

    /// Whom a new station may ask to be let in: the stations of this fleet
    /// are peers, and a request is approved at any of them. This station
    /// is first (it is the one that is surely up), then up to three others
    /// that answer now; each with the address this station reaches it at
    /// when that is one the new station would look at by itself.
    fn join_candidates(&self) -> Vec<(String, Option<String>)> {
        let plain = |name: &str| {
            !name.is_empty()
                && !name.starts_with('-')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        };
        let mut all = vec![(self.station.clone(), self.own_address())];
        let mut mates: Vec<Peer> = self
            .fleet_members()
            .into_iter()
            .filter(|p| p.state == "online" && plain(&p.station))
            .collect();
        mates.sort_by(|a, b| a.station.cmp(&b.station));
        all.extend(mates.into_iter().take(3).map(|p| {
            let tailnet = p
                .addr
                .parse::<std::net::SocketAddr>()
                .is_ok_and(|at| is_tailscale_address(at.ip()));
            (p.station, tailnet.then_some(p.addr))
        }));
        all
    }

    /// Where this station answers on the tailnet, for a machine that does
    /// not find it by name: its own Tailscale address (IPv4 first) and the
    /// port its web server listens on. None when Tailscale has not said.
    fn own_address(&self) -> Option<String> {
        let ips = self.fleet.own_addrs.borrow().as_ref()?.ips.clone();
        let ip = ips
            .into_iter()
            .filter(|ip| is_tailscale_address(*ip))
            .min_by_key(|ip| ip.is_ipv6())?;
        Some(std::net::SocketAddr::new(ip, self.cfg.web_port).to_string())
    }
}

/// The dialog's words when a station could not ask to join. `stations` are
/// the fleet's, as a list; `why` is what spandeck on that machine said, if
/// it said anything.
fn join_failure(name: &str, stations: &str, why: Option<&str>) -> String {
    match why {
        // A release from before the join looked for the station by itself.
        Some(w) if w.contains("no station called") => format!(
            "It has not asked to join your fleet: spandeck on {name} looked several times for the fleet's stations ({stations}) among the machines under your Tailscale login and saw none of them. Check that Tailscale is connected on {name} and signed in to the same account as your other machines."
        ),
        Some(w) if w.contains("was not found") => format!(
            "It has not asked to join your fleet, because it saw none of the fleet's stations ({stations}). What spandeck on {name} said: {w}"
        ),
        Some(w) => {
            format!("It could not ask to join your fleet. What spandeck on {name} said: {w}")
        }
        None => "It could not ask to join your fleet: the last lines below say why.".to_string(),
    }
}

/// The lines that make a station ask to join this fleet and say the code it
/// was given. They use only what every release has: `fleet --join NAME` and
/// `fleet`.
///
/// `ask` is the stations of this fleet that may be asked, by name and (where
/// known) by address, the one that sends the script first. They are peers:
/// each is asked in turn until one takes the request, so a new station joins
/// when any one of them can be seen from it.
///
/// A station that has only just started may not see any of them yet (the
/// owner's report of 17 Nov 2026). A release from this one on goes on
/// looking by itself for twenty seconds and then says what it tried. An
/// older one gives up at once ("no station called X was found"), so after
/// that answer, and only after it, the names are gone through again, up to
/// three times, with `spandeck fleet` between, which every release has and
/// which makes the station look at the tailnet and wait for the look. Last,
/// each is asked by its address: a release that takes an address asks there
/// directly; an older one answers as it did for a name, at once, and what
/// is shown is what it said of the first name.
fn unix_join_lines(ask: &[(String, Option<String>)], join: JoinBy) -> String {
    let names: Vec<&str> = ask.iter().map(|(n, _)| n.as_str()).collect();
    let (listed, words) = (names.join(", "), names.join(" "));
    let addresses: Vec<&str> = ask
        .iter()
        .filter_map(|(_, a)| a.as_deref())
        .filter(|a| !a.contains(|c: char| c == '\'' || c.is_whitespace()))
        .collect();
    let by_address = if addresses.is_empty() {
        String::new()
    } else {
        format!(
            r#"if [ "$joined" = no ]; then
  for at in {}; do
    echo "Asking at $at."
    if out=$("$bin" --json fleet --join "$at" 2>&1); then joined=yes; break; fi
  done
fi
"#,
            addresses.join(" ")
        )
    };
    match join {
        JoinBy::Code => format!(
            r#"echo "Asking to join the fleet (looking for its stations: {listed})."
tries=0 joined=no why=
while [ "$joined" = no ]; do
  tries=$((tries + 1)) again=no why=
  for name in {words}; do
    if out=$("$bin" --json fleet --join "$name" 2>&1); then joined=yes; break; fi
    [ -n "$why" ] || why=$(printf '%s\n' "$out" | sed '/^[[:space:]]*$/d' | tail -n 1)
    case "$out" in *"no station called"*) again=yes ;; esac
  done
  [ "$joined" = no ] && [ "$again" = yes ] && [ "$tries" -lt 3 ] || break
  echo "None of them is seen from here yet. Looking again (try $((tries + 1)) of 3)."
  sleep $((tries * 2))
  "$bin" fleet >/dev/null 2>&1 || true
done
{by_address}if [ "$joined" = no ]; then
  printf '%s\n' "$why"
  say "why $why"
  fail join
fi
code=$(printf '%s\n' "$out" | sed -n 's/.*"code": *"\([A-Z0-9]*\)".*/\1/p' | head -n 1)
[ -n "$code" ] || fail join
say "code $code""#
        ),
    }
}

/// The same for Windows PowerShell 5.1. What a program writes to its error
/// output arrives there as error records: `"$_"` is the text of each.
fn windows_join_lines(ask: &[(String, Option<String>)], join: JoinBy) -> String {
    let quoted = |items: Vec<&str>| {
        items
            .iter()
            .map(|i| format!("'{i}'"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let names: Vec<&str> = ask.iter().map(|(n, _)| n.as_str()).collect();
    let listed = names.join(", ");
    let names = quoted(names);
    let addresses: Vec<&str> = ask
        .iter()
        .filter_map(|(_, a)| a.as_deref())
        .filter(|a| !a.contains(|c: char| c == '\'' || c.is_whitespace()))
        .collect();
    let by_address = if addresses.is_empty() {
        String::new()
    } else {
        format!(
            r#"if (-not $joined) {{
    foreach ($at in @({})) {{
        Write-Output "Asking at $at."
        $out = (& $bin --json fleet --join $at 2>&1 | ForEach-Object {{ "$_" }} | Out-String)
        if ($LASTEXITCODE -eq 0) {{ $joined = $true; break }}
    }}
}}
"#,
            quoted(addresses)
        )
    };
    match join {
        JoinBy::Code => format!(
            r#"Write-Output 'Asking to join the fleet (looking for its stations: {listed}).'
$tries = 0; $joined = $false; $why = ''
while (-not $joined) {{
    $tries++; $again = $false; $why = ''
    foreach ($name in @({names})) {{
        $out = (& $bin --json fleet --join $name 2>&1 | ForEach-Object {{ "$_" }} | Out-String)
        if ($LASTEXITCODE -eq 0) {{ $joined = $true; break }}
        if (-not $why) {{ $why = "$(($out -split '\r?\n') | Where-Object {{ $_.Trim() }} | Select-Object -Last 1)" }}
        if ($out -match 'no station called') {{ $again = $true }}
    }}
    if ($joined -or (-not $again) -or ($tries -ge 3)) {{ break }}
    Write-Output "None of them is seen from here yet. Looking again (try $($tries + 1) of 3)."
    Start-Sleep -Seconds ($tries * 2)
    try {{ & $bin fleet *> $null }} catch {{ }}
}}
{by_address}if (-not $joined) {{
    Write-Output $why
    Say "why $why"
    Fail 'join'
}}
if ($out -match '"code":\s*"([A-Z0-9]+)"') {{ Say "code $($Matches[1])" }} else {{ Fail 'join' }}"#
        ),
    }
}

/// What runs on a station that is being uninstalled.
///
/// The station there does the part that touches the system, with the code
/// that removal with "also stop spandeck there" already uses
/// (`station.leave`): it closes its panes, takes its login service out and
/// ends. That code only touches the login service when the account it runs
/// under has one. The script touches it itself only when spandeck was not
/// running, and then only if the service's own file is in this account.
/// It never stops a process by name.
///
/// Then the programs are deleted. The state and config folders are kept
/// unless `erase` is set, and are deleted only if they look like spandeck's.
/// Nothing else on the machine is touched.
pub(crate) fn uninstall_script(erase: bool, os: &str) -> String {
    if os == "Windows" {
        let erase_lines = if erase {
            r#"foreach ($dir in @($state, $config)) {
    $ours = (Test-Path (Join-Path $dir 'spandeck.db')) -or (Test-Path (Join-Path $dir 'fleet.key')) -or (Test-Path (Join-Path $dir 'config.toml')) -or ((Split-Path $dir -Leaf) -eq 'spandeck')
    if ((Test-Path $dir) -and $ours) { Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue; Write-Output "Deleted $dir" }
}"#
        } else {
            r#"Write-Output "Kept: $state and $config""#
        };
        return format!(
            r#"$ErrorActionPreference = 'Continue'
function Say($m) {{ Write-Output "@spandeck $m" }}
function Fail($kind) {{ Say "fail $kind"; exit 1 }}
Say 'step stop'
$binDir = if ($env:SPANDECK_BIN_DIR) {{ $env:SPANDECK_BIN_DIR }} else {{ Join-Path $env:LOCALAPPDATA 'Programs\spandeck' }}
$bin = Join-Path $binDir 'spandeck.exe'
if (-not (Test-Path $bin)) {{
    $found = Get-Command spandeck.exe -ErrorAction SilentlyContinue
    if ($found) {{ $bin = $found.Source; $binDir = Split-Path $bin -Parent }}
}}
$state = if ($env:SPANDECK_STATE_DIR) {{ $env:SPANDECK_STATE_DIR }} else {{ Join-Path $env:LOCALAPPDATA 'spandeck' }}
$config = if ($env:SPANDECK_CONFIG_DIR) {{ $env:SPANDECK_CONFIG_DIR }} else {{ Join-Path $env:APPDATA 'spandeck' }}
function Running {{ try {{ & $bin status *> $null; return ($LASTEXITCODE -eq 0) }} catch {{ return $false }} }}
if ((Test-Path $bin) -and (Running)) {{
    try {{ & $bin rpc station.leave '{{\"stop\": true}}' *> $null }} catch {{ }}
    $n = 0
    while (Running) {{
        $n++
        if ($n -gt 30) {{ Write-Output 'spandeck did not stop.'; Fail 'stop' }}
        Start-Sleep -Seconds 1
    }}
    Write-Output 'spandeck was stopped and its panes closed.'
}}
$task = Join-Path $config 'start-at-login.ps1'
if (Test-Path $task) {{
    & schtasks.exe /Delete /TN spandeckd /F *> $null
    Remove-Item -Force $task -ErrorAction SilentlyContinue
}}
Say 'step remove'
$left = @()
foreach ($name in @('spandeck.exe', 'sd.exe', 'spandeckd.exe')) {{
    $file = Join-Path $binDir $name
    $n = 0
    while ((Test-Path $file) -and ($n -lt 20)) {{
        Remove-Item -Force $file -ErrorAction SilentlyContinue
        if (Test-Path $file) {{ $n++; Start-Sleep -Seconds 1 }}
    }}
    if (Test-Path $file) {{ $left += $file }}
}}
if ($left.Count -gt 0) {{ Write-Output "Could not delete (still in use?): $($left -join ', ')"; Fail 'remove' }}
if ((Test-Path $binDir) -and -not (Get-ChildItem -Force $binDir)) {{ Remove-Item -Force $binDir -ErrorAction SilentlyContinue }}
Remove-Item -Recurse -Force (Join-Path $state 'bin') -ErrorAction SilentlyContinue
Write-Output "Deleted the spandeck programs in $binDir"
{erase_lines}
Say 'done'
exit 0
"#
        );
    }
    let erase_lines = if erase {
        r#"for dir in "$state" "$config"; do
  ours "$dir" || continue
  rm -rf "$dir" && echo "Deleted $dir"
done"#
    } else {
        r#"echo "Kept: $state and $config""#
    };
    format!(
        r#"main() {{
set -u
say() {{ printf '@spandeck %s\n' "$*"; }}
fail() {{ say "fail $1"; exit 1; }}
# A folder is deleted only if it is spandeck's: never the home folder or /.
ours() {{
  [ -n "$1" ] && [ "$1" != / ] && [ "$1" != "$HOME" ] && [ "$1" != "$HOME/" ] && [ -d "$1" ] || return 1
  [ -f "$1/spandeck.db" ] || [ -f "$1/fleet.key" ] || [ -f "$1/config.toml" ] || [ "$(basename "$1")" = spandeck ]
}}
say step stop
bin_dir=${{SPANDECK_BIN_DIR:-$HOME/.local/bin}}
state=${{SPANDECK_STATE_DIR:-$HOME/.local/state/spandeck}}
config=${{SPANDECK_CONFIG_DIR:-$HOME/.config/spandeck}}
# Where the installer puts the command, then the PATH, then a build from source.
bin=
for at in "$bin_dir/spandeck" "$(command -v spandeck 2>/dev/null)" "$HOME/.cargo/bin/spandeck"; do
  if [ -n "$at" ] && [ -x "$at" ]; then bin=$at; break; fi
done
if [ -n "$bin" ] && "$bin" status >/dev/null 2>&1; then
  "$bin" rpc station.leave '{{"stop": true}}' >/dev/null 2>&1 || true
  n=0
  while "$bin" status >/dev/null 2>&1; do
    n=$((n + 1))
    [ "$n" -le 30 ] || {{ echo "spandeck did not stop."; fail stop; }}
    sleep 1
  done
  echo "spandeck was stopped and its panes closed."
fi
# It was not running, so it did not take its own login service out. Only a
# service whose file is in this account is this account's to remove.
case "$(uname -s)" in
  Darwin)
    unit=$HOME/Library/LaunchAgents/dev.spandeck.spandeckd.plist
    if [ -f "$unit" ]; then
      launchctl bootout "gui/$(id -u)/dev.spandeck.spandeckd" 2>/dev/null || true
      rm -f "$unit"
    fi ;;
  *)
    unit=$HOME/.config/systemd/user/spandeckd.service
    if [ -f "$unit" ]; then
      systemctl --user disable --now spandeckd 2>/dev/null || true
      rm -f "$unit"
      systemctl --user daemon-reload 2>/dev/null || true
    fi ;;
esac
say step remove
rm -f "$bin_dir/spandeck" "$bin_dir/sd" || fail remove
if [ -n "$bin" ] && [ -e "$bin" ]; then
  rm -f "$bin" "$(dirname "$bin")/sd" || {{ echo "Could not delete $bin"; fail remove; }}
fi
if ours "$state"; then rm -rf "$state/bin"; fi
[ ! -e "$bin_dir/spandeck" ] || fail remove
echo "Deleted the spandeck programs in $bin_dir"
{erase_lines}
say done
}}
main </dev/null
"#
    )
}

/// The line a Windows machine is given over SSH: PowerShell reads all of
/// standard input, then runs it as one script.
const WINDOWS_READS_STDIN: &str = "\"[Console]::In.ReadToEnd() | Invoke-Expression\"";
/// The same for an install, which may be sent the release after its script:
/// PowerShell reads one line and runs it. That line
/// (`supply::WINDOWS_READS_SCRIPT`) reads the script up to the line that
/// ends it and runs it, and the session's input stays open behind it. Like
/// the line above it has no `$` and no quote of its own.
const WINDOWS_READS_A_LINE: &str = "\"[Console]::In.ReadLine() | Invoke-Expression\"";

/// What a Windows machine being installed on is sent: the line that reads
/// the script, the script, and the line that ends it.
fn windows_framed(script: &str) -> String {
    format!(
        "{}\n{script}\n{}\n",
        supply::WINDOWS_READS_SCRIPT,
        supply::WINDOWS_SCRIPT_ENDS
    )
}

/// What runs on a Windows machine: the same steps in Windows PowerShell
/// 5.1, with `install.ps1` inside it. The start step registers the task
/// that starts spandeck at log on and runs it; a Windows station runs in
/// the owner's logged-on session, never in the SSH session, which ends.
fn windows_script(
    ask: &[(String, Option<String>)],
    releases: Option<&str>,
    join: JoinBy,
    send: Force,
) -> String {
    {
        let releases = match releases {
            Some(url) if !url.contains('\'') => {
                format!("$env:SPANDECK_RELEASES = '{}'\n", url.trim_end_matches('/'))
            }
            _ => String::new(),
        };
        let join_lines = windows_join_lines(ask, join);
        let receive = supply::windows_receive_lines();
        let asked = if send == Force::No { "" } else { "asked" };
        format!(
            r#"$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
function Say($m) {{ Write-Output "@spandeck $m" }}
function Fail($kind) {{ Say "fail $kind"; exit 1 }}
# Smart App Control, or another policy, refusing a program it does not know.
function Blocked($e) {{ return ("$e" -match 'Application Control|blocked by|0x800711C7|4551') }}
Say 'step install'
$arch = if ($env:PROCESSOR_ARCHITEW6432) {{ $env:PROCESSOR_ARCHITEW6432 }} else {{ $env:PROCESSOR_ARCHITECTURE }}
if ($arch -ne 'AMD64') {{ Write-Output "This machine's processor is $arch. There is no spandeck release for it."; Fail 'unsupported' }}
{releases}if (-not $env:SPANDECK_RELEASES) {{ $env:SPANDECK_RELEASES = 'https://github.com/michaelbaker-dev/spandeck/releases' }}
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
# Why the release has to be sent by the station at the other end, if it has to be.
$target = 'x86_64-pc-windows-msvc'
$supplyWhy = '{asked}'
if (-not $supplyWhy) {{
    try {{
        $probe = [Net.WebRequest]::Create("$($env:SPANDECK_RELEASES)/latest"); $probe.Method = 'HEAD'; $probe.Timeout = 20000
        $probe.GetResponse().Close()
    }} catch {{ Write-Output "This machine could not reach $($env:SPANDECK_RELEASES)"; $supplyWhy = 'network' }}
}}
# The install script sets up start at log on itself; here that is its own step.
$env:SPANDECK_NO_SERVICE = '1'
$install = @'
{INSTALL_PS1}
'@
if (-not $supplyWhy) {{
    try {{ & ([scriptblock]::Create($install)) }}
    catch {{
        Write-Output "$_"
        if ("$_" -match '\(404\)') {{ Fail 'unsupported' }}
        if ("$_" -match 'The release could not be downloaded') {{ $supplyWhy = 'download' }} else {{ Fail 'install' }}
    }}
}}
if ($supplyWhy) {{
{receive}
    if (-not $supplied) {{ Fail 'supply' }}
    $env:SPANDECK_ARCHIVE = $supplied; $env:SPANDECK_ARCHIVE_SHA256 = $suppliedSum; $env:SPANDECK_VERSION = $suppliedVersion
    try {{ & ([scriptblock]::Create($install)) }}
    catch {{
        Write-Output "$_"
        Remove-Item -Recurse -Force $suppliedDir -ErrorAction SilentlyContinue
        Fail 'install'
    }}
    Remove-Item -Recurse -Force $suppliedDir -ErrorAction SilentlyContinue
}}
# From here programs are run and their exit codes read: what one writes to
# its error output must not count as this script failing (it does in 5.1).
$ErrorActionPreference = 'Continue'
$binDir = if ($env:SPANDECK_BIN_DIR) {{ $env:SPANDECK_BIN_DIR }} else {{ Join-Path $env:LOCALAPPDATA 'Programs\spandeck' }}
$bin = Join-Path $binDir 'spandeck.exe'
Say 'step start'
try {{ & $bin service --install | Out-Null }}
catch {{ Write-Output "$_"; if (Blocked $_) {{ Fail 'blocked' }}; Fail 'start' }}
if ($LASTEXITCODE -ne 0) {{ Fail 'start' }}
$task = Join-Path (Join-Path $env:APPDATA 'spandeck') 'start-at-login.ps1'
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $task
if ($LASTEXITCODE -ne 0) {{ Write-Output 'The task that starts spandeck at log on could not be registered.'; Fail 'start' }}
$n = 0
while ($true) {{
    $ok = $false
    try {{ & $bin status *> $null; $ok = ($LASTEXITCODE -eq 0) }} catch {{ if (Blocked $_) {{ Write-Output "$_"; Fail 'blocked' }} }}
    if ($ok) {{ break }}
    $n++
    if ($n -gt 30) {{ Write-Output 'spandeck was installed but did not start. It starts when you are logged on at this machine.'; Fail 'start' }}
    Start-Sleep -Seconds 1
}}
Write-Output 'spandeck is running.'
Say "version $(& $bin --version 2>$null | Select-Object -First 1)"
Say 'step join'
{join_lines}
exit 0
"#
        )
    }
}

impl Daemon {
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
        let listener =
            spandeck_terminal::sys::local::Listener::bind(&sock).map_err(|_| "no_ssh")?;
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
        // What goes into the session after the script: only an install is
        // ever sent anything more (a release, when the machine asks for
        // one), so only its input is kept open.
        let windows = job.os == "Windows";
        let (feed, mut fed) = tokio::sync::mpsc::unbounded_channel::<Feed>();
        let (local, mut locals) = tokio::sync::mpsc::unbounded_channel::<Local>();
        let mut feed = (job.kind == "add").then_some(feed);
        let mut unix_script_to_send = None::<String>;
        if let Some(mut stdin) = child.stdin.take() {
            let script = match job.kind {
                "join" => self.join_script(join, &job.os),
                "uninstall" => uninstall_script(job.erase, &job.os),
                _ if windows => windows_framed(&self.add_script(join, &job.os, job.send)),
                // An install on a Mac or Linux machine: one line that
                // takes the script and runs it. The script is sent when
                // that line says it is ready for it.
                _ => {
                    let script = self.add_script(join, &job.os, job.send);
                    let boot = supply::unix_boot_line(script.len());
                    unix_script_to_send = Some(script);
                    boot
                }
            };
            let local = local.clone();
            tokio::spawn(async move {
                if stdin.write_all(script.as_bytes()).await.is_ok() {
                    let _ = stdin.flush().await;
                    // Ends when the job has dropped its end.
                    while let Some(next) = fed.recv().await {
                        match next {
                            Feed::Line(line) => {
                                if stdin.write_all(line.as_bytes()).await.is_err() {
                                    break;
                                }
                                let _ = stdin.flush().await;
                            }
                            Feed::Archive(supplied) => {
                                let done = supply::send(&mut stdin, &supplied, windows, |n| {
                                    let _ = local.send(Local::Sent(n));
                                })
                                .await;
                                let _ = local.send(Local::SendEnded(done.is_ok()));
                            }
                        }
                    }
                }
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

        let mut deadline = tokio::time::Instant::now() + ADD_TIMEOUT;
        let (mut code, mut failed, mut said) = (None::<String>, None::<&'static str>, vec![]);
        let (mut missing, mut done) = (false, false);
        // The release this machine asked to be sent, and how far that is.
        let mut sending: Option<Sending> = None;
        let mut not_sent: Option<&'static str> = None;
        let host = supply::ReleaseHost::new(self.cfg.update.url.as_deref(), Duration::from_secs(5))
            .map(|h| h.words())
            .unwrap_or_else(|_| "the release host".into());
        loop {
            // While a release is fetched and sent the clock that runs is
            // its own; the ten minutes are for everything else.
            let wake = match &sending {
                Some(s) if !s.over => match s.started {
                    Some(at) => (s.moved + SUPPLY_STALL).min(at + SUPPLY_TIMEOUT),
                    None => s.asked + SUPPLY_PREPARE,
                },
                _ => deadline,
            };
            let (is_err, line) = tokio::select! {
                next = rx.recv() => match next { Some(l) => l, None => break },
                Some(event) = locals.recv() => {
                    let Some(s) = sending.as_mut() else { continue };
                    match event {
                        Local::Got(Ok(supplied)) => {
                            let note = sending_words(&job.name, &self.station, &host, s.why, &supplied);
                            self.record(
                                job.log_kind(),
                                None,
                                None,
                                None,
                                Some(&job.by),
                                json!({"machine": job.name, "address": job.address, "user": job.user,
                                    "outcome": "sending a release over SSH", "release": supplied.tag,
                                    "platform": supplied.target, "sha256": supplied.sha256,
                                    "bytes": supplied.size, "from": match supplied.source {
                                        Source::Published => "the release host, checked against its published SHA-256",
                                        Source::Kept => "a download kept from before, checked then against its published SHA-256",
                                        Source::OwnCopy => "this station's own program",
                                        Source::Fleet => "another station of the fleet, which checked it against its published SHA-256",
                                        Source::FleetOwnCopy => "another station's own program",
                                    }, "via": supplied.via}),
                            );
                            if !self.add_line(id, &note) {
                                return Err("cancelled");
                            }
                            self.add_update(id, |j| j.note = Some(note));
                            if let Some(feed) = &feed {
                                let _ = feed.send(Feed::Line(supply::answer_line(&supplied)));
                            }
                            s.moved = tokio::time::Instant::now();
                            s.supplied = Some(Arc::new(supplied));
                        }
                        Local::Got(Err(none)) => {
                            let (kind, text) = self.no_supply_words(&job.name, &host, s.why, &s.target, &none);
                            not_sent = Some(kind);
                            self.add_update(id, |j| { j.told = Some(text); j.note = None; });
                            // Told there is nothing, the script ends.
                            if let Some(feed) = &feed {
                                let _ = feed.send(Feed::Line("none\n".into()));
                            }
                            s.over = true;
                        }
                        Local::Sent(bytes) => {
                            let now = tokio::time::Instant::now();
                            s.moved = now;
                            let Some(supplied) = &s.supplied else { continue };
                            // Said every tenth of the way, and not more
                            // often than every other second.
                            let tenth = bytes * 10 / supplied.size.max(1);
                            if tenth > s.said && bytes < supplied.size && now >= s.said_at + Duration::from_secs(2) {
                                (s.said, s.said_at) = (tenth, now);
                                let note = format!(
                                    "{} {} of {} sent.",
                                    sending_words(&job.name, &self.station, &host, s.why, supplied),
                                    supply::size_words(bytes),
                                    supply::size_words(supplied.size)
                                );
                                self.add_update(id, |j| j.note = Some(note));
                            }
                        }
                        Local::SendEnded(whole) => {
                            s.over = true;
                            // The time it took is not taken from the ten minutes.
                            deadline += s.asked.elapsed();
                            if whole {
                                let size = s.supplied.as_ref().map(|x| x.size).unwrap_or_default();
                                let note = format!("Sent {} to {}, which is checking and installing it.", supply::size_words(size), job.name);
                                if !self.add_line(id, &note) {
                                    return Err("cancelled");
                                }
                                self.add_update(id, |j| j.note = Some(note));
                            } else {
                                // The session took no more: the machine
                                // sees its input end and says what arrived.
                                not_sent = Some("transfer");
                                feed = None;
                            }
                        }
                    }
                    continue;
                }
                // Dropping the child on the way out kills it.
                _ = job.cancel.notified() => return Err("cancelled"),
                _ = tokio::time::sleep_until(wake) => {
                    return Err(match &sending {
                        Some(s) if !s.over && s.started.is_none() => "not_fetched",
                        Some(s) if !s.over => "stalled",
                        _ => "timeout",
                    });
                }
            };
            if let (false, Some(mark)) = (is_err, line.strip_prefix("@spandeck ")) {
                match mark.split_once(' ').unwrap_or((mark, "")) {
                    ("step", step) => {
                        let stage = match step {
                            "install" => "installing",
                            "start" => "starting",
                            "join" => "joining",
                            "stop" => "stopping",
                            "remove" => "removing",
                            _ => continue,
                        };
                        if step == "start" {
                            // The install is over: nothing more is sent.
                            feed = None;
                        }
                        self.add_update(id, |j| {
                            j.stage = stage;
                            if step != "install" {
                                j.note = None;
                            }
                        });
                    }
                    // The line that takes the script is ready for it.
                    ("script", _) => {
                        if let (Some(script), Some(feed)) = (unix_script_to_send.take(), &feed) {
                            let _ = feed.send(Feed::Line(script));
                        }
                    }
                    // The machine cannot download the release and asks for
                    // it: `supply PLATFORM WHY`. Once, and only an install.
                    // The platform must be one that has a release and of
                    // the system Tailscale says the machine runs; nothing
                    // else it says is used.
                    ("supply", asks) if feed.is_some() && sending.is_none() => {
                        let (target, why) = asks.split_once(' ').unwrap_or((asks, ""));
                        let known = supply::TARGETS.contains(&target)
                            && match job.os.as_str() {
                                "Windows" => target.ends_with("windows-msvc"),
                                "macOS" => target.ends_with("apple-darwin"),
                                _ => target.ends_with("linux-gnu"),
                            };
                        let why = match why {
                            "nocurl" => "nocurl",
                            "download" => "download",
                            "asked" => "asked",
                            _ => "network",
                        };
                        let now = tokio::time::Instant::now();
                        sending = Some(Sending {
                            target: target.to_string(),
                            why,
                            asked: now,
                            started: None,
                            moved: now,
                            supplied: None,
                            said: 0,
                            said_at: now,
                            over: false,
                        });
                        let (me, local, force) = (self.clone(), local.clone(), job.send);
                        let target = if known {
                            target.to_string()
                        } else {
                            String::new()
                        };
                        let getting = format!(
                            "{}. Getting spandeck for {} on {} to send it over SSH…",
                            why_words(&job.name, &host, why),
                            supply::platform_words(&target),
                            self.station
                        );
                        if !self.add_line(id, &getting) {
                            return Err("cancelled");
                        }
                        self.add_update(id, |j| j.note = Some(getting));
                        tokio::spawn(async move {
                            let _ = local.send(Local::Got(me.release_for(&target, force).await));
                        });
                    }
                    // It has made room and waits for the bytes.
                    ("ready", _) => {
                        if let (Some(s), Some(feed)) = (sending.as_mut(), &feed) {
                            if let (Some(supplied), None) = (&s.supplied, s.started) {
                                let now = tokio::time::Instant::now();
                                (s.started, s.moved) = (Some(now), now);
                                let _ = feed.send(Feed::Archive(supplied.clone()));
                            }
                        }
                    }
                    ("code", c) => code = Some(c.to_string()),
                    // "spandeck 0.4.10": the number is kept.
                    ("version", v) => {
                        let v: String = v
                            .rsplit(' ')
                            .next()
                            .unwrap_or_default()
                            .trim_start_matches('v')
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
                            .take(24)
                            .collect();
                        if v.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                            self.add_update(id, |j| j.version = Some(v));
                        }
                    }
                    // What spandeck there said when it could not ask.
                    ("why", w) => {
                        let w: String = w
                            .trim()
                            .trim_start_matches("spandeck: ")
                            .chars()
                            .take(400)
                            .collect();
                        if !w.is_empty() {
                            self.add_update(id, |j| j.why = Some(w));
                        }
                    }
                    ("done", _) => done = true,
                    ("fail", kind) => {
                        // Nothing more is sent to a script that has ended.
                        feed = None;
                        failed = Some(match kind {
                            "blocked" => "blocked",
                            "unsupported" => "unsupported",
                            "network" => "network",
                            "start" => "start",
                            "join" => "join",
                            "not_running" => "not_running",
                            "stop" => "stop",
                            "remove" => "remove",
                            "transfer" => "transfer",
                            // Why there was nothing to send is known here.
                            "supply" => not_sent.unwrap_or("supply"),
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
            if !self.add_line(id, &line) {
                return Err("cancelled");
            }
        }
        // The sending broke off and the script did not live to say so.
        if failed.is_none() && not_sent == Some("transfer") {
            failed = not_sent;
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
        if job.kind == "uninstall" {
            // No code to wait for: it says when it has finished.
            return if done {
                Ok(String::new())
            } else {
                Err("remove")
            };
        }
        code.filter(|c| (4..=12).contains(&c.len()) && c.chars().all(|c| c.is_ascii_alphanumeric()))
            .ok_or("join")
    }
}

/// What is put into a session after its script.
enum Feed {
    Line(String),
    Archive(Arc<Supplied>),
}

/// What happens at this station while a release is fetched and sent.
enum Local {
    /// The release to send, or why there is none.
    Got(std::result::Result<Supplied, NoSupply>),
    /// So many bytes of it have been handed to the session.
    Sent(u64),
    /// The sending ended: whole, or broken off.
    SendEnded(bool),
}

/// A release a machine asked to be sent, and how far that is.
struct Sending {
    target: String,
    why: &'static str,
    asked: tokio::time::Instant,
    /// When the bytes began to go.
    started: Option<tokio::time::Instant>,
    /// When something last moved.
    moved: tokio::time::Instant,
    supplied: Option<Arc<Supplied>>,
    /// The last tenth of the way that was said, and when.
    said: u64,
    said_at: tokio::time::Instant,
    over: bool,
}

/// Why a machine has to be sent the release, as the dialog begins it.
fn why_words(name: &str, host: &str, why: &str) -> String {
    match why {
        "nocurl" => format!("{name} has no curl to download with"),
        "download" => format!("The download from {host} did not finish on {name}"),
        "asked" => format!("{name} is sent the release over SSH, as asked"),
        _ => format!("{name} cannot reach {host}"),
    }
}

/// The dialog's line while a release is sent: progress, not a failure.
/// "monitoring cannot reach GitHub: sending spandeck from spark2 over SSH
/// (18 MB)…"
fn sending_words(name: &str, station: &str, host: &str, why: &str, supplied: &Supplied) -> String {
    let begin = why_words(name, host, why);
    let size = supply::size_words(supplied.size);
    let tag = &supplied.tag;
    match supplied.source {
        Source::Published => {
            format!("{begin}: sending spandeck {tag} from {station} over SSH ({size})…")
        }
        Source::Kept => format!(
            "{begin}, and {station} cannot reach {host} now either: sending the release {station} downloaded and checked earlier, {tag} ({size})…"
        ),
        Source::OwnCopy if why == "network" => format!(
            "{begin}, and neither can {station}: sending this station's own copy, {} ({size})…",
            tag.trim_start_matches('v')
        ),
        Source::OwnCopy => format!(
            "{begin}, and {station} has no release from {host} to send: sending this station's own copy, {} ({size})…",
            tag.trim_start_matches('v')
        ),
        // Got from a station of the fleet that could fetch it.
        Source::Fleet | Source::FleetOwnCopy => {
            let via = supplied.via.as_deref().unwrap_or("another station");
            let neither = if why == "network" {
                format!("and neither can {station}")
            } else {
                format!("and {station} cannot reach {host}")
            };
            format!(
                "{begin}, {neither}: sending the release {via} fetched and checked, {tag}, from {station} over SSH ({size})…"
            )
        }
    }
}

impl Daemon {
    /// One more line of what the job says, shown in the dialog. False when
    /// the job is no longer the one named.
    fn add_line(&self, id: &str, line: &str) -> bool {
        {
            let mut slot = self.adding.job.lock().unwrap();
            let Some(j) = slot.as_mut().filter(|j| j.id == id) else {
                return false;
            };
            if j.lines.len() >= KEPT_LINES {
                j.lines.pop_front();
            }
            j.lines.push_back(line.to_string());
        }
        self.emit(
            "fleet.add.output",
            None,
            None,
            json!({"id": id, "line": line}),
        );
        true
    }

    /// Why nothing was sent, as the kind of failure and the dialog's words.
    /// When neither this station nor the machine can get the release, the
    /// words name both facts and the stations of the fleet that are the
    /// same kind of machine, since any station can add a machine.
    fn no_supply_words(
        &self,
        name: &str,
        host: &str,
        why: &str,
        target: &str,
        none: &NoSupply,
    ) -> (&'static str, String) {
        let station = &self.station;
        let platform = supply::platform_words(target);
        match none {
            NoSupply::NoSuchPlatform => (
                "unsupported",
                format!("spandeck has no release for {name}'s system: it runs on macOS and Linux on Apple silicon, arm64 and x86-64, and on Windows on x86-64."),
            ),
            NoSupply::NotPublished { tag } => (
                "no_build",
                format!("The newest release, {tag}, has no build for {platform}, so there is nothing to install on {name}."),
            ),
            NoSupply::Checksum { tag } => (
                "supply_checksum",
                format!("{station} downloaded the {platform} build of {tag} to send to {name}, and it does not match its published checksum, so nothing was sent and nothing was installed. Try again; if it happens again, do not install that release."),
            ),
            NoSupply::OwnCopy(e) => (
                "own_copy",
                format!("{}, and {station} could not put its own copy of spandeck together to send: {e}", why_words(name, host, why)),
            ),
            NoSupply::NoSource { reached, fleet, .. } => {
                let mut same: Vec<String> = self
                    .fleet_members()
                    .into_iter()
                    .filter(|p| {
                        supply::target_of(
                            p.info["os"].as_str().unwrap_or_default(),
                            p.info["arch"].as_str().unwrap_or_default(),
                        ) == Some(target)
                    })
                    .map(|p| p.station)
                    .collect();
                same.sort();
                (
                    "no_release",
                    no_source_words(name, station, host, why, target, *reached, fleet, &same),
                )
            }
        }
    }
}

/// The dialog's words when neither the machine nor this station can get
/// the release, no station of the fleet could supply it (`fleet` says what
/// asking them came to), and this station is another kind of machine: the
/// facts, what to do, and the stations of the fleet that are the same kind.
#[allow(clippy::too_many_arguments)]
fn no_source_words(
    name: &str,
    station: &str,
    host: &str,
    why: &str,
    target: &str,
    reached: bool,
    fleet: &str,
    same: &[String],
) -> String {
    let fleet = if fleet.is_empty() {
        String::new()
    } else {
        format!("; {fleet}")
    };
    let platform = supply::platform_words(target);
    let same = if same.is_empty() {
        format!("No station of your fleet is known to be the same kind of machine as {name} ({platform}).")
    } else {
        format!(
            "The same kind of machine as {name} ({platform}) in your fleet: {}.",
            same.join(", ")
        )
    };
    let neither = if reached {
        format!(
            "and {host} has no {platform} build of {station}'s version ({}) or a newer one for {station} to fetch",
            crate::daemon::VERSION
        )
    } else if why == "network" {
        format!("and neither can {station} to fetch the {platform} build")
    } else {
        format!("and {station} cannot reach {host} to fetch the {platform} build")
    };
    format!(
        "{}, {neither}{fleet}. Add it when a station of your fleet can reach {host} (any of them then supplies it), or from a station that is the same kind of machine, which sends its own copy. {same}",
        why_words(name, host, why)
    )
}

/// Answer ssh's askpass helper. The password goes out once, for a prompt
/// that asks for a password, to a helper that knows the nonce. A second
/// prompt (the first password was wrong) gets nothing, so ssh gives up
/// instead of trying again.
async fn askpass_serve(
    mut listener: spandeck_terminal::sys::local::Listener,
    nonce: String,
    mut password: Option<Secret>,
    asked: Arc<AtomicBool>,
) {
    loop {
        let Ok(stream) = listener.accept().await else {
            return;
        };
        let (read, mut write) = tokio::io::split(stream);
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

    /// The owner's request (15 Nov 2026): uninstalling with "erase
    /// everything" deletes spandeck's programs, history and settings, and
    /// "the owner's project folders are never touched". The script is run
    /// here as it is run there, in a made-up home with stand-ins for the
    /// service manager first on its PATH, so nothing of this machine's is
    /// reached. A state folder that is not spandeck's is left alone.
    #[test]
    #[cfg(unix)]
    fn the_uninstall_script_deletes_only_what_is_spandecks() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        // The script looks for the command on its PATH, which here is the
        // stand-ins and the system's own folders. A machine that has
        // spandeck in one of those is not one to run this on.
        if ["/usr/bin/spandeck", "/bin/spandeck"]
            .iter()
            .any(|p| std::path::Path::new(p).exists())
        {
            return;
        }
        let run = |erase: bool, state_is_home: bool| {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            let at = |p: &str| home.join(p);
            for d in [
                ".local/bin",
                ".local/state/spandeck/bin",
                ".config/spandeck",
                "project",
                "fake",
            ] {
                std::fs::create_dir_all(at(d)).unwrap();
            }
            for f in [
                ".local/bin/sd",
                ".local/bin/other-program",
                ".local/state/spandeck/spandeck.db",
                ".local/state/spandeck/bin/running",
                ".config/spandeck/config.toml",
                "project/keep.txt",
                "notes.txt",
            ] {
                std::fs::write(at(f), "x").unwrap();
            }
            for tool in ["systemctl", "launchctl"] {
                let path = at("fake").join(tool);
                std::fs::write(
                    &path,
                    format!("#!/bin/sh\necho {tool} >> {}\n", at("fake/used").display()),
                )
                .unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-s")
                .env_clear()
                .env("HOME", &home)
                .env("PATH", format!("{}:/usr/bin:/bin", at("fake").display()))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped());
            if state_is_home {
                cmd.env("SPANDECK_STATE_DIR", &home);
            }
            let mut child = cmd.spawn().unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(uninstall_script(erase, "Linux").as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            let said = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(said.contains("@spandeck done"), "{said}");
            // No login service is in this home, so none was touched.
            assert!(!at("fake/used").exists(), "{said}");
            let left = |p: &str| at(p).exists();
            assert!(!left(".local/bin/sd"), "{said}");
            assert!(
                left(".local/bin/other-program") && left("project/keep.txt") && left("notes.txt")
            );
            (dir, home)
        };
        let (_keep, home) = run(false, false);
        assert!(home.join(".local/state/spandeck/spandeck.db").exists());
        assert!(!home.join(".local/state/spandeck/bin").exists());
        assert!(home.join(".config/spandeck/config.toml").exists());
        let (_keep, home) = run(true, false);
        assert!(!home.join(".local/state/spandeck").exists());
        assert!(!home.join(".config/spandeck").exists());
        // A state folder that is the home folder itself is never deleted.
        let (_keep, home) = run(true, true);
        assert!(home.join("project/keep.txt").exists() && home.join(".local/state").exists());
    }

    /// The owner's report of 17 Nov 2026: spandeck was installed and
    /// started on a Mac, and its request to join ended at once with
    /// "spandeck: no station called spark2 was found", which the dialog
    /// showed as the reason with nothing about what had worked. And, the
    /// same day: "What does spark2 have to do with adding this machine,
    /// aren't they all peers."
    ///
    /// The lines that ask are run here as they are run there, against a
    /// stand-in for the `spandeck` command (and a `sleep` that does not
    /// wait). A release that gives up at once, as 0.4.10 does, is asked
    /// again with `spandeck fleet` between the tries, and joins on the
    /// third. When the station that sent the script is not seen and
    /// another of the fleet is, that one is asked. A release that looked
    /// by itself and says so is not gone through again by name; it is asked
    /// by address. When nothing works the script says why, and the
    /// dialog's words name the fleet's stations, not one of them.
    /// The owner's Mac "monitoring" (17 Nov 2026): 0.4.11 was installed beside
    /// a station already running 0.4.10, which went on answering as 0.4.10.
    /// The script brings a running station to the program it has installed.
    #[test]
    #[cfg(unix)]
    fn a_station_already_running_an_older_program_is_brought_to_the_one_installed() {
        use std::os::unix::fs::PermissionsExt;
        let script = unix_script(
            &[("alpha".to_string(), None)],
            Some("http://127.0.0.1:9"),
            JoinBy::Code,
            Default::default(),
        );
        let from = script
            .find("have=$(\"$bin\" --version")
            .expect("the lines are in the script");
        let to = from
            + script[from..]
                .find("echo \"spandeck is running.\"")
                .unwrap();
        let lines = &script[from..to];
        let run = |running: &str| {
            let dir = tempfile::tempdir().unwrap();
            let at = |p: &str| dir.path().join(p);
            std::fs::write(at("runs"), running).unwrap();
            std::fs::write(
                at("spandeck"),
                format!(
                    "#!/bin/sh\necho \"$1\" >> {d}/calls\ncase \"$1\" in\n  --version) echo 'spandeck 0.4.11' ;;\n  status) echo \"station x  spandeck $(cat {d}/runs)  0 pane(s)\" ;;\n  upgrade) echo 0.4.11 > {d}/runs ;;\nesac\n",
                    d = dir.path().display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(at("spandeck"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("bin={}\n{lines}", at("spandeck").display()))
                .output()
                .unwrap();
            (
                String::from_utf8_lossy(&out.stdout).to_string(),
                std::fs::read_to_string(at("calls")).unwrap_or_default(),
                std::fs::read_to_string(at("runs"))
                    .unwrap()
                    .trim()
                    .to_string(),
            )
        };
        let (said, calls, runs) = run("0.4.10");
        assert!(
            said.contains("spandeck 0.4.10 was already running here: bringing it to 0.4.11."),
            "{said}"
        );
        assert!(calls.lines().any(|c| c == "upgrade"), "{calls}");
        assert_eq!(runs, "0.4.11");
        // One that already runs it is left alone.
        let (said, calls, _) = run("0.4.11");
        assert_eq!(said, "");
        assert!(!calls.lines().any(|c| c == "upgrade"), "{calls}");
    }

    #[test]
    #[cfg(unix)]
    fn a_new_station_asks_each_station_of_the_fleet_and_asks_again_on_an_old_release() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        // `standin` is the body of the stand-in for `spandeck`: `$n` is how
        // many times it has been asked to join, and `$4` whom it is to ask.
        let run = |standin: &str| {
            let dir = tempfile::tempdir().unwrap();
            let at = |p: &str| dir.path().join(p);
            let script = |name: &str, text: &str| {
                std::fs::write(at(name), format!("#!/bin/sh\n{text}\n")).unwrap();
                std::fs::set_permissions(at(name), std::fs::Permissions::from_mode(0o755)).unwrap();
            };
            script("sleep", "exit 0");
            script(
                "spandeck",
                &format!(
                    "echo \"$*\" >> {calls}\n[ \"$1\" = --json ] || exit 0\nn=$(grep -c -- --join {calls})\n{standin}",
                    calls = at("calls").display()
                ),
            );
            let mut child = std::process::Command::new("sh")
                .arg("-s")
                .env_clear()
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let ask = [
                ("spark2".to_string(), Some("100.64.0.1:7391".to_string())),
                ("m4".to_string(), None),
            ];
            let text = format!(
                "set -u\nsay() {{ printf '@spandeck %s\\n' \"$*\"; }}\nfail() {{ say \"fail $1\"; exit 1; }}\nbin={}\n{}\n",
                at("spandeck").display(),
                unix_join_lines(&ask, JoinBy::Code)
            );
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            let calls = std::fs::read_to_string(at("calls")).unwrap_or_default();
            (
                String::from_utf8_lossy(&out.stdout).to_string(),
                calls.lines().map(String::from).collect::<Vec<_>>(),
            )
        };
        const OLD: &str = r#"echo "spandeck: no station called $4 was found" >&2; exit 1"#;
        const CODE: &str = r#"echo '{ "station": "spark2", "code": "K7QM2X" }'"#;
        let join = |whom: &str| format!("--json fleet --join {whom}");

        // 0.4.10: nothing is seen at the first two looks, then spark2 is.
        let (said, calls) = run(&format!("if [ \"$n\" -lt 5 ]; then {OLD}; fi\n{CODE}"));
        assert!(said.contains("@spandeck code K7QM2X"), "{said}");
        assert!(
            said.contains("looking for its stations: spark2, m4"),
            "{said}"
        );
        assert_eq!(
            calls,
            [
                join("spark2"),
                join("m4"),
                "fleet".into(),
                join("spark2"),
                join("m4"),
                "fleet".into(),
                join("spark2")
            ],
            "{said}"
        );

        // spark2 is not seen from the new machine and m4 is: m4 is asked.
        let (said, calls) = run(&format!("case \"$4\" in m4) {CODE} ;; *) {OLD} ;; esac"));
        assert!(said.contains("@spandeck code K7QM2X"), "{said}");
        assert_eq!(calls, [join("spark2"), join("m4")], "{said}");

        // 0.4.10 that never sees any of them: three times through the
        // names, then the address (which it does not take), and what it
        // said of the first name is the reason.
        let (said, calls) = run(OLD);
        assert_eq!(calls.len(), 9, "{calls:?}");
        assert_eq!(calls[8], join("100.64.0.1:7391"));
        assert!(
            said.contains("@spandeck why spandeck: no station called spark2 was found")
                && said.ends_with("@spandeck fail join\n"),
            "{said}"
        );

        // A release that looks by itself has already waited: the names are
        // gone through once, then the address, which it takes.
        let (said, calls) = run(&format!(
            "case \"$4\" in 100.*) {CODE} ;; *) echo \"spandeck: $4 was not found among the 9 machines under your Tailscale login after 20 s of looking\" >&2; exit 1 ;; esac"
        ));
        assert!(said.contains("@spandeck code K7QM2X"), "{said}");
        assert_eq!(
            calls,
            [join("spark2"), join("m4"), join("100.64.0.1:7391")],
            "{said}"
        );

        // The dialog's words for the owner's case: the fleet and its
        // stations, in plain words, never only the raw line.
        let text = join_failure(
            "monitoring",
            "spark2, m4",
            Some("no station called spark2 was found"),
        );
        assert!(
            text.starts_with("It has not asked to join your fleet: spandeck on monitoring looked several times for the fleet's stations (spark2, m4)"),
            "{text}"
        );
        // The Windows lines ask the same way.
        let ask = [
            ("spark2".to_string(), Some("100.64.0.1:7391".to_string())),
            ("m4".to_string(), None),
        ];
        let windows = windows_join_lines(&ask, JoinBy::Code);
        for piece in [
            "foreach ($name in @('spark2', 'm4'))",
            "fleet --join $name",
            "-match 'no station called'",
            "& $bin fleet *> $null",
            "foreach ($at in @('100.64.0.1:7391'))",
            "Say \"why $why\"",
        ] {
            assert!(windows.contains(piece), "{piece}: {windows}");
        }
    }

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

    /// The owner's decision of 17 Nov 2026 ("Yes I want it"): a machine
    /// that cannot download spandeck is sent it by the station adding it,
    /// over the SSH session that is already open, and the install goes on.
    ///
    /// The script a Mac or Linux machine is sent is run here as it is run
    /// there, by `sh -s`, in a made-up home, with a `curl` that fails first
    /// on its PATH and stand-ins for the service managers. This test is the
    /// station at the other end: it answers what the script asks, as
    /// add.rs does. The archive holds a stand-in for the `spandeck` command.
    ///
    /// Sent whole, it is checked against the SHA-256 it was sent with,
    /// installed by the README's script, started, and asks to join. With a
    /// byte changed on the way, cut short, or sent with another SHA-256,
    /// nothing is installed and nothing of it is left behind. Told there is
    /// nothing to send, the script ends. A download that breaks off after
    /// the release page answered asks the same way.
    #[test]
    #[cfg(unix)]
    fn a_machine_that_cannot_download_takes_the_release_from_its_ssh_session() {
        use std::io::{BufRead, Write};
        use std::os::unix::fs::PermissionsExt;
        let target = format!(
            "{}-{}",
            std::env::consts::ARCH,
            if cfg!(target_os = "macos") {
                "apple-darwin"
            } else {
                "unknown-linux-gnu"
            }
        );
        // The release: a stand-in `spandeck` that answers what the script asks.
        let made = tempfile::tempdir().unwrap();
        let name = format!("spandeck-v9.9.9-{target}");
        std::fs::create_dir_all(made.path().join(&name)).unwrap();
        for b in ["spandeck", "sd"] {
            let file = made.path().join(&name).join(b);
            std::fs::write(
                &file,
                "#!/bin/sh\ncase \"$1\" in --version) echo 'spandeck 9.9.9' ;; --json) echo '{ \"code\": \"K7QM2X\" }' ;; esac\nexit 0\n",
            )
            .unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let packed = std::process::Command::new("tar")
            .arg("-czf")
            .arg(made.path().join("release.tar.gz"))
            .arg("-C")
            .arg(made.path())
            .arg(&name)
            .status()
            .unwrap();
        assert!(packed.success());
        let archive = std::fs::read(made.path().join("release.tar.gz")).unwrap();
        let sum = supply::sha256_hex(&archive);

        // What this end does when the script says `@spandeck ready`.
        #[derive(Clone, Copy, PartialEq)]
        enum Sends {
            Whole,
            OneByteChanged,
            Half,
            AnotherSum,
            Nothing,
        }
        // `curl`: the stand-in's body. Returns what the script said, whether
        // it ended well, whether the command was installed, and what is
        // left in the machine's temporary folder.
        // A Mac's `sh` is bash; most Linux machines' is not. With `as_bash`
        // the script, and the install script inside it, are run by bash
        // under the name `sh`.
        let as_bash = std::cell::Cell::new(false);
        let run = |sends: Sends, curl: &str, send: Force| {
            let dir = tempfile::tempdir().unwrap();
            let at = |p: &str| dir.path().join(p);
            for d in ["home", "tmp", "standin", "bin"] {
                std::fs::create_dir_all(at(d)).unwrap();
            }
            let standin = |name: &str, body: &str| {
                let file = at("standin").join(name);
                std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
            };
            standin("curl", curl);
            standin("sleep", "exit 0");
            for tool in ["systemctl", "launchctl", "loginctl"] {
                standin(tool, "exit 1");
            }
            let ask = [("alpha".to_string(), None)];
            let script = unix_script(&ask, Some("http://127.0.0.1:9"), JoinBy::Code, send);
            if as_bash.get() {
                std::os::unix::fs::symlink("/bin/bash", at("standin/sh")).unwrap();
            }
            // The machine's own `sh`: on a Mac that is bash 3.2, which is
            // where this hung when the script itself came on standard
            // input (17 Nov 2026).
            let mut child = std::process::Command::new(if as_bash.get() {
                at("standin/sh")
            } else {
                "/bin/sh".into()
            })
            .arg("-s")
            .env_clear()
            .env("HOME", at("home"))
            .env("TMPDIR", at("tmp"))
            .env("SPANDECK_BIN_DIR", at("bin"))
            .env("PATH", format!("{}:/usr/bin:/bin", at("standin").display()))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::fs::File::create(at("stderr")).unwrap())
            .spawn()
            .unwrap();
            // It cannot hang: a shell that has not ended in a minute is
            // ended, and the test fails with what it had said.
            let (pid, over) = (child.id(), std::sync::Arc::new(AtomicBool::new(false)));
            let (ended, late) = (over.clone(), std::sync::Arc::new(AtomicBool::new(false)));
            let too_late = late.clone();
            let watch = std::thread::spawn(move || {
                let began = std::time::Instant::now();
                while !ended.load(Ordering::SeqCst) {
                    if began.elapsed() > Duration::from_secs(60) {
                        too_late.store(true, Ordering::SeqCst);
                        let _ = std::process::Command::new("kill")
                            .args(["-9", &pid.to_string()])
                            .status();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
            let mut stdin = child.stdin.take();
            // As the station does: the one line first, and the script when
            // that line says it is ready to take it.
            stdin
                .as_mut()
                .unwrap()
                .write_all(supply::unix_boot_line(script.len()).as_bytes())
                .unwrap();
            let mut said = String::new();
            for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
                let line = line.unwrap();
                if line == "@spandeck script" {
                    stdin
                        .as_mut()
                        .unwrap()
                        .write_all(script.as_bytes())
                        .unwrap();
                    continue;
                }
                said.push_str(&line);
                said.push('\n');
                if line.starts_with("@spandeck supply ") {
                    let to = stdin.as_mut().unwrap();
                    match sends {
                        Sends::Nothing => to.write_all(b"none\n").unwrap(),
                        Sends::AnotherSum => to
                            .write_all(
                                format!("release v9.9.9 {} {}\n", archive.len(), "0".repeat(64))
                                    .as_bytes(),
                            )
                            .unwrap(),
                        _ => to
                            .write_all(
                                format!("release v9.9.9 {} {sum}\n", archive.len()).as_bytes(),
                            )
                            .unwrap(),
                    }
                    to.flush().unwrap();
                } else if line == "@spandeck ready" {
                    let mut bytes = archive.clone();
                    match sends {
                        Sends::OneByteChanged => bytes[archive.len() / 2] ^= 1,
                        Sends::Half => bytes.truncate(archive.len() / 2),
                        _ => {}
                    }
                    stdin.as_mut().unwrap().write_all(&bytes).unwrap();
                    if sends == Sends::Half {
                        // The session ends under the transfer.
                        stdin = None;
                    }
                }
            }
            let ok = child.wait().unwrap().success();
            over.store(true, Ordering::SeqCst);
            watch.join().unwrap();
            assert!(
                !late.load(Ordering::SeqCst),
                "the shell had not ended after a minute and was ended. It had said:\n{said}{}",
                std::fs::read_to_string(at("stderr")).unwrap_or_default()
            );
            let left: Vec<String> = std::fs::read_dir(at("tmp"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect();
            let installed = at("bin/spandeck").exists() || at("bin/sd").exists();
            let said = format!(
                "{said}{}",
                std::fs::read_to_string(at("stderr")).unwrap_or_default()
            );
            (said, ok, installed, left)
        };
        const NO_ROUTE: &str = "exit 7";
        let asks = format!("@spandeck supply {target} network\n@spandeck ready\n");

        // The release page does not answer the machine: it is sent the
        // release, checks it, installs it, starts it and asks to join.
        let (said, ok, installed, left) = run(Sends::Whole, NO_ROUTE, Force::No);
        assert!(ok && installed, "{said}");
        for piece in [
            "This machine could not reach http://127.0.0.1:9\n",
            asks.as_str(),
            &format!(
                "Received {} bytes, and their SHA-256 matches.\n",
                archive.len()
            ),
            &format!("Installing {name} from "),
            "Installed spandeck v9.9.9 to ",
            "(checksum verified)\n",
            "@spandeck step start\n",
            "spandeck is running.\n@spandeck version spandeck 9.9.9\n@spandeck step join\n",
            "@spandeck code K7QM2X\n",
        ] {
            assert!(said.contains(piece), "{piece:?} is not in: {said}");
        }
        assert!(left.is_empty(), "left behind: {left:?}");

        // The same where `sh` is bash, and a transfer cut short there.
        if std::path::Path::new("/bin/bash").exists() {
            as_bash.set(true);
            let (said, ok, installed, left) = run(Sends::Whole, NO_ROUTE, Force::No);
            assert!(ok && installed && left.is_empty(), "{said}");
            assert!(said.contains("@spandeck code K7QM2X\n"), "{said}");
            let (said, ok, installed, left) = run(Sends::Half, NO_ROUTE, Force::No);
            assert!(!ok && !installed && left.is_empty(), "{said}");
            assert!(said.contains("@spandeck fail transfer\n"), "{said}");
            as_bash.set(false);
        }

        // A byte changed on the way, the transfer cut short, and bytes that
        // do not have the SHA-256 they were sent with: refused, nothing
        // installed, nothing left behind.
        for (sends, says) in [
            (
                Sends::OneByteChanged,
                "What arrived does not have the SHA-256 it was sent with, so nothing was installed.".to_string(),
            ),
            (
                Sends::Half,
                format!(
                    "Only {} of {} bytes arrived, so nothing was installed.",
                    archive.len() / 2,
                    archive.len()
                ),
            ),
            (
                Sends::AnotherSum,
                "What arrived does not have the SHA-256 it was sent with, so nothing was installed.".to_string(),
            ),
        ] {
            let (said, ok, installed, left) = run(sends, NO_ROUTE, Force::No);
            assert!(!ok && !installed, "{said}");
            assert!(
                said.contains(&format!("{says}\n@spandeck fail transfer\n")),
                "{said}"
            );
            assert!(!said.contains("@spandeck step start"), "{said}");
            assert!(left.is_empty(), "left behind: {left:?}");
        }

        // This station has nothing to send: the script ends there.
        let (said, ok, installed, left) = run(Sends::Nothing, NO_ROUTE, Force::No);
        assert!(!ok && !installed && left.is_empty(), "{said}");
        assert!(
            said.ends_with(&format!(
                "@spandeck supply {target} network\n@spandeck fail supply\n"
            )),
            "{said}"
        );

        // The release page answers and the download then breaks off (here:
        // the release's name cannot be had): asked for the same way.
        let (said, ok, installed, _) = run(
            Sends::Whole,
            "case \"$*\" in *-fsSI*) exit 0 ;; esac\nexit 7",
            Force::No,
        );
        assert!(ok && installed, "{said}");
        assert!(
            said.contains(&format!("@spandeck supply {target} download\n")),
            "{said}"
        );

        // Asked for by hand: nothing is downloaded, although it could be.
        let (said, ok, installed, _) = run(
            Sends::Whole,
            "echo \"curl was run: $*\"; exit 0",
            Force::Send,
        );
        assert!(ok && installed, "{said}");
        assert!(
            said.contains(&format!("@spandeck supply {target} asked\n"))
                && !said.contains("curl was run"),
            "{said}"
        );
    }

    /// The same decision: the dialog's words while a release is sent are
    /// progress, in the owner's own example, and when neither end can get
    /// the release the failure names both facts and the stations that are
    /// the same kind of machine.
    #[test]
    fn the_words_while_a_release_is_sent_and_when_there_is_none_to_send() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("release");
        std::fs::write(&file, vec![7u8; 17_600_000]).unwrap();
        let of =
            |source| Supplied::of_file("v0.4.11", "aarch64-apple-darwin", &file, source).unwrap();
        assert_eq!(
            sending_words("monitoring", "spark2", "GitHub", "network", &of(Source::Published)),
            "monitoring cannot reach GitHub: sending spandeck v0.4.11 from spark2 over SSH (18 MB)…"
        );
        assert_eq!(
            sending_words("monitoring", "spark2", "GitHub", "network", &of(Source::OwnCopy)),
            "monitoring cannot reach GitHub, and neither can spark2: sending this station's own copy, 0.4.11 (18 MB)…"
        );
        assert_eq!(
            sending_words("monitoring", "spark2", "GitHub", "download", &of(Source::Kept)),
            "The download from GitHub did not finish on monitoring, and spark2 cannot reach GitHub now either: sending the release spark2 downloaded and checked earlier, v0.4.11 (18 MB)…"
        );
        assert_eq!(
            no_source_words(
                "monitoring",
                "spark2",
                "GitHub",
                "network",
                "aarch64-apple-darwin",
                false,
                "none of the stations asked (m4, studio) has it or can reach the release host",
                &["m4".to_string(), "studio".to_string()]
            ),
            "monitoring cannot reach GitHub, and neither can spark2 to fetch the macOS (Apple silicon) build; none of the stations asked (m4, studio) has it or can reach the release host. Add it when a station of your fleet can reach GitHub (any of them then supplies it), or from a station that is the same kind of machine, which sends its own copy. The same kind of machine as monitoring (macOS (Apple silicon)) in your fleet: m4, studio."
        );
        // Another station of the fleet could fetch it: it is sent on.
        let mut via = of(Source::Fleet);
        via.via = Some("m4".into());
        assert_eq!(
            sending_words("monitoring", "spark2", "GitHub", "network", &via),
            "monitoring cannot reach GitHub, and neither can spark2: sending the release m4 fetched and checked, v0.4.11, from spark2 over SSH (18 MB)…"
        );
        // Which stations are the same kind: from what each says of itself.
        assert_eq!(
            supply::target_of("macos", "aarch64"),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(supply::target_of("windows", "aarch64"), None);
    }

    /// The script a Windows machine is sent, read the way it is there: by a
    /// Windows PowerShell that reads one line from standard input and runs
    /// it; that line reads the script and runs it. With the release address
    /// pointing nowhere it must parse as a whole, say where it is, and ask
    /// to be sent the release. Told there is none, it stops: nothing is
    /// installed and nothing on this machine is changed. Sent bytes (as
    /// lines of base64) that are not a release, it takes them, finds their
    /// SHA-256 right, and the install script refuses what is not a .zip;
    /// with a line missing it says how many bytes arrived. This is the only
    /// place the Windows side of sending a release is run, and it runs only
    /// on Windows. (Over SSH the same line passes through cmd.exe first,
    /// which this cannot show.)
    #[test]
    #[cfg_attr(not(windows), ignore = "needs Windows PowerShell")]
    fn the_windows_script_runs_from_standard_input_and_takes_a_release_sent_after_it() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-release.zip");
        let bytes: Vec<u8> = (0..120_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, &bytes).unwrap();
        let supplied =
            Supplied::of_file("v9.9.9", "x86_64-pc-windows-msvc", &file, Source::Published)
                .unwrap();
        let mut lines = Vec::new();
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(supply::send(&mut lines, &supplied, true, |_| {}))
            .unwrap();
        let lines = String::from_utf8(lines).unwrap();
        let ask = [("alpha".to_string(), None)];
        let script = windows_framed(&windows_script(
            &ask,
            Some("http://127.0.0.1:9"),
            JoinBy::Code,
            Force::No,
        ));
        let run = |after: &str| {
            let mut child = std::process::Command::new("powershell.exe")
                .args(["-NoLogo", "-NoProfile", "-NonInteractive"])
                .args(["-ExecutionPolicy", "Bypass", "-Command"])
                .arg(WINDOWS_READS_A_LINE.trim_matches('"'))
                .env("SPANDECK_BIN_DIR", dir.path().join("bin"))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(script.as_bytes()).unwrap();
            stdin.write_all(after.as_bytes()).unwrap();
            drop(stdin);
            let out = child.wait_with_output().unwrap();
            let said = String::from_utf8_lossy(&out.stdout).to_string();
            let err = String::from_utf8_lossy(&out.stderr).to_string();
            assert_eq!(out.status.code(), Some(1), "{said}\n{err}");
            assert!(said.contains("@spandeck step install"), "{said}\n{err}");
            assert!(
                said.contains("@spandeck supply x86_64-pc-windows-msvc network"),
                "{said}\n{err}"
            );
            assert!(!said.contains("@spandeck step start"), "{said}");
            assert!(!dir.path().join("bin").exists(), "{said}");
            said
        };
        let said = run("none\n");
        assert!(said.contains("@spandeck fail supply"), "{said}");
        let said = run(&format!("{}{lines}", supply::answer_line(&supplied)));
        assert!(
            said.contains("@spandeck ready")
                && said.contains("Received 120000 bytes, and their SHA-256 matches.")
                && said.contains("@spandeck fail install"),
            "{said}"
        );
        let short: String = lines.lines().take(2).map(|l| format!("{l}\n")).collect();
        let said = run(&format!("{}{short}", supply::answer_line(&supplied)));
        assert!(
            said.contains("Only 96000 of 120000 bytes arrived, so nothing was installed.")
                && said.contains("@spandeck fail transfer"),
            "{said}"
        );
    }

    /// The script holds install.ps1 between `@'` and `'@`: a line in it that
    /// began with `'@` would end the text early and run the rest as code.
    /// And the line that ends the script for the reader must be nowhere in
    /// it, or the rest would be taken for a release. (The Windows side of
    /// sending a release is checked here as text; it is run only by the
    /// test above, on Windows.)
    #[test]
    fn install_ps1_can_be_carried_inside_the_windows_script() {
        assert!(!INSTALL_PS1.lines().any(|l| l.starts_with("'@")));
        let script = windows_script(
            &[("alpha".to_string(), None)],
            None,
            JoinBy::Code,
            Force::No,
        );
        assert!(script.contains("foreach ($name in @('alpha'))"));
        let framed = windows_framed(&script);
        let lines: Vec<&str> = framed.lines().collect();
        assert_eq!(lines[0], supply::WINDOWS_READS_SCRIPT);
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.trim() == supply::WINDOWS_SCRIPT_ENDS)
                .count(),
            1
        );
        assert_eq!(lines.last(), Some(&supply::WINDOWS_SCRIPT_ENDS));
        // The one line PowerShell is given on its command line: no `$` and
        // no quote inside, so cmd.exe and PowerShell read it the same.
        let inner = WINDOWS_READS_A_LINE.trim_matches('"');
        assert!(!inner.contains(['$', '"', '\'', '%', '^']), "{inner}");
        for piece in [
            "$supplyWhy = ''",
            "$supplyWhy = 'network'",
            "if (\"$_\" -match 'The release could not be downloaded') { $supplyWhy = 'download' } else { Fail 'install' }",
            "[Console]::Out.WriteLine(\"@spandeck supply $target $supplyWhy\"); [Console]::Out.Flush()",
            "[Console]::Out.WriteLine('@spandeck ready'); [Console]::Out.Flush()",
            "$bytes = [Convert]::FromBase64String($line)",
            "if ($sum -ne $suppliedSum) {",
            "if (-not $supplied) { Fail 'supply' }",
            "$env:SPANDECK_ARCHIVE = $supplied; $env:SPANDECK_ARCHIVE_SHA256 = $suppliedSum; $env:SPANDECK_VERSION = $suppliedVersion",
        ] {
            assert!(script.contains(piece), "{piece}");
        }
        // install.ps1 says so when it is the download that failed, and
        // installs from a file it is given.
        for piece in [
            "throw \"The release could not be downloaded: $_\"",
            "throw 'The release could not be downloaded: the download does not match its published checksum. Nothing was installed.'",
            "$zip = $archive",
        ] {
            assert!(INSTALL_PS1.contains(piece), "{piece}");
        }
        let asked = windows_script(
            &[("alpha".to_string(), None)],
            None,
            JoinBy::Code,
            Force::Send,
        );
        assert!(asked.contains("$supplyWhy = 'asked'"));
    }
}
