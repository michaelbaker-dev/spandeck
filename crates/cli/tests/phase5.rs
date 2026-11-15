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
#[cfg_attr(windows, ignore = "starts a stopped station with a Unix command line")]
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
    spandeck_terminal::sys::set_executable(&script).unwrap();
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
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
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
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
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

    // Mobile plan, phase M1: a pane's conversation, watched from a deck that
    // another station serves. The agent on bravo runs a tool, compacts, asks
    // for a permission and answers; the browser, which talks only to alpha,
    // is sent each item once, as it is written.
    let line = |v: Value| json!({"transcript": v});
    let said = |blocks: Value| json!({"type": "assistant", "message": {"role": "assistant", "content": blocks}});
    let worker = tokio::task::block_in_place(|| {
        b.fake(
            json!({"steps": [
                {"prompt": "count the otters"},
                line(said(json!([{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "wc -l otters.csv"}}]))),
                line(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "12 otters.csv"}]}, "toolUseResult": {"stdout": "12 otters.csv", "stderr": "", "interrupted": false}})),
                line(json!({"type": "system", "subtype": "compact_boundary", "compactMetadata": {"trigger": "manual", "preTokens": 5000, "postTokens": 400}})),
                line(json!({"type": "hologram-state", "note": "a record of a kind nobody knows"})),
                line(said(json!([{"type": "tool_use", "id": "toolu_2", "name": "Bash", "input": {"command": "rm otters.csv"}}]))),
                {"permission": {"name": "Bash", "command": "rm otters.csv"}},
                line(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_2", "content": ""}]}, "toolUseResult": {"stdout": "", "stderr": "", "interrupted": false}})),
                {"turn_end": "twelve otters, and the file is gone"}
            ]}),
            json!({"name": "counter"}),
        )
    });
    ws.send(Message::Text(
        json!({"id": 7, "method": "pane.conversation.watch", "params": {"pane": worker, "station": "bravo"}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    // What the view would hold: every item by id, in the order sent.
    let mut items: Vec<Value> = vec![];
    let mut live = Value::Null;
    let mut conv_chan = Value::Null;
    let mut not_understood = 0;
    macro_rules! watch_until {
        ($what:expr, $done:expr) => {
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    #[allow(clippy::redundant_closure_call)]
                    if $done(&items, &live) {
                        return;
                    }
                    let Some(Ok(Message::Text(t))) = ws.next().await else {
                        continue;
                    };
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    let page = if v["id"] == json!(7) {
                        assert!(v.get("error").is_none(), "{v}");
                        conv_chan = v["result"]["chan"].clone();
                        &v["result"]
                    } else if !conv_chan.is_null() && v["chan"] == conv_chan {
                        &v["conversation"]
                    } else {
                        continue;
                    };
                    if page["reset"] == json!(true) {
                        items.clear();
                    }
                    for item in page["items"].as_array().into_iter().flatten() {
                        assert!(
                            items.iter().all(|i| i["id"] != item["id"]),
                            "{} was sent twice",
                            item["id"]
                        );
                        items.push(item.clone());
                    }
                    for item in page["updated"].as_array().into_iter().flatten() {
                        let old = items.iter_mut().find(|i| i["id"] == item["id"]);
                        *old.expect("an update is for an item sent before") = item.clone();
                    }
                    live = page["live"].clone();
                    not_understood = page["not_understood"].as_u64().unwrap_or(not_understood);
                }
            })
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {}: {items:?} {live}", $what))
        };
    }
    // The request the hook holds, tied to the call it is about, once that
    // call has been sent.
    watch_until!("the approval", |items: &Vec<Value>, live: &Value| {
        let about = &live["approvals"][0]["item"];
        !about.is_null() && items.iter().any(|i| i["id"] == *about)
    });
    let shown: Vec<(&str, &str)> = items
        .iter()
        .map(|i| {
            (
                i["what"].as_str().or(i["kind"].as_str()).unwrap(),
                i["status"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        shown,
        vec![
            ("user", ""),
            ("tool", "ok"),
            ("compacted", ""),
            ("tool", "running")
        ]
    );
    assert_eq!(not_understood, 1, "the unknown record is counted");
    // The held approval points at the call it is about.
    let approval = live["approvals"][0].clone();
    assert_eq!(approval["item"], items[3]["id"], "{live}");
    assert_eq!(live["state"], json!("needs_you"));
    let via_alpha = |method: &str, params: Value| {
        tokio::task::block_in_place(|| {
            a.call(
                "peer.call",
                json!({"station": "bravo", "method": method, "params": params}),
            )
        })
    };
    // A call's detail on demand, through the gateway.
    let detail = via_alpha(
        "pane.conversation.detail",
        json!({"pane": worker, "item": items[1]["id"]}),
    );
    assert_eq!(detail["output"]["text"], json!("12 otters.csv"), "{detail}");

    // The owner allows it from alpha's deck: the call finishes and the reply
    // arrives, with nothing sent twice and nothing missed.
    via_alpha(
        "approval.answer",
        json!({"id": approval["approval"], "allow": true}),
    );
    watch_until!("the reply", |items: &Vec<Value>, live: &Value| items
        .iter()
        .any(|i| i["kind"] == json!("text"))
        && live["unread"] == json!(true));
    assert_eq!(items[3]["status"], json!("ok"));
    assert_eq!(
        items[4]["text"],
        json!("twelve otters, and the file is gone")
    );
    assert_eq!(items.len(), 5, "{items:?}");

    // Unread is kept at the pane's station, so it is the same whichever
    // station's deck marks it or asks.
    let unread_at_bravo = || tokio::task::block_in_place(|| b.pane(&worker))["unread"].clone();
    assert_eq!(unread_at_bravo(), json!(true));
    let read = via_alpha("pane.read", json!({"pane": worker}));
    assert_eq!(read["unread"], json!(false), "{read}");
    assert_eq!(unread_at_bravo(), json!(false));
    watch_until!("the read mark", |_: &Vec<Value>, live: &Value| live
        ["unread"]
        == json!(false));

    // A view that lost its connection comes back with its session and its
    // cursor, and is sent what came after it and nothing else.
    let session =
        via_alpha("pane.conversation", json!({"pane": worker, "limit": 1}))["session"].clone();
    ws.send(Message::Text(
        json!({"id": 9, "method": "pane.conversation.watch", "params": {
            "pane": worker, "station": "bravo", "session": session, "after": items[2]["line"]}})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let back: Value = loop {
        if let Some(Ok(Message::Text(t))) = ws.next().await {
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            if v["id"] == json!(9) {
                break v["result"].clone();
            }
        }
    };
    assert_eq!(back["reset"], json!(false), "{back}");
    assert_eq!(back["items"], json!([items[3], items[4]]), "{back}");
    assert_eq!(back["updated"], json!([]));

    // An agent is refused all of it, its own pane included; and the titles
    // of sessions, which are made from what was said.
    for method in [
        "pane.conversation",
        "pane.conversation.detail",
        "pane.read",
        "pane.titles",
    ] {
        let err = tokio::task::block_in_place(|| {
            b.call_as_pane(&worker, method, json!({"pane": worker, "item": "1.0"}))
        })
        .unwrap_err();
        assert_eq!(err.code, "denied", "{method}");
    }
    let (mut as_agent, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/api/ws", b.port))
            .await
            .unwrap();
    let token = tokio::task::block_in_place(|| b.pane_token(&worker));
    for msg in [
        json!({"auth": {"token": token}}),
        json!({"id": 1, "method": "pane.conversation.watch", "params": {"pane": worker}}),
    ] {
        as_agent
            .send(Message::Text(msg.to_string().into()))
            .await
            .unwrap();
    }
    let refused: Value = loop {
        let t = as_agent.next().await.unwrap().unwrap();
        let v: Value = serde_json::from_str(t.to_text().unwrap()).unwrap();
        if v["id"] == json!(1) {
            break v;
        }
    };
    assert_eq!(refused["error"]["code"], json!("denied"), "{refused}");
    // A pane with no conversation says why, so the view opens the terminal.
    let shell = via_alpha("pane.conversation", json!({"pane": pane}));
    assert_eq!(shell["available"], json!(false));
    assert_eq!(
        shell["reason"],
        json!("no conversation for this pane: it runs a shell, not an agent")
    );
    // Nothing of bravo's conversation was kept at the station that passed it on.
    assert_eq!(
        tokio::task::block_in_place(|| a.call("archive.sessions", json!({}))),
        json!([])
    );
    drop(tokio::task::spawn_blocking(move || drop((a, b))).await);
}

/// A fault found on 15 Nov 2026: a pane on another station, shown through the
/// station the deck was opened on, did not follow the sizing rule. Every tab
/// reached it as one client (`gw-<station>`), so two windows fought over its
/// size and neither was offered "Take control", and its resizes never
/// reached a deck served from elsewhere.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn a_pane_on_another_station_keeps_one_size_whichever_station_serves_the_deck() {
    use common::browser::Page;
    let (a, b) = Station::pair();
    let pane = b.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "one", "cwd": b.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let size = || {
        let p = b.pane(&pane);
        (p["cols"].as_u64().unwrap(), p["rows"].as_u64().unwrap())
    };
    let open = |s: &Station, w: u32, h: u32| {
        let page = Page::open(s, w, h);
        page.goto(&format!("/s/bravo/p/{pane}"));
        page.wait("the pane", "!!document.querySelector('.pane .xterm-rows')");
        page
    };
    // What a deck shows of the pane: whether it gave way, and the size its
    // terminal is drawn at.
    let gave_way = "!!document.querySelector('.term-body.letterbox') && [...document.querySelectorAll('.strip button')].some(b => b.innerText === 'Take control')";
    let shown = |page: &Page| {
        let v = page.js("(t => [t.cols, t.rows])(document.querySelector('.pane').paneTerm.term)");
        (v[0].as_u64().unwrap(), v[1].as_u64().unwrap())
    };
    let shows = |page: &Page, what: &str, want: (u64, u64)| {
        wait_for(what, 10, || (shown(page) == want).then_some(()));
    };

    // Opened at the pane's own station, and watched on a phone through
    // alpha, which takes no size.
    let direct = open(&b, 1500, 900);
    let first = wait_for("the size of the window at bravo", 10, || {
        let s = size();
        (s.0 > 120).then_some(s)
    });
    let phone = open(&a, 390, 800);
    shows(&phone, "the phone to show the pane's size", first);
    assert_eq!(size(), first, "watching on a phone took no size");

    // A window through alpha takes the size: the window at bravo gives way,
    // and the phone, which hears of it through alpha, shows the new size.
    let one = open(&a, 1100, 700);
    let through_one = wait_for("the size of alpha's window", 10, || {
        let s = size();
        (s.0 < first.0).then_some(s)
    });
    direct.wait("the window at bravo to give way", gave_way);
    shows(&phone, "the phone to follow the pane's size", through_one);
    let who = b.pane(&pane)["controller"].as_str().unwrap().to_string();
    assert!(who.starts_with("gw-alpha-web-"), "{who}");

    // Fitting the pane to the phone: a second client through alpha is a
    // client of its own at bravo. It takes the size, and alpha's window is
    // told, as the window at bravo is.
    phone.js("[...document.querySelectorAll('.phone-pane button')].find(b => b.innerText === 'Fit to screen').click()");
    let on_phone = wait_for("the phone's size", 10, || {
        let s = size();
        (s.0 < 80).then_some(s)
    });
    one.wait("alpha's window to give way", gave_way);
    assert_ne!(b.pane(&pane)["controller"], json!(who));
    shows(&one, "alpha's window to show the pane's size", on_phone);
    shows(&direct, "the window at bravo to show it", on_phone);

    // Taking control back in alpha's window works through alpha, and both
    // of the others are told.
    one.js("[...document.querySelectorAll('.strip button')].find(b => b.innerText === 'Take control').click()");
    wait_for("the size of alpha's window again", 10, || {
        (size() == through_one).then_some(())
    });
    assert_eq!(b.pane(&pane)["controller"], json!(who));
    phone.wait("the phone to give way", gave_way);
    shows(&phone, "the phone to show the pane's size", through_one);
    shows(&direct, "the window at bravo to show it", through_one);
    assert!(direct.js(gave_way).as_bool().unwrap());
    // Nobody fights over it.
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(size(), through_one);
    assert!(!one.js(gave_way).as_bool().unwrap());
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
#[cfg_attr(
    windows,
    ignore = "sends a Unix signal (SIGSTOP, SIGTERM); Windows has none"
)]
fn a_stop_for_an_unreachable_station_is_shown_honestly_and_delivered_on_reconnect() {
    let (a, b) = Station::pair();
    let long_job = json!({"steps": [{"prompt": "long job"}, {"work": 120000}]});
    let on_a = a.fake(long_job.clone(), json!({}));
    let on_b = b.fake(long_job.clone(), json!({}));
    a.wait_state(&on_a, "working");
    b.wait_state(&on_b, "working");

    // Bravo drops off the network. Its work keeps running.
    b.signal(SIGSTOP);
    b.wait_frozen();
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
    // It was aimed at the moment it was issued, converted to bravo's clock,
    // not counted back from when it arrived; and applied once.
    let applied = b.log("station.stop_all");
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0]["data"]["aimed_by"], json!("cutoff"));
}

/// Merge tier. Measured (plans/FAILURES.md §7): in a fleet stop a station
/// that answered waited 5.01 s behind one that did not. Done when: with one
/// of three stations frozen, the one that answers is confirmed in under a
/// second (a generous bound: the exact figure is the nightly benchmark's),
/// while the frozen one's row still says only that the stop was requested;
/// that row then says unreachable, never stopped; and the stop reaches the
/// frozen station when it is back, once. The other two stations' clocks are
/// 45 and 60 seconds ahead of the issuing one's, so the moment the stop was
/// issued has to be converted to each: counted the wrong way, the work that
/// was running would be taken for work started after the stop.
#[test]
#[ignore = "merge tier: three daemons, and it waits out the five-second limit of a station that does not answer"]
fn a_fleet_stop_reaches_the_station_that_answers_while_another_is_frozen() {
    let fleet = Station::fleet(&["alpha", "bravo", "charlie"], |name, _| {
        let ahead = match name {
            "bravo" => "45000",
            "charlie" => "60000",
            _ => "0",
        };
        (
            String::new(),
            vec![("SPANDECK_TEST_CLOCK_MS".to_string(), ahead.to_string())],
        )
    });
    let (a, b, c) = (&fleet[0], &fleet[1], &fleet[2]);
    wait_for("the three stations to find each other", 30, || {
        fleet
            .iter()
            .all(|s| {
                s.call("fleet.refresh", json!({}))["stations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|x| x["state"] == json!("online"))
                    .count()
                    == 3
            })
            .then_some(())
    });
    let long_job = json!({"steps": [{"prompt": "long job"}, {"work": 120000}]});
    let on_a = a.fake(long_job.clone(), json!({}));
    let on_b = b.fake(long_job.clone(), json!({}));
    let on_c = c.fake(long_job.clone(), json!({}));
    a.wait_state(&on_a, "working");
    b.wait_state(&on_b, "working");
    c.wait_state(&on_c, "working");
    c.signal(SIGSTOP);
    c.wait_frozen();

    let began = std::time::Instant::now();
    std::thread::scope(|scope| {
        let all = scope.spawn(|| a.call("stop.all", json!({"scope": "fleet"})));
        let took = wait_for("bravo's stop to be confirmed", 20, || {
            (stop_row(a, "bravo")["status"] == json!("confirmed")).then(|| began.elapsed())
        });
        let charlie = stop_row(a, "charlie");
        assert!(
            took < Duration::from_secs(1),
            "bravo was confirmed after {took:?}"
        );
        assert_eq!(charlie["status"], json!("requested"), "{charlie}");
        a.wait_text(&on_a, "[fake] interrupted");
        b.wait_text(&on_b, "[fake] interrupted");
        let out = all.join().unwrap();
        assert_eq!(out["stops"].as_array().unwrap().len(), 3, "{out}");
        assert!(
            out["stops"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["saved"] == json!(true)),
            "{out}"
        );
    });
    let row = stop_row(a, "charlie");
    assert_eq!(
        (&row["status"], &row["label"]),
        (&json!("unreachable"), &json!("station unreachable")),
        "{row}"
    );
    assert_eq!(stop_row(a, "bravo")["result"]["stopped"], json!([on_b]));
    assert_eq!(
        b.log("station.stop_all")[0]["data"]["aimed_by"],
        json!("cutoff")
    );

    // Charlie is back. Work started there since is left alone.
    c.signal(SIGCONT);
    let later = c.fake(long_job, json!({}));
    c.wait_state(&later, "working");
    wait_for("the waiting stop to be delivered", 30, || {
        a.call("fleet.refresh", json!({}));
        (stop_row(a, "charlie")["status"] == json!("confirmed")).then_some(())
    });
    c.wait_text(&on_c, "[fake] interrupted");
    assert_eq!(stop_row(a, "charlie")["result"]["stopped"], json!([on_c]));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(c.pane(&later)["state"], json!("working"));
    let applied = c.log("station.stop_all");
    assert_eq!(applied.len(), 1, "applied once, however often it arrived");
    assert_eq!(applied[0]["data"]["aimed_by"], json!("cutoff"));
}

/// Merge tier. One station of the fleet stays a version behind. A station
/// with the switch off stands in for it here: like 0.4.3 it announces no
/// `stop_cutoff`, sends none and reads none. Done when: a stop either way is
/// applied by its age, as before, and confirmed.
#[test]
#[ignore = "merge tier: starts two daemons"]
fn a_station_that_reads_no_cut_off_is_stopped_as_before() {
    let (a, b) = Station::pair_with("[flags]\nstop_cutoff = false\n");
    let long_job = json!({"steps": [{"prompt": "long job"}, {"work": 120000}]});
    for (from, to) in [(&a, &b), (&b, &a)] {
        let pane = to.fake(long_job.clone(), json!({}));
        to.wait_state(&pane, "working");
        let out = from.call("stop.all", json!({"scope": to.name}));
        assert_eq!(out["stops"][0]["status"], json!("confirmed"), "{out}");
        assert_eq!(out["stops"][0]["result"]["stopped"], json!([pane]));
        to.wait_text(&pane, "[fake] interrupted");
        let applied = to.log("station.stop_all");
        assert_eq!(applied.len(), 1, "{applied:?}");
        assert_eq!(applied[0]["data"]["aimed_by"], json!("age"));
    }
    let has = |s: &Station| {
        s.call("station.info", json!({}))["caps"]
            .as_array()
            .unwrap()
            .contains(&json!("stop_cutoff"))
    };
    assert_eq!((has(&a), has(&b)), (false, true));
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
/// and attaching from the phone is read-only. An agent's pane opens as its
/// conversation (below, `the_conversation_view`), with the terminal one tap
/// away; a shell is its terminal.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
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
    // A bug that happened (15 Nov 2026): the list is drawn again for every
    // change, and each time it jumped back to the top while the owner was
    // scrolling down it. Where it is scrolled to is kept.
    page.js("document.getElementById('app').style.height = '220px'");
    page.js("document.querySelector('main.phone').scrollTop = 90");
    let top = "document.querySelector('main.phone').scrollTop";
    assert_eq!(page.js(top), json!(90), "the list is long enough to scroll");
    page.redraw(&s, &busy);
    assert_eq!(page.js(top), json!(90), "still where it was scrolled to");
    page.js("document.getElementById('app').style.height = '800px'");
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

    // The list a computer shows on its left is one tap away, with every
    // control it had: what follows uses it.
    page.goto("#/fleet");
    page.wait(
        "the list by station",
        "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.home-switch button')].find(b => b.innerText === 'By station')) && !!document.querySelector('main.phone .phone-station')",
    );
    // Open the busy agent: the terminal, and a stop control. An agent's pane
    // opens as its conversation; its terminal is one tap from there, and the
    // phone remembers which was chosen.
    let open = |name: &str| {
        page.goto("#/fleet");
        // The list is redrawn every few seconds: wait for this pane's row.
        page.wait(
            "the pane's row in the list",
            &format!("(r => r ? (r.click(), true) : false)([...document.querySelectorAll('main.phone .phone-station .phone-row')].find(r => r.innerText.includes('{name}')))"),
        );
        page.wait(
            "the pane on the phone",
            "(t => (t && t.click(), !!document.querySelector('.phone-pane .xterm-rows') && !!document.querySelector('.phone-pane .composer-box')))([...document.querySelectorAll('.phone-chat .chat-switch button')].find(b => b.innerText === 'Terminal'))",
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
    page.redraw(&s, &talker);
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
    // A shell has no conversation: it is its terminal, and the bar says why
    // where the choice of Chat or Terminal would be.
    assert_eq!(
        page.js("[document.querySelector('.phone-pane .chat-why')?.innerText, !!document.querySelector('.chat-switch')]"),
        json!(["Terminal only: it runs a shell, not an agent", false])
    );
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
    // The drag is repeated until it takes: on a slow machine the first one
    // landed while the last lines were still being drawn, and the view went
    // back to the end (seen twice on the Mac CI runner).
    // Still fails now and then on the Mac CI runner only (never on a real
    // Mac): when it does, say what the page showed, so the cause can be found.
    let dragged = format!(
            "(() => {{ // The finger lands on the terminal's own element, as on a real phone.
           if (!{rows}.includes('1300')) return /1[0-2][0-9][0-9]/.test({rows});
           const el = document.querySelector('.phone-pane .xterm-screen');
           const touch = (type, y) => {{ const ev = new Event(type, {{bubbles: true, cancelable: true}}); Object.defineProperty(ev, 'touches', {{value: y === null ? [] : [{{clientX: 100, clientY: y}}]}}); return ev; }};
           el.dispatchEvent(touch('touchstart', 200));
           for (let y = 220; y <= 700; y += 20) el.dispatchEvent(touch('touchmove', y));
           el.dispatchEvent(touch('touchend', null));
           return false; }})()"
        );
    // The last failure showed the view at the earliest lines straight after a
    // drag, and back at the end a quarter of a second later. Note everything
    // that could move it, so the next failure names what did.
    page.js(
        "(() => { const t = document.querySelector('.phone-pane .pane').paneTerm; const log = window.__moved = []; const at = performance.now();
           const note = (what) => { if (log.length < 60) log.push(Math.round(performance.now() - at) + ' ' + what); };
           for (const name of ['write', 'reset', 'clear', 'resize', 'scrollToBottom', 'scrollToLine', 'refresh']) {
             const real = t.term[name].bind(t.term);
             t.term[name] = (...a) => { note(name + (name === 'write' ? ' ' + (a[0] ? a[0].length : 0) : ' ' + JSON.stringify(a.slice(0, 2))) + (name === 'write' ? '' : ' <- ' + (new Error().stack || '').split('\\n').slice(2, 5).map(x => x.trim().replace(/^at /, '').replace(/https?:[^ )]*\\//, '')).join(' < '))); return real(...a); };
           }
           let last = -1; t.term.onScroll((y) => { if (y > last && last >= 0) note('moved towards the end: ' + last + ' -> ' + y); last = y; });
           window.__paneEl = document.querySelector('.phone-pane .pane');
           return true; })()",
    );
    let start = std::time::Instant::now();
    loop {
        let v = page.js(&dragged);
        if v == json!(true) {
            break;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let moved = page.js("JSON.stringify({samePane: window.__paneEl === document.querySelector('.phone-pane .pane'), moved: window.__moved})");
            eprintln!("what moved the view: {moved}");
            let seen = page.js(
                "(() => { const v = document.querySelector('.phone-pane .xterm-viewport'); const r = document.querySelector('.phone-pane .xterm-rows'); const s = document.querySelector('.phone-pane .xterm-screen'); return JSON.stringify({scrollTop: v && v.scrollTop, scrollHeight: v && v.scrollHeight, clientHeight: v && v.clientHeight, rowCount: r && r.children.length, rowHeight: r && r.firstElementChild && r.firstElementChild.getBoundingClientRect().height, screen: s && [s.clientWidth, s.clientHeight], hidden: document.hidden, lastRows: r && r.innerText.split('\\n').filter(x => x.trim()).slice(-3)}); })()",
            );
            panic!(
                "timed out waiting for earlier output after dragging down; the page showed {seen}"
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    if std::env::var_os("SPANDECK_TEST_SHOW_MOVES").is_some() {
        eprintln!("moves: {}", page.js("JSON.stringify(window.__moved)"));
    }

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

/// What an agent says is data (AGENTS.md rule 6). For text written to
/// attack the page, what the markdown renderer builds holds only elements on
/// its list, no attribute but `class` and a link's `href`, and no address
/// that is not http or https; and nothing in it ran.
const HOSTILE: &[&str] = &[
    "<script>window.__hit = 1</script><img src=x onerror=\"window.__hit = 1\"> <a href=\"javascript:window.__hit=1\">x</a>",
    "[click](javascript:window.__hit=1) [d](data:text/html,<script>window.__hit=1</script>) [v](vbscript:x) [j](JaVaScRiPt:window.__hit=1) [t](\tjavascript:window.__hit=1)",
    "![x](javascript:window.__hit=1) ![y](data:image/svg+xml;base64,AAAA) ![z](https://ok.test/pixel.png) <javascript:window.__hit=1> <data:text/html,hi>",
    "[a](https://ok.test/\"onmouseover=\"window.__hit=1) [b](https://ok.test/ 'x' onclick=window.__hit=1) [c](https://ok.test/#\"><svg/onload=window.__hit=1>)",
    "```html\n<iframe src=javascript:window.__hit=1></iframe>\n```\n<style>*{display:none}</style>\n\n```\"><img src=x onerror=window.__hit=1>\ncode\n```",
    "| <svg onload=window.__hit=1> | [x](javascript:1) |\n|---|---|\n| <form action=javascript:window.__hit=1><input type=submit> | &lt;script&gt; |",
    "# <img src=x onerror=window.__hit=1>\n\n> <details open ontoggle=window.__hit=1>\n\n- [ ] <input onfocus=window.__hit=1 autofocus>\n1. <body onload=window.__hit=1>",
    "<https://ok.test/?q=javascript:alert(1)> https://ok.test/a_(b) http://ok.test/<script> HTTPS://OK.TEST/x **[bold link](https://ok.test/**)**",
    "[x]: javascript:window.__hit=1\n[x][x] [x]\n\n<a href='data:text/html;base64,PHNjcmlwdD4='>d</a> `<b onmouseover=window.__hit=1>`",
];

fn the_markdown_of_an_agent_is_drawn_as_text(page: &common::browser::Page) {
    let found = page.js(&format!(
        "(async () => {{ const m = await import('./md.js'); const bad = [];
           for (const src of {cases}) {{
             const el = m.renderMarkdown(src, {{ openLink() {{}}, copy() {{}} }});
             document.getElementById('overlays').append(el);
             for (const e of [el, ...el.querySelectorAll('*')]) {{
               if (!m.ALLOWED.includes(e.tagName)) bad.push('element ' + e.tagName);
               for (const a of e.attributes) {{
                 if (a.name !== 'class' && a.name !== 'href' && !a.name.startsWith('data-')) bad.push(e.tagName + ' has ' + a.name);
                 if (a.name === 'href' ? !/^https?:\\/\\/ok\\.test\\//.test(a.value) : /javascript:|data:/i.test(a.value)) bad.push(a.name + '=' + a.value);
               }}
             }}
             if (el.textContent.length < src.replace(/\\s/g, '').length / 2) bad.push('text lost: ' + src.slice(0, 40));
             el.remove();
           }}
           await new Promise(r => setTimeout(r, 50));
           if (window.__hit) bad.push('something ran');
           const links = m.renderMarkdown({cases}[7]).querySelectorAll('a').length;
           return {{ bad, links }}; }})()",
        cases = json!(HOSTILE)
    ));
    assert_eq!(found, json!({"bad": [], "links": 5}), "{found}");
}

/// Mobile plan, phase M2, on a phone: an agent's pane opens as what was
/// said, with the tool calls folded; an approval is answered from its card
/// and the reply arrives once; the terminal is one tap away and the draft
/// goes along; showing a conversation never resizes the pane; the unread dot
/// is set and cleared; and "Next" leads to what else waits.
fn the_conversation_view(s: &Station, page: &common::browser::Page) {
    the_markdown_of_an_agent_is_drawn_as_text(page);
    // An agent that has finished and said something the owner has not read.
    let finished = &s.fake(
        json!({"steps": [{"prompt": "x"}, {"turn_end": "ok"}]}),
        json!({"name": "asker"}),
    );
    let line = |v: Value| json!({"transcript": v});
    let said = |blocks: Value| json!({"type": "assistant", "message": {"role": "assistant", "content": blocks}});
    let result = |id: &str, out: &str| {
        line(
            json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}, "toolUseResult": {"stdout": out, "stderr": "", "interrupted": false}}),
        )
    };
    let counter = s.fake(
        json!({"steps": [
            line(json!({"type": "ai-title", "aiTitle": "Otter counts"})),
            line(json!({"type": "hologram-state", "note": "a record of a kind nobody knows"})),
            {"prompt": "count the otters"},
            line(said(json!([{"type": "text", "text": "I'll **count** them:\n\n- the file first\n- then the tally\n\n<img src=x onerror=\"window.__hit = 1\">"}]))),
            line(said(json!([{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "wc -l otters.csv"}}]))),
            result("toolu_1", "12 otters.csv"),
            line(said(json!([{"type": "tool_use", "id": "toolu_2", "name": "Read", "input": {"file_path": "/tmp/otters.csv"}}]))),
            result("toolu_2", "name,river"),
            line(said(json!([{"type": "tool_use", "id": "toolu_3", "name": "Bash", "input": {"command": "rm otters.csv"}}]))),
            {"permission": {"name": "Bash", "command": "rm otters.csv"}},
            result("toolu_3", ""),
            {"turn_end": "twelve otters, and the file is gone"}
        ]}),
        json!({"name": "counter"}),
    );
    s.wait_state(&counter, "needs_you");
    let click = |what: &str, el: &str| {
        page.wait(what, &format!("(e => e ? (e.click(), true) : false)({el})"));
    };
    let row = |name: &str| {
        format!("[...document.querySelectorAll('main.phone .session-row')].find(r => r.innerText.includes('{name}'))")
    };

    // The list of sessions: the agent's own title for the session, and a dot
    // on the one that finished and has not been read.
    page.goto("#/fleet");
    click(
        "Recent",
        "[...document.querySelectorAll('.home-switch button')].find(b => b.innerText === 'Recent')",
    );
    page.wait("the session under its title", &row("Otter counts"));
    page.wait(
        "the dot on the agent that finished",
        &format!(
            "(r => r && !!r.querySelector('.unread-dot'))({})",
            row("asker")
        ),
    );
    assert_eq!(s.pane(finished)["unread"], json!(true));

    // Its conversation: the reply as text (what looks like HTML is the
    // characters typed), the tool calls folded into one line, and a record
    // the station did not understand said to be missing, not passed over.
    click("the session's row", &row("Otter counts"));
    page.wait(
        "the reply and the folded tools",
        "!!document.querySelector('.phone-chat .chat-text strong') && !!document.querySelector('.phone-chat .chat-fold') && !!document.querySelector('.chat-cards .approval')",
    );
    let seen = page.js(
        "[document.querySelector('.chat-text').innerText, document.querySelector('.chat-text strong').innerText, document.querySelectorAll('.chat-text li').length,
          document.querySelector('.chat-fold').innerText.replace(/\\s+/g, ' ').trim(), document.querySelectorAll('.chat-log img, .chat-log script').length, !!window.__hit,
          document.querySelector('.chat-title b').innerText, document.querySelector('.chat-bubble').innerText, document.querySelectorAll('.phone-chat .xterm').length,
          document.querySelector('.chat-quiet.warn')?.innerText.replace(/\\s+/g, ' ')]",
    );
    assert!(
        seen[0].as_str().unwrap().contains("<img src=x onerror="),
        "{seen}"
    );
    assert_eq!(
        seen.as_array().unwrap()[1..],
        json!([
            "count",
            2,
            "Working: 3 tools so far ›",
            0,
            false,
            "Otter counts",
            "count the otters",
            0,
            "1 record not shown: open the terminal to see everything. Open terminal"
        ])
        .as_array()
        .unwrap()[..],
        "{seen}"
    );
    // One folded line opens to a row for each call, and a row to its detail.
    click("the folded tools", "document.querySelector('.chat-fold')");
    click("the first call", "document.querySelectorAll('.chat-tool-row').length === 3 && document.querySelector('.chat-tool-row')");
    page.wait(
        "what the call printed",
        "[...document.querySelectorAll('.chat-detail .chat-out')].some(o => o.innerText.includes('12 otters.csv'))",
    );

    // The request its hook holds is a card above the message box. Allowed
    // there, the call finishes and the reply arrives, each thing once.
    assert_eq!(
        page.js("document.querySelector('.chat-cards .approval .mono').innerText"),
        json!("Bash: rm otters.csv")
    );
    page.js("[...document.querySelectorAll('.chat-cards .approval button')].find(b => b.innerText === 'Allow').click()");
    s.wait_text(&counter, "[fake] permission: allow");
    page.wait(
        "the reply",
        "[...document.querySelectorAll('.chat-text')].some(t => t.innerText.includes('twelve otters')) && !document.querySelector('.chat-cards .approval') && !!document.querySelector('.chat-acts button')",
    );
    assert_eq!(
        page.js("[document.querySelectorAll('.chat-text').length, document.querySelectorAll('.chat-user').length, document.querySelectorAll('.chat-tool-row').length,
                  document.querySelector('.chat-fold').innerText.replace(/\\s+/g, ' ').trim().replace(/ · .* s /, ' '), [...document.querySelectorAll('.chat-acts button')].map(b => b.innerText)[0]]"),
        json!([2, 1, 3, "Ran 3 tools ▾", "Copy"])
    );
    // Showing a conversation never takes the pane's size.
    let pane = s.pane(&counter);
    assert_eq!(
        (pane["cols"].as_u64(), pane["controller"].is_null()),
        (Some(120), true),
        "the conversation did not resize the pane"
    );
    // It was on screen, so it is read: the station is told, and the dot goes.
    wait_for("the conversation to be marked read", 10, || {
        (s.pane(&counter)["unread"] == json!(false)).then_some(())
    });

    // The terminal is one tap away, and what was being written goes along.
    page.js("(document.querySelector('.chat-box').value = 'half a thought', document.querySelector('.chat-box').focus(), window.__box = document.querySelector('.chat-box'), true)");
    page.redraw(s, &counter);
    assert_eq!(
        page.js("[document.activeElement === window.__box, document.contains(window.__box), window.__box.value]"),
        json!([true, true, "half a thought"]),
        "the message box keeps the keyboard and the draft through a redraw"
    );
    click("Terminal", "[...document.querySelectorAll('.chat-switch button')].find(b => b.innerText === 'Terminal')");
    page.wait(
        "the terminal, with the draft",
        "!!document.querySelector('.phone-pane .xterm-rows') && document.querySelector('.phone-pane .composer-box')?.value === 'half a thought'",
    );
    page.js("(document.querySelector('.composer-box').value = 'half a thought, and more', true)");
    click(
        "Chat",
        "[...document.querySelectorAll('.chat-switch button')].find(b => b.innerText === 'Chat')",
    );
    page.wait(
        "the conversation again, with the draft",
        "document.querySelector('.phone-chat .chat-box')?.value === 'half a thought, and more' && document.querySelectorAll('.chat-text').length === 2",
    );
    // Sent the way the phone's message box sends.
    page.js("document.querySelector('.chat-send').click()");
    s.wait_text(&counter, "[fake] got: half a thought, and more");
    page.wait(
        "the message as a bubble, and the box empty",
        "document.querySelectorAll('.chat-user').length === 2 && document.querySelector('.chat-box').value === ''",
    );

    // "Next" leads to what else waits for the owner: here, the agent that
    // finished and has not been read. Opened, it is read, and its dot is gone
    // from the list.
    click(
        "Next",
        "(n => n && n.innerText.replace(/\\s+/g, ' ').trim() === 'Next › 1' && n)(document.querySelector('.chat-next'))",
    );
    page.wait(
        "the finished agent's conversation",
        &format!("location.hash.endsWith('/p/{finished}') && !!document.querySelector('.phone-chat .chat-text')"),
    );
    wait_for("that conversation to be marked read", 10, || {
        (s.pane(finished)["unread"] == json!(false)).then_some(())
    });
    page.goto("#/fleet");
    page.wait(
        "the dot to go",
        &format!(
            "(r => r && !r.querySelector('.unread-dot'))({})",
            row("asker")
        ),
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

    // A global layout too: the phone steps through it the same way, and it
    // may hold a pane that is in no project.
    let loose = s.call("pane.spawn", json!({"agent": "shell", "name": "scratch"}));
    s.call(
        "deck.layout.save",
        json!({"layout": {"name": "all", "tree": {"dir": "row", "ratio": 500,
            "a": {"pane": "lead", "project": "proj", "station": "alpha"}, "b": {"id": loose["id"], "station": "alpha"}}}}),
    );

    let page = Page::open(&s, 390, 844);
    page.wait(
        "the list by station",
        "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.home-switch button')].find(b => b.innerText === 'By station'))",
    );
    // The list follows a computer's: under the station its project, with the
    // project's layout, Watch, then its panes; then the global layouts.
    let list = page.wait(
        "the list",
        "(rows => rows.length >= 7 ? rows : null)([...document.querySelectorAll('main.phone .phone-station .phone-row, main.phone .phone-layouts .phone-row')].map(r => r.getAttribute('aria-label')))",
    );
    assert_eq!(
        list,
        json!([
            "build, layout",
            "Watch proj",
            "dev, Shell, idle",
            "dev2, Shell, idle",
            "lead, Shell, idle",
            "scratch, Shell, idle",
            "all, layout"
        ]),
    );
    page.wait("the global layout", "(r => r ? (r.click(), true) : false)(document.querySelector('main.phone .phone-layouts .lay-row'))");
    page.wait(
        "its map",
        "location.hash === '#/l/all' && document.querySelectorAll('.lay-map button').length === 2 && !!document.querySelector('.phone-layout .xterm-rows')",
    );
    assert_eq!(
        page.js("[[...document.querySelectorAll('.lay-map button')].map(b => b.getAttribute('aria-label')), document.querySelector('.lay-showing').innerText]"),
        json!([["lead from proj, idle", "scratch, idle"], "Showing lead (from proj) · 1 of 2"])
    );
    page.goto("#/fleet");
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
    page.redraw(&s, "dev2");
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

    // The same station and the same phone, for the conversation view.
    the_conversation_view(&s, &page);
}

/// Kills a daemon the harness did not start (the one "installed" by an add).
struct KillAtEnd(std::path::PathBuf);
impl Drop for KillAtEnd {
    fn drop(&mut self) {
        if let Some(pid) = std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|p| p.trim().parse::<i32>().ok())
        {
            kill_pid(pid, 9);
        }
    }
}

fn executable(path: &std::path::Path, text: &str) {
    std::fs::write(path, text).unwrap();
    spandeck_terminal::sys::set_executable(path).unwrap();
}

/// Every file under `dir` that holds `needle`.
fn files_holding(dir: &std::path::Path, needle: &str) -> Vec<std::path::PathBuf> {
    let mut found = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = e.path();
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            found.extend(files_holding(&path, needle));
        } else if kind.is_file() {
            let bytes = std::fs::read(&path).unwrap_or_default();
            if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                found.push(path);
            }
        }
    }
    found
}

/// Add a machine (the owner's request, 14 Nov 2026). Done when: the picker
/// lists only Mac and Linux machines under the owner's login that are online
/// and not stations; an add installs spandeck over the SSH command, starts
/// it, and ends in a join request shown with its code; a wrong password and
/// a machine that does not answer are each explained; only the owner may
/// start one; and the password is in no file, log, event or command line.
///
/// Tailscale and SSH are stand-ins: a `tailscale` that prints a fixed
/// tailnet, and an "ssh" that asks for the password the way ssh does (by
/// running the askpass helper it was given) and then runs what arrives on
/// its standard input here, as a machine with its own home directory. The
/// release is downloaded from a stand-in release page.
#[test]
#[cfg_attr(
    windows,
    ignore = "builds its release archive with sh and tar.gz; the Windows release is a .zip"
)]
fn a_machine_is_added_from_the_deck_and_its_password_is_kept_nowhere() {
    const RIGHT: &str = "c0rrect-horse-staple-91";
    const WRONG: &str = "wr0ng-horse-battery-37";
    let releases = common::fake_releases::Releases::start(true);
    let dir = tempfile::tempdir().unwrap();
    let at = |p: &str| dir.path().join(p);
    for d in [
        "standin",
        "seen",
        "remote-bin",
        "charlie/config",
        "charlie/home",
    ] {
        std::fs::create_dir_all(at(d)).unwrap();
    }
    let (pa, pc) = (free_port(), free_port());

    // The tailnet: two machines that can be added, and five that cannot.
    let peer = |name: &str, os: &str, ip: &str, more: Value| {
        let mut v = json!({"ID": format!("n-{name}"), "HostName": name, "DNSName": format!("{name}.tail0000.ts.net."),
            "OS": os, "UserID": 7, "Online": true, "TailscaleIPs": [ip]});
        for (k, x) in more.as_object().unwrap() {
            v[k] = x.clone();
        }
        (format!("key-{name}"), v)
    };
    let peers: serde_json::Map<String, Value> = [
        peer("charlie", "linux", "100.64.0.7", json!({})),
        peer(
            "studio",
            "macOS",
            "100.64.0.8",
            json!({"sshHostKeys": ["ssh-ed25519 AAAA"]}),
        ),
        peer("phone", "iOS", "100.64.0.9", json!({})),
        peer("winbox", "windows", "100.64.0.10", json!({})),
        peer(
            "ci-runner",
            "linux",
            "100.64.0.11",
            json!({"Tags": ["tag:ci"]}),
        ),
        peer("asleep", "macOS", "100.64.0.12", json!({"Online": false})),
        peer("guest", "linux", "100.64.0.13", json!({"UserID": 8})),
    ]
    .into_iter()
    .collect();
    std::fs::write(
        at("standin/status.json"),
        json!({"Self": {"UserID": 7, "HostName": "alpha", "TailscaleIPs": ["100.64.0.1"]},
            "User": {"7": {"LoginName": "owner@example.com"}}, "Peer": peers})
        .to_string(),
    )
    .unwrap();
    executable(
        &at("standin/tailscale"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in version) echo 1.0 ;; status) cat {} ;; *) exit 1 ;; esac\n",
            at("standin/status.json").display()
        ),
    );
    // The machine's own service manager is not this machine's: there, the
    // login service cannot be started, and spandeck is started directly.
    // Each notes what it was asked: the uninstall below must reach these,
    // never this machine's real login service.
    for tool in ["systemctl", "launchctl", "loginctl"] {
        executable(
            &at("remote-bin").join(tool),
            &format!(
                "#!/bin/sh\necho \"{tool} $*\" >> {}/tools\nexit 1\n",
                at("seen").display()
            ),
        );
    }
    std::fs::write(at("standin/right"), RIGHT).unwrap();
    std::fs::write(
        at("charlie/config/config.toml"),
        format!("station = \"charlie\"\nweb_port = {pc}\nweb_bind = [\"127.0.0.1\"]\ndiscovery = false\nkeep_awake_cmd = [\"sleep\", \"300\"]\nprices.auto = false\npeers = [\"127.0.0.1:{pa}\"]\n"),
    )
    .unwrap();
    let _charlie = KillAtEnd(at("charlie/state/spandeckd.pid"));
    executable(
        &at("standin/ssh"),
        &format!(
            r#"#!/bin/sh
# What any process on this machine could see of a real ssh: its command line
# and its environment. The test looks for the password in them.
{{ echo "argv: $*"; env; }} >> {seen}/ssh
user=$1 host=$2
case "$host" in
  100.64.0.8) echo "ssh: connect to host $host port 22: Operation timed out" >&2; exit 255 ;;
esac
pw=$("$SSH_ASKPASS" "$user@$host's password: ") || pw=
if [ "$pw" != "$(cat {standin}/right)" ]; then
  echo "$user@$host: Permission denied (publickey,password)." >&2
  exit 255
fi
unset pw
exec env -i PATH={remote_bin}:/usr/bin:/bin HOME={c}/home SPANDECK_STATE_DIR={c}/state \
  SPANDECK_CONFIG_DIR={c}/config SPANDECK_BIN_DIR={c}/bin SPANDECK_LOG=warn sh -s
"#,
            seen = at("seen").display(),
            standin = at("standin").display(),
            remote_bin = at("remote-bin").display(),
            c = at("charlie").display(),
        ),
    );

    let path = format!(
        "{}:{}",
        at("standin").display(),
        std::env::var("PATH").unwrap()
    );
    let mut a = Station::start_on(
        "alpha",
        pa,
        &format!(
            "add_machine_cmd = [\"{}\", \"{{user}}\", \"{{host}}\"]\npeers = [\"127.0.0.1:{pc}\"]\n[update]\nurl = \"{}\"\nin_place = false\n",
            at("standin/ssh").display(),
            releases.url()
        ),
        &[("PATH", &path)],
    );
    let add = |a: &Station, machine: &str, password: Option<&str>| {
        let mut params = json!({"machine": format!("n-{machine}"), "user": "sam"});
        if let Some(p) = password {
            params["password"] = json!(p);
        }
        a.try_call("fleet.add", params)
    };
    // The stage an add comes to rest at, with everything the deck is told.
    let rest_at = |a: &Station, stages: &[&str]| {
        wait_for_or(
            &format!("the add to reach {stages:?}"),
            60,
            || a.call("fleet.add_status", json!({})).to_string(),
            || {
                let v = a.call("fleet.add_status", json!({}));
                let stage = v["stage"].as_str().unwrap_or_default();
                assert!(
                    stages.contains(&stage) || v["running"] == json!(true),
                    "ended somewhere else: {v}"
                );
                stages.contains(&stage).then_some(v)
            },
        )
    };

    // Off until the owner turns it on at this station. Nothing is run.
    assert_eq!(
        a.call("fleet.candidates", json!({}))["enabled"],
        json!(false)
    );
    let err = add(&a, "charlie", Some(WRONG)).unwrap_err();
    assert!(
        err.message.contains("add_machines = true"),
        "{}",
        err.message
    );
    assert!(!at("seen/ssh").exists());
    // The owner turns it on from the tile; an agent cannot, and it holds
    // across a restart.
    let pane = a.fake(json!({"steps": []}), json!({}));
    assert_eq!(
        a.call_as_pane(&pane, "fleet.add_enable", json!({"on": true}))
            .unwrap_err()
            .code,
        "denied"
    );
    assert_eq!(
        a.call("fleet.add_enable", json!({"on": true}))["enabled"],
        json!(true)
    );
    a.kill9();
    a.launch();
    assert_eq!(
        a.call("fleet.candidates", json!({}))["enabled"],
        json!(true)
    );

    // Everything the Control Deck is sent from here on.
    let events = Arc::new(std::sync::Mutex::new(String::new()));
    {
        let (mut c, events) = (a.client().unwrap(), events.clone());
        c.subscribe(None).unwrap();
        std::thread::spawn(move || {
            while let Ok(ev) = c.next_event() {
                events.lock().unwrap().push_str(&ev.to_string());
            }
        });
    }

    // The picker: Macs and Linux machines of the owner's that are online.
    let listed = a.call("fleet.candidates", json!({}));
    let names: Vec<&str> = listed["machines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    // Windows machines are listed since they can be stations (14 Nov 2026).
    assert_eq!(names, ["charlie", "studio", "winbox"], "{listed}");
    assert_eq!(listed["machines"][1]["tailscale_ssh"], json!(true));

    // Only the owner, at this station: not an agent, and not through another station.
    let pane = a.fake(json!({"steps": [{"say": "here"}]}), json!({}));
    for method in ["fleet.add", "fleet.candidates", "fleet.add_cancel"] {
        let err = a
            .call_as_pane(
                &pane,
                method,
                json!({"machine": "n-charlie", "user": "sam"}),
            )
            .unwrap_err();
        assert_eq!(err.code, "denied", "{method}: {}", err.message);
    }
    let err = a
        .try_call(
            "peer.call",
            json!({"station": "bravo", "method": "fleet.add", "params": {"machine": "n-charlie", "user": "sam", "password": WRONG}}),
        )
        .unwrap_err();
    assert!(
        err.message.contains("cannot be forwarded"),
        "{}",
        err.message
    );
    // A caller names a machine from the list, never an address.
    assert!(add(&a, "phone", None).is_err());

    // A machine that does not answer.
    add(&a, "studio", None).unwrap();
    let v = rest_at(&a, &["failed"]);
    assert_eq!(v["problem"]["kind"], json!("unreachable"), "{v}");
    assert!(v["problem"]["text"].as_str().unwrap().contains("asleep"));

    // No key lets this station in, so a password is asked for; a wrong one
    // is said to be wrong.
    add(&a, "charlie", None).unwrap();
    rest_at(&a, &["needs_password"]);
    add(&a, "charlie", Some(WRONG)).unwrap();
    let v = rest_at(&a, &["failed"]);
    assert_eq!(v["problem"]["kind"], json!("wrong_password"), "{v}");

    // The right one: installed from the release page with its checksum
    // checked, started, and asking to join. The code is the one the new
    // station's request shows here; nothing is granted yet.
    add(&a, "charlie", Some(RIGHT)).unwrap();
    let v = rest_at(&a, &["approve"]);
    let said = v["lines"].to_string();
    assert!(said.contains("checksum verified"), "{said}");
    let waiting = a.call("trust.status", json!({}));
    let req = &waiting["requests"][0];
    assert_eq!(
        (
            req["kind"].clone(),
            req["name"].clone(),
            req["code"].clone(),
            req["id"].clone()
        ),
        (
            json!("station"),
            json!("charlie"),
            v["code"].clone(),
            v["request"].clone()
        ),
        "{waiting}"
    );
    assert!(at("charlie/bin/spandeck").exists());

    // The owner approves it with the usual control, and it becomes a station.
    a.call("trust.approve", json!({"request": v["request"]}));
    rest_at(&a, &["done"]);
    let fleet = a.call("fleet.view", json!({}));
    assert!(
        fleet["stations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["station"] == json!("charlie") && s["state"] == json!("online")),
        "{fleet}"
    );
    // A station is no longer a machine to add.
    let listed = a.call("fleet.candidates", json!({}));
    let left: Vec<&str> = listed["machines"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert_eq!(left, ["studio", "winbox"], "{listed}");

    // ---- "I shouldnt have to go to that machine, I should just say add to
    // the fleet", and "I also cant delete the machine then re-add it"
    // (15 Nov 2026). charlie is removed from the fleet and keeps running,
    // outside it. The owner adds it back from alpha: alpha connects over
    // SSH, installs nothing, and has charlie ask; the code is approved here.
    let charlie_state = |a: &Station| a.sees("charlie");
    let listed_machines = |a: &Station| -> Vec<String> {
        a.call("fleet.candidates", json!({}))["machines"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["name"].as_str().map(String::from))
            .collect()
    };
    a.call("fleet.remove", json!({"station": "charlie"}));
    wait_for("charlie to be outside the fleet", 20, || {
        (charlie_state(&a).as_deref() == Some("not_linked")).then_some(())
    });
    // Only the owner, at this station, for both new calls.
    for (method, params) in [
        ("fleet.add", json!({"station": "charlie", "user": "sam"})),
        (
            "fleet.uninstall",
            json!({"station": "charlie", "user": "sam"}),
        ),
    ] {
        let err = a.call_as_pane(&pane, method, params.clone()).unwrap_err();
        assert_eq!(err.code, "denied", "{method}: {}", err.message);
        let err = a
            .try_call(
                "peer.call",
                json!({"station": "charlie", "method": method, "params": params}),
            )
            .unwrap_err();
        assert!(
            err.message.contains("cannot be forwarded"),
            "{method}: {}",
            err.message
        );
    }
    let installs_before = std::fs::read_to_string(at("seen/ssh")).unwrap().len();
    // From the Control Deck: the button on charlie's tile, the user name,
    // then the password it turns out to need.
    let page = common::browser::Page::open(&a, 1200, 900);
    page.goto("/fleet");
    let dialog_says = |what: &str, needle: &str| {
        page.wait(
            what,
            &format!(
                "(document.querySelector('.dialog')?.innerText || '').includes({}) && document.querySelector('.dialog').innerText",
                json!(needle)
            ),
        )
        .as_str()
        .unwrap()
        .to_string()
    };
    let fill = |selector: &str, text: &str| {
        page.wait(
            "a box to type in",
            &format!(
                "(el => el ? (el.value = {}, true) : false)(document.querySelector({}))",
                json!(text),
                json!(selector)
            ),
        );
    };
    let press = |label: &str| {
        page.wait(
            &format!("the {label} button"),
            &format!(
                "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.dialog button')].find(b => b.innerText.trim() === {}))",
                json!(label)
            ),
        );
    };
    const USER: &str = ".dialog input[aria-label='User name on that machine']";
    const PASS: &str = ".dialog input[type=password]";
    page.wait(
        "the button on charlie's tile",
        "(b => b ? (b.click(), true) : false)(document.querySelector('a.tile.station button.add-station'))",
    );
    let said = dialog_says("the add dialog", "nothing is installed on it");
    assert!(said.contains("Add charlie to this fleet"), "{said}");
    fill(USER, "sam");
    press("Connect and ask");
    dialog_says("the password to be asked for", "asks for a password");
    fill(PASS, RIGHT);
    press("Connect and ask");
    dialog_says("charlie's code", "asks to join this fleet as");
    let v = rest_at(&a, &["approve"]);
    // Nothing was downloaded or installed for it.
    assert!(!v["lines"].to_string().contains("Downloading"), "{v}");
    press("Approve");
    dialog_says("charlie to be in the fleet", "is in your fleet");
    press("Done");
    assert_eq!(charlie_state(&a).as_deref(), Some("online"));
    // The station was reached where alpha found it, as the user named.
    let seen = std::fs::read_to_string(at("seen/ssh")).unwrap();
    assert!(
        seen[installs_before..].contains("argv: sam 127.0.0.1"),
        "{}",
        &seen[installs_before..]
    );
    a.call("fleet.add_cancel", json!({}));

    // ---- Uninstall spandeck on charlie, from alpha. A file of the owner's
    // on that machine must outlive all of it.
    std::fs::create_dir_all(at("charlie/home/project")).unwrap();
    std::fs::write(at("charlie/home/project/keep.txt"), "mine").unwrap();
    let service_files = [
        at("charlie/home/.config/systemd/user/spandeckd.service"),
        at("charlie/home/Library/LaunchAgents/dev.spandeck.spandeckd.plist"),
    ];
    assert!(service_files.iter().any(|f| f.exists()));
    let old_fleet = fleet_of(&a);
    // From the station's menu. The dialog names the station, says what
    // happens in order, and opens on Cancel.
    page.right_click(
        "[...document.querySelectorAll('a.tile.station')].find(x => x.innerText.includes('charlie'))",
    );
    page.choose("Uninstall spandeck on charlie");
    let said = dialog_says("the uninstall dialog", "never touched");
    assert!(
        said.contains("Uninstall spandeck on charlie?")
            && said.contains("charlie is removed from the fleet")
            && said.contains("No pane is on charlie.")
            && said.contains("Erase everything"),
        "{said}"
    );
    assert_eq!(page.js("document.activeElement.innerText"), json!("Cancel"));
    assert!(at("charlie/bin/spandeck").exists());
    // No key lets alpha in. charlie is in the fleet, so it is removed from
    // it first, as "Remove" does; then a password is asked for, and nothing
    // has been deleted there.
    fill(USER, "sam");
    press("Uninstall spandeck on charlie");
    dialog_says(
        "the password to be asked for",
        "Nothing has been deleted on charlie yet",
    );
    assert_ne!(fleet_of(&a), old_fleet);
    assert_eq!(rest_at(&a, &["needs_password"])["kind"], json!("uninstall"));
    assert!(at("charlie/bin/spandeck").exists());
    // With the password: stopped, its login service taken out, programs
    // deleted. Its history and settings are kept.
    fill(PASS, RIGHT);
    press("Connect and uninstall");
    dialog_says(
        "the uninstall to finish",
        "listed under Add a machine again",
    );
    press("Done");
    assert!(!at("charlie/bin/spandeck").exists() && !at("charlie/bin/sd").exists());
    assert!(!at("charlie/state/bin").exists());
    assert!(at("charlie/state/spandeck.db").exists() && at("charlie/config/config.toml").exists());
    assert!(!service_files.iter().any(|f| f.exists()));
    // What stopped it starting at login was asked of the stand-ins.
    let tools = std::fs::read_to_string(at("seen/tools")).unwrap();
    assert!(tools.contains("spandeckd"), "{tools}");
    let pid = std::fs::read_to_string(at("charlie/state/spandeckd.pid")).unwrap();
    wait_for("charlie's daemon to be gone", 10, || {
        (!pid_alive(pid.trim())).then_some(())
    });
    // (Nothing is left for the end of the test to stop.)
    let _ = std::fs::remove_file(at("charlie/state/spandeckd.pid"));
    assert_eq!(
        std::fs::read_to_string(at("charlie/home/project/keep.txt")).unwrap(),
        "mine"
    );
    // It is gone from the fleet, and is a machine to add again.
    assert_eq!(charlie_state(&a), None);
    assert!(listed_machines(&a).contains(&"charlie".to_string()));
    let outcomes: Vec<String> = a
        .log("fleet.uninstall")
        .iter()
        .map(|e| {
            assert_eq!(e["actor"], json!("you"), "{e}");
            e["data"]["outcome"].as_str().unwrap().to_string()
        })
        .collect();
    assert!(
        outcomes.contains(&"uninstalled".to_string()),
        "{outcomes:?}"
    );
    a.call("fleet.add_cancel", json!({}));

    // The audit log: who, which machine, which user, how it went.
    let log = a.log("fleet.add");
    let outcomes: Vec<&str> = log
        .iter()
        .filter(|e| e["data"]["machine"] == json!("charlie"))
        .map(|e| {
            assert_eq!(
                (&e["data"]["user"], &e["actor"]),
                (&json!("sam"), &json!("you")),
                "{e}"
            );
            e["data"]["outcome"].as_str().unwrap()
        })
        .collect();
    for want in [
        "started",
        "needs a password",
        "failed: wrong_password",
        "joined",
    ] {
        assert!(outcomes.contains(&want), "{want}: {outcomes:?}");
    }

    // The password guarantee. Neither password is in any file of either
    // station, in what ssh was started with, in the ship's log, or in
    // anything sent to the Control Deck.
    let seen = std::fs::read_to_string(at("seen/ssh")).unwrap();
    assert!(
        seen.contains("SSH_ASKPASS=") && seen.contains("argv: sam 100.64.0.7"),
        "{seen}"
    );
    let sent = events.lock().unwrap().clone();
    assert!(sent.contains("fleet.add.output") && sent.contains("checksum verified"));
    let told = format!(
        "{sent}{}{}{}",
        a.call("log.query", json!({"limit": 2000})),
        a.call("fleet.add_status", json!({})),
        a.call("notify.list", json!({"limit": 100})),
    );
    for secret in [RIGHT, WRONG] {
        assert!(!told.contains(secret), "a password was sent or logged");
        for place in [a.root.as_path(), &at("seen"), &at("charlie")] {
            assert_eq!(
                files_holding(place, secret),
                Vec::<std::path::PathBuf>::new()
            );
        }
    }
}

fn fleet_of(s: &Station) -> String {
    s.call("trust.status", json!({}))["fleet"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Whether a process is running. One that was killed and not yet collected
/// by its new parent (a zombie) is not.
fn pid_alive(pid: &str) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// The owner's request of 15 Nov 2026, "I should be able to delete a machine
/// and it should kill all the sessions that are running, or delete all
/// sessions on a machine". Done when: closing every pane ends agents, shells
/// and what they started, and an agent cannot ask for it; a removed station
/// has no pane left, is refused by the others and refuses them; the stations
/// that stay have a new key and still work with each other; nothing that
/// pointed at the removed station is left in the deck.
#[test]
#[cfg_attr(
    windows,
    ignore = "runs a Unix shell (/bin/sh) and checks a process by its pid"
)]
fn a_removed_station_loses_its_panes_and_the_rest_of_the_fleet_gets_a_new_key() {
    let (a, b, mut c) = Station::three();
    let old_fleet = fleet_of(&a);
    let old_key = std::fs::read_to_string(a.root.join("state/fleet.key")).unwrap();

    // charlie runs an agent and a shell, and the shell started a job of its own.
    let agent = c.fake(
        json!({"steps": [{"prompt": "go"}, {"work": 60000}]}),
        json!({}),
    );
    c.wait_state(&agent, "working");
    let shell = c.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "sh", "cwd": c.root.join("home")}),
    );
    let shell = shell["id"].as_str().unwrap().to_string();
    // Typed only once the shell is at its prompt: on the Mac CI runner the
    // line arrived while the shell was still starting, and was lost.
    wait_for("the shell's prompt", 10, || {
        let text = c.text(&shell);
        (text.contains('$') || text.contains('#')).then_some(())
    });
    c.call(
        "pane.send_text",
        // `$!` then a space: the Mac's /bin/sh reads `!-` as a history event.
        json!({"pane": shell, "text": "sleep 300 & k=$! ; echo job-is-$k-"}),
    );
    let kid = wait_for("the shell's job to start", 10, || {
        let text = c.text(&shell);
        let at = text.rfind("job-is-")? + 7;
        let pid: String = text[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        (!pid.is_empty() && text[at + pid.len()..].starts_with('-')).then_some(pid)
    });
    assert!(pid_alive(&kid));

    // An agent has no way to close a station's panes or to change the fleet.
    for (method, params) in [
        ("station.close_all", json!({})),
        ("station.leave", json!({})),
        ("fleet.remove", json!({"station": "alpha"})),
        (
            "fleet.rekey",
            json!({"key": "00".repeat(32), "removed": "alpha"}),
        ),
    ] {
        let err = c.call_as_pane(&agent, method, params).unwrap_err();
        assert_eq!(err.code, "denied", "{method}");
    }
    // Nor does the owner hand a station a key: only a station of the fleet does.
    let err = a
        .try_call(
            "fleet.rekey",
            json!({"key": "00".repeat(32), "removed": "bravo"}),
        )
        .unwrap_err();
    assert_eq!(err.code, "denied");

    // From alpha's Control Deck: close every pane on charlie.
    let closed = a.call(
        "peer.call",
        json!({"station": "charlie", "method": "station.close_all", "params": {}}),
    );
    assert_eq!(
        (closed["closed"].clone(), closed["working"].clone()),
        (json!(2), json!(1)),
        "{closed}"
    );
    assert_eq!(c.call("pane.list", json!({})), json!([]));
    wait_for_or(
        "the shell's own job to end with its pane",
        10,
        || {
            let out = std::process::Command::new("ps")
                .args(["-o", "pid,ppid,pgid,sess,tty,stat,args", "-p", &kid])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        },
        || (!pid_alive(&kid)).then_some(()),
    );
    let said = c.log("station.closed_all");
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(said[0]["data"]["panes"], json!(2));
    assert!(said[0]["actor"].as_str().unwrap().contains("via alpha"));

    // charlie gets a new pane, and the owner pins it: both must go.
    let later = c.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "later", "cwd": c.root.join("home")}),
    );
    let pin = json!({"kind": "pane", "station": "charlie", "id": later["id"]});
    let kept = json!({"kind": "pane", "station": "bravo", "id": "p-kept"});
    a.call("deck.pin", json!({"pin": pin}));
    a.call("deck.pin", json!({"pin": kept}));
    // And gives it a look of its own, which must go too (bravo's stays).
    let own = json!({"set": {"font.size": 15}});
    for key in [
        format!("charlie/{}", later["id"].as_str().unwrap()),
        "bravo/p-kept".to_string(),
    ] {
        a.call(
            "look.set",
            json!({"level": "pane", "key": key, "choice": own}),
        );
    }

    let out = a.call("fleet.remove", json!({"station": "charlie"}));
    assert_eq!(
        (
            out["reached"].clone(),
            out["rekeyed"].clone(),
            out["waiting"].clone()
        ),
        (json!(true), json!(["bravo"]), json!([])),
        "{out}"
    );
    assert_eq!(out["there"]["closed"], json!(1), "{out}");
    assert_eq!(c.call("pane.list", json!({})), json!([]));

    // The stations that stay share a new key; charlie has one of its own.
    let new_fleet = fleet_of(&a);
    assert_ne!(new_fleet, old_fleet);
    assert_eq!(fleet_of(&b), new_fleet);
    assert!(![old_fleet.clone(), new_fleet.clone()].contains(&fleet_of(&c)));
    for (s, other) in [(&a, "bravo"), (&b, "alpha")] {
        wait_for("the stations that stay to see each other", 15, || {
            (s.sees(other).as_deref() == Some("online")).then_some(())
        });
        let panes = s.call(
            "peer.call",
            json!({"station": other, "method": "pane.list", "params": {}}),
        );
        assert!(panes.is_array(), "{panes}");
        // To them charlie is a machine that is not in the fleet.
        assert_eq!(s.sees("charlie").as_deref(), Some("not_linked"));
        let err = s
            .try_call(
                "peer.call",
                json!({"station": "charlie", "method": "pane.list", "params": {}}),
            )
            .unwrap_err();
        assert!(err.message.contains("not in this fleet"), "{}", err.message);
    }
    // And charlie can do nothing to them.
    for other in ["alpha", "bravo"] {
        assert_eq!(c.sees(other).as_deref(), Some("not_linked"));
        let err = c
            .try_call(
                "peer.call",
                json!({"station": other, "method": "pane.list", "params": {}}),
            )
            .unwrap_err();
        assert!(err.message.contains("not in this fleet"), "{}", err.message);
    }
    // The pin that pointed at charlie is gone on every station that stays.
    for s in [&a, &b] {
        wait_for("the deck to forget charlie", 15, || {
            (s.call("deck.get", json!({}))["pins"] == json!([kept])).then_some(())
        });
        wait_for("charlie's pane's own look to be forgotten", 15, || {
            (s.call("look.get", json!({}))["panes"] == json!({"bravo/p-kept": own})).then_some(())
        });
    }
    // Each station's log says what happened and who asked; no key is shown.
    assert_eq!(
        a.log("fleet.removed")[0]["data"]["station"],
        json!("charlie")
    );
    assert_eq!(
        b.log("fleet.rekeyed")[0]["data"]["removed"],
        json!("charlie")
    );
    assert!(c.log("station.left")[0]["actor"]
        .as_str()
        .unwrap()
        .contains("via alpha"));
    let new_key = std::fs::read_to_string(a.root.join("state/fleet.key")).unwrap();
    assert_ne!(new_key.trim(), old_key.trim());
    for s in [&a, &b, &c] {
        let said = s.call("log.query", json!({"limit": 500})).to_string();
        assert!(!said.contains(new_key.trim()) && !said.contains(old_key.trim()));
    }

    // "Also stop spandeck there": the daemon ends by itself.
    let left = c.call("station.leave", json!({"stop": true}));
    assert_eq!(left["stopping"], json!(true), "{left}");
    wait_for("charlie's daemon to stop", 15, || {
        c.child.as_mut().unwrap().try_wait().ok().flatten()
    });
    c.child = None;
}

/// Guarantee (Trust), for a station that cannot be told it was removed: it
/// keeps the old key, so the fleet must stop accepting that key. Done when:
/// the removed station's signed call is refused and it sees the others as
/// not linked, its panes are still running, and a station that was away at
/// that moment is handed the new key when it is back, without a new approval.
#[test]
#[cfg_attr(windows, ignore = "freezes a daemon with a signal")]
fn an_unreachable_station_is_removed_from_the_fleets_side() {
    let (a, b, c) = Station::three();
    let pane = c.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "stays", "cwd": c.root.join("home")}),
    );
    // charlie is the one to remove; bravo is merely away.
    b.signal(SIGSTOP);
    c.signal(SIGSTOP);
    wait_for("alpha to lose sight of both", 20, || {
        (a.sees("bravo").as_deref() == Some("unreachable")
            && a.sees("charlie").as_deref() == Some("unreachable"))
        .then_some(())
    });
    let old_fleet = fleet_of(&a);
    let out = a.call("fleet.remove", json!({"station": "charlie", "stop": true}));
    assert_eq!(
        (
            out["reached"].clone(),
            out["rekeyed"].clone(),
            out["waiting"].clone()
        ),
        (json!(false), json!([]), json!(["bravo"])),
        "{out}"
    );
    assert_ne!(fleet_of(&a), old_fleet);
    // Forgotten at once: it is not kept as a station that is merely off.
    assert_eq!(
        a.call("fleet.view", json!({}))["stations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    b.signal(SIGCONT);
    c.signal(SIGCONT);
    // charlie still holds the key the fleet used to accept, and signs with it.
    assert_eq!(fleet_of(&c), old_fleet);
    let err = c
        .try_call(
            "peer.call",
            json!({"station": "alpha", "method": "pane.list", "params": {}}),
        )
        .unwrap_err();
    assert!(err.message.contains("not in this fleet"), "{}", err.message);
    assert_eq!(c.sees("alpha").as_deref(), Some("not_linked"));
    // Nothing was done on charlie: its pane runs on.
    assert_eq!(c.pane(pane["id"].as_str().unwrap())["exited"], json!(false));

    // bravo was only away. alpha hands it the new key when it sees it again.
    wait_for("bravo to be handed the new key", 20, || {
        (a.sees("bravo").as_deref() == Some("online")).then_some(())
    });
    assert_eq!(fleet_of(&b), fleet_of(&a));
    assert_eq!(
        b.log("fleet.rekeyed")[0]["data"]["removed"],
        json!("charlie")
    );
    wait_for("bravo to see alpha in the fleet", 15, || {
        (b.sees("alpha").as_deref() == Some("online")).then_some(())
    });
    assert!(a
        .call(
            "peer.call",
            json!({"station": "bravo", "method": "pane.list", "params": {}}),
        )
        .is_array());
    // And then charlie is outside for bravo too.
    assert_eq!(b.sees("charlie").as_deref(), Some("not_linked"));
    let err = c
        .try_call(
            "peer.call",
            json!({"station": "bravo", "method": "pane.list", "params": {}}),
        )
        .unwrap_err();
    assert!(err.message.contains("not in this fleet"), "{}", err.message);
}

fn reaches(from: &Station, to: &str) -> bool {
    from.try_call(
        "peer.call",
        json!({"station": to, "method": "pane.list", "params": {}}),
    )
    .is_ok_and(|v| v.is_array())
}

/// A bug that happened (15 Nov 2026): a station that had two fleet-mates
/// joined a lone station, took its key and left its mates behind, so the
/// owner had two fleets. "Should never have multiple fleets": a station that
/// joins brings the stations of its fleet with it, one that is on at once
/// and one that was off when it is next seen.
#[test]
#[cfg_attr(windows, ignore = "freezes a daemon with a signal")]
fn a_station_that_joins_another_fleet_brings_its_fleet_mates_with_it() {
    let key = "5a".repeat(32);
    let all = Station::several(&[
        ("alpha", Some(&key)),
        ("bravo", Some(&key)),
        ("delta", Some(&key)),
        ("charlie", None),
    ]);
    let (a, b, d, c) = (&all[0], &all[1], &all[2], &all[3]);
    wait_for("alpha to see its mates and charlie", 20, || {
        (a.sees("bravo").as_deref() == Some("online")
            && a.sees("delta").as_deref() == Some("online")
            && a.sees("charlie").as_deref() == Some("not_linked"))
        .then_some(())
    });
    let old_fleet = fleet_of(a);
    // Two browsers are signed in to charlie's fleet, and one is signed out
    // again. A station that arrives must refuse that one too.
    let web = |s: &Station, path: &str, token: Option<&str>, body: Value| -> (u16, Value) {
        let url = s.url(path);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut req = reqwest::Client::new().post(url).json(&body);
            if let Some(t) = token {
                req = req.bearer_auth(t);
            }
            let r = req.send().await.unwrap();
            (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
        })
    };
    let sign_in = |name: &str| -> (String, String) {
        let asked = web(c, "/api/device/request", None, json!({"name": name})).1["result"].clone();
        c.call("trust.approve", json!({"request": asked["id"]}));
        let got = web(
            c,
            "/api/device/collect",
            None,
            json!({"id": asked["id"], "collect": asked["collect"]}),
        );
        (
            asked["id"].as_str().unwrap().to_string(),
            got.1["result"]["token"]
                .as_str()
                .expect("a token")
                .to_string(),
        )
    };
    let lets_in = |s: &Station, token: &str| {
        web(
            s,
            "/api/rpc",
            Some(token),
            json!({"method": "pane.list", "params": {}}),
        )
        .0
    };
    let (_, kept_token) = sign_in("laptop");
    let (gone, gone_token) = sign_in("lost phone");
    c.call("device.revoke", json!({"device": gone}));
    assert_eq!(
        (lets_in(c, &kept_token), lets_in(c, &gone_token)),
        (200, 401)
    );
    // delta is off when it happens.
    d.signal(SIGSTOP);

    // alpha asks to join charlie, as `spandeck fleet --join charlie` does.
    // The request names the mates it brings.
    let asked = a.call("fleet.join", json!({"station": "charlie"}));
    assert_eq!(asked["with"], json!(["bravo", "delta"]), "{asked}");
    assert!(
        asked["next"]
            .as_str()
            .unwrap()
            .contains("Browsers signed in to those 3 stations"),
        "{asked}"
    );
    let waiting = c.call("trust.status", json!({}));
    assert_eq!(
        waiting["requests"][0]["with"],
        json!(["bravo", "delta"]),
        "{waiting}"
    );
    let notes = c.call("notify.list", json!({})).to_string();
    assert!(
        notes.contains("Station alpha and 2 other stations (bravo, delta) ask to join this fleet"),
        "{notes}"
    );
    c.call("trust.approve", json!({"request": asked["code"]}));

    // One approval moves alpha and, at once, bravo, which was on.
    wait_for("alpha and bravo to hold charlie's key", 20, || {
        (fleet_of(a) == fleet_of(c) && fleet_of(b) == fleet_of(c)).then_some(())
    });
    assert_ne!(fleet_of(c), old_fleet);
    assert_eq!(
        b.log("fleet.rekeyed")[0]["data"]["joined"],
        json!("charlie")
    );
    // The log says who asked, and what came with it.
    assert_eq!(a.log("trust.join_asked")[0]["actor"], json!("you"));
    let joined = &a.log("trust.joined")[0];
    assert_eq!(
        (joined["actor"].clone(), joined["data"]["with"].clone()),
        (json!("you"), json!(["bravo", "delta"]))
    );

    // delta was off. It comes back still holding the old key, and is handed
    // the new one with no second approval.
    assert_eq!(a.sees("delta").as_deref(), Some("unreachable"));
    d.signal(SIGCONT);
    wait_for("delta to be handed the key", 20, || {
        a.call("fleet.refresh", json!({}));
        (fleet_of(d) == fleet_of(c)).then_some(())
    });
    assert_eq!(
        d.log("fleet.rekeyed")[0]["data"]["joined"],
        json!("charlie")
    );
    // One fleet of four: each station sees and can call the others.
    wait_for("the four stations to be one fleet", 20, || {
        all.iter()
            .all(|s| {
                all.iter()
                    .all(|o| o.name == s.name || s.sees(&o.name).as_deref() == Some("online"))
            })
            .then_some(())
    });
    assert!(reaches(c, "bravo") && reaches(c, "delta") && reaches(a, "bravo"));
    assert!(reaches(b, "charlie") && reaches(d, "charlie") && reaches(d, "alpha"));
    // A gap found on 15 Nov 2026: the stations that arrive accept the
    // browsers of the fleet they joined, and must refuse the one that was
    // signed out there: the asker, its mate, and the one that was off.
    for s in [a, b, d] {
        assert_eq!(
            (lets_in(s, &kept_token), lets_in(s, &gone_token)),
            (200, 401),
            "{}",
            s.name
        );
    }
    // And each lists what charlie lists: the one browser still signed in
    // to the fleet they joined, approved at charlie.
    let devices = |s: &Station| s.call("trust.status", json!({}))["devices"].clone();
    wait_for(
        "the stations that arrived to list charlie's browser",
        15,
        || {
            let at_c = devices(c);
            (at_c.as_array().unwrap().len() == 1 && [a, b, d].iter().all(|s| devices(s) == at_c))
                .then_some(())
        },
    );
    assert_eq!(devices(d)[0]["at"], json!("charlie"));
    assert!(c.call("trust.status", json!({}))["requests"]
        .as_array()
        .unwrap()
        .is_empty());
    // No key is shown anywhere.
    let new_key = std::fs::read_to_string(c.root.join("state/fleet.key")).unwrap();
    for s in &all {
        let said = s.call("log.query", json!({"limit": 500})).to_string();
        assert!(!said.contains(new_key.trim()) && !said.contains(&key));
    }
}

/// The owner's requirement (15 Nov 2026): "Shouldn't all machines get the
/// request? ... you should be able to approve from any machine you're on at
/// the time". A request made at one station is listed at its fleet-mate,
/// with a notice, and answered there. Guarantee (Trust): an agent never gets
/// to answer one, wherever it waits.
#[test]
fn a_request_is_seen_and_answered_at_any_station_of_the_fleet() {
    let key = "5a".repeat(32);
    let mut all = Station::several(&[
        ("alpha", Some(&key)),
        ("bravo", Some(&key)),
        ("charlie", None),
    ])
    .into_iter();
    let (sa, sb, mut sc) = (
        all.next().unwrap(),
        all.next().unwrap(),
        all.next().unwrap(),
    );
    let (a, b, c) = (&sa, &sb, &sc);
    wait_for("the stations to find each other", 20, || {
        (a.sees("bravo").as_deref() == Some("online")
            && b.sees("alpha").as_deref() == Some("online")
            && c.sees("alpha").as_deref() == Some("not_linked"))
        .then_some(())
    });
    let relayed = |s: &Station| {
        s.call("trust.status", json!({}))["requests"]
            .as_array()
            .unwrap()
            .first()
            .cloned()
    };

    // charlie asks alpha. bravo, alpha's mate, lists the request and says
    // where it was asked; what it was told holds nothing to collect with.
    let asked = c.call("fleet.join", json!({"station": "alpha"}));
    let seen = wait_for("bravo to list alpha's request", 15, || relayed(b));
    assert_eq!(
        (
            seen["at"].clone(),
            seen["kind"].clone(),
            seen["name"].clone(),
            seen["code"].clone()
        ),
        (
            json!("alpha"),
            json!("station"),
            json!("charlie"),
            asked["code"].clone()
        ),
        "{seen}"
    );
    assert!(seen.get("collect").is_none() && seen.get("key").is_none());
    let notes = b.call("notify.list", json!({})).to_string();
    assert!(
        notes.contains("Station charlie asks to join this fleet")
            && notes.contains("Asked at alpha"),
        "{notes}"
    );

    // An agent on bravo cannot answer it, by code or by id, and cannot say
    // that a request waits somewhere.
    let pane = b.fake(
        json!({"steps": [{"prompt": "go"}, {"work": 60000}]}),
        json!({}),
    );
    let pane = pane.as_str();
    for (method, params) in [
        ("trust.approve", json!({"request": asked["code"]})),
        ("trust.approve", json!({"request": seen["id"]})),
        ("trust.deny", json!({"request": asked["code"]})),
        ("trust.relay", json!({"requests": []})),
        (
            "peer.call",
            json!({"station": "alpha", "method": "trust.approve", "params": {"request": asked["code"]}}),
        ),
    ] {
        let err = b.call_as_pane(pane, method, params).unwrap_err();
        assert_eq!(err.code, "denied", "{method}");
    }
    // Nor does the owner's own call stand in for a station's.
    let err = b
        .try_call("trust.relay", json!({"requests": []}))
        .unwrap_err();
    assert_eq!(err.code, "denied");
    assert!(relayed(a).is_some() && relayed(b).is_some());
    assert_eq!(c.sees("alpha").as_deref(), Some("not_linked"));

    // The owner approves at bravo. alpha decides, hands charlie the key, and
    // its log says who decided and through which station.
    // bravo's Control Deck says where it was asked, and its button answers.
    let page = common::browser::Page::open(b, 1200, 800);
    page.wait(
        "the Access button to show a request",
        "!!document.querySelector('.topbar button.attn')",
    );
    page.js("document.querySelector('.topbar button.attn').click()");
    let shown = page.wait(
        "the request",
        "document.querySelector('.dialog .note')?.innerText",
    );
    let shown = shown.as_str().unwrap();
    assert!(
        shown.contains("Station charlie")
            && shown.contains("asked at alpha")
            && shown.contains(asked["code"].as_str().unwrap()),
        "{shown}"
    );
    page.js("[...document.querySelectorAll('.dialog .note button')].find(x => x.innerText === 'Approve').click()");
    wait_for("charlie to be let in", 20, || {
        (c.sees("alpha").as_deref() == Some("online")
            && c.sees("bravo").as_deref() == Some("online"))
        .then_some(())
    });
    assert_eq!(fleet_of(c), fleet_of(b));
    let approved = &a.log("trust.approved")[0];
    assert_eq!(
        approved["data"]["by"],
        json!("you (via bravo)"),
        "{approved}"
    );
    assert_eq!(b.log("trust.passed_on")[0]["actor"], json!("you"));
    // It is cleared on both, and a second answer finds nothing to decide.
    wait_for("the request to be cleared everywhere", 15, || {
        (relayed(a).is_none() && relayed(b).is_none()).then_some(())
    });
    let err = b
        .try_call("trust.deny", json!({"request": asked["code"]}))
        .unwrap_err();
    assert_eq!(err.code, "not_found", "{}", err.message);

    let post = |path: &str, body: Value| -> Value {
        let url = a.url(path);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let r = reqwest::Client::new().post(url).json(&body).send();
            r.await.unwrap().json().await.unwrap()
        })
    };
    // Names in a request come from another machine (rule 6: they are
    // data). Markup and a long list are cut down before anything is shown,
    // here and at the mate.
    let hostile: Vec<String> = (0..30).map(|i| format!("<img src=x>{i}")).collect();
    post(
        "/api/peer/rpc",
        json!({"method": "fleet.join.request", "from": "x", "params": {"station": "<script>evil", "with": hostile}}),
    );
    let seen = wait_for("bravo to list the hostile request", 15, || relayed(b));
    for req in [&seen, &relayed(a).unwrap()] {
        assert_eq!(req["name"], json!("scriptevil"), "{req}");
        assert_eq!(req["with"].as_array().unwrap().len(), 20);
        assert!(!req.to_string().contains('<'), "{req}");
    }
    assert!(!b.call("notify.list", json!({})).to_string().contains('<'));
    a.call("trust.deny", json!({"request": seen["id"]}));
    wait_for("bravo to stop listing it", 15, || {
        relayed(b).is_none().then_some(())
    });

    // A browser's request is relayed the same way, and refusing it at the
    // mate reaches the station that holds it.
    let asked = post("/api/device/request", json!({"name": "a phone"}));
    let asked = &asked["result"];
    let seen = wait_for("bravo to list the browser's request", 15, || relayed(b));
    assert_eq!(
        (seen["kind"].clone(), seen["at"].clone()),
        (json!("device"), json!("alpha"))
    );
    b.call("trust.deny", json!({"request": seen["id"]}));
    let answer = post(
        "/api/device/collect",
        json!({"id": asked["id"], "collect": asked["collect"]}),
    );
    assert_eq!(answer["result"]["status"], json!("denied"), "{answer}");

    // ---- The owner (15 Nov 2026): "no matter where I go, it's all the
    // same view ... these must stay the same no matter which machine I'm
    // on". Measured on the real fleet: one station listed six signed-in
    // browsers and another station of the same fleet none. The browsers
    // signed in are one list for the fleet, the same on every station.
    let sign_in = |name: &str| -> (String, String) {
        let asked = post("/api/device/request", json!({"name": name}))["result"].clone();
        a.call("trust.approve", json!({"request": asked["id"]}));
        let got = post(
            "/api/device/collect",
            json!({"id": asked["id"], "collect": asked["collect"]}),
        );
        (
            asked["id"].as_str().unwrap().to_string(),
            got["result"]["token"]
                .as_str()
                .expect("a token")
                .to_string(),
        )
    };
    let lets_in = |s: &Station, token: &str| -> u16 {
        let url = s.url("/api/rpc");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let req = reqwest::Client::new()
                .post(url)
                .bearer_auth(token)
                .json(&json!({"method": "pane.list", "params": {}}));
            req.send().await.unwrap().status().as_u16()
        })
    };
    let devices = |s: &Station| s.call("trust.status", json!({}))["devices"].clone();
    let (phone, phone_token) = sign_in("a phone");
    let (_, laptop_token) = sign_in("a laptop");
    let listed = wait_for("every station to list both browsers", 15, || {
        let at_a = devices(a);
        (at_a.as_array().unwrap().len() == 2 && devices(b) == at_a && devices(c) == at_a)
            .then_some(at_a)
    });
    // Each says where it was approved.
    assert_eq!(
        (listed[0]["name"].clone(), listed[0]["at"].clone()),
        (json!("a phone"), json!("alpha")),
        "{listed}"
    );
    assert_eq!(lets_in(c, &phone_token), 200);

    // A sign-out reaches every station, also one that was off when it was
    // made. (Before, it was sent once to the stations that were on, so a
    // station that was off kept accepting the token.)
    sc.kill9();
    b.call("device.revoke", json!({"device": phone}));
    assert_eq!(
        (lets_in(a, &phone_token), lets_in(b, &phone_token)),
        (401, 401)
    );
    sc.launch();
    let c = &sc;
    wait_for(
        "charlie to hear of the sign-out once it is back",
        20,
        || (lets_in(c, &phone_token) == 401).then_some(()),
    );
    let left = wait_for("every station to list one browser", 15, || {
        let at_a = devices(a);
        (at_a.as_array().unwrap().len() == 1 && devices(b) == at_a && devices(c) == at_a)
            .then_some(at_a)
    });
    assert_eq!(left[0]["name"], json!("a laptop"), "{left}");
    assert_eq!(lets_in(c, &laptop_token), 200);

    // The owner goes from one station's Control Deck to another's without a
    // second approval: the deck this browser is signed in to opens the
    // other deck and hands it the browser's own token as a window message.
    // The token is never in an address, so it is in no history.
    // (This browser was at bravo's deck with bravo's own token: it is
    // taken away, so that bravo's deck has no sign-in for this browser.)
    page.js("(localStorage.removeItem('spandeck.token'), true)");
    page.visit(a);
    page.js(&format!(
        "(localStorage.setItem('spandeck.token', {}), location.reload(), true)",
        json!(laptop_token)
    ));
    page.wait(
        "alpha's deck, signed in as the laptop",
        &format!(
            "localStorage.getItem('spandeck.token') === {} && !!document.querySelector('.topbar')",
            json!(laptop_token)
        ),
    );
    page.goto("/fleet");
    page.js("(real => { window.open = (url, ...rest) => { window.__opened = url; return real.call(window, url, ...rest); }; return true; })(window.open)");
    page.right_click(
        "[...document.querySelectorAll('a.tile.station')].find(x => x.innerText.includes('bravo'))",
    );
    page.choose("Open bravo's own Control Deck");
    assert_eq!(
        page.js("window.__opened"),
        json!(format!("http://127.0.0.1:{}/#/fleet", b.port))
    );
    let there = page.opened(&format!(":{}/", b.port));
    there.wait(
        "bravo's deck, signed in with no new approval",
        &format!(
            "location.port === '{}' && !!document.querySelector('.topbar') && localStorage.getItem('spandeck.token') === {}",
            b.port,
            json!(laptop_token)
        ),
    );
    // Every address either window was at, or came from.
    for window in [&page, &there] {
        let visited = window
            .js("[location.href, document.referrer, ...performance.getEntriesByType('navigation').map(e => e.name)].join(' ')")
            .to_string();
        assert!(
            !visited.contains("dev.") && !visited.contains("token"),
            "{visited}"
        );
    }
    assert!(relayed(b).is_none());
    assert_eq!(devices(b), left);
    // A window that was not opened by the deck is told nothing, and the
    // window that was is answered once.
    assert_eq!(
        page.js("new Promise(done => { let got = 0; addEventListener('message', e => { if (e.data && e.data.kind) got++; }); window.postMessage('spandeck:hop-ready', '*'); setTimeout(() => done(got), 300); })"),
        json!(0)
    );
    assert_eq!(
        there.js("new Promise(done => { let got = 0; addEventListener('message', e => { if (e.data && e.data.kind) got++; }); window.opener.postMessage('spandeck:hop-ready', '*'); setTimeout(() => done(got), 300); })"),
        json!(0)
    );
    // The station's own token is never handed on: only a browser's.
    page.js(&format!(
        "(localStorage.setItem('spandeck.token', {}), location.reload(), true)",
        json!(a.token())
    ));
    page.wait(
        "alpha's deck, with alpha's own token",
        &format!(
            "localStorage.getItem('spandeck.token') === {} && !!document.querySelector('a.tile.station')",
            json!(a.token())
        ),
    );
    page.right_click(
        "[...document.querySelectorAll('a.tile.station')].find(x => x.innerText.includes('bravo'))",
    );
    assert_eq!(
        page.js("[...document.querySelectorAll('.menu-item')].some(i => i.innerText.includes('own Control Deck'))"),
        json!(false)
    );
}

// ----------------------------------------- performance page, Tailscale keys

const DAY_MS: u64 = 86_400_000;

/// A `tailscale` that prints a fixed tailnet for one station: itself, and
/// the other devices given. The station is started with discovery on and
/// this first on its PATH, so the real Tailscale client is never asked.
fn standin_tailnet(
    root: &std::path::Path,
    me: Value,
    peers: &[Value],
) -> (String, Vec<(String, String)>) {
    let dir = root.join("standin");
    std::fs::create_dir_all(&dir).unwrap();
    let peers: serde_json::Map<String, Value> = peers
        .iter()
        .map(|p| (format!("key-{}", p["ID"].as_str().unwrap()), p.clone()))
        .collect();
    std::fs::write(
        dir.join("status.json"),
        json!({"BackendState": "Running", "Self": me, "User": {"7": {"LoginName": "owner@example.com"}}, "Peer": peers})
            .to_string(),
    )
    .unwrap();
    executable(
        &dir.join("tailscale"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in version) echo 1.0 ;; status) cat {} ;; *) exit 1 ;; esac\n",
            dir.join("status.json").display()
        ),
    );
    let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap());
    (
        "discovery = true\n".to_string(),
        vec![("PATH".to_string(), path)],
    )
}

/// One of the owner's devices, as `tailscale status --json` lists it.
fn device(name: &str, os: &str, expires_in_days: Option<i64>, more: Value) -> Value {
    let now = spandeck_core::paths::now_ms();
    let mut v = json!({"ID": format!("n-{name}"), "HostName": name, "DNSName": format!("{name}.tail0000.ts.net."),
        "OS": os, "UserID": 7, "Online": false, "TailscaleIPs": ["127.0.0.1"]});
    if let Some(days) = expires_in_days {
        // Half a day inside, so that the count of whole days does not turn
        // over while the test runs.
        let at = now.saturating_add_signed(days * DAY_MS as i64 + DAY_MS as i64 / 2);
        v["KeyExpiry"] = json!(spandeck_core::tailnet::rfc3339(at));
        v["Expired"] = json!(days < 0);
    }
    for (k, x) in more.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

fn notices(s: &Station, holding: &str) -> Vec<Value> {
    s.call("notify.list", json!({"limit": 100}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["title"].as_str().unwrap().contains(holding))
        .cloned()
        .collect()
}

/// The owner, 15 Nov 2026: "Control deck should have a performance page that
/// you can see across the fleet", and "a Tailscale token expiring and lost
/// access to the machine, how can we expose this". Each station answers
/// `perf.get` for itself, to the owner and to another station and to no
/// agent; a key that expires in 6 days is warned of once for the fleet, in
/// the same words by both stations, and pushed by one; none of the owner's
/// devices is in anything an agent can read.
#[test]
#[cfg_attr(
    windows,
    ignore = "the stand-in for the tailscale command is a sh script"
)]
fn each_station_reports_its_numbers_and_a_key_near_expiry_is_warned_of_once() {
    let (url, hits) = webhook();
    let fleet = Station::fleet(&["alpha", "bravo"], |name, root| {
        let me = |n: &str, days| device(n, "linux", days, json!({"Online": true}));
        let (extra, env) = if name == "alpha" {
            standin_tailnet(
                root,
                me("alpha", None),
                &[
                    device("bravo-ts", "linux", Some(6), json!({"Online": true})),
                    device(
                        "pocket-phone",
                        "iOS",
                        Some(2),
                        json!({"TailscaleIPs": ["100.64.0.9"]}),
                    ),
                    device(
                        "old-box",
                        "linux",
                        Some(-147),
                        json!({"TailscaleIPs": ["100.64.0.10"]}),
                    ),
                ],
            )
        } else {
            standin_tailnet(
                root,
                me("bravo-ts", Some(6)),
                &[device("alpha", "linux", None, json!({"Online": true}))],
            )
        };
        (
            format!("{extra}at_keyboard_secs = 0\n[notify]\nwebhook_url = \"{url}\"\n"),
            env,
        )
    });
    let (a, b) = (&fleet[0], &fleet[1]);
    let title = "Tailscale key of bravo (bravo-ts) expires within 7 days";
    // Both raise it, in the same words (so the deck shows one entry), and
    // only alpha, the first by name, sends it on.
    for s in [a, b] {
        wait_for_or(
            "the warning",
            30,
            || {
                s.call("notify.list", json!({})).to_string()
                    + &s.call("tailnet.view", json!({})).to_string()
            },
            || {
                s.call("fleet.refresh", json!({}));
                (notices(s, "Tailscale key").len() == 1).then_some(())
            },
        );
    }
    let (at_a, at_b) = (notices(a, "Tailscale key"), notices(b, "Tailscale key"));
    assert_eq!(
        (at_a[0]["title"].as_str(), at_b[0]["title"].as_str()),
        (Some(title), Some(title))
    );
    assert_eq!(at_a[0]["body"], at_b[0]["body"]);
    assert!(
        at_a[0]["body"]
            .as_str()
            .unwrap()
            .contains("Disable key expiry"),
        "{}",
        at_a[0]
    );
    assert_eq!(
        at_b[0]["pushed"],
        json!(3),
        "bravo leaves the push to alpha"
    );
    wait_for("alpha's push", 20, || {
        (hits.load(Ordering::SeqCst) == 1).then_some(())
    });

    // What alpha answers for itself: its requests, its link to bravo, the
    // keys, and the owner's other devices that are near expiry (the phone:
    // 2 days; not the box that expired five months ago).
    let perf = a.call("perf.get", json!({}));
    assert!(
        perf["requests"]["handled"]["local"]["count"].as_u64() > Some(0),
        "{perf}"
    );
    assert!(
        perf["periodic"]["work"]["count"].as_u64() > Some(0),
        "{perf}"
    );
    assert!(perf["look"]["count"].as_u64() > Some(0), "{perf}");
    assert_eq!(
        perf["restarts"]["list"][0]["what"],
        json!("started"),
        "{perf}"
    );
    let link = &perf["links"][0];
    assert_eq!(
        (&link["station"], &link["state"]),
        (&json!("bravo"), &json!("online")),
        "{perf}"
    );
    assert!(link["rtt"]["count"].as_u64() > Some(0), "{perf}");
    assert_eq!(link["clock"]["refused"], json!(false), "{perf}");
    let key = &perf["tailnet"]["stations"]["bravo"];
    assert_eq!(
        (&key["source"], &key["id"]),
        (&json!("tailscale"), &json!("n-bravo-ts")),
        "{perf}"
    );
    assert_eq!(
        perf["tailnet"]["stations"]["alpha"]["expires_ms"],
        Value::Null,
        "{perf}"
    );
    let near: Vec<&str> = perf["tailnet"]["devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(near, ["pocket-phone"], "{perf}");
    // The phone never opened a deck, so no notice is raised for it.
    assert!(notices(a, "pocket-phone").is_empty());
    // The same from the other station, through this one, as the page asks.
    let there = a.call(
        "peer.call",
        json!({"station": "bravo", "method": "perf.get", "params": {}}),
    );
    assert_eq!(there["station"], json!("bravo"));
    assert_eq!(
        there["tailnet"]["stations"]["bravo"]["id"],
        json!("n-bravo-ts"),
        "{there}"
    );
    assert!(
        there["requests"]["handled"]["station"]["count"].as_u64() > Some(0),
        "{there}"
    );
    // `spandeck doctor` says the key of the machine it is run on.
    let checks = b.call("station.doctor", json!({}));
    let check = checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == json!("tailscale key"))
        .unwrap()
        .clone();
    assert_eq!(check["ok"], json!(false), "{check}");
    assert!(
        check["detail"]
            .as_str()
            .unwrap()
            .contains("expires in 6 days"),
        "{check}"
    );

    // Rule 7: no agent reads any of it, and what an agent can read
    // (`station.info`) names none of the owner's devices or their keys.
    let pane = a.fake(json!({"steps": []}), json!({}));
    for method in ["perf.get", "tailnet.view", "tailnet.report", "fleet.view"] {
        let err = a.call_as_pane(&pane, method, json!({})).unwrap_err();
        assert_eq!(err.code, "denied", "{method}: {}", err.message);
    }
    let info = a
        .call_as_pane(&pane, "station.info", json!({}))
        .unwrap()
        .to_string();
    for word in [
        "pocket-phone",
        "old-box",
        "bravo-ts",
        "n-bravo",
        "expires",
        "tailnet",
        "100.64.0.9",
    ] {
        assert!(!info.contains(word), "station.info holds {word:?}: {info}");
    }
    // Nor does the fleet view, which the read-only list has.
    let view = a.call("fleet.view", json!({})).to_string();
    assert!(
        !view.contains("pocket-phone") && !view.contains("n-bravo"),
        "{view}"
    );

    // A restart does not say it again: what was said is in the database.
    let mut fleet = fleet;
    fleet[0].kill9();
    fleet[0].launch();
    let a = &fleet[0];
    wait_for("alpha to see bravo's key again", 30, || {
        a.call("fleet.refresh", json!({}));
        (a.call("tailnet.view", json!({}))["stations"]["bravo"]["source"] == json!("tailscale"))
            .then_some(())
    });
    a.call("fleet.refresh", json!({}));
    assert_eq!(notices(a, "Tailscale key").len(), 1);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // It ended without being stopped, and says so.
    let restarts = a.call("perf.get", json!({}))["restarts"].clone();
    assert!(
        restarts["list"][0]["why"]
            .as_str()
            .unwrap()
            .contains("without being stopped"),
        "{restarts}"
    );
}

/// Audit, section 10: two stations whose clocks are five minutes apart
/// listed each other as "unreachable", a direct call said "not in this
/// fleet: join it first", and nothing anywhere said "clock". The calls are
/// still refused; the fleet view, the Performance page's link, a call's
/// error and `spandeck doctor` now say why.
#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "moves a station's clock, which only a debug build allows"
)]
fn stations_whose_clocks_differ_say_so_and_still_refuse_each_other() {
    let fleet = Station::fleet(&["alpha", "bravo"], |name, _| {
        let env = if name == "bravo" {
            vec![("SPANDECK_TEST_CLOCK_MS".to_string(), "300000".to_string())]
        } else {
            vec![]
        };
        (String::new(), env)
    });
    let (a, b) = (&fleet[0], &fleet[1]);
    for (s, other, ms) in [(a, "bravo", 300_000i64), (b, "alpha", -300_000)] {
        let seen = wait_for("the clock to be named", 30, || {
            s.call("fleet.refresh", json!({}))["stations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["station"] == json!(other) && x["why"] == json!("clock"))
                .cloned()
        });
        // Still not reachable: nothing about the check is weaker.
        assert_eq!(seen["state"], json!("unreachable"), "{seen}");
        let diff = seen["clock_diff_ms"].as_i64().unwrap();
        assert!((ms - 5_000..=ms + 5_000).contains(&diff), "{seen}");
        let err = s
            .try_call(
                "peer.call",
                json!({"station": other, "method": "pane.list", "params": {}}),
            )
            .unwrap_err();
        assert!(
            err.message.contains("differ by 5 minutes"),
            "{}",
            err.message
        );
        let perf = s.call("perf.get", json!({}));
        assert_eq!(perf["links"][0]["clock"]["refused"], json!(true), "{perf}");
        assert_eq!(
            perf["links"][0]["failures"]["last"]["why"],
            json!("clock"),
            "{perf}"
        );
        let checks = s.call("station.doctor", json!({}));
        let check = checks["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == json!(format!("clock ({other})")))
            .unwrap_or_else(|| panic!("no clock check: {checks}"))
            .clone();
        assert_eq!(check["ok"], json!(false));
        assert!(
            check["detail"].as_str().unwrap().contains("5 minutes"),
            "{check}"
        );
    }
}

/// Not a test of anything: pictures of the Performance page and the fleet
/// screen for a person to look at, at a computer's width and a phone's.
/// `SPANDECK_SHOTS=<folder> cargo test -p spandeck --test phase5 pictures -- --ignored`
#[test]
#[ignore = "takes pictures for a person to look at; needs Chrome"]
fn pictures_of_the_performance_page() {
    use common::browser::Page;
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let out = std::path::PathBuf::from(std::env::var("SPANDECK_SHOTS").expect("SPANDECK_SHOTS"));
    std::fs::create_dir_all(&out).unwrap();
    let shoot = |s: &Station, name: &str, hash: &str, ready: &str, (w, h): (u32, u32)| {
        let page = Page::open(s, w, h);
        page.goto(hash);
        page.wait(name, ready);
        std::thread::sleep(Duration::from_millis(600));
        let full = page.js("Math.max(document.querySelector('.perf-page, .fleet, main.phone')?.scrollHeight || 0, innerHeight)").as_u64().unwrap_or(h as u64) as u32;
        // The whole page, not only what fits the window.
        let tall = Page::open(s, w, full.clamp(h, 4000) + 80);
        tall.goto(hash);
        tall.wait(name, ready);
        std::thread::sleep(Duration::from_millis(600));
        let png = tall.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("{name}.png")), png).unwrap();
        let said =
            tall.js("document.querySelector('.perf-page, .fleet, main.phone')?.innerText || ''");
        std::fs::write(
            out.join(format!("{name}.txt")),
            said.as_str().unwrap_or_default(),
        )
        .unwrap();
    };
    let all_reported = |n: usize| {
        format!("document.querySelectorAll('.perf-verdicts .verdict').length === {n} && !document.body.innerText.includes('Asking every station')")
    };
    const WIDE: (u32, u32) = (1500, 900);
    const PHONE: (u32, u32) = (390, 844);

    // A healthy fleet of three, with something typed and a message sent.
    {
        let (a, b, _c) = Station::three();
        let sh = a.call("pane.spawn", json!({"agent": "shell", "name": "sh"}))["id"]
            .as_str()
            .unwrap()
            .to_string();
        a.call("pane.send_text", json!({"pane": sh, "text": "echo hello"}));
        let reader = b.fake(
            json!({"steps": [{"turn_end": "ready"}]}),
            json!({"name": "reader"}),
        );
        b.wait_state(&reader, "done");
        a.call(
            "msg.send",
            json!({"to": "reader@bravo", "body": "hello", "kind": "note"}),
        );
        std::thread::sleep(Duration::from_secs(4));
        for s in [&a, &b] {
            s.call("fleet.refresh", json!({}));
        }
        shoot(&a, "healthy-desktop", "#/perf", &all_reported(3), WIDE);
        shoot(&a, "healthy-phone", "#/perf", &all_reported(3), PHONE);
        shoot(
            &b,
            "healthy-desktop-from-bravo",
            "#/perf",
            &all_reported(3),
            WIDE,
        );
    }

    // Trouble: bravo's key expires in 6 days, charlie's has expired and it
    // is off, delta's clock is five minutes out, echo is simply off; the
    // phone's key has 2 days left.
    let names = ["alpha", "bravo", "charlie", "delta", "echo"];
    let mut fleet = Station::fleet(&names, |name, root| {
        let on = json!({"Online": true});
        let others: Vec<Value> = [
            ("alpha", None),
            ("bravo", Some(6)),
            ("charlie", Some(-2)),
            ("delta", None),
            ("echo", None),
        ]
        .iter()
        .filter(|(n, _)| *n != name)
        .map(|(n, days)| device(n, "linux", *days, on.clone()))
        .chain([
            device(
                "pocket-phone",
                "iOS",
                Some(2),
                json!({"TailscaleIPs": ["100.64.0.9"]}),
            ),
            device(
                "old-box",
                "linux",
                Some(-147),
                json!({"TailscaleIPs": ["100.64.0.10"]}),
            ),
            device(
                "spare-laptop",
                "windows",
                Some(21),
                json!({"TailscaleIPs": ["100.64.0.11"]}),
            ),
        ])
        .collect();
        let mine = match name {
            "bravo" => Some(6),
            _ => None,
        };
        let (extra, mut env) =
            standin_tailnet(root, device(name, "linux", mine, on.clone()), &others);
        if name == "delta" {
            env.push(("SPANDECK_TEST_CLOCK_MS".to_string(), "300000".to_string()));
        }
        (extra, env)
    });
    let online = |s: &Station| {
        s.call("fleet.refresh", json!({}))["stations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["state"] == json!("online"))
            .count()
    };
    wait_for("the four to find each other", 40, || {
        (online(&fleet[0]) == 4 && online(&fleet[1]) == 4).then_some(())
    });
    for i in [2, 4] {
        fleet[i].kill9();
    }
    wait_for("the two to be seen as off", 40, || {
        (online(&fleet[0]) == 2 && online(&fleet[1]) == 2).then_some(())
    });
    let a = &fleet[0];
    shoot(a, "trouble-desktop", "#/perf", &all_reported(5), WIDE);
    shoot(a, "trouble-phone", "#/perf", &all_reported(5), PHONE);
    shoot(
        &fleet[1],
        "trouble-desktop-from-bravo",
        "#/perf",
        &all_reported(5),
        WIDE,
    );
    let tiles = "document.querySelectorAll('a.tile.station').length === 5 && document.body.innerText.includes('Tailscale key')";
    shoot(a, "trouble-fleet-desktop", "#/fleet", tiles, WIDE);
    shoot(a, "trouble-fleet-phone", "#/fleet", "(b => (b && b.getAttribute('aria-pressed') !== 'true' && b.click(), document.querySelectorAll('.phone-station').length === 5 && document.body.innerText.includes('Tailscale key')))([...document.querySelectorAll('.home-switch button')].find(b => b.innerText === 'By station'))", PHONE);
    // The steps for the expired machine, as the owner would open them.
    let page = Page::open(a, WIDE.0, WIDE.1);
    page.goto("#/fleet");
    page.wait("the tiles", tiles);
    page.js("[...document.querySelectorAll('.key-chip.bad')].find(x => x.innerText.includes('expired')).click()");
    page.wait("the steps", "!!document.querySelector('.dialog ol')");
    std::fs::write(
        out.join("trouble-expired-steps.png"),
        page.tab.capture_screenshot(Png, None, None, true).unwrap(),
    )
    .unwrap();
    // This device's own key: the deck opened on bravo's own machine.
    let page = Page::open(&fleet[1], WIDE.0, WIDE.1);
    page.wait(
        "the line for this device",
        "!!document.querySelector('.own-key')",
    );
    std::fs::write(
        out.join("trouble-own-device-on-bravo.png"),
        page.tab.capture_screenshot(Png, None, None, true).unwrap(),
    )
    .unwrap();
}

/// Not a test of behaviour: pictures of the conversation view for a person to
/// look at, on a phone and on a computer, in a dark and a light theme.
/// `SPANDECK_SHOTS=<folder> cargo test -p spandeck --test phase5 pictures_of_the_conversation -- --ignored`
#[test]
#[ignore = "takes pictures for a person to look at; needs Chrome"]
fn pictures_of_the_conversation_view() {
    use common::browser::Page;
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let out = std::path::PathBuf::from(std::env::var("SPANDECK_SHOTS").expect("SPANDECK_SHOTS"));
    std::fs::create_dir_all(&out).unwrap();
    let s = Station::start("spark2");
    let at = std::cell::Cell::new(0u64);
    // A line of the session file, a few seconds after the one before.
    let line = |mut v: Value, secs: u64| {
        at.set(at.get() + secs);
        v["timestamp"] = json!(format!(
            "2026-11-15T10:{:02}:{:02}.000Z",
            at.get() / 60,
            at.get() % 60
        ));
        json!({"transcript": v})
    };
    let said = |blocks: Value| json!({"type": "assistant", "message": {"role": "assistant", "content": blocks}});
    let call = |id: &str, name: &str, input: Value| {
        said(json!([{"type": "tool_use", "id": id, "name": name, "input": input}]))
    };
    let result = |id: &str, out: &str, more: Value| {
        let mut r = json!({"stdout": out, "stderr": "", "interrupted": false});
        for (k, v) in more.as_object().into_iter().flatten() {
            r[k] = v.clone();
        }
        json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}, "toolUseResult": r})
    };
    let answer = "**Done: the tally now counts herons only.**\n\n**What I changed**\n- `tally.py` filters the rows to the heron family before it counts\n- the test for mixed rows passes again (3 of 3)\n\n**What I found**\n1. 412 rows in `survey.csv`, 37 of them herons\n2. Two rows spell the species `Heron ` with a space, which the old count missed\n\n```python\nherons = [r for r in rows if r.species.strip() == 'heron']\nreturn len(herons)\n```\n\n| Species | Rows |\n|:--|--:|\n| heron | 37 |\n| egret | 112 |\n\nThe survey's method is described at https://example.org/marsh/method. Raw HTML stays text: <b>not bold</b>.\n\n- [x] count the rows\n- [ ] ask about the egrets";
    let heron = s.fake(
        json!({"steps": [
            line(json!({"type": "ai-title", "aiTitle": "Heron counts for the marsh survey"}), 0),
            {"prompt": "Count the herons in survey.csv and fix the tally script."},
            line(said(json!([{"type": "thinking", "thinking": "…"}])), 4),
            line(said(json!([{"type": "text", "text": "I'll count the rows first, then fix `tally.py`."}])), 1),
            line(call("t1", "Bash", json!({"command": "wc -l survey.csv"})), 1), line(result("t1", "412 survey.csv", json!({})), 2),
            line(call("t2", "Read", json!({"file_path": "/home/ada/marsh/tally.py"})), 1), line(result("t2", "def tally(rows):\n    return len(rows)", json!({})), 1),
            line(call("t3", "Edit", json!({"file_path": "/home/ada/marsh/tally.py", "old_string": "    return len(rows)", "new_string": "    herons = [r for r in rows if r.species.strip() == 'heron']\n    return len(herons)"})), 3),
            line(result("t3", "The file /home/ada/marsh/tally.py has been updated.", json!({"filePath": "/home/ada/marsh/tally.py", "structuredPatch": [{"oldStart": 10, "oldLines": 2, "newStart": 10, "newLines": 3,
                "lines": [" def tally(rows):", "-    return len(rows)", "+    herons = [r for r in rows if r.species.strip() == 'heron']", "+    return len(herons)"]}]})), 1),
            line(call("t4", "Bash", json!({"command": "python -m pytest -q tests/test_tally.py"})), 2), line(result("t4", "...                                                                      [100%]\n3 passed in 0.41s", json!({})), 31),
            line(call("t5", "Grep", json!({"pattern": "len\\(rows\\)", "path": "."})), 1), line(result("t5", "No matches found", json!({})), 1),
            line(call("t6", "Bash", json!({"command": "git diff --stat"})), 1), line(result("t6", " tally.py | 3 ++-\n 1 file changed, 2 insertions(+), 1 deletion(-)", json!({})), 2),
            {"turn_end": answer}
        ]}),
        json!({"name": "heron"}),
    );
    let asker = s.fake(
        json!({"steps": [
            line(json!({"type": "ai-title", "aiTitle": "Clean the build folder"}), 0),
            {"prompt": "Clear out the old build and rebuild."},
            line(said(json!([{"type": "text", "text": "I'll remove `build/` and run the build again."}])), 2),
            line(call("t1", "Bash", json!({"command": "rm -rf build"})), 1),
            {"permission": {"name": "Bash", "command": "rm -rf build"}}, {"sleep": 600000}
        ]}),
        json!({"name": "builder"}),
    );
    let worker = s.fake(
        json!({"steps": [
            {"prompt": "Run the whole test suite and tell me what fails."},
            line(said(json!([{"type": "text", "text": "Running the tests now."}])), 2),
            line(call("t1", "Bash", json!({"command": "cargo test --workspace"})), 1),
            {"subagent": {"type": "Explore"}}, {"subagent": {"type": "Explore"}}, {"subagent": {"type": "general-purpose"}},
            {"say": "running 214 tests\ntest archive::follows_a_file ... ok\ntest archive::search ... ok\ntest fleet::join ... ok\ntest fleet::merge ... ok"},
            {"work": 600000}
        ]}),
        json!({"name": "tester"}),
    );
    let planner = s.fake(
        json!({"steps": [
            line(json!({"type": "ai-title", "aiTitle": "Which herons to count"}), 0),
            {"prompt": "Tally the survey."},
            line(call("t1", "AskUserQuestion", json!({"questions": [{"header": "Species", "question": "Which herons count?", "multiSelect": false,
                "options": [{"label": "Grey herons only", "description": "Skip egrets and bitterns"}, {"label": "All herons", "description": "Count every bird in the heron family"}]}]})), 3),
            {"say": "Which herons count?\n❯ 1. Grey herons only\n  2. All herons\n  3. Type something."},
            {"sleep": 600000}
        ]}),
        json!({"name": "planner"}),
    );
    let stuck = s.fake(
        json!({"steps": [
            {"prompt": "Build it."}, {"sleep": 2500},
            {"repeat": 3, "steps": [{"tool": {"name": "Bash", "command": "make", "ok": false}}]},
            {"say": "Do you want to keep trying?\n❯ 1. Yes\n  2. No, stop here"},
            {"sleep": 600000}
        ]}),
        json!({"name": "maker"}),
    );
    let sh = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "scratch", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.wait_state(&asker, "needs_you");
    s.wait_state(&worker, "working");
    s.wait_state(&stuck, "needs_you");
    s.wait_state(&heron, "done");
    s.call(
        "deck.pin",
        json!({"pin": {"kind": "pane", "station": "spark2", "id": worker}}),
    );
    s.call(
        "deck.layout.save",
        json!({"layout": {"name": "survey", "tree": {"dir": "row", "ratio": 500, "a": {"id": heron, "station": "spark2"}, "b": {"id": worker, "station": "spark2"}}}}),
    );

    let shot = |page: &Page, name: &str| {
        std::thread::sleep(Duration::from_millis(700));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("{name}.png")), png).unwrap();
    };
    let pane = |id: &str| format!("#/s/spark2/p/{id}");
    let click = |page: &Page, what: &str, el: &str| {
        page.wait(what, &format!("(e => e ? (e.click(), true) : false)({el})"));
    };
    let button = |text: &str| {
        format!("[...document.querySelectorAll('button')].find(b => b.innerText.trim().startsWith('{text}'))")
    };
    for theme in ["dark", "light"] {
        s.call(
            "look.set",
            json!({"level": "deck", "choice": {"theme": format!("spandeck-{theme}")}}),
        );
        let phone = Page::open(&s, 390, 844);
        phone.wait("the list", "document.querySelectorAll('.session-row').length >= 5 && document.body.innerText.includes('Heron counts')");
        shot(&phone, &format!("phone-{theme}-1-list"));
        phone.goto(&pane(&heron));
        phone.wait("heron", "!!document.querySelector('.chat-acts')");
        shot(&phone, &format!("phone-{theme}-2-conversation"));
        phone.js("(document.querySelector('.chat-log').scrollTop = 0, true)");
        click(&phone, "the fold", "document.querySelector('.chat-fold')");
        click(&phone, "the edit", "[...document.querySelectorAll('.chat-tool-row')].find(r => r.innerText.includes('Edit'))");
        phone.wait("the diff", "!!document.querySelector('.diff-add')");
        phone.js("(document.querySelector('.chat-run').scrollIntoView(), true)");
        shot(&phone, &format!("phone-{theme}-3-tools-open"));
        phone.goto(&pane(&asker));
        phone.wait(
            "approval",
            "!!document.querySelector('.chat-cards .approval')",
        );
        shot(&phone, &format!("phone-{theme}-4-approval"));
        phone.goto(&pane(&worker));
        click(&phone, "peek", "document.querySelector('.act-peek')");
        phone.wait(
            "peek",
            "document.querySelector('.chat-peek')?.innerText.includes('running')",
        );
        shot(&phone, &format!("phone-{theme}-5-working"));
        if theme == "dark" {
            click(&phone, "tasks", "document.querySelector('.chat-tasks')");
            shot(&phone, "phone-dark-5b-running-tasks");
            click(
                &phone,
                "close",
                "document.querySelector('.dialog-head .icon-btn')",
            );
        }
        phone.goto(&pane(&planner));
        phone.wait("question", "!!document.querySelector('.chat-card.ask')");
        shot(&phone, &format!("phone-{theme}-6-question"));
        phone.goto(&pane(&stuck));
        phone.wait("banner", "!!document.querySelector('.chat-card.wait') && document.querySelector('.chat-peek')?.innerText.length > 5");
        shot(&phone, &format!("phone-{theme}-7-waiting-in-terminal"));
        phone.goto(&pane(&sh));
        phone.wait("shell", "!!document.querySelector('.phone-pane .xterm-rows') && !!document.querySelector('.chat-why')");
        shot(&phone, &format!("phone-{theme}-8-shell"));
        if theme == "dark" {
            phone.goto(&pane(&heron));
            click(&phone, "Terminal", &button("Terminal"));
            phone.wait(
                "terminal",
                "!!document.querySelector('.phone-pane .xterm-rows')",
            );
            shot(&phone, "phone-dark-9-terminal-face");
            click(&phone, "Chat", &button("Chat"));
            phone.goto("#/fleet");
            click(
                &phone,
                "new session",
                "document.querySelector('.new-session')",
            );
            phone.wait("sheet", "!!document.querySelector('.dialog.sheet')");
            shot(&phone, "phone-dark-10-new-session");
            click(
                &phone,
                "close",
                "document.querySelector('.dialog-head .icon-btn')",
            );
            click(&phone, "by station", &button("By station"));
            phone.wait("stations", "!!document.querySelector('.phone-station')");
            shot(&phone, "phone-dark-11-by-station");
        }
        drop(phone);

        let wide = Page::open(&s, 1400, 900);
        wide.goto(&pane(&heron));
        click(&wide, "Chat", "document.querySelector('.strip .face-btn')");
        wide.wait("heron", "!!document.querySelector('.chat-pane .chat-acts')");
        click(&wide, "the fold", "document.querySelector('.chat-fold')");
        shot(&wide, &format!("desktop-{theme}-1-pane-as-conversation"));
        wide.goto("#/l/survey");
        wide.wait("the layout", "!!document.querySelector('.workspace .chat-pane') && !!document.querySelector('.workspace .xterm-rows')");
        shot(&wide, &format!("desktop-{theme}-2-layout-one-of-each"));
        wide.goto(&pane(&asker));
        wide.wait("terminal", "!!document.querySelector('.strip .face-btn')");
        click(&wide, "Chat", "document.querySelector('.strip .face-btn')");
        wide.wait(
            "approval",
            "!!document.querySelector('.chat-cards .approval')",
        );
        shot(&wide, &format!("desktop-{theme}-3-approval"));
        click(&wide, "Terminal", &button("Terminal"));
        wide.wait("terminal", "!!document.querySelector('.strip .face-btn')");
        shot(
            &wide,
            &format!("desktop-{theme}-4-terminal-with-chat-button"),
        );
    }
}

/// More of the conversation view (mobile plan, phase M2), in a browser: the
/// rest of what the phase asks for, which the every-change tier has no room
/// for. `cargo test -p spandeck --test phase5 the_rest_of_the_conversation_view -- --ignored`
#[test]
#[ignore = "merge tier: the rest of the conversation view, in a browser"]
fn the_rest_of_the_conversation_view() {
    use common::browser::Page;
    let s = Station::start("alpha");
    let line = |v: Value| json!({"transcript": v});
    let said = |text: &str| {
        line(
            json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}}),
        )
    };
    let click = |page: &Page, what: &str, el: &str| {
        page.wait(what, &format!("(e => e ? (e.click(), true) : false)({el})"));
    };
    let button = |text: &str| {
        format!(
            "[...document.querySelectorAll('button')].find(b => b.innerText.trim() === '{text}')"
        )
    };
    let pane = |id: &str| format!("#/s/alpha/p/{id}");

    // A long conversation: the newest page first, the page above when the
    // top is reached, and what was being read stays where it was.
    let long = s.fake(
        json!({"steps": [{"prompt": "talk"}, {"repeat": 140, "steps": [said("A line of talk, with a link: https://example.org/a?b=1")]}, {"turn_end": "the end"}]}),
        json!({"name": "long"}),
    );
    s.wait_state(&long, "done");
    let phone = Page::open(&s, 390, 844);
    phone.goto(&pane(&long));
    phone.wait("the newest page", "document.querySelectorAll('.chat-text').length >= 50 && !!document.querySelector('.chat-acts')");
    let texts = "document.querySelectorAll('.chat-text').length";
    let first = phone.js(texts).as_u64().unwrap();
    assert!(first < 142, "one page, not the whole conversation: {first}");
    let log = "document.querySelector('.chat-log')";
    assert_eq!(
        phone.js(&format!(
            "{log}.scrollHeight - {log}.scrollTop - {log}.clientHeight < 4"
        )),
        json!(true),
        "it opens at the end"
    );
    phone.js(&format!("({log}.scrollTop = 0, true)"));
    phone.wait("the page above", &format!("{texts} > {first}"));
    assert!(
        phone.js(&format!("{log}.scrollTop")).as_f64().unwrap() > 500.0,
        "what was at the top is still in sight: the page grew above it"
    );
    // A redraw leaves the log where it was, and new items do not pull it down.
    let top = phone.js(&format!("{log}.scrollTop"));
    phone.redraw(&s, &long);
    assert_eq!(phone.js(&format!("{log}.scrollTop")), top);
    phone.wait(
        "every page",
        &format!("({log}.scrollTop = 0, {texts} === 141)"),
    );

    // The page loses its connection and makes another: the conversation is
    // asked for from where it had got to, so nothing is there twice and what
    // was said meanwhile is there once.
    phone.js("(() => { const send = WebSocket.prototype.send; WebSocket.prototype.send = function (...a) { window.__ws = this; return send.apply(this, a); }; return true; })()");
    phone.wait("the page's connection", "!!window.__ws");
    phone.js("(window.__was = window.__ws, window.__ws.close(), true)");
    s.call(
        "pane.send_text",
        json!({"pane": long, "text": "after the break"}),
    );
    s.wait_text(&long, "[fake] got: after the break");
    phone.wait(
        "a new connection",
        "window.__ws !== window.__was && window.__ws.readyState === 1",
    );
    phone.wait(
        "what was said meanwhile, once",
        &format!("{texts} === 142 && document.querySelectorAll('.chat-user').length === 2"),
    );
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        phone.js(&format!("[{texts}, document.querySelectorAll('.chat-user').length, [...document.querySelectorAll('.chat-bubble')].pop().innerText]")),
        json!([142, 2, "after the break"])
    );

    // A link is shown before it is opened, and touching it goes nowhere.
    let at = phone.js("location.href");
    phone.js("(document.querySelector('.chat-text a.md-link').click(), true)");
    phone.wait(
        "where the link leads",
        "document.querySelector('.dialog .link-dest')?.innerText === 'https://example.org/a?b=1'",
    );
    assert_eq!(phone.js("location.href"), at);
    click(&phone, "Cancel", &button("Cancel"));

    // Read aloud. A browser with no screen has no voice (it ends at once),
    // so the page's speaker is stood in for: what is tested is what it is
    // given to say, and that the button starts and stops it.
    if phone.js("'speechSynthesis' in window") == json!(true) {
        phone.js(&format!("(Object.defineProperty(speechSynthesis, 'speak', {{value: (u) => {{ window.__spoken = u.text; }}, configurable: true}}), Object.defineProperty(speechSynthesis, 'cancel', {{value: () => {{}}, configurable: true}}), {log}.scrollTop = {log}.scrollHeight, true)"));
        click(&phone, "Read aloud", &button("Read aloud"));
        phone.wait(
            "reading",
            &format!(
                "!!{} && window.__spoken === 'the end'",
                button("Stop reading")
            ),
        );
        click(&phone, "Stop reading", &button("Stop reading"));
        phone.wait("stopped", &format!("!!{}", button("Read aloud")));
    }

    // "/" offers the agent's text commands; one fills the box.
    phone.js("(b => { b.value = '/c'; b.dispatchEvent(new Event('input')); return true; })(document.querySelector('.chat-box'))");
    assert_eq!(
        phone.js("[...document.querySelectorAll('.chat-slash button')].map(b => b.innerText)"),
        json!(["/compact", "/clear", "/context"])
    );
    phone.js("(document.querySelector('.chat-slash button').click(), true)");
    assert_eq!(
        phone.js("document.querySelector('.chat-box').value"),
        json!("/compact")
    );
    phone.js("(b => { b.value = ''; b.dispatchEvent(new Event('input')); return true; })(document.querySelector('.chat-box'))");

    // "+" sends a file: the station writes it and types its path for the agent.
    click(&phone, "+", "document.querySelector('.chat-plus')");
    click(&phone, "a file", "[...document.querySelectorAll('.menu-item')].find(i => i.innerText.startsWith('A photo or a file'))");
    phone.js("(i => { const d = new DataTransfer(); d.items.add(new File(['heron,37'], 'tally.csv')); i.files = d.files; i.dispatchEvent(new Event('change')); return true; })(document.querySelector('#overlays input[type=file]'))");
    s.wait_text(&long, "tally.csv");

    // While the agent works and nothing is written, Send is Stop.
    let busy = s.fake(
        json!({"steps": [{"prompt": "x"}, {"work": 120000}]}),
        json!({"name": "busy"}),
    );
    s.wait_state(&busy, "working");
    phone.goto(&pane(&busy));
    phone.wait("the working line", "document.querySelector('.chat-activity')?.innerText.includes('Working · Bash') && document.querySelector('.chat-send').innerText === 'Stop'");
    click(&phone, "Peek", "document.querySelector('.act-peek')");
    phone.wait(
        "the terminal's last lines",
        "document.querySelector('.chat-peek')?.innerText.includes('[fake] working')",
    );
    phone.js("(b => { b.value = 'wait'; b.dispatchEvent(new Event('input')); return true; })(document.querySelector('.chat-box'))");
    assert_eq!(
        phone.js("document.querySelector('.chat-send').innerText"),
        json!("Send")
    );
    phone.js("(b => { b.value = ''; b.dispatchEvent(new Event('input')); return true; })(document.querySelector('.chat-box'))");
    phone.js("(document.querySelector('.chat-send').click(), true)");
    s.wait_text(&busy, "[fake] interrupted");

    // An agent that needs the owner where no card can answer: the banner,
    // the terminal's last lines, and keys that reach the terminal.
    let stuck = s.fake(
        json!({"steps": [{"prompt": "go"}, {"repeat": 3, "steps": [{"tool": {"name": "Bash", "command": "make", "ok": false}}]}]}),
        json!({"name": "stuck"}),
    );
    s.wait_state(&stuck, "needs_you");
    phone.goto(&pane(&stuck));
    phone.wait("the banner", "document.querySelector('.chat-card.wait')?.innerText.startsWith('Waiting in the terminal: ') && document.querySelector('.chat-peek')?.innerText.includes('[fake] ran Bash (failed)')");
    for key in ["2", "Enter"] {
        phone.js(&format!("[...document.querySelectorAll('.wait-keys .key')].find(b => b.innerText === '{key}').click()"));
        std::thread::sleep(Duration::from_millis(150));
    }
    s.wait_text(&stuck, "[fake] got: 2");

    // A question that waits is shown, and is answered in the terminal.
    let asks = s.fake(
        json!({"steps": [{"prompt": "tally"}, line(json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "q1", "name": "AskUserQuestion",
            "input": {"questions": [{"header": "Species", "question": "Which herons count?", "multiSelect": false, "options": [{"label": "Grey herons only"}, {"label": "All herons"}]}]}}]}})), {"sleep": 600000}]}),
        json!({"name": "asks"}),
    );
    phone.goto(&pane(&asks));
    phone.wait("the question", "document.querySelector('.chat-card.ask')?.innerText.includes('Which herons count?') && document.querySelectorAll('.chat-card.ask button').length === 1");
    click(
        &phone,
        "Answer in the terminal",
        &button("Answer in the terminal"),
    );
    phone.wait(
        "the terminal",
        "!!document.querySelector('.phone-pane .xterm-rows')",
    );

    // A new session from the sheet lands in the new pane.
    phone.goto("#/fleet");
    click(
        &phone,
        "+ New session",
        "document.querySelector('.new-session')",
    );
    click(&phone, "Shell", "[...document.querySelectorAll('.dialog.sheet .tile-pick')].find(b => b.innerText.trim() === 'Shell')");
    click(&phone, "Start", &button("Start"));
    phone.wait("the new pane", "/#\\/s\\/alpha\\/p\\//.test(location.hash) && !!document.querySelector('.phone-pane .xterm-rows') && document.querySelector('.chat-why')?.innerText.includes('it runs a shell')");
    drop(phone);

    // On a computer: Chat on a terminal's title bar, in a pane at full size
    // and in a layout. The conversation holds no size: the terminal beside
    // it keeps control of its own pane, and the pane shown as a conversation
    // keeps the size it had.
    s.call(
        "deck.layout.save",
        json!({"layout": {"name": "two", "tree": {"dir": "row", "ratio": 500, "a": {"id": long, "station": "alpha"}, "b": {"id": busy, "station": "alpha"}}}}),
    );
    let wide = Page::open(&s, 1400, 900);
    wide.goto("#/l/two");
    wide.wait(
        "two terminals",
        "document.querySelectorAll('.workspace .pane .xterm-rows').length === 2",
    );
    let fitted = wait_for("the layout to fit its panes", 10, || {
        let p = s.pane(&long);
        (!p["controller"].is_null()).then(|| p["cols"].clone())
    });
    click(
        &wide,
        "Chat",
        &format!("document.querySelector('.pane[data-pane=\"{long}\"] .face-btn')"),
    );
    wide.wait("one of each", "document.querySelectorAll('.workspace .chat-pane .chat-text').length >= 40 && document.querySelectorAll('.workspace .pane .xterm-rows').length === 1");
    wide.redraw(&s, &busy);
    assert_eq!(
        s.pane(&long)["cols"],
        fitted,
        "showing the conversation did not resize the pane"
    );
    wide.goto(&pane(&long));
    wide.wait("the same face at full size", "!!document.querySelector('.station-main.zoomed .chat-pane .chat-text') && !document.querySelector('.station-main .xterm')");
    // Enter sends, on a computer. The path of the file sent from the phone
    // was typed for the agent and waits in its prompt: it goes with the
    // message.
    wide.js("(document.querySelector('.chat-box').focus(), true)");
    wide.tab.type_str("from a computer").unwrap();
    wide.tab.press_key("Enter").unwrap();
    s.wait_text(&long, "tally.csv from a computer");
    click(&wide, "Terminal", &button("Terminal"));
    wide.wait("the terminal again", "!!document.querySelector('.station-main.zoomed .pane .xterm-rows') && !document.querySelector('.chat-pane')");
}
