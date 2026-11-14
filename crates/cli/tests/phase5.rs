//! Phase 5 acceptance: the fleet.
//! Done when: two daemons discover and trust each other with no setup; a stop
//! sent to an unreachable station shows as unreachable, then is delivered on
//! reconnect; no push arrives while you are at a keyboard.
//! 0.2: a station joins on the owner's approval; a pane can be driven from a phone.
mod common;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

const SIGSTOP: i32 = if cfg!(target_os = "macos") { 17 } else { 19 };
const SIGCONT: i32 = if cfg!(target_os = "macos") { 19 } else { 18 };

/// 0.2 step 1, done when: a station without the fleet key is refused by every
/// station call; two stations join with one approval.
#[test]
fn a_station_joins_the_fleet_only_when_the_owner_approves_it_there() {
    let (a, b) = Station::strangers(None);
    let state_of = |s: &Station, name: &str| {
        s.call("fleet.refresh", json!({}))["stations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["station"] == json!(name))
            .map(|x| x["state"].clone())
    };
    assert_eq!(state_of(&a, "bravo"), Some(json!("not_linked")));
    // Seeing a station is not controlling it.
    let err = a
        .try_call(
            "peer.call",
            json!({"station": "bravo", "method": "pane.list", "params": {}}),
        )
        .unwrap_err();
    assert!(err.message.contains("not in this fleet"), "{}", err.message);
    assert!(b
        .call("pane.list", json!({}))
        .as_array()
        .unwrap()
        .is_empty());

    // bravo asks to join alpha's fleet. Both sides show the same code.
    let asked = b.call("fleet.join", json!({"station": "alpha"}));
    let waiting = a.call("trust.status", json!({}));
    let req = &waiting["requests"][0];
    assert_eq!(
        (
            req["kind"].clone(),
            req["name"].clone(),
            req["code"].clone()
        ),
        (json!("station"), json!("bravo"), asked["code"].clone())
    );
    // Until alpha's owner says yes, nothing changes.
    std::thread::sleep(Duration::from_millis(1800));
    assert_eq!(state_of(&b, "alpha"), Some(json!("not_linked")));

    a.call("trust.approve", json!({"request": asked["code"]}));
    wait_for("bravo to take the fleet key", 15, || {
        (state_of(&b, "alpha") == Some(json!("online"))).then_some(())
    });
    assert_eq!(
        a.call("trust.status", json!({}))["fleet"],
        b.call("trust.status", json!({}))["fleet"]
    );
    wait_for("alpha to see bravo in the fleet", 15, || {
        (state_of(&a, "bravo") == Some(json!("online"))).then_some(())
    });
    let panes = a.call(
        "peer.call",
        json!({"station": "bravo", "method": "pane.list", "params": {}}),
    );
    assert!(panes.is_array());
    // The key itself is never shown.
    let key = std::fs::read_to_string(a.root.join("state/fleet.key")).unwrap();
    for s in [&a, &b] {
        let said = format!(
            "{}{}",
            s.call("trust.status", json!({})),
            s.call("log.query", json!({"limit": 500}))
        );
        assert!(!said.contains(key.trim()));
    }
}

/// A refused request hands over nothing.
#[test]
fn a_refused_station_stays_out() {
    let (a, b) = Station::strangers(None);
    let asked = b.call("fleet.join", json!({"station": "alpha"}));
    a.call("trust.deny", json!({"request": asked["code"]}));
    wait_for("bravo to hear the refusal", 15, || {
        b.call("trust.status", json!({}))["outgoing"]["problem"]
            .as_str()
            .map(String::from)
    });
    assert_ne!(
        a.call("trust.status", json!({}))["fleet"],
        b.call("trust.status", json!({}))["fleet"]
    );
}

/// 0.2: one Control Deck looks after the other stations: restart one, and
/// start one whose daemon is not running, if this station is allowed to.
#[test]
fn another_station_is_restarted_and_a_stopped_one_is_started_from_the_deck() {
    // The "SSH" here is a script that starts bravo's daemon on this machine.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("start-bravo.sh");
    let (mut a, mut b) = Station::pair_with(&format!(
        "remote_start_cmd = [\"{}\", \"{{host}}\"]\nwake_broadcast = \"127.0.0.1\"\n",
        script.display()
    ));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$1\" > {dir}/asked\nSPANDECK_STATE_DIR={root}/state SPANDECK_CONFIG_DIR={root}/config SPANDECK_FAKE_DIR={root}/fake HOME={root}/home SPANDECK_LOG=warn nohup {SPANDECK} daemon >/dev/null 2>&1 &\n",
            dir = dir.path().display(),
            root = b.root.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bravo_is = |a: &Station, want: &str| {
        a.call("fleet.refresh", json!({}))["stations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["station"] == json!("bravo") && s["state"] == json!(want))
    };

    // Restart bravo from alpha: its terminal survives.
    let pane = b.fake(
        json!({"steps": [{"say": "before-restart marker"}]}),
        json!({}),
    );
    b.wait_text(&pane, "before-restart marker");
    let before = b.call("station.info", json!({}));
    a.call(
        "peer.call",
        json!({"station": "bravo", "method": "station.restart", "params": {}}),
    );
    wait_for("bravo to come back", 15, || {
        let i = b.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(())
    });
    assert!(b.text(&pane).contains("before-restart marker"));
    assert!(b.log("station.upgrading")[0]["actor"]
        .as_str()
        .unwrap()
        .contains("via alpha"));
    // Pointing another station at an arbitrary binary is still refused.
    let err = a
        .try_call("peer.call", json!({"station": "bravo", "method": "station.upgrade", "params": {"binary": "/bin/sh"}}))
        .unwrap_err();
    assert!(
        err.message.contains("cannot be forwarded"),
        "{}",
        err.message
    );
    wait_for("alpha to see bravo again", 20, || {
        bravo_is(&a, "online").then_some(())
    });

    // bravo's daemon dies. alpha still lists it, as unreachable.
    b.kill9();
    wait_for("alpha to see bravo is gone", 20, || {
        bravo_is(&a, "unreachable").then_some(())
    });
    // Starting it is refused until this station is allowed to.
    let err = a
        .try_call("fleet.start", json!({"station": "bravo"}))
        .unwrap_err();
    assert_eq!(err.code, "denied");
    assert!(!dir.path().join("asked").exists());
    // Waking sends packets to the address learned while it was online, if it has one.
    match a.try_call("fleet.wake", json!({"station": "bravo"})) {
        Ok(v) => assert!(v["sent"].as_u64().unwrap() > 0, "{v}"),
        Err(e) => assert!(e.message.contains("not known yet"), "{}", e.message),
    }

    // The owner turns remote start on for alpha. After alpha restarts it still
    // knows bravo, though bravo has been down the whole time.
    let cfg = a.root.join("config/config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, format!("remote_start = true\n{text}")).unwrap();
    a.kill9();
    a.launch();
    assert!(
        bravo_is(&a, "unreachable"),
        "a station that is off stays listed"
    );
    a.call("fleet.start", json!({"station": "bravo"}));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("asked"))
            .unwrap()
            .trim(),
        "127.0.0.1",
        "the command was given bravo's address"
    );
    wait_for("bravo to be online again", 20, || {
        bravo_is(&a, "online").then_some(())
    });
    // The daemon the script started is not the harness's child: stop it by its pid.
    let pid: i32 = std::fs::read_to_string(b.root.join("state/spandeckd.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    kill_pid(pid, 15);
}

#[test]
fn two_stations_find_each_other_and_one_deck_controls_both() {
    let (a, b) = Station::pair();
    let fleet = a.call("fleet.view", json!({}));
    let names: Vec<&str> = fleet["stations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["station"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["alpha", "bravo"]);

    // From alpha's Control Deck: start a pane on bravo and drive it.
    let pane = a.call("peer.call", json!({"station": "bravo", "method": "pane.spawn", "params": {"program": "/bin/sh", "name": "remote-sh", "cwd": b.root.join("home")}}));
    let id = pane["id"].as_str().unwrap();
    assert_eq!(pane["station"], json!("bravo"));
    a.call("peer.call", json!({"station": "bravo", "method": "pane.send_text", "params": {"pane": id, "text": "echo from-alpha-$((1+1))"}}));
    b.wait_text(id, "from-alpha-2");
    // The ship's log on bravo says who did it.
    assert!(b.log("pane.created").iter().any(|e| e["pane"] == json!(id)));
    // Features 4.10: who typed into a pane is logged, once, without the text.
    a.call("peer.call", json!({"station": "bravo", "method": "pane.send_text", "params": {"pane": id, "text": "echo again"}}));
    let typed = b.log("pane.typed");
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert_eq!(typed[0]["pane"], json!(id));
    assert!(
        typed[0]["actor"].as_str().unwrap().contains("via alpha"),
        "{typed:?}"
    );
    assert!(!typed[0].to_string().contains("from-alpha"));

    // An agent on alpha cannot use the gateway to reach bravo.
    let agent = a.fake(json!({"steps": []}), json!({}));
    a.wait_text(&agent, "[fake] session");
    let err = a
        .call_as_pane(
            &agent,
            "peer.call",
            json!({"station": "bravo", "method": "pane.list", "params": {}}),
        )
        .unwrap_err();
    assert_eq!(err.code, "denied");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pane_on_another_station_is_attached_through_the_gateway() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (a, b) = tokio::task::spawn_blocking(Station::pair).await.unwrap();
    let pane = tokio::task::block_in_place(|| {
        b.call(
            "pane.spawn",
            json!({"program": "/bin/sh", "cwd": b.root.join("home")}),
        )
    })["id"]
        .as_str()
        .unwrap()
        .to_string();

    // The browser talks only to alpha.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/api/ws", a.port))
        .await
        .unwrap();
    ws.send(Message::Text(
        json!({"auth": {"token": a.token()}}).to_string().into(),
    ))
    .await
    .unwrap();
    ws.next().await.unwrap().unwrap();
    ws.send(Message::Text(
        json!({"id": 1, "method": "pane.attach", "params": {"pane": pane, "station": "bravo"}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
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
    // Features 4.10: bravo's log says who attached, and through which station.
    let seen = tokio::task::block_in_place(|| b.log("pane.attached"));
    assert!(
        seen.iter()
            .any(|e| e["actor"].as_str().is_some_and(|a| a.contains("via alpha"))),
        "{seen:?}"
    );
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
    s.call("stop.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["station"] == json!(station))
        .cloned()
        .unwrap_or(Value::Null)
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
    assert_eq!(
        row["label"],
        json!("station unreachable"),
        "never shown as stopped"
    );
    // Later the fleet view marks it unreachable too, and claims nothing about its agents.
    let bravo = wait_for("bravo to show as unreachable", 20, || {
        let v = a.call("fleet.refresh", json!({}));
        v["stations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["station"] == json!("bravo") && s["state"] == json!("unreachable"))
            .cloned()
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
    assert_eq!(
        b.pane(&later)["state"],
        json!("working"),
        "only work running when the stop was issued is stopped"
    );
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
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
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
    let s = Station::start_with(
        "alpha",
        &format!("at_keyboard_secs = 2\n[notify]\nwebhook_url = \"{url}\"\n"),
        &[],
    );
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
            s.call("notify.list", json!({}))
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["kind"] == json!("needs_you") && n["pushed"] != json!(0))
                .cloned()
        });
        assert_eq!(
            note["pushed"],
            json!(2),
            "held back: you were at a keyboard"
        );
        stop.store(true, Ordering::SeqCst);
    });
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing was pushed");
}

#[test]
fn a_push_is_sent_when_you_are_away_and_muted_panes_stay_quiet() {
    let (url, hits) = webhook();
    let s = Station::start_with(
        "alpha",
        &format!("at_keyboard_secs = 1\n[notify]\nwebhook_url = \"{url}\"\n"),
        &[],
    );
    let muted = s.fake(stuck_script(), json!({}));
    s.call("pane.set", json!({"pane": muted, "muted": true}));
    let loud = s.fake(stuck_script(), json!({}));
    // Nobody touches the station while the agents get stuck.
    wait_for("the push", 20, || {
        (hits.load(Ordering::SeqCst) >= 1).then_some(())
    });
    s.wait_state(&muted, "needs_you");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "one push: the muted pane sent none"
    );
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
    let busy = s.fake(
        json!({"steps": [{"prompt": "x"}, {"work": 120000}]}),
        json!({"name": "worker"}),
    );
    s.wait_state(&asking, "needs_you");
    s.wait_state(&busy, "working");

    let page = Page::open(&s, 390, 800);
    page.wait(
        "the phone view",
        "!!document.querySelector('main.phone .approval')",
    );
    let text = page
        .js("document.querySelector('main.phone').innerText")
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text.contains("Needs you") && text.contains("asker") && text.contains("rm -rf build"),
        "{text}"
    );
    assert!(
        page.js("!document.querySelector('.sidebar') && !document.querySelector('.workspace')")
            .as_bool()
            .unwrap(),
        "no full client on a phone"
    );

    // Approve from the phone.
    page.js("[...document.querySelectorAll('.approval button')].find(b => b.innerText === 'Allow').click()");
    s.wait_text(&asking, "[fake] permission: allow");

    // Open the busy agent: the terminal, and a stop control.
    let open = |name: &str| {
        page.goto("#/fleet");
        // The list is redrawn every few seconds: wait for this pane's row.
        page.wait(
            "the pane's row in the list",
            &format!("(r => r ? (r.click(), true) : false)([...document.querySelectorAll('main.phone .phone-row')].find(r => r.innerText.includes('{name}')))"),
        );
        page.wait(
            "the pane on the phone",
            "!!document.querySelector('.phone-pane .xterm-rows') && !!document.querySelector('.phone-pane .composer-box')",
        );
    };
    open("worker");
    page.js("[...document.querySelectorAll('.phone-pane button')].find(b => b.innerText === 'Stop this turn').click()");
    s.wait_text(&busy, "[fake] interrupted");
    let size = s.pane(&busy);
    assert_eq!(
        (size["cols"].as_u64(), size["controller"].is_null()),
        (Some(120), true),
        "the phone did not resize the pane"
    );

    // 0.2 step 2: talk to an agent from the phone. Dictation fills the same
    // box, so what is tested here is the box: it survives a redraw, and a
    // message is sent as one line.
    let talker = s.fake(json!({"steps": []}), json!({"name": "talker"}));
    s.wait_text(&talker, "[fake] session");
    open("talker");
    let set = |text: &str| {
        page.js(&format!(
            "(document.querySelector('.composer-box').value = {}, true)",
            json!(text)
        ));
    };
    set("hello from\nmy phone");
    // A bug that happened on a real phone: the redraw every few seconds took
    // the keyboard away from the message box.
    page.js("(document.querySelector('.composer-box').focus(), window.__box = document.querySelector('.composer-box'), true)");
    std::thread::sleep(Duration::from_millis(3200));
    assert_eq!(
        page.js("document.activeElement === window.__box && document.contains(window.__box)"),
        json!(true),
        "the message box keeps the keyboard through a redraw"
    );
    assert_eq!(
        page.js("document.querySelector('.composer-box').value"),
        json!("hello from\nmy phone"),
        "a redraw keeps what you were writing"
    );
    page.js("document.querySelector('.composer-row button[type=submit]').click()");
    s.wait_text(&talker, "[fake] got: hello from my phone");
    page.wait(
        "the box to empty",
        "document.querySelector('.composer-box').value === ''",
    );

    // A shell on the phone: run a command, interrupt it with the Ctrl-C key.
    let sh = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "phone-sh", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    open("phone-sh");
    set("sleep 300");
    page.js("document.querySelector('.composer-row button[type=submit]').click()");
    page.wait(
        "the box to empty",
        "document.querySelector('.composer-box').value === ''",
    );
    std::thread::sleep(Duration::from_millis(400));
    page.js(
        "[...document.querySelectorAll('.composer .key')].find(b => b.innerText === '^C').click()",
    );
    set("echo back-$((40+2))");
    page.js("document.querySelector('.composer-row button[type=submit]').click()");
    s.wait_text(&sh, "back-42");

    // A bug that happened on a real phone: tapping the terminal brought up the
    // keyboard, and typing did nothing. Keys typed into the terminal arrive,
    // and the pane keeps its size.
    open("phone-sh");
    page.wait("terminal", "document.querySelector('.phone-pane .xterm-helper-textarea') && (document.querySelector('.phone-pane .xterm-helper-textarea').focus(), true)");
    page.tab.type_str("echo typed-$((20+3))").unwrap();
    page.tab.press_key("Enter").unwrap();
    s.wait_text(&sh, "typed-23");
    assert!(
        s.pane(&sh)["controller"].is_null(),
        "typing did not take the pane's size"
    );

    // Fitting the pane to the phone resizes it, which watching never does.
    page.js("[...document.querySelectorAll('.phone-pane button')].find(b => b.innerText === 'Fit to screen').click()");
    wait_for("the pane to be resized to the phone", 10, || {
        let p = s.pane(&sh);
        (p["cols"].as_u64().is_some_and(|c| c < 80) && !p["controller"].is_null()).then_some(())
    });

    // (Touches are built from plain events: a desktop browser with no touch
    // screen, as on the macOS CI machine, has no Touch constructor.)
    // A bug that happened on a real phone: a finger could not scroll the
    // terminal's history. Dragging down shows earlier output.
    set("seq 1000 1300");
    page.js("document.querySelector('.composer-row button[type=submit]').click()");
    s.wait_text(&sh, "1300");
    let rows = "document.querySelector('.phone-pane .xterm-rows').innerText";
    page.wait("the end of the output", &format!("{rows}.includes('1300')"));
    page.js(
        "(() => { // The finger lands on the terminal's own element, as on a real phone.
           const el = document.querySelector('.phone-pane .xterm-screen');
           const touch = (type, y) => { const ev = new Event(type, {bubbles: true, cancelable: true}); Object.defineProperty(ev, 'touches', {value: y === null ? [] : [{clientX: 100, clientY: y}]}); return ev; };
           el.dispatchEvent(touch('touchstart', 200));
           for (let y = 220; y <= 700; y += 20) el.dispatchEvent(touch('touchmove', y));
           el.dispatchEvent(touch('touchend', null));
           return true; })()",
    );
    page.wait(
        "earlier output after dragging down",
        &format!("!{rows}.includes('1300') && /12[0-9][0-9]/.test({rows})"),
    );

    // Found from a real phone's drag reports: Claude Code runs full-screen and
    // listens for the mouse, so it has to be told to scroll. A drag over such a
    // program sends it wheel turns.
    set("printf '\\033[?1049h\\033[?1003h\\033[?1006h'; cat -v");
    page.js("document.querySelector('.composer-row button[type=submit]').click()");
    std::thread::sleep(Duration::from_millis(900));
    page.js(
        "(() => { const el = document.querySelector('.phone-pane .xterm-screen');
           const touch = (type, y) => { const ev = new Event(type, {bubbles: true, cancelable: true}); Object.defineProperty(ev, 'touches', {value: y === null ? [] : [{clientX: 100, clientY: y}]}); return ev; };
           el.dispatchEvent(touch('touchstart', 200));
           for (let y = 220; y <= 420; y += 20) el.dispatchEvent(touch('touchmove', y));
           el.dispatchEvent(touch('touchend', null));
           return true; })()",
    );
    s.wait_text(&sh, "^[[<64;");

    // And start something new from the phone.
    page.goto("#/fleet");
    page.wait(
        "the list",
        "!!document.querySelector('main.phone .phone-station')",
    );
    page.js("[...document.querySelectorAll('.phone-station button')].find(b => b.innerText === 'New').click()");
    page.wait(
        "the new pane dialog",
        "!!document.querySelector('dialog select, .dialog select')",
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_tailscale_a_local_program_needs_a_token_and_stations_need_the_fleet_key() {
    let status: Value = serde_json::from_slice(
        &std::process::Command::new("tailscale")
            .args(["status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let ip = status["Self"]["TailscaleIPs"][0]
        .as_str()
        .expect("Tailscale is running")
        .to_string();
    let port = free_port();
    let ip2 = ip.clone();
    let s = tokio::task::spawn_blocking(move || {
        let s = Station::start_on("live", port, "", &[]);
        let cfg = s.root.join("config/config.toml");
        let text = std::fs::read_to_string(&cfg).unwrap().replace(
            "web_bind = [\"127.0.0.1\"]",
            &format!("web_bind = [\"127.0.0.1\", \"{ip2}\"]"),
        );
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
    let token = s.token();
    let rpc = |host: String, token: Option<String>, method: &'static str| {
        let client = client.clone();
        async move {
            let mut req = client
                .post(format!("http://{host}/api/rpc"))
                .json(&json!({"method": method, "params": {}}));
            if let Some(t) = token {
                req = req.bearer_auth(t);
            }
            let r = req.send().await.unwrap();
            (
                r.status().as_u16(),
                r.json::<Value>().await.unwrap_or(Value::Null),
            )
        }
    };
    // Tailscale says which device is calling, not which program. A program on
    // this machine calling the station's own Tailscale address gets nothing
    // without a token, exactly as on localhost.
    assert_eq!(
        rpc(format!("{ip}:{port}"), None, "station.info").await.0,
        401
    );
    assert_eq!(
        rpc(format!("127.0.0.1:{port}"), None, "station.info")
            .await
            .0,
        401
    );
    // With the token it is the owner, and the station sees Tailscale.
    let (status, body) = rpc(format!("{ip}:{port}"), Some(token), "station.doctor").await;
    assert_eq!(status, 200, "{body}");
    let ts = body["result"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == json!("tailscale"))
        .unwrap()
        .clone();
    assert_eq!(ts["ok"], json!(true), "{ts}");
    // The tailnet address says who it is to a device under this login, and
    // refuses station calls that are not signed with the fleet key.
    let peer = |method: &'static str| {
        let client = client.clone();
        let url = format!("http://{ip}:{port}/api/peer/rpc");
        async move {
            let r = client
                .post(url)
                .json(&json!({"method": method, "from": "probe"}))
                .send()
                .await
                .unwrap();
            (
                r.status().as_u16(),
                r.json::<Value>().await.unwrap_or(Value::Null),
            )
        }
    };
    assert_eq!(
        peer("station.hello").await.1["result"]["station"],
        json!("live")
    );
    assert_eq!(peer("station.info").await.0, 401);
    drop(tokio::task::spawn_blocking(move || drop(s)).await);
}

/// The phone's layout map (agreed prototype, screen 4): a project layout is
/// stepped through one pane at a time, from a small map of it.
#[test]
fn a_phone_steps_through_a_layout_from_its_map() {
    use common::browser::Page;
    let s = Station::start("alpha");
    let shell = |n: &str| json!({"name": n, "agent": "shell"});
    s.call(
        "project.new",
        json!({"path": "~/proj", "panes": [shell("dev"), shell("dev2"), shell("lead")], "open": true}),
    );
    let pane = |n: &str| json!({"pane": n});
    s.call(
        "project.layout.save",
        json!({"project": "proj", "layout": {"name": "build", "tree": {"dir": "col", "ratio": 600,
            "a": {"dir": "row", "ratio": 500, "a": pane("dev"), "b": pane("dev2")}, "b": pane("lead")}}}),
    );

    let page = Page::open(&s, 390, 844);
    page.wait(
        "the layout's row under its station",
        "(r => r ? (r.click(), true) : false)(document.querySelector('main.phone .phone-station .lay-row'))",
    );
    page.wait(
        "the map",
        "document.querySelectorAll('.lay-map button').length === 3 && !!document.querySelector('.phone-layout .composer-box')",
    );
    let boxes = page.js("[...document.querySelectorAll('.lay-map button')].map(b => { const r = b.getBoundingClientRect(); return { label: b.getAttribute('aria-label'), now: b.getAttribute('aria-current'), x: Math.round(r.left), y: Math.round(r.top), w: r.width, h: r.height }; })");
    let labels: Vec<&str> = boxes
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["dev, idle", "dev2, idle", "lead, idle"]);
    assert_eq!(boxes[0]["now"], json!("true"), "{boxes}");
    assert!(boxes[1]["now"].is_null(), "{boxes}");
    // The map is the layout's shape: dev beside dev2, lead under both.
    assert!(
        boxes[0]["x"].as_i64() < boxes[1]["x"].as_i64()
            && boxes[0]["y"] == boxes[1]["y"]
            && boxes[2]["y"].as_i64() > boxes[0]["y"].as_i64(),
        "{boxes}"
    );
    assert!(
        boxes
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["w"].as_f64() >= Some(44.0) && b["h"].as_f64() >= Some(44.0)),
        "every box is big enough for a finger: {boxes}"
    );
    let showing = "document.querySelector('.lay-showing').innerText";
    assert_eq!(page.js(showing), json!("Showing dev · 1 of 3"));

    // Tap the second box: that pane is shown, and the message box is its.
    page.js("document.querySelectorAll('.lay-map button')[1].click()");
    page.wait(
        "the second pane",
        &format!(
            "{showing} === 'Showing dev2 · 2 of 3' && document.querySelector('.phone-layout .pane')?.dataset.pane === {} && !!document.querySelector('.phone-layout .xterm-rows')",
            s.pane("dev2")["id"]
        ),
    );
    assert_eq!(
        page.js("document.querySelectorAll('.lay-map button')[1].getAttribute('aria-current')"),
        json!("true")
    );
    page.js("(document.querySelector('.composer-box').value = 'echo map-$((40+2))', true)");
    // The bug from a real phone, on this screen too: the redraw every few
    // seconds must not take the keyboard away from the message box.
    page.js("(document.querySelector('.composer-box').focus(), window.__box = document.querySelector('.composer-box'), true)");
    std::thread::sleep(Duration::from_millis(3200));
    assert_eq!(
        page.js("document.activeElement === window.__box && document.contains(window.__box)"),
        json!(true),
        "the message box keeps the keyboard through a redraw"
    );
    page.js("document.querySelector('.composer-row button[type=submit]').click()");
    s.wait_text("dev2", "map-42");
    assert!(!s.text("dev").contains("map-42"), "{}", s.text("dev"));

    // A swipe to the left over the terminal goes on to the next pane.
    page.js(
        "(() => { const el = document.querySelector('.phone-layout .xterm-screen');
           const touch = (type, x) => { const ev = new Event(type, {bubbles: true, cancelable: true}); Object.defineProperty(ev, 'touches', {value: x === null ? [] : [{clientX: x, clientY: 420}]}); return ev; };
           el.dispatchEvent(touch('touchstart', 300));
           for (let x = 280; x >= 120; x -= 20) el.dispatchEvent(touch('touchmove', x));
           el.dispatchEvent(touch('touchend', null));
           return true; })()",
    );
    page.wait(
        "the third pane after a swipe",
        &format!("{showing} === 'Showing lead · 3 of 3'"),
    );
}
