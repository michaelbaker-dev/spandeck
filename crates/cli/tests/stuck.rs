//! A pane said to be stuck (DECISIONS.md, "A pane that may be stuck is told
//! so quietly, and the owner can say it is fine"). The owner's report, 20
//! Nov 2026: an agent that watched long renders, a `tail` or two each turn
//! and no file changed, was said to be "stuck: 8 turns in a row used tools
//! but changed no files", in a card with the terminal's rows and keys that
//! could not be dismissed and came back after every turn: "why cant I get
//! rid of the terminal at the bottom here, why does it look this way?"
//!
//! Merge tier: stations with the stand-in agent, and one browser. A few
//! seconds.
//!
//!   cargo test -p spandeck --test stuck -- --ignored
//!
//! The counting rule itself is tested as the plain function it is, at every
//! change (`core/src/stuck.rs`).
#![cfg(unix)]
mod common;

use common::*;
use serde_json::{json, Value};

/// Stuck after two idle turns, so that a test's turns are few.
const TWO: &str = "stuck_idle_turns = 2\n";

/// A turn that uses tools and changes no file: one the count is about.
fn idle_turn(prompt: &str) -> Value {
    json!({"repeat": 1, "steps": [
        {"prompt": prompt},
        {"repeat": 5, "steps": [{"tool": {"name": "Bash", "command": "tail -3 render.log"}}]},
        {"turn_end": "still rendering"}
    ]})
}

/// The same, for what is typed into the pane (the prompt is what was typed).
fn idle_on_input() -> Value {
    json!([
        {"repeat": 5, "steps": [{"tool": {"name": "Bash", "command": "tail -3 render.log"}}]},
        {"turn_end": "still rendering"}
    ])
}

fn turns_done(s: &Station, pane: &str) -> usize {
    s.text(pane).matches("[fake] turn done").count()
}

fn wait_turns(s: &Station, pane: &str, n: usize) {
    wait_for(&format!("turn {n} to end"), 20, || {
        (turns_done(s, pane) >= n).then_some(())
    });
}

fn go(s: &Station, name: &str) {
    std::fs::write(s.root.join("home").join(name), "").unwrap();
}

#[test]
#[ignore = "merge tier: a station, an agent of five turns and a mail nudge"]
fn the_owner_speaking_starts_the_count_again_and_a_nudge_does_not() {
    let s = Station::start_with("alpha", TWO, &[]);
    let pane = s.fake(
        json!({"steps": [idle_turn("watch the render"), idle_turn("and again")], "on_input": idle_on_input()}),
        json!({"name": "watcher"}),
    );
    // Two turns that nobody typed, each of five tools and no file: raised.
    let p = s.wait_state(&pane, "needs_you");
    assert_eq!(
        p["needs_reason"],
        json!("may be stuck: 2 turns in a row ran 5 or more tools each and changed no files")
    );
    assert_eq!(
        p["stuck"],
        json!({"kind": "idle_turns", "says": "2 turns in a row ran 5 or more tools each and changed no files"})
    );
    assert_eq!(s.log("agent.stuck").len(), 1);

    // The owner speaks: the reason goes with the prompt, and the turn it
    // begins is the first of a new count, so its end raises nothing.
    s.call(
        "pane.send_text",
        json!({"pane": pane, "text": "that is expected, keep watching"}),
    );
    wait_turns(&s, &pane, 3);
    let p = s.wait_state(&pane, "done");
    assert_eq!(
        (&p["needs_reason"], &p["stuck"]),
        (&json!(null), &json!(null))
    );
    assert_eq!(s.log("agent.stuck").len(), 1, "one turn is not two");

    // Mail from another agent, which the station types a nudge for: not
    // the owner speaking. The turn it begins is the second, and is said.
    let other = s.fake(json!({"steps": []}), json!({"name": "other"}));
    s.wait_text(&other, "[fake] session");
    s.call_as_pane(
        &other,
        "msg.send",
        json!({"to": "watcher", "body": "any news?"}),
    )
    .unwrap();
    let p = s.wait_state(&pane, "needs_you");
    assert_eq!(p["stuck"]["kind"], json!("idle_turns"), "{p}");
    assert!(s.text(&pane).contains("new spandeck message"), "nudged");
    assert_eq!(s.log("agent.stuck").len(), 2);
}

#[test]
#[ignore = "merge tier: a station and an agent of five turns"]
fn a_turn_that_waits_on_background_work_is_not_counted() {
    let s = Station::start_with("alpha", TWO, &[]);
    let line = |v: Value| json!({"transcript": v});
    let pane = s.fake(
        json!({"steps": [
            {"prompt": "render the film and watch it"},
            // A command left running, as Claude Code writes one.
            line(json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_r1", "name": "Bash",
                "input": {"command": "render --all > render.log", "run_in_background": true}}]}})),
            line(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_r1", "content": "Running in the background"}]},
                "toolUseResult": {"stdout": "", "stderr": "", "backgroundTaskId": "bash_1"}})),
            {"repeat": 5, "steps": [{"tool": {"name": "Bash", "command": "tail -3 render.log"}}]},
            {"turn_end": "still rendering"},
            idle_turn("how far is it?"), idle_turn("and now?"),
            {"say": "[fake] the render goes on"},
            {"wait_file": "render.over"},
            // The program reports it over; turns after that are counted.
            line(json!({"type": "user", "message": {"role": "user", "content": "<task-notification>\n<task-id>bash_1</task-id>\n<tool-use-id>toolu_r1</tool-use-id>\n<status>completed</status>\n<summary>Command completed</summary>\n</task-notification>"},
                "origin": {"kind": "task-notification"}})),
            idle_turn("look again"), idle_turn("and again"),
            {"sleep": 600_000}
        ]}),
        json!({"name": "renderer"}),
    );
    s.wait_text(&pane, "[fake] the render goes on");
    wait_turns(&s, &pane, 3);
    let p = s.wait_state(&pane, "done");
    assert_eq!(p["needs_reason"], json!(null), "{p}");
    assert!(s.log("agent.stuck").is_empty(), "three turns, none counted");
    let live = s.call("pane.conversation", json!({"pane": pane, "limit": 1}))["live"].clone();
    assert_eq!(
        live["tasks"]["running"].as_array().map(Vec::len),
        Some(1),
        "{live}"
    );

    go(&s, "render.over");
    let p = s.wait_state(&pane, "needs_you");
    assert_eq!(
        p["needs_reason"],
        json!("may be stuck: 2 turns in a row ran 5 or more tools each and changed no files")
    );
    assert_eq!(turns_done(&s, &pane), 5);
}

#[test]
#[ignore = "merge tier: two stations and an agent of seven turns"]
fn it_is_fine_is_the_owners_and_holds_until_a_file_is_changed() {
    let (a, b) = Station::pair_with(TWO);
    let pane = a.fake(
        json!({"steps": [
            idle_turn("watch the render"), idle_turn("and again"),
            {"wait_file": "more"},
            idle_turn("a third"), idle_turn("a fourth"),
            {"say": "[fake] four turns"},
            {"wait_file": "write"},
            {"prompt": "note it down"}, {"tool": {"name": "Write", "file": "notes.md"}}, {"turn_end": "noted"},
            idle_turn("watch again"), idle_turn("and again"),
            {"sleep": 600_000}
        ]}),
        json!({"name": "watcher"}),
    );
    a.wait_state(&pane, "needs_you");
    let from_b = |method: &str| {
        b.try_call(
            "peer.call",
            json!({"station": "alpha", "method": method, "params": {"pane": pane}}),
        )
    };
    assert_eq!(
        from_b("pane.get").unwrap()["stuck"]["kind"],
        json!("idle_turns")
    );

    // No agent may say so, of its own pane or of another's.
    let err = a
        .call_as_pane(&pane, "pane.stuck_fine", json!({"pane": pane}))
        .unwrap_err();
    assert_eq!(err.code, "denied");
    assert_eq!(a.pane(&pane)["state"], json!("needs_you"));

    // The owner, from the other station's deck: gone at once at the
    // pane's own station, and so in what every station is told of it.
    let said = from_b("pane.stuck_fine").unwrap();
    assert_eq!(
        (&said["dismissed"], &said["kind"]),
        (&json!(true), &json!("idle_turns"))
    );
    for p in [a.pane(&pane), from_b("pane.get").unwrap()] {
        assert_eq!(
            (&p["state"], &p["needs_reason"], &p["stuck"]),
            (&json!("done"), &json!(null), &json!(null)),
            "{p}"
        );
    }
    let log = a.log("pane.stuck_fine");
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["pane"], json!(pane));
    assert!(
        log[0]["actor"].as_str().unwrap().contains("via bravo"),
        "{log:?}"
    );
    // Said a second time (two decks, one tap each): nothing to do.
    assert_eq!(
        from_b("pane.stuck_fine").unwrap()["dismissed"],
        json!(false)
    );
    assert_eq!(a.log("pane.stuck_fine").len(), 1);

    // Two more turns of the same kind: quiet.
    go(&a, "more");
    a.wait_text(&pane, "[fake] four turns");
    let p = a.wait_state(&pane, "done");
    assert_eq!(p["needs_reason"], json!(null), "{p}");
    assert_eq!(a.log("agent.stuck").len(), 1);

    // A file is changed: the count starts anew, and is watched again.
    go(&a, "write");
    let p = a.wait_state(&pane, "needs_you");
    assert_eq!(
        p["needs_reason"],
        json!("may be stuck: 2 turns in a row ran 5 or more tools each and changed no files")
    );
    assert_eq!(turns_done(&a, &pane), 7);
    assert_eq!(a.log("agent.stuck").len(), 2);
}

#[test]
#[ignore = "merge tier: a station and a browser at a phone's width"]
fn chat_shows_a_notice_for_stuck_and_the_terminal_for_a_real_prompt() {
    use common::browser::Page;
    let s = Station::start_with("alpha", TWO, &[]);
    let watcher = s.fake(
        json!({"steps": [idle_turn("watch the render"), idle_turn("and again"), {"sleep": 600_000}]}),
        json!({"name": "watcher"}),
    );
    let failing = s.fake(
        json!({"steps": [{"prompt": "build"}, {"repeat": 3, "steps": [{"tool": {"name": "Bash", "command": "make build", "ok": false}}]}, {"sleep": 600_000}]}),
        json!({"name": "builder"}),
    );
    // A program that really waits at a prompt of its own in its terminal.
    let asks = s.fake(
        json!({"steps": [{"prompt": "deploy"}, {"say": "Deploy to production?\n❯ 1. Yes\n  2. No"}, {"needs_input": "Claude needs your input"}]}),
        json!({"name": "deployer"}),
    );
    for p in [&watcher, &failing, &asks] {
        s.wait_state(p, "needs_you");
    }
    let at = |id: &str| format!("#/s/alpha/p/{id}");
    let phone = Page::open(&s, 390, 844);
    // What the dock holds: the notice's words and buttons, and whether
    // the terminal's rows, keys to press (the row the owner opens with
    // "Keys" is there, hidden) or the old card are in sight.
    let dock = "(d => d && d.querySelector('.chat-card') && ({says: d.querySelector('.stuck-says')?.innerText || null, \
        buttons: [...d.querySelectorAll('.stuck-note button')].map(b => b.innerText), \
        rows: !!d.querySelector('.chat-peek'), keys: [...d.querySelectorAll('.wait-keys .key')].filter(k => k.offsetParent).length, \
        card: d.querySelector('.chat-card.wait b')?.innerText || null}))(document.querySelector('.phone-chat .chat-dock'))";

    phone.goto(&at(&watcher));
    assert_eq!(
        phone.wait("the notice", dock),
        json!({"says": "watcher may be stuck: 2 turns in a row ran 5 or more tools each and changed no files. Nothing waits for an answer.",
            "buttons": ["It is fine", "Open terminal"], "rows": false, "keys": 0, "card": null})
    );
    // The same failure, the same notice.
    phone.goto(&at(&failing));
    assert_eq!(
        phone.wait("the notice", dock),
        json!({"says": "builder may be stuck: the same command failed 3 times in a row (make build). Nothing waits for an answer.",
            "buttons": ["It is fine", "Open terminal"], "rows": false, "keys": 0, "card": null})
    );
    // "It is fine": the notice goes, and the station says so.
    phone.js("(document.querySelector('.phone-chat .stuck-fine').click(), true)");
    phone.wait(
        "the notice gone",
        "!document.querySelector('.phone-chat .chat-card')",
    );
    let p = s.pane(&failing);
    assert_eq!(
        (&p["state"], &p["stuck"]),
        (&json!("working"), &json!(null)),
        "{p}"
    );
    assert_eq!(s.log("pane.stuck_fine").len(), 1);

    // A real prompt in the terminal: the card as it always was, with the
    // terminal's last rows and the keys, which reach the program.
    phone.goto(&at(&asks));
    let real = phone.wait(
        "the card, its rows and keys",
        &format!("(v => v && v.card && v.rows && document.querySelector('.phone-chat .chat-peek').innerText.includes('Deploy to production?') && v)({dock})"),
    );
    assert_eq!(
        real["card"],
        json!("Waiting in the terminal: Claude needs your input")
    );
    assert_eq!(real["says"], json!(null));
    assert!(real["keys"].as_u64().unwrap() >= 5, "{real}");
    for key in ["1", "Enter"] {
        phone.js(&format!("([...document.querySelectorAll('.phone-chat .chat-card .wait-keys .key')].find(b => b.innerText === '{key}').click(), true)"));
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    s.wait_text(&asks, "[fake] got: 1");
}
