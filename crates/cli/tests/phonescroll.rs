//! A terminal's history on a phone is scrolled by the phone (DECISIONS.md,
//! "On a phone a terminal's history is scrolled by the phone").
//!
//! The owner, 16 Nov 2026, on an iPhone, after a drag had been made to follow
//! the finger by the page's own arithmetic: "On the phone, all terminals are
//! choppy still, not like the chat window which is smooth.. have to fix
//! terminal to be like chat." No iPhone can be driven from here. What can be
//! shown is the structure that makes the chat smooth: the history is rows
//! that stay as drawn, inside an element the browser scrolls itself; a real
//! touch (the browser's own, not an event the page made up) scrolls it; no
//! move of the finger is refused; and the terminal draws nothing while it
//! moves.
//!
//! The scenario that runs on every change is in phase5.rs, beside the other
//! scenarios of a phone, so that it runs alongside them
//! (`a_terminals_history_on_a_phone_is_scrolled_by_the_phone`). Here: the
//! rest of it, for the merge tier, and the measure.
mod common;
use std::time::Duration;

use common::browser::{Page, T, TOP_ROW, WATCH};
use common::*;
use serde_json::{json, Value};

/// A phone: its width, and touches.
fn phone_of(s: &Station) -> Page {
    let page = Page::open(s, 390, 844);
    page.touches();
    page
}

fn shell(s: &Station, name: &str) -> String {
    s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": name, "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn paste(s: &Station, pane: &str, text: &str) {
    s.call(
        "pane.paste",
        json!({"pane": pane, "text": text, "enter": true}),
    );
}

/// A finger on the terminal, moved by (dx, dy) and lifted: a real touch.
fn drag(page: &Page, dx: f64, dy: f64, steps: u32) {
    page.touch_drag(&format!("{T}.body"), dx, dy, steps);
}

/// The rest: a pane another window holds (wider and taller than the phone),
/// the whole width, a full-screen program that listens for the mouse, a
/// program that prints faster than rows are made, and the lines the page
/// was not sent.
#[test]
#[ignore = "merge tier: the rest of a terminal's history on a phone, in a browser"]
fn the_rest_of_a_terminals_history_on_a_phone() {
    let s = Station::start("alpha");
    let pane = shell(&s, "wide");
    // A computer has the pane open, and so holds its size.
    let desk = Page::open(&s, 1300, 800);
    desk.goto(&format!("#/s/alpha/p/{pane}"));
    desk.wait(
        "the pane on the computer",
        "!!document.querySelector('.pane .xterm-rows')",
    );
    wait_for("the computer to hold the pane", 10, || {
        (s.pane(&pane)["cols"].as_u64().unwrap() > 100).then_some(())
    });
    paste(&s, &pane, "seq 1000 1400");
    s.wait_text(&pane, "1400");

    let phone = phone_of(&s);
    phone.goto(&format!("#/s/alpha/p/{pane}"));
    // Watched: at the phone's text size, wider and taller than its frame,
    // with the row the cursor is on in sight.
    let seen = phone.wait(
        "the pane watched, its history above it",
        &format!("(t => !!t && t.chan !== null && t.body.classList.contains('natural') && t.hist.lines > 300 && !t.hist.pending() && (b => (m => m.liveH > b.clientHeight && b.scrollTop >= m.liveTop && [b.scrollWidth > b.clientWidth + 100, getComputedStyle(b).overflowX, getComputedStyle(b).touchAction, t.hist.el.offsetWidth === t.term.element.offsetWidth,
            m.liveTop + (t.term.buffer.active.cursorY + 1) * m.lh <= b.scrollTop + b.clientHeight + 1])(t._metrics()))(t.body))({T})"),
    );
    assert_eq!(seen, json!([true, "auto", "manipulation", true, true]));
    // Sideways and up, by the browser.
    phone.js(&format!("{WATCH}({T})"));
    drag(&phone, -200.0, 0.0, 16);
    phone.wait(
        "the pane moved sideways",
        &format!("{T}.body.scrollLeft > 100"),
    );
    let top = phone.js(&format!("{T}.body.scrollTop")).as_f64().unwrap();
    drag(&phone, 0.0, 250.0, 16);
    drag(&phone, 0.0, 250.0, 16);
    phone.wait(
        "earlier output",
        &format!("(t => !t.following && t.body.scrollTop < {top} - 300)({T})"),
    );
    assert_eq!(
        phone.js("[seen.moves.some(e => e.defaultPrevented), seen.changed, seen.drawn]"),
        json!([false, 0, 0])
    );

    // The whole width: everything smaller, nothing to scroll sideways, the
    // history still above the screen and still the browser's to scroll.
    let button = |text: &str| {
        format!("(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.phone-pane button')].find(b => b.innerText.trim() === '{text}'))")
    };
    phone.wait("Show whole width", &button("Show whole width"));
    let whole = phone.wait(
        "the pane whole",
        &format!("(t => t.body.classList.contains('letterbox') && t._hs < 0.6 && (b => [getComputedStyle(b).overflowX === 'hidden' && b.scrollLeft === 0, Math.abs(t.hist.el.getBoundingClientRect().width - b.clientWidth) < 2 && Math.abs(t.hist.el.getBoundingClientRect().left - b.getBoundingClientRect().left) < 2 && Math.abs(t.term.element.getBoundingClientRect().left - b.getBoundingClientRect().left) < 2, b.scrollHeight > b.clientHeight + 500,
            Math.abs(t.hist.el.getBoundingClientRect().bottom - t.term.element.getBoundingClientRect().top) < 1, getComputedStyle(b).overflowY])(t.body))({T})"),
    );
    assert_eq!(whole, json!([true, true, true, true, "auto"]));
    phone.js(&format!("{T}.toEnd()"));
    let top = phone.js(&format!("{T}.body.scrollTop")).as_f64().unwrap();
    drag(&phone, 0.0, 250.0, 16);
    drag(&phone, 0.0, 250.0, 16);
    phone.wait(
        "the whole pane's earlier output",
        &format!("(t => !t.following && !!t._read && t.body.scrollTop < {top} - 300)({T})"),
    );
    phone.wait("Show at text size", &button("Show at text size"));
    phone.wait(
        "the pane at text size again",
        &format!("(t => t.body.classList.contains('natural') && t._hs === 1 && t.hist.el.style.transform === '')({T})"),
    );

    // A program prints seven thousand lines, more than the terminal keeps,
    // while earlier output is being read: what is being read stays where it
    // is (up to ten thousand lines are kept for a reader).
    let reading = phone.wait(
        "earlier output being read",
        &format!("(t => !t.following && !!t._read && [t.body.scrollTop, {TOP_ROW}(t)])({T})"),
    );
    paste(&s, &pane, "seq 10001 17000");
    s.wait_text(&pane, "17000");
    let read = phone.wait(
        "the flood, seen from earlier output",
        &format!("(t => !t.pill.hidden && !t.hist.pending() && t.body.querySelector('.xterm-rows').innerText.includes('17000') && [t.body.scrollTop, {TOP_ROW}(t), t.hist.lines > 5000 && t.hist.lines <= 10100])({T})"),
    );
    assert_eq!(read, json!([reading[0], reading[1], true]));
    // Back at the live screen: the rows are the terminal's last lines, none
    // of them lost, and no more than five thousand (whole blocks of a
    // hundred go from the top).
    phone.js(&format!("{T}.pill.click()"));
    let kept = phone.wait(
        "the history after a flood",
        &format!("(t => t.following && (rows => [t.hist.lines <= 5000 && t.hist.lines >= 4900, rows.length === t.hist.lines, t.hist.chunks.some(c => c.gone),
            rows.slice(-200).every((r, i, all) => i === 0 || Number(r.textContent) === Number(all[i - 1].textContent) + 1), Math.abs(t.body.scrollTop - t._metrics().liveTop) < 700])([...t.hist.el.querySelectorAll('.hist-row')]))({T})"),
    );
    assert_eq!(kept, json!([true, true, false, true, true]));

    // A full-screen program that listens for the mouse is told to scroll
    // with wheel turns.
    paste(
        &s,
        &pane,
        "printf '\\033[?1049h\\033[H\\033[?1000h\\033[?1006h'; cat -v",
    );
    phone.wait(
        "the full-screen program",
        &format!("(t => t._alt && t._driven && t.term.modes.mouseTrackingMode !== 'none')({T})"),
    );
    // (The screen is taller than its frame: a drag first moves it in the
    // frame, then goes to the program.)
    drag(&phone, 0.0, -300.0, 12);
    drag(&phone, 0.0, -300.0, 12);
    s.wait_text(&pane, "^[[<65;");
    // (A touch stops what was still going on after the finger lifted, so
    // that no wheel turn arrives at the shell once the program has ended.)
    drag(&phone, 0.0, 0.0, 1);
    std::thread::sleep(Duration::from_millis(300));
    for _ in 0..2 {
        s.call("pane.input", json!({"pane": pane, "data": "\u{3}"}));
    }
    paste(&s, &pane, "printf '\\033[?1000l\\033[?1006l\\033[?1049l'");
    phone.wait(
        "the history again",
        &format!("(t => !t._alt && !t._driven && t.following)({T})"),
    );

    // Opened again, the page is sent the last 2,000 lines; the station has
    // 5,000, and gives the rest as text when asked.
    phone.reload();
    phone.wait(
        "the pane again, with the offer of earlier output",
        &format!("(t => !!t && t.chan !== null && t.body.classList.contains('natural') && t.hist.lines >= 1990 && t.hist.lines <= 2100 && !t.hist.pending() && !t.hist.top.hidden && !!t.hist.top.querySelector('button'))({T})"),
    );
    phone.js(&format!("(t => {{ t.body.scrollTop = 0; }})({T})"));
    let first = phone.wait(
        "the top of what the page has",
        &format!("(t => !t.following && t.hist.firstTexts(1)[0])({T})"),
    );
    phone.js(&format!("{T}.hist.top.querySelector('button').click()"));
    let got = phone.wait(
        "the earlier lines",
        &format!("(t => t.hist.lines > 4900 && [t.hist.top.textContent, t.hist.firstTexts(1)[0], {TOP_ROW}(t), t.following])({T})"),
    );
    assert!(
        got[0].as_str().unwrap().contains("from alpha's own record")
            && got[0].as_str().unwrap().contains("without colours"),
        "{got}"
    );
    // Its first line is three thousand earlier; and the line that was at
    // the top of the frame is still there.
    let n = |v: &Value| v.as_str().unwrap().trim().parse::<i64>().unwrap();
    assert!(n(&got[1]) < n(&first) - 2900, "{got} after {first}");
    // (Three rows above it, where the offer was, are the lines before it.)
    assert_eq!(
        (n(&got[2]), &got[3]),
        (n(&first) - 3, &json!(false)),
        "{got}"
    );
}

/// A measure for docs/measurements.md: three quick drags over 3,000 lines of
/// history, with the page's processor slowed (`SPANDECK_THROTTLE`, 6 times
/// unless set), and what the page's own thread had to do meanwhile. It uses
/// nothing of the page but the terminal, so it runs the same against the
/// deck as it was before the history was the browser's to scroll.
#[test]
#[ignore = "takes measurements for a person to read; needs Chrome"]
fn measure_scrolling_a_terminals_history_on_a_phone() {
    let rate: f64 = std::env::var("SPANDECK_THROTTLE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6.0);
    let s = Station::start("alpha");
    let pane = shell(&s, "hist");
    let phone = phone_of(&s);
    phone.goto(&format!("#/s/alpha/p/{pane}"));
    phone.wait(
        "the terminal fitted to the phone",
        &format!("(t => !!t && t.control && t.chan !== null && t.term.cols < 60)({T})"),
    );
    // Lines with colours and bold, as an agent prints; or (`SPANDECK_PLAIN`)
    // numbers only, under which the deck as it was before does not lose the
    // finger (see docs/measurements.md).
    let plain = std::env::var("SPANDECK_PLAIN").is_ok();
    paste(
        &s,
        &pane,
        if plain {
            "seq 10001 13000; echo printed-all"
        } else {
            "i=0; while [ $i -lt 3000 ]; do printf '\\033[3%dm%04d\\033[0m the quick \\033[1mbrown\\033[0m fox jumps\\n' $((i%6+1)) $i; i=$((i+1)); done; echo printed-all"
        },
    );
    s.wait_text(&pane, "printed-all");
    phone.wait(
        "the end of the output",
        &format!("(t => t.body.querySelector('.xterm-rows').innerText.includes('printed-all') && t.term.buffer.active.baseY >= 2900 && !(t.hist && t.hist.pending()))({T})"),
    );
    std::thread::sleep(Duration::from_millis(1500));
    phone.protocol("Emulation.setCPUThrottlingRate", json!({"rate": rate}));
    phone.protocol("Performance.enable", json!({}));
    let at = format!("(t => t.flow ? t.body.scrollTop : t.term.buffer.active.viewportY * t.body.querySelector('.xterm-rows > div').getBoundingClientRect().height)({T})");
    phone.js(&format!(
        "(t => {{ const rec = window.rec = {{ frames: [], long: [], drawn: 0, t0: performance.now() }};
            const tick = (now) => {{ rec.frames.push(now); requestAnimationFrame(tick); }};
            requestAnimationFrame(tick);
            new PerformanceObserver(l => {{ for (const e of l.getEntries()) rec.long.push(e.duration); }}).observe({{ type: 'longtask' }});
            t.term.onRender(() => {{ rec.drawn += 1; }});
            return true; }})({T})"
    ));
    let metrics = || {
        let m = phone.protocol("Performance.getMetrics", json!({}));
        let get = |name: &str| {
            m["metrics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["name"] == json!(name))
                .and_then(|x| x["value"].as_f64())
                .unwrap_or(0.0)
        };
        [
            get("TaskDuration"),
            get("ScriptDuration"),
            get("LayoutDuration"),
            get("RecalcStyleDuration"),
        ]
    };
    let (from, before, started) = (
        phone.js(&at).as_f64().unwrap(),
        metrics(),
        std::time::Instant::now(),
    );
    for _ in 0..3 {
        drag(&phone, 0.0, 300.0, 12);
        std::thread::sleep(Duration::from_millis(1200));
    }
    let (wall, after, to) = (
        started.elapsed().as_millis(),
        metrics(),
        phone.js(&at).as_f64().unwrap(),
    );
    let seen = phone.js(
        "(r => { const gaps = r.frames.slice(1).map((t, i) => t - r.frames[i]).sort((a, b) => a - b), q = (p) => Math.round(gaps[Math.min(gaps.length - 1, Math.floor(gaps.length * p))] || 0);
            return { frames: gaps.length, median_ms: q(0.5), p95_ms: q(0.95), worst_ms: q(1), over_34_ms: gaps.filter(g => g > 34).length, over_100_ms: gaps.filter(g => g > 100).length,
              long_tasks: r.long.length, long_tasks_ms: Math.round(r.long.reduce((a, b) => a + b, 0)), terminal_drew: r.drawn }; })(window.rec)",
    );
    let ms = |i: usize| ((after[i] - before[i]) * 1000.0).round();
    println!(
        "MEASURE {}",
        json!({
            "native": phone.js(&format!("!!{T}.flow")), "plain": plain, "throttle": rate, "moved_px": (from - to).round(), "wall_ms": wall,
            "page": seen, "main_thread_ms": {"tasks": ms(0), "script": ms(1), "layout": ms(2), "style": ms(3)},
        })
    );
}

/// Pictures for a person to look at: `SPANDECK_SHOTS` names the folder.
#[test]
#[ignore = "takes pictures for a person to look at; needs Chrome"]
fn pictures_of_a_terminals_history_on_a_phone() {
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let out = std::path::PathBuf::from(std::env::var("SPANDECK_SHOTS").expect("SPANDECK_SHOTS"));
    std::fs::create_dir_all(&out).unwrap();
    let s = Station::start("spark2");
    let pane = shell(&s, "build");
    let shot = |page: &Page, name: &str| {
        std::thread::sleep(Duration::from_millis(700));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("{name}.png")), png).unwrap();
    };
    // What a build prints: colours, bold, and more lines than a station sends.
    let build = "i=1; while [ $i -le 2600 ]; do printf '\\033[1;32m   Compiling\\033[0m crate-%04d v0.%d.0\\n\\033[33mwarning\\033[0m: unused variable \\033[1m`x%d`\\033[0m\\n \\033[38;5;39m-->\\033[0m src/lib.rs:%d:9\\n' $i $((i%9)) $i $((i%400)); i=$((i+1)); done; printf '\\033[1;32m    Finished\\033[0m in 41.2s\\n'";
    let phone = phone_of(&s);
    phone.goto(&format!("#/s/spark2/p/{pane}"));
    phone.wait(
        "the terminal fitted to the phone",
        &format!("(t => !!t && t.control && t.chan !== null && t.term.cols < 60)({T})"),
    );
    paste(&s, &pane, build);
    s.wait_text(&pane, "Finished");
    phone.wait(
        "the end of the build",
        &format!("(t => t.following && !t.hist.pending() && t.body.querySelector('.xterm-rows').innerText.includes('Finished'))({T})"),
    );
    shot(&phone, "1-live-screen");
    drag(&phone, 0.0, 300.0, 12);
    drag(&phone, 0.0, 300.0, 12);
    phone.wait(
        "earlier output",
        &format!("(t => !t.following && !!t._read)({T})"),
    );
    shot(&phone, "2-earlier-output");
    paste(&s, &pane, "echo done; date");
    phone.wait("the button for new output", &format!("!{T}.pill.hidden"));
    shot(&phone, "3-new-output-while-reading");
    // Opened again: the page has the last 2,000 lines, and offers the rest.
    phone.reload();
    phone.wait(
        "the offer of earlier output",
        &format!("(t => !!t && t.chan !== null && t.control && t.hist.lines >= 1990 && !t.hist.pending() && !!t.hist.top.querySelector('button'))({T})"),
    );
    phone.js(&format!("(t => {{ t.body.scrollTop = 0; }})({T})"));
    shot(&phone, "4-top-of-what-the-page-has");
    phone.js(&format!("{T}.hist.top.querySelector('button').click()"));
    phone.wait("the earlier lines", &format!("{T}.hist.lines > 4900"));
    phone.js(&format!("(t => {{ t.body.scrollTop = 0; }})({T})"));
    shot(&phone, "5-earlier-lines-from-the-station");
    phone.js(&format!(
        "(t => {{ t.body.scrollTop = t._metrics().histTop + (t.hist.lines - 2000 - 12) * t.hist.th; }})({T})"
    ));
    shot(&phone, "6-where-fetched-lines-meet-the-pages-own");
    // A full-screen program: its screen alone.
    phone.js(&format!("{T}.toEnd()"));
    paste(
        &s,
        &pane,
        "printf '\\033[?1049h\\033[H\\033[7m full-screen program \\033[0m\\n\\nIt keeps its own history.\\nA drag is passed on to it.\\n'; cat -v",
    );
    phone.wait("the full-screen program", &format!("{T}._alt"));
    shot(&phone, "7-full-screen-program");
    s.call("pane.input", json!({"pane": pane, "data": "\u{3}"}));
    paste(&s, &pane, "printf '\\033[?1049l'");
    phone.wait("the history again", &format!("!{T}._alt"));

    // A pane a computer holds: at the phone's text size, scrolled both ways;
    // and its whole width.
    let desk = Page::open(&s, 1300, 800);
    desk.goto(&format!("#/s/spark2/p/{pane}"));
    desk.wait(
        "the pane on the computer",
        "!!document.querySelector('.pane .xterm-rows')",
    );
    wait_for("the computer to hold the pane", 10, || {
        (s.pane(&pane)["cols"].as_u64().unwrap() > 100).then_some(())
    });
    phone.reload();
    phone.wait(
        "the pane watched",
        &format!("(t => !!t && t.chan !== null && t.body.classList.contains('natural') && t.hist.lines > 300 && !t.hist.pending())({T})"),
    );
    shot(&phone, "8-held-by-a-computer");
    drag(&phone, 0.0, 300.0, 12);
    drag(&phone, -150.0, 0.0, 12);
    shot(&phone, "9-held-by-a-computer-earlier-and-sideways");
    phone.wait(
        "Show whole width",
        "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.phone-pane button')].find(b => b.innerText.trim() === 'Show whole width'))",
    );
    phone.wait(
        "the pane whole",
        &format!("{T}.body.classList.contains('letterbox')"),
    );
    phone.js(&format!("{T}.toEnd()"));
    shot(&phone, "10-whole-width");
    drag(&phone, 0.0, 200.0, 12);
    shot(&phone, "11-whole-width-earlier");
}
