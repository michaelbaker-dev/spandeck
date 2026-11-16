//! A stopped or restarted agent stays put until the owner releases it, and
//! a stop never claims a program it could not reach (DECISIONS.md, "A
//! stopped agent stays stopped"). Scenario tests with a real station: they
//! wait whole seconds to see that nothing arrives, and one kills the
//! station, so they are in the merge tier (`--ignored`).

mod common;

use std::time::Duration;

use common::*;
use serde_json::{json, Value};

/// The nudge that wakes an idle agent with mail, as its terminal shows it.
const NUDGE: &str = "[fake] got: You have ";
/// The notice a hook gives an agent in a turn.
const NOTICE: &str = "new spandeck message";

/// Long enough for the mail pump to have typed a nudge if it was going to:
/// it runs once a second, for an agent idle for 1.2 seconds.
fn a_nudge_would_have_come() {
    std::thread::sleep(Duration::from_millis(3500));
}

fn held(s: &Station, pane: &str) -> Value {
    s.pane(pane)["held"].clone()
}

fn unread(s: &Station, body: &str) -> bool {
    s.call("msg.recent", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["body"] == json!(body) && m["read"].is_null())
}

/// An agent that answers a line typed at its prompt with one tool call and
/// the end of its turn: every hook that can carry mail fires.
fn agent(s: &Station, name: &str, steps: Value) -> String {
    s.fake(
        json!({"steps": steps, "on_input": [{"tool": {"name": "Bash", "command": "true"}}, {"turn_end": "done"}]}),
        json!({"name": name}),
    )
}

/// Guarantees "Stop: the agent's current turn is interrupted; the process
/// stays" and "Pause: no new work is delivered". REVIEW2 blocker 3,
/// measured on 0.4.2: a stopped agent was woken again by mail, by the
/// typed nudge and by its hooks, and the turn it was stopped in was
/// continued with that mail.
#[test]
#[ignore = "merge tier: waits several times for a nudge that must not come"]
fn a_stopped_agent_stays_stopped() {
    let s = Station::start("alpha");
    let worker = agent(
        &s,
        "worker",
        json!([{"prompt": "build"}, {"work": 600_000}]),
    );
    s.wait_state(&worker, "working");
    assert_eq!(held(&s, &worker), Value::Null);
    assert!(
        s.call("station.info", json!({}))["caps"]
            .as_array()
            .unwrap()
            .contains(&json!("holds")),
        "a deck offers Release only to a station that says it holds"
    );

    // Mail arrives while it works. Then the owner stops it.
    s.call(
        "msg.send",
        json!({"to": "worker", "body": "one for the worker"}),
    );
    let stop = s.call("pane.stop", json!({"pane": worker}));
    assert_eq!(stop["stopped"], json!(true), "{stop}");
    s.wait_text(&worker, "[fake] interrupted");
    // The turn it was stopped in is not continued with the mail: with it,
    // the pane stayed "working".
    s.wait_state(&worker, "done");
    let h = held(&s, &worker);
    assert_eq!(h["stopped"]["by"], json!("you"), "{h}");
    assert_eq!(h["stopped"]["reached"], json!(true), "{h}");
    assert_eq!(h["restored"], Value::Null);
    assert_eq!(h["mail"], json!(1), "{h}");

    // No typed nudge, however long it sits idle with unread mail.
    a_nudge_would_have_come();
    let screen = s.text(&worker);
    assert!(!screen.contains(NUDGE), "{screen}");
    assert!(!screen.contains(NOTICE), "{screen}");
    assert!(s.log("msg.nudged").is_empty());
    assert!(unread(&s, "one for the worker"));

    // Nobody but the owner releases, and looking does not.
    let e = s
        .call_as_pane(&worker, "pane.release", json!({"pane": worker}))
        .unwrap_err();
    assert_eq!(e.code, "denied", "an agent released a pane: {}", e.message);
    let e = s
        .call_as_pane(&worker, "pane.release", json!({"all": true}))
        .unwrap_err();
    assert_eq!(e.code, "denied");
    // What a deck's terminal sends by itself when a pane is opened or
    // clicked into: focus, the cursor's place, the mouse. And a resize.
    s.call(
        "pane.input",
        json!({"pane": worker, "data": "\u{1b}[I\u{1b}[12;40R\u{1b}[<35;10;4M\u{1b}[O"}),
    );
    s.call(
        "pane.resize",
        json!({"pane": worker, "cols": 100, "rows": 30}),
    );
    s.call("pane.text", json!({"pane": worker}));
    assert!(
        held(&s, &worker)["stopped"].is_object(),
        "released by looking"
    );

    // The owner types into it: released, and the hook of the prompt they
    // typed announces the mail.
    s.call(
        "pane.send_text",
        json!({"pane": worker, "text": "carry on"}),
    );
    let screen = s.wait_text(&worker, NOTICE);
    assert!(screen.contains("carry on"), "{screen}");
    assert_eq!(held(&s, &worker), Value::Null);
    let released = s.log("pane.released");
    assert_eq!(released.len(), 1, "{released:?}");
    assert_eq!(released[0]["data"]["how"], json!("typed"));
    assert_eq!(released[0]["data"]["stopped"], json!(true));

    // ------------------------------------------- the marks are independent
    // A paused agent, stopped; then its program is started again in the
    // pane (as after an agent update); then released.
    let second = agent(&s, "second", json!([]));
    s.wait_state(&second, "idle");
    // A restart resumes only a session the agent has named.
    wait_for("the second agent's session id", 10, || {
        s.pane(&second)["session_id"].as_str().map(String::from)
    });
    assert_eq!(
        s.call("pane.pause", json!({"pane": second, "paused": true}))["saved"],
        json!(true)
    );
    let stop = s.call("pane.stop", json!({"pane": second}));
    assert_eq!(stop["saved"], json!(true), "{stop}");
    s.call(
        "msg.send",
        json!({"to": "second", "body": "one for the second"}),
    );
    s.call("pane.restart", json!({"pane": second}));
    s.wait_text(&second, "[fake] RESUMED");
    let p = s.pane(&second);
    assert_eq!(p["paused"], json!(true), "{p}");
    assert!(p["held"]["stopped"].is_object(), "{p}");

    let out = s.call("pane.release", json!({"pane": second}));
    assert_eq!(out["released"], json!(true), "{out}");
    assert_eq!(out["still_paused"], json!(true), "{out}");
    let p = s.pane(&second);
    assert_eq!(p["paused"], json!(true), "Release unpaused a pane: {p}");
    assert_eq!(p["held"], Value::Null);
    a_nudge_would_have_come();
    assert!(!s.text(&second).contains(NUDGE), "{}", s.text(&second));
    assert!(unread(&s, "one for the second"));
    // Resume is what ends a pause: now the mail is announced.
    s.call("pane.pause", json!({"pane": second, "paused": false}));
    s.wait_text(&second, NUDGE);
    assert_eq!(s.log("msg.nudged").len(), 1);

    // ------------------------------- typed after Stop, on one connection
    let third = agent(&s, "third", json!([{"prompt": "build"}, {"work": 600_000}]));
    s.wait_state(&third, "working");
    let mut c = s.client().unwrap();
    c.call("pane.stop", json!({"pane": third})).unwrap();
    c.call(
        "pane.send_text",
        json!({"pane": third, "text": "typed after the stop"}),
    )
    .unwrap();
    let screen = s.wait_text(&third, "[fake] got: typed after the stop");
    assert!(screen.contains("[fake] interrupted"), "{screen}");
    // Typing it released the pane it had just stopped.
    assert_eq!(held(&s, &third), Value::Null);

    // ------------------------------------ every hook that can carry mail
    // Stopped before its turn began (the key waits unread while it sleeps):
    // the prompt's hook, the tool's and the end of the turn all answer a
    // held pane with no mail, and the turn is not continued.
    let fifth = s.fake(
        json!({"steps": [{"sleep": 2500}, {"prompt": "go"}, {"tool": {"name": "Bash", "command": "true"}}, {"turn_end": "finished"}]}),
        json!({"name": "fifth"}),
    );
    s.wait_text(&fifth, "[fake] session");
    s.call("pane.stop", json!({"pane": fifth}));
    s.call(
        "msg.send",
        json!({"to": "fifth", "body": "one for the fifth"}),
    );
    let screen = s.wait_text(&fifth, "[fake] turn done");
    assert!(screen.contains("[fake] ran Bash"), "{screen}");
    assert!(
        !screen.contains(NOTICE),
        "a hook gave a held pane mail: {screen}"
    );
    assert!(!screen.contains("CONTINUE"), "{screen}");
    a_nudge_would_have_come();
    assert!(!s.text(&fifth).contains(NUDGE), "{}", s.text(&fifth));
    assert!(unread(&s, "one for the fifth"));
    // Released with the button's call: the mail is announced.
    s.call("pane.release", json!({"pane": fifth}));
    s.wait_text(&fifth, NUDGE);

    // ------------------------------------------------- the switch, off
    s.call("station.flags", json!({"set": {"holds.stopped": false}}));
    let fourth = agent(&s, "fourth", json!([]));
    s.wait_state(&fourth, "idle");
    s.call("pane.stop", json!({"pane": fourth}));
    assert_eq!(held(&s, &fourth), Value::Null);
    s.call(
        "msg.send",
        json!({"to": "fourth", "body": "one for the fourth"}),
    );
    s.wait_text(&fourth, NUDGE);
}

/// REVIEW2's measured hang (a 20 KB paste into a program that was not
/// reading stopped the whole station, Kill included) and "never claim
/// confirmed": Stop cannot reach such a program, and says so.
#[cfg(unix)]
#[test]
#[ignore = "merge tier: waits out two stops that cannot be delivered (2 s each)"]
fn stop_never_claims_a_program_that_is_not_reading() {
    let s = Station::start("alpha");
    let home = s.root.join("home");
    // An agent's pane whose program reads nothing.
    let deaf = s.call(
        "pane.spawn",
        json!({"agent": "fake", "program": "/bin/sh", "args": ["-c", "sleep 600"], "name": "deaf", "cwd": home}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.call(
        "pane.report_agent",
        json!({"pane": deaf, "state": "working"}),
    );
    // One that reads, working too.
    let hears = s.fake(
        json!({"steps": [{"prompt": "x"}, {"work": 600_000}]}),
        json!({"name": "hears"}),
    );
    s.wait_state(&hears, "working");

    // 20 KB of lines, as a terminal sends a paste to a program that did
    // not ask for pastes to be marked: the terminal fills, and the rest
    // waits in the pane.
    let lines = "0123456789abcdef0123456789abcdef0123456789abcdef\r".repeat(420);
    s.call("pane.input", json!({"pane": deaf, "data": lines}));
    s.call(
        "pane.input",
        json!({"pane": deaf, "data": "typed before the stop"}),
    );

    // The station answers, at once.
    let began = std::time::Instant::now();
    assert_eq!(s.call("station.info", json!({}))["station"], json!("alpha"));
    let answered = began.elapsed();

    let e = s.try_call("pane.stop", json!({"pane": deaf})).unwrap_err();
    assert_eq!(e.code, "not_reading", "{}", e.message);
    assert!(e.message.contains("not reading input"), "{}", e.message);
    assert!(e.message.contains("use Kill"), "{}", e.message);
    // What was waiting was thrown away: the paste that was cut and the
    // keys behind it, whole.
    let log = s.log("pane.stop_not_reading");
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["data"]["discarded"], json!(2), "{log:?}");
    assert!(s.log("pane.stopped").is_empty());
    let h = held(&s, &deaf);
    assert_eq!(h["stopped"]["reached"], json!(false), "{h}");

    // "Stop every agent": the one that reads is stopped, the other is
    // listed as not reading and never as stopped.
    s.call(
        "pane.report_agent",
        json!({"pane": deaf, "state": "working"}),
    );
    let out = s.call("stop.all", json!({"scope": "station"}));
    let row = &out["stops"][0];
    assert_eq!(row["status"], json!("confirmed"), "{out}");
    assert_eq!(row["result"]["stopped"], json!([hears]), "{out}");
    assert_eq!(row["result"]["not_reading"], json!([deaf]), "{out}");
    s.wait_text(&hears, "[fake] interrupted");

    // The station still answers, and Kill works.
    assert_eq!(s.call("station.info", json!({}))["station"], json!("alpha"));
    s.call("pane.kill", json!({"pane": deaf}));
    wait_for("the program that was not reading to end", 10, || {
        s.pane(&deaf)["exited"].as_bool().filter(|e| *e)
    });
    // Said for whoever reads a failure of this test on a slow machine.
    eprintln!("station.info with 20 KB waiting in a pane: {answered:?}");
}

/// Decided by default on 15 Nov 2026 (the owner may overrule): a session
/// started again after a crash waits for the owner before it takes mail.
/// REVIEW2 blocker 3: a restored pane was continued by mail, and a paused
/// one must still be paused after the restored hold is released.
#[test]
#[ignore = "merge tier: kills and starts two stations"]
fn a_restarted_session_waits_for_the_owner() {
    let mut s = Station::start("alpha");
    let panes: Vec<String> = ["one", "two", "three"]
        .iter()
        .map(|n| agent(&s, n, json!([{"prompt": "hello"}, {"turn_end": "hi"}])))
        .collect();
    for p in &panes {
        s.wait_text(p, "[fake] turn done");
        wait_for("a session id", 10, || {
            s.pane(p)["session_id"].as_str().map(String::from)
        });
    }
    // "two" is paused, and "three" was stopped, before the crash.
    assert_eq!(
        s.call("pane.pause", json!({"pane": panes[1], "paused": true}))["saved"],
        json!(true)
    );
    assert_eq!(
        s.call("pane.stop", json!({"pane": panes[2]}))["saved"],
        json!(true)
    );

    s.kill9();
    s.launch();
    for p in &panes {
        s.wait_text(p, "[fake] RESUMED");
        let v = s.pane(p);
        assert_eq!(v["held"]["restored"]["why"], json!("crash"), "{v}");
    }
    let v = s.pane(&panes[1]);
    assert_eq!(v["paused"], json!(true), "{v}");
    let v = s.pane(&panes[2]);
    assert!(v["held"]["stopped"].is_object(), "the stop's mark: {v}");

    // The notice says they wait, how to release them, and names no button
    // an older deck lacks.
    let notice = s
        .call("notify.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == json!("station.recovered"))
        .cloned()
        .expect("a recovery notice");
    let body = notice["body"].as_str().unwrap();
    assert!(body.contains("take no mail"), "{body}");
    assert!(body.contains("type into one"), "{body}");
    assert!(body.contains("spandeck pane release --all"), "{body}");

    // Mail for each: nobody is woken.
    for n in ["one", "two", "three"] {
        s.call("msg.send", json!({"to": n, "body": format!("for {n}")}));
    }
    a_nudge_would_have_come();
    for p in &panes {
        assert!(!s.text(p).contains(NUDGE), "{}", s.text(p));
    }
    assert!(s.log("msg.nudged").is_empty());

    // One pane, from the command line.
    let cli = |args: &[&str]| {
        let out = std::process::Command::new(SPANDECK)
            .args(args)
            .env("SPANDECK_STATE_DIR", s.root.join("state"))
            .env("SPANDECK_CONFIG_DIR", s.root.join("config"))
            .env_remove("SPANDECK_TOKEN")
            .env_remove("SPANDECK_PANE")
            .env_remove("SPANDECK_SOCKET")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let said = cli(&["pane", "release", "two"]);
    assert!(said.contains("released: it takes mail again"), "{said}");
    assert!(said.contains("still paused"), "{said}");
    assert_eq!(held(&s, &panes[1]), Value::Null);

    // The rest with the one action the recovery notice carries, in the
    // Control Deck's inbox.
    {
        let page = common::browser::Page::open(&s, 1200, 800);
        let inbox = "[...document.querySelectorAll('.topbar button')].find(x => x.innerText.startsWith('Inbox'))";
        // The inbox shows what the page knew when it was opened: it is
        // opened once the page has the notice (the button counts it) and
        // the station.
        page.wait(
            "the inbox button, with the notice counted",
            &format!(
                "!!{inbox}?.querySelector('.count') && !!document.querySelector('a.tile.station')"
            ),
        );
        page.js(&format!("({inbox}.click(), true)"));
        let release = "[...document.querySelectorAll('button')].find(b => b.innerText.trim() === 'Release all on alpha')";
        page.wait(
            "Release all beside the recovery notice",
            &format!("!!{release}"),
        );
        page.js(&format!("({release}.click(), true)"));
        let toast = page.wait(
            "the deck to say how many were released",
            "(t => t && t.innerText)([...document.querySelectorAll('.toast')].find(t => t.innerText.includes('Released')))",
        );
        assert_eq!(toast, json!("Released 2 panes on alpha"));
    }
    for p in &panes {
        assert_eq!(held(&s, p), Value::Null);
    }
    // One and three take their mail; two is still paused, and does not.
    s.wait_text(&panes[0], NUDGE);
    s.wait_text(&panes[2], NUDGE);
    a_nudge_would_have_come();
    assert_eq!(s.pane(&panes[1])["paused"], json!(true));
    assert!(!s.text(&panes[1]).contains(NUDGE), "{}", s.text(&panes[1]));
    assert_eq!(s.log("station.released_all").len(), 1);

    // Release all, from the command line: "one", stopped again, and "two",
    // which stays paused.
    s.call("pane.stop", json!({"pane": panes[0]}));
    s.call("pane.stop", json!({"pane": panes[1]}));
    let said = cli(&["pane", "release", "--all"]);
    assert!(said.contains("released 2 pane(s) on alpha"), "{said}");
    assert!(said.contains("1 of them still paused"), "{said}");
    assert_eq!(s.pane(&panes[1])["paused"], json!(true));

    drop(s);

    // --------------------------------- the per-station setting, turned off
    let mut b = Station::start_with("bravo", "[flags]\nholds.restored = false\n", &[]);
    let free = agent(&b, "free", json!([{"prompt": "hello"}, {"turn_end": "hi"}]));
    b.wait_text(&free, "[fake] turn done");
    wait_for("a session id", 10, || {
        b.pane(&free)["session_id"].as_str().map(String::from)
    });
    b.kill9();
    b.launch();
    b.wait_text(&free, "[fake] RESUMED");
    assert_eq!(held(&b, &free), Value::Null);
    let body = b
        .call("notify.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == json!("station.recovered"))
        .map(|n| n["body"].as_str().unwrap_or("").to_string())
        .expect("a recovery notice");
    assert!(!body.contains("take no mail"), "{body}");
    b.call("msg.send", json!({"to": "free", "body": "for free"}));
    b.wait_text(&free, NUDGE);
}

/// What the owner sees, in the Control Deck and on a phone, and "release is
/// explicit, never by looking" with a real deck: its terminal reports
/// focus and answers the program's questions as input the moment a pane is
/// opened.
#[test]
#[ignore = "merge tier: drives two browsers"]
fn the_deck_and_the_phone_show_a_held_pane_and_release_it() {
    use common::browser::Page;
    let s = Station::start("alpha");
    // An agent that asks its terminal to report focus, as agents' screens do.
    let worker = agent(
        &s,
        "worker",
        json!([{"say": "\u{1b}[?1004h"}, {"prompt": "build"}, {"work": 600_000}]),
    );
    s.wait_state(&worker, "working");
    // Stopped before any deck is open on it: the stand-in agent takes the
    // first byte of a focus report for the interrupt key.
    s.call("pane.stop", json!({"pane": worker}));
    s.wait_text(&worker, "[fake] interrupted");
    s.call(
        "msg.send",
        json!({"to": "worker", "body": "one for the worker"}),
    );
    let page = Page::open(&s, 1200, 800);
    page.goto(&format!("#/s/alpha/p/{worker}"));
    page.terminal_face();
    let strip = "document.querySelector('.strip')";
    let said = page.wait(
        "the strip to say it is stopped, with its mail",
        &format!("(t => t.includes('mail is waiting (1)') && t)({strip}.innerText)"),
    );
    let said = said.as_str().unwrap();
    assert!(said.contains("stopped by you at "), "{said}");
    assert!(said.contains("Release"), "{said}");

    // Looked at, from a second window too, and clicked into: still held.
    let phone = Page::open(&s, 390, 844);
    phone.goto(&format!("#/s/alpha/p/{worker}"));
    let card = "[...document.querySelectorAll('.chat-card')].find(c => c.innerText.includes('stopped by you at '))";
    let words = phone.wait(
        "the phone's card for a held pane",
        &format!("(c => c && c.innerText)({card})"),
    );
    assert!(
        words.as_str().unwrap().contains("mail is waiting (1)"),
        "{words}"
    );
    page.js("document.querySelector('.xterm-helper-textarea').focus()");
    page.js("document.querySelector('.xterm-helper-textarea').blur()");
    page.js("document.querySelector('.xterm-helper-textarea').focus()");
    // The deck did send the focus report as input: the agent shows it.
    s.wait_text(&worker, "[I");
    a_nudge_would_have_come();
    assert!(
        s.pane(&worker)["held"]["stopped"].is_object(),
        "opening and focusing the pane released it"
    );
    assert!(s.log("msg.nudged").is_empty());

    // Release, on the phone: the mail is announced, and both say so.
    phone.js(&format!(
        "[...{card}.querySelectorAll('button')].find(b => b.innerText.trim() === 'Release').click()"
    ));
    // (Its screen has focus reports in the line by now: the log is asked.)
    wait_for("the mail to be announced", 10, || {
        (s.log("msg.nudged").len() == 1).then_some(())
    });
    assert_eq!(held(&s, &worker), Value::Null);
    page.wait(
        "the strip to drop the hold",
        &format!("!{strip}.innerText.includes('stopped by')"),
    );
    phone.wait("the card to go", &format!("!({card})"));

    // The pane's menu, and "Release all" in the station's menu on the phone.
    s.call("pane.stop", json!({"pane": worker}));
    page.wait(
        "the strip to say it is stopped again",
        &format!("{strip}.innerText.includes('stopped by you at ')"),
    );
    page.right_click(&format!(
        "document.querySelector('.side-pane[data-id=\"{worker}\"]')"
    ));
    page.choose("Release (take mail again)");
    wait_for("the pane's menu to release it", 10, || {
        held(&s, &worker).is_null().then_some(())
    });
    s.call("pane.stop", json!({"pane": worker}));
    phone.goto("#/fleet");
    phone.js("localStorage.setItem('spandeck.home', 'stations')");
    let by_station = "[...document.querySelectorAll('.home-switch button')].find(b => b.innerText.trim() === 'By station')";
    phone.wait("the list of sessions", &format!("!!{by_station}"));
    phone.js(&format!("{by_station}.click()"));
    phone.wait(
        "the station's row",
        "!!document.querySelector('.station-more')",
    );
    phone.js("document.querySelector('.station-more').click()");
    phone.wait("the station's menu", "!!document.querySelector('.menu')");
    phone.choose("Release all on this station");
    wait_for("Release all to release it", 10, || {
        held(&s, &worker).is_null().then_some(())
    });
    assert_eq!(s.log("station.released_all").len(), 1);

    // What was typed into a pane and thrown away by a stop is said to
    // whoever typed it. A program that reads nothing, its terminal full:
    #[cfg(unix)]
    {
        let deaf = s.call(
            "pane.spawn",
            json!({"program": "/bin/sh", "args": ["-c", "sleep 600"], "name": "deaf", "cwd": s.root.join("home")}),
        )["id"]
            .as_str()
            .unwrap()
            .to_string();
        page.goto(&format!("#/s/alpha/p/{deaf}"));
        page.wait(
            "the other pane's terminal",
            &format!("{strip}.innerText.includes('deaf') && !!document.querySelector('.xterm-helper-textarea')"),
        );
        let lines = "0123456789abcdef0123456789abcdef0123456789abcdef\r".repeat(420);
        s.call("pane.input", json!({"pane": deaf, "data": lines}));
        page.js("document.querySelector('.xterm-helper-textarea').focus()");
        page.type_str("abc");
        // The keys are behind the lines, in the pane's input, when the
        // stop comes; the stop cannot reach the program, and says so.
        std::thread::sleep(Duration::from_millis(500));
        let e = s.try_call("pane.stop", json!({"pane": deaf})).unwrap_err();
        assert_eq!(e.code, "not_reading", "{}", e.message);
        let toast = page.wait(
            "the deck to say what was not sent",
            "(t => t && t.innerText)([...document.querySelectorAll('.toast')].find(t => t.innerText.includes('Not sent')))",
        );
        assert!(
            toast.as_str().unwrap().contains("the pane was stopped"),
            "{toast}"
        );
        s.call("pane.kill", json!({"pane": deaf}));
    }
}
