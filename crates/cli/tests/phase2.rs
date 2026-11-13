//! Phase 2 acceptance: the web client.
//! Done when: browser smoke tests open a pane, type, split, resize and drop a
//! file; a request with a wrong origin or no token is refused.
mod common;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap()
}

async fn rpc(s: &Station, token: Option<&str>, origin: Option<&str>, host: Option<&str>, method: &str) -> (u16, Value) {
    let mut req = http().post(s.url("/api/rpc")).json(&json!({"method": method, "params": {}}));
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
    let s = tokio::task::spawn_blocking(|| Station::start("alpha")).await.unwrap();
    let token = s.token();
    let good_origin = format!("http://127.0.0.1:{}", s.port);

    let (status, body) = rpc(&s, Some(&token), Some(&good_origin), None, "station.info").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["station"], json!("alpha"));

    // No token, wrong token.
    assert_eq!(rpc(&s, None, None, None, "station.info").await.0, 401);
    assert_eq!(rpc(&s, Some("0000"), None, None, "station.info").await.0, 401);
    // A page on another site calling this station (CSRF, DNS rebinding).
    assert_eq!(rpc(&s, Some(&token), Some("http://evil.example"), None, "station.info").await.0, 403);
    assert_eq!(rpc(&s, Some(&token), None, Some("evil.example"), "station.info").await.0, 403);

    // The station-to-station endpoint does not accept loopback callers or browsers.
    let peer = http().post(s.url("/api/peer/rpc")).json(&json!({"method": "station.info", "from": "x"})).send().await.unwrap();
    assert_eq!(peer.status().as_u16(), 403);

    // An agent's own token works over HTTP too, with an agent's limits.
    let pane = tokio::task::block_in_place(|| s.fake(json!({"steps": []}), json!({})));
    let pane_token = std::process::Command::new("sqlite3")
        .arg(s.root.join("state/spandeck.db"))
        .arg(format!("SELECT token FROM panes WHERE id='{pane}'"))
        .output()
        .unwrap();
    let pane_token = String::from_utf8_lossy(&pane_token.stdout).trim().to_string();
    assert_eq!(rpc(&s, Some(&pane_token), None, None, "pane.list").await.0, 200);
    assert_eq!(rpc(&s, Some(&pane_token), None, None, "stop.all").await.0, 403);
    drop(tokio::task::spawn_blocking(move || drop(s)).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_websocket_must_authenticate_in_its_first_message() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    let s = tokio::task::spawn_blocking(|| Station::start("alpha")).await.unwrap();
    let url = format!("ws://127.0.0.1:{}/api/ws", s.port);

    // Wrong token: told so, then nothing works.
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(Message::Text(json!({"auth": {"token": "nope"}}).to_string().into())).await.unwrap();
    let hello: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(hello["auth"]["ok"], json!(false));

    // A cross-origin page cannot even open the socket.
    let mut req = url.clone().into_client_request().unwrap();
    req.headers_mut().insert("origin", "http://evil.example".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(req).await.is_err());

    // Right token: RPC and a pane stream on the one socket.
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(Message::Text(json!({"auth": {"token": s.token(), "client": "test-1"}}).to_string().into())).await.unwrap();
    let hello: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(hello["auth"]["ok"], json!(true));
    ws.send(Message::Text(json!({"id": 1, "method": "pane.spawn", "params": {"program": "/bin/sh", "cwd": s.root.join("home")}}).to_string().into())).await.unwrap();
    let spawned: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    let pane = spawned["result"]["id"].as_str().unwrap().to_string();
    ws.send(Message::Text(json!({"id": 2, "method": "pane.attach", "params": {"pane": pane}}).to_string().into())).await.unwrap();
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

// ------------------------------------------------------------ browser smoke

fn chrome() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("SPANDECK_CHROME") {
        return Some(p.into());
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let mut found = vec![];
    for base in [format!("{home}/.cache/ms-playwright"), format!("{home}/Library/Caches/ms-playwright")] {
        for entry in std::fs::read_dir(&base).into_iter().flatten().flatten() {
            for sub in std::fs::read_dir(entry.path()).into_iter().flatten().flatten() {
                for name in ["chrome-headless-shell", "chrome", "Chromium.app/Contents/MacOS/Chromium"] {
                    let p = sub.path().join(name);
                    if p.is_file() {
                        found.push(p);
                    }
                }
            }
        }
    }
    found.sort();
    found.pop().or_else(|| {
        ["/usr/bin/google-chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"]
            .iter()
            .map(std::path::PathBuf::from)
            .find(|p| p.is_file())
    })
}

struct Page {
    _browser: headless_chrome::Browser,
    tab: std::sync::Arc<headless_chrome::Tab>,
}

impl Page {
    fn open(s: &Station, width: u32, height: u32) -> Page {
        let path = chrome().expect("no Chrome found: set SPANDECK_CHROME to a Chrome or Chromium binary");
        let browser = headless_chrome::Browser::new(
            headless_chrome::LaunchOptions::default_builder()
                .path(Some(path))
                .window_size(Some((width, height)))
                .sandbox(false)
                .idle_browser_timeout(Duration::from_secs(120))
                .build()
                .unwrap(),
        )
        .unwrap();
        let tab = browser.new_tab().unwrap();
        tab.navigate_to(&format!("{}#token={}", s.url("/"), s.token())).unwrap();
        tab.wait_for_element(".topbar").unwrap();
        Page { _browser: browser, tab }
    }

    fn js(&self, expr: &str) -> Value {
        // Values come back as JSON text, so arrays and objects survive.
        let wrapped = format!("(async () => JSON.stringify(await ({expr})))()");
        let r = self.tab.evaluate(&wrapped, true).unwrap_or_else(|e| panic!("js failed: {e}\n{expr}"));
        r.value.and_then(|v| v.as_str().and_then(|s| serde_json::from_str(s).ok())).unwrap_or(Value::Null)
    }

    fn wait(&self, what: &str, expr: &str) -> Value {
        wait_for(what, 20, || {
            let v = self.js(expr);
            (!v.is_null() && v != json!(false) && v != json!("")).then_some(v)
        })
    }

    fn goto(&self, hash: &str) {
        self.js(&format!("location.hash = {}", json!(hash)));
    }

    /// The text on a pane's terminal as the browser shows it.
    fn term_text(&self) -> String {
        self.js("[...document.querySelectorAll('.pane .xterm-rows')].map(r => r.innerText).join('\\n----\\n')").as_str().unwrap_or("").to_string()
    }
}

#[test]
fn browser_opens_a_pane_types_splits_resizes_and_drops_a_file() {
    let s = Station::start("alpha");
    let a = s.call("pane.spawn", json!({"program": "/bin/sh", "name": "one", "cwd": s.root.join("home")}))["id"].as_str().unwrap().to_string();
    let b = s.call("pane.spawn", json!({"program": "/bin/sh", "name": "two", "cwd": s.root.join("home")}))["id"].as_str().unwrap().to_string();
    let page = Page::open(&s, 1400, 900);

    // The token is taken out of the address bar once it has been stored.
    page.wait("the fleet view", "document.querySelector('.tile.station') && !location.hash.includes('token')");
    assert!(page.js("document.querySelector('.tile.station').innerText").as_str().unwrap().contains("alpha"));

    // Zoom: fleet -> station (the deck) -> pane. Every level has a URL.
    page.js("document.querySelector('.tile.station').click()");
    page.wait("the deck", "location.hash === '#/s/alpha' && document.querySelectorAll('.pane-tile').length === 2");
    page.js("[...document.querySelectorAll('.pane-tile')].find(t => t.innerText.includes('one')).click()");
    page.wait("the pane view", &format!("location.hash === '#/s/alpha/p/{a}' && !!document.querySelector('.pane .xterm-rows')"));

    // Type into the terminal through the browser.
    page.wait("terminal focus", "document.querySelector('.pane .xterm-helper-textarea') && (document.querySelector('.pane .xterm-helper-textarea').focus(), true)");
    page.tab.type_str("echo typed-$((20+3))").unwrap();
    page.tab.press_key("Enter").unwrap();
    wait_for("typed text on screen", 15, || page.term_text().contains("typed-23").then_some(()));
    s.wait_text(&a, "typed-23");

    // The browser fitted the pane to its window and told the station.
    let size = wait_for("resize to reach the station", 10, || {
        let p = s.pane(&a);
        (p["cols"].as_u64().unwrap() > 120).then_some(p)
    });
    assert_eq!(size["controller"].as_str().map(|c| c.starts_with("web-")), Some(true));

    // Split: open the first pane in a workspace, then the second beside it.
    page.goto("/s/alpha");
    page.wait("the deck again", "document.querySelectorAll('.pane-tile').length === 2");
    page.js("[...document.querySelectorAll('.side-pane')].find(x => x.innerText.includes('one')).dispatchEvent(new MouseEvent('contextmenu', {bubbles: true, clientX: 60, clientY: 120}))");
    page.wait("the right-click menu", "!!document.querySelector('.menu')");
    page.js("[...document.querySelectorAll('.menu-item')].find(i => i.innerText.includes('Open in workspace')).click()");
    page.wait("one pane in the workspace", "document.querySelectorAll('.workspace .pane').length === 1");
    page.js("document.querySelector('.workspace .pane').dispatchEvent(new MouseEvent('contextmenu', {bubbles: true, clientX: 600, clientY: 300}))");
    page.js("[...document.querySelectorAll('.menu-item')].find(i => i.innerText.includes('Split right')).click()");
    page.wait("the pane picker", "!!document.querySelector('.dialog .list-row')");
    page.js("[...document.querySelectorAll('.dialog .list-row')].find(r => r.innerText.includes('two')).click()");
    page.wait("two panes side by side", "document.querySelectorAll('.workspace .split.row .pane').length === 2");

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
    wait_for("the right pane to grow", 10, || (s.pane(&b)["cols"].as_u64().unwrap() > before + 10).then_some(()));

    // Drop a file on a pane: the station writes it and types its path.
    page.js(
        "(() => { const dt = new DataTransfer(); dt.items.add(new File(['spandeck drop test'], 'shot one.png', {type: 'image/png'})); \
         const pane = document.querySelectorAll('.workspace .pane')[0]; \
         pane.dispatchEvent(new DragEvent('drop', {bubbles: true, cancelable: true, dataTransfer: dt})); return true; })()",
    );
    // The path may wrap on a narrow pane, so compare without line breaks.
    let dir = s.root.join("state/drops").join(&a);
    let file = wait_for("the dropped file on the station", 15, || std::fs::read_dir(&dir).ok()?.flatten().next().map(|e| e.path()));
    assert!(file.file_name().unwrap().to_str().unwrap().ends_with("-shot_one.png"), "{file:?}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "spandeck drop test");
    wait_for("its path typed into the pane", 15, || s.text(&a).replace('\n', "").contains(file.to_str().unwrap()).then_some(()));
}

#[test]
fn a_second_browser_letterboxes_instead_of_resizing_the_pane() {
    let s = Station::start("alpha");
    let a = s.call("pane.spawn", json!({"program": "/bin/sh", "name": "one", "cwd": s.root.join("home")}))["id"].as_str().unwrap().to_string();
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
    assert_eq!(s.pane(&a)["cols"].as_u64().unwrap(), small_cols, "the letterboxed client did not fight over the size");
}
