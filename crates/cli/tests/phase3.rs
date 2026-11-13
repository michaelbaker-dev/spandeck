//! Phase 3 acceptance: agent hooks, states, approvals, stuck detection,
//! pause / stop / kill, and the session archive.
mod common;
use common::*;
use serde_json::json;

#[test]
fn hooks_drive_the_pane_state_and_capture_the_session() {
    let s = Station::start("alpha");
    let pane = s.fake(
        json!({"steps": [
            {"prompt": "add a parser"},
            {"tool": {"name": "Bash", "command": "cargo build"}},
            {"sleep": 300},
            {"turn_end": "parser added"}
        ]}),
        json!({}),
    );
    let p = s.wait_state(&pane, "done");
    assert_eq!(p["hooks_seen"], json!(true));
    assert!(p["session_id"].as_str().unwrap().starts_with("fake-"));
    assert_eq!(p["last_message"], json!("parser added"));
    // The ship's log has the tool call's name and result, not its contents.
    let tools = s.log("agent.tool");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["data"]["tool"], json!("Bash"));
    assert_eq!(tools[0]["data"]["ok"], json!(true));
    assert!(!tools[0]["data"].to_string().contains("cargo build"));
    // working -> done were both recorded.
    let states: Vec<String> = s.log("pane.state").iter().rev().map(|e| e["data"]["state"].as_str().unwrap().to_string()).collect();
    assert_eq!(states, vec!["working", "done"]);
}

/// Done when: a fake agent that loops is flagged as stuck.
#[test]
fn an_agent_repeating_a_failing_command_is_flagged_as_stuck() {
    let s = Station::start("alpha");
    let pane = s.fake(
        json!({"steps": [
            {"prompt": "fix the build"},
            {"repeat": 3, "steps": [{"tool": {"name": "Bash", "command": "make build", "ok": false}}]},
            {"sleep": 60000}
        ]}),
        json!({}),
    );
    let p = s.wait_state(&pane, "needs_you");
    assert!(p["needs_reason"].as_str().unwrap().contains("stuck"), "{p}");
    assert!(p["needs_reason"].as_str().unwrap().contains("make build"));
    assert_eq!(s.log("agent.stuck").len(), 1);
    // It also lands in the inbox.
    let n = s.call("notify.list", json!({}));
    assert!(n.as_array().unwrap().iter().any(|n| n["kind"] == json!("needs_you")));
}

#[test]
fn turns_that_burn_tool_calls_without_changing_files_are_flagged() {
    let s = Station::start_with("alpha", "stuck_idle_turns = 2\n", &[]);
    let busy_turn = json!([
        {"prompt": "investigate"},
        {"repeat": 5, "steps": [{"tool": {"name": "Read", "file": "a.rs"}}]},
        {"turn_end": "still looking"}
    ]);
    let pane = s.fake(json!({"steps": [{"repeat": 2, "steps": busy_turn}, {"sleep": 60000}]}), json!({}));
    let p = s.wait_state(&pane, "needs_you");
    assert!(p["needs_reason"].as_str().unwrap().contains("changed no files"), "{p}");
}

/// Done when: pause, stop and kill each do what Guarantees says.
#[test]
fn stop_interrupts_the_turn_and_the_process_stays_kill_ends_it() {
    let s = Station::start("alpha");
    let pane = s.fake(json!({"steps": [{"prompt": "long job"}, {"work": 60000}, {"turn_end": "finished"}]}), json!({}));
    s.wait_state(&pane, "working");
    let pid = s.pane(&pane)["pid"].clone();

    s.call("pane.stop", json!({"pane": pane}));
    s.wait_text(&pane, "[fake] interrupted");
    let p = s.wait_state(&pane, "done");
    assert_eq!(p["exited"], json!(false), "stop: the process stays");
    assert_eq!(p["pid"], pid);
    // Still usable after a stop.
    s.call("pane.send_text", json!({"pane": pane, "text": "next"}));
    s.wait_text(&pane, "[fake] got: next");

    s.call("pane.kill", json!({"pane": pane}));
    wait_for("the process to end", 10, || (s.pane(&pane)["exited"] == json!(true)).then_some(()));
}

#[test]
fn a_permission_request_waits_for_a_remote_answer() {
    let s = Station::start("alpha");
    let pane = s.fake(
        json!({"steps": [
            {"prompt": "write files"},
            {"permission": {"name": "Write", "file": "/tmp/a.txt"}},
            {"permission": {"name": "Bash", "command": "rm -rf build"}},
            {"turn_end": "done"}
        ]}),
        json!({}),
    );
    // First request: allowed from "the phone".
    let first = wait_for("an approval request", 10, || s.call("approval.list", json!({})).as_array().and_then(|a| a.first().cloned()));
    assert_eq!(first["summary"], json!("Write /tmp/a.txt"));
    assert_eq!(s.pane(&pane)["state"], json!("needs_you"));
    s.call("approval.answer", json!({"id": first["id"], "allow": true}));
    s.wait_text(&pane, "[fake] permission: allow");

    // Second request: denied, with a reason the agent sees.
    let second = wait_for("the second request", 10, || {
        s.call("approval.list", json!({})).as_array().and_then(|a| a.iter().find(|x| x["id"] != first["id"]).cloned())
    });
    assert!(second["summary"].as_str().unwrap().contains("rm -rf build"));
    s.call("approval.answer", json!({"id": second["id"], "allow": false, "reason": "not that directory"}));
    s.wait_text(&pane, "[fake] permission: deny");
    s.wait_state(&pane, "done");
    // An answered request cannot be answered again.
    assert!(s.try_call("approval.answer", json!({"id": first["id"], "allow": false})).is_err());
}

#[test]
fn an_unanswered_permission_request_falls_back_to_the_terminal_prompt() {
    let s = Station::start_with("alpha", "approval_timeout_secs = 1\n", &[]);
    let pane = s.fake(json!({"steps": [{"prompt": "x"}, {"permission": {"name": "Write", "file": "f"}}, {"sleep": 60000}]}), json!({}));
    // No decision is returned, so the agent shows its own prompt.
    s.wait_text(&pane, "[fake] permission: none");
    let all = s.call("approval.list", json!({"all": true}));
    assert_eq!(all[0]["status"], json!("expired"));
}

/// Done when: a session is archived line for line.
#[test]
fn the_session_file_is_archived_line_for_line() {
    let s = Station::start("alpha");
    let pane = s.fake(
        json!({"steps": [
            {"prompt": "first prompt"},
            {"transcript": {"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "name": "Bash", "input": {"command": "ls"}}]}}},
            {"transcript": {"type": "system", "subtype": "compact_boundary", "compactMetadata": {"trigger": "auto"}}},
            {"turn_end": "after the compaction"}
        ]}),
        json!({}),
    );
    s.wait_state(&pane, "done");
    let session = s.pane(&pane)["session_id"].as_str().unwrap().to_string();
    let file = s.root.join("fake").join(format!("{session}.jsonl"));
    let on_disk = std::fs::read_to_string(&file).unwrap().lines().count() as u64;
    assert_eq!(on_disk, 4);
    let archived = wait_for("the archive to catch up", 10, || {
        let sessions = s.call("archive.sessions", json!({}));
        let me = sessions.as_array()?.iter().find(|x| x["id"] == json!(session))?.clone();
        (me["lines"] == json!(on_disk)).then_some(me)
    });
    assert_eq!(archived["pane"], json!(pane));
    // A deleted session file can be put back for a resume.
    std::fs::remove_file(&file).unwrap();
    s.call("archive.restore", json!({"session": session}));
    assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count() as u64, on_disk);
}

/// Observing without hooks: the screen is the fallback signal.
#[test]
fn screen_detection_reports_state_when_no_hook_has_fired() {
    let s = Station::start("alpha");
    let pane = s.call(
        "pane.spawn",
        json!({"agent": "fake", "program": "/bin/sh", "args": ["-c", "echo '[fake] permission?'; sleep 60"], "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let p = s.wait_state(&pane, "needs_you");
    assert_eq!(p["hooks_seen"], json!(false));
}

#[test]
fn an_agent_cannot_use_methods_that_administer_the_station() {
    let s = Station::start("alpha");
    let pane = s.fake(json!({"steps": []}), json!({}));
    s.wait_text(&pane, "[fake] session");
    for (method, params) in [
        ("pane.kill", json!({"pane": pane})),
        ("station.upgrade", json!({"binary": "/bin/true"})),
        ("approval.answer", json!({"id": "a-1", "allow": true})),
        ("integration.install", json!({"agent": "claude"})),
        ("stop.all", json!({})),
        ("pane.spawn", json!({"program": "/bin/sh"})),
    ] {
        let err = s.call_as_pane(&pane, method, params).unwrap_err();
        assert_eq!(err.code, "denied", "{method} should be denied to an agent");
    }
    // It can read, and report on itself.
    assert!(s.call_as_pane(&pane, "pane.list", json!({})).is_ok());
    assert!(s.call_as_pane(&pane, "pane.report_agent", json!({"state": "working"})).is_ok());
}

#[test]
fn integration_install_is_reversible_through_the_api() {
    let s = Station::start("alpha");
    let st = s.call("integration.install", json!({"agent": "claude"}));
    assert_eq!(st["installed"], json!(true));
    let settings = s.root.join("home/.claude/settings.json");
    assert!(std::fs::read_to_string(&settings).unwrap().contains("hook claude"));
    let st = s.call("integration.uninstall", json!({"agent": "claude"}));
    assert_eq!(st["installed"], json!(false));
    assert!(!std::fs::read_to_string(&settings).unwrap().contains("spandeck"));
}

/// Live canary: the real Claude Code runs one trivial prompt in a pane.
/// Spends a little quota; run with `cargo test -- --ignored canary`.
#[test]
#[ignore]
fn canary_real_claude_code_reports_through_hooks() {
    let real_home = std::env::var("HOME").unwrap();
    let s = Station::start_with("canary", "approval_timeout_secs = 20\n", &[("HOME", &real_home)]);
    let work = s.git_repo("canary-work");
    let pane = s.call(
        "pane.spawn",
        json!({
            "agent": "claude", "cwd": work,
            "args": ["-p", "--model", "haiku", "--allowedTools", "Bash(echo:*)"],
            "prompt": "Use the Bash tool to run exactly: echo spandeck-canary . Then reply with the single word done."
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for("claude to finish", 120, || (s.pane(&pane)["exited"] == json!(true)).then_some(()));
    let p = s.pane(&pane);
    assert_eq!(p["hooks_seen"], json!(true), "hooks did not report; screen:\n{}", s.text(&pane));
    assert!(p["session_id"].is_string());
    let tools = s.log("agent.tool");
    assert!(tools.iter().any(|t| t["data"]["tool"] == json!("Bash") && t["data"]["ok"] == json!(true)), "{tools:?}");
    let sessions = s.call("archive.sessions", json!({}));
    assert!(sessions[0]["lines"].as_u64().unwrap() > 0, "the real session file was archived");
}
