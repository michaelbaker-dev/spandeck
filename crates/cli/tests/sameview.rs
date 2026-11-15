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
    let (a, b) = Station::pair();

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
    // The same from the other side.
    a.call("msg.send", json!({"to": "you", "body": "deploy is done"}));
    page.visit(&b);
    page.wait(
        "alpha's notice to be counted on bravo's deck",
        &format!("{inbox}?.querySelector('.count')?.innerText === '1'"),
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
    drop(c);
}
