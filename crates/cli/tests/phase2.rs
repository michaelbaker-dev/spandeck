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
    let pane_token = std::process::Command::new("sqlite3")
        .arg(s.root.join("state/spandeck.db"))
        .arg(format!("SELECT token FROM panes WHERE id='{pane}'"))
        .output()
        .unwrap();
    let pane_token = String::from_utf8_lossy(&pane_token.stdout)
        .trim()
        .to_string();
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
    drop(tokio::task::spawn_blocking(move || drop(s)).await);
}

use common::browser::Page;

#[test]
fn browser_opens_a_pane_types_splits_resizes_and_drops_a_file() {
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
    page.tab.type_str("echo typed-$((20+3))").unwrap();
    page.tab.press_key("Enter").unwrap();
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

    // Split: open the first pane in a workspace, then the second beside it.
    page.goto("/s/alpha");
    page.wait(
        "the deck again",
        "document.querySelectorAll('.pane-tile').length === 2",
    );
    page.js("[...document.querySelectorAll('.side-pane')].find(x => x.innerText.includes('one')).dispatchEvent(new MouseEvent('contextmenu', {bubbles: true, clientX: 60, clientY: 120}))");
    page.wait("the right-click menu", "!!document.querySelector('.menu')");
    page.js("[...document.querySelectorAll('.menu-item')].find(i => i.innerText.includes('Open in workspace')).click()");
    page.wait(
        "one pane in the workspace",
        "document.querySelectorAll('.workspace .pane').length === 1",
    );
    page.js("document.querySelector('.workspace .pane').dispatchEvent(new MouseEvent('contextmenu', {bubbles: true, clientX: 600, clientY: 300}))");
    page.js("[...document.querySelectorAll('.menu-item')].find(i => i.innerText.includes('Split right')).click()");
    page.wait(
        "the pane picker",
        "!!document.querySelector('.dialog .list-row')",
    );
    page.js("[...document.querySelectorAll('.dialog .list-row')].find(r => r.innerText.includes('two')).click()");
    page.wait(
        "two panes side by side",
        "document.querySelectorAll('.workspace .split.row .pane').length === 2",
    );

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
    let s = Station::start("spark2");
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
        json!({"steps": [{"say": "● I'll add the importer and its tests."}, {"prompt": "add the importer"}, {"tool": {"name": "Edit", "file": "src/importer.rs"}}, {"say": "● Running the test suite…"}, {"work": 600000}]}),
    );
    spawn(
        "reviewer",
        "reviewer",
        json!({"steps": [{"prompt": "clean the build"}, {"say": "I need to remove the build directory first."}, {"permission": {"name": "Bash", "command": "rm -rf build && cargo build --release"}}, {"sleep": 600000}]}),
    );
    spawn(
        "tester",
        "tester",
        json!({"steps": [{"prompt": "run tests"}, {"tool": {"name": "Bash", "command": "cargo test"}}, {"turn_end": "All 42 tests pass."}]}),
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
        page.wait("layout tab", "(t => t ? (t.click(), true) : false)([...document.querySelectorAll('.tab.proj')].find(t => t.innerText === 'build'))");
        page.wait(
            "layout",
            "document.querySelectorAll('.workspace .pane').length === 3",
        );
        shoot(&page, "2b-project-layout");
        page.js("[...document.querySelectorAll('.tabs button')].find(b => b.innerText === 'Arrange').click()");
        page.wait(
            "arrange",
            "document.querySelectorAll('.arr[data-ref]').length === 3",
        );
        page.js("document.querySelector('.arr[data-ref]').dataset.zone = 'right'");
        shoot(&page, "2c-arrange");
        page.js("[...document.querySelectorAll('.tabs button')].find(b => b.innerText === 'Done arranging').click()");

        // A guest pane from another project, and how Arrange lists the panes.
        let web = s.git_repo("web");
        s.call(
            "project.new",
            json!({"path": web, "panes": [{"name": "logs", "agent": "shell"}], "open": true}),
        );
        s.call("pane.spawn", json!({"agent": "shell", "name": "scratch"}));
        s.call("project.layout.save", json!({"project": "api", "layout": {"name": "with-logs", "tree": {"dir": "row", "ratio": 600,
            "a": {"pane": names[0]}, "b": {"pane": "logs", "project": "web"}}}}));
        page.wait("guest layout tab", "(t => t ? (t.click(), true) : false)([...document.querySelectorAll('.tab.proj')].find(t => t.innerText.endsWith('with-logs')))");
        page.wait(
            "guest",
            "!!document.querySelector('.workspace .pane .chip.guest')",
        );
        shoot(&page, "2d-guest-in-layout");
        page.js("[...document.querySelectorAll('.tabs button')].find(b => b.innerText === 'Arrange').click()");
        page.wait("arrange", "!!document.querySelector('.side-section')");
        shoot(&page, "2e-arrange-guest");
        page.js("[...document.querySelectorAll('.tabs button')].find(b => b.innerText === 'Done arranging').click()");
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
        page.js("[...document.querySelectorAll('.tab')].find(t => t.innerText.endsWith('Watch') && t.innerText.startsWith('api')).click()");
        page.wait("watch", "!!document.querySelector('.watch-card.guest')");
        shoot(&page, "2h-watch-linked");
        page.js("[...document.querySelectorAll('.tab')].find(t => t.innerText === 'Deck').click()");
    }
    page.goto(&format!("/s/spark2/p/{dev}"));
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
    let phone = Page::open(&s, 390, 844);
    phone.wait("phone", "!!document.querySelector('main.phone .approval')");
    shoot(&phone, "7-phone");
    phone.goto(&format!("/s/spark2/p/{dev}"));
    phone.wait(
        "phone pane",
        "!!document.querySelector('.phone-pane .composer-box')",
    );
    phone.js("(document.querySelector('.composer-box').value = 'run the tests and tell me what fails', true)");
    shoot(&phone, "8-phone-pane");
    phone.goto("/s/spark2/l/api/build");
    phone.wait(
        "phone layout",
        "document.querySelectorAll('.lay-map button').length === 3 && !!document.querySelector('.phone-layout .xterm-rows')",
    );
    shoot(&phone, "8b-phone-layout");
    phone.js("document.querySelectorAll('.lay-map button')[2].click()");
    shoot(&phone, "8c-phone-layout-third");
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
fn a_dialog_keeps_the_keyboard_while_a_pane_is_open() {
    use common::browser::Page;
    let s = Station::start("alpha");
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
    page.tab.type_str("new pro").unwrap();
    std::thread::sleep(Duration::from_millis(3300));
    page.tab.type_str("ject").unwrap();
    assert_eq!(
        page.js("[document.activeElement === window.__in, window.__in.value]"),
        json!([true, "new project"]),
        "the dialog kept the keyboard through a redraw"
    );
    assert!(
        !s.text(&pane).contains("ject"),
        "nothing typed went to the terminal"
    );
}
