//! Phase 1 acceptance: the core daemon.
mod common;
use common::*;
use serde_json::json;

#[test]
fn spawn_type_read_and_resize_through_the_local_api() {
    let s = Station::start("alpha");
    let pane = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "sh", "cwd": s.root.join("home")}),
    );
    let id = pane["id"].as_str().unwrap();
    assert_eq!(pane["agent"], json!("shell"));
    s.call(
        "pane.send_text",
        json!({"pane": id, "text": "echo he''llo-$((40+2))"}),
    );
    s.wait_text(id, "hello-42");
    s.call("pane.resize", json!({"pane": id, "cols": 100, "rows": 30}));
    s.call("pane.send_text", json!({"pane": id, "text": "stty size"}));
    s.wait_text(id, "30 100");
    // Panes are addressed by id, by name, and as id@station.
    assert_eq!(s.pane("sh")["id"], json!(id));
    assert_eq!(s.pane(&format!("{id}@alpha"))["id"], json!(id));
    s.call("pane.kill", json!({"pane": id}));
    wait_for("exit", 10, || {
        (s.pane(id)["exited"] == json!(true)).then_some(())
    });
}

#[test]
fn the_api_refuses_a_missing_or_wrong_token() {
    let s = Station::start("alpha");
    let sock = s.root.join("state/spandeckd.sock");
    let mut c = spandeck_core::client::Client::connect(&sock, "not-the-token").unwrap();
    let err = c.call("pane.list", json!({})).unwrap_err();
    assert_eq!(err.code, "denied");
}

/// Done when: a fake agent survives an upgrade by handoff.
#[test]
fn a_fake_agent_survives_an_upgrade_by_live_handoff() {
    let s = Station::start("alpha");
    let pane = s.fake(
        json!({"steps": [{"say": "before-upgrade marker"}]}),
        json!({}),
    );
    s.wait_text(&pane, "before-upgrade marker");
    let before = s.call("station.info", json!({}));
    let pane_pid = s.pane(&pane)["pid"].clone();

    s.call("station.upgrade", json!({"binary": SPANDECK}));
    let after = wait_for("the new image to answer", 15, || {
        let i = s.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(i)
    });
    assert_eq!(
        after["pid"], before["pid"],
        "same process: the image was replaced in place"
    );

    let p = s.pane(&pane);
    assert_eq!(p["exited"], json!(false));
    assert_eq!(p["pid"], pane_pid, "the agent process was never restarted");
    assert!(
        s.text(&pane).contains("before-upgrade marker"),
        "screen state was carried across"
    );
    // The terminal is live in both directions.
    s.call(
        "pane.send_text",
        json!({"pane": pane, "text": "still-here"}),
    );
    s.wait_text(&pane, "[fake] got: still-here");
    assert_eq!(s.log("station.upgraded")[0]["data"]["adopted"], json!(1));
}

/// Done when: a forced handoff failure ends with the agent resumed by session id.
#[test]
fn a_failed_handoff_falls_back_to_resume_by_session_id() {
    let s = Station::start("alpha");
    let pane = s.fake(json!({"steps": []}), json!({}));
    s.wait_text(&pane, "[fake] session");
    let session = wait_for("session id", 10, || {
        s.pane(&pane)["session_id"].as_str().map(String::from)
    });

    // A "new binary" that loses the handoff state before starting.
    let bad = s.root.join("bad-upgrade.sh");
    std::fs::write(
        &bad,
        format!("#!/bin/sh\nif [ \"$1\" = handoff-check ]; then exec {SPANDECK} handoff-check; fi\nrm -f \"$3\"\nexec {SPANDECK} daemon\n"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
    let before = s.call("station.info", json!({}));
    s.call("station.upgrade", json!({"binary": bad}));
    wait_for("the new image", 15, || {
        let i = s.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(())
    });

    // Terminals were lost; the agent came back with its conversation.
    s.wait_text(&pane, &format!("[fake] RESUMED {session}"));
    let p = s.pane(&pane);
    assert_eq!(p["exited"], json!(false));
    assert_eq!(p["session_id"], json!(session));
    assert_eq!(s.log("pane.resumed").len(), 1);
}

#[test]
fn after_a_daemon_crash_agents_resume_and_shells_do_not() {
    let mut s = Station::start("alpha");
    let agent = s.fake(json!({"steps": []}), json!({}));
    let shell = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let session = wait_for("session id", 10, || {
        s.pane(&agent)["session_id"].as_str().map(String::from)
    });
    s.kill9();
    s.launch();
    s.wait_text(&agent, &format!("[fake] RESUMED {session}"));
    assert!(
        s.try_call("pane.get", json!({"pane": shell})).is_err(),
        "a plain shell is not brought back"
    );
}

/// Guarantee (Upgrades): after an upgrade, a health check, and rollback to
/// the previous binary if it fails. The terminals survive both swaps.
#[test]
fn an_upgrade_that_fails_its_health_check_goes_back_and_keeps_every_terminal() {
    // The switch makes the new image report itself unhealthy. It is read
    // only by an image started by an upgrade, and only in debug builds.
    let s = Station::start_with("alpha", "", &[("SPANDECK_TEST_UNHEALTHY", "1")]);
    let pane = s.fake(
        json!({"steps": [{"say": "before-upgrade marker"}]}),
        json!({}),
    );
    s.wait_text(&pane, "before-upgrade marker");
    let before = s.call("station.info", json!({}));
    let pane_pid = s.pane(&pane)["pid"].clone();

    s.call("station.upgrade", json!({"binary": SPANDECK}));
    let said = wait_for("the station to say it went back", 20, || {
        s.try_call("log.query", json!({"kind": "station.rolled_back"}))
            .ok()
            .and_then(|l| l.get(0).cloned())
    });
    assert!(
        said["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("test said so"),
        "{said}"
    );
    let after = s.call("station.info", json!({}));
    assert_eq!(after["pid"], before["pid"], "still the same process");
    // The terminal went there and back: same child, old output, still usable.
    let p = s.pane(&pane);
    assert_eq!(p["pid"], pane_pid);
    assert!(s.text(&pane).contains("before-upgrade marker"));
    s.call(
        "pane.send_text",
        json!({"pane": pane, "text": "still here"}),
    );
    s.wait_text(&pane, "[fake] got: still here");
    // The owner is told.
    let notes = s.call("notify.list", json!({"limit": 20})).to_string();
    assert!(notes.contains("was undone"), "{notes}");
}

#[test]
fn an_upgrade_to_an_incompatible_binary_is_refused_and_changes_nothing() {
    let s = Station::start("alpha");
    let pane = s.fake(json!({"steps": []}), json!({}));
    s.wait_text(&pane, "[fake] session");
    // Any program that is not spandeck. macOS keeps `true` in /usr/bin.
    let not_spandeck = ["/bin/true", "/usr/bin/true"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .expect("a `true` binary");
    let err = s
        .try_call("station.upgrade", json!({"binary": not_spandeck}))
        .unwrap_err();
    assert!(err.message.contains("handoff check"), "{}", err.message);
    s.call("pane.send_text", json!({"pane": pane, "text": "fine"}));
    s.wait_text(&pane, "[fake] got: fine");
}

/// Keep-awake: the station does not sleep while any pane is working.
#[test]
fn the_station_is_held_awake_only_while_an_agent_works() {
    let s = Station::start("alpha");
    assert_eq!(
        s.call("station.info", json!({}))["keep_awake"],
        json!(false)
    );
    let pane = s.fake(
        json!({"steps": [{"prompt": "go"}, {"work": 2500}, {"turn_end": "done"}]}),
        json!({}),
    );
    s.wait_state(&pane, "working");
    wait_for("keep-awake on", 5, || {
        (s.call("station.info", json!({}))["keep_awake"] == json!(true)).then_some(())
    });
    s.wait_state(&pane, "done");
    wait_for("keep-awake off", 5, || {
        (s.call("station.info", json!({}))["keep_awake"] == json!(false)).then_some(())
    });
}

#[test]
fn doctor_reports_on_the_station() {
    let s = Station::start("alpha");
    let d = s.call("station.doctor", json!({}));
    let names: Vec<&str> = d["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    for want in ["daemon", "database", "git", "tailscale", "keep-awake"] {
        assert!(names.contains(&want), "doctor is missing the {want} check");
    }
    assert_eq!(d["ok"], json!(true));
}
