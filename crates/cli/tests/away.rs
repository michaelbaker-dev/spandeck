//! While spandeck is away (DECISIONS.md, "While spandeck is away"): an
//! agent's rule check waits, then refuses where a rule is enforced; an
//! approval survives a restart. Merge tier: each test ends a station's
//! daemon several times while its agents run on, and waits out a hook's
//! deadline.
//!
//!   cargo test -p spandeck --test away -- --ignored
//!
//! The agents outlive the daemon because the station keeps its panes in
//! the pane host (`SPANDECK_PANE_HOST=1`), as every Windows station does.
//! The stand-in agent's hooks are `spandeck hook`'s own code
//! (core/guard.rs); what it does not have is a real agent's timeout, which
//! it only tells the hook.
#![cfg(unix)]
mod common;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

/// How the refusal begins on the agent's screen, and how it ends.
const DOWN: &str = "BLOCKED Bash: Blocked by spandeck: spandeck is not running on this station";
const DOWN_END: &str = "stop and wait for the owner.";

/// A station whose panes are in the pane host, which waits a minute for a
/// daemon before it ends them.
fn station() -> Station {
    Station::start_with(
        "alpha",
        "",
        &[("SPANDECK_PANE_HOST", "1"), ("SPANDECK_HOST_LINGER", "60")],
    )
}

/// A step that waits until the test says go: the daemon cannot be asked to
/// type into a pane while it is down.
fn wait_go(name: &str, n: u32) -> Value {
    json!({"sh": format!("until [ -e {name}.go{n} ]; do sleep 0.05; done")})
}

fn go(s: &Station, name: &str, n: u32) {
    std::fs::write(s.root.join("home").join(format!("{name}.go{n}")), "").unwrap();
}

/// An agent that makes the same tool call each time it is told to go.
fn tool_agent(s: &Station, name: &str, deadline: Value, calls: u32) -> String {
    // "ready" is said once the prompt's own hook has been answered: no
    // hook is in flight when the daemon is first ended.
    let mut steps = vec![json!({"prompt": "tidy up"}), json!({"say": "ready"})];
    for n in 1..=calls {
        steps.push(wait_go(name, n));
        steps.push(json!({"tool": {"name": "Bash", "command": "rm -rf build"}}));
        // When the call was settled, one way or the other: the screen
        // cannot be read while the daemon is down.
        steps.push(json!({"sh": format!("touch {name}.done{n}")}));
    }
    let pane = s.fake(
        json!({"steps": steps, "hook_deadline": deadline}),
        json!({"name": name}),
    );
    s.wait_text(&pane, "ready");
    pane
}

/// How long the agent's `n`th call took from "go" to settled.
fn took(s: &Station, name: &str, n: u32) -> Duration {
    let at = |what: &str| {
        std::fs::metadata(s.root.join("home").join(format!("{name}.{what}{n}")))
            .and_then(|m| m.modified())
            .unwrap()
    };
    at("done").duration_since(at("go")).unwrap_or_default()
}

fn guard(s: &Station, pane: &str) -> Value {
    wait_for("the pane's guard", 15, || {
        let text =
            std::fs::read_to_string(s.root.join(format!("state/panes/{pane}.guard"))).ok()?;
        serde_json::from_str(&text).ok()
    })
}

fn count(s: &Station, pane: &str, needle: &str) -> usize {
    s.text(pane).matches(needle).count()
}

/// The daemon is ended without warning and started again `down` later.
fn away_for(s: &mut Station, down: Duration, during: impl FnOnce(&Station)) {
    s.kill9();
    during(s);
    std::thread::sleep(down);
    s.launch();
}

/// The audit's gap 7 (plans/FAILURES.md): with the daemon away a hook
/// passed in silence, 596 of 596 in a minute, so an enforced rule was not
/// applied. And REVIEW2's blocker 8: the hook had 10 seconds, so one that
/// waited longer and then refused would never have refused.
///
/// Guarantees, Enforcement: "only a hook or approval channel can block";
/// "A rule is shown as enforced for an agent only where that agent's
/// integration can block it." Decided by default on 15 Nov 2026: the hook
/// waits, then refuses where a rule is enforced, and allows otherwise.
#[test]
#[ignore = "merge tier: ends a station's daemon six times under running agents"]
fn rules_hold_while_spandeck_is_away() {
    let mut s = station();
    // Told its timeout, as a session started after the update is: six
    // seconds here, so that it answers by four.
    let agent = tool_agent(&s, "agent", json!(6), 5);
    let g = guard(&s, &agent);
    assert_eq!(
        (&g["enforced"], &g["when_down"]),
        (&json!(false), &json!("deny_enforced")),
        "{g}"
    );

    // No rule is enforced for it. Away for two seconds: the call waits and
    // is answered by the daemon itself.
    away_for(&mut s, Duration::from_secs(2), |s| {
        go(s, "agent", 1);
    });
    s.wait_text(&agent, "[fake] ran Bash (ok)");
    assert!(!s.text(&agent).contains("hook error"), "{}", s.text(&agent));
    assert_eq!(s.log("pane.kept").len(), 1, "the agent outlived the daemon");

    // Away for longer than the hook has: with nothing enforced the call
    // goes ahead, and only then.
    let began = Instant::now();
    away_for(&mut s, Duration::from_secs(6), |s| {
        go(s, "agent", 2);
    });
    wait_for("the second call to go ahead", 15, || {
        (count(&s, &agent, "[fake] ran Bash (ok)") == 2).then_some(())
    });
    assert_eq!(count(&s, &agent, "hook error"), 1, "{}", s.text(&agent));
    assert!(began.elapsed() > Duration::from_secs(4));
    assert!(!s.text(&agent).contains("BLOCKED"));

    // The owner makes a rule. It is in every pane's guard within ten
    // seconds, and at once when a switch is set.
    std::fs::write(
        s.root.join("config/orders.toml"),
        "[[order]]\nid = \"keep-build\"\ntext = \"never delete the build folder\"\nkind = \"rule\"\nlayer = \"fleet\"\ntool = \"Bash\"\npattern = \"rm -rf build\"\n",
    )
    .unwrap();
    s.call(
        "station.flags",
        json!({"set": {"hooks_when_down": "deny_enforced"}}),
    );
    let g = guard(&s, &agent);
    assert_eq!(
        (&g["enforced"], &g["rules"]),
        (&json!(true), &json!(["keep-build"])),
        "{g}"
    );
    assert_eq!(
        s.call("orders.view", json!({"pane": agent}))["orders"][0]["enforced"],
        json!(true),
        "the guard says what the Control Deck shows"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(s.root.join(format!("state/panes/{agent}.guard")))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "a private file");
    }
    // A session from before the update: ten seconds, and its hook is told
    // nothing.
    let old = tool_agent(&s, "old", json!("old"), 2);
    assert_eq!(guard(&s, &old)["enforced"], json!(true));

    // Away for two seconds: both get the daemon's own answer, which is the
    // rule's.
    away_for(&mut s, Duration::from_secs(2), |s| {
        go(s, "agent", 3);
        go(s, "old", 1);
    });
    for pane in [&agent, &old] {
        let screen = s.wait_text(pane, "BLOCKED Bash: Blocked by a spandeck standing order");
        assert!(!screen.contains("spandeck is not running"), "{screen}");
    }
    // No hook went unanswered this time (the one before is still on the
    // first agent's screen).
    assert_eq!(count(&s, &agent, "hook error"), 1);
    assert_eq!(count(&s, &old, "hook error"), 0);
    assert_eq!(s.log("order.blocked").len(), 2);

    // Away for longer than either hook has: refused, in so many words, and
    // the one from before decides inside its ten seconds.
    s.kill9();
    go(&s, "agent", 4);
    go(&s, "old", 2);
    // (The daemon is down: the screens are read when it is back.)
    std::thread::sleep(Duration::from_millis(9_500));
    s.launch();
    for pane in [&agent, &old] {
        let screen = s.wait_text(pane, DOWN);
        assert!(screen.contains(DOWN_END), "{screen}");
        assert_eq!(count(&s, pane, "BLOCKED"), 2, "{screen}");
    }
    assert_eq!(
        count(&s, &agent, "[fake] ran Bash (ok)"),
        2,
        "nothing more ran"
    );
    assert_eq!(count(&s, &old, "[fake] ran Bash"), 0);
    // Each hook answered two seconds before its agent would have cancelled
    // it: at four of its six seconds, and at eight of the ten a session
    // from before the update gives. The daemon saw neither call.
    let (told, untold) = (took(&s, "agent", 4), took(&s, "old", 2));
    println!("refused after {told:?} (told 6 s) and {untold:?} (not told: 10 s)");
    assert!(
        told > Duration::from_millis(3500) && told < Duration::from_millis(5500),
        "{told:?}"
    );
    assert!(
        untold > Duration::from_millis(7500) && untold < Duration::from_millis(9500),
        "{untold:?}"
    );
    assert_eq!(
        s.log("order.blocked").len(),
        2,
        "the daemon blocked neither"
    );

    // The switch: as before this, a hook with no answer says nothing.
    let set = s.call(
        "station.flags",
        json!({"set": {"hooks_when_down": "allow"}}),
    );
    assert_eq!(set["flags"]["hooks_when_down"], json!("allow"), "{set}");
    assert_eq!(guard(&s, &agent)["when_down"], json!("allow"));
    away_for(&mut s, Duration::from_secs(5), |s| {
        go(s, "agent", 5);
    });
    wait_for("the call to go ahead with the switch set", 15, || {
        (count(&s, &agent, "[fake] ran Bash (ok)") == 3).then_some(())
    });
    assert_eq!(count(&s, &agent, "BLOCKED"), 2);

    // A pane that is closed leaves no guard behind.
    s.call("pane.close", json!({"pane": old, "force": true}));
    wait_for("the closed pane's guard to go", 25, || {
        (!s.root.join(format!("state/panes/{old}.guard")).exists()).then_some(())
    });
}

fn waiting(s: &Station) -> Vec<Value> {
    s.call("approval.list", json!({}))
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn status(s: &Station, id: &Value) -> Value {
    s.call("approval.list", json!({"all": true}))
        .as_array()
        .unwrap()
        .iter()
        .find(|a| &a["id"] == id)
        .map(|a| a["status"].clone())
        .unwrap_or(Value::Null)
}

/// The audit's gap 4: a pending approval did not survive the daemon. After
/// an upgrade, a kill or the pane host adopting its panes it stayed
/// "pending" for good and could not be answered.
///
/// Pre-build resolutions: "the permission hook holds for an answer from
/// the Control Deck or phone, then falls back to the terminal prompt on
/// timeout." Guarantees, Trust: `approval.answer` is the owner's alone.
#[test]
#[ignore = "merge tier: restarts a station under a waiting approval four times"]
fn an_approval_survives_a_restart_and_is_answered_once() {
    let mut s = station();
    let mut steps = vec![json!({"prompt": "publish"}), json!({"say": "ready"})];
    for n in 1..=3 {
        steps.push(wait_go("asker", n));
        steps.push(
            json!({"permission": {"name": "Bash", "command": format!("cargo publish #{n}")}}),
        );
    }
    let asker = s.fake(json!({"steps": steps}), json!({"name": "asker"}));
    s.wait_text(&asker, "ready");
    let pid = s.pane(&asker)["pid"].as_i64().unwrap() as i32;

    // 1. A restart in place under a waiting approval.
    go(&s, "asker", 1);
    let first = wait_for("the approval", 15, || waiting(&s).first().cloned());
    assert_eq!(first["attached"], json!(true), "{first}");
    // The agent cannot answer it, list it, or learn the id its hook made.
    for (method, params) in [
        ("approval.answer", json!({"id": first["id"], "allow": true})),
        ("approval.list", json!({})),
    ] {
        let refused = s.call_as_pane(&asker, method, params).unwrap_err();
        assert_eq!(refused.code, "denied", "{method}: {refused}");
    }
    assert!(first["detail"].get("hook").is_none(), "{first}");
    assert_eq!(first["detail"]["tool"]["name"], json!("Bash"));

    let before = s.call("station.info", json!({}));
    s.call("station.restart", json!({}));
    wait_for("the new image", 15, || {
        let i = s.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(())
    });
    // The same approval, waited on again by the same hook.
    let again = wait_for("the hook to find its approval again", 15, || {
        waiting(&s)
            .into_iter()
            .find(|a| a["attached"] == json!(true))
    });
    assert_eq!(again["id"], first["id"]);
    assert_eq!(waiting(&s).len(), 1, "not a second approval");
    assert_eq!(s.log("approval.reattached").len(), 1);
    assert!(!s.text(&asker).contains("[fake] permission:"), "still held");
    let done = s.call("approval.answer", json!({"id": first["id"], "allow": true}));
    assert_eq!(
        (&done["ok"], &done["delivered"]),
        (&json!(true), &json!(true)),
        "{done}"
    );
    s.wait_text(&asker, "[fake] permission: allow");
    assert_eq!(status(&s, &first["id"]), json!("allowed"));

    // 2. Answered while the hook is away. The agent is frozen, so that its
    // hook cannot be back before the owner answers: the daemon is ended and
    // started, the owner denies, and only then does the hook reconnect.
    go(&s, "asker", 2);
    let second = wait_for("the second approval", 15, || waiting(&s).first().cloned());
    kill_pid(pid, 19); // SIGSTOP
    away_for(&mut s, Duration::from_millis(300), |_| {});
    let listed = waiting(&s);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(
        (&listed[0]["id"], &listed[0]["attached"]),
        (&second["id"], &json!(false)),
        "still waiting, with its hook away"
    );
    let done = s.call(
        "approval.answer",
        json!({"id": second["id"], "allow": false, "reason": "not from this branch"}),
    );
    assert_eq!(done["delivered"], json!(false), "{done}");
    assert_eq!(
        done["note"],
        json!("recorded: the agent will get it when it reconnects")
    );
    assert!(waiting(&s).is_empty(), "answered: it has left the list");
    // The other answer is refused, as any second answer is.
    assert!(s
        .try_call(
            "approval.answer",
            json!({"id": second["id"], "allow": true})
        )
        .is_err());
    kill_pid(pid, 18); // SIGCONT
    s.wait_text(&asker, "[fake] permission: deny");
    assert_eq!(status(&s, &second["id"]), json!("denied"));
    assert_eq!(s.log("approval.collected").len(), 1);
    assert_eq!(
        count(&s, &asker, "[fake] permission: allow"),
        1,
        "each answer once"
    );
    assert_eq!(count(&s, &asker, "[fake] permission: deny"), 1);

    // 3. A hook that does not come back: its approval leaves the list by
    // itself (it stayed for ever), and the hook, when it does turn up,
    // falls through to the agent's own prompt.
    go(&s, "asker", 3);
    let third = wait_for("the third approval", 15, || waiting(&s).first().cloned());
    kill_pid(pid, 19);
    away_for(&mut s, Duration::from_millis(300), |_| {});
    assert_eq!(waiting(&s).len(), 1);
    wait_for(
        "the approval nobody came back for to be expired",
        30,
        || waiting(&s).is_empty().then_some(()),
    );
    assert_eq!(status(&s, &third["id"]), json!("expired"));
    kill_pid(pid, 18);
    s.wait_text(&asker, "[fake] permission: none");
    assert_eq!(
        s.log("approval.requested").len(),
        3,
        "three were asked, in all"
    );
}
