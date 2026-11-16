//! An idle agent is woken when its mail arrives (DECISIONS.md, "An idle
//! agent is told of its mail when it arrives"). Merge tier: it waits
//! several times for a nudge that must not come, and times the ones that
//! must.
//!
//!   cargo test -p spandeck --test wake -- --ignored
#![cfg(unix)]
mod common;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

/// The nudge that wakes an idle agent with mail, as its terminal shows it.
const NUDGE: &str = "[fake] got: You have ";

fn reads_mail() -> Value {
    json!({"steps": [], "on_input": [{"rpc": {"method": "msg.inbox"}}, {"turn_end": "read my mail"}]})
}

/// The times (the station's clock, ms) of this kind of entry for a pane in
/// the ship's log, oldest first.
fn logged(s: &Station, kind: &str, pane: &str) -> Vec<u64> {
    let mut at: Vec<u64> = s
        .log(kind)
        .iter()
        .filter(|e| e["pane"] == json!(pane))
        .filter_map(|e| e["ts"].as_u64())
        .collect();
    at.sort();
    at
}

/// Send from `from` to an agent that has been idle for longer than the
/// nudge waits for, `times` times, each at another moment of the second.
/// Returns how long each nudge came after the message was sent, by the
/// station's own log (both stations are on this machine's clock).
fn woken_after(from: &Station, at: &Station, to: &str, pane: &str, times: u64) -> Vec<u64> {
    let mut took = vec![];
    for i in 0..times {
        wait_for("the agent to be idle", 20, || {
            matches!(at.pane(pane)["state"].as_str(), Some("idle" | "done")).then_some(())
        });
        // Idle for more than 1.2 s, and at a different phase of the look
        // once a second each time.
        std::thread::sleep(Duration::from_millis(1500 + i * 137));
        let before = logged(at, "msg.nudged", pane).len();
        let sent = from.call("msg.send", json!({"to": to, "body": format!("note {i}")}));
        let nudged = wait_for("the agent to be told of its mail", 20, || {
            logged(at, "msg.nudged", pane).get(before).copied()
        });
        let sent_at = wait_for("the send in the ship's log", 10, || {
            from.log("msg.sent")
                .iter()
                .find(|e| e["data"]["id"] == sent["id"])
                .and_then(|e| e["ts"].as_u64())
        });
        took.push(nudged.saturating_sub(sent_at));
        // It reads its mail, and is idle again.
        wait_for("the mail to be read", 20, || {
            (at.text(pane).matches("[fake] turn done").count() as u64 > i).then_some(())
        });
    }
    took
}

/// The measured fault (plans/performance-REPORT.md): an idle agent was
/// told of its mail by a look made once a second, so send→nudge was 515 ms
/// at the median on one station and 1126 ms across two. Now the pane is
/// looked at when the message is stored. A look once a second would put
/// half of these over 500 ms; every one must be under 250.
///
/// And the Guarantees the new road must keep (Stop, pause and offline:
/// "Pause: no new work is delivered", "no mail reaches a paused agent by
/// any road"; Decisions: "A typed one-line nudge only wakes an idle
/// agent"): a paused agent and one in a turn are not typed into; an agent
/// that has just gone idle is nudged only once it has been idle 1200 ms;
/// and with the switch off the look once a second still delivers.
#[test]
#[ignore = "merge tier: times nudges and waits for nudges that must not come"]
fn an_idle_agent_is_woken_when_its_mail_arrives_and_nobody_else_is() {
    let (a, b) = Station::pair();
    let near = a.fake(reads_mail(), json!({"name": "near"}));
    let far = b.fake(reads_mail(), json!({"name": "far"}));
    a.wait_text(&near, "[fake] session");
    b.wait_text(&far, "[fake] session");

    // Same station, then across two.
    let same = woken_after(&a, &a, "near", &near, 8);
    let cross = woken_after(&a, &b, "far@bravo", &far, 8);
    println!("send→nudged, same station: {same:?} ms; across two: {cross:?} ms");
    for (what, took) in [("same station", &same), ("across two stations", &cross)] {
        assert!(
            took.iter().all(|ms| *ms < 250),
            "{what}: a nudge waited for the look once a second: {took:?} ms"
        );
    }
    // One nudge per message, and only the nudge was typed.
    assert_eq!(a.text(&near).matches(NUDGE).count(), 8);
    assert_eq!(b.text(&far).matches(NUDGE).count(), 8);

    // Paused: nothing is typed, however the mail arrives.
    a.wait_state(&near, "done");
    a.call("pane.pause", json!({"pane": near, "paused": true}));
    a.call("msg.send", json!({"to": "near", "body": "while paused"}));
    std::thread::sleep(Duration::from_millis(3000));
    assert_eq!(a.text(&near).matches(NUDGE).count(), 8, "paused: no nudge");
    assert_eq!(logged(&a, "msg.nudged", &near).len(), 8);
    // Resumed: the look once a second tells it.
    a.call("pane.pause", json!({"pane": near, "paused": false}));
    wait_for("the nudge after Resume", 10, || {
        (a.text(&near).matches(NUDGE).count() == 9).then_some(())
    });

    // In a turn: nothing is typed into it; its own hook tells it.
    let busy = a.fake(
        json!({"steps": [{"prompt": "go"}, {"work": 2500}, {"turn_end": "done"}],
               "on_input": [{"turn_end": "ok"}]}),
        json!({"name": "busy"}),
    );
    a.wait_text(&busy, "[fake] working");
    a.call("msg.send", json!({"to": "busy", "body": "while working"}));
    a.wait_text(&busy, "[fake] work finished");
    let screen = a.wait_text(&busy, "new spandeck message");
    assert!(!screen.contains(NUDGE), "{screen}");
    std::thread::sleep(Duration::from_millis(3000));
    assert!(
        logged(&a, "msg.nudged", &busy).is_empty(),
        "announced by the hook, so never typed: {}",
        a.text(&busy)
    );

    // Just gone idle: mail that arrives then is announced once the agent
    // has been idle for more than 1200 ms, not before, and not a look
    // later.
    let quick = a.fake(
        json!({"steps": [], "on_input": [{"prompt": "x"}, {"turn_end": "ok"}]}),
        json!({"name": "quick"}),
    );
    a.wait_text(&quick, "[fake] session");
    a.call("pane.send_text", json!({"pane": quick, "text": "go"}));
    a.wait_text(&quick, "[fake] turn done");
    a.wait_state(&quick, "done");
    a.call("msg.send", json!({"to": "quick", "body": "just idle"}));
    let nudged = wait_for("the nudge", 10, || {
        logged(&a, "msg.nudged", &quick).first().copied()
    });
    let done = a
        .log("pane.state")
        .iter()
        .filter(|e| e["pane"] == json!(quick) && e["data"]["state"] == json!("done"))
        .filter_map(|e| e["ts"].as_u64())
        .filter(|ts| *ts <= nudged)
        .max()
        .expect("the pane's turn ended");
    let idle_for = nudged - done;
    println!("nudged after {idle_for} ms idle");
    assert!(idle_for > 1200, "nudged after only {idle_for} ms idle");
    assert!(
        idle_for < 1450,
        "the nudge waited for a look: {idle_for} ms"
    );

    // The switch off: the look once a second still tells an idle agent.
    let off = a.call("station.flags", json!({"set": {"mail_wake": false}}));
    assert_eq!(off["flags"]["mail_wake"], json!(false), "{off}");
    a.wait_state(&near, "done");
    std::thread::sleep(Duration::from_millis(1500));
    a.call("msg.send", json!({"to": "near", "body": "by the look"}));
    wait_for("the nudge by the look once a second", 10, || {
        (a.text(&near).matches(NUDGE).count() == 10).then_some(())
    });
    a.call("station.flags", json!({"set": {"mail_wake": null}}));
}
