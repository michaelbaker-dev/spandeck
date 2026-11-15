//! The same view from every station (the owner's requirement of 15 Nov 2026):
//! "no matter where I go, it's all the same view from spandeck". Each test
//! here is one of the faults an audit found in a fleet whose stations are all
//! joined and online; they share stations because starting one costs time.
mod common;
use std::sync::{Arc, Mutex};

use common::*;
use serde_json::{json, Value};

/// Everything a Control Deck connected to this station is sent from here on.
fn events_of(s: &Station) -> Arc<Mutex<Vec<Value>>> {
    let events = Arc::new(Mutex::new(vec![]));
    let (mut c, sink) = (s.client().unwrap(), events.clone());
    c.subscribe(None).unwrap();
    std::thread::spawn(move || {
        while let Ok(ev) = c.next_event() {
            sink.lock().unwrap().push(ev);
        }
    });
    events
}

fn event(events: &Arc<Mutex<Vec<Value>>>, kind: &str, holds: &str) -> Option<Value> {
    let found = events.lock().unwrap().iter().find_map(|e| {
        let e = e.get("event").unwrap_or(e);
        (e["kind"] == json!(kind) && e.to_string().contains(holds)).then(|| e.clone())
    });
    found
}

/// The part that is the Control Deck's: the same things are on screen
/// whichever station serves the page.
#[test]
fn a_deck_opened_on_either_station_shows_the_same() {
    use common::browser::Page;
    // Alpha forgets after a second that the owner was at a keyboard.
    let (a, b) = Station::pair_with("at_keyboard_secs = 1\n");

    // ---- Fault 1: the inbox was the serving station's only. A notice and
    // a message kept at bravo are in the inbox of a deck served by alpha,
    // each with the station it is from, and are marked read where they are.
    b.call("msg.send", json!({"to": "you", "body": "tests are green"}));
    let page = Page::open(&a, 1300, 800);
    let inbox = "[...document.querySelectorAll('.topbar button')].find(x => x.innerText.startsWith('Inbox'))";
    page.wait(
        "bravo's notice to be counted on alpha's deck",
        &format!("{inbox}?.querySelector('.count')?.innerText === '1'"),
    );
    page.js(&format!("({inbox}.click(), true)"));
    let said = page.wait(
        "the inbox",
        "document.querySelector('[role=dialog][aria-label=Inbox]')?.innerText",
    );
    let said = said.as_str().unwrap();
    assert!(
        said.contains("Message from you") && said.contains("tests are green"),
        "{said}"
    );
    assert!(
        said.matches("bravo").count() >= 2,
        "the message and the notice name bravo: {said}"
    );
    page.js("([...document.querySelectorAll('[role=dialog] button')].find(x => x.innerText === 'Mark all read').click(), true)");
    wait_for(
        "the notice and the message to be marked read at bravo",
        10,
        || {
            let unread = |m: &str, p: Value| {
                b.call(m, p)
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|n| n["read"].is_null() || n["read"] == json!(false))
            };
            (!unread("notify.list", json!({}))
                && !unread("msg.inbox", json!({"all": true, "peek": true})))
            .then_some(())
        },
    );
    page.wait(
        "the count to go",
        &format!("{inbox} && !{inbox}.querySelector('.count')"),
    );

    // ---- Fault 4: a leaf of a global layout that named no station meant
    // "the station serving this deck", so one layout showed bravo's pane on
    // bravo and nothing on alpha. Such a leaf is given the station its pane
    // is found on, once, and the layout then shows that pane everywhere.
    let scratch = b.call("pane.spawn", json!({"agent": "shell", "name": "scratch"}));
    let scratch = scratch["id"].as_str().unwrap();
    a.call(
        "deck.layout.save",
        json!({"layout": {"name": "all", "tree": {"id": scratch}}}),
    );
    wait_for("the leaf to be given bravo, where its pane is", 15, || {
        let tree = a.call("deck.get", json!({}))["layouts"][0]["tree"].clone();
        (tree == json!({"id": scratch, "station": "bravo"})).then_some(())
    });
    let shown = "[...document.querySelectorAll('.station-main .pane')].map(p => p.dataset.pane)";
    page.goto("#/l/all");
    let on_alpha = page.wait(
        "the layout on alpha's deck",
        &format!("(p => p.length ? p : null)({shown})"),
    );
    assert_eq!(on_alpha, json!([scratch]));

    // The same from the other side: the inbox, and the layout.
    a.call("msg.send", json!({"to": "you", "body": "deploy is done"}));
    page.visit(&b);
    page.wait(
        "alpha's notice to be counted on bravo's deck",
        &format!("{inbox}?.querySelector('.count')?.innerText === '1'"),
    );
    page.goto("#/l/all");
    let on_bravo = page.wait(
        "the layout on bravo's deck",
        &format!("(p => p.length ? p : null)({shown})"),
    );
    assert_eq!(on_bravo, on_alpha);

    // ---- Fault 2: the owner at a deck counted as "at a keyboard" for the
    // station serving it only, so the others pushed to the phone meanwhile.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let at_keyboard = || a.call("station.info", json!({}))["at_keyboard"].clone();
    assert_eq!(at_keyboard(), json!(false), "nobody has been at alpha");
    page.js("(document.body.dispatchEvent(new MouseEvent('mousedown', {bubbles: true})), true)");
    wait_for(
        "the owner at bravo's deck to count at alpha too",
        10,
        || (at_keyboard() == json!(true)).then_some(()),
    );
}

#[test]
fn every_station_of_a_fleet_shows_the_same() {
    let (a, b, c) = Station::three();

    // ---- Fault 1: a notice raised at one station reached only the decks
    // that station served. A deck on alpha is told what bravo raised, with
    // the station it is from; the notice stays in bravo's inbox, where it is
    // marked read, and the deck on alpha hears of that too.
    let at_alpha = events_of(&a);
    b.call("msg.send", json!({"to": "you", "body": "tests are green"}));
    let told = wait_for("alpha's deck to hear of bravo's notice", 10, || {
        event(&at_alpha, "notify.new", "tests are green")
    });
    assert_eq!(told["data"]["station"], json!("bravo"), "{told}");
    let held = b.call("notify.list", json!({}));
    assert_eq!(
        told["data"]["id"], held[0]["id"],
        "the id bravo holds it under"
    );
    assert_eq!(
        a.call("notify.list", json!({})),
        json!([]),
        "kept at bravo only"
    );
    b.call("notify.read", json!({"id": held[0]["id"]}));
    wait_for("alpha's deck to hear that it was read", 10, || {
        event(&at_alpha, "notify.read", "bravo")
    });
    // Only a station says so: the owner's call, an agent's and another
    // station's text are not a way to put words in a deck.
    let refused = a
        .try_call("notify.relay", json!({"title": "x"}))
        .unwrap_err();
    assert_eq!(refused.code, "denied", "{}", refused.message);

    // ---- Fault 3: a station that had just come back changed its stale
    // copy of a synced document, stamped it the newest, and every station
    // then took it: what was changed while it was away was gone.
    let (mut a, mut c) = (a, c);
    let layouts = |s: &Station| -> Vec<String> {
        let deck = s.call("deck.get", json!({}));
        let names = deck["layouts"].as_array().unwrap().iter();
        names
            .map(|l| l["name"].as_str().unwrap().to_string())
            .collect()
    };
    let themes = |s: &Station| -> Vec<String> {
        let look = s.call("look.get", json!({}));
        let names = look["themes"].as_array().unwrap().iter();
        names
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    let model = |s: &Station| {
        s.call("setup.get", json!({}))["setup"]["defaults"]["claude"]["model"].clone()
    };
    let own_look = |s: &Station, key: &str| !s.call("look.get", json!({}))["panes"][key].is_null();
    // An agent's pane is still there after its station was off; a shell's is not.
    let scratch = c.fake(json!({"steps": []}), json!({}));
    wait_for("the agent's session", 10, || {
        c.pane(&scratch)["session_id"].as_str().map(String::from)
    });
    let scratch_look = format!("charlie/{scratch}");
    a.call("deck.layout.save", json!({"layout": {"name": "one"}}));
    a.call(
        "look.set",
        json!({"level": "pane", "key": scratch_look, "choice": {"set": {"font.size": 15}}}),
    );
    wait_for("charlie to have the layout and the pane's look", 10, || {
        (layouts(&c) == ["one"] && own_look(&c, &scratch_look)).then_some(())
    });
    // Charlie is off while the owner works at alpha.
    c.kill9();
    a.call("deck.layout.save", json!({"layout": {"name": "two"}}));
    a.call("look.theme.save", json!({"theme": {"name": "Later"}}));
    a.call(
        "setup.set",
        json!({"setup": {"defaults": {"claude": {"model": "opus"}}}}),
    );
    // It is back, and at once: a pane is closed there, which changes its
    // copy of the looks without the owner asking (3b), and the owner pins
    // something and saves a theme there (3a).
    c.launch();
    c.call("pane.close", json!({"pane": scratch}));
    assert!(
        !own_look(&c, &scratch_look),
        "the closed pane's look is gone there"
    );
    c.call(
        "deck.pin",
        json!({"pin": {"kind": "layout", "name": "one"}}),
    );
    c.call("look.theme.save", json!({"theme": {"name": "Mine"}}));
    for s in [&a, &b, &c] {
        wait_for_or(
            "every station to hold what was changed at alpha and at charlie",
            20,
            || format!("{}: {:?} {:?} {}", s.name, layouts(s), themes(s), model(s)),
            || {
                let pins = s.call("deck.get", json!({}))["pins"]
                    .as_array()
                    .unwrap()
                    .len();
                (layouts(s) == ["one", "two"]
                    && pins == 1
                    && themes(s) == ["Later", "Mine"]
                    && model(s) == json!("opus"))
                .then_some(())
            },
        );
    }
    // 3d: an editor that opened the setup before it was changed elsewhere
    // is told its save was not kept, never "Saved".
    let stale = b
        .try_call(
            "setup.set",
            json!({"setup": {"updated_ms": 1, "defaults": {"claude": {"model": "haiku"}}}}),
        )
        .unwrap_err();
    assert!(stale.message.starts_with("not saved"), "{}", stale.message);
    assert_eq!(model(&a), json!("opus"));

    // What is left of the fault: the same document changed at two stations
    // that could not reach each other. The later change wins whole, and the
    // owner is told which station's change went, at every station.
    drop(b);
    c.kill9();
    a.call("deck.layout.save", json!({"layout": {"name": "at-alpha"}}));
    a.kill9();
    c.launch();
    c.call(
        "deck.layout.save",
        json!({"layout": {"name": "at-charlie"}}),
    );
    a.launch();
    for s in [&a, &c] {
        wait_for("the later change to be the one both hold", 20, || {
            (layouts(s) == ["one", "two", "at-charlie"]).then_some(())
        });
    }
    let said = wait_for("alpha to say that its change was not kept", 10, || {
        let notes = a.call("notify.list", json!({}));
        let lost = notes.as_array().unwrap().iter();
        lost.map(|n| n["title"].as_str().unwrap().to_string())
            .find(|t| t.contains("was not kept"))
    });
    assert_eq!(
        said,
        "A change to the global layouts and pins made at alpha was not kept"
    );
}
