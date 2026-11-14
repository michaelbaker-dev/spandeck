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
