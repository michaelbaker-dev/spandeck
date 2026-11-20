//! `spandeck link add` and `spandeck link remove` (DECISIONS.md, "A station
//! reached over the owner's own SSH", the part of 20 Nov 2026 headed
//! `spandeck link add`).
//!
//! The reason for every test here is the owner's decision of that day: on
//! a machine reached with their own ssh, where only the home folder may be
//! assumed, spandeck is put there over that ssh, nothing is downloaded
//! there, nothing that is there is replaced without a yes, and nothing
//! that works today changes.
//!
//! No real ssh is run. The owner's `ssh` is a stand-in, a script of that
//! name: it runs the command it is given in a shell whose home folder is a
//! throwaway one and whose PATH holds a handful of the system's tools and
//! stand-ins for `systemctl`, `loginctl`, `launchctl` and `crontab`, so
//! that this user's own services and crontab are never asked anything.
//! The release host is the stand-in the update tests use. The stand-in
//! agent program; no real agent.
//!
//!   cargo test -p spandeck --test linkadd -- --ignored
#![cfg(unix)]
mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use common::fake_releases::{Releases, TAG};
use common::*;
use serde_json::{json, Value};

/// What the proxy's password is in these tests: it must be in the far
/// station's config file and nowhere else.
const SECRET: &str = "s3cret-pw-71c4";

fn executable(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The far "machine": a station's folders that nothing has started, and
/// the stand-in for the owner's ssh that reaches a shell there.
struct Far {
    station: Station,
    dir: tempfile::TempDir,
}

impl Far {
    fn at(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn ssh(&self) -> String {
        self.at("standin/ssh").display().to_string()
    }

    fn bin(&self) -> PathBuf {
        self.station.root.join("home/.local/bin/spandeck")
    }

    fn crontab(&self) -> String {
        std::fs::read_to_string(self.at("crontab.txt")).unwrap_or_default()
    }

    /// `crontab`: does it work on the far machine?
    fn new(crontab: bool) -> Far {
        let station = Station::not_started("far", "");
        let dir = tempfile::tempdir().unwrap();
        let at = |name: &str| dir.path().join(name);
        std::fs::create_dir_all(at("tmp")).unwrap();
        // The far machine's programs: these of the system's, and no other.
        let tools = at("remote-bin");
        std::fs::create_dir_all(&tools).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        for tool in [
            "sh",
            "uname",
            "sed",
            "head",
            "id",
            "grep",
            "cat",
            "mkdir",
            "chmod",
            "rm",
            "sleep",
            "nohup",
            "tr",
            "wc",
            "cut",
            "mktemp",
            "tar",
            "gzip",
            "cp",
            "mv",
            "sha256sum",
            "shasum",
            "dirname",
            "basename",
            "env",
        ] {
            if let Some(real) = std::env::split_paths(&path)
                .map(|d| d.join(tool))
                .find(|p| p.is_file())
            {
                std::os::unix::fs::symlink(real, tools.join(tool)).unwrap();
            }
        }
        // No user service manager, as on a host that gives a user none.
        for tool in ["systemctl", "loginctl", "launchctl"] {
            executable(&tools.join(tool), "#!/bin/sh\nexit 1\n");
        }
        // A crontab of its own, in a file: or one this user may not use.
        executable(
            &tools.join("crontab"),
            &if crontab {
                format!(
                    "#!/bin/sh\nf={}\ncase \"${{1:-}}\" in\n  -l) if [ -f \"$f\" ]; then cat \"$f\"; else echo \"no crontab for me\" >&2; exit 1; fi ;;\n  -) cat > \"$f\" ;;\n  *) exit 2 ;;\nesac\n",
                    at("crontab.txt").display()
                )
            } else {
                "#!/bin/sh\necho 'You (me) are not allowed to use this program (crontab)' >&2\nexit 1\n".to_string()
            },
        );
        // The login shell there sets the company's proxy, as a file in
        // /etc/profile.d would: a shell that is not a login shell has not.
        executable(
            &at("loginshell"),
            &format!(
                "#!/bin/sh\nif [ \"$1\" = -l ]; then\n  HTTPS_PROXY='http://user:{SECRET}@proxy.corp:3128'\n  NO_PROXY=localhost,.corp\n  export HTTPS_PROXY NO_PROXY\n  echo 'Welcome to devbox.'\n  shift\nfi\nexec sh \"$@\"\n"
            ),
        );
        // ssh: every word it was given is kept, and the last one is run
        // by a shell on the far machine.
        executable(
            &at("standin/ssh"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nfor last; do :; done\nexec /usr/bin/env -i PATH={tools} HOME={home} TMPDIR={tmp} SHELL={shell} SPANDECK_STATE_DIR={state} SPANDECK_CONFIG_DIR={config} SPANDECK_FAKE_DIR={fake} SPANDECK_LOG=warn sh -c \"$last\"\n",
                log = at("ssh.log").display(),
                tools = tools.display(),
                home = station.root.join("home").display(),
                tmp = at("tmp").display(),
                shell = at("loginshell").display(),
                state = station.root.join("state").display(),
                config = station.root.join("config").display(),
                fake = station.root.join("fake").display(),
            ),
        );
        Far { station, dir }
    }

    /// A program called `spandeck` that is there already and is not this
    /// build: it says a version, knows `link` or does not, and says a
    /// station runs.
    fn old_spandeck(&self, version: &str, knows_link: bool) -> String {
        let body = format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo \"spandeck {version}\" ;;\n  status) echo \"station far  spandeck {version}  0 pane(s)\" ;;\n  link) {} ;;\n  *) exit 2 ;;\nesac\n",
            if knows_link {
                "[ \"$2\" = --help ] || exit 2"
            } else {
                "echo \"error: unrecognized subcommand 'link'\" >&2; exit 2"
            }
        );
        executable(&self.bin(), &body);
        body
    }
}

impl Drop for Far {
    fn drop(&mut self) {
        // The station the far side started is not this test's child: it
        // is ended by its own process id, as it says it.
        let pid = self
            .station
            .try_call("station.info", json!({}))
            .ok()
            .and_then(|i| i["pid"].as_u64());
        if let Some(pid) = pid {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
            wait_for("the far station to end", 20, || {
                self.station
                    .try_call("station.info", json!({}))
                    .is_err()
                    .then_some(())
            });
        }
    }
}

/// `spandeck link <args>` at the near station, as the owner types it, with
/// what they answer on standard input.
fn link(near: &Station, args: &[&str], answers: &str) -> (bool, String) {
    use std::io::Write;
    let mut child = near
        .command(SPANDECK)
        .arg("link")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(answers.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // SPANDECK_SHOW=1 with --nocapture: what the owner would read.
    if std::env::var_os("SPANDECK_SHOW").is_some() {
        eprintln!("$ spandeck link {}\n{said}", args.join(" "));
    }
    (out.status.success(), said)
}

fn config_of(s: &Station) -> String {
    std::fs::read_to_string(s.root.join("config/config.toml")).unwrap()
}

fn has(said: &str, piece: &str) {
    assert!(said.contains(piece), "{piece:?} is not in:\n{said}");
}

fn release_file() -> String {
    format!(
        "spandeck-{TAG}-{}-{}.tar.gz",
        std::env::consts::ARCH,
        if cfg!(target_os = "macos") {
            "apple-darwin"
        } else {
            "unknown-linux-gnu"
        }
    )
}

/// Every file under a folder that holds these bytes.
fn files_holding(dir: &Path, needle: &str, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            files_holding(&path, needle, found);
        } else if meta.is_file() && meta.len() < 64 << 20 {
            let bytes = std::fs::read(&path).unwrap_or_default();
            if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                found.push(path);
            }
        }
    }
}

async fn page_through(port: u64) -> (u16, String) {
    let got = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .unwrap();
    (
        got.status().as_u16(),
        got.text().await.unwrap_or_default().to_lowercase(),
    )
}

/// Done when, on a far machine that has a home folder and nothing else
/// (no spandeck, no route to a release host, no service manager for a
/// user): `spandeck link add` sends the release through the ssh session
/// and it is installed under that home's `.local/bin`; the far station
/// runs; the near config has exactly one `[[link]]` more and is otherwise
/// byte for byte what it was; the station that was already running on the
/// near side takes the link up without a restart, and the far deck answers
/// through the port on this machine; the address printed signs a browser
/// in. The first ssh is the owner's to answer and forwards nothing. The
/// proxy of the far login shell is copied to `[pane_env]` only on a yes,
/// a pane there then has it, and its value is in the far config file and
/// nowhere else: no log, no answer of the API, nothing printed. Run
/// again, nothing is sent, installed or replaced, and the crontab has its
/// one line still. `link remove` gives the near config back as it was and
/// leaves the far side alone; with `--uninstall` it is taken away there.
#[test]
#[ignore = "merge tier: two stations, a stand-in for ssh and a release host; run with --ignored"]
fn link_add_sends_the_release_and_sets_up_both_sides_and_remove_undoes_it() {
    let releases = Releases::start(true);
    let far = Far::new(true);
    let near = Station::start_with(
        "near",
        &format!("[update]\nurl = \"{}\"\nin_place = false\n", releases.url()),
        &[],
    );
    let ssh = far.ssh();
    let before = config_of(&near);
    let far_config_before = config_of(&far.station);
    // A Mac as the far side has its launchd agent, which from an ssh
    // session only loads at the next login; a Linux one with no service
    // manager for a user has its crontab.
    let linux = cfg!(target_os = "linux");
    // A station with no link and no `[pane_env]` is as it always was.
    assert_eq!(near.call("link.list", json!({})), json!([]));
    assert!(!near.root.join("state/links").exists());
    let doctor = near.call("station.doctor", json!({})).to_string();
    assert!(!doctor.contains("pane environment"), "{doctor}");

    // ---------------------------------------------------- the first time
    let (ok, said) = link(
        &near,
        &["add", "--name", "devbox", &ssh, "me@devbox"],
        "n\n",
    );
    assert!(ok, "{said}");
    for piece in [
        "If ssh asks for a password",
        "devbox is ",
        "and has no spandeck",
        &format!("build of {TAG} to devbox over ssh"),
        "checked against its published SHA-256",
        "bytes, and their SHA-256 matches.",
        &format!("Installed spandeck {TAG} to "),
        "devbox has a config file: it was left as it is.",
        "Your login shell on devbox sets HTTPS_PROXY, NO_PROXY.",
        "Not copied.",
        if linux {
            "Start at login on devbox: the line in your crontab there"
        } else {
            "Start at login on devbox: a launchd agent, from your next login"
        },
        "is running on devbox.",
        "Added the link devbox to ",
        "has the link up, without a restart",
        "http://localhost:",
        "`spandeck link remove devbox --uninstall`",
    ] {
        has(&said, piece);
    }
    assert!(!said.contains(SECRET), "{said}");
    // Installed under the far home, and nowhere else; from the bytes the
    // near station downloaded and checked, once.
    assert!(far.bin().is_file());
    assert!(far.station.root.join("home/.local/bin/sd").is_file());
    assert_eq!(releases.asked(&release_file()), 1);
    // The far station runs, and its config file is as it was: the answer
    // to the proxy question was No.
    let info = far.station.call("station.info", json!({}));
    assert_eq!(info["station"], json!("far"), "{info}");
    assert_eq!(config_of(&far.station), far_config_before);
    // Start at login: one line in the crontab, which names the program.
    let cron = far.crontab();
    if linux {
        assert_eq!(cron.lines().count(), 1, "{cron}");
        has(&cron, "@reboot ");
        has(&cron, &format!("\"{}\" daemon", far.bin().display()));
    }
    // The near config: what it was, and exactly one `[[link]]` after it.
    let with = config_of(&near);
    assert!(with.starts_with(&before), "{with}");
    assert_eq!(
        &with[before.len()..],
        format!("\n[[link]]\nname = \"devbox\"\ncmd = [\"{ssh}\", \"me@devbox\"]\n")
    );
    // The station that was running took it up: the same process.
    let row = near.call("link.list", json!({}))[0].clone();
    assert_eq!(
        (&row["name"], &row["state"], &row["station"]),
        (&json!("devbox"), &json!("up"), &json!("far")),
        "{row}"
    );
    assert_eq!(
        near.call("station.info", json!({}))["pid"],
        json!(near.pid())
    );
    let port = row["port"].as_u64().unwrap();
    // The far deck answers through that port, and the printed address
    // signs a browser in to the far station.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (status, page) = rt.block_on(page_through(port));
    assert_eq!(status, 200);
    has(&page, "<!doctype html");
    let url = said
        .lines()
        .find(|l| l.starts_with("http://localhost:"))
        .expect(&said);
    let (at, sign) = url.trim().split_once("/#signin=").expect(url);
    assert_eq!(at, format!("http://localhost:{port}"));
    let (id, collect) = sign.split_once('.').unwrap();
    let token = rt.block_on(async {
        reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/api/device/collect"))
            .json(&json!({"id": id, "collect": collect}))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["result"]["token"]
            .as_str()
            .map(String::from)
    });
    assert!(token.is_some_and(|t| t.len() > 20));
    // What ssh was run with: the first time for the owner to answer
    // (no BatchMode, the host's key theirs to accept), then as a link's
    // carrier; nothing forwarded either time, and the owner's own
    // configuration both times.
    let ssh_log = std::fs::read_to_string(far.at("ssh.log")).unwrap();
    let runs: Vec<&str> = ssh_log.lines().collect();
    assert!(runs.len() >= 2, "{ssh_log}");
    assert!(runs[0].ends_with(" me@devbox sh -s"), "{}", runs[0]);
    assert!(!runs[0].contains("BatchMode") && !runs[0].contains("StrictHostKeyChecking"));
    assert!(runs[1].contains("BatchMode=yes") && runs[1].contains("link --stdio"));
    for run in &runs {
        has(
            run,
            "-o ClearAllForwardings=yes -o ForwardAgent=no -o ForwardX11=no",
        );
        assert!(!run.contains("-F ") && !run.contains("KnownHosts"), "{run}");
    }
    // Both sides say it in the ship's log.
    let sent = near.log("link.release");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(
        (&sent[0]["data"]["release"], &sent[0]["data"]["from"]),
        (&json!(TAG), &json!("published"))
    );
    assert_eq!(
        near.log("link.reload")[0]["data"]["added"],
        json!(["devbox"])
    );
    // `spandeck doctor` on the far side says how it starts.
    let doctor = far.station.call("station.doctor", json!({})).to_string();
    if linux {
        has(&doctor, "from a line in your crontab");
    }

    // -------------------------------------- again: nothing is replaced
    let installed = std::fs::metadata(far.bin()).unwrap().modified().unwrap();
    let far_pid = info["pid"].clone();
    let (ok, said) = link(&near, &["add", "--name", "devbox", "--pane-env"], "");
    assert!(ok, "{said}");
    for piece in [
        "nothing is installed or replaced",
        "The link devbox is in ",
        "Copy them to `[pane_env]` in devbox's config, for every pane there?",
        "Yes (as you said).",
        "Copied to `[pane_env]` on devbox.",
        "(the same entry as before)",
    ] {
        has(&said, piece);
    }
    assert!(
        !said.contains("Sending") && !said.contains(SECRET),
        "{said}"
    );
    assert_eq!(releases.asked(&release_file()), 1);
    assert_eq!(
        std::fs::metadata(far.bin()).unwrap().modified().unwrap(),
        installed
    );
    assert_eq!(far.station.call("station.info", json!({}))["pid"], far_pid);
    assert_eq!(far.crontab(), cron);
    assert_eq!(config_of(&near), with);
    assert_eq!(
        near.call("link.list", json!({})).as_array().unwrap().len(),
        1
    );
    // `[pane_env]`: added to the far config, which is otherwise as it
    // was and is now this user's alone to read.
    let far_config = config_of(&far.station);
    assert!(far_config.starts_with(&far_config_before));
    assert_eq!(
        &far_config[far_config_before.len()..],
        format!("\n[pane_env]\nHTTPS_PROXY = \"http://user:{SECRET}@proxy.corp:3128\"\nNO_PROXY = \"localhost,.corp\"\n")
    );
    let mode = std::fs::metadata(far.station.root.join("config/config.toml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "{mode:o}");
    // A pane started there now has it, with no restart of the station.
    let pane = far.station.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "cwd": far.station.root.join("home"), "args": ["-c",
            "case \"$HTTPS_PROXY\" in http://user:*@proxy.corp:3128) echo proxy-is-${NO_PROXY} ;; *) echo proxy-is-not-set ;; esac; sleep 60"]}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    far.station.wait_text(&pane, "proxy-is-localhost,.corp");
    // Asked a third time, there is nothing left to copy and nothing is asked.
    let (ok, said) = link(&near, &["add", "--name", "devbox"], "");
    assert!(ok, "{said}");
    assert!(!said.contains("Copy them"), "{said}");
    assert_eq!(config_of(&far.station), far_config);
    // The value is in that one file. It is in no answer of the API...
    let doctor = far.station.call("station.doctor", json!({})).to_string();
    has(&doctor, "HTTPS_PROXY, NO_PROXY (values are not shown)");
    for (method, params) in [
        ("station.info", json!({})),
        ("station.doctor", json!({})),
        ("pane.list", json!({})),
        ("pane.get", json!({"pane": pane})),
        ("log.query", json!({})),
        ("link.list", json!({})),
    ] {
        for s in [&far.station, &near] {
            let Ok(v) = s.try_call(method, params.clone()) else {
                continue;
            };
            assert!(!v.to_string().contains(SECRET), "{method}: {v}");
        }
    }
    // ...and in no file of either station but that one: no log, no
    // database, nothing kept for the link, nothing ssh was told.
    let mut found = vec![];
    files_holding(&far.station.root, SECRET, &mut found);
    files_holding(&near.root, SECRET, &mut found);
    assert_eq!(found, [far.station.root.join("config/config.toml")]);
    assert!(!std::fs::read_to_string(far.at("ssh.log"))
        .unwrap()
        .contains(SECRET));

    // --------------------------------------------------------- remove
    let (ok, said) = link(&near, &["remove", "devbox"], "");
    assert!(ok, "{said}");
    has(&said, "Nothing on devbox was touched");
    has(&said, "`spandeck link remove devbox --uninstall`");
    assert_eq!(config_of(&near), before);
    assert_eq!(near.call("link.list", json!({})), json!([]));
    assert!(!near.root.join("state/links/devbox.json").exists());
    wait_for("the port on this machine to be let go of", 10, || {
        std::net::TcpStream::connect(("127.0.0.1", port as u16))
            .is_err()
            .then_some(())
    });
    assert_eq!(far.station.call("station.info", json!({}))["pid"], far_pid);
    assert!(far.bin().is_file());
    // Added again (nothing is installed), and taken away there as well:
    // the station stopped, the program and the crontab's line gone, the
    // settings and the state kept.
    let (ok, said) = link(&near, &["add", "--no-pane-env", &ssh, "me@devbox"], "");
    assert!(ok, "{said}");
    has(&said, "nothing is installed or replaced");
    assert_eq!(releases.asked(&release_file()), 1);
    let (ok, said) = link(&near, &["remove", "devbox", "--uninstall"], "");
    assert!(ok, "{said}");
    has(&said, "spandeck on devbox was stopped and uninstalled.");
    assert!(far.station.try_call("station.info", json!({})).is_err());
    assert!(!far.bin().exists());
    assert_eq!(far.crontab().trim(), "");
    assert_eq!(config_of(&far.station), far_config);
    assert!(far.station.root.join("state/spandeck.db").exists());
    assert_eq!(config_of(&near), before);
    drop(rt);
}

/// Done when a spandeck that is on the far machine already is never
/// replaced without a yes. One older than the near side that knows links:
/// the owner is asked, the answer is No unless they say yes, the file is
/// then byte for byte what it was, no release is fetched or sent, and the
/// link is added to what is there. One from before links, which a link
/// cannot reach: asked, and on a No nothing is changed on either machine,
/// the near config included. And a far side that needs a release, at a
/// near machine whose station is not running: said, and nothing changed.
#[test]
#[ignore = "merge tier: a stand-in for ssh; run with --ignored"]
fn a_spandeck_that_is_there_is_not_replaced_without_a_yes() {
    let far = Far::new(false);
    // The near machine's station is not running: nothing here needs it.
    let near = Station::not_started("near", "[update]\nurl = \"http://127.0.0.1:9\"\n");
    let ssh = far.ssh();
    let before = config_of(&near);

    // No spandeck there, and no station here to get a release from.
    let (ok, said) = link(&near, &["add", &ssh, "me@devbox"], "");
    assert!(!ok, "{said}");
    has(
        &said,
        "The station on this machine is not running, and it is what gets the release to send",
    );
    assert!(!far.bin().exists());
    assert_eq!(config_of(&near), before);

    // One from before links: No is the answer when none is given.
    let old = far.old_spandeck("0.4.18", false);
    let (ok, said) = link(&near, &["add", &ssh, "me@devbox"], "");
    assert!(!ok, "{said}");
    has(&said, "has spandeck 0.4.18, from before links");
    has(&said, "Replace it with the newest release?");
    has(&said, "(no answer: No)");
    has(&said, "nothing was changed on either machine");
    assert_eq!(std::fs::read_to_string(far.bin()).unwrap(), old);
    assert_eq!(config_of(&near), before);

    // An older one that knows links: asked, No, and left alone; the link
    // is added to it. Nothing could be set up to start it at login there
    // (no service manager, a crontab this user may not use), and that is
    // said as it is.
    let old = far.old_spandeck("0.4.1", true);
    let (ok, said) = link(&near, &["add", &ssh, "me@devbox"], "n\n");
    assert!(ok, "{said}");
    for piece in [
        "has spandeck 0.4.1, older than this machine's",
        "Replace it with the newest release?",
        "Left as it is: spandeck 0.4.1 stays on devbox.",
        "The spandeck on devbox is from before `[pane_env]`",
        "Start at login on devbox: none could be set up without an administrator",
        "after a reboot it is down until this machine connects",
        "Added the link devbox to ",
        "The station on this machine is not running, so the link is not open yet.",
        "~/.local/bin/spandeck and sd, or wherever it was already",
    ] {
        has(&said, piece);
    }
    assert!(!said.contains("Sending"), "{said}");
    assert_eq!(std::fs::read_to_string(far.bin()).unwrap(), old);
    assert!(!far.at("crontab.txt").exists());
    let with = config_of(&near);
    assert!(
        with.starts_with(&before) && with.matches("[[link]]").count() == 1,
        "{with}"
    );
    // The same name with another destination is refused, and so is a
    // command for the far side; neither runs ssh.
    let runs = std::fs::read_to_string(far.at("ssh.log"))
        .unwrap()
        .lines()
        .count();
    let (ok, said) = link(&near, &["add", "--name", "devbox", &ssh, "me@other"], "");
    assert!(
        !ok && said.contains("there is a link named devbox already"),
        "{said}"
    );
    let (ok, said) = link(&near, &["add", &ssh, "me@box2", "rm", "-rf", "x"], "");
    assert!(
        !ok && said.contains("comes after the destination"),
        "{said}"
    );
    assert_eq!(
        std::fs::read_to_string(far.at("ssh.log"))
            .unwrap()
            .lines()
            .count(),
        runs
    );
    assert_eq!(config_of(&near), with);
    std::thread::sleep(Duration::from_millis(10));
}
