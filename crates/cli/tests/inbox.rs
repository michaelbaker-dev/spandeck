//! The Inbox on a phone and on a computer (DECISIONS.md, "The Inbox is one
//! size of text, and nothing in it runs off its card"). The owner's report,
//! 16 Nov 2026, from an iPhone: "The inbox looks like random text sizes and
//! overruns the box it's in".
//!
//! Merge tier: a station and a browser, about five seconds.
//!
//!   cargo test -p spandeck --test inbox -- --ignored
//!
//! `SPANDECK_SHOTS=/some/dir` writes pictures for a human to look at.
#![cfg(unix)]
mod common;
use std::path::PathBuf;

use common::browser::Page;
use common::*;
use serde_json::{json, Value};

/// A station whose inbox holds each kind of notice the report showed: an
/// agent that needs you with a short reason and one with a long one, a
/// message, a station removed, stations asking to join under a very long
/// title, and a browser asking to sign in from an address. The first two
/// are raised by the station itself (a pane that asks a permission, and a
/// message to you); the rest are written into its database as it writes
/// them, because raising them takes a fleet.
fn station_with_an_inbox() -> (Station, String) {
    let s = Station::start("alpha");
    let pane = s.fake(
        json!({"steps": [{"prompt": "x"}, {"permission": {"name": "Write", "file": "f"}}, {"sleep": 600_000}]}),
        json!({"name": "dev2"}),
    );
    s.wait_state(&pane, "needs_you");
    s.call_as_pane(
        &pane,
        "msg.send",
        json!({"to": "you", "body": "13\nThe count is thirteen, and the rest of this message is long enough to need more lines than a phone gives a message before it is opened: /home/someone/code/a-project/with/a/long/path/that/has/no/spaces/in/it/at/all/anywhere.txt and then some more words, and more, and more again until six lines are full on a wide screen as well, which takes a good many words indeed, so here are some more of them, and a few after that, to be sure of it."}),
    )
    .unwrap();
    let notices: [(&str, &str, &str, Option<String>, i64); 9] = [
        ("needs_you", "antigravity needs you", "waiting for you", Some(format!("/#/s/alpha/p/{pane}")), 0),
        ("needs_you", "claude needs you", "asks you to trust spandeck's hooks in /home/someone/code/a-very-long-project-directory-name/with/a/deep/path/below/it before it runs anything there, and says why at some length", Some(format!("/#/s/alpha/p/{pane}")), 2),
        ("fleet.removed", "Station was removed from the fleet", "The stations of the fleet no longer accept workbench-in-the-back-room: its key was taken off every one of them, and what it held is kept there.", None, 2),
        ("trust.request", "Station Workbench-In-The-Back-Room and 1 other station (the-second-machine-of-that-fleet) ask to join your fleet, and this title is long enough to need more than two lines on a phone", "The fleet has a new request. Check that the code it shows is 4821-7733 before you approve it.", None, 2),
        ("trust.request", "A browser asks to sign in: Mac (Safari)", "From 100.109.214.37 fd7a:115c:a1e0:ab12:4843:cd96:626d:d625:0000:1111:2222:3333:4444:5555:6666:7777:8888:9999:aaaa:bbbb", None, 2),
        ("error", "builder hit an error", "", None, 1),
        ("done", "reviewer finished", "All 212 tests pass.", None, 0),
        ("needs_you", "planner needs you", "waiting for you", None, 0),
        ("done", "writer finished", "The draft is in docs/draft.md.", None, 0),
    ];
    let conn = rusqlite::Connection::open(s.paths().db()).unwrap();
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    for (i, (kind, title, body, url, pushed)) in notices.iter().enumerate() {
        conn.execute(
            "INSERT INTO notifications(id,ts,kind,pane,project,title,body,url,pushed) VALUES(?1,?2,?3,NULL,NULL,?4,?5,?6,?7)",
            rusqlite::params![format!("n-test-{i}"), now - 60_000 * (i as i64 + 1), kind, title, body, url, pushed],
        )
        .unwrap();
    }
    (s, pane)
}

fn shoot(page: &Page, name: &str) {
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let Some(dir) = std::env::var_os("SPANDECK_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
    std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
}

/// Open the Inbox once the deck has the station's notices (its button
/// counts the unread ones): the dialog shows what the deck held when it was
/// opened. Twelve: the nine written above, and the three the pane raised
/// (it needs you, it asks an approval, and its message).
fn open_inbox(page: &Page) {
    page.wait(
        "the Inbox, opened",
        "(b => document.querySelector('.inbox') ? true : (b && b.click(), false))([...document.querySelectorAll('.topbar button')].find(b => /^Inbox\\d+$/.test(b.innerText.replace(/\\s/g, ''))))",
    );
    let n = page.js("document.querySelectorAll('.inbox .inbox-note').length");
    assert_eq!(n, json!(12));
}

/// What the Inbox measures as: sizes of each part, what is wider than its
/// box, and what scrolls.
const MEASURED: &str = r#"(() => {
  const body = document.querySelector('.dialog-body'), box = body.getBoundingClientRect();
  const cs = getComputedStyle(body), right = box.right - parseFloat(cs.paddingRight), left = box.left + parseFloat(cs.paddingLeft);
  const sizes = (sel) => [...new Set([...body.querySelectorAll(sel)].map(e => getComputedStyle(e).fontSize + ' ' + getComputedStyle(e).fontWeight))];
  const items = [...body.querySelectorAll('.inbox-item')];
  return {
    items: items.length,
    title: sizes('.inbox-title'), text: sizes('.inbox-body'), meta: sizes('.inbox-meta'),
    wide: [body, ...items].filter(e => e.scrollWidth > e.clientWidth).map(e => e.className),
    outside: [...body.querySelectorAll('*')].filter(e => { const r = e.getBoundingClientRect(); return r.width && (r.right > right + 0.5 || r.left < left - 0.5); }).map(e => e.className || e.tagName),
    metaInside: items.every(i => { const m = i.querySelector('.inbox-meta'), r = m && m.getBoundingClientRect(), o = i.getBoundingClientRect(); return m && m.innerText.trim() && r.width > 0 && r.left >= o.left && r.right <= o.right + 0.5 && r.top >= o.top && r.bottom <= o.bottom + 0.5; }),
    short: [...body.querySelectorAll('button')].filter(b => b.getBoundingClientRect().height < 44).map(b => b.innerText),
    scrolls: (body.scrollTop = 60, body.scrollTop === 60 && body.scrollHeight > body.clientHeight),
    page: [document.scrollingElement.scrollHeight <= innerHeight, document.querySelector('.backdrop').scrollHeight <= document.querySelector('.backdrop').clientHeight],
  };
})()"#;

/// The notice whose title starts with `title`, as an expression.
fn note(title: &str) -> String {
    format!(
        "[...document.querySelectorAll('.inbox .inbox-note')].find(i => i.querySelector('.inbox-title').innerText.startsWith({}))",
        json!(title)
    )
}

/// Done when, at an iPhone's width, every title in the Inbox is one size
/// and weight, every body one smaller size, and every line of who and when
/// one size, on its own line inside its card; nothing is wider than the
/// dialog; a long body is cut to a few lines and a tap shows all of it; the
/// dialog's body scrolls and the page behind it does not; every button is a
/// finger tall; and the pane of a notice is still one tap away. On a
/// computer a notice is one line, cut with an ellipsis, and a click shows
/// the rest.
#[test]
#[ignore = "merge tier: a browser"]
fn the_inbox_is_one_size_of_text_and_nothing_runs_off_its_card() {
    let (s, pane) = station_with_an_inbox();
    let page = Page::open(&s, 390, 844);
    open_inbox(&page);
    shoot(&page, "phone");
    let m = page.js(MEASURED);
    assert_eq!(m["items"], json!(13), "twelve notices and a message: {m}");
    for part in ["title", "text", "meta"] {
        assert_eq!(m[part].as_array().map(Vec::len), Some(1), "{part}: {m}");
    }
    let px = |v: &Value| -> f64 {
        let s = v[0].as_str().unwrap();
        s.split("px").next().unwrap().parse().unwrap()
    };
    assert!(px(&m["text"]) < px(&m["title"]), "{m}");
    assert!(px(&m["meta"]) <= px(&m["text"]), "{m}");
    assert_eq!(
        (&m["wide"], &m["outside"], &m["metaInside"], &m["short"]),
        (&json!([]), &json!([]), &json!(true), &json!([])),
        "{m}"
    );
    assert_eq!(
        (&m["scrolls"], &m["page"]),
        (&json!(true), &json!([true, true])),
        "{m}"
    );

    // A long body is cut, and a tap on its card shows all of it, in place.
    // The tap also reads it: the card is no longer marked unread.
    let long = note("claude needs you");
    let cut = format!(
        "(i => {{ const b = i.querySelector('.inbox-body'), t = i.querySelector('.inbox-title'); return [b.scrollHeight > b.clientHeight + 1, t.scrollHeight > t.clientHeight + 1, i.classList.contains('open'), i.classList.contains('unread'), b.clientHeight]; }})({long})"
    );
    let before = page.js(&cut);
    assert_eq!(
        before.as_array().unwrap()[..4],
        [json!(true), json!(false), json!(false), json!(true)],
        "{before}"
    );
    let long_title = page.js(&format!(
        "(t => t.scrollHeight > t.clientHeight + 1)({}.querySelector('.inbox-title'))",
        note("Station Workbench")
    ));
    assert_eq!(
        long_title,
        json!(true),
        "a title of more than two lines is cut"
    );
    page.js(&format!("{long}.querySelector('.inbox-main').click()"));
    let after = page.js(&cut);
    assert_eq!(
        after.as_array().unwrap()[..4],
        [json!(false), json!(false), json!(true), json!(false)],
        "{after}"
    );
    assert!(after[4].as_f64() > before[4].as_f64(), "{before} {after}");
    shoot(&page, "phone-open");
    let m = page.js(MEASURED);
    assert_eq!(
        (&m["wide"], &m["outside"]),
        (&json!([]), &json!([])),
        "opened: {m}"
    );
    s.wait_for_read("n-test-1");

    // The pane a notice is about is one tap away, as it was.
    page.js(&format!(
        "[...{}.querySelectorAll('button')].find(b => b.innerText === 'Open').click()",
        note("antigravity needs you")
    ));
    page.wait(
        "the pane of the notice",
        &format!("location.hash.endsWith('/p/{pane}') && !document.querySelector('.inbox')"),
    );

    // The dialogs beside it, on the same phone: nothing wider than its box.
    page.goto("#/fleet");
    page.wait(
        "Approval requests",
        "(b => document.querySelector('.dialog .approval') ? true : (b && b.click(), false))([...document.querySelectorAll('.topbar button')].find(b => b.innerText.startsWith('Approvals')))",
    );
    shoot(&page, "phone-approvals");
    let wide = page.js("[document.querySelector('.dialog-body'), ...document.querySelectorAll('.dialog .approval, .phone-home .approval, .phone-home .phone-row')].filter(e => e.scrollWidth > e.clientWidth).map(e => e.className)");
    assert_eq!(wide, json!([]));

    // On a computer: one line each, cut with an ellipsis; a click shows it.
    let desk = Page::open(&s, 1280, 800);
    open_inbox(&desk);
    shoot(&desk, "desktop");
    let m = desk.js(MEASURED);
    for part in ["title", "text", "meta"] {
        assert_eq!(m[part].as_array().map(Vec::len), Some(1), "{part}: {m}");
    }
    assert_eq!((&m["wide"], &m["outside"]), (&json!([]), &json!([])), "{m}");
    let long = note("Station Workbench");
    let line = format!(
        "(i => {{ const [t, b, w] = ['.inbox-title', '.inbox-body', '.inbox-meta'].map(q => i.querySelector(q)), r = (e) => e.getBoundingClientRect(), c = getComputedStyle(t); return [r(t).bottom > r(b).top && r(b).bottom > r(w).top && r(t).right <= r(b).left && r(b).right <= r(w).left, c.whiteSpace + ' ' + c.textOverflow, t.scrollWidth > t.clientWidth, t.title === t.innerText]; }})({long})"
    );
    assert_eq!(desk.js(&line), json!([true, "nowrap ellipsis", true, true]));
    desk.js(&format!("{long}.querySelector('.inbox-main').click()"));
    let open = desk.js(&format!(
        "(t => [t.scrollWidth <= t.clientWidth, t.scrollHeight <= t.clientHeight + 1])({long}.querySelector('.inbox-title'))"
    ));
    assert_eq!(open, json!([true, true]));
    shoot(&desk, "desktop-open");
}

trait Read {
    fn wait_for_read(&self, id: &str);
}
impl Read for Station {
    /// The notice is marked read at the station.
    fn wait_for_read(&self, id: &str) {
        wait_for("the notice read at the station", 10, || {
            self.call("notify.list", json!({}))
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["id"] == id && n["read"] == true)
                .then_some(())
        });
    }
}
