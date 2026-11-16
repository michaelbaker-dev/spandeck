//! Unit 06 of the performance plan: the deck asks each station one question,
//! not seven, each station by itself, and never piles its questions up.
//! The two with a real browser take most of a minute each, so they are not
//! in the every-change check: `cargo test -p spandeck --test refresh --
//! --ignored --nocapture` (nightly runs them).
//!
//! The owner's rule (15 Nov 2026): "the slow or absent station has to be
//! fixed with the right architecture for a system like this, never to slow
//! everyone else down." Measured before: a refresh was 1 + 7 per station + 5
//! calls (34 for four stations), every 2.5 s and 120 ms after almost any
//! event, with nothing to stop one starting before the last had ended; with
//! one station frozen a round took 20 s, and every station's approvals,
//! notices and stops waited for it.
mod common;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

// The numbers differ between Linux and macOS.
const SIGSTOP: i32 = if cfg!(target_os = "macos") { 17 } else { 19 };
const SIGCONT: i32 = if cfg!(target_os = "macos") { 19 } else { 18 };

/// alpha and bravo as they are; charlie with the switch off, which is how a
/// station from before `deck.snapshot` (0.4.5 and earlier) answers.
fn fleet_with_one_from_before() -> Vec<Station> {
    let stations = Station::fleet(&["alpha", "bravo", "charlie"], |name, _| {
        let extra = if name == "charlie" {
            "flags.deck_snapshot = false\n"
        } else {
            ""
        };
        (extra.to_string(), vec![])
    });
    for s in &stations {
        wait_for("the three stations to find each other", 30, || {
            let v = s.call("fleet.refresh", json!({}));
            let on = |x: &&Value| x["state"] == json!("online");
            (v["stations"].as_array().unwrap().iter().filter(on).count() == 3).then_some(())
        });
    }
    stations
}

/// A station with something in every list a deck shows of it: an agent
/// that asks for a permission (a pane, an approval, a notice), a message,
/// and a stop.
fn fill(s: &Station, name: &str) -> String {
    // The stop first: it is for what is running when it is issued.
    s.call("stop.all", json!({"scope": "station"}));
    let pane = s.fake(
        json!({"steps": [{"prompt": "x"}, {"permission": {"name": "Write", "file": "f"}}, {"sleep": 600_000}]}),
        json!({"name": name}),
    );
    s.wait_state(&pane, "needs_you");
    s.call(
        "msg.send",
        json!({"to": "you", "body": format!("from {name}")}),
    );
    pane
}

/// Without the fields that change with the clock alone, which a snapshot's
/// versions pass over too.
fn steady(mut v: Value) -> Value {
    for row in v.as_array_mut().into_iter().flatten() {
        if let Some(o) = row.as_object_mut() {
            o.remove("last_output_ms");
            o.remove("age_ms");
        }
    }
    v
}

/// Done when: one answer says exactly what the seven lists say; a deck that
/// holds it is sent nothing again, and only what changed after a change; an
/// agent cannot ask it; and a station from before answers "unknown" and
/// does not announce it, so a deck asks that one the old way.
#[test]
fn one_answer_says_what_the_seven_lists_say() {
    let stations = fleet_with_one_from_before();
    let (a, c) = (&stations[0], &stations[2]);
    let pane = fill(a, "asker");

    let lists = [
        ("panes", "pane.list", json!({})),
        ("projects", "project.list", json!({})),
        ("tasks", "task.list", json!({"active": true, "limit": 100})),
        ("msgs", "msg.recent", json!({"limit": 60})),
        ("approvals", "approval.list", json!({})),
        ("notes", "notify.list", json!({"limit": 60})),
        ("stops", "stop.list", json!({})),
        ("trust", "trust.status", json!({})),
        ("deck", "deck.get", json!({})),
        ("look", "look.get", json!({})),
        ("prefs", "prefs.get", json!({})),
    ];
    // The agent is waiting for its answer, so nothing is changing.
    let whole = a.call("deck.snapshot", json!({"docs": true}));
    for (section, method, params) in &lists {
        assert_eq!(
            steady(whole[*section].clone()),
            steady(a.call(method, params.clone())),
            "{section} is what {method} says"
        );
    }
    assert!(
        whole["panes"].as_array().is_some_and(|p| p.len() == 1)
            && whole["approvals"].as_array().is_some_and(|p| p.len() == 1)
            && whole["notes"].as_array().is_some_and(|p| !p.is_empty())
            && whole["msgs"].as_array().is_some_and(|p| !p.is_empty()),
        "{whole}"
    );
    // Without `docs`, the seven only.
    let seven = a.call("deck.snapshot", json!({}));
    assert!(seven.get("deck").is_none() && seven.get("trust").is_none());
    // Holding all of it: nothing is sent again.
    let have = whole["v"].clone();
    let again = a.call("deck.snapshot", json!({"docs": true, "have": have}));
    for (section, _, _) in &lists {
        assert!(
            again[*section].is_null(),
            "{section} had not changed: {again}"
        );
    }
    // The approval is answered: the approvals changed, and the pane, and
    // (its notice aside) nothing else.
    let id = whole["approvals"][0]["id"].clone();
    a.call("approval.answer", json!({"id": id, "allow": true}));
    a.wait_text(&pane, "[fake] permission: allow");
    let after = wait_for("the change to show", 10, || {
        let v = a.call("deck.snapshot", json!({"docs": true, "have": have}));
        (v["approvals"] == json!([]) && !v["panes"].is_null()).then_some(v)
    });
    for section in [
        "projects", "tasks", "stops", "trust", "deck", "look", "prefs",
    ] {
        assert!(
            after[section].is_null(),
            "{section} had not changed: {after}"
        );
    }
    assert_ne!(after["v"]["approvals"], have["approvals"]);

    // Rule 7: an agent has no such call; a read-only caller has (it is the
    // lists it could already read).
    let refused = a
        .call_as_pane(&pane, "deck.snapshot", json!({}))
        .unwrap_err();
    assert_eq!(refused.code, "denied", "{}", refused.message);

    // A station from before: it does not say it can, and answers as to any
    // method it does not know. Through another station the words are the
    // same, which is what the deck goes by if it asked after all.
    let caps = |s: &Station| s.call("station.info", json!({}))["caps"].clone();
    assert!(caps(a)
        .as_array()
        .unwrap()
        .contains(&json!("deck_snapshot")));
    assert!(!caps(c)
        .as_array()
        .unwrap()
        .contains(&json!("deck_snapshot")));
    let unknown = a
        .try_call(
            "peer.call",
            json!({"station": "charlie", "method": "deck.snapshot", "params": {}}),
        )
        .unwrap_err();
    assert!(
        unknown.message.contains("method deck.snapshot"),
        "{}",
        unknown.message
    );
    // And through a station that can, the answer is that station's.
    let far = a.call(
        "peer.call",
        json!({"station": "bravo", "method": "deck.snapshot", "params": {}}),
    );
    assert_eq!(
        (far["station"].clone(), far["panes"].clone()),
        (json!("bravo"), json!([]))
    );
}

/// Done when, in a real browser on alpha's deck: every list is filled from
/// a station from before (asked the seven) as from the others; with bravo
/// frozen, a pane started on alpha and one on charlie are on the page as
/// soon as when nothing is frozen, bravo is said to be not answering (or
/// unreachable, once the fleet's look has found it so) and is never shown
/// as if up to date, and the page's questions to it do not pile up; when
/// bravo answers again the page shows what happened there; and the check
/// has found nothing to correct.
#[test]
#[ignore = "merge tier: starts three daemons and a browser, and freezes a station"]
fn the_page_stays_right_with_a_station_frozen_and_one_from_before() {
    use common::browser::Page;
    let stations = fleet_with_one_from_before();
    let (a, b, c) = (&stations[0], &stations[1], &stations[2]);
    fill(a, "ada");
    fill(b, "bob");
    fill(c, "cy");

    let page = Page::open(a, 1300, 900);
    let tile = |st: &str| {
        format!("[...document.querySelectorAll('.tile.station')].find(t => t.querySelector('b')?.innerText === '{st}')?.innerText")
    };
    let has = |st: &str, text: &str| format!("({} || '').includes({})", tile(st), json!(text));
    // Every station's lists are on the page, the one from before included:
    // its pane, its approval, its notice (three in the inbox's count, one
    // from each), its stop.
    for (st, name) in [("alpha", "ada"), ("bravo", "bob"), ("charlie", "cy")] {
        page.wait(&format!("{st}'s agent on its tile"), &has(st, name));
        page.wait(
            &format!("{st}'s stop"),
            &format!("[...document.querySelectorAll('.stops .stop')].some(x => x.title.includes('at {st}'))"),
        );
    }
    let count = |button: &str| {
        format!("Number([...document.querySelectorAll('.topbar button')].find(x => x.innerText.startsWith('{button}'))?.querySelector('.count')?.innerText || 0)")
    };
    page.wait(
        "an approval from each station to be counted",
        &format!("{} === 3", count("Approvals")),
    );
    page.wait(
        "a notice from each station to be counted",
        &format!("{} >= 3", count("Inbox")),
    );
    let number = |key: &str| {
        page.js(&format!(
            "Number(document.getElementById('app').dataset.{key} || 0)"
        ))
        .as_u64()
        .unwrap()
    };
    // How soon something started on a station is on the page.
    let shown = |s: &Station, st: &str, name: &str| -> f64 {
        let began = Instant::now();
        s.fake(
            json!({"steps": [{"prompt": "x"}, {"sleep": 600_000}]}),
            json!({"name": name}),
        );
        page.wait(&format!("{name} on {st}'s tile"), &has(st, name));
        began.elapsed().as_secs_f64()
    };
    let healthy = [shown(a, "alpha", "ada2"), shown(c, "charlie", "cy2")];
    // An idle deck, all healthy: a round is the list of stations, one
    // question to alpha and bravo each, and seven to the one from before.
    let (asked0, at0) = (number("asked"), Instant::now());
    std::thread::sleep(Duration::from_secs(10));
    let idle = (number("asked") - asked0) as f64 / at0.elapsed().as_secs_f64() * 2.5;

    b.signal(SIGSTOP);
    b.wait_frozen();
    let frozen = [shown(a, "alpha", "ada3"), shown(c, "charlie", "cy3")];
    // bravo is marked: as not answering, with what is shown of it still
    // there and said to be from before; or, once the look at the fleet has
    // found it away, as unreachable.
    let marked = page.wait(
        "bravo to be marked",
        &format!(
            "(t => /not answering: as of|Unreachable/.test(t || '') ? t : null)({})",
            tile("bravo")
        ),
    );
    let marked = marked.as_str().unwrap().to_string();
    if marked.contains("not answering") {
        assert!(marked.contains("bob"), "shown from before: {marked}");
    }
    // The others' approvals, notices and stops did not wait for bravo:
    // a new approval on charlie is on the page while bravo is frozen.
    let began = Instant::now();
    c.fake(
        json!({"steps": [{"prompt": "x"}, {"permission": {"name": "Bash", "command": "make"}}, {"sleep": 600_000}]}),
        json!({"name": "cy4"}),
    );
    page.wait(
        "charlie's new approval, with bravo frozen",
        &format!("{} === 4", count("Approvals")),
    );
    let approval = began.elapsed().as_secs_f64();
    // Ten more seconds of it: the page's questions do not pile up.
    let (asked1, at1) = (number("asked"), Instant::now());
    std::thread::sleep(Duration::from_secs(10));
    let during = (number("asked") - asked1) as f64 / at1.elapsed().as_secs_f64() * 2.5;
    let perf = a.call("perf.get", json!({}));
    let refused = perf["requests"]["waited"]["busy"]["last_5m"]
        .as_u64()
        .unwrap_or(0);

    b.signal(SIGCONT);
    let thawed = shown(b, "bravo", "bob2");
    page.wait(
        "bravo's mark to go",
        &format!(
            "(t => t && !/not answering|Unreachable/.test(t))({})",
            tile("bravo")
        ),
    );
    // Two full checks later, nothing was found to correct.
    std::thread::sleep(Duration::from_secs(21));
    let corrections = number("corrections");
    println!(
        "on the page after being started: healthy {:.2} s and {:.2} s; with bravo frozen {:.2} s and {:.2} s; an approval on charlie {:.2} s; on bravo once it answered {:.2} s | questions a round: idle {idle:.1}, with bravo frozen {during:.1} | refused for piling up: {refused} | corrections: {corrections} | bravo's tile while frozen: {:?}",
        healthy[0], healthy[1], frozen[0], frozen[1], approval, thawed, marked.replace('\n', " / "),
    );
    // A pane is on the page after an event (alpha) or at the next round
    // (2.5 s; another station has no events here yet): with a station
    // frozen, as when none is. Before, the lists waited their turn behind
    // the frozen station's five seconds.
    assert!(frozen[0] < 2.0, "{frozen:?}");
    assert!(frozen[1] < 4.0 && approval < 4.5, "{frozen:?} {approval}");
    // 1 + 1 + 1 + 7 = 10 a round, and what events ask; before, with three
    // stations, 27. No more with a station frozen, and none refused.
    assert!(idle < 13.0 && during < 13.0, "{idle} {during}");
    assert_eq!(refused, 0, "{}", perf["requests"]["waited"]);
    assert_eq!(corrections, 0);
}

/// Nightly: what an idle deck asks. Four stations, twenty panes, a real
/// browser. Stored baseline (0.4.5, measured 15 Nov 2026): 34 calls a round,
/// 816 a minute. The brief's bound: at least five times fewer.
#[test]
#[ignore = "nightly: four daemons and a browser for half a minute, and it asserts on a measured count"]
fn an_idle_deck_asks_five_times_less_than_it_did() {
    use common::browser::Page;
    const BEFORE_A_MINUTE: f64 = 816.0;
    let stations = Station::fleet(&["alpha", "bravo", "charlie", "delta"], |_, _| {
        (String::new(), vec![])
    });
    for s in &stations {
        wait_for("the four stations to find each other", 30, || {
            let v = s.call("fleet.refresh", json!({}));
            let on = |x: &&Value| x["state"] == json!("online");
            (v["stations"].as_array().unwrap().iter().filter(on).count() == 4).then_some(())
        });
        for i in 0..5 {
            s.call(
                "pane.spawn",
                json!({"agent": "shell", "name": format!("p{i}")}),
            );
        }
    }
    let page = Page::open(&stations[0], 1300, 900);
    page.wait(
        "every station's panes in the list",
        "document.querySelectorAll('.sidebar .side-pane').length >= 20",
    );
    // Let the shells finish starting: an idle deck is what is counted.
    std::thread::sleep(Duration::from_secs(5));
    let number = |key: &str| {
        page.js(&format!(
            "Number(document.getElementById('app').dataset.{key} || 0)"
        ))
        .as_u64()
        .unwrap()
    };
    let (asked, began) = (number("asked"), Instant::now());
    std::thread::sleep(Duration::from_secs(30));
    let a_minute = (number("asked") - asked) as f64 / began.elapsed().as_secs_f64() * 60.0;
    println!(
        "an idle deck, four stations, twenty panes: {a_minute:.0} questions a minute ({:.1} a round); before: {BEFORE_A_MINUTE:.0} (34 a round); corrections: {}",
        a_minute / 24.0,
        number("corrections")
    );
    assert!(a_minute * 5.0 <= BEFORE_A_MINUTE, "{a_minute}");
    assert_eq!(number("corrections"), 0);
}
