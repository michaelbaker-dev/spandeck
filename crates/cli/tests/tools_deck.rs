//! The Control Deck's Tools dialog, in a real browser.
//!
//! The reason for every test here is the owner's request of 18 Nov 2026
//! (DECISIONS.md, "Tools for agents": add a tool once, "this machine or the
//! fleet?", roll out later, upgrade across the fleet) and the rule the
//! dialog shares with Agent programs ("Every action says what it did, one
//! job at a time"): a press is greyed until the station's record says the
//! job ended, also after the dialog is closed and opened again.
//!
//! Nothing is downloaded: each station's own config names a stand-in
//! installer (`[tools.playwright]`), as in tools.rs, and the registry is a
//! server on this machine.
//!
//! With `SPANDECK_SHOTS` set, pictures are written there:
//! `SPANDECK_SHOTS=/some/dir cargo test --test tools_deck -- --ignored`
#![cfg(unix)]
mod common;
use common::browser::Page;
use common::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The stand-in installer of one station (the same as tools.rs has). What
/// it does is in files in `<root>/toolctl`: `fail` (it prints this and
/// fails), `sleep` (it takes this long). Every run adds a line to `runs`.
fn stand_in(root: &Path, latest: &str) -> String {
    let ctl = root.join("toolctl");
    std::fs::create_dir_all(&ctl).unwrap();
    let ctl = ctl.display();
    std::fs::write(
        root.join("tool-install.sh"),
        format!(
            r#"d={ctl}
echo run >> $d/runs
echo "fetching $SPANDECK_TOOL_NAME $SPANDECK_TOOL_VERSION"
[ -f $d/sleep ] && sleep "$(cat $d/sleep)"
if [ -f $d/fail ]; then cat $d/fail >&2; exit 1; fi
mkdir -p "$SPANDECK_TOOL_DIR/node_modules/@playwright/mcp"
echo "// $SPANDECK_TOOL_VERSION" > "$SPANDECK_TOOL_DIR/node_modules/@playwright/mcp/cli.js"
echo "added 3 packages"
"#
        ),
    )
    .unwrap();
    std::fs::write(root.join("tool-check.sh"), "echo started\n").unwrap();
    format!(
        "[tools.playwright]\ninstall_cmd = [\"/bin/sh\", \"{0}/tool-install.sh\"]\ncheck_cmd = [\"/bin/sh\", \"{0}/tool-check.sh\"]\nruntime = \"/opt/stand-in/node\"\nlatest_url = \"{latest}\"\n",
        root.display()
    )
}

fn ctl(s: &Station, what: &str, value: Option<&str>) {
    let file = s.root.join("toolctl").join(what);
    match value {
        Some(v) => std::fs::write(file, v).unwrap(),
        None => {
            let _ = std::fs::remove_file(file);
        }
    }
}

fn runs(s: &Station) -> usize {
    std::fs::read_to_string(s.root.join("toolctl/runs"))
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

/// The registry, on this machine: it answers every request with the
/// version the test put there.
struct Registry {
    port: u16,
    says: Arc<Mutex<String>>,
}

impl Registry {
    fn start(version: &str) -> Registry {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let says: Arc<Mutex<String>> = Default::default();
        let s2 = says.clone();
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let b = s2.lock().unwrap().clone();
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}",
                    b.len()
                );
            }
        });
        let r = Registry { port, says };
        r.publish(version);
        r
    }

    fn publish(&self, version: &str) {
        *self.says.lock().unwrap() =
            format!(r#"{{"name":"@playwright/mcp","version":"{version}"}}"#);
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/latest", self.port)
    }
}

fn fleet(names: &[&str], registry: &Registry) -> Vec<Station> {
    let stations = Station::fleet(names, |_, root| (stand_in(root, &registry.url()), vec![]));
    wait_for("the stations see each other", 30, || {
        stations
            .iter()
            .all(|s| {
                names
                    .iter()
                    .filter(|n| **n != s.name)
                    .all(|n| s.sees(n).as_deref() == Some("online"))
            })
            .then_some(())
    });
    stations
}

fn status_row(s: &Station, name: &str) -> Option<Value> {
    s.call("tools.status", json!({}))["tools"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["name"] == json!(name)).cloned())
}

/// The deck of `s` at a phone's size or a desktop's.
fn deck(s: &Station, phone: bool) -> Page {
    if phone {
        let p = Page::open(s, 390, 844);
        p.phone_screen();
        p
    } else {
        Page::open(s, 1200, 800)
    }
}

fn shot(page: &Page, name: &str) {
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    if let Some(out) = std::env::var_os("SPANDECK_SHOTS").map(PathBuf::from) {
        std::fs::create_dir_all(&out).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(350));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("tools-{name}.png")), png).unwrap();
    }
}

/// Open the Tools dialog the way the owner does: the top bar's button on a
/// desktop, the top bar's menu on a phone.
fn open_tools(page: &Page, phone: bool) {
    if phone {
        page.wait(
            "Tools in the top bar's menu",
            "(i => i ? (i.click(), true) : (document.querySelector('.top-more')?.click(), false))([...document.querySelectorAll('.menu-item')].find(i => i.innerText.startsWith('Tools')))",
        );
    } else {
        page.wait(
            "the Tools button",
            "(b => b ? (b.click(), true) : false)(document.querySelector('.topbar .tools-btn'))",
        );
    }
    page.wait(
        "the Tools dialog",
        "!!document.querySelector('.tools-panel .tl-add')",
    );
}

/// An expression about one station's row of a tool's card (`r`).
fn at(tool: &str, station: &str, rest: &str) -> String {
    format!("(r => r && ({rest}))(document.querySelector('.tl-card[data-tool={tool}] .tl-st[data-station={station}]'))")
}

/// Press the question's own button (the last dialog is the question).
fn agree(page: &Page, words: &str) {
    page.wait(
        "the question",
        &format!(
            "(d => d && d.innerText.includes({}))([...document.querySelectorAll('.tl-confirm')].pop())",
            json!(words)
        ),
    );
    page.js("[...document.querySelectorAll('.tl-confirm button.primary')].pop().click()");
    page.wait(
        "the question gone",
        "!document.querySelector('.tl-confirm')",
    );
}

/// 44 px targets and nothing off the side, in every dialog that is open.
const FITS: &str = "[...document.querySelectorAll('.dialog')].every(d => { const b = d.querySelector('.dialog-body'); return b.scrollWidth <= b.clientWidth && d.getBoundingClientRect().right <= innerWidth + 0.5 && [...d.querySelectorAll('*')].every(x => !x.offsetParent || x.getBoundingClientRect().right <= innerWidth + 0.5) && [...d.querySelectorAll('button, summary, label.tl-choice, label.tl-check')].filter(x => x.offsetParent).every(x => x.getBoundingClientRect().height >= 43.5); }) && document.documentElement.scrollWidth <= innerWidth";

/// What does not fit, for a failure's message.
const MISFITS: &str = "JSON.stringify([innerWidth, document.documentElement.scrollWidth, ...[...document.querySelectorAll('.dialog *')].filter(x => x.offsetParent && (x.getBoundingClientRect().right > innerWidth + 0.5 || (x.matches('button, summary, label.tl-choice, label.tl-check') && x.getBoundingClientRect().height < 43.5))).slice(0, 8).map(x => [x.tagName, x.className, x.innerText.slice(0, 40), Math.round(x.getBoundingClientRect().right), Math.round(x.getBoundingClientRect().height)])])";

/// The owner's request, start to end: add a tool from the catalogue for
/// this station, see it install and be ready here and not chosen on the
/// other; roll it out to the fleet later and see the other become ready
/// without the dialog being opened again; upgrade one station first, roll
/// that version out, and go back.
fn the_owners_request(phone: bool) {
    let registry = Registry::start("0.0.85");
    let mut stations = fleet(&["alpha", "bravo"], &registry);
    let b = stations.pop().unwrap();
    let a = stations.pop().unwrap();
    let tag = if phone { "phone" } else { "desktop" };
    let page = deck(&a, phone);
    open_tools(&page, phone);
    page.wait(
        "no tools yet",
        "document.querySelector('.tl-top').innerText.includes('No tools yet')",
    );

    // Add a tool: from the catalogue, then "Where?".
    ctl(&a, "sleep", Some("3"));
    page.js("document.querySelector('.tl-add').click()");
    page.wait(
        "the catalogue's entry, with what it gives and needs",
        "(c => c && c.innerText.includes('Playwright') && c.innerText.includes('A browser the agent can drive') && c.innerText.includes('Needs: Node 18'))(document.querySelector('.tools-add .tl-what'))",
    );
    page.js("document.querySelector('.tools-add input[name=tl-what][value=playwright]').click()");
    assert_eq!(
        page.js("JSON.stringify([...document.querySelectorAll('.tools-add .tl-where > .tl-choice')].map(l => [l.querySelector('span').firstChild.textContent, l.querySelector('input').checked]))"),
        json!(json!([
            ["This station (alpha)", true],
            ["Choose stations", false],
            ["The whole fleet, and stations that join later", false]
        ])
        .to_string()),
        "the question the owner described, this station first"
    );
    assert_eq!(
        page.js("(t => !t.querySelector('input').checked && t.innerText.includes('Agents may use this without asking me') && t.innerText.includes('Claude Code and Codex honour this today'))(document.querySelector('.tools-add .tl-tick'))"),
        json!(true),
        "off unless ticked, and it says which programs honour it"
    );
    assert_eq!(
        page.js("[...document.querySelectorAll('.tools-add .tl-agents input')].map(i => i.checked).join()"),
        json!("true,true,true,true"),
        "every agent, unless the owner says otherwise"
    );
    if phone {
        assert_eq!(page.js(FITS), json!(true), "{}", page.js(MISFITS));
    }
    page.js("document.querySelector('.tools-add .tl-where').scrollIntoView({block: 'center'})");
    shot(&page, &format!("{tag}-2-add-where"));
    page.js("document.querySelector('.tools-add .tl-add-go').click()");

    // It installs here, and the grid says so without being opened again.
    page.wait(
        "alpha installing, with the installer's last line",
        &at("playwright", "alpha", "r.dataset.state === 'installing' && r.querySelector('.ap-job.running')?.innerText.includes('Installing 0.0.85') && r.querySelector('.ap-line')?.innerText.includes('fetching playwright 0.0.85')"),
    );
    assert_eq!(
        page.js("!document.querySelector('.tools-add')"),
        json!(true),
        "the form is gone once the tool is added"
    );
    page.wait(
        "bravo: not chosen",
        &at("playwright", "bravo", "r.dataset.state === 'not_chosen' && r.querySelector('.tl-chip').innerText === 'not chosen here'"),
    );
    shot(&page, &format!("{tag}-3-installing"));
    page.wait(
        "alpha ready, and what the job said",
        &at("playwright", "alpha", "r.querySelector('.tl-chip').innerText === 'ready 0.0.85' && r.querySelector('.tl-result.good')?.innerText.includes('Installed 0.0.85')"),
    );
    ctl(&a, "sleep", None);
    assert_eq!(runs(&b), 0, "nothing was installed where it was not chosen");
    assert_eq!(
        page.js("document.querySelector('.tl-card[data-tool=playwright] .tl-scope').innerText.startsWith('For alpha')"),
        json!(true)
    );

    // Later: roll out to the whole fleet. Bravo installs it by itself.
    ctl(&b, "sleep", Some("2"));
    page.js("document.querySelector('.tl-card[data-tool=playwright] .tl-rollout').click()");
    agree(&page, "Each station installs it for itself: alpha, bravo");
    page.wait(
        "bravo installing",
        &at("playwright", "bravo", "r.dataset.state === 'installing'"),
    );
    assert_eq!(
        page.js("[...document.querySelectorAll('.tl-card[data-tool=playwright] .tl-acts button:not(.tl-remove)')].every(b => b.disabled && b.getAttribute('aria-disabled') === 'true')"),
        json!(true),
        "the tool's own buttons are greyed while a station is installing"
    );
    page.wait(
        "bravo ready",
        &at("playwright", "bravo", "r.querySelector('.tl-chip').innerText === 'ready 0.0.85' && r.querySelector('.tl-result.good')?.innerText.includes('Installed 0.0.85')"),
    );
    ctl(&b, "sleep", None);
    assert_eq!(
        page.js("(c => c.querySelector('.tl-scope').innerText.startsWith('For the whole fleet, and stations that join later') && !c.querySelector('.tl-rollout') && document.querySelector('.tl-top').innerText.includes('Everything is ready where you chose it'))(document.querySelector('.tl-card[data-tool=playwright]'))"),
        json!(true),
        "{}",
        page.js("document.querySelector('.tools-panel').innerText")
    );
    assert_eq!((runs(&a), runs(&b)), (1, 1));

    // A newer version is published: "Check again" finds it.
    registry.publish("0.0.86");
    page.js("document.querySelector('.tl-check-again').click()");
    page.wait(
        "0.0.86 available",
        "document.querySelector('.tl-card[data-tool=playwright] .tl-latest')?.innerText === '0.0.86 available'",
    );
    // Upgrade on one station first; the question says what happens to
    // running panes.
    page.wait(
        "Upgrade on bravo",
        &at("playwright", "bravo", "(u => u && u.innerText === 'Upgrade on bravo' && (u.click(), true))(r.querySelector('.tl-upgrade'))"),
    );
    agree(
        &page,
        "restart and go back to their conversations; an agent in the middle of a turn, or waiting for you, restarts when its turn is over",
    );
    page.wait(
        "bravo on 0.0.86, alpha as it was",
        &format!(
            "{} && {}",
            at("playwright", "bravo", "r.querySelector('.tl-chip').innerText === 'ready 0.0.86' && r.querySelector('.tl-result.good')?.innerText.includes('Upgraded 0.0.85 → 0.0.86') && !!r.querySelector('.tl-own-pin')"),
            at("playwright", "alpha", "r.querySelector('.tl-chip').innerText === 'ready 0.0.85'")
        ),
    );
    assert_eq!(
        page.js("document.querySelector('.tl-card[data-tool=playwright] .tl-pin').innerText"),
        json!("bravo runs 0.0.86"),
        "a station on another version is marked"
    );
    assert_eq!((runs(&a), runs(&b)), (1, 2));
    if phone {
        assert_eq!(page.js(FITS), json!(true), "{}", page.js(MISFITS));
    }
    shot(&page, &format!("{tag}-4-one-station-upgraded"));

    // Then the fleet.
    page.wait(
        "Roll out 0.0.86 to the fleet",
        "(b => b && !b.disabled && b.innerText === 'Roll out 0.0.86 to the fleet' && (b.click(), true))(document.querySelector('.tl-card[data-tool=playwright] .tl-rollout-version'))",
    );
    agree(
        &page,
        "Every station the tool is for installs 0.0.86 for itself",
    );
    page.wait(
        "alpha on 0.0.86 too, and no station on a version of its own",
        &format!(
            "{} && !document.querySelector('.tl-card[data-tool=playwright] .tl-pin') && document.querySelector('.tl-card[data-tool=playwright] .tl-latest').innerText === '✓ latest'",
            at("playwright", "alpha", "r.querySelector('.tl-chip').innerText === 'ready 0.0.86' && r.querySelector('.tl-result.good')?.innerText.includes('Upgraded 0.0.85 → 0.0.86')")
        ),
    );
    assert_eq!((runs(&a), runs(&b)), (2, 2));

    // And back: the earlier version is still on alpha's disk.
    page.wait(
        "Go back to 0.0.85",
        &at("playwright", "alpha", "(u => u && !u.disabled && u.innerText === 'Go back to 0.0.85' && (u.click(), true))(r.querySelector('.tl-back'))"),
    );
    agree(&page, "0.0.85 is still on alpha's disk");
    page.wait(
        "alpha back on 0.0.85",
        &at("playwright", "alpha", "r.querySelector('.tl-chip').innerText === 'ready 0.0.85' && r.querySelector('.tl-result.good')?.innerText.includes('Rolled back 0.0.86 → 0.0.85')"),
    );
    assert_eq!(runs(&a), 2, "going back installed nothing");
    assert_eq!(
        status_row(&b, "playwright").unwrap()["version"],
        json!("0.0.86"),
        "bravo was left as it was"
    );
    shot(&page, &format!("{tag}-5-gone-back"));

    // "Change where": back to this station only. Bravo is no longer
    // chosen, and says so.
    page.js("document.querySelector('.tl-card[data-tool=playwright] .tl-change').click()");
    page.wait(
        "the scope as it is",
        "[...document.querySelectorAll('.tools-add .tl-where input[type=radio]')].find(i => i.checked)?.value === 'fleet'",
    );
    page.js("[...document.querySelectorAll('.tools-add .tl-where input[type=radio]')].find(i => i.value === 'here').click()");
    page.js("document.querySelector('.tools-add button[type=submit]').click()");
    page.wait(
        "bravo: not chosen any more",
        &format!(
            "{} && document.querySelector('.tl-card[data-tool=playwright] .tl-scope').innerText.startsWith('For alpha')",
            at("playwright", "bravo", "r.dataset.state === 'not_chosen'")
        ),
    );
    if std::env::var_os("SPANDECK_SHOTS").is_some() {
        a.call(
            "look.set",
            json!({"level": "deck", "choice": {"theme": "spandeck-light"}}),
        );
        page.wait(
            "the light theme",
            "getComputedStyle(document.documentElement).colorScheme === 'light' || document.documentElement.hasAttribute('data-themed')",
        );
        shot(&page, &format!("{tag}-6-light"));
    }
}

#[test]
#[ignore = "merge tier: a browser at a phone's size, two stations"]
fn a_tool_is_added_here_rolled_out_upgraded_and_taken_back_on_a_phone() {
    the_owners_request(true);
}

/// Not a test of anything the phone's does not cover: the same at a
/// desktop's width, for the pictures.
#[test]
#[ignore = "pictures"]
fn a_tool_is_added_here_rolled_out_upgraded_and_taken_back_on_a_desktop() {
    the_owners_request(false);
}

/// A failed install shows the program's own line, as text, with what it
/// printed behind "Show what it said", and "Try again", which works once
/// the cause is gone. From the press until the station says the job ended
/// the button is greyed, also after the dialog is closed and opened again:
/// the greying is the station's record, not the page's memory. A station
/// that does not answer says so in its own row and holds up no other.
fn a_failure(phone: bool) {
    let registry = Registry::start("0.0.85");
    let mut stations = fleet(&["alpha", "bravo", "charlie"], &registry);
    let mut c = stations.pop().unwrap();
    let b = stations.pop().unwrap();
    let a = stations.pop().unwrap();
    let tag = if phone { "phone" } else { "desktop" };
    // What a program prints is shown as text, markup and all.
    ctl(
        &b,
        "fail",
        Some("npm warn deprecated something\nnpm error 404 Not Found <b id=injected>not markup</b>\n"),
    );
    // An MCP server of Agent setup is a tool "of your own" here.
    a.call(
        "setup.set",
        json!({"fleet": true, "setup": {"mcp_servers": {"docs": {"command": "no-such-program-here", "args": ["-y"]}}}}),
    );
    a.call(
        "tools.add",
        json!({"catalogue": "playwright", "version": "0.0.85", "fleet": true}),
    );
    wait_for("alpha ready, bravo failed", 30, || {
        (status_row(&a, "playwright")?["state"] == json!("ready")
            && status_row(&b, "playwright")?["state"] == json!("failed"))
        .then_some(())
    });
    c.kill9();
    wait_for("alpha no longer hears charlie", 60, || {
        (a.sees("charlie").as_deref() == Some("unreachable")).then_some(())
    });

    let page = deck(&a, phone);
    open_tools(&page, phone);
    page.wait(
        "alpha ready, bravo failed with the program's line, charlie not answering",
        &format!(
            "{} && {} && {}",
            at("playwright", "alpha", "r.querySelector('.tl-chip').innerText === 'ready 0.0.85'"),
            at("playwright", "bravo", "r.dataset.state === 'failed' && r.querySelector('.tl-chip').innerText === 'failed' && r.querySelector('.tl-result.failed')?.innerText.includes('Could not install: npm error 404 Not Found <b id=injected>not markup</b>')"),
            at("playwright", "charlie", "r.querySelector('.tl-chip').innerText === 'not answering' && !r.querySelector('button')")
        ),
    );
    assert_eq!(
        page.js("!document.querySelector('#injected')"),
        json!(true),
        "a program's line is text"
    );
    assert_eq!(
        page.js("(t => t.innerText.includes('1 install not ready: Playwright on bravo'))(document.querySelector('.tl-top'))"),
        json!(true),
        "{}",
        page.js("document.querySelector('.tl-top').innerText")
    );
    // Agent setup's MCP server: listed, for the whole fleet, as before.
    assert_eq!(
        page.js("(c => c.innerText.includes('from Agent setup') && c.querySelector('.tl-scope').innerText.startsWith('For the whole fleet') && c.querySelector('.tl-command').innerText === 'no-such-program-here -y' && c.querySelector('.tl-st[data-station=alpha]').innerText.includes('there is no `no-such-program-here` on alpha'))(document.querySelector('.tl-card[data-tool=docs]'))"),
        json!(true),
        "{}",
        page.js("document.querySelector('.tl-card[data-tool=docs]')?.innerText")
    );
    if !phone {
        assert_eq!(
            page.js("JSON.stringify([...document.querySelectorAll('.tl-matrix tbody tr')].map(r => [...r.children].map(c => c.innerText)))"),
            json!(json!([
                ["Playwright", "ready 0.0.85", "failed", "not answering"],
                ["docs", "needs something", "needs something", "not answering"]
            ])
            .to_string()),
            "the grid at a glance: tool by station"
        );
    }
    shot(&page, &format!("{tag}-1-mixed"));

    // What it printed is behind a tap, and stays open while the dialog
    // goes on asking the stations.
    page.js(&at(
        "playwright",
        "bravo",
        "(r.querySelector('.ap-output summary').click(), true)",
    ));
    page.wait(
        "what it said",
        &at("playwright", "bravo", "r.querySelector('.ap-output').open && r.querySelector('.ap-output pre').innerText.includes('npm warn deprecated something')"),
    );
    page.js(&at(
        "playwright",
        "bravo",
        "(r.scrollIntoView({block: 'center'}), true)",
    ));
    let scrolled =
        page.js("document.querySelector('.tools-panel').closest('.dialog-body').scrollTop");
    shot(&page, &format!("{tag}-2-failed-opened"));
    // Both events mean "ask again": many of them leave what the owner
    // opened, and where they scrolled to, as they are.
    for _ in 0..12 {
        a.call("tools.check", json!({"again": true}));
    }
    std::thread::sleep(std::time::Duration::from_millis(2500));
    assert_eq!(
        page.js(&at(
            "playwright",
            "bravo",
            "r.querySelector('.ap-output').open"
        )),
        json!(true),
        "still open after the stations were asked again"
    );
    assert_eq!(
        page.js("document.querySelector('.tools-panel').closest('.dialog-body').scrollTop"),
        scrolled,
        "and the dialog is where the owner scrolled it"
    );
    if phone {
        assert_eq!(page.js(FITS), json!(true), "{}", page.js(MISFITS));
    }

    // The cause is removed. Try again: greyed from the press.
    ctl(&b, "fail", None);
    ctl(&b, "sleep", Some("6"));
    let greyed = at(
        "playwright",
        "bravo",
        "(b => b.length > 0 && b.every(x => x.disabled && x.getAttribute('aria-disabled') === 'true'))([...r.querySelectorAll('.tl-st-acts button')])",
    );
    assert_eq!(
        page.js(&at(
            "playwright",
            "bravo",
            "(t => t.innerText === 'Try again' && !t.disabled && (t.click(), t.click(), true))(r.querySelector('.tl-retry'))"
        )),
        json!(true)
    );
    assert_eq!(page.js(&greyed), json!(true), "greyed at once");
    page.wait(
        "bravo says it is installing",
        &at("playwright", "bravo", "r.dataset.state === 'installing' && r.querySelector('.ap-job.running')?.innerText.includes('Installing 0.0.85')"),
    );
    assert_eq!(page.js(&greyed), json!(true), "greyed while the job runs");
    // Alpha's row is not held up by bravo's job.
    assert_eq!(
        page.js(&at("playwright", "alpha", "!r.dataset.busy")),
        json!(true)
    );
    // Closed and opened again in the middle: the station says the job is
    // running, so the row does, and its button is greyed.
    page.js("document.querySelector('.tools-panel .tl-close').click()");
    page.wait("the dialog gone", "!document.querySelector('.tools-panel')");
    open_tools(&page, phone);
    page.wait(
        "bravo's row, from the station",
        &at("playwright", "bravo", "r.dataset.state === 'installing'"),
    );
    assert_eq!(page.js(&greyed), json!(true), "after reopening, mid-job");
    shot(&page, &format!("{tag}-3-retrying"));
    page.wait(
        "bravo ready",
        &at("playwright", "bravo", "r.querySelector('.tl-chip').innerText === 'ready 0.0.85' && r.querySelector('.tl-result.good')?.innerText.includes('Installed 0.0.85') && !r.querySelector('button[disabled]')"),
    );
    assert_eq!(runs(&b), 2, "two presses started one install");

    // Remove asks once, and says what it does and does not do.
    page.js("document.querySelector('.tl-card[data-tool=playwright] .tl-remove').click()");
    page.wait(
        "what removing does",
        "(d => d && d.innerText.includes('Agents lose the tool at their next start') && d.innerText.includes('files stay on each station'))(document.querySelector('.tl-confirm'))",
    );
    agree(&page, "Remove");
    page.wait(
        "the tool gone from the grid",
        "!document.querySelector('.tl-card[data-tool=playwright]') && !!document.querySelector('.tl-card[data-tool=docs]')",
    );
    assert!(status_row(&a, "playwright").is_none());
}

#[test]
#[ignore = "merge tier: a browser, three stations"]
fn a_failed_install_says_why_and_try_again_is_greyed_until_the_station_says_it_ended() {
    a_failure(false);
}

/// The same at a phone's size, for the pictures.
#[test]
#[ignore = "pictures"]
fn a_failed_install_on_a_phone() {
    a_failure(true);
}

/// The owner's phone: at 320 px nothing is wider than the page, in the
/// grid with a failed cell opened and in the form a tool is added with,
/// and each of them can be closed. (The owner has reported dialogs with no
/// way out.)
#[test]
#[ignore = "merge tier: a browser at the narrowest phone's size, two stations"]
fn at_320_px_nothing_is_wider_than_the_page_and_the_dialog_can_be_closed() {
    let registry = Registry::start("0.0.86");
    let mut stations = fleet(&["m4-macbook-of-the-owner", "bravo"], &registry);
    let b = stations.pop().unwrap();
    let a = stations.pop().unwrap();
    ctl(
        &b,
        "fail",
        Some("npm error 404 Not Found - GET https://registry.npmjs.org/@playwright%2fmcp/-/a-very-long-address-without-a-single-space-in-it-anywhere-at-all.tgz\n"),
    );
    a.call(
        "tools.add",
        json!({"catalogue": "playwright", "version": "0.0.85", "fleet": true}),
    );
    a.call(
        "tools.add",
        json!({"name": "notes", "command": "/opt/a/long/folder/of/programs/that/goes/on/notes-server", "args": ["--with", "an-argument-that-is-long-too"], "stations": ["bravo"]}),
    );
    wait_for("alpha ready, bravo failed", 30, || {
        (status_row(&a, "playwright")?["state"] == json!("ready")
            && status_row(&b, "playwright")?["state"] == json!("failed"))
        .then_some(())
    });
    let page = Page::open(&a, 390, 700);
    page.protocol(
        "Emulation.setDeviceMetricsOverride",
        json!({"width": 320, "height": 640, "deviceScaleFactor": 1, "mobile": false}),
    );
    page.wait("a small phone's screen", "innerWidth === 320");
    open_tools(&page, true);
    page.wait(
        "the failed cell and a newer version",
        &format!(
            "{} && document.querySelector('.tl-card[data-tool=playwright] .tl-latest')?.innerText === '0.0.86 available'",
            at("playwright", "bravo", "r.dataset.state === 'failed' && !!r.querySelector('.ap-output')")
        ),
    );
    page.js(&at(
        "playwright",
        "bravo",
        "(r.querySelector('.ap-output summary').click(), true)",
    ));
    assert_eq!(page.js(FITS), json!(true), "{}", page.js(MISFITS));
    // A card for each tool and a row for each station: no table.
    assert_eq!(
        page.js("getComputedStyle(document.querySelector('.tl-matrix')).display === 'none' && document.querySelectorAll('.tl-card[data-tool=playwright] .tl-st').length === 2"),
        json!(true)
    );
    shot(&page, "phone320-1-grid");
    page.js(&at(
        "playwright",
        "bravo",
        "(r.scrollIntoView({block: 'start'}), true)",
    ));
    shot(&page, "phone320-2-failed-opened");

    // The form, with its stations listed; and the way out of it.
    page.js("document.querySelector('.tl-add').click()");
    page.wait(
        "the form",
        "!!document.querySelector('.tools-add input[name=tl-what][value=own]')",
    );
    page.js("document.querySelector('.tools-add input[name=tl-what][value=own]').click()");
    page.js("[...document.querySelectorAll('.tools-add .tl-where input[type=radio]')].find(i => i.value === 'some').click()");
    assert_eq!(page.js(FITS), json!(true), "{}", page.js(MISFITS));
    shot(&page, "phone320-3-add");
    // "A tool of your own": a command, as Agent setup's MCP servers take.
    page.js("(set => { set(document.querySelector('.tools-add .tl-own input[aria-label^=Name]'), 'files'); set(document.querySelector('.tools-add .tl-own input[aria-label^=Command]'), 'no-such-program-here --root /srv'); return true; })((i, v) => { i.value = v; })");
    page.js("document.querySelector('.tools-add .tl-add-go').click()");
    page.wait(
        "the owner's own tool, for the station chosen",
        "(c => c && c.innerText.includes('of your own') && c.querySelector('.tl-command').innerText === 'no-such-program-here --root /srv' && c.querySelector('.tl-scope').innerText.startsWith('For m4-macbook-of-the-owner') && c.querySelector('.tl-st[data-station=bravo] .tl-chip').innerText === 'not chosen here')(document.querySelector('.tl-card[data-tool=files]'))",
    );
    assert_eq!(page.js(FITS), json!(true), "{}", page.js(MISFITS));
    page.js("document.querySelector('.tl-add').click()");
    page.wait("the form again", "!!document.querySelector('.tools-add')");
    let way_out = "(x => { const r = x.getBoundingClientRect(); return r.width >= 44 && r.height >= 44 && r.left >= 0 && r.right <= innerWidth && r.top >= 0 && document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2) === x; })([...document.querySelectorAll('.dialog-head .icon-btn')].pop())";
    assert_eq!(
        page.js(way_out),
        json!(true),
        "the form's Close is in reach"
    );
    page.js("[...document.querySelectorAll('.dialog-head .icon-btn')].pop().click()");
    page.wait("the form gone", "!document.querySelector('.tools-add')");
    assert_eq!(
        page.js("!!document.querySelector('.tools-panel')"),
        json!(true),
        "closing the form leaves the grid"
    );
    assert_eq!(
        page.js(way_out),
        json!(true),
        "the dialog's Close is in reach"
    );
    page.js("[...document.querySelectorAll('.dialog-head .icon-btn')].pop().click()");
    page.wait(
        "the dialog gone",
        "!document.querySelector('.dialog') && !document.querySelector('.backdrop')",
    );
}
