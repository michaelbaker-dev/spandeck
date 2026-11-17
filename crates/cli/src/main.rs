//! `spandeck`: the daemon and its command line.
//!
//! Every subcommand except `daemon` is a thin client of the local socket, so
//! the CLI covers the API and nothing else.

mod mcp;

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use spandeck_core::client::Client;
use spandeck_core::{Config, Daemon, Paths, StartOpts};

#[derive(Parser)]
#[command(
    name = "spandeck",
    version,
    about = "Run AI coding agents in panes across your machines, from one Control Deck"
)]
struct Cli {
    /// Print raw JSON instead of a summary.
    #[arg(long, global = true)]
    json: bool,
    /// Do not wait for spandeckd when it is not answering (an upgrade or
    /// a restart is a moment; a command otherwise waits up to 5 seconds).
    #[arg(long, global = true)]
    no_wait: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon in the foreground.
    Daemon {
        /// Adopt terminals left by the previous program image (used by upgrades).
        #[arg(long)]
        handoff: Option<PathBuf>,
        /// Do not serve the Control Deck.
        #[arg(long)]
        no_web: bool,
    },
    /// What this station is doing.
    Status,
    /// Offer the Codex sandbox fix in a pane, before Codex starts.
    #[command(hide = true)]
    FixCodexSandbox,
    /// Check this station's setup.
    Doctor {
        /// Fix what needs an administrator: asks for your password with sudo.
        #[arg(long)]
        fix: bool,
    },
    /// Print the Control Deck address (with the localhost token).
    Cc {
        /// Also show a QR code to open the Control Deck on a phone.
        #[arg(long)]
        qr: bool,
    },
    /// Work with panes.
    Pane {
        #[command(subcommand)]
        cmd: PaneCmd,
    },
    /// Work with projects.
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Send a message to a pane, `name@station`, or `you`.
    Send {
        to: String,
        body: String,
        #[arg(long, default_value = "note")]
        kind: String,
    },
    /// Read your messages (a pane reads its own; outside a pane, the user's).
    Inbox {
        #[arg(long)]
        all: bool,
    },
    /// Reply to a message.
    Reply { id: String, body: String },
    /// Mark a message acted on.
    Ack { id: String },
    /// Hand a task to a helper with a role.
    Delegate {
        role: String,
        task: String,
        #[arg(long, default_value = "")]
        acceptance: String,
    },
    /// Work with tasks.
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// Search earlier session history.
    Recall {
        query: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Add a note to the shared project memory.
    Note {
        text: String,
        #[arg(long, default_value = "note")]
        kind: String,
    },
    /// Search the shared project memory.
    Context { query: Option<String> },
    /// Wait until a pane is done, needs you, or fails.
    Wait {
        pane: String,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Sign off a project's current commit.
    Signoff {
        project: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Start a review loop on an author's pane.
    Review {
        pane: String,
        #[arg(long)]
        reviewer: Option<String>,
    },
    /// Show what changed in a pane's workspace.
    Diff { pane: String },
    /// Standing orders that apply to a pane, or the enforcement table.
    Orders { pane: Option<String> },
    /// Stop every running agent turn on this station (or the whole fleet).
    StopAll {
        #[arg(long)]
        fleet: bool,
    },
    /// The fleet: every station under your Tailscale login.
    Fleet {
        /// Ask to be let into the fleet this station is in: any station of
        /// that fleet, by its name or by the address it answers at. Approve
        /// the code at any station of that fleet. The stations of the fleet
        /// this one is in come with it: afterwards there is one fleet.
        #[arg(long, value_name = "STATION")]
        join: Option<String>,
        /// Remove a station from the fleet: its panes are closed, and the
        /// fleet gets a new key so that station can no longer act on the
        /// others. Browsers signed in from other devices sign in again.
        #[arg(long, value_name = "STATION", conflicts_with = "join")]
        remove: Option<String>,
        /// With --remove: also stop spandeck there and stop it starting at
        /// login. The programs and its data stay on that machine's disk.
        #[arg(long, requires = "remove")]
        stop: bool,
    },
    /// Approve a station or browser that asked to be let in, by the code it
    /// shows. Works at any station of the fleet, whichever one was asked.
    Approve { code: String },
    /// Refuse a station or browser that asked to be let in.
    Deny { code: String },
    /// Requests waiting for you at any station of the fleet, and the
    /// browsers signed in to the fleet: the same list on every station.
    Trust {
        /// Stop trusting a signed-in browser, by its id.
        #[arg(long, value_name = "DEVICE")]
        revoke: Option<String>,
    },
    /// Agent hook integrations.
    Integration {
        #[command(subcommand)]
        cmd: IntegrationCmd,
    },
    /// Start spandeckd at login.
    Service {
        /// Write the unit and print the command that enables it.
        #[arg(long)]
        install: bool,
        /// Stop the station that is running, on purpose: its panes end,
        /// and nothing starts it again until the next log on.
        #[arg(long, conflicts_with = "install")]
        stop: bool,
    },
    /// Replace the running daemon with a new binary, keeping every terminal.
    Upgrade { binary: PathBuf },
    /// Restart the daemon in place, keeping every terminal.
    Restart {
        /// Another station in the fleet, by name.
        #[arg(long)]
        station: Option<String>,
    },
    /// Update to the latest release (or a named one), keeping every terminal.
    Update {
        /// A release such as v0.2.0. Default: the latest.
        version: Option<String>,
        /// Another station in the fleet, by name.
        #[arg(long)]
        station: Option<String>,
    },
    /// Wake a sleeping station on this network (wake-on-LAN).
    Wake { station: String },
    /// Start a station whose daemon is not running, over SSH (needs remote_start = true).
    Start { station: String },
    /// Call any API method: spandeck rpc pane.list '{}'
    Rpc {
        method: String,
        params: Option<String>,
    },
    /// Called by agent hooks. Reads the hook payload on stdin.
    #[command(hide = true)]
    Hook { agent: String },
    /// The stdio MCP server agents use.
    #[command(hide = true)]
    Mcp,
    /// Used by upgrades to check a new binary before switching to it.
    #[command(hide = true)]
    HandoffCheck,
}

#[derive(Subcommand)]
enum PaneCmd {
    /// Start a pane: a shell, or an agent (claude, codex).
    New {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Continue the last conversation the agent saved in the folder; a
        /// new one starts when there is none.
        #[arg(long)]
        resume_last: bool,
        /// If the agent's program asks whether to trust the folder, the
        /// answer is yes, and that is remembered for this folder and this
        /// program. It lets the program read, edit and run files there and
        /// apply the folder's own settings: do not use it for a folder
        /// copied from somewhere you do not trust.
        #[arg(long)]
        trust_folder: bool,
        /// Do not answer the agent's review of hooks (Codex's "Hooks need
        /// review") for me, here or in later panes on this station. Without
        /// it spandeck chooses "Trust all and continue" when every hook
        /// the agent can be asking about is spandeck's own.
        #[arg(long, conflicts_with = "trust_hooks")]
        no_trust_hooks: bool,
        /// Turn that back on for this station.
        #[arg(long)]
        trust_hooks: bool,
        /// Program and arguments instead of an agent.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Answer "yes" to the folder-trust question a pane's program is
    /// showing. With --remember: also for later panes of that program in
    /// that folder.
    TrustFolder {
        pane: String,
        #[arg(long)]
        remember: bool,
    },
    /// Choose "Trust all and continue" at the review of hooks a pane's
    /// program is showing. Refused, with the reason, when the hooks may
    /// not all be spandeck's own.
    TrustHooks {
        pane: String,
    },
    List,
    /// Print a pane's screen.
    Text {
        pane: String,
        #[arg(long, default_value_t = 0)]
        history: u64,
    },
    /// Type text into a pane.
    Type {
        pane: String,
        text: String,
    },
    /// Pause: no new work is delivered; the current turn finishes.
    Pause {
        pane: String,
    },
    Unpause {
        pane: String,
    },
    /// Stop: interrupt the current turn; the process stays.
    Stop {
        pane: String,
    },
    /// Kill: end the process.
    Kill {
        pane: String,
    },
    /// Forget a pane (a running one is killed first). With --all: every
    /// pane on this station, or on the one named with --station.
    Close {
        #[arg(required_unless_present = "all")]
        pane: Option<String>,
        /// End and remove every pane on the station. Projects, layouts,
        /// history and the archive are not touched.
        #[arg(long, conflicts_with = "pane")]
        all: bool,
        /// With --all: another station in the fleet, by name.
        #[arg(long, requires = "all")]
        station: Option<String>,
    },
    /// Let a held agent take mail again: one that was stopped, or one
    /// that was started again after spandeck restarted. With --all: every
    /// pane on this station, or on the one named with --station. A paused
    /// pane stays paused.
    Release {
        #[arg(required_unless_present = "all")]
        pane: Option<String>,
        #[arg(long, conflicts_with = "pane")]
        all: bool,
        /// Another station in the fleet, by name.
        #[arg(long)]
        station: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Create a project in a folder (default: here) and open it.
    New {
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Panes to start, as name:agent[:role], for example dev:claude:author
        #[arg(long = "pane")]
        panes: Vec<String>,
        #[arg(long)]
        no_open: bool,
    },
    List,
    Open {
        name: String,
    },
    Close {
        name: String,
    },
    Remove {
        name: String,
    },
}

#[derive(Subcommand)]
enum TaskCmd {
    List {
        #[arg(long)]
        active: bool,
    },
    Show {
        task: String,
    },
    /// Report the result of your task. This is what completes it.
    Result {
        #[arg(long)]
        summary: String,
        #[arg(long = "finding")]
        findings: Vec<String>,
        #[arg(long)]
        report_file: Option<PathBuf>,
        #[arg(long)]
        failed: bool,
        #[arg(long)]
        task: Option<String>,
    },
    Cancel {
        task: String,
    },
}

#[derive(Subcommand)]
enum IntegrationCmd {
    Status,
    /// Write spandeck's hook entries into the agent's config.
    Install {
        agent: String,
    },
    /// Remove exactly the entries install wrote.
    Uninstall {
        agent: String,
    },
}

/// ssh runs this binary as its askpass helper while a machine is being
/// added from the Control Deck: one argument, the prompt, and the answer is
/// read from standard output. The daemon decides whether the prompt gets the
/// password; this only carries it, and keeps no copy.
fn askpass(socket: &str) -> i32 {
    use std::io::{Read, Write};
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let nonce = std::env::var("SPANDECK_ASKPASS_NONCE").unwrap_or_default();
    let answer = (|| -> std::io::Result<Vec<u8>> {
        let mut s = spandeck_terminal::sys::local::connect(std::path::Path::new(socket))?;
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
        let prompt: String = prompt.chars().filter(|c| !c.is_control()).collect();
        s.write_all(format!("{nonce}\n{prompt}\n").as_bytes())?;
        let mut answer = vec![];
        s.read_to_end(&mut answer)?;
        Ok(answer)
    })();
    match answer {
        Ok(mut answer) if !answer.is_empty() => {
            answer.push(b'\n');
            let sent = std::io::stdout().write_all(&answer).is_ok();
            answer.fill(0);
            i32::from(!sent)
        }
        _ => 1,
    }
}

fn main() {
    if let Ok(socket) = std::env::var("SPANDECK_ASKPASS_SOCK") {
        std::process::exit(askpass(&socket));
    }
    // Two helpers the daemon starts itself, never typed by a person. Their
    // arguments are another program's, so they are not parsed as spandeck's.
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match raw.first().map(String::as_str) {
        // Windows: what a pane runs when its agent's program is missing.
        Some("install-then-launch") => {
            std::process::exit(spandeck_core::agents::install_then_launch_main(&raw[1..]))
        }
        // Windows: hold the machine awake until the daemon (a pid) ends.
        Some("keep-awake") => {
            let pid = raw.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
            let ok = spandeck_terminal::sys::hold_awake(pid).is_ok();
            std::process::exit(i32::from(!ok));
        }
        _ => {}
    }
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("spandeck: {e}");
            1
        }
    };
    std::process::exit(code);
}

/// The connection a command makes its calls on. While spandeckd is away
/// (an upgrade, a restart) a call waits for it, five seconds at most, and
/// says so after one; a change whose answer was lost is not made twice
/// (core/client.rs). `--no-wait`: one try, as before.
fn connect(paths: &Paths, no_wait: bool) -> anyhow::Result<Client> {
    use spandeck_core::client::{CLI_NOTICE, CLI_WAIT};
    if no_wait {
        return Ok(Client::from_env(paths)?);
    }
    Ok(Client::from_env_retrying(paths, CLI_WAIT)?.notice(CLI_NOTICE, "waiting for spandeckd…"))
}

fn show(json_out: bool, v: &Value, summary: impl FnOnce(&Value) -> String) {
    if json_out {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    } else {
        let s = summary(v);
        if !s.is_empty() {
            println!("{s}");
        }
    }
}

fn pane_line(p: &Value) -> String {
    format!(
        "{:<28} {:<14} {:<10} {:<10} {}{}",
        p["id"].as_str().unwrap_or(""),
        p["name"].as_str().unwrap_or(""),
        p["agent"].as_str().unwrap_or(""),
        if p["exited"] == json!(true) {
            "exited"
        } else {
            p["state"].as_str().unwrap_or("")
        },
        p["project"].as_str().unwrap_or("-"),
        p["needs_reason"]
            .as_str()
            .map(|r| format!("  ({r})"))
            .unwrap_or_default(),
    )
}

fn run(cli: Cli) -> anyhow::Result<i32> {
    let paths = Paths::from_env();
    let j = cli.json;
    match cli.cmd {
        Cmd::Daemon { handoff, no_web } => return daemon_main(paths, handoff, no_web),
        Cmd::Hook { agent } => return Ok(hook_main(&paths, &agent)),
        Cmd::FixCodexSandbox => {
            // Run in a Codex pane before Codex starts. Never stops Codex starting.
            return Ok(if fix_with_sudo().is_ok() { 0 } else { 1 });
        }
        Cmd::Mcp => {
            mcp::run(&paths)?;
            return Ok(0);
        }
        Cmd::HandoffCheck => {
            println!(
                "{}",
                json!({"handoff": 1, "version": spandeck_core::VERSION, "protocol": spandeck_core::PROTOCOL, "schema": spandeck_core::db::Db::LATEST_SCHEMA})
            );
            return Ok(0);
        }
        Cmd::Service { stop: true, .. } => return service_stop(&paths),
        Cmd::Service { install, .. } => {
            let exe = std::env::current_exe()?;
            let home = spandeck_core::paths::home_dir();
            let (path, body, enable) = spandeck_core::doctor::service_definition(&exe, &home);
            if install {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, body)?;
                println!(
                    "Wrote {}\nTo start spandeckd at login, run:\n  {enable}",
                    path.display()
                );
            } else {
                println!("# {}\n{body}\n# enable with: {enable}", path.display());
            }
            return Ok(0);
        }
        _ => {}
    }

    let mut c = connect(&paths, cli.no_wait)?;
    c.set_timeout(Some(Duration::from_secs(3700)));
    match cli.cmd {
        Cmd::Status => {
            let info = c.call("station.info", json!({}))?;
            let panes = c.call("pane.list", json!({}))?;
            show(j, &json!({"station": info, "panes": panes}), |v| {
                let i = &v["station"];
                let mut s = format!(
                    "station {}  spandeck {}  {} pane(s), {} working, {} need you\n",
                    i["station"].as_str().unwrap_or(""),
                    i["version"].as_str().unwrap_or(""),
                    i["panes"],
                    i["working"],
                    i["needs_you"]
                );
                for p in v["panes"].as_array().into_iter().flatten() {
                    s.push_str(&pane_line(p));
                    s.push('\n');
                }
                s.trim_end().to_string()
            });
        }
        Cmd::Doctor { fix } => {
            if fix {
                fix_with_sudo()?;
            }
            let v = c.call("station.doctor", json!({"reach": true}))?;
            show(j, &v, |v| {
                v["checks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| {
                        format!(
                            "{} {:<22} {}",
                            if c["ok"] == json!(true) {
                                "ok  "
                            } else if c["required"] == json!(true) {
                                "FAIL"
                            } else {
                                "warn"
                            },
                            c["name"].as_str().unwrap_or(""),
                            c["detail"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            return Ok(if v["ok"] == json!(true) { 0 } else { 1 });
        }
        Cmd::Cc { qr } => {
            let info = c.call("station.info", json!({}))?;
            let token = std::fs::read_to_string(paths.token_file()).unwrap_or_default();
            for addr in info["web"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str())
            {
                if addr.starts_with("127.") {
                    println!("http://{addr}/#token={}", token.trim());
                } else {
                    println!("http://{addr}/   (from your other devices on Tailscale)");
                }
            }
            if qr {
                // The address a phone can reach: the Tailscale one. It carries no
                // token; the phone asks to sign in and you approve it here.
                let phone = info["web"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a.as_str())
                    .find(|a| !a.starts_with("127."));
                match phone {
                    Some(addr) => {
                        let url = format!("http://{addr}/");
                        let code = qrcode::QrCode::new(url.as_bytes())?;
                        println!(
                            "\n{}\nScan with a phone on your Tailscale account: {url}\nIt shows a code; approve it here with: spandeck approve <code>",
                            code.render::<qrcode::render::unicode::Dense1x2>()
                                .quiet_zone(true)
                                .build()
                        );
                    }
                    None => println!(
                        "\nNo Tailscale address to show a QR code for. Is Tailscale running?"
                    ),
                }
            }
        }
        Cmd::Pane { cmd } => match cmd {
            PaneCmd::New {
                agent,
                name,
                project,
                cwd,
                resume_last,
                trust_folder,
                no_trust_hooks,
                trust_hooks,
                command,
            } => {
                let cwd = cwd.or_else(|| std::env::current_dir().ok());
                let (program, args) = match command.split_first() {
                    Some((p, rest)) => (Some(p.clone()), rest.to_vec()),
                    None => (None, vec![]),
                };
                // Not given, nothing is sent: the owner's standing choice
                // for the folder stands as it is.
                let trust = trust_folder.then_some(true);
                // The same for spandeck's own hooks: on unless the owner
                // turned it off, and only a flag changes that.
                let hooks = match (no_trust_hooks, trust_hooks) {
                    (true, _) => Some(false),
                    (_, true) => Some(true),
                    _ => None,
                };
                let v = c.call("pane.spawn", json!({"agent": agent, "name": name, "project": project, "cwd": cwd, "program": program, "args": args, "resume_last": resume_last, "trust_folder": trust, "trust_hooks": hooks}))?;
                // The id stays alone on standard output, for scripts.
                if resume_last && v["resumed"] != json!(true) && v["agent"] != json!("shell") {
                    eprintln!("spandeck: no saved conversation to continue in this folder; a new one was started");
                }
                show(j, &v, |v| v["id"].as_str().unwrap_or("").to_string());
            }
            PaneCmd::TrustFolder { pane, remember } => {
                c.call(
                    "pane.trust_folder",
                    json!({"pane": pane, "remember": remember}),
                )?;
            }
            PaneCmd::TrustHooks { pane } => {
                c.call("pane.trust_hooks", json!({"pane": pane}))?;
            }
            PaneCmd::List => {
                let v = c.call("pane.list", json!({}))?;
                show(j, &v, |v| {
                    v.as_array()
                        .into_iter()
                        .flatten()
                        .map(pane_line)
                        .collect::<Vec<_>>()
                        .join("\n")
                });
            }
            PaneCmd::Text { pane, history } => {
                let v = c.call("pane.text", json!({"pane": pane, "history": history}))?;
                show(j, &v, |v| {
                    v["lines"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|l| l.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim_end()
                        .to_string()
                });
            }
            PaneCmd::Type { pane, text } => {
                c.call("pane.send_text", json!({"pane": pane, "text": text}))?;
            }
            PaneCmd::Pause { pane } => {
                c.call("pane.pause", json!({"pane": pane, "paused": true}))?;
            }
            PaneCmd::Unpause { pane } => {
                c.call("pane.pause", json!({"pane": pane, "paused": false}))?;
            }
            PaneCmd::Stop { pane } => {
                c.call("pane.stop", json!({"pane": pane}))?;
            }
            PaneCmd::Kill { pane } => {
                c.call("pane.kill", json!({"pane": pane}))?;
            }
            PaneCmd::Close {
                pane: Some(pane), ..
            } => {
                c.call("pane.close", json!({"pane": pane}))?;
            }
            PaneCmd::Release {
                pane: Some(pane),
                station,
                ..
            } => {
                let v = on_station(
                    &mut c,
                    station.as_deref(),
                    "pane.release",
                    json!({"pane": pane}),
                )?;
                show(j, &v, |v| {
                    format!(
                        "{}{}",
                        if v["released"] == json!(true) {
                            "released: it takes mail again"
                        } else {
                            "it was not held"
                        },
                        if v["still_paused"] == json!(true) {
                            "; it is still paused (spandeck pane unpause)"
                        } else {
                            ""
                        }
                    )
                });
            }
            PaneCmd::Release { station, .. } => {
                let v = on_station(
                    &mut c,
                    station.as_deref(),
                    "pane.release",
                    json!({"all": true}),
                )?;
                show(j, &v, |v| {
                    format!(
                        "released {} pane(s) on {}{}",
                        v["released"],
                        v["station"].as_str().unwrap_or(""),
                        match v["still_paused"].as_u64().unwrap_or(0) {
                            0 => String::new(),
                            n => format!("; {n} of them still paused"),
                        }
                    )
                });
            }
            PaneCmd::Close { station, .. } => {
                let v = on_station(&mut c, station.as_deref(), "station.close_all", json!({}))?;
                show(j, &v, |v| {
                    format!(
                        "closed {} pane(s) on {} ({} running, {} of them working right then)",
                        v["closed"],
                        v["station"].as_str().unwrap_or(""),
                        v["running"],
                        v["working"]
                    )
                });
            }
        },
        Cmd::Project { cmd } => match cmd {
            ProjectCmd::New {
                name,
                path,
                panes,
                no_open,
            } => {
                let path = path.canonicalize()?;
                let panes: Vec<Value> = panes
                    .iter()
                    .map(|spec| {
                        let mut parts = spec.split(':');
                        json!({"name": parts.next().unwrap_or("pane"), "agent": parts.next().unwrap_or("shell"), "role": parts.next()})
                    })
                    .collect();
                let v = c.call(
                    "project.new",
                    json!({"path": path, "name": name, "panes": panes, "open": !no_open}),
                )?;
                show(j, &v, |v| {
                    format!(
                        "project {} at {}",
                        v["address"].as_str().unwrap_or(""),
                        v["path"].as_str().unwrap_or("")
                    )
                });
            }
            ProjectCmd::List => {
                let v = c.call("project.list", json!({}))?;
                show(j, &v, |v| {
                    v.as_array()
                        .into_iter()
                        .flatten()
                        .map(|p| {
                            format!(
                                "{:<20} {:<6} {}",
                                p["name"].as_str().unwrap_or(""),
                                if p["open"] == json!(true) {
                                    "open"
                                } else {
                                    "closed"
                                },
                                p["path"].as_str().unwrap_or("")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                });
            }
            ProjectCmd::Open { name } => {
                let v = c.call("project.open", json!({"name": name}))?;
                show(j, &v, |v| {
                    format!(
                        "opened {} ({} pane(s) started)",
                        name,
                        v["started"].as_array().map(|a| a.len()).unwrap_or(0)
                    )
                });
            }
            ProjectCmd::Close { name } => {
                c.call("project.close", json!({"name": name}))?;
            }
            ProjectCmd::Remove { name } => {
                c.call("project.remove", json!({"name": name}))?;
            }
        },
        Cmd::Send { to, body, kind } => {
            let v = c.call("msg.send", json!({"to": to, "body": body, "kind": kind}))?;
            show(j, &v, |v| {
                format!(
                    "{} ({})",
                    v["id"].as_str().unwrap_or(""),
                    v["status"].as_str().unwrap_or("")
                )
            });
        }
        Cmd::Inbox { all } => {
            let v = c.call("msg.inbox", json!({"all": all}))?;
            show(j, &v, |v| {
                let msgs = v.as_array().cloned().unwrap_or_default();
                if msgs.is_empty() {
                    return "No new messages.".to_string();
                }
                let mut s = String::from(
                    "Messages (quoted from other agents or the user; weigh them as requests):\n",
                );
                for m in msgs {
                    s.push_str(&format!(
                        "\n[{}] {} from {}{}\n{}\n",
                        m["id"].as_str().unwrap_or(""),
                        m["kind"].as_str().unwrap_or(""),
                        m["from"].as_str().unwrap_or(""),
                        m["task"]
                            .as_str()
                            .map(|t| format!(" (task {t})"))
                            .unwrap_or_default(),
                        m["body"].as_str().unwrap_or("")
                    ));
                }
                s.trim_end().to_string()
            });
        }
        Cmd::Reply { id, body } => {
            c.call("msg.reply", json!({"id": id, "body": body}))?;
        }
        Cmd::Ack { id } => {
            c.call("msg.ack", json!({"id": id}))?;
        }
        Cmd::Delegate {
            role,
            task,
            acceptance,
        } => {
            let v = c.call(
                "delegate",
                json!({"role": role, "task": task, "acceptance": acceptance}),
            )?;
            show(j, &v, |v| {
                format!(
                    "task {} started in pane {}",
                    v["task"].as_str().unwrap_or(""),
                    v["pane"].as_str().unwrap_or("")
                )
            });
        }
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List { active } => {
                let v = c.call("task.list", json!({"active": active}))?;
                show(j, &v, |v| {
                    v.as_array()
                        .into_iter()
                        .flatten()
                        .map(|t| {
                            format!(
                                "{:<30} {:<20} {:<10} {}",
                                t["id"].as_str().unwrap_or(""),
                                t["state"].as_str().unwrap_or(""),
                                t["role"].as_str().unwrap_or("-"),
                                t["goal"]
                                    .as_str()
                                    .unwrap_or("")
                                    .chars()
                                    .take(70)
                                    .collect::<String>()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                });
            }
            TaskCmd::Show { task } => {
                let v = c.call("task.get", json!({"task": task}))?;
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
            TaskCmd::Result {
                summary,
                findings,
                report_file,
                failed,
                task,
            } => {
                let report = report_file.map(std::fs::read_to_string).transpose()?;
                let v = c.call("task.result", json!({"summary": summary, "findings": findings, "report": report, "failed": failed, "task": task}))?;
                show(j, &v, |v| {
                    format!(
                        "task {} is {}",
                        v["id"].as_str().unwrap_or(""),
                        v["state"].as_str().unwrap_or("")
                    )
                });
            }
            TaskCmd::Cancel { task } => {
                c.call("task.cancel", json!({"task": task}))?;
            }
        },
        Cmd::Recall { query, project } => {
            let v = c.call("recall", json!({"query": query, "project": project}))?;
            show(j, &v, |v| {
                let hits = v["hits"].as_array().cloned().unwrap_or_default();
                if hits.is_empty() {
                    return "Nothing found.".to_string();
                }
                let mut s = format!("{}\n", v["note"].as_str().unwrap_or(""));
                for h in hits {
                    s.push_str(&format!(
                        "\n[{} · {} · {}{}] {} {}:\n  {}\n",
                        h["station"].as_str().unwrap_or(""),
                        h["ts"].as_str().unwrap_or("?"),
                        h["commit"]
                            .as_str()
                            .map(|c| &c[..c.len().min(8)])
                            .unwrap_or("no commit"),
                        h["project"]
                            .as_str()
                            .map(|p| format!(" · {p}"))
                            .unwrap_or_default(),
                        h["role"].as_str().unwrap_or(""),
                        h["kind"].as_str().unwrap_or(""),
                        h["excerpt"].as_str().unwrap_or("").replace('\n', " ")
                    ));
                }
                s.trim_end().to_string()
            });
        }
        Cmd::Note { text, kind } => {
            c.call("note.add", json!({"text": text, "kind": kind}))?;
        }
        Cmd::Context { query } => {
            let v = c.call(
                "context.search",
                json!({"query": query.unwrap_or_default()}),
            )?;
            show(j, &v, |v| {
                v.as_array()
                    .into_iter()
                    .flatten()
                    .map(|n| {
                        format!(
                            "[{}{}] {}",
                            n["kind"].as_str().unwrap_or(""),
                            n["author"]
                                .as_str()
                                .map(|a| format!(" by {a}"))
                                .unwrap_or_default(),
                            n["text"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        Cmd::Wait { pane, timeout } => {
            let v = c.call("wait", json!({"pane": pane, "timeout_ms": timeout * 1000}))?;
            show(j, &v, pane_line);
            return Ok(if v["timed_out"] == json!(true) { 2 } else { 0 });
        }
        Cmd::Signoff { project, note } => {
            let v = c.call("signoff.give", json!({"project": project, "note": note}))?;
            show(j, &v, |v| {
                format!("signed off commit {}", v["commit"].as_str().unwrap_or(""))
            });
        }
        Cmd::Review { pane, reviewer } => {
            let v = c.call("recipe.review", json!({"pane": pane, "reviewer": reviewer}))?;
            show(j, &v, |v| {
                format!("review loop {} started", v["id"].as_str().unwrap_or(""))
            });
        }
        Cmd::Diff { pane } => {
            let v = c.call("diff", json!({"pane": pane}))?;
            show(j, &v, |v| {
                format!(
                    "{}\n{}",
                    v["stat"].as_str().unwrap_or(""),
                    v["diff"].as_str().unwrap_or("")
                )
            });
        }
        Cmd::Orders { pane } => {
            let v = match pane {
                Some(p) => c.call("orders.view", json!({"pane": p}))?,
                None => c.call("orders.table", json!({}))?,
            };
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::StopAll { fleet } => {
            let v = c.call(
                "stop.all",
                json!({"scope": if fleet { "fleet" } else { "station" }}),
            )?;
            show(j, &v, |v| {
                v["stops"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| {
                        format!(
                            "{:<16} {}",
                            s["station"].as_str().unwrap_or(""),
                            s["label"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        cmd @ (Cmd::Approve { .. } | Cmd::Deny { .. }) => {
            let (method, code) = match cmd {
                Cmd::Approve { code } => ("trust.approve", code),
                Cmd::Deny { code } => ("trust.deny", code),
                _ => unreachable!(),
            };
            let v = c.call(method, json!({"request": code}))?;
            show(j, &v, |v| {
                format!(
                    "{} {} {}{}{}",
                    if v["approved"] == json!(true) {
                        "approved"
                    } else {
                        "refused"
                    },
                    v["kind"].as_str().unwrap_or(""),
                    v["name"].as_str().unwrap_or(""),
                    came_with(&v["with"]),
                    match v["at"].as_str() {
                        Some(at) => format!("  (it was asked at {at}, which decided)"),
                        None => String::new(),
                    }
                )
            });
        }
        Cmd::Trust { revoke } => {
            if let Some(device) = revoke {
                c.call("device.revoke", json!({"device": device}))?;
            }
            let v = c.call("trust.status", json!({}))?;
            show(j, &v, |v| {
                let mut out = vec![format!("fleet {}", v["fleet"].as_str().unwrap_or(""))];
                for r in v["requests"].as_array().into_iter().flatten() {
                    out.push(format!(
                        "waiting: {} {}{} from {}{}  code {}   (spandeck approve {})",
                        r["kind"].as_str().unwrap_or(""),
                        r["name"].as_str().unwrap_or(""),
                        came_with(&r["with"]),
                        r["from"].as_str().unwrap_or(""),
                        match r["at"].as_str() {
                            Some(at) if r["at"] != v["station"] => format!(", asked at {at}"),
                            _ => String::new(),
                        },
                        r["code"].as_str().unwrap_or(""),
                        r["code"].as_str().unwrap_or("")
                    ));
                }
                for d in v["devices"].as_array().into_iter().flatten() {
                    out.push(format!(
                        "signed in: {}  {}  from {}{}{}",
                        d["id"].as_str().unwrap_or(""),
                        d["name"].as_str().unwrap_or(""),
                        d["from"].as_str().unwrap_or(""),
                        match d["at"].as_str() {
                            Some(at) if !at.is_empty() => format!("  approved at {at}"),
                            _ => String::new(),
                        },
                        if d["unconfirmed"] == json!(true) {
                            "  (listed from before; may no longer be signed in)"
                        } else {
                            ""
                        }
                    ));
                }
                out.join("\n")
            });
        }
        Cmd::Fleet {
            remove: Some(station),
            stop,
            ..
        } => {
            c.set_timeout(Some(Duration::from_secs(60)));
            let v = c.call("fleet.remove", json!({"station": station, "stop": stop}))?;
            show(j, &v, removed_summary);
        }
        Cmd::Fleet {
            join: Some(station),
            ..
        } => {
            // The station looks for the one named by itself, and goes on
            // looking for a while when it does not see it yet.
            let v = c.call("fleet.join", json!({"station": station}))?;
            show(j, &v, |v| v["next"].as_str().unwrap_or("").to_string());
        }
        Cmd::Fleet { join: None, .. } => {
            let v = c.call("fleet.refresh", json!({}))?;
            show(j, &v, |v| {
                v["stations"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| {
                        format!(
                            "{:<18} {:<14} {:<8} working {}  need you {}",
                            s["station"].as_str().unwrap_or(""),
                            s["state"].as_str().unwrap_or(""),
                            s["version"].as_str().unwrap_or(""),
                            s["working"],
                            s["needs_you"]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        Cmd::Integration { cmd } => {
            let v = match cmd {
                IntegrationCmd::Status => c.call("integration.status", json!({}))?,
                IntegrationCmd::Install { agent } => {
                    c.call("integration.install", json!({"agent": agent}))?
                }
                IntegrationCmd::Uninstall { agent } => {
                    c.call("integration.uninstall", json!({"agent": agent}))?
                }
            };
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Restart { station } | Cmd::Update { station, .. }
            if station.as_deref().is_some_and(|s| s.is_empty()) =>
        {
            anyhow::bail!("--station needs a name");
        }
        Cmd::Restart { station } => {
            let v = on_station(&mut c, station.as_deref(), "station.restart", json!({}))?;
            show(j, &v, |_| {
                "restarting; terminals stay open. If it does not come back healthy, the restart is undone.".to_string()
            });
        }
        Cmd::Update { version, station } => {
            let v = on_station(
                &mut c,
                station.as_deref(),
                "station.update",
                json!({"version": version}),
            )?;
            show(j, &v, |v| {
                if v["current"] == json!(true) {
                    format!(
                        "already on the latest release ({})",
                        v["version"].as_str().unwrap_or("")
                    )
                } else if v["fleet"] == json!(true) {
                    // It cannot download: it asks the fleet, and goes on by itself.
                    format!(
                        "{} It updates by itself when it has it; a notice in the Control Deck says how it went.",
                        v["note"].as_str().unwrap_or("")
                    )
                } else {
                    format!(
                        "updating {} -> {}; the download matched its checksum. Terminals stay open, and a version that does not start properly is undone.",
                        v["from"].as_str().unwrap_or(""),
                        v["release"].as_str().unwrap_or("")
                    )
                }
            });
        }
        Cmd::Wake { station } => {
            let v = c.call("fleet.wake", json!({"station": station}))?;
            show(j, &v, |v| {
                format!(
                    "sent {} wake packets. {}",
                    v["sent"],
                    v["note"].as_str().unwrap_or("")
                )
            });
        }
        Cmd::Start { station } => {
            let v = c.call("fleet.start", json!({"station": station}))?;
            show(j, &v, |v| {
                format!(
                    "{} is now {}",
                    v["station"].as_str().unwrap_or(""),
                    v["state"].as_str().unwrap_or("")
                )
            });
        }
        Cmd::Upgrade { binary } => {
            let v = c.call("station.upgrade", json!({"binary": binary.canonicalize()?}))?;
            show(j, &v, |v| {
                let behind: Vec<&str> = v["peers_needing_update"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.as_str())
                    .collect();
                format!(
                    "upgrading {} -> {}; terminals stay open. If the new version does not start properly, this station goes back to {} by itself.{}",
                    v["from"].as_str().unwrap_or(""),
                    v["to"].as_str().unwrap_or(""),
                    v["from"].as_str().unwrap_or(""),
                    if behind.is_empty() {
                        String::new()
                    } else {
                        format!("\nThese stations speak an older protocol and cannot be controlled from here until they are upgraded too: {}", behind.join(", "))
                    }
                )
            });
        }
        Cmd::Rpc { method, params } => {
            let params: Value = match params {
                Some(p) => serde_json::from_str(&p)
                    .map_err(|e| anyhow::anyhow!("params are not JSON: {e}"))?,
                None => json!({}),
            };
            let v = c.call(&method, params)?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Daemon { .. }
        | Cmd::FixCodexSandbox
        | Cmd::Hook { .. }
        | Cmd::Mcp
        | Cmd::HandoffCheck
        | Cmd::Service { .. } => unreachable!(),
    }
    Ok(0)
}

/// `spandeck hook <agent>`: forward one hook payload to the daemon and print
/// its decision (core/guard.rs, which also says what a hook does when the
/// daemon stays away). The agent's timeout for this hook is in the
/// environment when the installed command line says it.
fn hook_main(paths: &Paths, agent: &str) -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let deadline = std::env::var(spandeck_agents::install::DEADLINE_ENV)
        .ok()
        .and_then(|secs| secs.trim().parse::<u64>().ok());
    // The entry for a question or a plan says which one it is.
    let answers = std::env::var(spandeck_agents::install::ANSWERS_ENV).is_ok_and(|v| v == "1");
    let reply = spandeck_core::guard::run(paths, agent, deadline, answers, &input);
    if !reply.stdout.is_empty() {
        println!("{}", reply.stdout);
    }
    reply.exit_code
}

/// `spandeck service --stop`. Windows: the daemon has no console and no
/// signals, and the program that keeps it running would start a daemon that
/// was merely ended, so the stop is asked for by name; the daemon then
/// stops as it does for SIGTERM on Unix (panes ended, the clean-stop mark
/// written), and its supervisor ends with it. Elsewhere the login service
/// is what stops it, and its command is printed.
fn service_stop(paths: &Paths) -> anyhow::Result<i32> {
    use spandeck_terminal::sys;
    if !sys::WINDOWS {
        println!(
            "The login service stops spandeck on this system:\n  {}",
            if cfg!(target_os = "macos") {
                "launchctl bootout gui/$(id -u)/dev.spandeck.spandeckd"
            } else {
                "systemctl --user stop spandeckd"
            }
        );
        return Ok(0);
    }
    let socket = paths.socket();
    if !sys::request_stop(&socket)? {
        println!("spandeck is not running here.");
        return Ok(0);
    }
    let asked = std::time::Instant::now();
    while sys::local::connect(&socket).is_ok() {
        if asked.elapsed() > Duration::from_secs(30) {
            anyhow::bail!("spandeck was asked to stop and is still answering after 30 seconds");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!(
        "spandeck was stopped and its panes closed. It starts again when you log on, or now with:\n  schtasks /Run /TN {}",
        spandeck_core::doctor::WINDOWS_TASK
    );
    Ok(0)
}

fn daemon_main(paths: Paths, handoff: Option<PathBuf>, no_web: bool) -> anyhow::Result<i32> {
    use spandeck_terminal::sys;
    if handoff.is_none() {
        // Descriptors inherited from a previous image whose handoff is not
        // being adopted: closed, so orphaned terminals do not stay half-open.
        sys::close_inherited();
    }
    // Windows: an upgrade starts this daemon beside the one it replaces,
    // which then ends. Its pipe, port and database are free once it is gone.
    if let Some(pid) = std::env::var("SPANDECK_WAIT_PID")
        .ok()
        .and_then(|p| p.parse::<u32>().ok())
    {
        sys::wait_for_exit(pid, std::time::Duration::from_secs(20));
    }
    std::env::remove_var("SPANDECK_WAIT_PID");
    // Windows: whenever this daemon hands over to another process (an
    // upgrade, a restart in place, going back), that process's pid goes in
    // the pid file before this one ends, for the supervisor to follow.
    sys::record_successor_in(&paths.pid_file());
    // Children and the next program image find the same station through these.
    std::env::set_var("SPANDECK_STATE_DIR", &paths.state_dir);
    std::env::set_var("SPANDECK_CONFIG_DIR", &paths.config_dir);
    std::fs::create_dir_all(&paths.state_dir)?;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file())?;
    use tracing_subscriber::prelude::*;
    let filter = tracing_subscriber::EnvFilter::try_from_env("SPANDECK_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(std::sync::Mutex::new(log_file)),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_writer(std::io::stderr),
        )
        .init();

    // Set when this image was started by an upgrade: the binary to go back to
    // if this one fails its health check.
    let previous = handoff
        .as_ref()
        .and(std::env::var_os("SPANDECK_PREVIOUS_EXE"))
        .map(PathBuf::from);
    std::env::remove_var("SPANDECK_PREVIOUS_EXE");
    let install_to = std::env::var_os("SPANDECK_INSTALL_TO")
        .map(PathBuf::from)
        .filter(|p| previous.is_some() && !p.as_os_str().is_empty());
    std::env::remove_var("SPANDECK_INSTALL_TO");
    let rolled_back = std::env::var("SPANDECK_ROLLED_BACK").ok();
    std::env::remove_var("SPANDECK_ROLLED_BACK");
    // Before any terminal has been taken over, going back is just starting
    // the previous binary on the same handoff file.
    fn back_untouched(
        previous: &Option<PathBuf>,
        handoff: &Option<PathBuf>,
        paths: &Paths,
        reason: String,
    ) -> anyhow::Error {
        if let (Some(prev), Some(file)) = (previous, handoff) {
            tracing::error!(%reason, "upgrade failed before it took over; going back");
            spandeck_core::daemon::restore_db_before_upgrade(paths);
            let err = spandeck_terminal::sys::replace_process(
                std::process::Command::new(prev)
                    .arg("daemon")
                    .arg("--handoff")
                    .arg(file)
                    .env("SPANDECK_ROLLED_BACK", &reason),
            );
            return anyhow::anyhow!("{reason}; and going back failed: {err}");
        }
        anyhow::anyhow!(reason)
    }

    let cfg = match Config::load(&paths) {
        Ok(c) => c,
        Err(e) => {
            return Err(back_untouched(
                &previous,
                &handoff,
                &paths,
                format!("the configuration did not load: {e}"),
            ))
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let socket = paths.socket();
    let token_file = paths.token_file();
    let saved_db = paths.db_before_upgrade();
    runtime.block_on(async move {
        // Another daemon already answering on this socket?
        if handoff.is_none() && sys::local::connect(&paths.socket()).is_ok() {
            anyhow::bail!("spandeckd is already running on {}", paths.socket().display());
        }
        let daemon = match Daemon::start(
            paths.clone(),
            cfg,
            StartOpts {
                handoff: handoff.clone(),
                exe: None,
            },
        ) {
            Ok(d) => d,
            Err(e) => {
                return Err(back_untouched(
                    &previous,
                    &handoff,
                    &paths,
                    format!("the station did not start: {e}"),
                ))
            }
        };
        // From here the terminals belong to this image; going back hands them over again.
        let mut unhealthy: Option<String> = None;
        match spandeck_core::server::bind(&daemon) {
            Ok(listener) => {
                tokio::spawn(spandeck_core::server::serve(daemon.clone(), listener));
            }
            Err(e) => unhealthy = Some(format!("the local socket did not open: {e}")),
        }
        if !no_web {
            match spandeck_web::start(daemon.clone()).await {
                Ok(addrs) => tracing::info!(?addrs, "Control Deck listening"),
                Err(e) => {
                    tracing::warn!(error = %e, "Control Deck is not available");
                    if previous.is_some() {
                        unhealthy.get_or_insert(format!("the Control Deck did not start: {e}"));
                    }
                }
            }
        }
        if let Some(prev) = &previous {
            // After an update: a health check. Does the station answer?
            if unhealthy.is_none() {
                let token = std::fs::read_to_string(&token_file).unwrap_or_default();
                let answered = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    tokio::task::spawn_blocking(move || {
                        spandeck_core::client::Client::connect(&socket, token.trim())
                            .and_then(|mut c| c.call("station.info", json!({})))
                            .is_ok()
                    }),
                )
                .await;
                if !matches!(answered, Ok(Ok(true))) {
                    unhealthy = Some("the station did not answer after starting".into());
                }
            }
            // Debug builds only, for the test of going back.
            if cfg!(debug_assertions) && std::env::var_os("SPANDECK_TEST_UNHEALTHY").is_some() {
                unhealthy = Some("the test said so".into());
            }
            if let Some(reason) = unhealthy {
                let err = daemon.roll_back(prev, &reason);
                tracing::error!(error = %err, "could not go back to the previous binary; staying on this one");
            } else {
                let _ = std::fs::remove_file(&saved_db);
                // An update that proved healthy also replaces the binary this
                // station is started from.
                if let Some(to) = &install_to {
                    // An update started by 0.4.0 or earlier names its own
                    // unpacked image here, not the program started at login.
                    let to = &daemon.paths.started_from(to);
                    let put = |from: &std::path::Path, to: &std::path::Path| {
                        let tmp = to.with_extension("new");
                        std::fs::copy(from, &tmp)?;
                        // Windows: `to` may be running (a `sd` command, a
                        // hook, the pane host); it is moved aside, not
                        // written over.
                        sys::replace_file(&tmp, to)
                    };
                    let installed = std::env::current_exe().and_then(|me| {
                        put(&me, to)?;
                        // The programs released beside it replace the ones
                        // installed beside it. A link to `spandeck` follows it.
                        for name in ["sd", "spandeckd", "spandeck-pane-host"] {
                            let name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
                            let (from, beside) = (me.with_file_name(&name), to.with_file_name(&name));
                            let own_file = std::fs::symlink_metadata(&beside)
                                .map(|m| m.is_file())
                                .unwrap_or(false);
                            if from.is_file() && own_file {
                                if let Err(e) = put(&from, &beside) {
                                    tracing::warn!(error = %e, to = %beside.display(), "could not replace an installed program");
                                }
                            }
                        }
                        Ok(())
                    });
                    if let Err(e) = installed {
                        tracing::warn!(error = %e, to = %to.display(), "the new version runs, but could not replace the installed binary");
                    }
                    // Windows: the program that keeps this station running
                    // was one of those replaced, and the copy that is
                    // running is the old one (or, after an update from
                    // before 15 Nov 2026, none is). The new one is started
                    // now and the old one leaves. Only for a station that
                    // has a supervisor, or is the one started at log on: a
                    // daemon run by hand is not given one.
                    let supervisor = to.with_file_name("spandeckd.exe");
                    let home = spandeck_core::paths::home_dir();
                    let at_log_on = sys::default_dirs(&home).0 == daemon.paths.state_dir
                        && spandeck_core::doctor::service_installed(&home);
                    if sys::WINDOWS
                        && supervisor.is_file()
                        && (at_log_on || sys::supervised(&daemon.paths.socket()))
                    {
                        let started = sys::spawn_detached(
                            std::process::Command::new(&supervisor).env("SPANDECKD_TAKE_OVER", "1"),
                        );
                        match started {
                            Ok(()) => tracing::info!(program = %supervisor.display(), "started the new supervisor"),
                            Err(e) => tracing::warn!(error = %e, program = %supervisor.display(), "could not start the new supervisor; the one from before, if any, goes on"),
                        }
                    }
                }
            }
        } else if let Some(reason) = unhealthy {
            anyhow::bail!(reason);
        }
        if let Some(reason) = &rolled_back {
            daemon.note_rolled_back(reason);
        }
        tracing::info!(station = %daemon.station, version = spandeck_core::VERSION, socket = %daemon.paths.socket().display(), "spandeckd ready");

        // A signal, or the owner removing this station with "also stop it".
        let station = daemon.paths.socket();
        tokio::select! {
            r = sys::shutdown_signal(&station) => r?,
            _ = daemon.quit.notified() => {}
        }
        // The ship's log says the station was stopped on purpose: the next
        // start then knows it did not follow a crash.
        daemon.note_stopping("asked to stop");
        // Asked to stop, not replaced: the panes end with the station.
        daemon.stop_pane_host();
        // What the database has not taken yet is written before the
        // process ends: two seconds at most.
        if !daemon.db.flush(std::time::Duration::from_secs(2)) {
            tracing::error!(waiting = daemon.db.stats().waiting, "stopping with changes the database did not take");
        }
        tracing::info!("spandeckd stopping");
        let _ = std::fs::remove_file(daemon.paths.socket());
        Ok(0)
    })
}

/// " and 2 other stations (a, b)": the fleet-mates a station says come with
/// it when it joins. Names from another machine: printed without anything
/// a terminal would act on.
fn came_with(with: &Value) -> String {
    let names: Vec<String> = with
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n.as_str())
        .map(|n| n.chars().filter(|c| !c.is_control()).collect())
        .collect();
    match names.len() {
        0 => String::new(),
        1 => format!(" and 1 other station ({})", names[0]),
        n => format!(" and {n} other stations ({})", names.join(", ")),
    }
}

/// What `fleet.remove` did, in the owner's words.
fn removed_summary(v: &Value) -> String {
    let st = v["station"].as_str().unwrap_or("");
    let names = |k: &str| -> Vec<&str> {
        v[k].as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str())
            .collect()
    };
    let mut out = vec![];
    if v["reached"] == json!(true) {
        let there = &v["there"];
        out.push(format!(
            "{st} left the fleet: {} pane(s) closed there, and it no longer trusts the other stations.",
            there["closed"].as_u64().unwrap_or(0)
        ));
        if there["stopping"] == json!(true) {
            out.push(format!(
                "spandeck is stopping on {st} and will not start at login. Its program ({}) and data ({}, {}) stay on that machine.",
                there["dirs"]["program"].as_str().unwrap_or("?"),
                there["dirs"]["state"].as_str().unwrap_or("?"),
                there["dirs"]["config"].as_str().unwrap_or("?")
            ));
        }
        if there["vault_token"] == json!(true) {
            out.push(format!(
                "{st} still holds its vault token. Revoke spandeck:{st} on the vault's Tokens page."
            ));
        }
    } else {
        out.push(format!(
            "{st} could not be reached ({}). It was removed from the fleet's side: its panes keep running until someone stops spandeck there, and it cannot act on the fleet or rejoin without a new approval. If it holds a vault token, revoke spandeck:{st} on the vault's Tokens page.",
            v["problem"].as_str().unwrap_or("no answer")
        ));
    }
    // The station that handed out the new key: this one, or, when this one
    // is the station removed, the one that did it in its place.
    let through = v["through"].as_str();
    if v["self"] == json!(true) && through.is_none() {
        let not_told = names("not_told");
        if !not_told.is_empty() {
            out.push(format!(
                "No other station could be told ({}). They still list this one, and still accept the old key, which this station has thrown away. Remove {st} at one of them as well.",
                not_told.join(", ")
            ));
        }
    } else {
        let (rekeyed, waiting) = (names("rekeyed"), names("waiting"));
        let first = through.unwrap_or("this station");
        out.push(if v["alone"] == json!(true) {
            format!("What stays is a fleet of one ({first}), with a new key.")
        } else {
            format!(
                "The fleet has a new key. Took it: {first}{}{}.",
                if rekeyed.is_empty() { "" } else { ", " },
                rekeyed.join(", ")
            )
        });
        if !waiting.is_empty() {
            out.push(format!(
                "Away, and still on the old key: {}. Each is handed the new key when it is next seen (for 30 days); until then it shows as not linked, and the removed station could still act on it.",
                waiting.join(", ")
            ));
        }
    }
    out.push("Browsers signed in from other devices must sign in again.".to_string());
    out.join("\n")
}

/// Call a method here, or on another station in the fleet through this one.
fn on_station(
    c: &mut spandeck_core::client::Client,
    station: Option<&str>,
    method: &str,
    params: Value,
) -> Result<Value, spandeck_core::client::ClientError> {
    match station {
        Some(st) => c.call(
            "peer.call",
            json!({"station": st, "method": method, "params": params}),
        ),
        None => c.call(method, params),
    }
}

/// The repairs that need root. Each says what it does before sudo asks.
fn fix_with_sudo() -> anyhow::Result<()> {
    use spandeck_core::doctor::{
        codex_sandbox_blocked, CODEX_SANDBOX_PROFILE, CODEX_SANDBOX_PROFILE_PATH,
    };
    let home = spandeck_core::paths::home_dir();
    if !codex_sandbox_blocked(&home) {
        println!("Nothing here needs an administrator.");
        return Ok(());
    }
    println!(
        "Codex cannot run commands on this machine: the system blocks the sandbox Codex uses.\n\
         This installs one AppArmor file, {CODEX_SANDBOX_PROFILE_PATH}, that allows it for\n\
         the sandbox program (bubblewrap) only. Nothing else on the system changes.\n\
         It needs your password once. Press Ctrl-C to skip.\n"
    );
    let tmp = std::env::temp_dir().join(format!("spandeck-codex-bwrap.{}", std::process::id()));
    std::fs::write(&tmp, CODEX_SANDBOX_PROFILE)?;
    let script = format!(
        "install -m 644 -o root -g root '{}' '{CODEX_SANDBOX_PROFILE_PATH}' && apparmor_parser -r '{CODEX_SANDBOX_PROFILE_PATH}'",
        tmp.display()
    );
    let status = std::process::Command::new("sudo")
        .args(["sh", "-c", &script])
        .status();
    let _ = std::fs::remove_file(&tmp);
    match status {
        Ok(s) if s.success() => {
            println!("Done. Start a new Codex pane; panes already open pick it up on their next command.");
            Ok(())
        }
        Ok(_) => anyhow::bail!("the profile was not installed"),
        Err(e) => anyhow::bail!("could not run sudo: {e}"),
    }
}
