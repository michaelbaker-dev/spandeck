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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::wire::{Deck, Link};
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

// ------------------------------------------------- what is typed, at a station

/// A page's own clock, which agrees with no other: it started somewhere, and
/// a test can make it stand still for a while (a machine that slept).
struct PageClock {
    began: Instant,
    behind_ms: AtomicU64,
}

impl PageClock {
    fn new() -> PageClock {
        PageClock {
            began: Instant::now(),
            behind_ms: AtomicU64::new(0),
        }
    }

    fn now(&self) -> f64 {
        7_000_000.0 + self.began.elapsed().as_secs_f64() * 1000.0
            - self.behind_ms.load(Ordering::SeqCst) as f64
    }
}

/// A page as it types: a deck's socket, its clock, and the number of the
/// last thing it typed on each attachment.
struct Typist {
    deck: Deck,
    clock: PageClock,
    pings: AtomicU64,
    iseq: AtomicU64,
}

impl Typist {
    async fn open(s: &Station, port: u16, client: &str, instance: &str) -> Typist {
        Typist {
            deck: Deck::open_as(s, port, page(client, instance)).await,
            clock: PageClock::new(),
            pings: AtomicU64::new(0),
            iseq: AtomicU64::new(0),
        }
    }

    /// A ping, as the page sends one: its clock, its round trip, and
    /// whether it was away. Not waited for.
    async fn ping(&self, rtt_ms: u64, fresh: bool) -> u64 {
        let n = 1 + self.pings.fetch_add(1, Ordering::SeqCst);
        let fresh = if fresh { r#","fresh":true"# } else { "" };
        self.deck
            .text(format!(
                r#"{{"ping":{n},"t":{},"rtt":{rtt_ms}{fresh}}}"#,
                self.clock.now()
            ))
            .await;
        n
    }

    async fn pong(&self, n: u64) {
        self.deck
            .until(&format!("the answer to ping {n}"), 20, |seen| {
                seen.pongs
                    .iter()
                    .any(|p| p["pong"] == json!(n))
                    .then_some(())
            })
            .await
    }

    async fn pinged(&self, rtt_ms: u64, fresh: bool) {
        let n = self.ping(rtt_ms, fresh).await;
        self.pong(n).await;
    }

    /// Type into a channel: numbered, and stamped with when it was typed.
    /// Its number.
    async fn key(&self, chan: u64, data: &str) -> u64 {
        let iseq = 1 + self.iseq.fetch_add(1, Ordering::SeqCst);
        self.key_as(chan, data, iseq, self.clock.now()).await;
        iseq
    }

    async fn key_as(&self, chan: u64, data: &str, iseq: u64, t: f64) {
        self.deck
            .raw(json!({"id": null, "method": "pane.input", "params": {"chan": chan, "data": data, "iseq": iseq, "t": t}}))
            .await;
    }

    /// Wait until this many things typed into the channel were said not
    /// to have been taken, for `why`; what was said.
    async fn dropped(&self, chan: u64, why: &str, n: usize) -> Vec<Value> {
        self.deck
            .until(&format!("{n} things dropped as {why}"), 20, |seen| {
                let said: Vec<Value> = seen
                    .said
                    .iter()
                    .filter(|v| v["chan"] == json!(chan) && v["why"] == json!(why))
                    .filter(|v| v["input_dropped"] == json!(1))
                    .cloned()
                    .collect();
                (said.len() >= n).then_some(said)
            })
            .await
    }
}

/// What a pane's screen shows, some seconds from now (so that what must
/// never arrive has had its time to).
async fn text_after(s: &Arc<Station>, pane: &str, ms: u64) -> String {
    tokio::time::sleep(Duration::from_millis(ms)).await;
    let (s, pane) = (s.clone(), pane.to_string());
    off(move || s.text(&pane)).await
}

fn count(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

/// Unit 03c at one station: the audit's gap 3. Done when, for a page that
/// stamps and numbers what it types (its clock agrees with nobody's):
/// - what it types before the station has read its clock from a ping is
///   refused and said so, and nothing typed sets that clock;
/// - what it types then arrives once, in order; a number sent a second time
///   is not written a second time;
/// - typed into a link that stands still for one second: all of it arrives,
///   once, in order;
/// - typed into a link that stands still for five seconds: none of it is
///   written when the link returns, and each key is said to be late, with
///   its number and how late;
/// - a key in the connection when the page slept, followed by the ping that
///   says the page was away: refused (it must not pass as fresh), and a page
///   whose clock stood still while it slept is refused until that ping, and
///   types again after it;
/// - over a link that takes two seconds each way, typing works;
/// - typed by the pane's name (a phone typing into a pane it watches): on
///   time it is taken, late it is refused with an answer that says so; a
///   message from the message box has ten seconds, and no more.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts a daemon and holds a link still for seconds at a time"]
async fn typed_input_is_on_time_or_refused() {
    let a = Arc::new(off(|| Station::start("alpha")).await);
    let pane = shell(&a, "here");
    let wire = Link::to(a.port);
    let p = Typist::open(&a, wire.port, "web-1-aa", PAGE).await;
    let caps = p.deck.hello["auth"]["caps"].to_string();
    assert!(caps.contains("\"input\""), "{caps}");
    let chan = attach(&p.deck, json!({"pane": pane}), 100).await.unwrap();

    // ---- before the station has read this page's clock
    let early = p.key(chan, "echo early-$((1+1))\r").await;
    let said = p.dropped(chan, "not_ready", 1).await;
    assert_eq!(said[0]["iseq"], json!(early), "{said:?}");
    // Nothing typed sets the clock: the next key is refused too.
    p.key(chan, "echo early-$((1+1))\r").await;
    p.dropped(chan, "not_ready", 2).await;

    // ---- on time: once, in order; a number is taken once
    p.pinged(1, true).await;
    for piece in ["echo ", "ab", "cd", "e-$((2", "+2))\r"] {
        p.key(chan, piece).await;
    }
    p.deck.shows(chan, "abcde-4").await;
    let again = p.key(chan, "echo once-$((3+3))\r").await;
    p.deck.shows(chan, "once-6").await;
    p.key_as(chan, "echo twice-$((5+5))\r", again, p.clock.now())
        .await;
    p.key(chan, "echo next-$((4+4))\r").await;
    p.deck.shows(chan, "next-8").await;
    let text = text_after(&a, &pane, 0).await;
    assert!(!text.contains("early") && !text.contains("twice"), "{text}");
    assert_eq!(count(&text, "once-6"), 1, "{text}");

    // ---- a gap of one second: delivered, once, in order
    wire.stall(true);
    for piece in ["echo g", "ap1", "-$((5", "+6))\r"] {
        p.key(chan, piece).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    wire.stall(false);
    p.deck.shows(chan, "gap1-11").await;
    assert_eq!(count(&text_after(&a, &pane, 300).await, "gap1-11"), 1);

    // ---- a gap of five seconds: refused with the count, and nothing
    // arrives when the link returns
    wire.stall(true);
    let mut late = vec![];
    for piece in ["echo ", "la", "te", "-$((", "7+", "7))", "\r"] {
        late.push(p.key(chan, piece).await);
    }
    tokio::time::sleep(Duration::from_secs(5)).await;
    wire.stall(false);
    let said = p.dropped(chan, "late", 7).await;
    assert_eq!(said.len(), 7, "{said:?}");
    let numbers: Vec<u64> = said.iter().map(|v| v["iseq"].as_u64().unwrap()).collect();
    assert_eq!(numbers, late, "which keys, in the order typed");
    for v in &said {
        let (age, limit) = (
            v["age_ms"].as_u64().unwrap(),
            v["limit_ms"].as_u64().unwrap(),
        );
        assert!((4500..8000).contains(&age), "{v}");
        assert!((2000..2200).contains(&limit), "{v}");
        assert_eq!(v["station"], json!("alpha"));
    }
    let text = text_after(&a, &pane, 1500).await;
    assert!(!text.contains("late") && !text.contains("14"), "{text}");
    // And the pane is typed into again at once.
    p.pinged(1, false).await;
    p.key(chan, "echo back-$((8+8))\r").await;
    p.deck.shows(chan, "back-16").await;

    // ---- a page that slept with a key in the connection: the key was
    // written before it slept, and the ping that says "I was away" follows
    // it. It is judged by the clock as it was read before that ping.
    wire.stall(true);
    let held = p.key(chan, "echo slept-$((9+9))\r").await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let woke = p.ping(1, true).await;
    wire.stall(false);
    p.pong(woke).await;
    let said = p.dropped(chan, "late", 8).await;
    assert_eq!(said[7]["iseq"], json!(held));
    // Its clock stood still for a minute while it slept: by it, what it
    // types now is a minute old, and is refused until its ping says so.
    p.clock.behind_ms.store(60_000, Ordering::SeqCst);
    p.key(chan, "echo stale-$((10+10))\r").await;
    let said = p.dropped(chan, "late", 9).await;
    assert!(said[8]["age_ms"].as_u64().unwrap() >= 59_000, "{said:?}");
    p.pinged(1, true).await;
    p.key(chan, "echo awake-$((11+11))\r").await;
    p.deck.shows(chan, "awake-22").await;
    let text = text_after(&a, &pane, 500).await;
    assert!(!text.contains("slept") && !text.contains("stale"), "{text}");

    // ---- a link that is slow and alive, two seconds each way: typing works
    wire.slow(2000);
    p.pinged(4000, false).await;
    p.key(chan, "echo slow-$((12+12))\r").await;
    p.deck.shows(chan, "slow-24").await;
    wire.slow(0);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    p.pinged(1, false).await;

    // ---- by the pane's name, as a phone types into a pane it watches
    let by_name = |method: &str, mut params: Value, age_ms: f64| {
        params["pane"] = json!(pane);
        params["t"] = json!(p.clock.now() - age_ms);
        let deck = &p.deck;
        let method = method.to_string();
        async move {
            let id = deck.ask(&method, params).await;
            deck.answer(id).await
        }
    };
    let ok = by_name(
        "pane.input",
        json!({"data": "echo named-$((13+13))\r"}),
        0.0,
    )
    .await;
    assert_eq!(ok, Ok(json!({"ok": true})));
    p.deck.shows(chan, "named-26").await;
    let no = by_name(
        "pane.input",
        json!({"data": "echo old-$((14+14))\r"}),
        5500.0,
    )
    .await
    .unwrap_err();
    assert_eq!(no["code"], json!("late"), "{no}");
    let words = no["message"].as_str().unwrap();
    assert!(
        words.starts_with("not sent: this reached alpha 5.")
            && words.contains("s after it was typed"),
        "{words}"
    );
    // The message box: one deliberate act has ten seconds.
    let ok = by_name(
        "pane.paste",
        json!({"text": "echo box-$((15+15))", "enter": true}),
        5000.0,
    )
    .await;
    assert_eq!(ok, Ok(json!({"ok": true})));
    p.deck.shows(chan, "box-30").await;
    let no = by_name(
        "pane.paste",
        json!({"text": "echo boxlate-$((16+16))", "enter": true}),
        10_500.0,
    )
    .await
    .unwrap_err();
    assert_eq!(no["code"], json!("late"), "{no}");
    let text = text_after(&a, &pane, 500).await;
    assert!(
        !text.contains("old-") && !text.contains("boxlate"),
        "{text}"
    );
    // What is not stamped (a page from before, the command line) is taken
    // as before.
    p.deck.key(chan, "echo plain-$((17+17))\r").await;
    p.deck.shows(chan, "plain-34").await;
    let late = a.call("perf.get", json!({}))["typed"]["late"]["since_start"]
        .as_u64()
        .unwrap();
    assert_eq!(late, 11, "7, the two of the sleeping page, and two by name");

    drop(p);
    off(move || drop(a)).await;
}

/// The same through another station: the page is served by alpha and the
/// pane is bravo's, with the link between the two stations standing still.
/// Done when:
/// - what is typed arrives, once; and what a phone types into a pane it
///   watches (by the pane's name) keeps its order with a paste;
/// - typed just before a gap of one second: delivered once;
/// - typed just before a gap of four seconds: bravo refuses each
///   key when the link returns, alpha passes that on with the channel and
///   the key's number, and nothing is written;
/// - typed while bravo has said nothing for four seconds: alpha does not
///   put it in the connection at all, and says so at once;
/// - typed by the pane's name into the gap: not written when the link
///   returns;
/// - and typing works again afterwards.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts two daemons and holds the link between them still"]
async fn typed_through_another_station_is_on_time_or_refused() {
    let (a, b, wire) = off(|| {
        let (pa, pb) = (free_port(), free_port());
        let wire = Link::to(pb);
        let a = Station::in_fleet("alpha", pa, &[wire.port], "");
        let b = Station::in_fleet("bravo", pb, &[pa], "");
        wait_for("the two stations to see each other", 30, || {
            (a.sees("bravo").as_deref() == Some("online")
                && b.sees("alpha").as_deref() == Some("online"))
            .then_some(())
        });
        (Arc::new(a), Arc::new(b), wire)
    })
    .await;
    let (bee, sea) = (shell(&b, "bee"), shell(&b, "sea"));
    let p = Typist::open(&a, a.port, "web-1-aa", PAGE).await;
    p.pinged(1, true).await;
    let on_bravo = |pane: &str| json!({"pane": pane, "station": "bravo"});
    let chan = attach(&p.deck, on_bravo(&bee), 200).await.unwrap();
    let mut watching = on_bravo(&sea);
    watching["mode"] = json!("watch");
    let watch = attach(&p.deck, watching, 201).await.unwrap();

    // ---- on time, once
    p.key(chan, "echo far-$((1+1))\r").await;
    p.deck.shows(chan, "far-2").await;
    // ---- a phone typing into the pane it watches, with a paste: in order
    let to_sea = |method: &str, mut params: Value| {
        params["pane"] = json!(sea);
        params["t"] = json!(p.clock.now());
        json!({"station": "bravo", "method": method, "params": params})
    };
    let first = p
        .deck
        .ask(
            "peer.call",
            to_sea("pane.paste", json!({"text": "echo one", "enter": false})),
        )
        .await;
    let second = p
        .deck
        .ask(
            "peer.call",
            to_sea("pane.input", json!({"data": "-two-$((2+2))\r"})),
        )
        .await;
    assert!(p.deck.answer(first).await.is_ok() && p.deck.answer(second).await.is_ok());
    p.deck.shows(watch, "one-two-4").await;

    // ---- a gap of one second between the stations: delivered, once
    wire.stall(true);
    p.key(chan, "echo gap1-$((5+6))\r").await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    wire.stall(false);
    p.deck.shows(chan, "gap1-11").await;
    assert_eq!(count(&text_after(&b, &bee, 300).await, "gap1-11"), 1);

    // ---- a gap of four seconds. (Bravo is heard from just before it, so
    // that the connection, which is given up after six seconds of
    // silence, outlives it.)
    p.key(chan, "echo pre-$((6+6))\r").await;
    p.deck.shows(chan, "pre-12").await;
    wire.stall(true);
    let mut late = vec![];
    for piece in ["echo ", "late", "-$((7", "+7))", "\r"] {
        late.push(p.key(chan, piece).await);
    }
    // By the pane's name, into the same gap.
    let ghost = p
        .deck
        .ask(
            "peer.call",
            to_sea("pane.input", json!({"data": "echo ghost-$((8+8))\r"})),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(3800)).await;
    // Bravo has now said nothing for over three and a half seconds: this
    // one is not even put in the connection.
    let silent = p.key(chan, "echo silent-$((9+9))\r").await;
    let said = p.dropped(chan, "silent", 1).await;
    assert_eq!(said[0]["iseq"], json!(silent), "{said:?}");
    assert_eq!(said[0]["station"], json!("bravo"));
    assert!(said[0]["quiet_ms"].as_u64().unwrap() >= 3500, "{said:?}");
    wire.stall(false);
    let said = p.dropped(chan, "late", 5).await;
    let numbers: Vec<u64> = said.iter().map(|v| v["iseq"].as_u64().unwrap()).collect();
    assert_eq!(numbers, late);
    for v in &said {
        assert_eq!(v["station"], json!("bravo"), "{v}");
        assert!(v["age_ms"].as_u64().unwrap() >= 3500, "{v}");
    }
    assert!(p.deck.answer(ghost).await.is_err());
    let text = format!(
        "{}\n{}",
        text_after(&b, &bee, 1500).await,
        text_after(&b, &sea, 0).await
    );
    for never in ["late", "14", "ghost", "silent"] {
        assert!(!text.contains(never), "{never}: {text}");
    }
    // ---- and typing works again
    assert!(p.deck.unreachable(chan).is_none());
    p.key(chan, "echo back-$((10+10))\r").await;
    p.deck.shows(chan, "back-20").await;

    drop(p);
    off(move || drop((a, b))).await;
}

/// Mixed versions, which is a station with the switch off. Done when:
/// - a station that does not judge says so (`caps` has no `input`), and
///   takes what is stamped as it takes anything, however old the stamp;
/// - a pane of such a station shown through a station of today is typed
///   into as before, and what is typed while it has said nothing for four
///   seconds is refused by the station serving the page.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts two daemons, one with the switch off"]
async fn a_station_that_does_not_judge_is_typed_into_as_before() {
    let (a, b, wire) = off(|| {
        let (pa, pb) = (free_port(), free_port());
        let wire = Link::to(pb);
        let a = Station::in_fleet("alpha", pa, &[wire.port], "");
        let b = Station::in_fleet("bravo", pb, &[pa], "[flags]\ndeck_input = false\n");
        wait_for("the two stations to see each other", 30, || {
            (a.sees("bravo").as_deref() == Some("online")
                && b.sees("alpha").as_deref() == Some("online"))
            .then_some(())
        });
        (Arc::new(a), Arc::new(b), wire)
    })
    .await;
    let bee = shell(&b, "bee");
    // Its own page: nothing is judged.
    let own = Typist::open(&b, b.port, "web-2-bb", PAGE).await;
    let caps = own.deck.hello["auth"]["caps"].to_string();
    assert!(!caps.contains("\"input\""), "{caps}");
    let chan = attach(&own.deck, json!({"pane": bee}), 300).await.unwrap();
    own.key_as(chan, "echo any-$((1+1))\r", 1, own.clock.now() - 60_000.0)
        .await;
    own.deck.shows(chan, "any-2").await;

    // Through a station of today.
    let p = Typist::open(&a, a.port, "web-1-aa", PAGE).await;
    p.pinged(1, true).await;
    let chan = attach(&p.deck, json!({"pane": bee, "station": "bravo"}), 400)
        .await
        .unwrap();
    p.key(chan, "echo through-$((2+2))\r").await;
    p.deck.shows(chan, "through-4").await;
    wire.stall(true);
    tokio::time::sleep(Duration::from_millis(3800)).await;
    let silent = p.key(chan, "echo silent-$((3+3))\r").await;
    let said = p.dropped(chan, "silent", 1).await;
    assert_eq!(said[0]["iseq"], json!(silent));
    wire.stall(false);
    // Typed again once bravo has been heard from.
    tokio::time::sleep(Duration::from_millis(700)).await;
    p.key(chan, "echo after-$((4+4))\r").await;
    p.deck.shows(chan, "after-8").await;
    assert!(!text_after(&b, &bee, 500).await.contains("silent"));

    drop((p, own));
    off(move || drop((a, b))).await;
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
    // The copy opened in front. A tab behind another is not drawn by the
    // browser (the nightly run on a hosted machine waited for it in vain),
    // so the original is looked at again, as its owner would.
    page.tab.bring_to_front().unwrap();
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

// ------------------------------------------------- what is typed, in the page

fn pictures() -> Option<std::path::PathBuf> {
    let out = std::path::PathBuf::from(std::env::var_os("SPANDECK_SHOTS")?);
    std::fs::create_dir_all(&out).unwrap();
    Some(out)
}

fn picture(page: &browser::Page, name: &str) {
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    if let Some(out) = pictures() {
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("{name}.png")), png).unwrap();
    }
}

/// What the pane says of what was typed into it, or "".
const SAID: &str =
    "(n => n && !n.hidden ? n.innerText : '')(document.querySelector('.pane .type-note'))";

fn said(page: &browser::Page, what: &str, needle: &str) -> String {
    let expr = format!("(t => t.includes({}) ? t : '')({SAID})", json!(needle));
    page.wait(what, &expr).as_str().unwrap().to_string()
}

fn ready(page: &browser::Page) {
    attached(page);
    page.wait("the station to have read the page's clock", "api().ready");
}

/// The page's rules for what is typed, each as the plain function it is
/// (crates/web/assets/typed.js), run in a real browser because that is
/// where the page's code runs (the every-change tier has no JavaScript
/// engine). Done when: what a terminal says by itself (focus, the mouse,
/// its answers to the program) is not taken for typing, by the same rule as
/// the station's; a paste is told from keys; the hold takes keys up to 256
/// bytes and refuses a paste or a key past that; a held key is too old
/// after 1.5 s by either clock; and the counts are said in words.
#[test]
#[ignore = "merge tier: starts a daemon and a browser"]
fn the_pages_rules_for_what_is_typed_are_the_ones_written_down() {
    let a = Station::start("alpha");
    let page = browser::Page::open(&a, 1000, 700);
    let rules = page.js(
        "import('/typed.js').then(m => { \
           const hold = []; \
           const takes = ['a', 'b'.repeat(60), 'c'.repeat(64), 'd'.repeat(64), 'e'.repeat(64), 'ff', 'g'].map(d => m.holdTake(hold, { data: d, t: 0, wall: 0 })); \
           return { \
             typing: ['a', '\\r', '\\x1b[A', '\\x03', '\\x1b[I', '\\x1b[O', '\\x1b[12;40R', '\\x1b[?1;2c', '\\x1b[<64;10;5M', '\\x1b[Ia', '\\x1b]11;rgb:0000/0000/0000\\x07', '\\x1b[200~ls\\x1b[201~', ''].map(m.isTyping), \
             paste: ['a', '\\x1b[200~ls\\x1b[201~', 'x'.repeat(64), 'x'.repeat(65), 'é'.repeat(33)].map(m.isPaste), \
             takes, held: hold.length, \
             pasteNotHeld: m.holdTake([], { data: '\\x1b[200~ls\\x1b[201~', t: 0, wall: 0 }), \
             old: [[1500, 1500], [1501, 0], [0, 1501], [100, 100]].map(([p, w]) => m.heldTooLong({ t: 1000, wall: 5000 }, 1000 + p, 5000 + w)), \
             words: [m.keysWords(1), m.keysWords(7), m.keysWords(7, 1), m.keysWords(0, 1), m.keysWords(0, 2), m.wasWere(1), m.wasWere(2), m.secsWords(5234)], \
             off: [m.offWords('closed', 'alpha', 'alpha'), m.offWords('silent', 'alpha', 'alpha'), m.offWords('clock', 'alpha', 'alpha'), m.offWords('pane', 'alpha', 'alpha'), m.offWords('pane', 'alpha', 'bravo')], \
             late: m.droppedWords('late', 7, 0, 'web', { station: 'alpha', ageMs: 5234, limitMs: 2080 }), \
             one: m.droppedWords('late', 1, 0, 'web', { station: 'alpha', ageMs: 5234, limitMs: 2080 }), \
             silent: m.droppedWords('silent', 2, 1, 'web', { station: 'bravo', quietMs: 4100 }), \
             limits: [m.HOLD_MS, m.HOLD_BYTES, m.PASTE_BYTES, m.PROBE_AFTER, m.ACT_MS] }; })",
    );
    assert_eq!(
        rules["typing"],
        json!([
            true, true, true, true, false, false, false, false, false, true, false, true, false
        ])
    );
    assert_eq!(rules["paste"], json!([false, true, false, true, true]));
    // 1 + 60 + 64 + 64 = 189 bytes held; the next 64 would make 253 and
    // are held; then 2 more make 255, and one more 256; nothing over.
    assert_eq!(
        rules["takes"],
        json!(["held", "held", "held", "held", "held", "held", "held"])
    );
    assert_eq!(rules["held"], json!(7));
    assert_eq!(rules["pasteNotHeld"], json!("paste"));
    assert_eq!(rules["old"], json!([false, true, true, false]));
    assert_eq!(
        rules["words"],
        json!([
            "1 key",
            "7 keys",
            "7 keys and a paste",
            "a paste",
            "2 pastes",
            "was",
            "were",
            "5.2 s"
        ])
    );
    assert_eq!(
        rules["off"],
        json!([
            "Not connected to alpha",
            "alpha is not answering",
            "alpha has not answered this page yet",
            "This pane is not attached yet",
            "bravo is not showing this pane yet"
        ])
    );
    assert_eq!(rules["late"], json!("7 keys typed into web were not sent: they reached alpha 5.2 s after you typed them, and nothing typed is delivered more than 2.1 s late."));
    assert_eq!(rules["one"], json!("1 key typed into web was not sent: it reached alpha 5.2 s after you typed it, and nothing typed is delivered more than 2.1 s late."));
    assert_eq!(
        rules["silent"],
        json!("2 keys and a paste typed into web were not sent: bravo had not answered for 4.1 s.")
    );
    assert_eq!(rules["limits"], json!([1500, 256, 64, 300, 10000]));
    // One byte more than the hold takes: all of it is refused.
    let over = page.js(
        "import('/typed.js').then(m => { const hold = [{ data: 'x'.repeat(60), t: 0, wall: 0 }, { data: 'x'.repeat(60), t: 0, wall: 0 }, { data: 'x'.repeat(60), t: 0, wall: 0 }, { data: 'x'.repeat(60), t: 0, wall: 0 }]; return [m.holdTake(hold, { data: 'x'.repeat(16), t: 0, wall: 0 }), m.holdTake(hold, { data: 'y', t: 0, wall: 0 })]; })",
    );
    assert_eq!(over, json!(["held", "full"]));
    // The round trip a page reports for a key's limit: the middle one of
    // its last pings; the latest when that is longer (a link that has just
    // turned slow), but one answer that came late because the link stood
    // still counts as at most a second more than twice the middle one.
    page.js(API);
    let way = page.js(
        "import('/api.js').then(m => [[5, 5, 5], [5, 5, 4000], [5, 5, 60000], [4000, 4000, 4000, 9000], [4000, 4000, 100], []].map(r => api()._way.call({ rtts: r, rtt: () => m.median(r) })))",
    );
    assert_eq!(way, json!([5, 1010, 1010, 9000, 4000, 0]));
}

/// The audit's gap 3 in a real page, at a computer's width (the pane is
/// typed into by its channel) and at a phone's (the pane is watched and
/// typed into by its name). Done when:
/// - typed into a link that stands still for one second: it arrives, once,
///   and the pane says nothing;
/// - typed into a link that stands still for five seconds: within three
///   seconds the pane says how many keys may not have been sent; keys typed
///   from then on are held a second and a half and then refused on the
///   pane, with their count, and every further key adds to it; when the
///   link returns the pane says how many keys the station refused and how
///   late they were, typing works again, and nothing of any of it is ever
///   written to the pane;
/// - a key held by a page that then was not running (its script stood
///   still for longer than a key may be held) is not sent when the page
///   runs again, and the pane says so;
/// - the Performance page counts the keys not sent.
///
/// With `SPANDECK_SHOTS=<folder>` it also writes a picture of each state.
#[test]
#[ignore = "nightly: a daemon and a browser, a link held still for seconds, and it asserts on measured times"]
fn typing_into_a_gap_is_delivered_on_time_or_refused_on_the_pane() {
    for (width, height, tag) in [(1300, 900, "desktop"), (390, 800, "phone")] {
        let a = Station::start("alpha");
        let pane = shell(&a, "here");
        let wire = Link::to(a.port);
        let page = browser::Page::open_through(&a, wire.port, width, height);
        page.js(API);
        page.goto(&format!("#/s/alpha/p/{pane}"));
        ready(&page);
        type_in(&page, "echo one-$((1+1))\n");
        a.wait_text(&pane, "one-2");

        // ---- one second
        wire.stall(true);
        type_in(&page, "echo gap-$((2+2))\n");
        std::thread::sleep(Duration::from_secs(1));
        wire.stall(false);
        a.wait_text(&pane, "gap-4");
        assert_eq!(page.js(SAID), json!(""), "{tag}");

        // ---- five seconds
        std::thread::sleep(Duration::from_secs(2));
        wire.stall(true);
        let stalled = Instant::now();
        for piece in ["echo ", "la", "te", "-$((", "7+", "7))", "\n"] {
            type_in(&page, piece);
        }
        let maybe = said(
            &page,
            "the pane to say what may not have been sent",
            "may not have been sent",
        );
        eprintln!(
            "{tag}: after {:.1} s: {maybe}",
            stalled.elapsed().as_secs_f64()
        );
        assert!(stalled.elapsed() < Duration::from_secs(3), "{tag}");
        assert!(
            maybe.contains("alpha stopped answering: the last 7 keys typed into here may not have been sent. Look at the pane before typing them again."),
            "{tag}: {maybe}"
        );
        picture(&page, &format!("{tag}-1-may-not-have-been-sent"));
        for key in ["x", "y", "z"] {
            type_in(&page, key);
        }
        let off = said(&page, "the pane to say what was not sent", "Typing is off");
        eprintln!(
            "{tag}: after {:.1} s: {off}",
            stalled.elapsed().as_secs_f64()
        );
        assert!(
            off.contains("alpha is not answering: 3 keys typed into here were not sent. Typing is off until it is back."),
            "{tag}: {off}"
        );
        type_in(&page, "w");
        let off = said(
            &page,
            "the count to go up",
            "4 keys typed into here were not sent",
        );
        assert!(
            off.contains("alpha is not answering: 4 keys"),
            "{tag}: {off}"
        );
        picture(&page, &format!("{tag}-2-keys-not-sent"));
        std::thread::sleep(Duration::from_secs(5).saturating_sub(stalled.elapsed()));
        wire.stall(false);
        let late = said(
            &page,
            "the pane to say what the station refused",
            "7 keys typed into here were not sent: they reached alpha ",
        );
        eprintln!("{tag}: when the link returned: {late}");
        // How late, as the station measured it: the link stood still for
        // five seconds, and the keys were typed in its first second.
        let age: f64 = late
            .split("they reached alpha ")
            .nth(1)
            .and_then(|rest| rest.split(" s after you typed them").next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("{tag}: {late}"));
        assert!((4.0..7.0).contains(&age), "{tag}: {late}");
        assert!(
            late.contains("nothing typed is delivered more than 2."),
            "{tag}: {late}"
        );
        let again = said(
            &page,
            "the pane to say typing works again",
            "Typing works again",
        );
        assert!(
            again.contains("4 keys typed into here were not sent. Typing works again."),
            "{tag}: {again}"
        );
        assert!(!again.contains("may not have been sent"), "{tag}: {again}");
        picture(&page, &format!("{tag}-3-refused-as-late"));
        std::thread::sleep(Duration::from_millis(1500));
        let text = a.text(&pane);
        for never in ["late", "xyz"] {
            assert!(!text.contains(never), "{tag}: {never}: {text}");
        }
        type_in(&page, "echo back-$((8+8))\n");
        a.wait_text(&pane, "back-16");

        // ---- a page that was not running, with a key held
        std::thread::sleep(Duration::from_secs(2));
        wire.stall(true);
        page.wait("the page to know its link is silent", "!api().live");
        type_in(&page, "q");
        // The page's script stands still for 2.6 s, and the link returns
        // while it does: when it runs again it could send, and must not.
        let unstall = std::thread::scope(|s| {
            let wire = &wire;
            s.spawn(move || {
                std::thread::sleep(Duration::from_millis(600));
                wire.stall(false);
            });
            page.js("(() => { const until = performance.now() + 2600; while (performance.now() < until); return true; })()")
        });
        assert_eq!(unstall, json!(true));
        let slept = said(
            &page,
            "the pane to say the held key was not sent",
            "1 key typed into here was not sent",
        );
        eprintln!("{tag}: after standing still: {slept}");
        picture(&page, &format!("{tag}-4-held-while-not-running"));
        page.wait("the link to be back", "api().ready");
        type_in(&page, "echo awake-$((9+9))\n");
        a.wait_text(&pane, "awake-18");
        let text = a.text(&pane);
        assert!(!text.contains("qecho"), "{tag}: {text}");

        // ---- the Performance page counts them: 7 + 4 + 1
        page.goto("#/perf");
        let counted = page.wait(
            "the count of keys not sent",
            "document.querySelector('.keys-not-sent b')?.innerText",
        );
        assert_eq!(counted, json!("12"), "{tag}");
        picture(&page, &format!("{tag}-5-performance"));
    }
}

/// The audit: "in a two-pane layout the terminal lost the keyboard after a
/// reconnect", and keys typed in the gap of a reconnect were dropped without
/// a word. Done when, in a layout of two panes with the keyboard in the
/// second: the page's connection is lost and made again; what is typed in
/// the gap is held and arrives, once, in the second pane and nowhere else;
/// and afterwards the keyboard is still in that terminal, so that real key
/// presses, with no click, reach it.
#[test]
#[ignore = "nightly: a daemon and a browser"]
fn a_layout_keeps_the_keyboard_and_the_keys_of_a_reconnect() {
    let a = Station::start("alpha");
    let (left, right) = (shell(&a, "left"), shell(&a, "right"));
    a.call(
        "deck.layout.save",
        json!({"layout": {"name": "two", "tree": {"dir": "row", "ratio": 500, "a": {"id": left}, "b": {"id": right}}}}),
    );
    let page = browser::Page::open(&a, 1300, 800);
    page.js(API);
    page.goto("#/l/two");
    let term = format!("document.querySelector('.pane[data-pane=\"{right}\"]').paneTerm");
    page.wait(
        "both panes to be attached",
        "(p => p.length === 2 && p.every(x => x.paneTerm.chan !== null))([...document.querySelectorAll('.station-main .pane')])",
    );
    page.wait("the station to have read the page's clock", "api().ready");
    // The keyboard goes to the second pane, as a click puts it there.
    page.js(&format!("({term}.el.dispatchEvent(new MouseEvent('mousedown', {{bubbles: true}})), {term}.term.focus(), true)"));
    // What was typed has come back to the page, not only reached the pane:
    // a connection lost before that leaves the page saying, rightly, that
    // the keys may not have been sent, and the next step loses it.
    let echoed = |what: &str| {
        wait_for("what was typed to be on the page", 10, || {
            page.term_text().contains(what).then_some(())
        });
    };
    page.type_str("echo before-$((1+1))");
    page.press_key("Enter");
    a.wait_text(&right, "before-2");
    echoed("before-2");
    for round in 0..3 {
        // The connection is lost, and what is typed at that moment is held.
        page.js(&format!(
            "(api()._drop(), {term}.term.input('echo held{round}-$((2+2))\\n'), true)"
        ));
        a.wait_text(&right, &format!("held{round}-4"));
        page.wait("the page to be connected again", "api().ready");
        page.wait(
            "the pane to be attached again",
            &format!("{term}.chan !== null"),
        );
        std::thread::sleep(Duration::from_millis(300));
        // No click: the keyboard is where it was.
        assert_eq!(
            page.js(&format!("{term}.el.contains(document.activeElement)")),
            json!(true),
            "round {round}"
        );
        page.type_str(&format!("echo after{round}-$((3+3))"));
        page.press_key("Enter");
        a.wait_text(&right, &format!("after{round}-6"));
        echoed(&format!("after{round}-6"));
    }
    let (l, r) = (a.text(&left), a.text(&right));
    assert!(!l.contains("held") && !l.contains("after"), "{l}");
    for round in 0..3 {
        assert_eq!(count(&r, &format!("held{round}-4")), 1, "{r}");
        assert_eq!(count(&r, &format!("after{round}-6")), 1, "{r}");
    }
    assert_eq!(
        page.js(SAID
            .replace(
                ".pane .type-note",
                &format!(".pane[data-pane=\"{right}\"] .type-note")
            )
            .as_str()),
        json!("")
    );
    assert_eq!(page.js("api().reconnects"), json!(3));
}

/// The second review: the message box (a typed or dictated message) is one
/// deliberate act; it keeps its text when it is refused. On a phone. Done
/// when: with the link not answering, Send says the message was not sent
/// and why, and the text is still in the box; sent into a link that then
/// stands still for longer than the act's ten seconds, the box says it may
/// or may not have been sent (never "not sent" of something it cannot
/// know), the text is still in the box, and the station refuses the message
/// when it arrives; and with the link back, Send sends it and empties the
/// box.
/// With `SPANDECK_SHOTS=<folder>` it also writes a picture of each state.
#[test]
#[ignore = "nightly: a daemon and a browser, a link held still for fifteen seconds"]
fn the_message_box_keeps_its_text_when_it_is_not_sent() {
    let a = Station::start("alpha");
    let pane = shell(&a, "here");
    let wire = Link::to(a.port);
    let page = browser::Page::open_through(&a, wire.port, 390, 800);
    page.js(API);
    page.goto(&format!("#/s/alpha/p/{pane}"));
    ready(&page);
    let write = |text: &str| {
        page.js(&format!(
            "(b => {{ b.value = {}; b.dispatchEvent(new Event('input', {{bubbles: true}})); return true; }})(document.querySelector('.composer-box'))",
            json!(text)
        ));
    };
    let send =
        || page.js("(document.querySelector('.composer-row button[type=submit]').click(), true)");
    let box_text = || page.js("document.querySelector('.composer-box').value");
    let box_said =
        "(n => n && !n.hidden ? n.innerText : '')(document.querySelector('.composer .send-said'))";

    // ---- the link is not answering: not sent, and said at once
    wire.stall(true);
    page.wait("the page to know its link is silent", "!api().live");
    write("echo msg-$((5+5))");
    send();
    let no = page.wait("the box to say so", box_said);
    assert_eq!(
        no,
        json!("Not sent: alpha is not answering. Your message is still in the box.")
    );
    assert_eq!(box_text(), json!("echo msg-$((5+5))"));
    picture(&page, "phone-6-message-not-sent");
    wire.stall(false);
    page.wait("the link to be back", "api().ready");

    // ---- sent, and the link stands still for longer than the act has
    std::thread::sleep(Duration::from_secs(2));
    wire.stall(true);
    let began = Instant::now();
    send();
    page.wait(
        "the button to say it is sending",
        "document.querySelector('.composer-row button[type=submit]').innerText === 'Sending…'",
    );
    picture(&page, "phone-7-message-sending");
    let unknown = wait_for("the box to say it does not know", 30, || {
        page.js(box_said)
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from)
    });
    eprintln!("after {:.1} s: {unknown}", began.elapsed().as_secs_f64());
    assert!(
        // Whichever came first: the act's own time, or the page giving
        // up a connection that had said nothing for ten seconds.
        (unknown.starts_with("No answer from alpha within 1")
            || unknown.starts_with("The connection to alpha was lost before it answered"))
            && unknown.ends_with(": this may or may not have been sent. Look at the pane before sending it again. Your message is still in the box."),
        "{unknown}"
    );
    assert!(began.elapsed() > Duration::from_secs(8), "{unknown}");
    assert_eq!(box_text(), json!("echo msg-$((5+5))"));
    picture(&page, "phone-8-message-may-not-have-been-sent");
    // The link returns when the message is more than ten seconds old (the
    // page may have said it does not know a moment before that, when it
    // gave the silent connection up): it arrives now, and is refused.
    std::thread::sleep(Duration::from_millis(11_500).saturating_sub(began.elapsed()));
    wire.stall(false);
    std::thread::sleep(Duration::from_secs(3));
    assert!(!a.text(&pane).contains("msg-10"), "{}", a.text(&pane));

    // ---- and with the link back it is sent, once
    page.wait("the link to be back", "api().ready");
    send();
    a.wait_text(&pane, "msg-10");
    page.wait(
        "the box to be empty",
        "document.querySelector('.composer-box').value === ''",
    );
    assert_eq!(page.js(box_said), json!(""));
    assert_eq!(count(&a.text(&pane), "msg-10"), 1);

    // ---- at a computer's width the same act is the paste dialog (where
    // the browser gives the page no clipboard, as over plain http)
    let desk = browser::Page::open_through(&a, wire.port, 1300, 900);
    desk.js(API);
    desk.goto(&format!("#/s/alpha/p/{pane}"));
    ready(&desk);
    desk.js("(Object.defineProperty(navigator, 'clipboard', {value: undefined, configurable: true}), true)");
    desk.right_click("document.querySelector('.pane')");
    desk.choose("Paste");
    desk.wait(
        "the box to paste into",
        "!!document.querySelector('.dialog textarea')",
    );
    wire.stall(true);
    desk.wait("the page to know its link is silent", "!api().live");
    let paste = "(b => (b.value = 'echo pasted-$((30+3))', b.closest('form').querySelector('button[type=submit]').click(), true))(document.querySelector('.dialog textarea'))";
    desk.js(paste);
    let no = desk.wait(
        "the dialog to say so",
        "(n => n && !n.hidden ? n.innerText : '')(document.querySelector('.dialog .send-said'))",
    );
    assert_eq!(
        no,
        json!("Not sent: alpha is not answering. Your text is still here.")
    );
    assert_eq!(
        desk.js("document.querySelector('.dialog textarea').value"),
        json!("echo pasted-$((30+3))")
    );
    picture(&desk, "desktop-6-paste-not-sent");
    wire.stall(false);
    desk.wait("the link to be back", "api().ready");
    desk.js(paste);
    a.wait_text(&pane, "echo pasted-$((30+3))");
    desk.wait("the dialog to close", "!document.querySelector('.dialog')");
}

/// The second review's condition for the probe: a link that is slow and
/// alive can be typed over, and the page neither drops its connection nor
/// refuses the keys. Done when, over a link that takes two seconds each
/// way: what is typed arrives, the pane never says a key was not sent, and
/// the page made no new connection.
#[test]
#[ignore = "nightly: a daemon and a browser over a slow link for half a minute"]
fn a_slow_link_that_is_alive_can_be_typed_over() {
    let a = Station::start("alpha");
    let pane = shell(&a, "here");
    let wire = Link::to(a.port);
    let page = browser::Page::open_through(&a, wire.port, 1300, 900);
    page.js(API);
    page.goto(&format!("#/s/alpha/p/{pane}"));
    ready(&page);
    // The link turns slow under a page that is waiting for nothing: with
    // answers still on their way, each one that arrives late is taken for
    // proof of the link and the ping behind it is let go of, so the page
    // never measures the round trip (it read 15 ms for forty seconds).
    page.wait(
        "the link to be quiet",
        "(a => a.pingOut === null && a.awake - a.frameAwake > 500)(api())",
    );
    wire.slow(2000);
    // The page learns the link's round trip from its pings.
    wait_for("the page to measure the slow link", 30, || {
        page.js("api()._trip()").as_f64().filter(|ms| *ms > 3000.0)
    });
    page.wait("typing to be on", "api().ready");
    for n in 0..3 {
        type_in(&page, &format!("echo slow{n}-$((6+6))\n"));
        a.wait_text(&pane, &format!("slow{n}-12"));
        std::thread::sleep(Duration::from_secs(3));
    }
    let l = page.js("({ dropped: api().dropped, reconnects: api().reconnects })");
    assert_eq!(l, json!({"dropped": 0, "reconnects": 0}));
    let text = a.text(&pane);
    for n in 0..3 {
        assert_eq!(count(&text, &format!("slow{n}-12")), 1, "{text}");
    }
    let noted = page.js(SAID);
    assert!(!noted.as_str().unwrap().contains("not sent"), "{noted}");
    picture(&page, "desktop-9-slow-link-typed-over");
}
