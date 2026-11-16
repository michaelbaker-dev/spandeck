//! Phase 2 acceptance: the web client.
//! Done when: browser smoke tests open a pane, type, split, resize and drop a
//! file; a request with a wrong origin or no token is refused.
mod common;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

async fn rpc(
    s: &Station,
    token: Option<&str>,
    origin: Option<&str>,
    host: Option<&str>,
    method: &str,
) -> (u16, Value) {
    let mut req = http()
        .post(s.url("/api/rpc"))
        .json(&json!({"method": method, "params": {}}));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    if let Some(o) = origin {
        req = req.header("origin", o);
    }
    if let Some(h) = host {
        req = req.header("host", h);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_without_a_token_or_from_another_origin_are_refused() {
    let s = tokio::task::spawn_blocking(|| Station::start("alpha"))
        .await
        .unwrap();
    let token = s.token();
    let good_origin = format!("http://127.0.0.1:{}", s.port);

    let (status, body) = rpc(&s, Some(&token), Some(&good_origin), None, "station.info").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["station"], json!("alpha"));

    // No token, wrong token.
    assert_eq!(rpc(&s, None, None, None, "station.info").await.0, 401);
    assert_eq!(
        rpc(&s, Some("0000"), None, None, "station.info").await.0,
        401
    );
    // A page on another site calling this station (CSRF, DNS rebinding).
    assert_eq!(
        rpc(
            &s,
            Some(&token),
            Some("http://evil.example"),
            None,
            "station.info"
        )
        .await
        .0,
        403
    );
    assert_eq!(
        rpc(&s, Some(&token), None, Some("evil.example"), "station.info")
            .await
            .0,
        403
    );

    // The station-to-station endpoint refuses a caller that cannot sign with
    // the fleet key, and any browser.
    let peer = http()
        .post(s.url("/api/peer/rpc"))
        .json(&json!({"method": "station.info", "from": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(peer.status().as_u16(), 401);
    let forged = http()
        .post(s.url("/api/peer/rpc"))
        .header("x-spandeck-station", "x")
        .header("x-spandeck-sig", "1.aaaa.bbbb")
        .json(&json!({"method": "rpc.forward", "params": {"method": "pane.list"}, "from": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status().as_u16(), 401);
    let browser = http()
        .post(s.url("/api/peer/rpc"))
        .header("origin", &good_origin)
        .json(&json!({"method": "station.hello", "from": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(browser.status().as_u16(), 403);

    // An agent's own token works over HTTP too, with an agent's limits.
    let pane = tokio::task::block_in_place(|| s.fake(json!({"steps": []}), json!({})));
    let pane_token = s.pane_token(&pane);
    assert_eq!(
        rpc(&s, Some(&pane_token), None, None, "pane.list").await.0,
        200
    );
    assert_eq!(
        rpc(&s, Some(&pane_token), None, None, "stop.all").await.0,
        403
    );
    drop(tokio::task::spawn_blocking(move || drop(s)).await);
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
async fn a_websocket_must_authenticate_in_its_first_message() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    let s = tokio::task::spawn_blocking(|| Station::start("alpha"))
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}/api/ws", s.port);

    // Wrong token: told so, then nothing works.
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(Message::Text(
        json!({"auth": {"token": "nope"}}).to_string().into(),
    ))
    .await
    .unwrap();
    let hello: Value =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(hello["auth"]["ok"], json!(false));

    // A cross-origin page cannot even open the socket.
    let mut req = url.clone().into_client_request().unwrap();
    req.headers_mut()
        .insert("origin", "http://evil.example".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(req).await.is_err());

    // Right token: RPC and a pane stream on the one socket.
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(Message::Text(
        json!({"auth": {"token": s.token(), "client": "test-1"}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let hello: Value =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(hello["auth"]["ok"], json!(true));
    ws.send(Message::Text(json!({"id": 1, "method": "pane.spawn", "params": {"program": "/bin/sh", "cwd": s.root.join("home")}}).to_string().into())).await.unwrap();
    let spawned: Value =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    let pane = spawned["result"]["id"].as_str().unwrap().to_string();
    ws.send(Message::Text(
        json!({"id": 2, "method": "pane.attach", "params": {"pane": pane}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let attached = loop {
        let m = ws.next().await.unwrap().unwrap();
        if let Ok(t) = m.to_text() {
            let v: Value = serde_json::from_str(t).unwrap_or(Value::Null);
            if v["id"] == json!(2) {
                break v;
            }
        }
    };
    let chan = attached["result"]["chan"].as_u64().unwrap() as u32;
    ws.send(Message::Text(json!({"id": null, "method": "pane.input", "params": {"chan": chan, "data": "echo ws-$((6*7))\r"}}).to_string().into())).await.unwrap();
    let mut seen = Vec::new();
    let got = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(Ok(Message::Binary(b))) = ws.next().await {
                assert_eq!(b[0], 1, "an output frame");
                assert_eq!(u32::from_le_bytes([b[1], b[2], b[3], b[4]]), chan);
                seen.extend_from_slice(&b[5..]);
                if String::from_utf8_lossy(&seen).contains("ws-42") {
                    return true;
                }
            }
        }
    })
    .await;
    assert_eq!(got, Ok(true));

    // The audit's gap 13, measured on 0.4.2: the page comes back after a
    // drop still holding this pane's channel number, and what it sends with
    // it must not reach the pane the new connection gave that number to
    // (every connection used to count from 1, so it did).
    drop(ws);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let say = |v: Value| Message::Text(v.to_string().into());
    ws.send(say(
        json!({"auth": {"token": s.token(), "client": "test-1"}}),
    ))
    .await
    .unwrap();
    ws.send(say(json!({"id": 3, "method": "pane.spawn", "params": {"program": "/bin/sh", "cwd": s.root.join("home")}}))).await.unwrap();
    async fn answer(
        ws: &mut (impl futures_util::Stream<
            Item = Result<Message, tokio_tungstenite::tungstenite::Error>,
        > + Unpin),
        id: u64,
    ) -> Value {
        loop {
            let m = ws.next().await.unwrap().unwrap();
            let v: Value = serde_json::from_str(m.to_text().unwrap_or("")).unwrap_or(Value::Null);
            if v["id"] == json!(id) {
                return v;
            }
        }
    }
    let other = answer(&mut ws, 3).await["result"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    ws.send(say(
        json!({"id": 4, "method": "pane.attach", "params": {"pane": other}}),
    ))
    .await
    .unwrap();
    let now = answer(&mut ws, 4).await["result"]["chan"].as_u64().unwrap();
    assert_ne!(
        now, chan as u64,
        "a connection counts from a place of its own"
    );
    // Asked for an answer, it is refused; sent as a key is, it goes nowhere.
    ws.send(say(json!({"id": 5, "method": "pane.input", "params": {"chan": chan, "data": "echo stray-$((1+1))\r"}}))).await.unwrap();
    let refused = answer(&mut ws, 5).await;
    assert_eq!(refused["error"]["message"], json!("no such channel"));
    for (c, line) in [(chan as u64, "stray-$((1+1))"), (now, "here-$((2+3))")] {
        ws.send(say(json!({"id": null, "method": "pane.input", "params": {"chan": c, "data": format!("echo {line}\r")}}))).await.unwrap();
    }
    let s = tokio::task::spawn_blocking(move || {
        // What was sent after the stray keys has arrived, and they have not.
        s.wait_text(&other, "here-5");
        for p in [&pane, &other] {
            assert!(!s.text(p).contains("stray"), "{}", s.text(p));
        }
        s
    })
    .await
    .unwrap();
    drop(tokio::task::spawn_blocking(move || drop(s)).await);
}

use common::browser::Page;

#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn browser_opens_a_pane_types_copies_pastes_splits_resizes_and_drops_a_file() {
    let s = Station::start("alpha");
    let a = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "one", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "two", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let page = Page::open(&s, 1400, 900);

    // The token is taken out of the address bar once it has been stored.
    page.wait(
        "the fleet view",
        "document.querySelector('.tile.station') && !location.hash.includes('token')",
    );
    assert!(page
        .js("document.querySelector('.tile.station').innerText")
        .as_str()
        .unwrap()
        .contains("alpha"));

    // Zoom: fleet -> station (the deck) -> pane. Every level has a URL.
    page.js("document.querySelector('.tile.station').click()");
    page.wait(
        "the deck",
        "location.hash === '#/s/alpha' && document.querySelectorAll('.pane-tile').length === 2",
    );
    page.js("[...document.querySelectorAll('.pane-tile')].find(t => t.innerText.includes('one')).click()");
    page.wait(
        "the pane view",
        &format!(
            "location.hash === '#/s/alpha/p/{a}' && !!document.querySelector('.pane .xterm-rows')"
        ),
    );

    // Type into the terminal through the browser.
    page.wait("terminal focus", "document.querySelector('.pane .xterm-helper-textarea') && (document.querySelector('.pane .xterm-helper-textarea').focus(), true)");
    page.type_str("echo typed-$((20+3))");
    page.press_key("Enter");
    wait_for("typed text on screen", 15, || {
        page.term_text().contains("typed-23").then_some(())
    });
    s.wait_text(&a, "typed-23");

    // The browser fitted the pane to its window and told the station.
    let size = wait_for("resize to reach the station", 10, || {
        let p = s.pane(&a);
        (p["cols"].as_u64().unwrap() > 120).then_some(p)
    });
    assert_eq!(
        size["controller"].as_str().map(|c| c.starts_with("web-")),
        Some(true)
    );

    // Layouts: the first pane goes into a new global layout from its menu in
    // the list, then the second beside it. The tab bar is gone.
    page.goto("/s/alpha");
    page.wait(
        "the deck again",
        "document.querySelectorAll('.pane-tile').length === 2 && !document.querySelector('.tabs, .tab')",
    );
    let row = |name: &str| {
        format!("[...document.querySelectorAll('.side-pane')].find(x => x.querySelector('.side-name').innerText === '{name}')")
    };
    page.right_click(&row("one"));
    page.choose("Add to a layout");
    page.wait("the layout chooser", "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.dialog .list-row')].find(r => r.innerText.includes('New global layout')))");
    page.wait("its name", "(f => f && f.querySelector('input').value === 'global' ? (f.querySelector('button[type=submit]').click(), true) : false)(document.querySelector('.dialog form'))");
    page.wait(
        "one pane in the layout",
        "location.hash === '#/l/global' && document.querySelectorAll('.workspace .pane').length === 1",
    );
    page.right_click(&row("two"));
    page.choose("Add to global, to the right");
    page.wait(
        "two panes side by side",
        "document.querySelectorAll('.workspace .split.row .pane').length === 2",
    );

    // Copy and paste (the owner's report, 14 Nov 2026: with text highlighted
    // nothing was copied, and the pane's menu had no Copy). The menu copies
    // what is selected, and pastes as one paste that runs nothing by itself.
    // Here the page is given no clipboard, as over plain http on the tailnet.
    page.js("(() => { Object.defineProperty(navigator, 'clipboard', {value: undefined, configurable: true}); \
              document.addEventListener('copy', () => { window.__copied = document.activeElement.value; }); return true; })()");
    let first = "document.querySelector('.workspace .pane')";
    page.right_click(first);
    assert_eq!(
        page.js("[...document.querySelectorAll('.menu-item')].filter(i => /^(Copy|Paste)/.test(i.innerText)).map(i => [i.innerText.split('\\n')[0], i.disabled]).slice(0, 3)"),
        json!([["Copy", true], ["Paste", false], ["Copy everything on screen", false]]),
        "with nothing selected there is nothing to copy"
    );
    page.js("document.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true}))");
    assert_eq!(
        page.js(&format!("(() => {{ const t = {first}.paneTerm.term, b = t.buffer.active; \
            for (let y = 0; y < b.length; y++) if (b.getLine(y).translateToString(true) === 'typed-23') {{ t.select(0, y, 8); return t.getSelection(); }} return null; }})()")),
        json!("typed-23")
    );
    page.right_click(first);
    page.choose("Copy");
    page.wait(
        "the selection to be copied",
        "window.__copied === 'typed-23'",
    );
    page.right_click(first);
    page.choose("Paste");
    page.wait("the box to paste into", "(b => b ? (b.value = 'echo pasted-$((30+3))', b.closest('form').querySelector('button[type=submit]').click(), true) : false)(document.querySelector('.dialog textarea'))");
    s.wait_text(&a, "echo pasted-$((30+3))");
    assert!(!s.text(&a).contains("pasted-33"), "a paste runs nothing");
    page.wait(
        "terminal focus",
        &format!(
            "(t => t ? (t.focus(), true) : false)({first}.querySelector('.xterm-helper-textarea'))"
        ),
    );
    page.press_key("Enter");
    s.wait_text(&a, "pasted-33");

    // Drag the gutter: the split is resized and the panes are re-fitted.
    let before = s.pane(&b)["cols"].as_u64().unwrap();
    let wide = page.js("(() => { const g = document.querySelector('.gutter').getBoundingClientRect(); return [g.left + 2, g.top + 80]; })()");
    let (gx, gy) = (wide[0].as_f64().unwrap(), wide[1].as_f64().unwrap());
    page.js(&format!(
        "(() => {{ const g = document.querySelector('.gutter'); g.dispatchEvent(new MouseEvent('mousedown', {{bubbles: true, clientX: {gx}, clientY: {gy}}})); \
         window.dispatchEvent(new MouseEvent('mousemove', {{bubbles: true, clientX: {}, clientY: {gy}}})); \
         window.dispatchEvent(new MouseEvent('mouseup', {{bubbles: true}})); return true; }})()",
        gx - 260.0
    ));
    wait_for("the right pane to grow", 10, || {
        (s.pane(&b)["cols"].as_u64().unwrap() > before + 10).then_some(())
    });

    // Drop a file on a pane: the station writes it and types its path.
    page.js(
        "(() => { const dt = new DataTransfer(); dt.items.add(new File(['spandeck drop test'], 'shot one.png', {type: 'image/png'})); \
         const pane = document.querySelectorAll('.workspace .pane')[0]; \
         pane.dispatchEvent(new DragEvent('drop', {bubbles: true, cancelable: true, dataTransfer: dt})); return true; })()",
    );
    // The path may wrap on a narrow pane, so compare without line breaks.
    let dir = s.root.join("state/drops").join(&a);
    let file = wait_for("the dropped file on the station", 15, || {
        std::fs::read_dir(&dir)
            .ok()?
            .flatten()
            .next()
            .map(|e| e.path())
    });
    assert!(
        file.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("-shot_one.png"),
        "{file:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "spandeck drop test"
    );
    wait_for("its path typed into the pane", 15, || {
        s.text(&a)
            .replace('\n', "")
            .contains(file.to_str().unwrap())
            .then_some(())
    });
}

#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn a_second_browser_letterboxes_instead_of_resizing_the_pane() {
    let s = Station::start("alpha");
    let a = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "one", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let big = Page::open(&s, 1500, 900);
    big.goto(&format!("/s/alpha/p/{a}"));
    big.wait("the pane", "!!document.querySelector('.pane .xterm-rows')");
    let cols = wait_for("the first browser's size", 10, || {
        let c = s.pane(&a)["cols"].as_u64().unwrap();
        (c > 120).then_some(c)
    });

    // A smaller window takes control; the first one keeps the pane's real size
    // and offers to take control back.
    let small = Page::open(&s, 800, 500);
    small.goto(&format!("/s/alpha/p/{a}"));
    small.wait("the pane", "!!document.querySelector('.pane .xterm-rows')");
    let small_cols = wait_for("the smaller size", 10, || {
        let c = s.pane(&a)["cols"].as_u64().unwrap();
        (c < cols).then_some(c)
    });
    big.wait("the first browser to letterbox", "!!document.querySelector('.term-body.letterbox') && [...document.querySelectorAll('.strip button')].some(b => b.innerText === 'Take control')");
    assert_eq!(
        s.pane(&a)["cols"].as_u64().unwrap(),
        small_cols,
        "the letterboxed client did not fight over the size"
    );
}

/// Not a test of behaviour: writes screenshots of each view for a human to
/// look at. `SPANDECK_SHOTS=/some/dir cargo test --test phase2 -- --ignored screenshots`
#[test]
#[ignore]
fn screenshots_of_every_view() {
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let dir =
        std::path::PathBuf::from(std::env::var("SPANDECK_SHOTS").expect("set SPANDECK_SHOTS"));
    std::fs::create_dir_all(&dir).unwrap();
    let prices = fake_prices::Prices::start();
    let s = Station::start_with("spark2", &prices.config(), &[]);
    // What each agent's session file says it used.
    let used = |id: &str, model: &str, side: bool, n: [u64; 4]| {
        json!({"transcript": {"type": "assistant", "isSidechain": side, "message": {"id": id, "model": model,
            "usage": {"input_tokens": n[0], "output_tokens": n[1], "cache_read_input_tokens": n[2], "cache_creation_input_tokens": n[3]}}}})
    };
    let repo = s.git_repo("api");
    std::fs::create_dir_all(repo.join(".spandeck")).unwrap();
    std::fs::write(
        repo.join(".spandeck/project.toml"),
        "name = \"api\"\n\n[[pane]]\nname = \"term\"\nagent = \"shell\"\n",
    )
    .unwrap();
    s.call("project.new", json!({"path": repo, "open": true}));
    let spawn = |name: &str, role: &str, script: Value| {
        s.fake(
            script,
            json!({"name": name, "project": "api", "role": role, "cwd": repo}),
        )
    };
    let dev = spawn(
        "dev",
        "author",
        json!({"steps": [{"say": "● I'll add the importer and its tests."}, {"prompt": "add the importer"}, used("m1", "claude-opus-5-5", false, [2100, 18400, 6_120_000, 410_000]), used("m2", "claude-haiku-5-5", true, [900, 3200, 880_000, 120_000]), used("m3", "claude-opus-5-5", false, [40, 900, 618_000, 3100]), {"tool": {"name": "Edit", "file": "src/importer.rs"}}, {"say": "● Running the test suite…"}, {"work": 600000}]}),
    );
    spawn(
        "reviewer",
        "reviewer",
        json!({"steps": [{"prompt": "clean the build"}, used("m1", "claude-fable-5-2", false, [310, 4200, 164_000, 22_000]), {"say": "I need to remove the build directory first."}, {"permission": {"name": "Bash", "command": "rm -rf build && cargo build --release"}}, {"sleep": 600000}]}),
    );
    spawn(
        "tester",
        "tester",
        json!({"steps": [{"prompt": "run tests"}, used("m1", "claude-experimental-9", false, [120, 800, 31_000, 9000]), {"tool": {"name": "Bash", "command": "cargo test"}}, {"turn_end": "All 42 tests pass."}]}),
    );
    s.call(
        "pane.send_text",
        json!({"pane": "term", "text": "git log --oneline && ls -la"}),
    );
    s.wait_state(&dev, "working");
    s.wait_state("reviewer", "needs_you");

    let shoot = |page: &Page, name: &str| {
        std::thread::sleep(Duration::from_millis(1200));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
    };
    let page = Page::open(&s, 1400, 860);
    page.wait(
        "fleet",
        "!!document.querySelector('.tile.station .agent-line')",
    );
    shoot(&page, "1-fleet");
    page.goto("/s/spark2");
    page.wait(
        "deck",
        "document.querySelectorAll('.pane-tile').length === 4",
    );
    shoot(&page, "2-station-deck");
    let names: Vec<String> = s
        .call("pane.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["project"] == json!("api"))
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    if names.len() >= 3 {
        s.call("project.layout.save", json!({"project": "api", "layout": {"name": "build", "tree": {"dir": "col", "ratio": 600,
            "a": {"dir": "row", "ratio": 500, "a": {"pane": names[0]}, "b": {"pane": names[1]}}, "b": {"pane": names[2]}}}}));
        let head = |label: &str| {
            format!("[...document.querySelectorAll('.lay-head button')].find(b => b.innerText === '{label}').click()")
        };
        let layout = |name: &str| {
            format!("(t => t ? (t.click(), true) : false)([...document.querySelectorAll('.side-layout')].find(t => t.querySelector('.side-name').innerText === '{name}'))")
        };
        page.wait("layout row", &layout("build"));
        page.wait(
            "layout",
            "document.querySelectorAll('.workspace .pane').length === 3",
        );
        shoot(&page, "2b-project-layout");
        page.js(&head("Arrange"));
        page.wait(
            "arrange",
            "document.querySelectorAll('.arr[data-ref]').length === 3",
        );
        page.js("document.querySelector('.arr[data-ref]').dataset.zone = 'right'");
        shoot(&page, "2c-arrange");
        page.js(&head("Done arranging"));

        // A guest pane from another project, and how Arrange lists the panes.
        let web = s.git_repo("web");
        s.call(
            "project.new",
            json!({"path": web, "panes": [{"name": "logs", "agent": "shell"}], "open": true}),
        );
        s.call("pane.spawn", json!({"agent": "shell", "name": "scratch"}));
        s.call("project.layout.save", json!({"project": "api", "layout": {"name": "with-logs", "tree": {"dir": "row", "ratio": 600,
            "a": {"pane": names[0]}, "b": {"pane": "logs", "project": "web"}}}}));
        page.wait("guest layout row", &layout("with-logs"));
        page.wait(
            "guest",
            "!!document.querySelector('.workspace .pane .chip.guest')",
        );
        shoot(&page, "2d-guest-in-layout");
        page.js(&head("Arrange"));
        page.wait("arrange", "!!document.querySelector('.arr[data-ref]')");
        shoot(&page, "2e-arrange-guest");
        page.js(&head("Done arranging"));
        page.js("document.querySelector('.side-project [aria-label=\"Project menu\"]').click()");
        page.js("[...document.querySelectorAll('.menu-item')].find(b => b.innerText.startsWith('Link with')).click()");
        shoot(&page, "2f-link-dialog");
        page.js("document.querySelector('.dialog .icon-btn').click()");
        // Linked projects. A station that cannot link yet is shown as if it
        // had: the page is told api and web are linked.
        if s.try_call("project.link", json!({"project": "api", "with": "web"}))
            .is_err()
        {
            page.js("(() => { const parse = JSON.parse; JSON.parse = (t) => { const v = parse(t); if (Array.isArray(v?.result)) for (const x of v.result) if (x && x.definition && x.name === 'api') x.links = [{ with: 'web', station: 'spark2' }]; return v; }; return true; })()");
        }
        page.wait(
            "linked",
            "!!document.querySelector('.sidebar .chip.linked')",
        );
        shoot(&page, "2g-linked-layout");
        page.goto("/s/spark2/w/api");
        page.wait("watch", "!!document.querySelector('.watch-card.guest')");
        shoot(&page, "2h-watch-linked");
        // A global layout with a pane that is in no project, pins, and menus.
        s.call("deck.layout.save", json!({"layout": {"name": "everything", "tree": {"dir": "row", "ratio": 550,
            "a": {"pane": names[0], "project": "api", "station": "spark2"},
            "b": {"dir": "col", "ratio": 500, "a": {"pane": "logs", "project": "web", "station": "spark2"}, "b": {"id": s.pane("scratch")["id"], "station": "spark2"}}}}}));
        s.call(
            "deck.pin",
            json!({"pin": {"kind": "layout", "name": "everything"}}),
        );
        s.call(
            "deck.pin",
            json!({"pin": {"kind": "pane", "station": "spark2", "id": dev}}),
        );
        page.wait("global layout row", &layout("everything"));
        page.wait(
            "global layout",
            "document.querySelectorAll('.workspace .pane').length === 3 && !!document.querySelector('.side-pinned')",
        );
        shoot(&page, "2i-global-layout");
        page.right_click("document.querySelector('.side-globals .side-layout')");
        shoot(&page, "2j-layout-menu");
        page.right_click("document.querySelector('.side-project')");
        shoot(&page, "2k-project-menu");
        page.right_click("document.querySelector('.sidebar')");
        shoot(&page, "2l-empty-space-menu");
        page.js(
            "document.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true}))",
        );
        // Hidden panes: two over one with the bottom one hidden, then with
        // the pane that needs the owner hidden, its menu, and all but one.
        let two_over_one = |hidden: Value| {
            s.call("project.layout.save", json!({"project": "api", "layout": {"name": "hide", "tree": {"dir": "col", "ratio": 600,
                "a": {"dir": "row", "ratio": 500, "a": {"pane": "dev"}, "b": {"pane": "reviewer"}}, "b": {"pane": "term"}}, "hidden": hidden}}));
        };
        two_over_one(json!([{"pane": "term"}]));
        page.wait("hide layout row", &layout("hide"));
        page.wait(
            "one hidden",
            "document.querySelectorAll('.workspace .pane').length === 2 && document.querySelectorAll('.hid-chip').length === 1",
        );
        shoot(&page, "17-hidden-bottom-pane");
        two_over_one(json!([{"pane": "reviewer"}]));
        page.wait(
            "the chip that needs you",
            "!!document.querySelector('.hid-chip.needs_you') && document.querySelectorAll('.workspace .pane').length === 2",
        );
        shoot(&page, "18-hidden-chip-needs-you");
        page.js("document.querySelector('.hid-menu').click()");
        shoot(&page, "19-hidden-chip-menu");
        page.js(
            "document.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true}))",
        );
        two_over_one(json!([{"pane": "reviewer"}, {"pane": "term"}]));
        page.wait(
            "two chips",
            "document.querySelectorAll('.hid-chip').length === 2 && document.querySelectorAll('.workspace .pane').length === 1",
        );
        shoot(&page, "20-hidden-all-but-one");
        two_over_one(json!([{"pane": "reviewer"}]));
        page.wait(
            "one chip again",
            "document.querySelectorAll('.hid-chip').length === 1",
        );
    }
    page.goto(&format!("/s/spark2/p/{dev}"));
    page.terminal_face();
    page.wait("pane", "!!document.querySelector('.pane .xterm-rows')");
    shoot(&page, "3-pane");
    page.js("document.querySelector('.pane').dispatchEvent(new MouseEvent('contextmenu', {bubbles: true, clientX: 700, clientY: 260}))");
    shoot(&page, "4-right-click-menu");
    page.js("document.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true}))");
    page.js("[...document.querySelectorAll('.topbar button')].find(b => b.innerText.startsWith('Approvals')).click()");
    shoot(&page, "5-approvals");
    page.js("document.querySelector('.dialog .icon-btn').click()");
    page.js("[...document.querySelectorAll('.topbar button')].find(b => b.innerText === 'Agent setup').click()");
    page.wait(
        "setup",
        "!!document.querySelector('.dialog [aria-label=\"Server name\"]')",
    );
    shoot(&page, "6-agent-setup");
    let close = "(document.querySelector('.dialog .icon-btn').click(), true)";
    page.js(close);
    page.js("[...document.querySelectorAll('.topbar button')].find(b => b.innerText === 'Usage').click()");
    page.wait("fleet usage", "!!document.querySelector('.usage-station')");
    shoot(&page, "13-fleet-usage");
    page.js(close);
    page.goto(&format!("/s/spark2/p/{dev}"));
    page.wait("the gauge", "!!document.querySelector('.strip .usage-ctx')");
    page.js("document.querySelector('.strip .usage-ctx').click()");
    page.wait(
        "pane usage",
        "!!document.querySelector('.usage-dialog .facts')",
    );
    shoot(&page, "14-pane-usage");
    let phone = Page::open(&s, 390, 844);
    phone.wait("phone", "!!document.querySelector('main.phone .approval')");
    shoot(&phone, "7-phone");
    phone.goto(&format!("/s/spark2/p/{dev}"));
    phone.wait(
        "phone pane",
        "(t => (t && t.click(), !!document.querySelector('.phone-pane .composer-box')))([...document.querySelectorAll('.phone-chat .chat-switch button')].find(b => b.innerText === 'Terminal'))",
    );
    phone.js("(document.querySelector('.composer-box').value = 'run the tests and tell me what fails', true)");
    shoot(&phone, "8-phone-pane");
    phone.js("document.querySelector('.phone-usage').click()");
    phone.wait(
        "phone usage",
        "!!document.querySelector('.usage-dialog .facts')",
    );
    shoot(&phone, "15-phone-pane-usage");
    phone.js("(document.querySelector('.dialog .icon-btn').click(), true)");
    phone.goto("/s/spark2/l/api/build");
    phone.wait(
        "phone layout",
        "document.querySelectorAll('.lay-map button').length === 3 && !!document.querySelector('.phone-layout .xterm-rows')",
    );
    shoot(&phone, "8b-phone-layout");
    phone.js("document.querySelectorAll('.lay-map button')[2].click()");
    shoot(&phone, "8c-phone-layout-third");
    phone.goto("/l/everything");
    phone.wait(
        "phone global layout",
        "document.querySelectorAll('.lay-map button').length === 3 && !!document.querySelector('.phone-layout .xterm-rows')",
    );
    shoot(&phone, "8d-phone-global-layout");
    if names.len() >= 3 {
        phone.goto("/s/spark2/l/api/hide");
        phone.wait(
            "phone layout with a hidden pane",
            "!!document.querySelector('.lay-map .map-box.hid') && !!document.querySelector('.phone-layout .xterm-rows')",
        );
        shoot(&phone, "21-phone-layout-hidden");
        phone.js("document.querySelector('.lay-map .map-box.hid').click()");
        phone.wait(
            "the hidden pane looked at",
            "document.querySelector('.lay-showing').innerText.includes('hidden')",
        );
        shoot(&phone, "22-phone-looking-at-hidden");
    }
    phone.goto("/s/spark2/w/api");
    phone.wait(
        "phone watch",
        "!!document.querySelector('.phone-watch .watch-card')",
    );
    shoot(&phone, "8e-phone-watch");
    phone.goto("/fleet");
    phone.wait(
        "phone list",
        "(b => (b && b.getAttribute('aria-pressed') !== 'true' && b.click(), !!document.querySelector('main.phone .phone-station')))([...document.querySelectorAll('.home-switch button')].find(b => b.innerText === 'By station'))",
    );
    phone.js("(document.querySelector('main.phone').scrollTop = 330, true)");
    shoot(&phone, "7b-phone-list-stations");
    phone.js("[...document.querySelectorAll('main.phone button')].find(b => b.innerText.startsWith('Usage')).click()");
    phone.wait(
        "phone fleet usage",
        "!!document.querySelector('.usage-station')",
    );
    shoot(&phone, "16-phone-fleet-usage");
    phone.js("(document.querySelector('.dialog .icon-btn').click(), true)");
    let fresh = Page::open_signed_out(&s, 390, 844);
    fresh.js("document.querySelector('main.login button').click()");
    fresh.wait("code", "!!document.querySelector('.big-code')");
    shoot(&fresh, "9-phone-sign-in");
    phone.goto("/fleet");
    phone.wait("access", "!!document.querySelector('.topbar')");
    phone.js("[...document.querySelectorAll('.topbar button')].find(b => b.innerText.startsWith('Access'))?.click()");
    shoot(&phone, "10-phone-access");
}

/// 0.2 step 1, done when: a program with no device token is refused; a
/// browser is signed in only by the owner's approval, once.
#[tokio::test(flavor = "multi_thread")]
async fn a_browser_is_signed_in_only_when_the_owner_approves_it() {
    let s = tokio::task::spawn_blocking(|| Station::start("alpha"))
        .await
        .unwrap();
    let post = |path: &'static str, body: Value| {
        let url = s.url(path);
        async move {
            let r = http().post(url).json(&body).send().await.unwrap();
            (
                r.status().as_u16(),
                r.json::<Value>().await.unwrap_or(Value::Null),
            )
        }
    };
    let (status, asked) = post("/api/device/request", json!({"name": "Mike's phone <b>"})).await;
    assert_eq!(status, 200, "{asked}");
    let asked = asked["result"].clone();
    let collect = json!({"id": asked["id"], "collect": asked["collect"]});
    assert_eq!(
        post("/api/device/collect", collect.clone()).await.1["result"]["status"],
        json!("waiting")
    );
    // Asking grants nothing.
    assert_eq!(rpc(&s, None, None, None, "pane.list").await.0, 401);
    // Someone who does not hold the asker's secret cannot collect.
    let (_, wrong) = post(
        "/api/device/collect",
        json!({"id": asked["id"], "collect": "guess"}),
    )
    .await;
    assert!(wrong["result"].is_null(), "{wrong}");

    // The owner sees the request, with the same code the browser shows.
    let view = tokio::task::block_in_place(|| s.call("trust.status", json!({})));
    let req = &view["requests"][0];
    assert_eq!(
        (req["kind"].clone(), req["code"].clone()),
        (json!("device"), asked["code"].clone())
    );
    assert_eq!(
        req["name"],
        json!("Mike's phone b"),
        "the name is plain text"
    );
    tokio::task::block_in_place(|| s.call("trust.approve", json!({"request": asked["code"]})));

    let (_, got) = post("/api/device/collect", collect.clone()).await;
    let token = got["result"]["token"]
        .as_str()
        .expect("a token")
        .to_string();
    // Handed over once.
    assert_eq!(
        post("/api/device/collect", collect).await.1["result"]["status"],
        json!("gone")
    );
    // The token is the owner's authority, and a changed one is nothing.
    assert_eq!(rpc(&s, Some(&token), None, None, "pane.list").await.0, 200);
    // Except for one thing: the address at which the station's Tailscale
    // client is signed in again is given on the machine's own deck, which
    // holds the station's own token, and to no browser signed in from
    // another device.
    let own = s.token();
    assert_eq!(
        rpc(&s, Some(&own), None, None, "tailnet.signin").await.0,
        200
    );
    let (_, refused) = rpc(&s, Some(&token), None, None, "tailnet.signin").await;
    assert_eq!(refused["error"]["code"], json!("denied"), "{refused}");
    let mut forged = token.clone();
    forged.replace_range(
        forged.len() - 1..,
        if token.ends_with('0') { "1" } else { "0" },
    );
    assert_eq!(rpc(&s, Some(&forged), None, None, "pane.list").await.0, 401);
    // Nothing the station says or logs contains the token.
    let log =
        tokio::task::block_in_place(|| s.call("log.query", json!({"limit": 500}))).to_string();
    assert!(!log.contains(&token) && !view.to_string().contains(&token));

    // Revoked: refused from then on.
    let device = tokio::task::block_in_place(|| s.call("trust.status", json!({})))["devices"][0]
        ["id"]
        .as_str()
        .unwrap()
        .to_string();
    tokio::task::block_in_place(|| s.call("device.revoke", json!({"device": device})));
    assert_eq!(rpc(&s, Some(&token), None, None, "pane.list").await.0, 401);
}

/// 0.2 step 1: the same sign-in, as the owner meets it in a browser.
#[test]
fn a_new_browser_shows_a_code_and_opens_the_deck_once_it_is_approved() {
    use common::browser::Page;
    let s = Station::start("alpha");
    let page = Page::open_signed_out(&s, 1100, 700);
    page.wait(
        "the sign-in button",
        "!!document.querySelector('main.login button')",
    );
    assert!(
        s.call("trust.status", json!({}))["requests"]
            .as_array()
            .unwrap()
            .is_empty(),
        "opening the page asks for nothing by itself"
    );
    page.js("document.querySelector('main.login button').click()");
    let shown = page.wait("the code", "document.querySelector('.big-code')?.innerText");
    let waiting = wait_for("the request to reach the station", 10, || {
        s.call("trust.status", json!({}))["requests"]
            .get(0)
            .cloned()
    });
    assert_eq!(waiting["code"], shown, "both sides show the same code");
    s.call("trust.approve", json!({"request": waiting["id"]}));
    page.wait("the deck to open", "!!document.querySelector('.topbar')");
    // It is listed, and can be signed out.
    let devices = s.call("trust.status", json!({}))["devices"].clone();
    assert_eq!(devices.as_array().unwrap().len(), 1, "{devices}");
}

/// A bug that happened: with a pane open full size, typing in a dialog lost
/// the keyboard to the terminal every few seconds.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn a_dialog_keeps_the_keyboard_while_a_pane_is_open() {
    use common::browser::Page;
    let prices = fake_prices::Prices::start();
    let s = Station::start_with("alpha", &prices.config(), &[]);
    let pane = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "sh", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let page = Page::open(&s, 1200, 760);
    page.goto(&format!("/s/alpha/p/{pane}"));
    page.wait(
        "the pane",
        "!!document.querySelector('.station-main.zoomed .xterm-rows')",
    );
    page.js("document.dispatchEvent(new KeyboardEvent('keydown', {key: 'k', ctrlKey: true, bubbles: true}))");
    page.wait("the palette", "!!document.querySelector('.dialog input')");
    page.js("(document.querySelector('.dialog input').focus(), window.__in = document.querySelector('.dialog input'), true)");
    page.type_str("new pro");
    page.redraw(&s, &pane);
    page.type_str("ject");
    assert_eq!(
        page.js("[document.activeElement === window.__in, window.__in.value]"),
        json!([true, "new project"]),
        "the dialog kept the keyboard through a redraw"
    );
    assert!(
        !s.text(&pane).contains("ject"),
        "nothing typed went to the terminal"
    );

    // Asked for on 14 Nov 2026: the strip shows the agent's model and how
    // full its context is, and the pane's Usage dialog shows the totals and
    // an estimated cost that is called an estimate.
    page.js("document.querySelector('.dialog .icon-btn').click()");
    let agent = s.fake(
        json!({"steps": [{"prompt": "go"}, {"transcript": {"type": "assistant", "message": {
            "id": "msg_1", "model": "claude-opus-5-5",
            "usage": {"input_tokens": 100, "output_tokens": 250,
                      "cache_read_input_tokens": 700000, "cache_creation_input_tokens": 50}}}}]}),
        json!({"name": "dev"}),
    );
    page.goto(&format!("/s/alpha/p/{agent}"));
    page.terminal_face();
    let strip = page.wait(
        "the model and the context gauge in the strip",
        "(m => m && document.querySelector('.strip .usage-ctx .gauge i') && document.querySelector('.strip .usage-ctx').title.includes('at list prices') ? [m.innerText, document.querySelector('.strip .usage-ctx').innerText, document.querySelector('.strip .usage-ctx').title] : false)(document.querySelector('.strip .usage-model'))",
    );
    assert_eq!(strip[0], json!("opus-5-5"));
    assert_eq!(strip[1], json!("70%"), "700,150 of the list's 1,000,000");
    let title = strip[2].as_str().unwrap();
    assert!(
        title.contains("Model: claude-opus-5-5")
            && title.contains("Context: 700,150 tokens of 1,000,000"),
        "{title}"
    );
    page.js("document.querySelector('.strip .icon-btn').click()");
    page.wait("the pane menu", "!!document.querySelector('.menu')");
    page.choose("Usage");
    let text = page.wait(
        "the usage dialog",
        "(d => d && d.innerText.includes('Est. cost') ? d.innerText : false)(document.querySelector('.dialog'))",
    );
    let text = text.as_str().unwrap();
    for part in [
        "claude-opus-5-5",
        "Total\n700k", // 100 + 250 + 700,000 + 50
        "Estimated cost",
        "$0.15", // 100 x 4 + 250 x 20 + 700,000 x 0.2 + 50 x 5, per million
        "an estimate at list prices from 127.0.0.1",
        "What you actually pay depends on your plan.",
        "No sub-agent usage",
    ] {
        assert!(text.contains(part), "no {part:?} in:\n{text}");
    }
}

/// The owner's request (15 Nov 2026): "if I create one I like I should be able
/// to set as a theme to reuse", for the whole deck, with a layout's or a
/// pane's own look winning over it. Done when: what is being edited shows on
/// the page before anything is saved; a theme saved in the Appearance editor
/// is on the station and on the page, and still after a reload; a pane given
/// its own look shows it beside a pane that follows the deck; a pane's
/// program can change none of it; a value that is more than a colour is
/// refused by the station; and a closed pane's look is forgotten. Fonts (the
/// same day's request): a file that is not a font is refused, and a pane's
/// program can neither add a font nor take one away.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn a_saved_theme_is_used_again_and_a_panes_own_look_wins() {
    use common::browser::Page;
    let s = Station::start("alpha");
    let spawn = |name: &str| {
        s.call(
            "pane.spawn",
            json!({"program": "/bin/sh", "name": name, "cwd": s.root.join("home")}),
        )["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (one, two) = (spawn("one"), spawn("two"));
    s.call(
        "deck.layout.save",
        json!({"layout": {"name": "both", "tree": {"dir": "row", "ratio": 500, "a": {"id": one}, "b": {"id": two}}}}),
    );
    let page = Page::open(&s, 1400, 900);
    page.goto("#/l/both");
    // The page's background, then each terminal's.
    let shown = "(bg => document.querySelectorAll('.pane .xterm-rows').length === 2 && [getComputedStyle(document.body).backgroundColor, bg('one'), bg('two')])(n => [...document.querySelectorAll('.pane')].find(p => p.paneTerm.pane.name === n).paneTerm.term.options.theme.background)";
    let shows = |what: &str, want: Value| {
        page.wait(
            what,
            &format!("JSON.stringify({shown}) === {}", json!(want.to_string())),
        );
    };
    let press = |sel: &str, text: &str| {
        page.wait(
            text,
            &format!(
                "(b => b ? (b.click(), true) : false)([...document.querySelectorAll({})].find(b => b.innerText.trim() === {}))",
                json!(sel),
                json!(text)
            ),
        );
    };
    let background = |hex: &str| {
        page.wait(
            "the terminal background's box",
            &format!("(i => i ? (i.value = {}, i.dispatchEvent(new Event('input')), true) : false)(document.querySelector('.look-row[data-key=\"term.background\"] .look-hex'))", json!(hex)),
        );
    };
    shows(
        "the deck as it always was",
        json!(["rgb(15, 19, 24)", "#0f1318", "#0f1318"]),
    );

    // The deck: the light theme with one colour changed. It is on the page
    // at once and nowhere else until it is saved.
    press(".topbar button", "Appearance");
    page.wait(
        "the editor",
        "(s => s ? (s.value = 'spandeck-light', s.dispatchEvent(new Event('change')), true) : false)(document.querySelector('#look-theme'))",
    );
    background("#101820");
    let light = json!(["rgb(244, 246, 248)", "#101820", "#101820"]);
    shows("the change as it is made", light.clone());
    assert_eq!(s.call("look.get", json!({}))["deck"], json!({}));
    press(".look-foot button", "Save as theme…");
    page.js("document.querySelector('.look-namer input').value = 'Mine'");
    press(".look-namer button", "Save theme");
    page.wait(
        "the new theme to be the one chosen",
        "document.querySelector('#look-theme').selectedOptions[0].textContent === 'Mine'",
    );
    press(".look-foot button", "Save");
    page.wait("the editor to close", "!document.querySelector('.dialog')");
    let doc = s.call("look.get", json!({}));
    let mine = &doc["themes"][0];
    assert_eq!(
        (
            &mine["name"],
            &mine["settings"]["term.background"],
            &mine["settings"]["deck.bg"]
        ),
        (&json!("Mine"), &json!("#101820"), &json!("#f4f6f8")),
        "{doc}"
    );
    assert_eq!(doc["deck"], json!({"theme": mine["id"]}), "{doc}");
    page.reload();
    shows("the theme after a reload", light);

    // One pane gets a look of its own: it wins there, and only there.
    page.right_click(
        "[...document.querySelectorAll('.pane')].find(p => p.paneTerm.pane.name === 'two')?.querySelector('.strip')",
    );
    page.choose("Appearance");
    background("#300000");
    press(".look-foot button", "Save");
    shows(
        "the pane's own look beside the deck's",
        json!(["rgb(244, 246, 248)", "#101820", "#300000"]),
    );
    // The page shows the look before the station has stored it (the store
    // was read too soon once, on the Linux CI runner).
    page.wait("the editor to close", "!document.querySelector('.dialog')");
    let key = format!("alpha/{two}");
    let want = json!({ key.as_str(): {"set": {"term.background": "#300000"}} });
    wait_for("the pane's look to be stored", 10, || {
        (s.call("look.get", json!({}))["panes"] == want).then_some(())
    });

    // Rule 7: how the deck looks is the owner's. A pane's program changes none of it.
    for (method, params) in [
        (
            "look.set",
            json!({"level": "deck", "choice": {"theme": "nord"}}),
        ),
        ("look.theme.save", json!({"theme": {"name": "Theirs"}})),
        ("look.theme.remove", json!({"id": mine["id"]})),
        ("look.push", json!({"look": {"updated_ms": u64::MAX}})),
        (
            "look.font.add",
            json!({"name": "Theirs", "data_b64": "d09GMgAA"}),
        ),
        ("look.font.remove", json!({"id": "0".repeat(32)})),
    ] {
        assert_eq!(
            s.call_as_pane(&one, method, params).unwrap_err().code,
            "denied",
            "{method}"
        );
    }
    // What a theme file or another station sends is data: more than a colour is refused.
    let bad = json!({"theme": {"name": "Bad", "settings": {"deck.bg": "#fff;}</style><script>alert(1)</script>"}}});
    assert_eq!(
        s.try_call("look.theme.save", bad).unwrap_err().code,
        "invalid"
    );
    // "<svg onload=…>" under a font's name is not a font.
    let not_a_font = json!({"name": "Mine", "data_b64": "PHN2ZyBvbmxvYWQ9YWxlcnQoMSk+"});
    assert_eq!(
        s.try_call("look.font.add", not_a_font).unwrap_err().code,
        "invalid"
    );
    // A pane that is closed takes its look with it.
    s.call("pane.close", json!({"pane": two}));
    assert_eq!(s.call("look.get", json!({}))["panes"], json!({}));
}

/// Hide and show (the owner's request, 15 Nov 2026): "if you have a layout of
/// 2 over 1, if you hide the bottom one the 2 top ones would resize to take
/// all the space, if you [unhide] it it will go back to its original size."
/// Hiding changes only what is seen: the hidden pane keeps running at the
/// size it had, stays in sight as a chip, is still hidden after a reload, and
/// comes back where it was with its screen as its program left it.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn a_hidden_pane_gives_its_room_to_the_others_and_comes_back_as_it_was() {
    use common::browser::Page;
    let s = Station::start("alpha");
    let spawn = |name: &str| {
        s.call(
            "pane.spawn",
            json!({"program": "/bin/sh", "name": name, "cwd": s.root.join("home")}),
        )["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (one, two, three) = (spawn("one"), spawn("two"), spawn("three"));
    s.call(
        "deck.layout.save",
        json!({"layout": {"name": "trio", "tree": {"dir": "col", "ratio": 600,
            "a": {"dir": "row", "ratio": 500, "a": {"id": one}, "b": {"id": two}}, "b": {"id": three}}}}),
    );
    s.call(
        "pane.send_text",
        json!({"pane": three, "text": "echo keep-$((40+2))"}),
    );
    s.wait_text(&three, "keep-42");
    let page = Page::open(&s, 1400, 900);
    page.goto("#/l/trio");

    // What is on screen: each pane's name, its height in the layout and its
    // terminal's rows, top to bottom and left to right; and the chips.
    let seen = "[...document.querySelectorAll('.workspace .pane')].map(p => [p.paneTerm.pane.name, Math.round(p.getBoundingClientRect().height), p.paneTerm.term.rows])";
    let chips =
        "[...document.querySelectorAll('.lay-hidden .hid-chip .hid-name')].map(c => c.innerText)";
    let rows_of = |id: &str| s.pane(id)["rows"].as_u64().unwrap();
    // The layout as the browser fitted it, once the station has each size.
    let settled = |what: &str, panes: usize| {
        wait_for(what, 20, || {
            let v = page.js(seen);
            let list = v.as_array()?;
            (list.len() == panes
                && list.iter().all(|p| {
                    let id = match p[0].as_str() {
                        Some("one") => &one,
                        Some("two") => &two,
                        _ => &three,
                    };
                    Some(rows_of(id)) == p[2].as_u64()
                }))
            .then_some(v)
        })
    };
    let before = settled("two over one", 3);
    let (top, bottom) = (
        before[0][1].as_u64().unwrap(),
        before[2][1].as_u64().unwrap(),
    );
    assert_eq!(before[2][0], json!("three"), "{before}");
    assert!(top > bottom && bottom > 200, "{before}");
    let hidden_rows = rows_of(&three);

    // Hide the bottom pane from its title bar.
    page.wait(
        "the Hide button on the bottom pane",
        "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.workspace .pane')].find(p => p.paneTerm.pane.name === 'three')?.querySelector('.strip-hide'))",
    );
    let hidden = settled("the top two to take the room", 2);
    for p in hidden.as_array().unwrap() {
        // The whole height (the line between the parts went with the pane),
        // and more rows in the terminal, which the station was told of.
        assert!(
            p[1].as_u64().unwrap() >= top + bottom && p[2].as_u64() > before[0][2].as_u64(),
            "{hidden} after {before}"
        );
    }
    assert_eq!(page.js(chips), json!(["three"]));
    assert_eq!(
        page.js("document.querySelector('.hid-show').getAttribute('aria-label')"),
        json!("Show three, Shell, idle. Hidden.")
    );
    // Saved with the layout (the deck gave each pane its station, so the
    // layout means the same panes on every station), and nothing of it reached the pane: it runs on
    // at the size it had, and its screen is not on this page.
    wait_for("the layout to be saved with its hidden pane", 10, || {
        (s.call("deck.get", json!({}))["layouts"][0]["hidden"]
            == json!([{"id": three, "station": "alpha"}]))
        .then_some(())
    });
    s.call(
        "pane.send_text",
        json!({"pane": three, "text": "echo while-hidden-$((1+1))"}),
    );
    s.wait_text(&three, "while-hidden-2");
    assert_eq!(rows_of(&three), hidden_rows);
    assert!(!page.term_text().contains("keep-42"));

    // Still hidden after a reload.
    page.reload();
    page.wait(
        "the layout after a reload",
        &format!("JSON.stringify({chips}) === '[\"three\"]' && document.querySelectorAll('.workspace .pane .xterm-rows').length === 2"),
    );

    // The chip shows it again: the three are where they were, at the sizes
    // they had, and the pane's screen has what was printed, hidden or not.
    page.js("document.querySelector('.hid-show').click()");
    let after = settled("the three back", 3);
    assert_eq!(after, before);
    assert_eq!(page.js(chips), json!([]));
    wait_for("the shown pane's screen", 10, || {
        let text = page.term_text();
        (text.contains("keep-42") && text.contains("while-hidden-2")).then_some(())
    });
    wait_for("the layout to be saved with nothing hidden", 10, || {
        s.call("deck.get", json!({}))["layouts"][0]
            .get("hidden")
            .is_none()
            .then_some(())
    });

    // "Show only this" hides the others in one step; the one left showing
    // cannot be hidden; "Show all" brings them back.
    page.right_click(
        "[...document.querySelectorAll('.workspace .pane')].find(p => p.paneTerm.pane.name === 'two')?.querySelector('.strip')",
    );
    page.choose("Show only this");
    wait_for_or(
        "one pane and two chips",
        20,
        || {
            format!(
                "DIAG chips={} seen={} striphide={} deck={}",
                page.js(chips),
                page.js(&format!("JSON.stringify({seen})")),
                page.js("document.querySelectorAll('.strip-hide').length"),
                s.call("deck.get", json!({}))["layouts"]
            )
        },
        || {
            let v = page.js(&format!("JSON.stringify({chips}) === '[\"one\",\"three\"]' && JSON.stringify({seen}.map(p => p[0])) === '[\"two\"]' && !document.querySelector('.strip-hide')"));
            (v == json!(true)).then_some(())
        },
    );
    page.right_click("document.querySelector('.workspace .pane .strip')");
    assert_eq!(
        page.js("[...document.querySelectorAll('.menu-item')].filter(i => i.innerText.startsWith('Hide')).map(i => i.disabled)"),
        json!([true])
    );
    page.js("document.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true}))");
    page.wait(
        "Show all",
        "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.lay-hidden button')].find(b => b.innerText === 'Show all'))",
    );
    assert_eq!(settled("all three again", 3), before);
}
