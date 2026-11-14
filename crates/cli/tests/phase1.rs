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
    let session = wait_for_or(
        "session id",
        10,
        || format!("screen:\n{}\npane: {}", s.text(&pane), s.pane(&pane)),
        || s.pane(&pane)["session_id"].as_str().map(String::from),
    );

    // A "new binary" that loses the handoff state before starting.
    let bad = s.root.join("bad-upgrade.sh");
    std::fs::write(
        &bad,
        format!("#!/bin/sh\nif [ \"$1\" = handoff-check ]; then exec {SPANDECK} handoff-check; fi\nrm -f \"$3\"\nexec {SPANDECK} daemon\n"),
    )
    .unwrap();
    spandeck_terminal::sys::set_executable(&bad).unwrap();
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
    let session = wait_for_or(
        "session id",
        10,
        || format!("screen:\n{}\npane: {}", s.text(&agent), s.pane(&agent)),
        || s.pane(&agent)["session_id"].as_str().map(String::from),
    );
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

/// 0.2: a station updates itself to a published release, checked against its
/// checksum, and keeps every terminal. A download that does not match is
/// refused and changes nothing.
#[test]
fn a_station_updates_to_a_release_only_if_the_download_matches_its_checksum() {
    use common::fake_releases::{Releases, TAG};
    let (good, bad) = (Releases::start(true), Releases::start(false));
    let config = |r: &Releases| format!("[update]\nurl = \"{}\"\nin_place = false\n", r.url());

    let s = Station::start_with("alpha", &config(&bad), &[]);
    let pane = s.fake(
        json!({"steps": [{"say": "before-update marker"}]}),
        json!({}),
    );
    s.wait_text(&pane, "before-update marker");
    let before = s.call("station.info", json!({}));
    let err = s.try_call("station.update", json!({})).unwrap_err();
    assert!(err.message.contains("checksum"), "{}", err.message);
    assert!(s.log("station.upgrading").is_empty());
    assert_eq!(
        s.call("station.info", json!({}))["started_ms"],
        before["started_ms"]
    );
    drop(s);

    let s = Station::start_with("alpha", &config(&good), &[]);
    let pane = s.fake(
        json!({"steps": [{"say": "before-update marker"}]}),
        json!({}),
    );
    s.wait_text(&pane, "before-update marker");
    let before = s.call("station.info", json!({}));
    let pane_pid = s.pane(&pane)["pid"].clone();
    let out = s.call("station.update", json!({}));
    assert_eq!(out["release"], json!(TAG));
    let after = wait_for("the updated image to answer", 20, || {
        let i = s.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(i)
    });
    assert_eq!(after["pid"], before["pid"]);
    assert_eq!(s.pane(&pane)["pid"], pane_pid);
    assert!(s.text(&pane).contains("before-update marker"));
    // It runs the downloaded file, not the one it was started from.
    let exe = std::fs::read_link(format!("/proc/{}/exe", after["pid"]))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "releases/".into());
    assert!(exe.contains("releases/"), "{exe}");
    assert!(s.log("station.rolled_back").is_empty());
}

/// Asked for after a Claude pane ended: restart an agent in its pane and go
/// back to its conversation, or start it afresh.
#[test]
fn a_pane_is_restarted_in_place_and_the_agent_resumes_its_conversation() {
    let s = Station::start("alpha");
    let pane = s.fake(json!({"steps": [{"exit": 3}]}), json!({"name": "dev"}));
    wait_for("the agent to end", 10, || {
        (!s.pane(&pane)["exited"].is_null() && s.pane(&pane)["exited"] != json!(false))
            .then_some(())
    });
    let session = s.pane(&pane)["session_id"].as_str().unwrap().to_string();

    // Resume: the same pane, a new program, the same conversation.
    let again = s.call("pane.restart", json!({"pane": pane}));
    assert_eq!(
        (again["id"].clone(), again["name"].clone()),
        (json!(pane), json!("dev"))
    );
    s.wait_text(&pane, &format!("[fake] RESUMED {session}"));
    assert_eq!(s.pane(&pane)["exited"], json!(false));
    assert_eq!(s.log("pane.restarted")[0]["data"]["resumed"], json!(true));

    // Afresh: a live agent is stopped and started with a new conversation.
    s.call("pane.restart", json!({"pane": pane, "fresh": true}));
    let fresh = wait_for("a new session", 10, || {
        s.pane(&pane)["session_id"]
            .as_str()
            .filter(|id| *id != session)
            .map(String::from)
    });
    assert_ne!(fresh, session);
    assert!(!s.text(&pane).contains("RESUMED"), "{}", s.text(&pane));
    assert_eq!(
        s.call("pane.list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["name"] == json!("dev"))
            .count(),
        1
    );
}

/// 0.2: a restart keeps every terminal.
#[test]
fn a_restart_keeps_every_terminal() {
    let s = Station::start("alpha");
    let pane = s.fake(
        json!({"steps": [{"say": "before-restart marker"}]}),
        json!({}),
    );
    s.wait_text(&pane, "before-restart marker");
    let before = s.call("station.info", json!({}));
    let pane_pid = s.pane(&pane)["pid"].clone();
    s.call("station.restart", json!({}));
    let after = wait_for("the station to come back", 15, || {
        let i = s.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(i)
    });
    assert_eq!(after["pid"], before["pid"]);
    assert_eq!(s.pane(&pane)["pid"], pane_pid);
    s.call(
        "pane.send_text",
        json!({"pane": pane, "text": "still here"}),
    );
    s.wait_text(&pane, "[fake] got: still here");
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

/// Asked for by the owner on 14 Nov 2026: each pane shows its agent's model,
/// tokens, context and an estimated cost, with nothing typed in. The numbers
/// follow the session file as it grows, are priced from the fetched list,
/// survive a daemon restart, and start again with a new conversation.
#[test]
fn a_panes_usage_follows_its_session_file_is_priced_and_survives_a_restart() {
    let prices = fake_prices::Prices::start();
    let mut s = Station::start_with("alpha", &prices.config(), &[]);
    let reply = |output: u64| {
        json!({"transcript": {"type": "assistant", "message": {
            "id": "msg_1", "model": "claude-opus-5-5-20260901",
            "usage": {"input_tokens": 100, "output_tokens": output,
                      "cache_read_input_tokens": 1000, "cache_creation_input_tokens": 50}}}})
    };
    // One model call written as two lines, the second with the final count.
    let pane = s.fake(
        json!({"steps": [{"prompt": "go"}, reply(10), reply(200), {"turn_end": "done"}]}),
        json!({"name": "dev"}),
    );
    let priced = |s: &Station| {
        let u = s.pane(&pane)["usage"].clone();
        (u["total_tokens"] == json!(1350) && !u["cost_estimate"].is_null()).then_some(u)
    };
    let u = wait_for_or(
        "the pane's usage, priced",
        15,
        || s.pane(&pane)["usage"].to_string(),
        || priced(&s),
    );
    assert_eq!(u["model"], json!("claude-opus-5-5-20260901"));
    assert_eq!(
        (
            u["input_tokens"].clone(),
            u["output_tokens"].clone(),
            u["cache_read_tokens"].clone(),
            u["cache_write_tokens"].clone()
        ),
        (json!(100), json!(200), json!(1000), json!(50))
    );
    assert_eq!(u["context_tokens"], json!(1150));
    // The session file states no window: the list's is used, and said so.
    assert_eq!(u["context_window"], json!(1_000_000));
    assert_eq!(u["context_window_source"], json!("price list"));
    assert_eq!(u["turns"], json!(1));
    assert_eq!(u["source"], json!("Claude Code session file"));
    assert_eq!(u["price_source"], json!(prices.url()));
    // 100 x 4 + 200 x 20 + 1000 x 0.2 + 50 x 5, per million.
    let cost = u["cost_estimate"].as_f64().unwrap();
    assert!((cost - 0.00485).abs() < 1e-9, "{cost}");
    assert_eq!(u["by_model"][0]["model"], json!("claude-opus-5-5-20260901"));
    assert_eq!(u["by_model"][0]["cost_estimate"], u["cost_estimate"]);

    let sum = s.call("usage.summary", json!({}));
    assert_eq!(sum["all"]["total_tokens"], json!(1350));
    assert_eq!(sum["today"]["total_tokens"], json!(1350));
    assert_eq!(sum["panes"][0]["name"], json!("dev"));
    assert_eq!(sum["panes"][0]["all"]["cost_estimate"], u["cost_estimate"]);
    assert_eq!(sum["all"]["by_model"][0]["turns"], json!(1));

    let session = s.pane(&pane)["session_id"].as_str().unwrap().to_string();
    s.kill9();
    s.launch();
    s.wait_text(&pane, &format!("[fake] RESUMED {session}"));
    let after = wait_for_or(
        "the same usage after the restart",
        15,
        || s.pane(&pane)["usage"].to_string(),
        || priced(&s),
    );
    assert_eq!(after["cost_estimate"], u["cost_estimate"]);
    assert_eq!(after["price_fetched_ms"], u["price_fetched_ms"]);

    // A new conversation starts from nothing; the station's total keeps it.
    s.call("pane.restart", json!({"pane": pane, "fresh": true}));
    wait_for("the new conversation's empty usage", 10, || {
        let p = s.pane(&pane);
        (p["session_id"] != json!(session) && p["usage"]["total_tokens"].is_null()).then_some(())
    });
    assert_eq!(
        s.call("usage.summary", json!({}))["all"]["total_tokens"],
        json!(1350)
    );
}
