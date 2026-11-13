//! Phase 5 acceptance: the fleet.
//! Done when: two daemons discover and trust each other with no setup; a stop
//! sent to an unreachable station shows as unreachable, then is delivered on
//! reconnect; no push arrives while you are at a keyboard.
mod common;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

const SIGSTOP: i32 = if cfg!(target_os = "macos") { 17 } else { 19 };
const SIGCONT: i32 = if cfg!(target_os = "macos") { 19 } else { 18 };

#[test]
fn two_stations_find_each_other_and_one_deck_controls_both() {
    let (a, b) = Station::pair();
    let fleet = a.call("fleet.view", json!({}));
    let names: Vec<&str> = fleet["stations"].as_array().unwrap().iter().map(|s| s["station"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["alpha", "bravo"]);

    // From alpha's Control Deck: start a pane on bravo and drive it.
    let pane = a.call("peer.call", json!({"station": "bravo", "method": "pane.spawn", "params": {"program": "/bin/sh", "name": "remote-sh", "cwd": b.root.join("home")}}));
    let id = pane["id"].as_str().unwrap();
    assert_eq!(pane["station"], json!("bravo"));
    a.call("peer.call", json!({"station": "bravo", "method": "pane.send_text", "params": {"pane": id, "text": "echo from-alpha-$((1+1))"}}));
    b.wait_text(id, "from-alpha-2");
    // The ship's log on bravo says who did it.
    assert!(b.log("pane.created").iter().any(|e| e["pane"] == json!(id)));

    // An agent on alpha cannot use the gateway to reach bravo.
    let agent = a.fake(json!({"steps": []}), json!({}));
    a.wait_text(&agent, "[fake] session");
    let err = a.call_as_pane(&agent, "peer.call", json!({"station": "bravo", "method": "pane.list", "params": {}})).unwrap_err();
    assert_eq!(err.code, "denied");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pane_on_another_station_is_attached_through_the_gateway() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (a, b) = tokio::task::spawn_blocking(Station::pair).await.unwrap();
    let pane = tokio::task::block_in_place(|| b.call("pane.spawn", json!({"program": "/bin/sh", "cwd": b.root.join("home")})))["id"].as_str().unwrap().to_string();

    // The browser talks only to alpha.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/api/ws", a.port)).await.unwrap();
    ws.send(Message::Text(json!({"auth": {"token": a.token()}}).to_string().into())).await.unwrap();
    ws.next().await.unwrap().unwrap();
    ws.send(Message::Text(json!({"id": 1, "method": "pane.attach", "params": {"pane": pane, "station": "bravo"}}).to_string().into())).await.unwrap();
    let attached: Value = loop {
        if let Ok(t) = ws.next().await.unwrap().unwrap().to_text() {
            let v: Value = serde_json::from_str(t).unwrap_or(Value::Null);
            if v["id"] == json!(1) {
                break v;
            }
        }
    };
    assert_eq!(attached["result"]["station"], json!("bravo"), "{attached}");
    let chan = attached["result"]["chan"].as_u64().unwrap();
    ws.send(Message::Text(json!({"id": null, "method": "pane.input", "params": {"chan": chan, "data": "echo via-gateway-$((3*3))\r"}}).to_string().into())).await.unwrap();
    let mut seen = Vec::new();
    let ok = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(Ok(Message::Binary(bytes))) = ws.next().await {
                seen.extend_from_slice(&bytes[5..]);
                if String::from_utf8_lossy(&seen).contains("via-gateway-9") {
                    return true;
                }
            }
        }
    })
    .await;
    assert_eq!(ok, Ok(true));
    drop(tokio::task::spawn_blocking(move || drop((a, b))).await);
}

fn stop_row(s: &Station, station: &str) -> Value {
    s.call("stop.list", json!({})).as_array().unwrap().iter().find(|r| r["station"] == json!(station)).cloned().unwrap_or(Value::Null)
}

/// Done when: a stop sent to an unreachable station shows as unreachable, then
/// is delivered on reconnect, and applies only to work running when issued.
#[test]
fn a_stop_for_an_unreachable_station_is_shown_honestly_and_delivered_on_reconnect() {
    let (a, b) = Station::pair();
    let long_job = json!({"steps": [{"prompt": "long job"}, {"work": 120000}]});
    let on_a = a.fake(long_job.clone(), json!({}));
    let on_b = b.fake(long_job.clone(), json!({}));
    a.wait_state(&on_a, "working");
    b.wait_state(&on_b, "working");

    // Bravo drops off the network. Its work keeps running.
    b.signal(SIGSTOP);
    let out = a.call("stop.all", json!({"scope": "fleet"}));
    assert_eq!(out["stops"].as_array().unwrap().len(), 2);
    a.wait_text(&on_a, "[fake] interrupted");
    assert_eq!(stop_row(&a, "alpha")["status"], json!("confirmed"));
    let row = stop_row(&a, "bravo");
    assert_eq!(row["status"], json!("unreachable"), "{row}");
    assert_eq!(row["label"], json!("station unreachable"), "never shown as stopped");
    // Later the fleet view marks it unreachable too, and claims nothing about its agents.
    let bravo = wait_for("bravo to show as unreachable", 20, || {
        let v = a.call("fleet.refresh", json!({}));
        v["stations"].as_array().unwrap().iter().find(|s| s["station"] == json!("bravo") && s["state"] == json!("unreachable")).cloned()
    });
    assert!(bravo["working"].is_null());

    // Bravo comes back. Work that started after the stop was issued is left alone.
    b.signal(SIGCONT);
    let later = b.fake(long_job, json!({}));
    b.wait_state(&later, "working");
    wait_for("the queued stop to be delivered", 30, || {
        a.call("fleet.refresh", json!({}));
        (stop_row(&a, "bravo")["status"] == json!("confirmed")).then_some(())
    });
    b.wait_text(&on_b, "[fake] interrupted");
    assert_eq!(stop_row(&a, "bravo")["result"]["stopped"], json!([on_b]));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(b.pane(&later)["state"], json!("working"), "only work running when the stop was issued is stopped");
    assert!(!b.text(&later).contains("interrupted"));
}

/// A tiny webhook sink: counts the POSTs it receives.
fn webhook() -> (String, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let _ = stream.read(&mut buf);
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        }
    });
    (url, hits)
}

fn stuck_script() -> Value {
    json!({"steps": [
        {"prompt": "go"}, {"sleep": 2500},
        {"repeat": 3, "steps": [{"tool": {"name": "Bash", "command": "make", "ok": false}}]},
        {"sleep": 60000}
    ]})
}

/// Done when: no push arrives while you are at a keyboard.
#[test]
fn no_push_while_you_are_at_a_keyboard_but_the_inbox_still_gets_it() {
    let (url, hits) = webhook();
    let s = Station::start_with("alpha", &format!("at_keyboard_secs = 2\n[notify]\nwebhook_url = \"{url}\"\n"), &[]);
    let pane = s.fake(stuck_script(), json!({}));
    // You are working at a full client the whole time.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::SeqCst) {
                let _ = s.try_call("presence.touch", json!({}));
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        s.wait_state(&pane, "needs_you");
        let note = wait_for("the notification to be decided", 10, || {
            s.call("notify.list", json!({})).as_array().unwrap().iter().find(|n| n["kind"] == json!("needs_you") && n["pushed"] != json!(0)).cloned()
        });
        assert_eq!(note["pushed"], json!(2), "held back: you were at a keyboard");
        stop.store(true, Ordering::SeqCst);
    });
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing was pushed");
}

#[test]
fn a_push_is_sent_when_you_are_away_and_muted_panes_stay_quiet() {
    let (url, hits) = webhook();
    let s = Station::start_with("alpha", &format!("at_keyboard_secs = 1\n[notify]\nwebhook_url = \"{url}\"\n"), &[]);
    let muted = s.fake(stuck_script(), json!({}));
    s.call("pane.set", json!({"pane": muted, "muted": true}));
    let loud = s.fake(stuck_script(), json!({}));
    // Nobody touches the station while the agents get stuck.
    wait_for("the push", 20, || (hits.load(Ordering::SeqCst) >= 1).then_some(()));
    s.wait_state(&muted, "needs_you");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(hits.load(Ordering::SeqCst), 1, "one push: the muted pane sent none");
    let notes = s.call("notify.list", json!({}));
    let notes = notes.as_array().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["pane"], json!(loud));
    assert_eq!(notes[0]["pushed"], json!(1));
}

/// The phone checklist: status, blockers, an approval and a stop all work,
/// and attaching from the phone is read-only.
#[test]
fn the_phone_view_shows_blockers_answers_an_approval_and_stops_an_agent() {
    use common::browser::Page;
    let s = Station::start("alpha");
    let asking = s.fake(json!({"steps": [{"prompt": "x"}, {"permission": {"name": "Bash", "command": "rm -rf build"}}, {"turn_end": "ok"}]}), json!({"name": "asker"}));
    let busy = s.fake(json!({"steps": [{"prompt": "x"}, {"work": 120000}]}), json!({"name": "worker"}));
    s.wait_state(&asking, "needs_you");
    s.wait_state(&busy, "working");

    let page = Page::open(&s, 390, 800);
    page.wait("the phone view", "!!document.querySelector('main.phone .approval')");
    let text = page.js("document.querySelector('main.phone').innerText").as_str().unwrap().to_string();
    assert!(text.contains("Needs you") && text.contains("asker") && text.contains("rm -rf build"), "{text}");
    assert!(page.js("!document.querySelector('.sidebar') && !document.querySelector('.workspace')").as_bool().unwrap(), "no full client on a phone");

    // Approve from the phone.
    page.js("[...document.querySelectorAll('.approval button')].find(b => b.innerText === 'Allow').click()");
    s.wait_text(&asking, "[fake] permission: allow");

    // Open the busy agent: watching only, with a stop control.
    page.js("[...document.querySelectorAll('.phone-row')].find(r => r.innerText.includes('worker')).click()");
    page.wait("the pane on the phone", "!!document.querySelector('.phone-pane .xterm-rows')");
    assert!(page.js("document.querySelector('.phone-pane').innerText").as_str().unwrap().contains("Watching only"));
    page.wait("terminal", "document.querySelector('.xterm-helper-textarea') && (document.querySelector('.xterm-helper-textarea').focus(), true)");
    page.tab.type_str("typed-from-phone").unwrap();
    page.js("[...document.querySelectorAll('.phone-pane button')].find(b => b.innerText === 'Stop this turn').click()");
    s.wait_text(&busy, "[fake] interrupted");
    assert!(!s.text(&busy).contains("typed-from-phone"), "a watching client cannot type");
    let size = s.pane(&busy);
    assert_eq!((size["cols"].as_u64(), size["controller"].is_null()), (Some(120), true), "watching did not resize the pane");
}

/// Live: Tailscale identity on this machine. A connection that arrives on the
/// tailnet address is identified by whois, with no token.
/// Run with `cargo test -- --ignored live_tailscale`.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_tailscale_identity_trusts_the_same_login_without_a_token() {
    let status: Value = serde_json::from_slice(&std::process::Command::new("tailscale").args(["status", "--json"]).output().unwrap().stdout).unwrap();
    let ip = status["Self"]["TailscaleIPs"][0].as_str().expect("Tailscale is running").to_string();
    let port = free_port();
    let ip2 = ip.clone();
    let s = tokio::task::spawn_blocking(move || {
        let s = Station::start_on("live", port, "", &[]);
        let cfg = s.root.join("config/config.toml");
        let text = std::fs::read_to_string(&cfg).unwrap().replace("web_bind = [\"127.0.0.1\"]", &format!("web_bind = [\"127.0.0.1\", \"{ip2}\"]"));
        std::fs::write(&cfg, text).unwrap();
        s
    })
    .await
    .unwrap();
    let mut s = s;
    tokio::task::block_in_place(|| {
        s.kill9();
        s.launch();
    });
    let client = reqwest::Client::new();
    // Over the tailnet address: no token, identified as the owner by whois.
    let r = client.post(format!("http://{ip}:{port}/api/rpc")).json(&json!({"method": "station.doctor", "params": {}})).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.unwrap();
    let ts = body["result"]["checks"].as_array().unwrap().iter().find(|c| c["name"] == json!("tailscale")).unwrap().clone();
    assert_eq!(ts["ok"], json!(true), "{ts}");
    // The same station on loopback still needs a token.
    let r = client.post(format!("http://127.0.0.1:{port}/api/rpc")).json(&json!({"method": "station.info", "params": {}})).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 401);
    // And the tailnet address answers station-to-station calls from this login.
    let r = client.post(format!("http://{ip}:{port}/api/peer/rpc")).json(&json!({"method": "station.info", "from": "probe"})).send().await.unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["result"]["station"], json!("live"));
    drop(tokio::task::spawn_blocking(move || drop(s)).await);
}
