//! A pane's header says more at a glance (DECISIONS.md, 19 Nov 2026).
//!
//! The reason for every test here is the owner's request of that day,
//! looking at a conversation's bar: "looks like a place we can add other
//! information. versions, if updates are needed, what else would be good to
//! know at a glance? AgentVault Lock/Key knowing its connected?", and for a
//! terminal "maybe cpu usage, memory usage?"; and their choice of "options A
//! with the more graphical chips". And rule 6 of AGENTS.md: a branch's name
//! is put on the page as text.
//!
//! Real stations, a real shell, real git, and the stand-in agent program;
//! no real agent. The first test is the station's side and runs with every
//! change. The others need a browser (the merge tier):
//!
//!   cargo test -p spandeck --test chips -- --ignored
//!
//! With `SPANDECK_SHOTS` set, pictures are written there.
#![cfg(unix)]
mod common;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use common::browser::Page;
use common::fake_vault::FakeVault;
use common::*;
use serde_json::{json, Value};

fn spawn(s: &Station, params: Value) -> String {
    s.call("pane.spawn", params)["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A shell that only waits, and one that does nothing but use a processor.
fn shell(s: &Station, name: &str, dir: &Path) -> String {
    spawn(s, json!({"program": "/bin/sh", "name": name, "cwd": dir}))
}

fn spinner(s: &Station, name: &str, dir: &Path) -> String {
    spawn(
        s,
        json!({"program": "/bin/sh", "args": ["-c", "while :; do :; done"], "name": name, "cwd": dir}),
    )
}

/// What the station holds of a pane, once `ready` says it holds enough.
fn glance(s: &Station, pane: &str, what: &str, ready: impl Fn(&Value) -> bool) -> Value {
    wait_for(what, 20, || {
        let row = s.call("pane.glance", json!({}))["panes"][pane].clone();
        ready(&row).then_some(row)
    })
}

/// The station's side: a pane's folder is on a branch with so many files
/// changed (a clean repository, a changed one, a folder that is no
/// repository, a detached HEAD); the programs of a pane that spins use a
/// processor and those of a shell that waits use next to none, and both
/// hold memory; and none of it is an agent's to ask.
#[test]
fn a_pane_says_its_branch_and_what_its_programs_use() {
    let s = Station::start("alpha");
    let clean = s.git_repo("clean");
    let changed = s.git_repo("changed");
    git(&changed, &["checkout", "-q", "-b", "fix/herons"]);
    std::fs::write(changed.join("README.md"), "hello, herons\n").unwrap();
    std::fs::write(changed.join("tally.py"), "print(9)\n").unwrap();
    let detached = s.git_repo("detached");
    git(&detached, &["checkout", "-q", "--detach"]);
    let at = git(&detached, &["rev-parse", "HEAD"]);
    let plain = s.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();

    let idle = shell(&s, "idle", &clean);
    let busy = spinner(&s, "busy", &changed);
    let adrift = shell(&s, "adrift", &detached);
    let nowhere = shell(&s, "nowhere", &plain);

    let git_of =
        |pane: &str| glance(&s, pane, "its branch", |r| !r["git"].is_null())["git"].clone();
    assert_eq!(
        git_of(&idle),
        json!({"branch": "main", "detached": false, "changed": 0})
    );
    assert_eq!(
        git_of(&busy),
        json!({"branch": "fix/herons", "detached": false, "changed": 2})
    );
    assert_eq!(
        git_of(&adrift),
        json!({"branch": at.trim()[..7], "detached": true, "changed": 0})
    );

    // Processor and memory, where this system is read.
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        let spinning = glance(&s, &busy, "the spinning pane's processor", |r| {
            r["cpu_pct"].as_f64().is_some_and(|c| c > 20.0)
        });
        assert!(
            spinning["mem_bytes"].as_u64().unwrap() > 0 && spinning["procs"] == json!(1),
            "{spinning}"
        );
        let waiting = glance(&s, &idle, "the waiting shell's processor", |r| {
            r["cpu_pct"].is_number()
        });
        let (cpu, mem) = (
            waiting["cpu_pct"].as_f64().unwrap(),
            waiting["mem_bytes"].as_u64().unwrap(),
        );
        assert!(cpu < 5.0 && mem > 0 && mem < 200 * 1024 * 1024, "{waiting}");
    }
    // By now every folder has been asked: one that is no repository says
    // nothing, and is not called slow.
    let none = glance(&s, &nowhere, "the pane in no repository", |r| r.is_object());
    assert!(
        none["git"].is_null() && none["git_slow"].is_null(),
        "{none}"
    );

    // An agent's unread mail is counted; a pane with none says nothing.
    let reader = s.fake(json!({"steps": []}), json!({"name": "reader"}));
    s.call(
        "msg.send",
        json!({"to": "reader", "body": "one for the reader"}),
    );
    let mail = glance(&s, &reader, "the reader's mail", |r| r["mail"].is_number());
    assert_eq!(mail["mail"], json!(1));
    assert!(glance(&s, &idle, "the shell", |r| r.is_object())["mail"].is_null());

    // Not an agent's: another project's branch is not its to read, and a
    // pane's own view (which it does read) has none of this.
    let refused = s.call_as_pane(&idle, "pane.glance", json!({})).unwrap_err();
    assert!(
        refused.message.contains("pane.glance"),
        "{}",
        refused.message
    );
    let view = s.pane(&busy);
    assert!(
        view["git"].is_null() && view["cpu_pct"].is_null() && view["mem_bytes"].is_null(),
        "{view}"
    );
    assert!(s.call("station.info", json!({}))["caps"]
        .as_array()
        .unwrap()
        .contains(&json!("pane_glance")));
}

// ------------------------------------------------------------ in a browser

/// A branch an agent could have made: the page must show it as these
/// characters and nothing else.
const BRANCH: &str = "<img/src=x/onerror=window.pwned=1>";

/// A release host on this machine: every path answers "2.1.330".
fn release_host() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-length: 7\r\nconnection: close\r\n\r\n2.1.330"
            );
        }
    });
    port
}

struct Scene {
    s: Station,
    vault: FakeVault,
    lead: String,
    spin: String,
    _tools: tempfile::TempDir,
}

/// One station with a vault it is connected to, a price list, and a
/// stand-in `claude` on its path that says it is 2.1.326 while 2.1.330 is
/// published. In one repository, on the branch `BRANCH` with two files
/// changed: an agent that has answered once (so its model, its context
/// and its cost are known), and a shell that spins.
fn scene() -> Scene {
    let prices = fake_prices::Prices::start();
    let vault = FakeVault::start();
    let tools = tempfile::tempdir().unwrap();
    let claude = tools.path().join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo \"claude 2.1.326\"; exit 0; fi\nsleep 60\n",
    )
    .unwrap();
    spandeck_terminal::sys::set_executable(&claude).unwrap();
    // A test never runs a real agent program, whatever this machine has.
    let missing = tools.path().join("missing");
    let mut config = prices.config();
    config += &format!(
        "[agents.claude]\nlatest_url = \"http://127.0.0.1:{}/claude\"\n",
        release_host()
    );
    for other in ["codex", "opencode", "antigravity"] {
        config += &format!("[agents.{other}]\nprogram = \"{}\"\n", missing.display());
    }
    config += &vault.config();
    let path = format!("{}:/usr/bin:/bin", tools.path().display());
    let s = Station::start_with("alpha", &config, &[("PATH", &path)]);

    wait_for("the vault to be found", 10, || {
        (s.call("vault.refresh", json!({}))["found"] == json!(true)).then_some(())
    });
    s.call("vault.connect", json!({}));
    wait_for("the request to reach the vault", 10, || {
        (vault.decide(true) > 0).then_some(())
    });
    wait_for("the station to collect its token", 10, || {
        (s.call("vault.status", json!({}))["state"] == json!("connected")).then_some(())
    });

    let repo = s.git_repo("mere");
    git(&repo, &["checkout", "-q", "-b", BRANCH]);
    std::fs::write(repo.join("README.md"), "hello, herons\n").unwrap();
    std::fs::write(repo.join("tally.py"), "print(9)\n").unwrap();
    let script = s.script_file(&json!({"steps": [
        {"prompt": "Count the herons"},
        {"transcript": {"type": "assistant", "message": {"id": "msg_1", "role": "assistant", "model": "claude-opus-5-5",
            "content": [{"type": "text", "text": "Nine herons."}],
            "usage": {"input_tokens": 100, "output_tokens": 250, "cache_read_input_tokens": 700000, "cache_creation_input_tokens": 50}}}},
        {"turn_end": "ok"}]}));
    let lead = spawn(
        &s,
        json!({"agent": "claude", "name": "lead", "program": FAKE, "args": ["--script", script], "cwd": repo}),
    );
    s.wait_state(&lead, "done");
    let spin = spinner(&s, "spin", &repo);
    Scene {
        s,
        vault,
        lead,
        spin,
        _tools: tools,
    }
}

impl Scene {
    /// The owner asked whether the programs are the latest (as the Agent
    /// programs dialog does when it opens): the station now holds that
    /// 2.1.330 is published.
    fn asked_for_the_latest(&self) {
        self.s.call("agents.status", json!({}));
        self.s.call("agents.check", json!({}));
        wait_for("the release host's answer", 30, || {
            let rows = self.s.call("agents.status", json!({}));
            rows.as_array()
                .unwrap()
                .iter()
                .any(|r| {
                    r["agent"] == json!("claude") && r["latest"]["state"] == json!("available")
                })
                .then_some(())
        });
    }

    fn vault_locked(&self) {
        self.vault.set_unlocked(false);
        assert_eq!(
            self.s.call("vault.refresh", json!({}))["state"],
            json!("locked")
        );
    }
}

fn shoot(page: &Page, name: &str) {
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let Some(dir) = std::env::var_os("SPANDECK_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
    std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
}

fn width(page: &Page, width: u32, height: u32) {
    page.protocol(
        "Emulation.setDeviceMetricsOverride",
        json!({"width": width, "height": height, "deviceScaleFactor": 1, "mobile": false}),
    );
    page.wait("the window's width", &format!("innerWidth === {width}"));
}

/// The chips in sight in the conversation's bar, as their words.
const CHIPS: &str = "[...document.querySelectorAll('.chat-bar .gchips .gchip')].filter(c => c.getClientRects().length).map(c => c.innerText.replace(/\\s+/g, ' ').trim())";

/// Every piece of the chips' text that is hard to read against what is
/// drawn behind it (less than 4.5:1), and how many pieces were looked at.
const HARD_TO_READ: &str = r#"(() => {
    const ctx = (window.__ctx ||= document.createElement('canvas').getContext('2d', { willReadFrequently: true }));
    const rgba = (css) => { ctx.clearRect(0, 0, 1, 1); ctx.fillStyle = '#000'; ctx.fillStyle = css; ctx.fillRect(0, 0, 1, 1); return [...ctx.getImageData(0, 0, 1, 1).data]; };
    const behind = (e) => { for (; e; e = e.parentElement) { const c = rgba(getComputedStyle(e).backgroundColor); if (c[3] > 250) return c; } return [0, 0, 0, 255]; };
    const lum = (c) => { const [r, g, b] = c.slice(0, 3).map(v => { const s = v / 255; return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4; }); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
    const contrast = (a, b) => { const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x); return (hi + 0.05) / (lo + 0.05); };
    const low = []; let seen = 0;
    for (const e of document.querySelectorAll('.gchip, .gchip b, .gchip .g-more, .strip-git, .strip-git b, .strip-git .g-more, .strip-cost b, .strip-use, .strip-use b, .strip .usage-model, .strip .usage-ctx, .strip .usage-ctx b')) {
        if (!e.getClientRects().length || !e.innerText.trim()) continue;
        seen += 1;
        const c = contrast(rgba(getComputedStyle(e).color), behind(e));
        if (c < 4.5) low.push(`${e.className || e.tagName} "${e.innerText.trim().slice(0, 24)}" ${c.toFixed(2)}`);
    }
    return [seen, low];
})()"#;

fn readable(page: &Page, what: &str, at_least: u64) {
    let seen = page.js(HARD_TO_READ);
    assert!(
        seen[0].as_u64().unwrap() >= at_least && seen[1] == json!([]),
        "{what}: {seen}"
    );
}

/// At a computer. The conversation's bar has a third line of chips: the
/// model, the context with its share, the branch with what is changed and
/// the estimated cost, always; an update for the agent program and a
/// locked vault only when it is so, each opening the dialog that deals
/// with it. A branch named as markup is shown as its characters. A narrow
/// window lets go of the cost and keeps the warnings. The terminal's
/// strip says the same on its one line, with the processor and memory of
/// the pane's own programs; a shell has the branch and those and no model.
/// And all of it can be read in a light theme and in a pane with a look of
/// its own.
#[test]
#[ignore = "merge tier: a browser"]
fn the_bar_and_the_strip_say_what_is_known_at_a_glance() {
    let sc = scene();
    let (s, lead, spin) = (&sc.s, &sc.lead, &sc.spin);
    let page = Page::open(s, 2000, 900);
    page.goto(&format!("#/s/alpha/p/{lead}"));

    // ---- the conversation's bar ----
    let chips = page.wait(
        "the four chips",
        &format!("(c => c.length === 4 && c)({CHIPS})"),
    );
    assert_eq!(
        chips,
        json!([
            "opus-5-5",
            "context 70%",
            format!("{BRANCH} · 2 changed"),
            chips[3]
        ])
    );
    assert!(chips[3].as_str().unwrap().starts_with('$'), "{chips}");
    // A branch's name is text: nothing was made of it.
    assert_eq!(
        page.js("[window.pwned === undefined, document.querySelectorAll('.chat-bar img, .strip img').length, document.querySelector('.gchips .g-git b').textContent]"),
        json!([true, 0, BRANCH])
    );
    // Each chip spells itself out; the cost is called an estimate.
    let says = page.js("['.g-model', '.g-ctx', '.g-git', '.g-cost'].map(c => document.querySelector('.gchips ' + c).getAttribute('aria-label'))");
    assert_eq!(says[0], json!("Model: claude-opus-5-5"));
    assert_eq!(says[1], json!("Context used: 70% of 1,000,000 tokens"));
    assert_eq!(
        says[2],
        json!(format!("Git branch: {BRANCH}, 2 files changed or new"))
    );
    assert!(
        says[3].as_str().unwrap().contains("Estimated cost")
            && says[3].as_str().unwrap().contains("never a bill"),
        "{says}"
    );
    // Under the folder, in the bar's own column, and one line of them.
    let placed = page.js("(r => [r.parentNode.className, r.previousSibling.className.includes('chat-cwd'), r.getBoundingClientRect().height < 30, r.querySelector('.gmeter i').style.width])(document.querySelector('.gchips'))");
    assert_eq!(placed, json!(["chat-title", true, true, "70%"]));
    // Nothing is wrong, so nothing says so: the vault is connected, and
    // nobody has asked whether Claude Code is the latest. The page does not
    // make the station ask.
    assert_eq!(
        page.js("document.querySelectorAll('.gchip.warn, .gchip.bad, .gchip.info').length"),
        json!(0)
    );
    shoot(&page, "chat-chips-dark");

    // ---- only when true ----
    sc.asked_for_the_latest();
    page.wait(
        "the update's chip",
        "(c => c && c.innerText === 'Claude Code 2.1.330 available')(document.querySelector('.gchips .g-update'))",
    );
    page.js("document.querySelector('.gchips .g-update').click()");
    page.wait(
        "Agent programs",
        "!!document.querySelector('.dialog .agent-program[data-agent=claude]')",
    );
    page.js("document.querySelector('.dialog .icon-btn').click()");
    page.wait("the dialog closed", "!document.querySelector('.dialog')");
    sc.vault_locked();
    page.wait(
        "the vault's chip",
        "(c => c && c.innerText.trim() === 'Vault locked' && c.classList.contains('bad') && !!c.querySelector('svg'))(document.querySelector('.gchips .g-vault'))",
    );
    page.js("document.querySelector('.gchips .g-vault').click()");
    page.wait(
        "the Vault dialog",
        "document.querySelector('.dialog')?.getAttribute('aria-label') === 'Vault'",
    );
    page.js("document.querySelector('.dialog .icon-btn').click()");
    page.wait("the dialog closed", "!document.querySelector('.dialog')");
    assert_eq!(
        page.js(&format!("({CHIPS}).length")),
        json!(6),
        "four always, two because they are so"
    );
    readable(&page, "Spandeck Dark, with warnings", 10);
    shoot(&page, "chat-chips-warnings");

    // ---- a narrow pane ----
    // The cost goes first, then how many files are changed; the warnings
    // stay, the update's in fewer words if it must, and it is still one
    // line of chips, each of them whole.
    let whole = "(r => [...r.querySelectorAll('.gchip')].filter(c => c.getClientRects().length).every(c => c.getBoundingClientRect().right <= r.getBoundingClientRect().right + 1))(document.querySelector('.gchips'))";
    let warned = |c: &Value| {
        let c: Vec<&str> = c
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        c.contains(&"Vault locked")
            && (c.contains(&"Claude Code 2.1.330 available") || c.contains(&"Update available"))
    };
    width(&page, 1000, 900);
    let narrow = page.wait(
        "the cost let go of",
        &format!("(c => !c.some(t => t.startsWith('$')) && c)({CHIPS})"),
    );
    assert!(
        warned(&narrow) && narrow[0] == json!("opus-5-5") && narrow[1] == json!("context 70%"),
        "{narrow}"
    );
    assert_eq!(
        page.js(&format!(
            "[{whole}, document.querySelector('.gchips').getBoundingClientRect().height < 30]"
        )),
        json!([true, true])
    );
    shoot(&page, "chat-chips-narrow");
    // Narrower still: the warnings are the last to be there.
    width(&page, 800, 900);
    let narrower = page.wait(
        "the model let go of",
        &format!("(c => !c.includes('opus-5-5') && c)({CHIPS})"),
    );
    assert!(warned(&narrower), "the warnings stay: {narrower}");
    assert_eq!(page.js(whole), json!(true));
    shoot(&page, "chat-chips-narrower");
    width(&page, 2000, 900);
    page.wait(
        "the cost back",
        &format!("({CHIPS}).some(t => t.startsWith('$'))"),
    );

    // ---- the terminal's strip, for the agent ----
    page.terminal_face();
    let strip = page.wait(
        "the strip's pieces",
        "(q => q('.strip-git') && q('.strip-cost') && q('.strip-cpu') && q('.strip-mem') && q('.g-update') && q('.g-vault') && ['.usage-model', '.usage-ctx', '.strip-git', '.strip-cost', '.g-update', '.g-vault'].map(c => q(c).innerText.replace(/\\s+/g, ' ').trim()))(c => document.querySelector('.strip ' + c))",
    );
    assert_eq!(
        strip,
        json!([
            "opus-5-5",
            "context 70%",
            format!("{BRANCH} · 2 changed"),
            strip[3],
            "Claude Code 2.1.330 available",
            "Vault locked"
        ])
    );
    // One line, in order: the folder, the facts, then at the right the
    // processor and memory, before the Chat button.
    let order = page.js("(s => { const at = (c) => [...s.children].findIndex(e => e.matches(c)); return [s.getBoundingClientRect().height, at('.strip-cwd') < at('.usage-model'), at('.strip-cost') < at('.grow'), at('.grow') < at('.strip-cpu'), at('.strip-mem') < at('.face-btn'), s.scrollWidth <= s.clientWidth]; })(document.querySelector('.strip'))");
    assert_eq!(order, json!([28, true, true, true, true, true]));
    assert_eq!(
        page.js("document.querySelector('.strip .usage-ctx').getAttribute('aria-label')"),
        json!("Context used: 70% of 1,000,000 tokens")
    );
    shoot(&page, "strip-agent");
    // A narrow strip lets go of the cost whole (never a number cut in
    // half), and keeps the warnings.
    width(&page, 1150, 900);
    let narrow = page.wait(
        "the strip without the cost",
        "(q => q('.strip-cost')?.hidden && [q('.g-update').getClientRects().length > 0, q('.g-vault').getClientRects().length > 0, [...document.querySelectorAll('.strip [data-drop]')].filter(e => e.getClientRects().length).every(e => e.scrollWidth <= e.clientWidth + 1), document.querySelector('.strip').getBoundingClientRect().height])(c => document.querySelector('.strip ' + c))",
    );
    assert_eq!(narrow, json!([true, true, true, 28]));
    shoot(&page, "strip-agent-narrow");
    width(&page, 2000, 900);

    // ---- a shell: the branch, the processor and the memory ----
    page.goto(&format!("#/s/alpha/p/{spin}"));
    let shell = page.wait(
        "the spinning shell's strip",
        "(q => q('.strip-git') && q('.strip-mem') && parseInt(q('.strip-cpu b')?.innerText) > 20 && [q('.strip-git b').textContent, /^\\d+%$/.test(q('.strip-cpu b').innerText), /^(<1|\\d+(\\.\\d)?) (MB|GB)$/.test(q('.strip-mem b').innerText), !!q('.usage-model'), !!q('.usage-ctx'), !!q('.strip-cost'), !!q('.g-vault'), q('.strip-cpu').innerText.startsWith('CPU'), q('.strip-mem').innerText.startsWith('Mem')])(c => document.querySelector('.strip ' + c))",
    );
    assert_eq!(
        shell,
        json!([BRANCH, true, true, false, false, false, false, true, true])
    );
    shoot(&page, "strip-shell");

    // ---- two panes side by side ----
    s.call(
        "deck.layout.save",
        json!({"layout": {"name": "pair", "tree": {"dir": "row", "ratio": 500,
            "a": {"id": lead, "station": "alpha"}, "b": {"id": spin, "station": "alpha"}}}}),
    );
    page.goto("#/l/pair");
    page.wait(
        "both panes with what they know",
        "document.querySelectorAll('.strip .strip-cpu').length >= 1 && document.querySelectorAll('.strip-git, .gchips .g-git').length === 2",
    );
    // In half the window the warnings are still there and whole.
    assert_eq!(
        page.js("[...document.querySelectorAll('.g-update, .g-vault')].filter(c => c.getClientRects().length).length >= 2 && [...document.querySelectorAll('.pane')].every(p => p.scrollWidth <= p.clientWidth)"),
        json!(true)
    );
    shoot(&page, "layout-of-two");

    // ---- a light theme, and a pane with a look of its own ----
    s.call(
        "look.set",
        json!({"level": "deck", "choice": {"theme": "spandeck-light"}}),
    );
    page.goto(&format!("#/s/alpha/p/{lead}"));
    page.wait(
        "the light theme",
        "document.documentElement.hasAttribute('data-themed') && !!document.querySelector('.strip .strip-git')",
    );
    readable(&page, "Spandeck Light, the strip", 10);
    shoot(&page, "strip-agent-light");
    page.wait(
        "the pane as its conversation",
        "(b => b ? (b.click(), false) : !!document.querySelector('.chat-bar .gchips'))(document.querySelector('.strip .face-btn'))",
    );
    page.wait("the chips", &format!("({CHIPS}).length === 6"));
    readable(&page, "Spandeck Light, the conversation's bar", 10);
    shoot(&page, "chat-chips-light");
    s.call(
        "look.set",
        json!({"level": "pane", "key": format!("alpha/{lead}"), "choice": {"theme": "dracula", "set": {"term.background": "#006d8f"}}}),
    );
    page.wait(
        "the pane's own look",
        "!!document.querySelector('.chat-bar[data-themed]') && getComputedStyle(document.querySelector('.chat-bar')).backgroundColor !== getComputedStyle(document.querySelector('.topbar')).backgroundColor",
    );
    readable(&page, "a pane's own look, the conversation's bar", 10);
    shoot(&page, "chat-chips-own-look");
    page.terminal_face();
    page.wait("the strip", "!!document.querySelector('.strip .strip-git')");
    readable(&page, "a pane's own look, the strip", 10);
}

/// On a phone: one row under the conversation's bar, 44 px to tap, with
/// the model, the context and any warning; a tap opens a sheet that lists
/// everything known as a name and its value.
#[test]
#[ignore = "merge tier: a browser"]
fn on_a_phone_two_chips_and_any_warning_open_everything() {
    let sc = scene();
    sc.asked_for_the_latest();
    let page = Page::open(&sc.s, 390, 844);
    page.phone_screen();
    page.goto(&format!("#/s/alpha/p/{}", sc.lead));
    let seen = "[...document.querySelectorAll('.phone-chat .gchips-phone .gchip')].filter(c => c.getClientRects().length).map(c => c.innerText.replace(/\\s+/g, ' ').trim())";
    let one = page.wait(
        "the row with one warning",
        &format!("(c => c.includes('Update available') && c)({seen})"),
    );
    assert_eq!(one, json!(["opus-5-5", "context 70%", "Update available"]));
    shoot(&page, "phone-row-one-warning");
    // A second warning: both are there and whole, and the row lets go of
    // the model to make room on a screen this narrow.
    sc.vault_locked();
    let row = page.wait(
        "the row with its warnings",
        "(r => r && r.querySelector('.g-update') && r.querySelector('.g-vault') && [[...r.querySelectorAll('.gchip')].filter(c => c.getClientRects().length).map(c => c.innerText.replace(/\\s+/g, ' ').trim()), r.getBoundingClientRect().height >= 44, r.getBoundingClientRect().right <= 390, r.tagName, r.parentNode.className.includes('chat-bar'), [...r.querySelectorAll('.gchip')].every(c => c.getBoundingClientRect().right <= r.getBoundingClientRect().right + 1), document.documentElement.scrollWidth <= 390])(document.querySelector('.phone-chat .gchips-phone'))",
    );
    assert_eq!(
        row,
        json!([
            ["context 70%", "Update available", "Vault locked"],
            true,
            true,
            "BUTTON",
            true,
            true,
            true
        ])
    );
    // The conversation is where it was: at its end, and it scrolls itself.
    assert_eq!(
        page.js("(l => l.scrollHeight - l.scrollTop - l.clientHeight < 48 && l.getBoundingClientRect().top >= document.querySelector('.phone-chat .gchips-phone').getBoundingClientRect().bottom)(document.querySelector('.phone-chat .chat-log'))"),
        json!(true)
    );
    readable(&page, "the phone's row", 4);
    shoot(&page, "phone-row");

    page.js("document.querySelector('.phone-chat .gchips-phone').click()");
    let facts = page.wait(
        "the sheet",
        "(d => d && d.querySelectorAll('.fact').length >= 8 && [...d.querySelectorAll('.fact')].map(f => [f.querySelector('.muted').innerText, f.querySelector('b').innerText]))(document.querySelector('.dialog.sheet .glance-sheet'))",
    );
    let fact = |name: &str| {
        facts
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f[0] == json!(name))
            .unwrap_or_else(|| panic!("no line {name}: {facts}"))[1]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(fact("Model"), "claude-opus-5-5");
    assert_eq!(fact("Context used"), "70%");
    assert_eq!(fact("Branch"), format!("{BRANCH} · 2 changed"));
    assert!(
        fact("This conversation").starts_with('$')
            && fact("This conversation").contains("(estimated) · 700k tokens"),
        "{facts}"
    );
    assert_eq!(fact("Claude Code"), "2.1.326 → 2.1.330 available");
    assert_eq!(fact("Vault"), "locked");
    assert!(fact("Pane").contains("MB"), "{facts}");
    if cfg!(target_os = "linux") {
        assert!(fact("alpha").starts_with("load "), "{facts}");
    }
    // What can be tapped in the sheet is 44 px.
    assert_eq!(
        page.js("[...document.querySelectorAll('.glance-sheet button')].every(b => b.getBoundingClientRect().height >= 44)"),
        json!(true)
    );
    shoot(&page, "phone-sheet");
    page.js("[...document.querySelectorAll('.glance-sheet .g-act')].find(b => b.innerText === 'Vault…').click()");
    page.wait(
        "the Vault dialog",
        "document.querySelector('.dialog')?.getAttribute('aria-label') === 'Vault'",
    );
}
