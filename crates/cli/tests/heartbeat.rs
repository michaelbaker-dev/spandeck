//! A connection that has gone silent is noticed, and the screen says so
//! (DECISIONS.md, the section of that name; unit 03a of the performance and
//! continuity plan). Scenario tests with real stations and a real page
//! (merge tier), and the timings (nightly). Run with `--ignored`.
//!
//! The audit (FAILURES sections 4 and 5, gap 2, measured on 0.4.2): with
//! the browser's link half-open, the page showed nothing at all at 1 s, 5 s,
//! 30 s, 60 s, 120 s and 300 s, and every key typed meanwhile was delivered
//! when the link returned (232 keys up to 60 s late); a pane reached through
//! another station froze for 60 s and for 10 minutes with no notice. There
//! was no ping anywhere and no time limit on a request in the page.

// A station is frozen with a Unix signal, and the panes are `/bin/sh`.
#![cfg(unix)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::wire::{unreachable_in, Deck, Link};
use common::*;
use serde_json::{json, Value};

// The numbers differ between Linux and macOS.
const SIGSTOP: i32 = if cfg!(target_os = "macos") { 17 } else { 19 };
const SIGCONT: i32 = if cfg!(target_os = "macos") { 19 } else { 18 };

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

/// How many connections a station has given up for silence, by kind.
fn closed_silent(s: &Station, kind: &str) -> u64 {
    s.call("perf.get", json!({}))["connections"]["closed_silent"][kind]["since_start"]
        .as_u64()
        .unwrap_or(0)
}

fn two(extra_alpha: &str) -> (Station, Station) {
    let (pa, pb) = (free_port(), free_port());
    let a = Station::in_fleet("alpha", pa, &[pb], extra_alpha);
    let b = Station::in_fleet("bravo", pb, &[pa], "");
    wait_for("the two stations to see each other", 30, || {
        (a.sees("bravo").as_deref() == Some("online")
            && b.sees("alpha").as_deref() == Some("online"))
        .then_some(())
    });
    (a, b)
}

/// The audit's gap 2, between stations and towards a page that is gone.
/// Done when: a page's ping is answered with its number, its time and the
/// station's clock; a pane of another station whose station freezes is said
/// to be unreachable on its channel within seven seconds, with the station's
/// name and for how long it said nothing, and that station is then known as
/// not answering by the same state the calls use (an attach to it is refused
/// at once, in its words); when the station is back the pane is attached
/// again and typing reaches it; a station whose gateway goes silent lets go
/// of that gateway's connection; and a deck whose link is half-open is let
/// go of by its station within the half minute.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts two daemons, freezes each in turn, and waits out half a minute"]
async fn a_silent_connection_is_noticed() {
    let (a, b) = off(|| two("")).await;
    let (a, b) = (Arc::new(a), Arc::new(b));
    let sb = b.clone();
    let bee = off(move || shell(&sb, "bee")).await;

    let deck = Deck::open(&a, a.port, "tab-1").await;
    let caps = deck.hello["auth"]["caps"].as_array().unwrap().clone();
    assert!(caps.contains(&json!("ping")), "{caps:?}");
    // A ping is a frame, not a request: no method, no id, and its answer
    // carries the number and the page's time back.
    deck.raw(json!({"ping": 7, "t": 1234.5})).await;
    let pong = deck
        .until("a pong", 5, |seen| seen.pongs.first().cloned())
        .await;
    assert_eq!((&pong["pong"], &pong["t"]), (&json!(7), &json!(1234.5)));
    assert!(pong["s"].is_u64(), "the station's clock: {pong}");

    let chan = deck
        .attach(json!({"pane": bee, "station": "bravo"}))
        .await
        .unwrap();
    deck.key(chan, "echo one-$((40+2))\n").await;
    deck.shows(chan, "one-42").await;

    // bravo freezes. The pane's channel is told, by the gateway's own
    // pings: nobody has to ask anything of bravo for it to be noticed.
    let sb = b.clone();
    off(move || {
        sb.signal(SIGSTOP);
        sb.wait_frozen();
    })
    .await;
    let frozen = Instant::now();
    let said = deck
        .until("the pane to be told its station is silent", 9, |seen| {
            unreachable_in(seen, chan)
        })
        .await;
    assert!(
        frozen.elapsed() < Duration::from_secs(7),
        "told after {:?}",
        frozen.elapsed()
    );
    eprintln!(
        "the pane was told {:?} after its station froze",
        frozen.elapsed()
    );
    assert_eq!(said["station"], json!("bravo"), "{said}");
    assert!(
        said["quiet_ms"].as_u64().is_some_and(|ms| ms >= 6000),
        "for how long it had said nothing: {said}"
    );
    let sa = a.clone();
    assert_eq!(off(move || closed_silent(&sa, "panes_elsewhere")).await, 1);
    // The same state the calls feed: of two attaches asked together, one
    // may be let through to find out, and the other is refused at once in
    // words that name the station.
    let asked = Instant::now();
    let first = deck
        .ask("pane.attach", json!({"pane": bee, "station": "bravo"}))
        .await;
    let second = deck
        .ask("pane.attach", json!({"pane": bee, "station": "bravo"}))
        .await;
    let refused = deck
        .until("an attach refused at once", 3, |seen| {
            seen.answers
                .iter()
                .find(|(id, a)| (*id == first || *id == second) && a.is_err())
                .map(|(_, a)| a.clone().unwrap_err())
        })
        .await;
    assert!(asked.elapsed() < Duration::from_secs(1), "{refused}");
    assert_eq!(refused["code"], json!("not_answering"), "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .starts_with("bravo is not answering"),
        "{refused}"
    );

    // bravo is back. It was itself away for those seconds, so it concludes
    // nothing about its own connections from them; the pane is attached
    // again (the page tries by itself; here the test does) and typing
    // reaches it.
    let sb = b.clone();
    off(move || sb.signal(SIGCONT)).await;
    let began = Instant::now();
    let chan = loop {
        match deck.attach(json!({"pane": bee, "station": "bravo"})).await {
            Ok(chan) => break chan,
            Err(why) => {
                assert!(began.elapsed() < Duration::from_secs(15), "{why}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    deck.key(chan, "echo two-$((40+3))\n").await;
    deck.shows(chan, "two-43").await;

    // The other end of the same rule: the gateway freezes, and the pane's
    // station lets go of the connection it held for that pane.
    let sa = a.clone();
    off(move || {
        sa.signal(SIGSTOP);
        sa.wait_frozen();
    })
    .await;
    let sb = b.clone();
    off(move || {
        wait_for("bravo to let go of the silent gateway", 12, || {
            (closed_silent(&sb, "gateways") >= 1).then_some(())
        })
    })
    .await;
    let sa = a.clone();
    off(move || sa.signal(SIGCONT)).await;

    // A deck whose link is half-open: nothing passes, nothing closes. Its
    // station lets go of it within the half minute (two pings unanswered).
    let link = Link::to(b.port);
    let gone = Deck::open(&b, link.port, "tab-2").await;
    let held = gone.attach(json!({"pane": bee})).await.unwrap();
    gone.key(held, "echo three-$((40+4))\n").await;
    gone.shows(held, "three-44").await;
    link.stall(true);
    let sb = b.clone();
    let began = Instant::now();
    off(move || {
        wait_for("bravo to let go of the deck that is gone", 40, || {
            (closed_silent(&sb, "decks") >= 1).then_some(())
        })
    })
    .await;
    assert!(
        began.elapsed() > Duration::from_secs(20),
        "not before it has gone unanswered twice: {:?}",
        began.elapsed()
    );
    // Meanwhile a deck that is there was never let go of.
    let own = Deck::open(&b, b.port, "tab-3").await;
    own.raw(json!({"ping": 1})).await;
    own.until("a pong", 5, |seen| seen.pongs.first().cloned())
        .await;
    link.stall(false);
}

/// Mixed versions. A station with the switch off behaves as one from
/// before (0.4.6): it says nothing of pings, answers none of a page's, and
/// sends none itself. Done when: its sign-in answer has no `ping` and a
/// page's ping gets no answer and breaks nothing; a pane reached through it
/// on a station that does ping is not let go of (the gateway from before
/// answers that station's pings, as any WebSocket does); a pane of the
/// station from before, reached through a station that pings, stays too,
/// and is still said to be unreachable when the station from before
/// freezes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts two daemons, one with the heartbeat switched off, and freezes it"]
async fn a_station_from_before_is_neither_let_go_of_nor_left_unwatched() {
    let (a, b) = off(|| two("[flags]\ndeck_ping = false\n")).await;
    let (a, b) = (Arc::new(a), Arc::new(b));
    let (sa, sb) = (a.clone(), b.clone());
    let (ay, bee) = off(move || (shell(&sa, "ay"), shell(&sb, "bee"))).await;

    let old = Deck::open(&a, a.port, "tab-1").await;
    let caps = old.hello["auth"]["caps"].as_array().unwrap().clone();
    assert!(!caps.contains(&json!("ping")), "{caps:?}");
    old.raw(json!({"ping": 1, "t": 1.0})).await;
    // Through the station from before, to one that pings its connections
    // every two seconds and gives one up after six.
    let through_old = old
        .attach(json!({"pane": bee, "station": "bravo"}))
        .await
        .unwrap();
    let new = Deck::open(&b, b.port, "tab-2").await;
    let through_new = new
        .attach(json!({"pane": ay, "station": "alpha"}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(9)).await;
    assert!(
        old.seen.lock().unwrap().pongs.is_empty(),
        "no answer to a ping"
    );
    assert_eq!(old.unreachable(through_old), None);
    assert_eq!(new.unreachable(through_new), None);
    old.key(through_old, "echo old-$((40+2))\n").await;
    old.shows(through_old, "old-42").await;
    new.key(through_new, "echo new-$((40+3))\n").await;
    new.shows(through_new, "new-43").await;
    let (sa, sb) = (a.clone(), b.clone());
    let closed = off(move || {
        (
            closed_silent(&sb, "gateways"),
            closed_silent(&sb, "panes_elsewhere"),
            sa.call("perf.get", json!({}))["connections"]["closed_silent"]["gateways"]
                ["since_start"]
                .clone(),
        )
    })
    .await;
    assert_eq!(closed, (0, 0, json!(0)));

    // The station from before freezes: the one that pings notices.
    let sa = a.clone();
    off(move || {
        sa.signal(SIGSTOP);
        sa.wait_frozen();
    })
    .await;
    let said = new
        .until("the pane to be told", 9, |seen| {
            unreachable_in(seen, through_new)
        })
        .await;
    assert_eq!(said["station"], json!("alpha"), "{said}");
    let sa = a.clone();
    off(move || sa.signal(SIGCONT)).await;
}

// ------------------------------------------------------------------ the page

/// What the page's connection says of itself.
fn link(page: &browser::Page) -> Value {
    page.js("(() => { const l = api().link(); return { state: l.state, level: l.level, dropped: api().dropped, reconnects: api().reconnects, silences: api().silences, rtt: api().rtt(), connected: api().connected, live: api().live }; })()")
}

/// The page's `Api`, from the terminal that leads back to it or from the
/// page itself (a test reads it; the deck does not).
const API: &str = "window.api = () => document.getElementById('app').api";

fn shown(sel: &str) -> String {
    format!("(e => !!e && !e.hidden && e.offsetParent !== null ? e.innerText : '')(document.querySelector('{sel}'))")
}

/// The rules of the page's heartbeat, each as the plain function it is
/// (crates/web/assets/api.js), run in a real browser because that is where
/// the page's code runs. Done when: the times between tries are the
/// schedule's, 30% either way, and then five seconds for ever; a tick while
/// the page is hidden judges nothing, the first after it or after the page
/// was not running is a waking and not a silence, and only a tick on time
/// counts; the bound for "not answering" follows the round trip between
/// 1.5 and 6 s and the bound for giving a connection up is never under 10 s;
/// and what the screen says follows the one value. And the ordinary loss,
/// a station that ends and is started again (the socket closes): the page
/// says it is reconnecting, is connected again by itself, shows its pane
/// again and takes typing.
#[test]
#[ignore = "merge tier: starts a daemon twice and a browser"]
fn the_pages_rules_are_the_ones_written_down() {
    let mut s = Station::start("alpha");
    let page = browser::Page::open(&s, 1000, 700);
    let rules = page.js(
        "import('/api.js').then(m => ({ \
           retry: [0,1,2,3,4,5,6,7,8,40].map(n => [m.retryIn(n, 0), m.retryIn(n, 0.5), m.retryIn(n, 1)]), \
           tick: [[false,false,500],[false,true,500],[true,true,500],[true,false,500],[true,false,2000],[true,false,2001],[true,false,600000]].map(a => m.tickKind(...a)), \
           slow: [null, 0, 100, 375, 1000, 1500, 4000].map(m.slowAfter), \
           dead: [null, 100, 2000, 4000, 20000].map(m.deadAfter), \
           open: [0, 1, 2, 9].map(m.openWithin), signIn: [10, 4000].map(m.signInWithin), \
           median: [m.median([]), m.median([5]), m.median([9, 1, 5]), m.median([4, 1, 3, 2])], \
           level: [['ok',0,0],['ok',99999,99999],['closed',299,299],['closed',300,300],['closed',2999,2999],['silent',3000,3000],['closed',9999,9999],['closed',10000,10000],['closed',10000,59999],['closed',10000,60000],['closed',100,3600000],['silent',2999,10000]].map(a => m.linkLevel(...a)) }))",
    );
    assert_eq!(
        rules["retry"],
        json!([
            [0, 0, 0],
            [70, 100, 130],
            [140, 200, 260],
            [280, 400, 520],
            [560, 800, 1040],
            [1120, 1600, 2080],
            [2240, 3200, 4160],
            [3500, 5000, 6500],
            [3500, 5000, 6500],
            [3500, 5000, 6500]
        ])
    );
    assert_eq!(
        rules["tick"],
        json!(["hidden", "hidden", "woke", "run", "run", "woke", "woke"])
    );
    assert_eq!(
        rules["slow"],
        json!([1500, 1500, 1500, 1500, 4000, 6000, 6000])
    );
    assert_eq!(rules["dead"], json!([10000, 10000, 10000, 14000, 30000]));
    assert_eq!(rules["open"], json!([5000, 10000, 10000, 10000]));
    assert_eq!(rules["signIn"], json!([5000, 12000]));
    assert_eq!(rules["median"], json!([null, 5, 5, 2.5]));
    assert_eq!(rules["level"], json!([0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 2, 2]));

    page.js(API);
    s.kill9();
    let chip = page.wait("the top bar to say so", &shown(".link-chip"));
    assert!(chip.as_str().unwrap().starts_with("reconnecting"), "{chip}");
    std::thread::sleep(Duration::from_secs(2));
    s.launch();
    wait_for("the page to be connected again by itself", 15, || {
        let l = link(&page);
        (l["level"] == json!(0) && l["connected"] == json!(true)).then_some(())
    });
    assert_eq!(link(&page)["reconnects"], json!(1));
    // A daemon that was killed leaves no pane: one started now is shown
    // and typed into over the new connection.
    let pane = shell(&s, "again");
    page.goto(&format!("#/s/alpha/p/{pane}"));
    page.wait(
        "the pane to be attached",
        "document.querySelector('.pane')?.paneTerm?.chan != null",
    );
    page.js("(t => { t.term.focus(); t.term.input('echo again-$((40+2))\\n'); return true; })(document.querySelector('.pane').paneTerm)");
    wait_for("typing to reach the pane", 10, || {
        s.text(&pane).contains("again-42").then_some(())
    });
    assert_eq!(page.js(&shown(".link-chip")), json!(""));
}

/// A browser signed in from another device (a token the owner approved),
/// which is what can be handed on to another station's deck.
fn sign_in_as_device(page: &browser::Page, s: &Station) {
    let asked = page.js(
        "fetch('/api/device/request', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({name: 'the test browser'})}).then(r => r.json()).then(r => r.result)",
    );
    s.call("trust.approve", json!({"request": asked["code"]}));
    let got = page.js(&format!(
        "fetch('/api/device/collect', {{method: 'POST', headers: {{'content-type': 'application/json'}}, body: JSON.stringify({{id: {}, collect: {}}})}}).then(r => r.json()).then(r => r.result)",
        asked["id"], asked["collect"]
    ));
    let token = got["token"].as_str().expect("a token");
    page.js(&format!(
        "(localStorage.setItem('spandeck.token', {}), location.reload(), true)",
        json!(token)
    ));
    std::thread::sleep(Duration::from_millis(300));
    page.wait(
        "the deck again",
        "!!document.querySelector('.topbar') && !!document.getElementById('app').api?.connected",
    );
    page.js(API);
}

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

/// The audit's half-open case, for the page's own connection, at a
/// computer's width and at a phone's. Done when, with the link stalled
/// (nothing passes, nothing closes): within five seconds the top bar says
/// there is no answer and for how long, what is shown is marked "as of" the
/// time of the last answer, and the pane says typing is off; what is typed
/// from then on is not sent, then or later; by twelve seconds a bar names
/// the station, says the agents keep running, and offers the other
/// station's deck; after a minute it says when the station last answered;
/// and when the link returns the page is connected again by itself within
/// ten seconds, says it is back, attaches its pane again, and typing works.
/// Then, with the page hidden: a stall of fifteen seconds is not judged and
/// nothing is dropped; on being shown again while the link is still dead it
/// is found out within four seconds. And a page whose own script was held
/// up for four seconds drops nothing.
/// With `SPANDECK_SHOTS=<folder>` it also writes a picture of each state.
#[test]
#[ignore = "nightly: two daemons and a browser for three minutes, and it asserts on measured times"]
fn the_page_says_when_its_link_is_silent_and_comes_back_by_itself() {
    for (width, height, tag) in [(1300, 900, "desktop"), (390, 800, "phone")] {
        let (a, b) = two("");
        let pane = shell(&a, "here");
        b.fake(
            json!({"steps": [{"prompt": "x"}, {"sleep": 600_000}]}),
            json!({"name": "bob"}),
        );
        let wire = Link::to(a.port);
        let page = browser::Page::open_through(&a, wire.port, width, height);
        sign_in_as_device(&page, &a);
        page.wait(
            "bravo on the page",
            "document.body.innerText.includes('bravo')",
        );
        page.goto(&format!("#/s/alpha/p/{pane}"));
        page.wait("the pane", "!!document.querySelector('.pane .xterm-rows')");
        let type_in = |text: &str| {
            page.js(&format!(
                "(t => {{ t.term.focus(); t.term.input({}); return true; }})(document.querySelector('.pane').paneTerm)",
                json!(text)
            ));
        };
        wait_for("a heartbeat round trip", 10, || {
            link(&page)["rtt"].as_f64().map(|_| ())
        });
        type_in("echo before-$((40+2))\n");
        wait_for("the pane to answer", 10, || {
            a.text(&pane).contains("before-42").then_some(())
        });
        assert_eq!(link(&page)["level"], json!(0));
        picture(&page, &format!("{tag}-0-connected"));

        // ---- the link goes half-open
        wire.stall(true);
        let cut = Instant::now();
        let chip = page.wait("the top bar to say so", &shown(".link-chip"));
        let noticed = cut.elapsed();
        assert!(noticed < Duration::from_secs(5), "said after {noticed:?}");
        assert!(chip.as_str().unwrap().contains("no answer"), "{chip}");
        let as_of = page.wait("what is shown to be marked", &shown(".link-as-of"));
        assert!(as_of.as_str().unwrap().starts_with("as of "), "{as_of}");
        let note = page.wait("the pane to say typing is off", &shown(".pane .link-note"));
        assert!(note.as_str().unwrap().contains("typing is off"), "{note}");
        assert_eq!(link(&page)["live"], json!(false));
        picture(&page, &format!("{tag}-1-no-answer-5s"));
        type_in("echo during-$((40+3))\n");

        let bar = page.wait("the bar", &shown(".link-bar"));
        let barred = cut.elapsed();
        assert!(barred < Duration::from_secs(13), "the bar after {barred:?}");
        let bar = bar.as_str().unwrap().to_string();
        assert!(
            bar.contains("alpha has not answered for")
                && bar.contains("Your agents keep running there"),
            "{bar}"
        );
        assert!(bar.contains("Open bravo's deck"), "{bar}");
        // The connection was given up, not waited on for ever, and a new
        // one is being tried.
        wait_for("the silent connection to be given up", 5, || {
            (link(&page)["dropped"] == json!(1)).then_some(())
        });
        picture(&page, &format!("{tag}-2-bar-12s"));
        std::thread::sleep(Duration::from_secs(20));
        picture(&page, &format!("{tag}-3-bar-30s"));
        std::thread::sleep(Duration::from_secs(25));
        let late = page.wait(
            "the bar to say when it last answered",
            &format!(
                "(t => t.includes('last answered') ? t : '')({})",
                shown(".link-bar")
            ),
        );
        assert!(
            late.as_str().unwrap().contains("alpha last answered 1 min"),
            "{late}"
        );
        picture(&page, &format!("{tag}-4-bar-60s"));
        if tag == "desktop" {
            page.goto("#/fleet");
            page.wait("the tiles to be marked", &shown(".tile.station .as-of"));
            picture(&page, &format!("{tag}-5-fleet-as-of"));
            page.goto(&format!("#/s/alpha/p/{pane}"));
            page.wait("the pane", "!!document.querySelector('.pane .xterm-rows')");
        }

        // ---- the link returns
        wire.stall(false);
        let back = Instant::now();
        wait_for("the page to be connected again by itself", 15, || {
            let l = link(&page);
            (l["level"] == json!(0) && l["connected"] == json!(true)).then_some(())
        });
        let took = back.elapsed();
        assert!(took < Duration::from_secs(10), "back after {took:?}");
        page.wait(
            "it to say it is back",
            "[...document.querySelectorAll('.toast')].some(t => /alpha is back after 1 min/.test(t.innerText))",
        );
        picture(&page, &format!("{tag}-6-back"));
        page.wait(
            "the pane to be attached again",
            "(t => t.chan !== null && t.linkNote.hidden)(document.querySelector('.pane').paneTerm)",
        );
        type_in("echo after-$((40+4))\n");
        wait_for("typing to work again", 10, || {
            a.text(&pane).contains("after-44").then_some(())
        });
        // What was typed while the page said typing was off never arrived.
        std::thread::sleep(Duration::from_secs(1));
        assert!(!a.text(&pane).contains("during-43"), "{}", a.text(&pane));
        eprintln!("{tag}: said after {noticed:?}, the bar after {barred:?}, back after {took:?}");

        // ---- hidden: nothing is judged
        let before = link(&page);
        page.js("(Object.defineProperty(document, 'visibilityState', {get: () => window.__hidden ? 'hidden' : 'visible', configurable: true}), window.__hidden = true, document.dispatchEvent(new Event('visibilitychange')), true)");
        wire.stall(true);
        std::thread::sleep(Duration::from_secs(15));
        let hidden = link(&page);
        assert_eq!(
            (&hidden["dropped"], &hidden["state"], &hidden["level"]),
            (&before["dropped"], &json!("ok"), &json!(0)),
            "nothing is concluded while nobody looks: {hidden}"
        );
        // Shown again, the link still dead: one ping decides.
        page.js("(window.__hidden = false, document.dispatchEvent(new Event('visibilitychange')), true)");
        let shown_again = Instant::now();
        wait_for("the dead link to be found out", 6, || {
            (link(&page)["connected"] == json!(false)).then_some(())
        });
        assert!(shown_again.elapsed() < Duration::from_secs(4));
        wire.stall(false);
        wait_for("the page to be back", 15, || {
            (link(&page)["connected"] == json!(true)).then_some(())
        });

        // ---- the page's own script held up: not the link's silence
        std::thread::sleep(Duration::from_secs(4));
        let before = link(&page);
        page.js("(() => { const until = Date.now() + 4000; while (Date.now() < until); return true; })()");
        std::thread::sleep(Duration::from_secs(3));
        let after = link(&page);
        assert_eq!(
            (&after["dropped"], &after["reconnects"], &after["state"]),
            (&before["dropped"], &before["reconnects"], &json!("ok")),
            "{after}"
        );
        if tag == "desktop" {
            page.goto("#/perf");
            page.wait(
                "the heartbeat on the Performance page",
                &shown(".fact.heartbeat"),
            );
            page.js("(document.querySelector('.fact.heartbeat').scrollIntoView({block: 'center'}), true)");
            picture(&page, &format!("{tag}-7-performance"));
        }
    }
}

/// The second review's condition: a link that is slow and alive is not
/// taken down. Done when, over a link that takes two seconds each way, for
/// forty seconds: the page gives up no connection and makes no new one, its
/// round trip reads about four seconds, typing reaches the pane, and it
/// said "no answer" at most once (when the link first turned slow under
/// it).
#[test]
#[ignore = "nightly: a daemon and a browser for a minute, and it asserts on measured times"]
fn a_link_that_is_slow_and_alive_is_not_dropped() {
    let a = Station::start("alpha");
    let pane = shell(&a, "here");
    let wire = Link::to(a.port);
    let page = browser::Page::open_through(&a, wire.port, 1300, 900);
    page.js(API);
    page.goto(&format!("#/s/alpha/p/{pane}"));
    page.wait("the pane", "!!document.querySelector('.pane .xterm-rows')");
    page.wait(
        "the pane to be attached",
        "document.querySelector('.pane').paneTerm.chan !== null",
    );
    wire.slow(2000);
    std::thread::sleep(Duration::from_secs(20));
    page.js("(t => { t.term.focus(); t.term.input('echo slow-$((40+2))\\n'); return true; })(document.querySelector('.pane').paneTerm)");
    std::thread::sleep(Duration::from_secs(20));
    let l = link(&page);
    eprintln!("over a link 2 s each way: {l}");
    assert_eq!(
        (&l["dropped"], &l["reconnects"], &l["state"]),
        (&json!(0), &json!(0), &json!("ok")),
        "{l}"
    );
    assert!(l["silences"].as_u64().unwrap() <= 1, "{l}");
    assert!(
        l["rtt"]
            .as_f64()
            .is_some_and(|ms| (3500.0..6000.0).contains(&ms)),
        "{l}"
    );
    assert!(a.text(&pane).contains("slow-42"), "{}", a.text(&pane));
    picture(&page, "desktop-8-slow-link");
}

/// The audit's other half-open case: a pane on another station, shown
/// through this one, whose station freezes. Done when: within nine seconds
/// the pane itself says that station has not answered and that typing is
/// off, while the page's own connection and the pane of this station beside
/// it carry on; and when the station is back the pane is attached again by
/// itself and typing reaches it.
/// With `SPANDECK_SHOTS=<folder>` it also writes a picture of the state.
#[test]
#[ignore = "nightly: two daemons and a browser, a station frozen, and it asserts on measured times"]
fn a_pane_on_a_station_that_goes_silent_says_so_and_comes_back() {
    for (width, height, tag) in [(1300, 900, "desktop"), (390, 800, "phone")] {
        let (a, b) = two("");
        let bee = shell(&b, "bee");
        let page = browser::Page::open(&a, width, height);
        page.js(API);
        page.goto(&format!("#/s/bravo/p/{bee}"));
        page.wait(
            "the pane to be attached",
            "document.querySelector('.pane')?.paneTerm?.chan != null",
        );
        let type_in = |text: &str| {
            page.js(&format!(
                "(t => {{ t.term.focus(); t.term.input({}); return true; }})(document.querySelector('.pane').paneTerm)",
                json!(text)
            ));
        };
        type_in("echo before-$((40+2))\n");
        wait_for("the pane to answer", 10, || {
            b.text(&bee).contains("before-42").then_some(())
        });

        b.signal(SIGSTOP);
        b.wait_frozen();
        let frozen = Instant::now();
        let note = page.wait(
            "the pane to say its station is silent",
            "(n => n && !n.hidden && /has not answered/.test(n.innerText) ? n.innerText : '')(document.querySelector('.pane .term-note:not(.link-note)'))",
        );
        let told = frozen.elapsed();
        assert!(told < Duration::from_secs(9), "told after {told:?}");
        let note = note.as_str().unwrap().to_string();
        assert!(
            note.contains("bravo has not answered for") && note.contains("typing is off"),
            "{note}"
        );
        // This page's own connection is as it was.
        let l = link(&page);
        assert_eq!(
            (&l["state"], &l["dropped"]),
            (&json!("ok"), &json!(0)),
            "{l}"
        );
        std::thread::sleep(Duration::from_secs(3));
        picture(&page, &format!("{tag}-9-pane-elsewhere-silent"));

        b.signal(SIGCONT);
        let back = Instant::now();
        page.wait(
            "the pane to be attached again by itself",
            "(t => t.chan !== null && t.note.hidden)(document.querySelector('.pane').paneTerm)",
        );
        let took = back.elapsed();
        assert!(
            took < Duration::from_secs(12),
            "attached again after {took:?}"
        );
        type_in("echo after-$((40+3))\n");
        wait_for("typing to reach the pane again", 10, || {
            b.text(&bee).contains("after-43").then_some(())
        });
        eprintln!("{tag}: the pane was told after {told:?}, attached again after {took:?}");
    }
}
