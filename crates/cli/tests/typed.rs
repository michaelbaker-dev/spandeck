//! What is typed into a pane from a Control Deck (DECISIONS.md, "A key can
//! never reach another pane" and "What is typed"; units 03b and 03c of the
//! performance and continuity plan). Scenario tests with real stations
//! (merge tier), and a real page in a browser with measured times
//! (nightly). Run with `--ignored`.
//!
//! The audit (FAILURES, gaps 13 and 3, measured on 0.4.2): channel numbers
//! restarted on every connection and a page kept a terminal's old number
//! until its new attach was answered, so a key carrying pane B's old number
//! was written to pane C; keys typed during a gap were dropped silently
//! (about 0.8 s per upgrade or reconnect) or delivered much later in one
//! burst (232 keys up to 60 s late, 2,996 up to ten minutes late through a
//! gateway); and in a two-pane layout the terminal lost the keyboard after a
//! reconnect.

// The panes are `/bin/sh`.
#![cfg(unix)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::wire::Deck;
use common::*;
use serde_json::{json, Value};

/// Two page loads' instances, as a page makes one (16 hex digits).
const PAGE: &str = "0123456789abcdef";
const DUPLICATE: &str = "fedcba9876543210";

fn shell(s: &Station, name: &str) -> String {
    s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": name, "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Something that waits on a station, kept off the runtime's threads.
async fn off<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

/// A page of today: it names its attachments and says which page load it is.
fn page(client: &str, instance: &str) -> Value {
    json!({"client": client, "instance": instance, "caps": ["busy", "names"]})
}

/// Attach under the name the page chose.
async fn attach(deck: &Deck, mut params: Value, name: u64) -> Result<u64, Value> {
    params["chan"] = json!(name);
    deck.attach(params).await
}

async fn closed(deck: &Deck) -> bool {
    tokio::time::sleep(Duration::from_millis(300)).await;
    deck.seen.lock().unwrap().closed
}

/// Unit 03b: the audit's gap 13, closed by construction, and the second
/// review's blocker 6. Done when:
/// - a pane is attached under the name the page chose, a name is one
///   attachment's, and a page that names its panes is given no pane under a
///   number it did not choose;
/// - a duplicated tab (the same client id, its own instance) and its
///   original both stay connected, and both are sent the pane's output;
/// - the same page come back (the same client id and instance) closes its
///   earlier connection and no other, and on the new connection a key that
///   carries a name from the old one reaches no pane, whatever is attached;
/// - through a gateway, one tab's two panes are both kept, the same tab
///   attaching a pane again closes its own earlier connection for that pane
///   at the pane's station, and another page's connection for the same pane
///   is left alone;
/// - a page from before (no instance, no names) is never closed by any of
///   this and is given the station's numbers, as before.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts two daemons"]
async fn a_page_that_comes_back_replaces_only_itself() {
    let (a, b) = off(Station::pair).await;
    let (a, b) = (Arc::new(a), Arc::new(b));
    let (here, other) = (shell(&a, "here"), shell(&a, "other"));
    let (bee, sea) = (shell(&b, "bee"), shell(&b, "sea"));

    // ---- names
    let first = Deck::open_as(&a, a.port, page("web-1-aa", PAGE)).await;
    let caps = first.hello["auth"]["caps"].to_string();
    assert!(caps.contains("\"names\""), "{caps}");
    assert_eq!(attach(&first, json!({"pane": here}), 1000).await, Ok(1000));
    let twice = attach(&first, json!({"pane": other}), 1000).await;
    assert!(
        twice
            .as_ref()
            .is_err_and(|e| e["message"].as_str().unwrap().contains("already attached")),
        "{twice:?}"
    );
    let unnamed = first.attach(json!({"pane": other})).await;
    assert!(
        unnamed.as_ref().is_err_and(|e| e["message"]
            .as_str()
            .unwrap()
            .contains("names its attachments")),
        "{unnamed:?}"
    );
    for bad in [json!(0), json!(-4), json!("7"), json!(1u64 << 32)] {
        let mut params = json!({"pane": other});
        params["chan"] = bad.clone();
        assert!(first.attach(params).await.is_err(), "{bad}");
    }
    first.key(1000, "echo named-$((20+3))\r").await;
    first.shows(1000, "named-23").await;

    // ---- a duplicated tab: both stay, and both are sent the output
    let dup = Deck::open_as(&a, a.port, page("web-1-aa", DUPLICATE)).await;
    assert_eq!(attach(&dup, json!({"pane": here}), 2000).await, Ok(2000));
    for round in 0..4 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (deck, chan) = if round % 2 == 0 {
            (&first, 1000)
        } else {
            (&dup, 2000)
        };
        deck.key(chan, &format!("echo both-$(({round}+50))\r"))
            .await;
        first.shows(1000, &format!("both-{}", round + 50)).await;
        dup.shows(2000, &format!("both-{}", round + 50)).await;
    }
    assert!(!closed(&first).await && !closed(&dup).await);

    // ---- the same page come back: its earlier connection is closed, the
    // duplicate's is not
    let again = Deck::open_as(&a, a.port, page("web-1-aa", PAGE)).await;
    first
        .until("the earlier connection to be closed", 5, |seen| {
            seen.closed.then_some(())
        })
        .await;
    assert!(!closed(&dup).await && !closed(&again).await);
    // Reconnect, then type with the terminal's old name: before anything
    // is attached, and after another pane is. It names nothing.
    again.key(1000, "echo stray-$((3+4))\r").await;
    assert_eq!(attach(&again, json!({"pane": other}), 1001).await, Ok(1001));
    again.key(1000, "echo stray-$((3+4))\r").await;
    let id = again
        .ask(
            "pane.input",
            json!({"chan": 1000, "data": "echo stray-7\r"}),
        )
        .await;
    let refused = again.answer(id).await.unwrap_err();
    assert_eq!(refused["message"], json!("no such channel"));
    again.key(1001, "echo other-$((30+1))\r").await;
    again.shows(1001, "other-31").await;
    let (sa, h, o) = (a.clone(), here.clone(), other.clone());
    let texts = off(move || format!("{}\n{}", sa.text(&h), sa.text(&o))).await;
    assert!(!texts.contains("stray"), "{texts}");
    assert!(
        a.call("perf.get", json!({}))["connections"]["replaced"]["since_start"]
            .as_u64()
            .is_some_and(|n| n >= 1)
    );

    // ---- through a gateway: one tab's two panes, both kept
    let on_bravo = |pane: &str| json!({"pane": pane, "station": "bravo"});
    assert_eq!(attach(&again, on_bravo(&bee), 3000).await, Ok(3000));
    assert_eq!(attach(&again, on_bravo(&sea), 3001).await, Ok(3001));
    again.key(3000, "echo bee-$((1+1))\r").await;
    again.key(3001, "echo sea-$((2+2))\r").await;
    again.shows(3000, "bee-2").await;
    again.shows(3001, "sea-4").await;
    // Another page (the duplicate) showing the same pane through the same
    // station is another connection there, and is kept too.
    assert_eq!(attach(&dup, on_bravo(&bee), 2001).await, Ok(2001));
    // The same tab attaches the pane again: at bravo its earlier
    // connection for that pane is closed, which this station passes on.
    assert_eq!(attach(&again, on_bravo(&bee), 3002).await, Ok(3002));
    again
        .until("the earlier attachment to be let go of", 10, |seen| {
            common::wire::unreachable_in(seen, 3000)
        })
        .await;
    again.key(3002, "echo bee-again-$((3+3))\r").await;
    again.key(3001, "echo sea-still-$((4+4))\r").await;
    again.shows(3002, "bee-again-6").await;
    again.shows(3001, "sea-still-8").await;
    dup.shows(2001, "bee-again-6").await;
    assert!(again.unreachable(3001).is_none() && again.unreachable(3002).is_none());
    assert!(dup.unreachable(2001).is_none());

    // ---- a page from before: no instance, no names
    let old = Deck::open(&a, a.port, "web-9-zz").await;
    let old_again = Deck::open(&a, a.port, "web-9-zz").await;
    let given = old.attach(json!({"pane": here})).await.unwrap();
    assert!(given > 0);
    assert!(!closed(&old).await && !closed(&old_again).await);
    old.key(given, "echo old-$((5+5))\r").await;
    old.shows(given, "old-10").await;

    drop((first, dup, again, old, old_again));
    off(move || drop((a, b))).await;
}

/// The same with the switch off, which is a station from before: it does
/// not say `names`, gives its own numbers whatever the page asks for, and
/// closes nothing when the same page signs in twice.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts a daemon"]
async fn with_the_switch_off_names_and_fencing_are_as_before() {
    let a = off(|| Station::start_with("alpha", "[flags]\ndeck_names = false\n", &[])).await;
    let a = Arc::new(a);
    let here = shell(&a, "here");
    let first = Deck::open_as(&a, a.port, page("web-1-aa", PAGE)).await;
    let caps = first.hello["auth"]["caps"].to_string();
    assert!(!caps.contains("\"names\""), "{caps}");
    let given = attach(&first, json!({"pane": here}), 1000).await.unwrap();
    assert_ne!(given, 1000);
    let again = Deck::open_as(&a, a.port, page("web-1-aa", PAGE)).await;
    assert!(!closed(&first).await && !closed(&again).await);
    // The page uses the number the answer gives, as it always did.
    first.key(given, "echo plain-$((6+6))\r").await;
    first.shows(given, "plain-12").await;
    drop((first, again));
    off(move || drop(a)).await;
}

// ------------------------------------------------------------------ the page

/// The page's `Api` (a test reads it; the deck does not).
const API: &str = "window.api = () => document.getElementById('app').api";

/// Type into the pane on the page, as a keyboard does.
fn type_in(page: &browser::Page, text: &str) {
    page.js(&format!(
        "(t => {{ t.term.focus(); t.term.input({}); return true; }})(document.querySelector('.pane').paneTerm)",
        json!(text)
    ));
}

fn attached(page: &browser::Page) {
    page.wait("the pane", "!!document.querySelector('.pane .xterm-rows')");
    page.wait(
        "the pane to be attached",
        "document.querySelector('.pane').paneTerm.chan !== null",
    );
}

/// The second review's blocker 6, in a real browser: a duplicated tab
/// copies `sessionStorage`, and with it the tab's part of the client id.
/// Done when: the copy finds out and makes itself a client id of its own;
/// the original's connection is never closed by it; both show the pane and
/// what is typed in each arrives; and both are still connected, with no
/// reconnecting back and forth, seconds later.
#[test]
#[ignore = "nightly: a daemon and a browser with two tabs"]
fn a_duplicated_tab_and_its_original_both_work() {
    let a = Station::start("alpha");
    let pane = shell(&a, "here");
    let page = browser::Page::open(&a, 1000, 700);
    page.js(API);
    page.goto(&format!("#/s/alpha/p/{pane}"));
    attached(&page);
    let original = page.js("api().client");
    // What "Duplicate tab" does: the new tab starts with a copy of this
    // one's session storage.
    page.js("(window.open(location.href, '_blank'), true)");
    let dup = page.opened("#/s/alpha/p/");
    dup.wait("the copy's deck", "!!document.querySelector('.topbar')");
    dup.js(API);
    dup.wait("the copy to be a client of its own", "api().renamed === 1");
    attached(&dup);
    let copy = dup.js("api().client");
    assert_ne!(original, copy);
    assert_eq!(page.js("api().client"), original);
    type_in(&page, "echo first-$((1+1))\n");
    a.wait_text(&pane, "first-2");
    // The copy took the pane's size, as any second client does: typing in
    // it works, and the original still shows what it prints.
    type_in(&dup, "echo copy-$((2+2))\n");
    a.wait_text(&pane, "copy-4");
    wait_for("the original to show it too", 10, || {
        page.term_text().contains("copy-4").then_some(())
    });
    std::thread::sleep(Duration::from_secs(4));
    let state = |p: &browser::Page| {
        p.js("({ connected: api().connected, reconnects: api().reconnects, dropped: api().dropped, renamed: api().renamed })")
    };
    assert_eq!(
        state(&page),
        json!({"connected": true, "reconnects": 0, "dropped": 0, "renamed": 0})
    );
    let s = state(&dup);
    assert_eq!(
        (&s["connected"], &s["dropped"], &s["renamed"]),
        (&json!(true), &json!(0), &json!(1)),
        "{s}"
    );
    assert!(s["reconnects"].as_u64().unwrap() <= 1, "{s}");
}
