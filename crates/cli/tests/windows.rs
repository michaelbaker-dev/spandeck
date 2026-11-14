//! Windows acceptance (DECISIONS.md, "Windows"): a station runs natively,
//! the CLI reaches it over the named pipe, panes are pseudoconsoles, and an
//! upgrade restarts the panes and resumes agents by session id.
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
    assert_eq!(info["live_handoff"], json!(false));
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

/// The Windows form of the upgrade Guarantee: there is no live handoff, so a
/// restart starts a new daemon process, the old panes end, an agent goes
/// back to its conversation and a shell is not brought back.
#[test]
fn a_restart_starts_a_new_daemon_and_resumes_agents_by_session_id() {
    let s = Station::start("win");
    let agent = s.fake(json!({"steps": []}), json!({}));
    let shell = spawn(
        &s,
        json!({"program": "cmd.exe", "cwd": s.root.join("home")}),
    );
    let session = wait_for("session id", 15, || {
        s.pane(&agent)["session_id"].as_str().map(String::from)
    });
    let before = s.call("station.info", json!({}));
    let reply = s.call("station.restart", json!({}));
    assert_eq!(reply["live_handoff"], json!(false));
    let after = wait_for("the new daemon to answer", 40, || {
        let i = s.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(i)
    });
    assert_ne!(
        after["pid"], before["pid"],
        "a new process, not the old one"
    );
    s.wait_text(&agent, &format!("[fake] RESUMED {session}"));
    assert!(
        s.try_call("pane.get", json!({"pane": shell})).is_err(),
        "a plain shell is not brought back"
    );
    assert_eq!(
        s.log("station.upgraded")[0]["data"]["live"],
        json!(false),
        "the ship's log says the terminals were not kept"
    );
    // The daemon the harness started is gone; end the one that replaced it.
    if let Some(pid) = after["pid"].as_u64() {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
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
