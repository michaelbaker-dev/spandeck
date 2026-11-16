//! Windows acceptance (DECISIONS.md, "Windows"): a station runs natively,
//! the CLI reaches it over the named pipe, panes are pseudoconsoles held by
//! the pane host, and a daemon that is ended takes no pane with it.
//!
//! These run only on Windows. The scenario tests in the other files run
//! there too where they can; one that cannot carries its reason.
#![cfg(windows)]

mod common;

use common::*;
use serde_json::json;

fn spawn(s: &Station, params: serde_json::Value) -> String {
    s.call("pane.spawn", params)["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The daemon starts, the client reaches it through the pipe, a shell pane
/// shows its output, and kill ends it.
#[test]
fn a_station_runs_a_shell_pane_and_kill_ends_it() {
    let s = Station::start("win");
    let info = s.call("station.info", json!({}));
    assert_eq!(info["os"], json!("windows"));
    assert_eq!(info["live_handoff"], json!(true));
    assert!(s
        .paths()
        .socket()
        .to_string_lossy()
        .starts_with(r"\\.\pipe\spandeck-"));

    let pane = spawn(
        &s,
        json!({"program": "cmd.exe", "args": ["/d", "/c", "echo conpty-and-pipe-ok& ping -n 120 127.0.0.1 >nul"], "cwd": s.root.join("home")}),
    );
    s.wait_text(&pane, "conpty-and-pipe-ok");
    assert_eq!(s.pane(&pane)["exited"], json!(false));
    s.call("pane.kill", json!({"pane": pane}));
    wait_for("the pane to end", 15, || {
        (s.pane(&pane)["exited"] == json!(true)).then_some(())
    });
}

/// A plain pane is the default shell (PowerShell), and what is typed reaches it.
#[test]
fn the_default_shell_takes_typed_input() {
    let s = Station::start("win");
    let pane = spawn(&s, json!({"cwd": s.root.join("home")}));
    s.call(
        "pane.send_text",
        json!({"pane": pane, "text": "echo typed-$(20+22)"}),
    );
    s.wait_text(&pane, "typed-42");
}

/// An agent's hooks reach the daemon from inside a pane (the pane's token,
/// the pipe's name in its environment), and typed input reaches the agent.
#[test]
fn a_fake_agent_reports_through_the_pipe_and_reads_input() {
    let s = Station::start("win");
    let pane = s.fake(json!({"steps": [{"say": "agent-is-up"}]}), json!({}));
    s.wait_text(&pane, "agent-is-up");
    wait_for("a session id from the SessionStart hook", 15, || {
        s.pane(&pane)["session_id"].as_str().map(String::from)
    });
    s.call("pane.send_text", json!({"pane": pane, "text": "hello"}));
    s.wait_text(&pane, "[fake] got: hello");
}

/// The pane host: panes are not in the daemon's process, so a daemon that
/// is ended without warning takes none of them with it. The next daemon
/// finds each pane in the host with its screen, and it is still the same
/// program at the other end. (Upgrade, restart and going back are the same
/// tests as on Unix, in phase1.rs.)
#[test]
fn after_a_daemon_crash_every_pane_is_still_there_with_its_screen() {
    let mut s = Station::start("win");
    let agent = s.fake(
        json!({"steps": [{"say": "before-the-crash marker"}]}),
        json!({}),
    );
    let shell = spawn(
        &s,
        json!({"program": "cmd.exe", "args": ["/d", "/k", "echo shell-was-here"], "cwd": s.root.join("home")}),
    );
    s.wait_text(&agent, "before-the-crash marker");
    s.wait_text(&shell, "shell-was-here");
    let before = s.call("station.info", json!({}));
    assert!(before["pane_host"]["pid"].as_u64().is_some());
    let (agent_pid, shell_pid) = (s.pane(&agent)["pid"].clone(), s.pane(&shell)["pid"].clone());

    s.kill9();
    s.launch();
    let after = s.call("station.info", json!({}));
    assert_ne!(after["pid"], before["pid"]);
    assert_eq!(
        after["pane_host"]["pid"], before["pane_host"]["pid"],
        "the same pane host"
    );
    for (pane, pid, said) in [
        (&agent, &agent_pid, "before-the-crash marker"),
        (&shell, &shell_pid, "shell-was-here"),
    ] {
        let p = s.pane(pane);
        assert_eq!(p["exited"], json!(false));
        assert_eq!(&p["pid"], pid, "the program was never restarted");
        s.wait_text(pane, said);
    }
    s.call(
        "pane.send_text",
        json!({"pane": agent, "text": "still here"}),
    );
    s.wait_text(&agent, "[fake] got: still here");
    assert_eq!(s.log("pane.kept").len(), 2);
    assert!(
        s.log("pane.resumed").is_empty(),
        "nothing had to be resumed"
    );
}

// ------------------------------------------------- the station's supervisor
//
// The fault (rtx6000, 15 Nov 2026): after its first upgrade the daemon was a
// process `spandeckd.exe` had not started; ended with `taskkill /F`, nothing
// started it again in 150 seconds, with its agents running out of reach.
// These start a station as the log-on task does, `spandeckd.exe` run
// directly, under folders of their own.

/// What the supervisor wrote about what it did.
fn supervisor_log(s: &Station) -> String {
    std::fs::read_to_string(s.root.join("state/spandeckd-supervisor.log")).unwrap_or_default()
}

/// When a test fails, what the supervisor wrote: it has no console, and a
/// failure on a machine nobody can log in to says nothing else.
struct SaysOnFailure(std::path::PathBuf);

impl Drop for SaysOnFailure {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "---- the supervisor's log ----\n{}",
                std::fs::read_to_string(&self.0).unwrap_or_default()
            );
        }
    }
}

/// Upgrade in place and wait for the daemon that took over. Its pid.
fn upgrade(s: &Station) -> u32 {
    let before = s.daemon_pid();
    s.call("station.upgrade", json!({"binary": SPANDECK}));
    wait_for("the new daemon to answer", 20, || {
        let i = s.try_call("station.info", json!({})).ok()?;
        let pid = i["pid"].as_u64()? as u32;
        (pid != before && i["web"].as_array().is_some_and(|w| !w.is_empty())).then_some(pid)
    })
}

fn still_supervising(s: &mut Station) -> bool {
    matches!(s.supervisor.as_mut().unwrap().try_wait(), Ok(None))
}

/// The fault itself: a daemon that is ended after an upgrade is started
/// again by the supervisor, which did not start the one that ended, and the
/// panes are the same programs under the same pane host.
#[test]
fn a_daemon_ended_after_an_upgrade_comes_back_by_itself_with_its_panes() {
    // The pane host waits this long for a daemon before it ends its panes
    // (ten minutes outside tests; the harness's five seconds is too near
    // the supervisor's first wait to tell the two apart).
    let mut s = Station::start_supervised("win", &[("SPANDECK_HOST_LINGER", "60")]);
    let agent = s.fake(
        json!({"steps": [{"say": "before-it-ended marker"}]}),
        json!({}),
    );
    let shell = spawn(
        &s,
        json!({"program": "cmd.exe", "args": ["/d", "/k", "echo shell-was-here"], "cwd": s.root.join("home")}),
    );
    s.wait_text(&agent, "before-it-ended marker");
    s.wait_text(&shell, "shell-was-here");
    let first = s.daemon_pid();
    let host = s.call("station.info", json!({}))["pane_host"]["pid"].clone();
    assert!(host.as_u64().is_some());
    let (agent_pid, shell_pid) = (s.pane(&agent)["pid"].clone(), s.pane(&shell)["pid"].clone());

    // Twice, as the real station had been: the second daemon was started by
    // the first, the third by the second.
    let second = upgrade(&s);
    let third = upgrade(&s);
    assert!(still_supervising(&mut s), "{}", supervisor_log(&s));

    Station::end_process(third);
    let _told = SaysOnFailure(s.root.join("state/spandeckd-supervisor.log"));
    let fourth = wait_for("a daemon to be started again", 30, || {
        let i = s.try_call("station.info", json!({})).ok()?;
        let pid = i["pid"].as_u64()? as u32;
        (![first, second, third].contains(&pid)
            && i["web"].as_array().is_some_and(|w| !w.is_empty()))
        .then_some(pid)
    });
    let after = s.call("station.info", json!({}));
    assert_eq!(after["pane_host"]["pid"], host, "the same pane host");
    for (pane, pid, said) in [
        (&agent, &agent_pid, "before-it-ended marker"),
        (&shell, &shell_pid, "shell-was-here"),
    ] {
        let p = s.pane(pane);
        assert_eq!(p["exited"], json!(false));
        assert_eq!(&p["pid"], pid, "the program was never restarted");
        s.wait_text(pane, said);
    }
    s.call("pane.send_text", json!({"pane": agent, "text": "back"}));
    s.wait_text(&agent, "[fake] got: back");

    // And again: it follows the daemon it started as it followed the others.
    Station::end_process(fourth);
    wait_for("a daemon to be started a second time", 30, || {
        let pid = s.try_call("station.info", json!({})).ok()?["pid"].as_u64()? as u32;
        (pid != fourth).then_some(())
    });
    assert!(still_supervising(&mut s), "{}", supervisor_log(&s));
}

/// A station its owner stopped stays stopped: the supervisor ends with it
/// and starts nothing. The daemon stopped here is one the supervisor did
/// not start (an upgrade came first), as on a real station.
#[test]
fn a_station_stopped_on_purpose_is_not_started_again() {
    let mut s = Station::start_supervised("win", &[]);
    let pane = spawn(
        &s,
        json!({"program": "cmd.exe", "args": ["/d", "/k", "echo a-pane"], "cwd": s.root.join("home")}),
    );
    s.wait_text(&pane, "a-pane");
    let daemon = upgrade(&s);

    let out = s
        .command(SPANDECK)
        .args(["service", "--stop"])
        .stdout(std::process::Stdio::piped())
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{said}");
    assert!(said.contains("was stopped"), "{said}");
    assert!(s.try_call("station.info", json!({})).is_err());

    let ended = wait_for("the supervisor to end", 15, || {
        s.supervisor.as_mut().unwrap().try_wait().ok().flatten()
    });
    assert!(ended.success(), "{}", supervisor_log(&s));
    // Longer than its first wait before a start, were it to make one.
    std::thread::sleep(std::time::Duration::from_secs(4));
    assert!(
        s.try_call("station.info", json!({})).is_err(),
        "nothing answers: {}",
        supervisor_log(&s)
    );
    assert!(!spandeck_terminal::sys::pid_alive(daemon as i32));
    let log = supervisor_log(&s);
    assert_eq!(log.matches("started a daemon").count(), 1, "{log}");
    assert!(log.contains("stopped on purpose"), "{log}");
    // The stop was the clean one: the next start says so, and holds
    // nothing back.
    s.launch();
    let started = s.log("station.started");
    assert_eq!(started[0]["data"]["clean"], json!(true), "{started:?}");
}

/// An upgrade hands over from one process to the next; the supervisor
/// follows and starts nothing, whether the new daemon stays or the station
/// goes back to the one before.
#[test]
fn an_upgrade_under_the_supervisor_leaves_one_daemon() {
    for unhealthy in [false, true] {
        let env: &[(&str, &str)] = if unhealthy {
            // The new daemon reports itself unhealthy and the station goes
            // back: three processes, one after another.
            &[("SPANDECK_TEST_UNHEALTHY", "1")]
        } else {
            &[]
        };
        let mut s = Station::start_supervised("win", env);
        let first = s.daemon_pid();
        let now = upgrade(&s);
        if unhealthy {
            wait_for("the station to say it went back", 20, || {
                s.try_call("log.query", json!({"kind": "station.rolled_back"}))
                    .ok()
                    .and_then(|l| l.get(0).cloned())
            });
        }
        // Past the time a daemon started by mistake would take to appear.
        std::thread::sleep(std::time::Duration::from_secs(3));
        let running = s.daemon_pid();
        assert_ne!(running, first);
        if !unhealthy {
            assert_eq!(running, now);
        }
        let named: u32 = std::fs::read_to_string(s.paths().pid_file())
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(named, running, "the pid file names the daemon that answers");
        assert!(still_supervising(&mut s));
        let log = supervisor_log(&s);
        assert_eq!(log.matches("started a daemon").count(), 1, "{log}");
        assert!(
            log.contains(&format!(
                "following the daemon that is running (pid {running})"
            )),
            "{log}"
        );
        // One start that was not an upgrade: the first.
        assert_eq!(s.log("station.started").len(), 1);
    }
}

/// One supervisor per station: a second one (a second log on, the task run
/// by hand) ends at once and starts nothing.
#[test]
fn a_second_supervisor_ends_and_starts_nothing() {
    let mut s = Station::start_supervised("win", &[]);
    let daemon = s.daemon_pid();

    let second = s.command(SPANDECKD).output().unwrap();
    assert!(second.status.success());
    assert!(still_supervising(&mut s));
    assert_eq!(s.daemon_pid(), daemon);
    let log = supervisor_log(&s);
    assert_eq!(log.matches("supervising").count(), 1, "{log}");
    assert_eq!(log.matches("started a daemon").count(), 1, "{log}");
}

/// Ends the supervisors a station's daemon started itself, which are not
/// this test's children: by the pids they wrote in their log, and only a
/// process that is still `spandeckd.exe`. Declared after its station, so it
/// goes first and nothing starts the daemon again once the station ends it.
struct EndSupervisors(std::path::PathBuf);

impl Drop for EndSupervisors {
    fn drop(&mut self) {
        let log = std::fs::read_to_string(&self.0).unwrap_or_default();
        for line in log.lines() {
            let pid = line
                .split("supervising (pid ")
                .nth(1)
                .and_then(|rest| rest.split(',').next());
            if let Some(pid) = pid {
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", pid, "/F", "/FI", "IMAGENAME eq spandeckd.exe"])
                    .output();
            }
        }
    }
}

/// The supervisor now runs for as long as the station does, so an update
/// finds its file in use, and here a copy from the update before is still
/// the one running. The update replaces the file all the same; the daemon
/// it brings starts the new supervisor, the old one leaves without the
/// daemon being touched, and the new one does the supervising. (A station
/// installed before 15 Nov 2026 gets its supervisor the same way.)
#[test]
fn an_update_replaces_the_running_supervisor_and_the_new_one_takes_over() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    // The installed programs, and a release of the same ones.
    let bin = dir.path().join("bin");
    let name = "spandeck-v9.9.9-x86_64-pc-windows-msvc";
    let stage = dir.path().join(name);
    for folder in [&bin, &stage] {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::copy(SPANDECK, folder.join("spandeck.exe")).unwrap();
        std::fs::copy(SPANDECKD, folder.join("spandeckd.exe")).unwrap();
    }
    let installed = bin.join("spandeckd.exe");
    let zip = dir.path().join("release.zip");
    let made = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command"])
        .arg(format!(
            "Compress-Archive -Path '{}' -DestinationPath '{}'",
            stage.display(),
            zip.display()
        ))
        .status()
        .unwrap();
    assert!(made.success());
    let bytes = std::fs::read(&zip).unwrap();
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let port = release_server(bytes, format!("{hash}  {name}.zip\n"));

    let mut s = Station::start_supervised_by(
        "win",
        &format!("[update]\nurl = \"http://127.0.0.1:{port}\"\n"),
        &[],
        &installed,
    );
    let _end = EndSupervisors(s.root.join("state/spandeckd-supervisor.log"));
    let daemon = s.daemon_pid();

    // An update before this one: the copy that runs was moved aside then.
    let aside = bin.join("spandeckd.exe.old");
    let new = bin.join("spandeckd.new");
    std::fs::copy(SPANDECKD, &new).unwrap();
    spandeck_terminal::sys::replace_file(&new, &installed).unwrap();
    assert!(aside.is_file() && installed.is_file());

    let out = s.call("station.update", json!({}));
    assert_eq!(out["release"], json!("v9.9.9"), "{out}");
    let updated = wait_for("the updated daemon to answer", 30, || {
        let i = s.try_call("station.info", json!({})).ok()?;
        let pid = i["pid"].as_u64()? as u32;
        (pid != daemon && i["web"].as_array().is_some_and(|w| !w.is_empty())).then_some(pid)
    });
    let left = wait_for("the first supervisor to leave", 20, || {
        s.supervisor.as_mut().unwrap().try_wait().ok().flatten()
    });
    assert!(left.success());
    wait_for("the new one to say it supervises", 15, || {
        supervisor_log(&s).contains("took over").then_some(())
    });
    assert_eq!(s.daemon_pid(), updated, "the daemon was not touched");
    assert!(installed.is_file());
    wait_for("the old copy's file to be free", 10, || {
        std::fs::remove_file(&aside).ok()
    });

    Station::end_process(updated);
    wait_for("the one that took over to start a daemon", 30, || {
        let pid = s.try_call("station.info", json!({})).ok()?["pid"].as_u64()? as u32;
        (pid != updated).then_some(())
    });
    let log = supervisor_log(&s);
    assert_eq!(log.matches("started a daemon").count(), 2, "{log}");
}

/// Private files: the state folder is for this user, SYSTEM and
/// Administrators only, with nothing inherited from the folder above.
#[test]
fn the_state_folder_is_kept_to_this_user() {
    let s = Station::start("win");
    let out = std::process::Command::new("icacls")
        .arg(s.root.join("state"))
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&out.stdout).to_string();
    for other in ["Everyone", r"BUILTIN\Users", "Authenticated Users"] {
        assert!(!listing.contains(other), "{other} can reach it:\n{listing}");
    }
    assert!(
        !listing.contains("(I)"),
        "nothing is inherited from the folder above:\n{listing}"
    );
}

/// A stand-in for the release page: `/latest` redirects to the tag, and the
/// zip and its checksum are served from memory.
fn release_server(zip: Vec<u8>, checksum: String) -> u16 {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (zip, checksum) = (zip.clone(), checksum.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                let _ = reader.read_line(&mut first);
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let mut parts = first.split_whitespace();
                let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                let (status, extra, body): (&str, String, Vec<u8>) = if path == "/latest" {
                    ("302 Found", "Location: /tag/v9.9.9\r\n".into(), vec![])
                } else if path == "/tag/v9.9.9" {
                    ("200 OK", String::new(), vec![])
                } else if path.ends_with(".zip") {
                    ("200 OK", String::new(), zip)
                } else if path.ends_with(".zip.sha256") {
                    ("200 OK", String::new(), checksum.into_bytes())
                } else {
                    ("404 Not Found", String::new(), vec![])
                };
                let mut stream = stream;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\n{extra}Content-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                if method != "HEAD" {
                    let _ = stream.write_all(&body);
                }
            });
        }
    });
    port
}

/// install.ps1, in Windows PowerShell, against a local release: it finds the
/// latest version, checks the download against its checksum, and puts the
/// programs in the folder named. It is told not to touch the PATH or set up
/// start at log on, so nothing of the machine's own is changed. A release
/// whose checksum does not match installs nothing.
#[test]
fn install_ps1_installs_a_release_that_matches_its_checksum_and_no_other() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let name = "spandeck-v9.9.9-x86_64-pc-windows-msvc";
    let stage = dir.path().join(name);
    std::fs::create_dir_all(&stage).unwrap();
    for exe in ["spandeck.exe", "sd.exe"] {
        std::fs::copy(SPANDECK, stage.join(exe)).unwrap();
    }
    let zip = dir.path().join("release.zip");
    let made = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command"])
        .arg(format!(
            "Compress-Archive -Path '{}' -DestinationPath '{}'",
            stage.display(),
            zip.display()
        ))
        .status()
        .unwrap();
    assert!(made.success());
    let bytes = std::fs::read(&zip).unwrap();
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.ps1");
    let install = |port: u16, to: &std::path::Path| {
        std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .env("SPANDECK_RELEASES", format!("http://127.0.0.1:{port}"))
            .env("SPANDECK_BIN_DIR", to)
            .env("SPANDECK_NO_SERVICE", "1")
            .env("SPANDECK_NO_PATH", "1")
            .env_remove("SPANDECK_VERSION")
            .output()
            .unwrap()
    };

    let good = release_server(bytes.clone(), format!("{hash}  {name}.zip\n"));
    let to = dir.path().join("bin");
    for round in ["a first install", "over a copy already there"] {
        let out = install(good, &to);
        let said = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{round}: {said}");
        assert!(said.contains("checksum verified"), "{round}: {said}");
        let ran = std::process::Command::new(to.join("spandeck.exe"))
            .arg("handoff-check")
            .output()
            .unwrap();
        assert!(ran.status.success(), "{round}: the installed program runs");
        assert!(to.join("sd.exe").is_file());
    }

    let bad = release_server(bytes, format!("{}  {name}.zip\n", "0".repeat(64)));
    let nowhere = dir.path().join("bin-bad");
    let out = install(bad, &nowhere);
    assert!(!out.status.success());
    assert!(!nowhere.join("spandeck.exe").exists());
}
