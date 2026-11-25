//! Finding a station in the list (DECISIONS.md, "Finding a station in the
//! list", 25 Nov 2026). The owner, with nine stations: "I find it hard to
//! find the machine I am looking for in the list on the left of spandeck."
//! Each station has a colour, the pinned ones come first in the owner's
//! order, and with "Fold quiet stations" ticked the ones with nothing
//! running are one row.
//!
//! With `SPANDECK_SHOTS` set, both tests write pictures there for a person
//! to look at.
// The fake agent that asks for a permission is a Unix shell's.
#![cfg(unix)]
mod common;
use common::browser::Page;
use common::*;
use serde_json::{json, Value};

fn shot(page: &Page, name: &str) {
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    if let Some(out) = std::env::var_os("SPANDECK_SHOTS").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&out).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(450));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("stations-{name}.png")), png).unwrap();
    }
}

/// Stations of one fleet, each of which sees all the others.
fn fleet(names: &[&str]) -> Vec<Station> {
    let key = "5a".repeat(32);
    let listed: Vec<(&str, Option<&str>)> =
        names.iter().map(|n| (*n, Some(key.as_str()))).collect();
    let all = Station::several(&listed);
    for s in &all {
        wait_for("the stations to find each other", 30, || {
            let v = s.call("fleet.refresh", json!({}));
            let on = |x: &&Value| x["state"] == json!("online");
            (v["stations"].as_array().unwrap().iter().filter(on).count() == names.len())
                .then_some(())
        });
    }
    all
}

/// What the fleet keeps of one of the owner's preferences, at `s`.
fn kept(s: &Station, key: &str) -> Value {
    s.call("prefs.get", json!({}))["values"][key].clone()
}

/// The stations of the list on the left, in the order they are shown.
const SIDE: &str = "[...document.querySelectorAll('.side-stations .side-station-group')].map(g => g.dataset.station)";

fn side_row(station: &str) -> String {
    format!("[...document.querySelectorAll('.side-station-group')].find(g => g.dataset.station === '{station}')?.querySelector('.side-station')")
}

/// The computer's list: a colour for each station (its own, and one the
/// owner chooses), the pinned ones first in the owner's order, and the
/// quiet ones folded when the owner ticks it; never a pinned one, the one
/// the page is on, or one that needs the owner.
#[test]
fn the_list_has_a_colour_for_each_station_the_pinned_first_and_the_quiet_folded() {
    let all: [Station; 4] = fleet(&["alpha", "bravo", "charlie", "delta"])
        .try_into()
        .ok()
        .unwrap();
    let [a, mut b, _c, d] = all;
    let (a, d) = (&a, &d);
    // bravo is in use; delta has an agent that waits for the owner; alpha
    // (which serves the page) and charlie run nothing.
    b.call("pane.spawn", json!({"agent": "shell", "name": "work"}));
    let asker = d.fake(
        json!({"steps": [{"prompt": "x"}, {"permission": {"name": "Bash", "command": "rm -rf build"}}, {"turn_end": "ok"}]}),
        json!({"name": "asker"}),
    );
    d.wait_state(&asker, "needs_you");

    // A document with none of these preferences goes to every station as before.
    let before = a.call(
        "prefs.set",
        json!({"key": "newPane.resumeLast", "value": true}),
    );
    assert_eq!(before["format"], json!(1));

    let page = Page::open(a, 1300, 800);
    page.wait(
        "the four stations by name, each with what runs there",
        &format!("JSON.stringify({SIDE}) === '[\"alpha\",\"bravo\",\"charlie\",\"delta\"]' && document.querySelectorAll('.side-station-group .side-pane').length === 2"),
    );

    // ---- A colour for each: one of the palette, the row tinted with it,
    // and the line its panes hang from. None stored: it is the name's.
    let colours = page.js(
        "[...document.querySelectorAll('.side-station-group')].map(g => { const row = g.querySelector('.side-station'), kids = g.querySelector(':scope > .side-kids');
           return { st: g.style.getPropertyValue('--st'), row: getComputedStyle(row).backgroundColor, panel: getComputedStyle(document.querySelector('.sidebar')).backgroundColor,
             line: getComputedStyle(kids).borderLeftColor, plain: getComputedStyle(document.querySelector('.sidebar')).borderRightColor }; })",
    );
    for c in colours.as_array().unwrap() {
        assert!(c["st"].as_str().unwrap().starts_with('#'), "{c}");
        assert_ne!(c["row"], c["panel"], "the name is on its colour: {c}");
        assert_ne!(
            c["line"], c["plain"],
            "its panes hang from a line of it: {c}"
        );
    }
    assert!(kept(a, "stations.colour").is_null(), "nothing is stored");
    let auto = page.js(&format!(
        "{}.parentElement.style.getPropertyValue('--st')",
        side_row("bravo")
    ));
    shot(&page, "computer-fold-off");

    // The owner chooses one: it is the fleet's, and Automatic puts it back.
    page.right_click(&side_row("bravo"));
    page.choose("Colour");
    page.wait(
        "the colours to choose from",
        "document.querySelectorAll('.st-swatch').length === 9 && document.querySelector('.st-swatch[data-colour=auto]').getAttribute('aria-pressed') === 'true'",
    );
    shot(&page, "computer-colours");
    let other = if auto == json!("#cdaa7d") {
        ("teal", "#3fc8c0")
    } else {
        ("tan", "#cdaa7d")
    };
    page.js(&format!(
        "(document.querySelector('.st-swatch[data-colour={}]').click(), true)",
        other.0
    ));
    page.wait(
        "bravo in the colour chosen",
        &format!(
            "{}?.parentElement.style.getPropertyValue('--st') === '{}'",
            side_row("bravo"),
            other.1
        ),
    );
    wait_for("the colour to be the fleet's", 10, || {
        (kept(d, "stations.colour") == json!({"bravo": other.0})).then_some(())
    });
    // A station from before these preferences would drop them from its copy
    // and hand that copy on: a document that holds one is in the format such
    // a station is never offered (prefs.rs).
    assert_eq!(a.call("prefs.get", json!({}))["format"], json!(2));
    page.right_click(&side_row("bravo"));
    page.choose("Colour");
    page.wait("the colours again", "(b => b ? (b.click(), true) : false)(document.querySelector('.st-swatch[data-colour=auto]'))");
    page.wait(
        "bravo in its own colour again",
        &format!(
            "{}?.parentElement.style.getPropertyValue('--st') === {auto}",
            side_row("bravo")
        ),
    );

    // ---- Fold quiet stations: off until ticked. alpha and charlie run
    // nothing and become one row; bravo is in use and delta needs the owner.
    assert_eq!(
        page.js("document.querySelector('.fold-opt input').checked"),
        json!(false)
    );
    assert_eq!(
        page.js("!!document.querySelector('.st-quiet')"),
        json!(false)
    );
    page.js("(document.querySelector('.fold-opt input').click(), true)");
    page.wait(
        "the quiet two in one row, at the end",
        &format!("JSON.stringify({SIDE}) === '[\"bravo\",\"delta\"]' && document.querySelector('.side-stations > :last-child.st-quiet')?.innerText.includes('2 quiet stations') && document.querySelectorAll('.st-quiet .st-dots i').length === 2"),
    );
    wait_for("the tick to be the fleet's", 10, || {
        (kept(&b, "stations.foldQuiet") == json!(true)).then_some(())
    });
    shot(&page, "computer-fold-on");
    // The station the page is on is never folded.
    page.goto("#/s/charlie");
    page.wait(
        "charlie on the list while the page is on it",
        &format!("JSON.stringify({SIDE}) === '[\"bravo\",\"charlie\",\"delta\"]' && document.querySelector('.st-quiet')?.innerText.includes('1 quiet station')"),
    );
    shot(&page, "computer-on-a-station");
    page.goto("#/fleet");
    page.wait("charlie folded again", &format!("{SIDE}.length === 2"));
    // The row opens in place, by name, and shuts again.
    page.js("(document.querySelector('.st-quiet').click(), true)");
    page.wait(
        "the quiet ones under their row",
        &format!("JSON.stringify({SIDE}) === '[\"bravo\",\"delta\",\"alpha\",\"charlie\"]' && document.querySelector('.st-quiet').getAttribute('aria-expanded') === 'true'"),
    );
    shot(&page, "computer-fold-open");

    // ---- Pinned to top: first, in the owner's order, with a mark; a pinned
    // station is never folded, quiet as it is.
    page.right_click(&side_row("charlie"));
    page.choose("Pin to top");
    page.wait(
        "charlie first",
        &format!("JSON.stringify({SIDE}) === '[\"charlie\",\"bravo\",\"delta\",\"alpha\"]' && !!{}.querySelector('.st-pin')", side_row("charlie")),
    );
    page.js("(document.querySelector('.st-quiet').click(), true)");
    page.wait(
        "charlie on the list with the row shut",
        &format!("JSON.stringify({SIDE}) === '[\"charlie\",\"bravo\",\"delta\"]'"),
    );
    page.right_click(&side_row("delta"));
    page.choose("Pin to top");
    page.wait(
        "delta after charlie",
        &format!("JSON.stringify({SIDE}) === '[\"charlie\",\"delta\",\"bravo\"]'"),
    );
    page.right_click(&side_row("delta"));
    page.choose("Move up");
    page.wait(
        "delta before charlie",
        &format!("JSON.stringify({SIDE}) === '[\"delta\",\"charlie\",\"bravo\"]'"),
    );
    wait_for("the order to be the fleet's", 10, || {
        (kept(&b, "stations.pinned") == json!(["delta", "charlie"])).then_some(())
    });
    assert_eq!(
        page.js("[...document.querySelectorAll('.side-station')].map(r => !!r.querySelector('.st-pin'))"),
        json!([true, true, false])
    );
    // The section called "Pinned" is panes and layouts, and is not this.
    assert_eq!(
        page.js("!!document.querySelector('.side-pinned')"),
        json!(false)
    );
    shot(&page, "computer-pinned");
    page.right_click(&side_row("delta"));
    page.choose("Unpin from top");
    page.wait(
        "delta by name again",
        &format!("JSON.stringify({SIDE}) === '[\"charlie\",\"bravo\",\"delta\"]'"),
    );

    // ---- A station that is away is quiet, in use as it was.
    b.kill9();
    wait_for("alpha to find bravo away", 30, || {
        let v = a.call("fleet.refresh", json!({}));
        let away = |s: &Value| s["station"] == json!("bravo") && s["state"] == json!("unreachable");
        v["stations"]
            .as_array()
            .unwrap()
            .iter()
            .any(away)
            .then_some(())
    });
    page.wait(
        "bravo folded with alpha",
        &format!("JSON.stringify({SIDE}) === '[\"charlie\",\"delta\"]' && document.querySelector('.st-quiet')?.innerText.includes('2 quiet stations')"),
    );

    // Unticked: every station on the list, as before.
    page.js("(document.querySelector('.fold-opt input').click(), true)");
    page.wait(
        "all four, and no row of quiet ones",
        &format!("JSON.stringify({SIDE}) === '[\"charlie\",\"alpha\",\"bravo\",\"delta\"]' && !document.querySelector('.st-quiet') && {}.classList.contains('away')", side_row("bravo")),
    );
    shot(&page, "computer-away");
    // For a person to look at: the same in a light theme.
    if std::env::var_os("SPANDECK_SHOTS").is_some() {
        a.call(
            "look.set",
            json!({"level": "deck", "choice": {"theme": "spandeck-light"}}),
        );
        page.wait(
            "the light theme",
            "document.documentElement.hasAttribute('data-themed')",
        );
        page.goto("#/s/charlie");
        shot(&page, "computer-light");
    }
}

/// The phone's "By station" list: a tile for each station in its colour, in
/// the list's order, the quiet ones one tile when the owner ticks it, each
/// station's block under its colour, and a pane's bar says whose it is.
#[test]
fn on_a_phone_stations_are_tiles_and_a_pane_says_whose_it_is() {
    let all = fleet(&["alpha", "bravo", "charlie"]);
    let (a, b) = (&all[0], &all[1]);
    let worker = b.fake(
        json!({"steps": [{"prompt": "x"}, {"work": 120000}]}),
        json!({"name": "worker"}),
    );
    b.wait_state(&worker, "working");

    let page = Page::open(a, 390, 844);
    page.phone_screen();
    page.wait("the list by station", "(b => b ? (b.click(), true) : false)(document.querySelector('[data-keep=\"home:stations\"]'))");
    const TILES: &str = "[...document.querySelectorAll('.st-tiles .st-tile:not(.wide)')].map(t => t.dataset.station)";
    const BLOCKS: &str = "[...document.querySelectorAll('.phone-station[data-station]')].map(t => t.dataset.station)";
    let tile = |st: &str| {
        format!(
            "[...document.querySelectorAll('.st-tile')].find(t => t.dataset.station === '{st}')"
        )
    };
    page.wait(
        "a tile for each station, and bravo's says what runs there",
        &format!("JSON.stringify({TILES}) === '[\"alpha\",\"bravo\",\"charlie\"]' && /1 pane\\s*working/.test({}.innerText) && !!{}.querySelector('.dot.working')", tile("bravo"), tile("bravo")),
    );
    assert_eq!(page.js(BLOCKS), json!(["alpha", "bravo", "charlie"]));
    // Each tile and each block's band is on its station's colour.
    let same = page.js(&format!(
        "{TILES}.every(st => {{ const t = [...document.querySelectorAll('.st-tile')].find(x => x.dataset.station === st), blk = [...document.querySelectorAll('.phone-station')].find(x => x.dataset.station === st), panel = getComputedStyle(document.documentElement).getPropertyValue('--panel');
           return t.style.getPropertyValue('--st').startsWith('#') && t.style.getPropertyValue('--st') === blk.style.getPropertyValue('--st') && getComputedStyle(t).backgroundColor === getComputedStyle(blk.querySelector('.phone-station-head')).backgroundColor && getComputedStyle(t).backgroundColor !== 'rgb(22, 28, 35)'; }})"
    ));
    assert_eq!(same, json!(true));
    // A thumb's size, and nothing wider than the screen.
    let fits = "document.documentElement.scrollWidth <= 390 && document.querySelector('.phone').scrollWidth <= 390";
    assert_eq!(page.js(fits), json!(true));
    assert_eq!(
        page.js("[...document.querySelectorAll('.st-tile, .fold-opt')].every(x => x.getBoundingClientRect().height >= 44)"),
        json!(true)
    );
    shot(&page, "phone-fold-off");

    // What the owner pinned (on any station) is first here too.
    a.call(
        "prefs.set",
        json!({"key": "stations.pinned", "value": ["charlie"]}),
    );
    page.wait("charlie's tile first", &format!("JSON.stringify({TILES}) === '[\"charlie\",\"alpha\",\"bravo\"]' && !!{}.querySelector('.st-pin')", tile("charlie")));
    assert_eq!(page.js(BLOCKS), json!(["charlie", "alpha", "bravo"]));

    // The same tick: alpha runs nothing and is one tile, its block out of
    // the way; charlie is pinned and bravo is in use.
    page.js("(document.querySelector('.fold-opt input').click(), true)");
    page.wait(
        "the quiet one as a tile of its own",
        &format!("JSON.stringify({TILES}) === '[\"charlie\",\"bravo\"]' && /1 quiet station\\s*show/.test(document.querySelector('.st-tile.wide')?.innerText) && JSON.stringify({BLOCKS}) === '[\"charlie\",\"bravo\"]'"),
    );
    wait_for("the tick to be the fleet's", 10, || {
        (kept(b, "stations.foldQuiet") == json!(true)).then_some(())
    });
    assert_eq!(page.js(fits), json!(true));
    shot(&page, "phone-fold-on");
    page.js("(document.querySelector('.st-tile.wide').click(), true)");
    page.wait(
        "alpha shown again, after the others",
        &format!("JSON.stringify({TILES}) === '[\"charlie\",\"bravo\",\"alpha\"]' && JSON.stringify({BLOCKS}) === '[\"charlie\",\"bravo\",\"alpha\"]'"),
    );
    shot(&page, "phone-fold-open");
    // A tap on a tile brings that station's block onto the screen.
    page.js(&format!("({}.click(), true)", tile("alpha")));
    page.wait(
        "alpha's block on screen",
        "(r => r && r.top >= 0 && r.bottom <= innerHeight)([...document.querySelectorAll('.phone-station')].find(x => x.dataset.station === 'alpha')?.getBoundingClientRect())",
    );

    // ---- A pane: the one bar at its top is on its station's colour and
    // says the station first.
    let colour = page.js(&format!("{}.style.getPropertyValue('--st')", tile("bravo")));
    page.goto(&format!("#/s/bravo/p/{worker}"));
    page.wait(
        "the pane's bar on bravo's colour",
        &format!("(bar => bar && bar.style.getPropertyValue('--st') === {colour} && bar.querySelector('.st-name')?.innerText === 'bravo' && getComputedStyle(bar).backgroundColor !== 'rgb(22, 28, 35)')(document.querySelector('.phone .chat-bar.st-bar'))"),
    );
    assert_eq!(
        page.js("document.querySelectorAll('.phone > header, .phone > .row').length"),
        json!(1),
        "one bar, as before"
    );
    assert_eq!(page.js(fits), json!(true));
    shot(&page, "phone-pane");
    if std::env::var_os("SPANDECK_SHOTS").is_some() {
        a.call(
            "look.set",
            json!({"level": "deck", "choice": {"theme": "spandeck-light"}}),
        );
        page.wait(
            "the light theme",
            "document.documentElement.hasAttribute('data-themed')",
        );
        shot(&page, "phone-pane-light");
        page.goto("#/fleet");
        page.wait("the list", "document.querySelector('.st-tiles')");
        shot(&page, "phone-light");
    }
}
