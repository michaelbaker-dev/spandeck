//! `spandeck link add` and `spandeck link remove`: run by the owner, in a
//! terminal, at the machine they sit at (DECISIONS.md, "A station reached
//! over the owner's own SSH", the part of 20 Nov 2026 headed
//! `spandeck link add`; core: link/add.rs).
//!
//! `add` runs the owner's own `ssh`, once and with them present, so that
//! what ssh asks it asks them. Through that one session it looks at the
//! far machine, sends spandeck where there is none (the release for that
//! platform, which this machine's station gets and checks), sets up a
//! config file only where there is none and start at login where the
//! machine allows it, and then adds the `[[link]]` to this machine's
//! config, which the station that runs here takes up without a restart.
//!
//! What the far machine says is text: cleaned, cut short, and printed.
//! Nothing it says is run here, and the only thing taken from it is the
//! name of its platform.

use std::io::Write as _;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spandeck_core::client::Client;
use spandeck_core::link::add::{
    carrier_of, config_with_link, config_without_link, far_script, far_uninstall_script,
    interactive_argv, write_config, REMOTE_SH,
};
use spandeck_core::link::mux::clean;
use spandeck_core::link::{is_ssh, plain_name};
use spandeck_core::paneenv::LOGIN_NAMES;
use spandeck_core::supply::{self, Source, Supplied};
use spandeck_core::{Config, Paths, VERSION};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// How long the far side may say nothing: the owner may be signing in.
const QUIET: Duration = Duration::from_secs(15 * 60);
/// How long the sending of a release may take (as Add a machine's).
const SENDING: Duration = Duration::from_secs(50 * 60);

pub struct Add {
    pub name: Option<String>,
    /// `--pane-env` or `--no-pane-env`: the answer given in advance.
    pub pane_env: Option<bool>,
    /// The owner's destination and options for ssh.
    pub ssh: Vec<String>,
}

/// The command as the owner would type it.
fn typed(cmd: &[String]) -> String {
    cmd.iter()
        .map(|a| {
            if !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./@:=+,".contains(c))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The station on this machine, as its owner: the station's own token,
/// from its file, never what the environment says.
fn station(paths: &Paths) -> Option<Client> {
    let token = std::fs::read_to_string(paths.token_file()).ok()?;
    let mut c = Client::connect(&paths.socket(), token.trim()).ok()?;
    c.set_timeout(Some(Duration::from_secs(20)));
    c.call("station.info", json!({})).ok()?;
    Some(c)
}

/// A question to the owner, whose answer is No unless they say yes.
/// `given`: the answer they gave in advance.
fn ask(question: &str, given: Option<bool>) -> bool {
    if let Some(yes) = given {
        println!(
            "{question} {}",
            if yes {
                "Yes (as you said)."
            } else {
                "No (as you said)."
            }
        );
        return yes;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(n) if n > 0 => matches!(line.trim(), "y" | "Y" | "yes" | "Yes" | "YES"),
        _ => {
            println!("(no answer: No)");
            false
        }
    }
}

/// A version as the far side said it: only what a version is made of.
fn version_of(said: &str) -> String {
    said.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        .take(32)
        .collect()
}

/// What the far side's session came to.
#[derive(Default)]
struct Far {
    /// The release that was sent and installed, if one was.
    installed: Option<String>,
    /// How it starts at login: `systemd`, `launchd`, `cron` or `link`.
    method: String,
}

/// The owner's ssh, started with what it reads and writes in our hands,
/// and what it says on standard error shown as it says it.
struct Session {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    out: tokio::io::BufReader<tokio::process::ChildStdout>,
    /// The last of what ssh itself said.
    said: Arc<Mutex<String>>,
}

impl Session {
    fn start(cmd: &[String]) -> Result<Session, String> {
        let argv = interactive_argv(cmd, REMOTE_SH);
        let mut command = tokio::process::Command::new(&argv[0]);
        command.args(&argv[1..]);
        // ssh is not this station's program: it is told nothing of its
        // folders, its token or a pane.
        for name in [
            "SPANDECK_STATE_DIR",
            "SPANDECK_CONFIG_DIR",
            "SPANDECK_TOKEN",
            "SPANDECK_PANE",
            "SPANDECK_SOCKET",
        ] {
            command.env_remove(name);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("`{}` could not be started on this machine: {e}", argv[0]))?;
        let (Some(stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err("ssh gave no pipes".into());
        };
        // What ssh says is the owner's to read, at once and as it is; the
        // end of it is kept to say why a session that failed did.
        let said: Arc<Mutex<String>> = Arc::default();
        let keep = said.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let mut out = tokio::io::stderr();
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let _ = out.write_all(&buf[..n]).await;
                let _ = out.flush().await;
                let mut kept = keep.lock().unwrap();
                kept.push_str(&String::from_utf8_lossy(&buf[..n]));
                if kept.len() > 4096 {
                    let cut = kept.len() - 2048;
                    let cut = (cut..kept.len())
                        .find(|i| kept.is_char_boundary(*i))
                        .unwrap_or(kept.len());
                    kept.drain(..cut);
                }
            }
        });
        Ok(Session {
            child,
            stdin: Some(stdin),
            out: tokio::io::BufReader::new(stdout),
            said,
        })
    }

    async fn write(&mut self, text: &str) -> bool {
        let Some(stdin) = self.stdin.as_mut() else {
            return false;
        };
        stdin.write_all(text.as_bytes()).await.is_ok() && stdin.flush().await.is_ok()
    }

    /// The next line the far side printed: `Ok(None)` when it has ended.
    async fn line(&mut self) -> Result<Option<String>, String> {
        // Whatever bytes it printed: a line is text once it is cleaned.
        let mut raw = vec![];
        let mut capped = (&mut self.out).take(64 * 1024);
        match tokio::time::timeout(QUIET, capped.read_until(b'\n', &mut raw)).await {
            Ok(Ok(n)) if n > 0 => Ok(Some(clean(&String::from_utf8_lossy(&raw), 400))),
            Ok(_) => Ok(None),
            Err(_) => Err(format!(
                "the far side said nothing for {} minutes, so the connection was ended",
                QUIET.as_secs() / 60
            )),
        }
    }

    /// How ssh ended, in words, with the last thing it said.
    async fn ended(&mut self) -> (bool, String) {
        self.stdin = None;
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await;
        let (ok, how) = match status {
            Ok(Ok(s)) if s.success() => (true, String::new()),
            Ok(Ok(s)) => (false, format!(" ({s})")),
            _ => {
                let _ = self.child.kill().await;
                (false, String::new())
            }
        };
        let said = self.said.lock().unwrap().clone();
        let last = said.lines().map(|l| clean(l, 200)).rfind(|l| !l.is_empty());
        (
            ok,
            match last {
                Some(last) => format!("{how}: {last}"),
                None => how,
            },
        )
    }
}

/// Why this machine's station has no release to send, in sentences.
fn no_release_words(name: &str, platform: &str, v: &Value) -> String {
    let text = |k: &str| clean(v[k].as_str().unwrap_or(""), 300);
    let host = match text("host") {
        h if h.is_empty() => "the release host".to_string(),
        h => h,
    };
    let tag = text("tag");
    match v["none"].as_str().unwrap_or("") {
        "not_published" => format!(
            "{host} has no {platform} build of {tag}, so there is nothing to send to {name}. Nothing was installed."
        ),
        "checksum" => format!(
            "This machine downloaded the {platform} build of {tag} to send to {name}, and it does not match its published checksum, so nothing was sent and nothing was installed."
        ),
        "own_copy" => format!(
            "{host} does not answer this machine, and its own copy could not be packed to send ({}). Nothing was installed.",
            text("why")
        ),
        "slow" => format!(
            "This machine did not have the {platform} build to send to {name} within seven minutes. Nothing was installed."
        ),
        _ => {
            let reach = if v["reached"] == json!(true) {
                format!("{host} answers this machine but did not give it")
            } else {
                format!("{host} does not answer this machine")
            };
            let own = supply::platform_words(v["own"].as_str().unwrap_or(""));
            format!(
                "{name} needs the {platform} build of spandeck, and this machine cannot get it to send: {reach} ({}), nothing is kept from an earlier download, and this machine is another kind ({own}), so its own copy would not run there. Nothing was installed. Run `spandeck link add` again when this machine reaches {host}, or name a copy of the release page it can reach as `[update] url` in this machine's config.",
                text("why")
            )
        }
    }
}

/// Where the bytes that are sent came from, in words.
fn from_words(v: &Value) -> String {
    let host = clean(v["host"].as_str().unwrap_or("the release host"), 80);
    let via = clean(v["via"].as_str().unwrap_or("another station"), 64);
    match v["from"].as_str().unwrap_or("") {
        "published" => format!("from {host}, checked against its published SHA-256"),
        "kept" => format!(
            "{host} does not answer now: the release this machine downloaded and checked earlier"
        ),
        "own" => format!("{host} does not answer: this machine's own copy"),
        "fleet" => format!("got from {via}, which checked it against its published SHA-256"),
        _ => format!("{via}'s own copy"),
    }
}

/// The one ssh session with the far side: look, send, set up.
async fn session(
    paths: &Paths,
    name: &str,
    cmd: &[String],
    pane_env: Option<bool>,
    near: &mut Option<Client>,
) -> Result<Far, String> {
    let mut s = Session::start(cmd)?;
    let script = far_script();
    if !s.write(&supply::unix_boot_line(script.len())).await {
        let (_, how) = s.ended().await;
        return Err(format!("ssh ended before anything was sent{how}"));
    }
    let mut far = Far::default();
    let (mut taken, mut done, mut stopped) = (false, false, false);
    let mut failed: Option<String> = None;
    // Why, when it is this side that knows.
    let mut why: Option<String> = None;
    let mut target: Option<&'static str> = None;
    let mut install = false;
    let mut supplied: Option<(Supplied, Option<std::path::PathBuf>)> = None;
    let mut asked_release = false;
    let platform = |t: Option<&'static str>| supply::platform_words(t.unwrap_or(""));
    while let Some(line) = s.line().await? {
        let Some(mark) = line.strip_prefix("@spandeck ") else {
            // The install script's two closing lines are for someone who
            // ran it by hand.
            if !line.is_empty()
                && !line.starts_with("Next:")
                && !line.starts_with("Add it to your PATH")
            {
                println!("  {name}: {line}");
            }
            continue;
        };
        let (mark, rest) = mark.split_once(' ').unwrap_or((mark, ""));
        match mark {
            // The line that takes the script is ready for it.
            "script" if !taken => {
                taken = true;
                s.write(&script).await;
            }
            // The one thing taken from the far side: its platform's
            // name, which must be one that has a release.
            "platform" => {
                target = supply::TARGETS
                    .iter()
                    .copied()
                    .find(|t| *t == rest && !t.ends_with("windows-msvc"));
                if target.is_none() {
                    why = Some(format!(
                        "{name} named a platform there is no release for. Nothing was changed there."
                    ));
                    s.write("stop\n").await;
                }
            }
            "have" if target.is_some() && !install => {
                let (version, knows) = rest.split_once(' ').unwrap_or((rest, "no"));
                let (version, knows) = (version_of(version), knows == "yes");
                let kind = platform(target);
                let decision = if version == "none" || version.is_empty() {
                    println!(
                        "{name} is {kind} and has no spandeck (looked in ~/.local/bin and on the PATH there)."
                    );
                    true
                } else if !knows {
                    println!("{name} is {kind} and has spandeck {version}, from before links: a link cannot reach it.");
                    let yes = ask(
                        &format!("Replace it with the newest release? A station running on {name} is brought to it with its terminals kept."),
                        None,
                    );
                    if !yes {
                        why = Some(format!(
                            "spandeck on {name} was left as it is. A link needs a newer one there, so no link was added; nothing was changed on either machine."
                        ));
                        s.write("stop\n").await;
                        continue;
                    }
                    true
                } else if version != "unknown" && supply::older(&version, VERSION) {
                    println!(
                        "{name} is {kind} and has spandeck {version}, older than this machine's {VERSION}."
                    );
                    let yes = ask(
                        &format!("Replace it with the newest release? A station running on {name} is brought to it with its terminals kept."),
                        None,
                    );
                    if !yes {
                        println!("Left as it is: spandeck {version} stays on {name}.");
                    }
                    yes
                } else {
                    println!(
                        "{name} is {kind} and has spandeck {version}: nothing is installed or replaced."
                    );
                    false
                };
                if decision && near.is_none() {
                    why = Some(
                        "The station on this machine is not running, and it is what gets the release to send (it downloads and checks it, or packs its own copy). Start it (`spandeck daemon`, or `spandeck service --install` for every login) and run this again. Nothing was changed on either machine.".into(),
                    );
                    s.write("stop\n").await;
                    continue;
                }
                install = decision;
                s.write(if decision { "install\n" } else { "keep\n" }).await;
            }
            // The far side waits for the release: once.
            "supply" if install && !asked_release => {
                asked_release = true;
                let (Some(target), Some(client)) = (target, near.as_mut()) else {
                    s.write("none\n").await;
                    continue;
                };
                let kind = platform(Some(target));
                println!("Getting the {kind} build of spandeck on this machine, to send it over ssh…");
                client.set_timeout(Some(Duration::from_secs(8 * 60)));
                let got = tokio::task::block_in_place(|| {
                    client.call("link.release", json!({"target": target}))
                });
                client.set_timeout(Some(Duration::from_secs(20)));
                let v = match got {
                    Ok(v) if v["none"].is_null() => v,
                    Ok(v) => {
                        why = Some(no_release_words(name, kind, &v));
                        s.write("none\n").await;
                        continue;
                    }
                    Err(e) => {
                        why = Some(if e.code == "not_found" {
                            "The station running on this machine is from before `spandeck link add`: restart it (`spandeck restart`) and run this again. Nothing was installed.".to_string()
                        } else {
                            format!(
                                "The station on this machine did not hand over a release to send: {}. Nothing was installed.",
                                clean(&e.message, 300)
                            )
                        });
                        s.write("none\n").await;
                        continue;
                    }
                };
                let tag = v["tag"].as_str().unwrap_or("");
                let path = std::path::PathBuf::from(v["path"].as_str().unwrap_or(""));
                // The file is read here, and must be what the station
                // said it checked: its size and its SHA-256.
                let file = Supplied::of_file(tag, target, &path, Source::Published)
                    .ok()
                    .filter(|f| {
                        supply::valid_tag(tag)
                            && Some(f.size) == v["size"].as_u64()
                            && Some(f.sha256.as_str()) == v["sha256"].as_str()
                    });
                let Some(file) = file else {
                    why = Some("The release this machine's station made ready is not the file it said it checked. Nothing was sent and nothing was installed.".into());
                    s.write("none\n").await;
                    continue;
                };
                println!(
                    "Sending the {kind} build of {tag} to {name} over ssh, {} ({}).",
                    supply::size_words(file.size),
                    from_words(&v)
                );
                s.write(&supply::answer_line(&file)).await;
                let temporary = v["temporary"].as_str().map(std::path::PathBuf::from);
                supplied = Some((file, temporary));
            }
            // It has made room and waits for the bytes.
            "ready" => {
                let (Some((file, temporary)), Some(stdin)) = (supplied.take(), s.stdin.as_mut())
                else {
                    continue;
                };
                let (mut tenth, mut at) = (0, Instant::now());
                let sent = tokio::time::timeout(
                    SENDING,
                    supply::send(stdin, &file, false, |n| {
                        let now = n * 10 / file.size.max(1);
                        if now > tenth && n < file.size && at.elapsed() >= Duration::from_secs(2) {
                            (tenth, at) = (now, Instant::now());
                            println!(
                                "  … {} of {} sent.",
                                supply::size_words(n),
                                supply::size_words(file.size)
                            );
                        }
                    }),
                )
                .await;
                // A copy packed for this one sending, in the station's
                // own folder for such: deleted now.
                if let Some(dir) = temporary.filter(|d| {
                    d.starts_with(paths.state_dir.join("supply"))
                        && d.file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with("own-"))
                }) {
                    let _ = std::fs::remove_dir_all(dir);
                }
                match sent {
                    Ok(Ok(())) => {
                        println!(
                            "Sent {} to {name}, which is checking and installing it.",
                            supply::size_words(file.size)
                        );
                        far.installed = Some(file.tag.clone());
                    }
                    _ => {
                        why = Some(format!(
                            "The sending to {name} stopped before it was whole. Nothing was installed there."
                        ));
                        // The far side sees its input end and says what arrived.
                        s.stdin = None;
                    }
                }
            }
            "config" => match rest {
                "kept" => println!("{name} has a config file: it was left as it is."),
                "written no" => println!(
                    "{name} had no config file: a small one was written, with `discovery = false` since that machine has no Tailscale."
                ),
                _ => println!("{name} had no config file: a small one was written."),
            },
            "env" => {
                let names: Vec<&str> = rest
                    .split_whitespace()
                    .filter(|n| LOGIN_NAMES.contains(n))
                    .collect();
                if rest == "?" {
                    println!("The spandeck on {name} is from before `[pane_env]`, so your login shell there was not looked at for a proxy.");
                } else if names.is_empty() {
                    // Nothing is asked; the far side does not wait.
                    if pane_env == Some(true) {
                        println!("Your login shell on {name} sets no proxy or certificate variable that `[pane_env]` there does not have.");
                    }
                } else {
                    println!(
                        "Your login shell on {name} sets {}. A station started at login does not have them, and agents there may need them.",
                        names.join(", ")
                    );
                    let yes = ask(
                        &format!("Copy them to `[pane_env]` in {name}'s config, for every pane there? The values stay on {name} and are not shown; that file is then readable by you alone."),
                        pane_env,
                    );
                    if !yes {
                        println!("Not copied. `spandeck link add --name {name} --pane-env` copies them later.");
                    }
                    s.write(if yes { "copy\n" } else { "skip\n" }).await;
                }
            }
            "kept" if rest == "?" => println!(
                "They could not be written to {name}'s config file, which is as it was."
            ),
            "kept" => println!(
                "Copied to `[pane_env]` on {name}. Panes started there from now on have them."
            ),
            "start" => {
                let mut said = rest.split(' ');
                let (method, linger, now) = (
                    said.next().unwrap_or(""),
                    said.next() == Some("yes"),
                    said.next() == Some("yes"),
                );
                let later = if now {
                    ""
                } else {
                    " The station that runs now was started another way and is not ended for this: the service takes over the next time."
                };
                far.method = match method {
                    "systemd" | "launchd" | "cron" => method.to_string(),
                    _ => "link".to_string(),
                };
                println!(
                    "{}",
                    match (far.method.as_str(), linger) {
                        ("systemd", true) => format!("Start at login on {name}: a systemd user service with lingering on, so it starts when that machine does and goes on after you log out.{later}"),
                        ("systemd", false) => format!("Start at login on {name}: the systemd user service that was there already. Lingering is off for you there and could not be turned on, so it starts at your first login after a reboot and may stop when your last session ends.{later}"),
                        ("launchd", _) if now => format!("Start at login on {name}: a launchd agent."),
                        ("launchd", _) => format!("Start at login on {name}: a launchd agent, from your next login at that Mac (it could not be loaded from an ssh session)."),
                        ("cron", _) => format!("Start at login on {name}: the line in your crontab there shown above, so it starts when that machine does. (A systemd user service was not used: there is none there, or it would stop when you log out.)"),
                        _ => format!("Start at login on {name}: none could be set up without an administrator (no systemd user service that outlives your logout, no crontab). spandeck there is started by the link: after a reboot it is down until this machine connects, and on a host that ends a user's programs at logout it also ends when the last ssh session does."),
                    }
                );
            }
            "station" => println!("spandeck {} is running on {name}.", version_of(rest)),
            "fail" => failed = Some(rest.to_string()),
            "stopped" => stopped = true,
            "done" => done = true,
            _ => {}
        }
    }
    let (ok, how) = s.ended().await;
    if let Some(why) = why {
        return Err(why);
    }
    if !taken {
        return Err(format!(
            "ssh ended before the far side took anything{how}. Nothing was changed on either machine. (The far side must be a Mac or a Linux machine: one whose shell has no `sh`, as Windows', cannot be set up this way.)"
        ));
    }
    if let Some(kind) = failed {
        return Err(match kind.as_str() {
            "unsupported" => format!("spandeck has no release for {name} (what it said is above). Nothing was installed."),
            "supply" => format!("{name} was sent no release. Nothing was installed."),
            "transfer" => format!("What was sent did not arrive whole on {name}. Nothing was installed; run this again."),
            "install" => format!("The install script failed on {name} (what it said is above)."),
            "config" => format!("A config file could not be written on {name} (what it said is above)."),
            "start" => format!("spandeck is installed on {name} and did not start there. Its log is ~/.local/state/spandeck/spandeckd.log on {name}."),
            _ => format!("Setting up {name} failed (what it said is above)."),
        });
    }
    if stopped || !done || !ok {
        return Err(format!(
            "The connection to {name} ended before it was set up{how}. Run this again: what is there is looked at first, and nothing is installed twice."
        ));
    }
    Ok(far)
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?)
}

/// `spandeck link add`.
pub fn add(paths: &Paths, opts: Add) -> anyhow::Result<i32> {
    let file = paths.config_file();
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => anyhow::bail!("{}: {e}", file.display()),
    };
    let cfg = Config::load(paths)
        .map_err(|e| anyhow::anyhow!("the config file does not read, so nothing was done: {e}"))?;
    let named = |name: &str| cfg.link.iter().find(|l| l.name == name);
    // Which link: a new one, or one that is there, looked at again.
    let (cmd, name, there) = if opts.ssh.is_empty() {
        let Some(name) = opts.name else {
            anyhow::bail!("say where ssh is to connect, as you would to ssh itself: `spandeck link add me@devbox`");
        };
        let Some(link) = named(&name) else {
            anyhow::bail!("there is no link named {name} to look at again: say where ssh is to connect (`spandeck link add --name {name} me@host`)");
        };
        (link.cmd.clone(), name, true)
    } else {
        let (cmd, default) = carrier_of(&opts.ssh).map_err(|e| anyhow::anyhow!(e))?;
        let Some(name) = opts.name.or(default) else {
            anyhow::bail!("that destination gives no name for the link: give one with --name (letters, digits, `-`, `_`, `.`), before the destination");
        };
        if !plain_name(&name) {
            anyhow::bail!("a link's name is 1 to 32 letters, digits, `-`, `_` or `.`");
        }
        match named(&name) {
            Some(link) if link.cmd == cmd => (cmd, name, true),
            Some(link) => anyhow::bail!(
                "there is a link named {name} already, with another command (`{}`). `spandeck link add --name {name}` looks at that one again; `spandeck link remove {name}` takes it out; or give this one another name with --name, before the destination",
                typed(&link.cmd)
            ),
            None => (cmd, name, false),
        }
    };
    if !is_ssh(&cmd[0]) {
        anyhow::bail!(
            "the link {name} is carried by `{}`, not by ssh: `spandeck link add` sets up a far side over ssh only",
            cmd[0]
        );
    }
    // Before anything is done on the far side: can the link be written?
    let with = match there {
        true => None,
        false => Some(config_with_link(&text, &name, &cmd).map_err(|e| {
            anyhow::anyhow!(
                "{} cannot take the link, so nothing was done: {e}",
                file.display()
            )
        })?),
    };
    let mut near = station(paths);

    println!(
        "Connecting with your own ssh: `{}`. If ssh asks for a password, a code, a touch or whether to trust the host's key, it is asking you; spandeck sees none of what you answer and keeps none of it.",
        typed(&cmd)
    );
    let far = match runtime()?.block_on(session(paths, &name, &cmd, opts.pane_env, &mut near)) {
        Ok(far) => far,
        Err(why) => {
            eprintln!("{why}");
            return Ok(1);
        }
    };

    // The near side: the `[[link]]`, and the station taking it up.
    match with {
        Some(with) => {
            write_config(&file, &with)?;
            println!(
                "Added the link {name} to {} (a `[[link]]` at its end; the rest is as it was).",
                file.display()
            );
        }
        None => println!("The link {name} is in {} already.", file.display()),
    }
    let mut code = 0;
    // Asked again: the one from before may have waited a long time.
    match station(paths) {
        None => {
            println!(
                "The station on this machine is not running, so the link is not open yet. Start it (`spandeck daemon`, or `spandeck service --install` for every login), then `spandeck link open {name}` signs this machine's browser in and prints the address."
            );
        }
        Some(mut c) => match open(&mut c, &name) {
            Ok(words) => println!("{words}"),
            Err(words) => {
                eprintln!("{words}");
                code = 1;
            }
        },
    }
    println!(
        "\nWhat is on {name} now, all of it under your home folder there: the program (~/.local/bin/spandeck and sd{}), its settings (~/.config/spandeck) and its state (~/.local/state/spandeck: history, the archive of sessions, the log){}. `spandeck link remove {name} --uninstall` stops it and takes the program and the start at login away, and keeps the settings and the state; with `--erase` those go too.",
        if far.installed.is_some() { "" } else { ", or wherever it was already" },
        match far.method.as_str() {
            "systemd" => ", and a systemd user service",
            "launchd" => ", and a launchd agent",
            "cron" => ", and one line in your crontab",
            _ => "",
        },
    );
    Ok(code)
}

/// Have the station here read its links again, wait for this one, and do
/// what `spandeck link open` does.
fn open(c: &mut Client, name: &str) -> Result<String, String> {
    if let Err(e) = c.call("link.reload", json!({})) {
        return Err(if e.code == "not_found" {
            format!("The station running on this machine is from before `spandeck link add`: restart it (`spandeck restart`), then `spandeck link open {name}`.")
        } else {
            format!("The station on this machine did not take the link up: {}. `spandeck link list` says how it stands.", clean(&e.message, 300))
        });
    }
    let began = Instant::now();
    let mut kicked = false;
    let row = loop {
        let rows = c.call("link.list", json!({})).map_err(|e| e.message)?;
        let row = rows
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["name"] == json!(name))
            .cloned()
            .unwrap_or(Value::Null);
        match row["state"].as_str().unwrap_or("") {
            "up" => break row,
            "held" => {
                return Err(format!(
                    "The link {name} is held closed: `spandeck link up {name}` opens it, then `spandeck link open {name}`."
                ))
            }
            "not started" => {
                return Err(format!(
                    "The link {name} was not started: {}.",
                    clean(row["said"].as_str().unwrap_or(""), 300)
                ))
            }
            // It waited for the owner, who has just signed in: now.
            "needs you" if !kicked => {
                kicked = true;
                let _ = c.call("link.up", json!({"name": name}));
            }
            _ => {}
        }
        if began.elapsed() > Duration::from_secs(45) {
            return Err(format!(
                "The link {name} is added and is not up yet ({}): {}\n`spandeck link list` says how it stands, and `spandeck link open {name}` signs this machine's browser in when it is up.",
                clean(row["state"].as_str().unwrap_or("not listed"), 40),
                clean(row["said"].as_str().unwrap_or(""), 400)
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let v = c
        .call("link.open", json!({"name": name}))
        .map_err(|e| format!("The link {name} is up, and the sign-in could not be made: {}. `spandeck link open {name}` tries again.", clean(&e.message, 300)))?;
    Ok(format!(
        "The station on this machine has the link up, without a restart: {}'s deck is at port {} here.\n\n{}",
        name,
        row["port"],
        crate::link_cmd::open_words(name, &v)
    ))
}

/// `spandeck link remove`.
pub fn remove(paths: &Paths, name: &str, uninstall: bool, erase: bool) -> anyhow::Result<i32> {
    let file = paths.config_file();
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let cfg = Config::load(paths)
        .map_err(|e| anyhow::anyhow!("the config file does not read, so nothing was done: {e}"))?;
    let Some(link) = cfg.link.iter().find(|l| l.name == name) else {
        let names: Vec<&str> = cfg.link.iter().map(|l| l.name.as_str()).collect();
        anyhow::bail!(
            "there is no link named {name}{}",
            if names.is_empty() {
                String::new()
            } else {
                format!("; there is: {}", names.join(", "))
            }
        );
    };
    let without = config_without_link(&text, name).map_err(|e| anyhow::anyhow!(e))?;
    if uninstall {
        if !is_ssh(&link.cmd[0]) {
            anyhow::bail!("the link {name} is carried by `{}`, not by ssh: spandeck is uninstalled over ssh only", link.cmd[0]);
        }
        println!(
            "Connecting with your own ssh: `{}`, to stop spandeck on {name} and take it away{}.",
            typed(&link.cmd),
            if erase {
                ", with its settings and its state"
            } else {
                "; its settings and its state are kept"
            }
        );
        if let Err(why) = runtime()?.block_on(far_uninstall(name, &link.cmd, erase)) {
            eprintln!("{why}\nThe link {name} was left in this machine's config.");
            return Ok(1);
        }
    }
    write_config(&file, &without)?;
    // The station here lets go of it; what was kept for it goes.
    let kept = paths.state_dir.join("links").join(format!("{name}.json"));
    let running = match station(paths) {
        Some(mut c) => c.call("link.reload", json!({})).is_ok(),
        None => false,
    };
    let _ = std::fs::remove_file(&kept);
    println!(
        "Removed the link {name} from this machine: its `[[link]]` in {} and what was kept for it (its port, and which browser entry on {name} was this machine's).{}",
        file.display(),
        if running { " The station here has closed it." } else { "" }
    );
    if uninstall {
        println!("spandeck on {name} was stopped and uninstalled.");
    } else {
        println!(
            "Nothing on {name} was touched: spandeck is still installed there, a station that was running still runs, and this machine's browser entry is still listed under Access there. To take spandeck off {name} as well, the link has to be there: `spandeck link add --name {name} {}` (it installs nothing where spandeck is), then `spandeck link remove {name} --uninstall`.",
            typed(&link.cmd)
        );
    }
    Ok(0)
}

/// Stop and uninstall spandeck on the far side, over the owner's ssh,
/// with the script the Control Deck's "Uninstall spandeck on …" uses.
async fn far_uninstall(name: &str, cmd: &[String], erase: bool) -> Result<(), String> {
    let mut s = Session::start(cmd)?;
    s.write(&far_uninstall_script(erase)).await;
    // The script is read to its end by the shell there.
    s.stdin = None;
    let (mut done, mut failed) = (false, None::<String>);
    while let Some(line) = s.line().await? {
        match line.strip_prefix("@spandeck ") {
            None if !line.is_empty() => println!("  {name}: {line}"),
            Some("done") => done = true,
            Some(mark) => {
                if let Some(kind) = mark.strip_prefix("fail ") {
                    failed = Some(kind.to_string());
                }
            }
            None => {}
        }
    }
    let (_, how) = s.ended().await;
    match (done, failed) {
        (true, None) => Ok(()),
        (_, Some(kind)) => Err(format!(
            "spandeck on {name} was not uninstalled: it failed at \"{}\" (what it said is above).",
            clean(&kind, 40)
        )),
        _ => Err(format!(
            "The connection to {name} ended before spandeck was uninstalled there{how}."
        )),
    }
}
