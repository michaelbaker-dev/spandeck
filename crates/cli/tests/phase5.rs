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

/// The owner's report of 17 Nov 2026, adding the Mac "monitoring" from
/// spark2's deck: spandeck was installed and started there, and its request
/// to join ended at once with "no station called spark2 was found". A
/// station that has only just started has not seen the others yet.
///
/// Here bravo is told to join alpha before it can see it (alpha is not
/// running yet). It goes on looking, and asks as soon as alpha is there:
/// the code alpha shows is the one bravo was given. A station can also be
/// named by the address it answers at.
#[test]
fn a_station_told_to_join_one_it_does_not_see_yet_goes_on_looking_for_it() {
    let mut a = Station::not_started("alpha", "");
    let b = Station::start_with(
        "bravo",
        &format!("peers = [\"127.0.0.1:{}\"]\n", a.port),
        &[],
    );
    assert_eq!(b.sees("alpha"), None);
    let (asked, waiting) = std::thread::scope(|s| {
        let join = s.spawn(|| b.try_call("fleet.join", json!({"station": "alpha"})));
        // Before this was fixed the answer came at once, and was the error.
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!join.is_finished(), "it gave up without looking again");
        a.launch();
        let asked = join.join().unwrap().expect("the join, once alpha is there");
        (asked, a.call("trust.status", json!({})))
    });
    assert_eq!(asked["station"], json!("alpha"), "{asked}");
    let req = &waiting["requests"][0];
    assert_eq!(
        (req["name"].clone(), req["code"].clone()),
        (json!("bravo"), asked["code"].clone()),
        "{waiting}"
    );
    // By address: the one in bravo's own config. An address this station
    // would not look at by itself is not asked.
    let by_address = b.call(
        "fleet.join",
        json!({"station": format!("127.0.0.1:{}", a.port)}),
    );
    assert_eq!(by_address["station"], json!("alpha"), "{by_address}");
    let err = b
        .try_call("fleet.join", json!({"station": "192.0.2.7:7391"}))
        .unwrap_err();
    assert!(
        err.message.contains("does not ask it") || err.message.contains("Tailscale did not answer"),
        "{}",
        err.message
    );
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
    // The pane's size and who holds it, from one look at the pane. The
    // station sets the size and then notes whose it is: for a moment the
    // new size is there under the last holder's name. Asked one after the
    // other, the answers came from either side of that moment (once on
    // the Linux CI runner; seen here by asking without a pause).
    let held = |what: &str, want: &dyn Fn((u64, u64), &Value) -> bool| {
        wait_for_or(
            what,
            10,
            || format!("the pane is {}", b.pane(&pane)),
            || {
                let p = b.pane(&pane);
                let size = (p["cols"].as_u64().unwrap(), p["rows"].as_u64().unwrap());
                want(size, &p["controller"]).then_some(size)
            },
        )
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
    let on_phone = held("the phone's size, held by the phone", &|s, by| {
        s.0 < 80 && !by.is_null() && *by != json!(who)
    });
    one.wait("alpha's window to give way", gave_way);
    assert_ne!(b.pane(&pane)["controller"], json!(who));
    shows(&one, "alpha's window to show the pane's size", on_phone);
    shows(&direct, "the window at bravo to show it", on_phone);

    // Taking control back in alpha's window works through alpha, and both
    // of the others are told.
    one.js("[...document.querySelectorAll('.strip button')].find(b => b.innerText === 'Take control').click()");
    held("the size of alpha's window again, held by it", &|s, by| {
        s == through_one && *by == json!(who)
    });
    assert_eq!(b.pane(&pane)["controller"], json!(who));
    // (A phone that gives way shows the pane at its own text size, and says
    // where the pane is open.)
    phone.wait("the phone to give way", "!!document.querySelector('.term-body.natural') && (document.querySelector('.fit-note')?.innerText || '').includes('Fit takes its size')");
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
    // The shell that is opened further down. (Started before the page is
    // open: a pane the page sees start is not fitted to the phone in its
    // first six seconds, which this tier has no time to wait out.)
    let sh = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "phone-sh", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();

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
    // Nobody else has the pane open: the phone fitted it to itself.
    wait_for("the pane to be fitted to the phone", 10, || {
        let size = s.pane(&busy);
        (size["cols"].as_u64().is_some_and(|c| c < 80) && !size["controller"].is_null())
            .then_some(())
    });

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
    open("phone-sh");
    // A shell opens as its terminal, and has the choice of Chat or Terminal
    // like every pane (16 Nov 2026; its Chat is in `chat.rs`). Until then
    // the bar said "Terminal only: it runs a shell, not an agent" there.
    assert_eq!(
        page.wait("the shell's bar", "(b => b.length === 2 && [!!document.querySelector('.phone-pane .chat-why'), b.map(x => x.innerText + (x.classList.contains('on') ? '*' : '')).join(' ')])([...document.querySelectorAll('.phone-pane .chat-switch button')])"),
        json!([false, "Chat Terminal*"])
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

    // Nobody else has the pane open, so the phone fitted it to itself (16 Nov
    // 2026; `a_pane_on_a_phone_is_read_at_the_phones_text_size`). Turned
    // off, the pane has its size back and is watched.
    let fit = |label: &str| {
        format!("(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.phone-pane button')].find(b => b.innerText === '{label}'))")
    };
    wait_for("the pane to be fitted to the phone", 10, || {
        let p = s.pane(&sh);
        (p["cols"].as_u64().is_some_and(|c| c < 80) && !p["controller"].is_null()).then_some(())
    });
    page.wait("Fitted, turned off", &fit("Fitted"));
    let watched = wait_for("the pane to have its size back", 10, || {
        let p = s.pane(&sh);
        p["cols"].as_u64().filter(|c| *c >= 80)
    });
    // A bug that happened on a real phone: tapping the terminal brought up the
    // keyboard, and typing did nothing. Keys typed into the terminal arrive,
    // and the pane keeps its size.
    page.wait("terminal", "document.querySelector('.phone-pane .term-body.natural .xterm-helper-textarea') && (document.querySelector('.phone-pane .xterm-helper-textarea').focus(), true)");
    page.type_str("echo typed-$((20+3))");
    page.press_key("Enter");
    s.wait_text(&sh, "typed-23");
    assert_eq!(
        s.pane(&sh)["cols"].as_u64(),
        Some(watched),
        "typing did not take the pane's size"
    );

    // Fitting the pane to the phone resizes it, which watching never does.
    page.wait("Fit to screen", &fit("Fit to screen"));
    wait_for("the pane to be resized to the phone", 10, || {
        let p = s.pane(&sh);
        (p["cols"].as_u64().is_some_and(|c| c < 80) && !p["controller"].is_null()).then_some(())
    });

    // A bug that happened on a real phone: a finger could not scroll the
    // terminal's history. Dragging down shows earlier output. (The history
    // is the phone's own to scroll since 16 Nov 2026, phonescroll.rs: so
    // the finger is a real touch, which the browser scrolls with.)
    set("seq 1000 1300");
    page.js("document.querySelector('.composer-row button[type=submit]').click()");
    s.wait_text(&sh, "1300");
    let term = "document.querySelector('.phone-pane .pane').paneTerm";
    page.wait(
        "the end of the output, and its history",
        &format!("(t => t.following && t.hist.lines > 250 && !t.hist.pending() && t.body.querySelector('.xterm-rows').innerText.split('\\n').some(x => x.trim() === '1300'))({term})"),
    );
    page.touches();
    page.touch_drag(&format!("{term}.body"), 0.0, 250.0, 12);
    page.wait(
        "earlier output after dragging down",
        &format!("(t => !t.following && t.body.scrollTop < t._metrics().liveTop - 60)({term})"),
    );

    // Found from a real phone's drag reports: Claude Code runs full-screen and
    // listens for the mouse, so it has to be told to scroll. A drag over such a
    // program sends it wheel turns.
    set("printf '\\033[?1049h\\033[?1003h\\033[?1006h'; cat -v");
    page.js("document.querySelector('.composer-row button[type=submit]').click()");
    page.wait(
        "the full-screen program",
        &format!("(t => t._alt && t._driven && t.term.modes.mouseTrackingMode !== 'none')({term})"),
    );
    page.touch_drag(&format!("{term}.body"), 0.0, 200.0, 10);
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
    // Started here, so that it is in the list by the time the list is looked
    // at: the agent whose question and plan are answered below.
    let surveyor = surveyor(s);
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
    answered_in_place(s, page, &surveyor);
    page.goto("#/fleet");
    the_phones_appearance_sheet(s, page, &counter);
}

/// Mobile plan, phase M3, on a phone: a question the agent asks is a card
/// above the message box, with each choice a button; several questions are
/// steps; several choices and an answer of the owner's own are sent as the
/// one answer the agent expects, once; a plan is drawn as markdown and sent
/// back with a note. The card says until when it waits here. The pane is
/// under "Needs you", and its row leads to the card. What the agent wrote
/// is text on the page, never HTML (rule 6). The rest (a plan approved, the
/// hold running out, "Answer in the terminal instead", who may answer, a
/// restart) is in `questions.rs` and `away.rs`, without a browser.
fn answered_in_place(s: &Station, page: &common::browser::Page, surveyor: &str) {
    let click = |what: &str, el: &str| {
        page.wait(what, &format!("(e => e ? (e.click(), true) : false)({el})"));
    };
    let opt = |text: &str| {
        format!("[...document.querySelectorAll('.chat-card.held .ask-opt')].find(b => b.innerText.includes('{text}'))")
    };
    answered_in_place_on(s, page, surveyor, &click, &opt);
}

/// An agent that asks two questions together, then proposes a plan.
fn surveyor(s: &Station) -> String {
    s.fake(
        json!({"steps": [
            {"prompt": "tally the birds"},
            {"ask": {"questions": [
                {"header": "Species", "question": "Which herons count?", "multiSelect": false,
                 "options": [{"label": "Grey herons only", "description": "Skip egrets"},
                             {"label": "<img src=x onerror=\"window.__hit = 2\">All herons", "description": "<b>Every</b> bird"}]},
                {"header": "Rivers", "question": "Which rivers?", "multiSelect": true,
                 "options": [{"label": "Avon"}, {"label": "Exe"}]}]}},
            {"say": "settled the question"},
            {"plan": {"plan": "# Tally plan\n\n1. Count the **grey** herons\n\n<img src=x onerror=\"window.__hit = 3\">"}},
            {"say": "settled the plan"},
            {"turn_end": "counted"}
        ]}),
        json!({"name": "surveyor"}),
    )
}

fn answered_in_place_on(
    s: &Station,
    page: &common::browser::Page,
    surveyor: &str,
    click: &dyn Fn(&str, &str),
    opt: &dyn Fn(&str) -> String,
) {
    // Under "Needs you" in the list, saying what kind of thing it asks; its
    // row (the address a notice carries) leads to the card.
    let row = "[...document.querySelectorAll('.phone-needs a')].find(r => r.innerText.includes('surveyor') && r.innerText.includes('asks you a question'))";
    click("the row under Needs you", row);
    page.wait(
        "the card, at its first question",
        "document.querySelectorAll('.chat-cards .chat-card.held .ask-opt').length === 3 && /^Waits here for 10 more minutes, then moves to the terminal\\.$/.test(document.querySelector('.ask-wait')?.innerText || '')",
    );
    assert_eq!(
        page.js("[document.querySelector('.ask-head').innerText.replace(/\\s+/g, ' ').trim(), document.querySelector('.ask-q').innerText,
                  [...document.querySelectorAll('.ask-opt b')].map(b => b.innerText), document.querySelectorAll('.chat-card.held img, .chat-card.held b b').length, !!window.__hit,
                  !!document.querySelector('.chat-card.held .ask-terminal'), !!document.querySelector('.chat-card.ask.terminal')]"),
        json!(["Species The agent asks · question 1 of 2", "Which herons count?",
               ["Grey herons only", "<img src=x onerror=\"window.__hit = 2\">All herons", "Other…"], 0, false, true, false])
    );
    // One choice, then the next question: several choices and one of the
    // owner's own.
    click("a choice", &opt("All herons"));
    page.wait(
        "the second question",
        "document.querySelector('.ask-head')?.innerText.includes('question 2 of 2') && document.querySelector('.ask-send')?.disabled === true",
    );
    click("Avon", &opt("Avon"));
    click("Other", &opt("Other"));
    page.wait("the field", "(o => o ? (o.value = 'Dart', o.dispatchEvent(new Event('input')), true) : false)(document.querySelector('.ask-other'))");
    click("Send answers", "(b => b && !b.disabled && b.innerText === 'Send answers' && b)(document.querySelector('.ask-send'))");
    s.wait_text(surveyor, "settled the question");
    let screen: String = s.text(surveyor).split_whitespace().collect();
    let answers = "\"answers\":{\"Whichheronscount?\":\"<imgsrc=xonerror=\\\"window.__hit=2\\\">Allherons\",\"Whichrivers?\":\"Avon,Dart\"}";
    assert_eq!(screen.matches(answers).count(), 1, "{screen}");
    // (Written after the agent has its answer: asked at once, it was not
    // there yet on a CI runner.)
    wait_for("the answer in the ship's log", 10, || {
        (!s.log("question.answered").is_empty()).then_some(())
    });
    assert_eq!(s.log("question.answered").len(), 1);

    // The plan takes its place: markdown, drawn as text; sent back with a
    // note, and the item says so.
    page.wait(
        "the plan's card",
        "!!document.querySelector('.chat-card.held.plan .ask-plan strong') && document.querySelectorAll('.chat-answer').length === 2",
    );
    assert_eq!(
        page.js("[document.querySelector('.ask-plan .md-h1').innerText, document.querySelector('.ask-plan strong').innerText, document.querySelectorAll('.chat-card.held img').length, !!window.__hit,
                  document.querySelector('.ask-plan').innerText.includes('<img src=x onerror='), [...document.querySelectorAll('.chat-answer')].map(a => a.innerText.replace(/\\s+/g, ' '))[1]]"),
        json!(["Tally plan", "grey", 0, false, true, "Which rivers? Avon, Dart"])
    );
    page.js("(n => (n.value = 'count the egrets too', n.dispatchEvent(new Event('input')), true))(document.querySelector('.ask-note'))");
    click("Keep planning", "document.querySelector('.ask-keep')");
    s.wait_text(surveyor, "settled the plan");
    assert!(
        s.text(surveyor)
            .split_whitespace()
            .collect::<String>()
            .contains("keepplanning,anddonotstartthework.Theowner'snote:counttheegretstoo"),
        "{}",
        s.text(surveyor)
    );
    // (That the item then reads "sent back" is looked at in `questions.rs`,
    // as its status, and in the pictures: this tier has no time to wait.)
}

/// The owner's request (15 Nov 2026): "Can the mobile change colors fonts
/// etc?" On a phone Appearance is a sheet, opened from the list and from a
/// pane's menu. A theme chosen there is the deck's, kept by the station; the
/// size of text is this device's own, one for a conversation and (a step
/// smaller) a terminal: it changes what is on screen, is still there after a
/// reload, is in nothing the station keeps, and is not used by the same
/// browser at a computer's width. (That a pane nobody else has open is
/// fitted to the phone, and one somebody has is not resized, is
/// `a_pane_on_a_phone_is_read_at_the_phones_text_size`.)
fn the_phones_appearance_sheet(s: &Station, page: &common::browser::Page, pane: &str) {
    let click = |page: &common::browser::Page, what: &str, el: &str| {
        page.wait(what, &format!("(e => e ? (e.click(), true) : false)({el})"));
    };
    let item = "[...document.querySelectorAll('.menu-item')].find(b => b.innerText.startsWith('Appearance'))";
    let step = |which: &str, times: u32| {
        format!("(b => {{ for (let i = 0; i < {times}; i += 1) b.click(); return true; }})(document.querySelector('.pl-size[data-size={which}] .pl-step:last-of-type'))")
    };
    let bg = "getComputedStyle(document.documentElement).getPropertyValue('--bg').trim()";
    let was = s.pane(pane);
    // From the list: the theme, for every device and station.
    click(page, "More", "document.querySelector('.top-more')");
    click(page, "Appearance", item);
    click(
        page,
        "the light theme",
        "document.querySelector('.phone-look .pl-theme[data-theme=spandeck-light]')",
    );
    page.wait("the page in the light theme, and the tick on it", &format!("{bg} === '#f4f6f8' && document.querySelector('.pl-theme[aria-checked=true]').dataset.theme === 'spandeck-light'"));
    let said = page.js("document.querySelector('.phone-look').innerText");
    for line in [
        "Changes the theme everywhere",
        "Only on this device",
        "is on a computer",
    ] {
        assert!(said.as_str().unwrap().contains(line), "{line}: {said}");
    }
    wait_for("the station to hold the theme", 10, || {
        (s.call("look.get", json!({}))["deck"]["theme"] == json!("spandeck-light")).then_some(())
    });
    click(page, "Done", "document.querySelector('.sheet-foot button')");

    // From a conversation's menu: the sizes, shown as they are changed.
    page.goto(&format!("#/s/{}/p/{pane}", s.name));
    let sizes = "[parseFloat(getComputedStyle(document.querySelector('.phone-chat .chat-text')).fontSize), parseFloat(getComputedStyle(document.querySelector('.phone-chat .chat-box')).fontSize)]";
    page.wait(
        "the conversation",
        "!!document.querySelector('.phone-chat .chat-text')",
    );
    assert_eq!(page.js(sizes), json!([15, 16]));
    click(
        page,
        "the pane's menu",
        "document.querySelector('.chat-more')",
    );
    click(page, "Appearance…", item);
    page.wait("the sheet", "!!document.querySelector('.phone-look')");
    page.js(&step("chat", 3));
    assert_eq!(
        page.js(&format!(
            "[{sizes}, [...document.querySelectorAll('.pl-value')].map(v => v.innerText)]"
        )),
        json!([[18, 18], ["18 px"]])
    );
    click(page, "Done", "document.querySelector('.sheet-foot button')");
    click(page, "Terminal", "[...document.querySelectorAll('.chat-switch button')].find(b => b.innerText === 'Terminal')");
    let term = "document.querySelector('.pane')?.paneTerm?.term.options.fontSize";
    page.wait(
        "the terminal at this phone's size",
        &format!("{term} === 14 && !!document.querySelector('.phone-pane .xterm-rows')"),
    );
    // A bug that happened (15 Nov 2026): a terminal left for the list and
    // opened again had its bar twice.
    page.goto("#/fleet");
    page.wait("the list", "!document.querySelector('.phone-pane')");
    page.goto(&format!("#/s/{}/p/{pane}", s.name));
    page.wait("the terminal again, under one bar", "!!document.querySelector('.phone-pane .xterm-rows') && document.querySelectorAll('.phone-pane .chat-bar').length === 1");

    // Kept in this browser, and by no station.
    page.reload();
    page.wait(
        "the sizes after a reload",
        &format!("document.readyState === 'complete' && {term} === 14 && getComputedStyle(document.documentElement).getPropertyValue('--chat-scale') === '1.2'"),
    );
    // A bug that happened (15 Nov 2026): a pane wider than the phone, shown
    // whole and shrunk, was drawn off to the right of its frame. (Since 16
    // Nov it is fitted to the phone, nobody else having it open.)
    page.wait(
        "the terminal inside its frame",
        "((a, b) => !!a && a.left >= b.left - 1 && a.right <= b.right + 1 && a.width > b.width / 2)(document.querySelector('.phone-pane .xterm-screen')?.getBoundingClientRect(), document.querySelector('.phone-pane .term-body').getBoundingClientRect())",
    );
    let look = s.call("look.get", json!({}));
    assert_eq!(look["deck"]["theme"], json!("spandeck-light"));
    assert!(
        look["deck"]["set"].as_object().is_none_or(|m| m.is_empty())
            && !look.to_string().contains("size"),
        "a size reached the station: {look}"
    );
    // Fitted to the phone at that size: fewer columns than it had.
    wait_for("the pane fitted to the phone", 10, || {
        let held = s.pane(pane);
        (held["cols"].as_u64() < was["cols"].as_u64() && !held["controller"].is_null())
            .then_some(())
    });
    // Another page of the same browser, at a computer's width: the theme,
    // and the deck's sizes.
    page.js(&format!(
        "(window.open(location.origin + '/#/s/{}/p/{pane}', '_blank'), true)",
        s.name
    ));
    let wide = page.opened("/p/");
    wide.tab
        .call_method(
            headless_chrome::protocol::cdp::Emulation::SetDeviceMetricsOverride {
                width: 1300,
                height: 800,
                device_scale_factor: 1.0,
                mobile: false,
                scale: None,
                screen_width: None,
                screen_height: None,
                position_x: None,
                position_y: None,
                dont_set_visible_size: None,
                screen_orientation: None,
                viewport: None,
                display_feature: None,
                device_posture: None,
            },
        )
        .unwrap();
    wide.wait("the pane on a wide screen, in the deck's size", &format!("innerWidth > 760 && {bg} === '#f4f6f8' && {term} === 13 && !document.documentElement.dataset.chatSize"));
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

/// The owner's reports (16 Nov 2026, from an iPhone): "the appearance did not
/// take effect, the colors did but not font", then, of a conversation beside
/// a 120-column terminal shrunk to the phone's width, "Two different windows
/// with 2 differ t fonts , should be mobile wide font", and "the Claude chat
/// screen scrolls really nice but the antigravity terminal doesn't".
///
/// On a phone every pane is read at the phone's one text size, in either
/// face. A pane another window has open keeps its size and is drawn at that
/// text size, scrolled sideways, with the reason said; a pane nobody has
/// open is fitted to the phone by itself, from its conversation or its
/// terminal, and gets its size back when the phone leaves it. The size and
/// the font chosen in the phone's sheet reach the open terminal, one opened
/// afterwards, the conversation and what a pane printed, are there after a
/// reload, and the font is kept by the station. A drag over a terminal
/// follows the finger by the pixel and goes on after it lifts.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn a_pane_on_a_phone_is_read_at_the_phones_text_size() {
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
    let (wide, other) = (spawn("wide"), spawn("other"));
    let click = |page: &Page, what: &str, el: &str| {
        page.wait(what, &format!("(e => e ? (e.click(), true) : false)({el})"));
    };
    let button = |text: &str| {
        format!("[...document.querySelectorAll('.phone-pane button, .phone-chat .chat-switch button')].find(b => b.innerText.trim() === '{text}')")
    };
    let cols = |pane: &str| s.pane(pane)["cols"].as_u64().unwrap();
    let holders = |pane: &str| s.pane(pane)["holders"].as_array().map_or(99, |h| h.len());

    // A computer has the pane open, and so holds its size.
    let desk = Page::open(&s, 1300, 800);
    desk.goto(&format!("#/s/alpha/p/{wide}"));
    desk.wait(
        "the pane on the computer",
        "!!document.querySelector('.pane .xterm-rows')",
    );
    let held = wait_for("the computer to hold the pane", 10, || {
        let p = s.pane(&wide);
        (holders(&wide) == 1 && !p["controller"].is_null() && cols(&wide) > 100)
            .then(|| cols(&wide))
    });
    s.call(
        "pane.paste",
        json!({"pane": wide, "text": "echo printed-$((40+2))", "enter": true}),
    );
    s.wait_text(&wide, "printed-42");

    // The phone: the pane is not resized, and not shrunk either.
    let phone = Page::open(&s, 390, 844);
    phone.goto(&format!("#/s/alpha/p/{wide}"));
    let term = "document.querySelector('.phone-pane .pane')?.paneTerm";
    let drawn = "(r => r && [parseFloat(getComputedStyle(r).fontSize), getComputedStyle(r).fontFamily.split(',')[0].trim()])(document.querySelector('.phone-pane .xterm-rows'))";
    let seen = phone.wait(
        "the pane at the phone's text size, scrolling sideways, with the reason",
        &format!("(b => !!b && b.classList.contains('natural') && b.scrollWidth > b.clientWidth + 100 && [document.querySelector('.fit-note')?.innerText || '', getComputedStyle(b.querySelector('.xterm')).transform, {term}.term.options.fontSize, {drawn}[0]])(document.querySelector('.phone-pane .term-body'))"),
    );
    assert!(
        seen[0].as_str().unwrap().contains("Fit takes its size") && seen[1] == json!("none"),
        "{seen}"
    );
    // The default: a terminal's text is at least 11 px on a phone.
    assert_eq!(
        (seen[2].as_f64(), seen[3].as_f64()),
        (Some(12.0), Some(12.0)),
        "{seen}"
    );
    assert_eq!(
        (cols(&wide), holders(&wide)),
        (held, 1),
        "a phone resized a pane a computer has open"
    );
    // The whole width, shrunk, is still there to ask for.
    click(&phone, "Show whole width", &button("Show whole width"));
    phone.wait("the pane whole", "(b => b.classList.contains('letterbox') && getComputedStyle(b.querySelector('.xterm')).transform !== 'none')(document.querySelector('.phone-pane .term-body'))");
    click(&phone, "Show at text size", &button("Show at text size"));
    phone.wait(
        "the pane at text size again",
        "document.querySelector('.phone-pane .term-body').classList.contains('natural')",
    );

    // The sheet: one size for everything, and the terminal's font.
    click(
        &phone,
        "the pane's menu",
        "document.querySelector('.chat-more')",
    );
    click(&phone, "Appearance…", "[...document.querySelectorAll('.menu-item')].find(b => b.innerText.startsWith('Appearance'))");
    phone.wait(
        "the sheet",
        "!!document.querySelector('.phone-look .pl-font')",
    );
    phone.js("(b => { for (let i = 0; i < 3; i += 1) b.click(); return true; })(document.querySelector('.pl-size .pl-step:last-of-type'))");
    click(
        &phone,
        "each device's own monospace",
        "document.querySelector('.pl-font[data-font=system]')",
    );
    wait_for("the station to keep the font", 10, || {
        (s.call("look.get", json!({}))["deck"]["set"]["font.family"]
            == json!(["ui-monospace", "monospace"]))
        .then_some(())
    });
    assert_eq!(
        phone.js("[document.querySelector('.pl-value').innerText, document.querySelectorAll('.pl-size').length, document.querySelector('.pl-mono-note').innerText.includes('14 px')]"),
        json!(["18 px", 1, true])
    );
    click(
        &phone,
        "Done",
        "document.querySelector('.sheet-foot button')",
    );
    // The terminal that was open.
    phone.wait(
        "the open terminal in that size and font",
        &format!("JSON.stringify({drawn}) === '[14,\"ui-monospace\"]' && {term}.term.options.fontFamily.startsWith('ui-monospace, monospace')"),
    );
    // Its conversation, made from its screen: what was sent, and what the
    // pane printed, which is text set like the terminal's.
    click(&phone, "Chat", &button("Chat"));
    let faces = "(q => [q('.chat-bubble'), q('.chat-screen .chat-mono pre'), q('.chat-box')].map(e => e && [parseFloat(getComputedStyle(e).fontSize), getComputedStyle(e).fontFamily.split(',')[0].trim()]))(x => document.querySelector('.phone-chat ' + x))";
    phone.wait(
        "what the pane printed, in the conversation",
        "(document.querySelector('.phone-chat .chat-screen .chat-mono pre')?.innerText || '').includes('printed-42')",
    );
    let got = phone.js(faces);
    assert_eq!(
        (got[0][0].as_f64(), &got[1], got[2][0].as_f64()),
        (Some(18.0), &json!([14, "ui-monospace"]), Some(18.0)),
        "{got}"
    );
    assert_eq!(
        cols(&wide),
        held,
        "the conversation resized a pane a computer has open"
    );

    // The computer lets go of the pane: now fitting it takes its size from
    // nobody, and the conversation asks for the phone's width.
    desk.goto("#/fleet");
    let fitted = wait_for("the pane at the phone's width", 15, || {
        (cols(&wide) < 60).then(|| cols(&wide))
    });
    assert!(fitted >= 20, "{fitted} columns");
    // As its terminal it is fitted and in control, and fills its frame.
    click(&phone, "Terminal", &button("Terminal"));
    phone.wait(
        "the terminal fitted to the phone",
        &format!("(t => !!t && t.control && t.chan !== null && t.term.cols < 60 && !t.body.classList.contains('natural') && !t.body.classList.contains('letterbox') && t.body.querySelector('.xterm-screen').offsetHeight > t.body.clientHeight * 0.8 && {}?.getAttribute('aria-pressed') === 'true' && !document.querySelector('.fit-note'))({term})", button("Fitted")),
    );
    wait_for("the station to say the phone holds it", 10, || {
        let p = s.pane(&wide);
        (holders(&wide) == 1 && !p["controller"].is_null() && cols(&wide) < 60).then_some(())
    });

    // A drag over a full-screen program is followed by the page and passed
    // on to it: the arithmetic of that (touch.js). The history of any other
    // program is scrolled by the phone itself (phonescroll.rs).
    let sums = phone.js(
        "import('./touch.js').then(t => [t.axisOf(3, 4, true), t.axisOf(30, 4, true), t.axisOf(30, 4, false), t.axisOf(4, 30, true),
           t.linesOf(40, 17), t.linesOf(-9, 17),
           t.speedOf([{t: 0, x: 0, y: 0}, {t: 50, x: 0, y: 50}], 55, 'y'), t.speedOf([{t: 0, x: 0, y: 0}, {t: 50, x: 0, y: 50}], 400, 'y'), t.speedOf([{t: 0, x: 0, y: 0}, {t: 2, x: 0, y: 500}], 3, 'y'),
           Math.round(t.glide(1, 1e6).moved), Math.round(t.glide(1, 16).moved + t.glide(t.glide(1, 16).speed, 1e6).moved)])",
    );
    assert_eq!(
        sums,
        json!([null, "x", "y", "y", {"whole": 2, "rest": 6}, {"whole": 0, "rest": -9}, 1, 0, 0, 325, 325]),
        "the arithmetic of a drag"
    );

    // (What is to be given back is written down when the page is next
    // redrawn, within a few seconds of the terminal taking its size: a
    // reload before that gives nothing back. Found when this test stopped
    // spending those seconds on a drag.)
    phone.wait(
        "the size to give back, kept for a reload",
        &format!("(t => Object.values(JSON.parse(sessionStorage.getItem('spandeck.phonefit.before') || '{{}}')).some(k => k.mc === t.size.cols && k.mr === t.size.rows))({term})"),
    );
    // After a reload: the size and the font, and the pane fitted again.
    phone.reload();
    phone.wait(
        "the size and the font after a reload",
        &format!("document.readyState === 'complete' && !!{term} && {term}.control && JSON.stringify({drawn}) === '[14,\"ui-monospace\"]'"),
    );
    // A terminal opened afterwards has them; and the pane the phone left
    // has the size it had before the phone fitted it.
    phone.goto(&format!("#/s/alpha/p/{other}"));
    phone.wait(
        "another terminal in that size and font, fitted",
        &format!("location.hash.endsWith('{other}') && !!{term} && {term}.pane.id === '{other}' && {term}.control && JSON.stringify({drawn}) === '[14,\"ui-monospace\"]'"),
    );
    wait_for("the pane the phone left to have its size back", 10, || {
        (cols(&wide) == held && holders(&wide) == 0).then_some(())
    });
    // "Fitted" turned off is remembered for that pane, and said.
    wait_for("the other pane fitted", 10, || {
        (cols(&other) < 60).then_some(())
    });
    click(&phone, "Fitted", &button("Fitted"));
    let off = phone.wait(
        "the pane not fitted, and why",
        &format!("(n => !!n && {term}.body.classList.contains('natural') && [n.innerText, localStorage.getItem('spandeck.phonefit.off')])(document.querySelector('.fit-note'))"),
    );
    assert!(
        off[0].as_str().unwrap().contains("you turned that off")
            && off[1].as_str().unwrap().contains(other.as_str()),
        "{off}"
    );
    wait_for("its size given back", 10, || {
        (cols(&other) >= 80).then_some(())
    });
}

/// A terminal's history on a phone is scrolled by the phone (DECISIONS.md,
/// "On a phone a terminal's history is scrolled by the phone"; the owner on
/// an iPhone, 16 Nov 2026: "all terminals are choppy still, not like the
/// chat window which is smooth.. have to fix terminal to be like chat"). No
/// iPhone can be driven from here. What is shown is the structure that makes
/// the chat smooth: rows that stay as drawn, in an element the browser
/// scrolls itself, with a real touch; no move of the finger refused; and the
/// terminal drawing nothing meanwhile. The rest is in phonescroll.rs.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn a_terminals_history_on_a_phone_is_scrolled_by_the_phone() {
    use common::browser::{Page, T, TOP_ROW, WATCH};
    let paste = |s: &Station, pane: &str, text: &str| {
        s.call(
            "pane.paste",
            json!({"pane": pane, "text": text, "enter": true}),
        );
    };
    let drag = |page: &Page, dx: f64, dy: f64, steps: u32| {
        page.touch_drag(&format!("{T}.body"), dx, dy, steps);
    };
    let s = Station::start("alpha");
    let pane = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "hist", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let phone = Page::open(&s, 390, 844);
    phone.phone_screen();
    // The owner, 17 Nov 2026: "when I try and hit a up arrow in a terminal
    // to quick instead of taking it as another up arrow, it reading it as a
    // double tap on the app to enlarge the screen". Two taps are two taps,
    // everywhere on the page.
    assert_eq!(
        phone.js("getComputedStyle(document.documentElement).touchAction"),
        json!("manipulation")
    );
    phone.touches();
    phone.goto(&format!("#/s/alpha/p/{pane}"));
    phone.wait(
        "the terminal fitted to the phone",
        &format!("(t => !!t && t.control && t.chan !== null && t.term.cols < 60 && t.body.clientHeight > 350)({T})"),
    );
    // Colours, a line longer than the phone is wide, something that would be
    // markup if it were put in as markup, and four hundred lines.
    paste(
        &s,
        &pane,
        "printf '\\033[31mredword\\033[0m plain \\033[1mboldword\\033[0m \\033[38;5;208morange256\\033[0m \\033[48;2;1;2;3mrgbbg\\033[0m\\n'; echo '<img src=x onerror=alert(1)>'; printf 'w%.0s' $(seq 1 300); echo; seq 1000 1400",
    );
    s.wait_text(&pane, "1400");
    phone.wait(
        "the history as rows, and the live screen in sight",
        &format!("(t => t.following && t.hist.lines > 350 && !t.hist.pending() && Math.abs(t.body.scrollTop - t._metrics().liveTop) < 1)({T})"),
    );

    // The element that holds them is one the browser scrolls, and the
    // terminal's own scrolling element is not.
    // (Contained: without that the page laid out every row of the history
    // again each second, when the pane's strip was redrawn; measured, see
    // docs/measurements.md.)
    let is = phone.js(&format!(
        "(t => (b => [getComputedStyle(b).overflowY, getComputedStyle(b).touchAction, b.scrollHeight > b.clientHeight + 1000, b.scrollTop === b.scrollHeight - b.clientHeight,
            getComputedStyle(b.querySelector('.xterm-viewport')).overflowY, b.scrollWidth <= b.clientWidth, t.hist.el.nextElementSibling === t.term.element, getComputedStyle(b).contain])(t.body))({T})"
    ));
    assert_eq!(
        is,
        json!([
            "auto",
            "manipulation",
            true,
            true,
            "hidden",
            true,
            true,
            "strict"
        ]),
        "the element that scrolls"
    );
    // Nothing from the text under a finger up to the page refuses a pan.
    let refusing = phone.js(&format!(
        "(t => {{ const out = []; for (let e = t.hist.el.querySelector('.hist-row'); e; e = e.parentElement) if (!/^(auto|manipulation)$/.test(getComputedStyle(e).touchAction)) out.push(e.className + ': ' + getComputedStyle(e).touchAction); return out; }})({T})"
    ));
    assert_eq!(refusing, json!([]), "elements that refuse a pan");
    // A row of history is set as a row of the terminal is.
    let set = "(r => (c => [c.fontFamily, c.fontSize, c.height, c.lineHeight, c.whiteSpace])(getComputedStyle(r)))";
    let rows = phone.js(&format!(
        "(t => [{set}(t.hist.el.querySelector('.hist-row')), {set}(t.body.querySelector('.xterm-rows > div'))])({T})"
    ));
    assert_eq!(
        rows[0], rows[1],
        "a row of history, and a row of the terminal"
    );
    assert!(rows[0][1] == json!("12px"), "{rows}");
    // Its colours and its bold are the terminal's; what was printed is text.
    let look = phone.js(&format!(
        "(t => {{ const probe = document.createElement('span'); t.body.append(probe);
            const as = (prop, value) => {{ probe.style[prop] = value; return getComputedStyle(probe)[prop]; }};
            const span = (text) => [...t.hist.el.querySelectorAll('.hist-row span')].find(x => x.textContent === text);
            const row = [...t.hist.el.querySelectorAll('.hist-row')].find(r => r.textContent.startsWith('redword'));
            const out = [row.textContent, getComputedStyle(span('redword')).color === as('color', t.term.options.theme.red), getComputedStyle(span('boldword')).fontWeight === as('fontWeight', t.term.options.fontWeightBold),
              getComputedStyle(row).fontWeight === as('fontWeight', t.term.options.fontWeight), getComputedStyle(span('orange256')).color, getComputedStyle(span('rgbbg')).backgroundColor,
              [...t.hist.el.querySelectorAll('.hist-row')].some(r => r.textContent === '<img src=x onerror=alert(1)>'), t.hist.el.querySelector('img')];
            probe.remove(); return out; }})({T})"
    ));
    assert_eq!(
        look,
        json!([
            "redword plain boldword orange256 rgbbg",
            true,
            true,
            true,
            "rgb(255, 135, 0)",
            "rgb(1, 2, 3)",
            true,
            null
        ]),
        "colours, bold, and text as text"
    );

    // A finger moves it: the browser scrolls, no move is refused, and the
    // terminal draws nothing and is not scrolled itself.
    let before = phone.js(&format!("{T}.body.scrollTop")).as_f64().unwrap();
    phone.js(&format!("{WATCH}({T})"));
    drag(&phone, 0.0, 300.0, 20);
    phone.wait(
        "the view at rest, on earlier output",
        &format!("(t => !t.following && !!t._read)({T})"),
    );
    let after = phone.js(&format!(
        "(t => [t.body.scrollTop, seen.moves.length, seen.moves.some(e => e.defaultPrevented), seen.changed, seen.drawn, t.term.buffer.active.viewportY === t.term.buffer.active.baseY, t.pill.hidden])({T})"
    ));
    assert!(
        after[0].as_f64().unwrap() < before - 200.0,
        "a drag of 300 px moved it from {before} to {after}"
    );
    assert!(after[1].as_u64().unwrap() >= 10, "{after}");
    assert_eq!(
        json!(after.as_array().unwrap()[2..]),
        json!([false, 0, 0, true, true]),
        "moves refused, changes to the terminal's rows, times it drew, its own lines at their end, no button: {after}"
    );

    // The pane prints while earlier output is being read: nothing moves, and
    // a button says so.
    let reading = phone.js(&format!("(t => [t.body.scrollTop, {TOP_ROW}(t)])({T})"));
    paste(&s, &pane, "seq 2000 2100");
    s.wait_text(&pane, "2100");
    phone.wait(
        "the button for new output",
        &format!("(t => !t.pill.hidden && t.pill.offsetHeight > 0 && t.hist.lines > 450 && t.body.querySelector('.xterm-rows').innerText.includes('2100'))({T})"),
    );
    assert_eq!(
        phone.js(&format!("(t => [t.body.scrollTop, {TOP_ROW}(t)])({T})")),
        reading,
        "new output moved what was being read"
    );

    // The phone is turned narrower: the lines are wrapped again, and the line
    // that was being read is still at the top.
    let wrapped = "(t => [t.term.cols, [...t.hist.el.querySelectorAll('.hist-row')].filter(r => /^w+$/.test(r.textContent)).length, t.hist.made])";
    let wide = phone.js(&format!("{wrapped}({T})"));
    phone.protocol(
        "Emulation.setDeviceMetricsOverride",
        json!({"width": 320, "height": 844, "deviceScaleFactor": 1, "mobile": false}),
    );
    let made = wide[2].as_u64().unwrap();
    let narrow = phone.wait(
        "the lines wrapped again, and the reading line in place",
        &format!("(t => t.term.cols < {} && t.hist.made > {made} && !t.hist.pending() && {TOP_ROW}(t) === {} && !t.following && {wrapped}(t))({T})", wide[0], reading[1]),
    );
    let rows_of = |v: &Value| {
        (
            v[1].as_u64().unwrap(),
            300_u64.div_ceil(v[0].as_u64().unwrap()),
        )
    };
    assert_eq!(rows_of(&wide).0, rows_of(&wide).1, "{wide}");
    assert_eq!(rows_of(&narrow).0, rows_of(&narrow).1, "{narrow}");
    assert!(rows_of(&narrow).0 > rows_of(&wide).0, "{wide} {narrow}");

    // The button leads back to the live screen, and there new output keeps
    // it in sight.
    phone.js(&format!("{T}.pill.click()"));
    paste(&s, &pane, "seq 3000 3100");
    s.wait_text(&pane, "3100");
    phone.wait(
        "the live screen in sight after new output",
        &format!("(t => t.following && t.pill.hidden && t.hist.lines > 550 && Math.abs(t.body.scrollTop - t._metrics().liveTop) < 1 && t.body.querySelector('.xterm-rows').innerText.includes('3100'))({T})"),
    );

    // A full-screen program keeps its own history: there is none of ours in
    // the element, and a drag is passed on to the program (arrow keys, for
    // one that does not listen for the mouse).
    // Its screen is full of coloured text, and the finger is on it: the
    // terminal makes those pieces of text again when it draws, and a drag
    // that began on one used to stop after its first line. The finger is on
    // the screen itself, which stays.
    paste(
        &s,
        &pane,
        "printf '\\033[?1049h\\033[H'; i=0; while [ $i -lt 40 ]; do printf '\\033[32mgreen green green green green green green green\\033[0m\\n'; i=$((i+1)); done; cat -v",
    );
    let under = phone.wait(
        "the full-screen program alone",
        &format!("(t => t._alt && t._driven && getComputedStyle(t.hist.el).display === 'none' && getComputedStyle(t.body).touchAction === 'none' && t.body.scrollHeight <= t.body.clientHeight + 1 && t.body.querySelector('.xterm-rows').innerText.includes('green green') &&
            (r => (e => [!!e.closest('.xterm'), !!e.closest('.xterm-rows')])(document.elementFromPoint(r.left + 60, r.top + 40)))(t.body.getBoundingClientRect()))({T})"),
    );
    assert_eq!(
        under,
        json!([true, false]),
        "what a finger on the text touches"
    );
    drag(&phone, 0.0, -150.0, 10);
    // Ten lines of drag: five keys, not the one of the first move.
    s.wait_text(&pane, "^[[B^[[B^[[B^[[B");
    // It ends: the history is back, with the live screen in sight, and the
    // browser scrolls it again.
    s.call("pane.input", json!({"pane": pane, "data": "\u{3}"}));
    paste(&s, &pane, "printf '\\033[?1049l'");
    phone.wait(
        "the history again",
        &format!("(t => !t._alt && !t._driven && t.following && getComputedStyle(t.body).touchAction === 'manipulation' && t.hist.lines > 550 && Math.abs(t.body.scrollTop - t._metrics().liveTop) < 1)({T})"),
    );
    let before = phone.js(&format!("{T}.body.scrollTop")).as_f64().unwrap();
    drag(&phone, 0.0, 200.0, 12);
    phone.wait(
        "the history scrolled by the browser again",
        &format!("(t => !t.following && t.body.scrollTop < {before} - 100)({T})"),
    );
    // A tap on earlier output is not a tap on the terminal: it takes the
    // keyboard for nothing.
    drag(&phone, 0.0, 300.0, 12);
    phone.wait(
        "nothing but earlier output in sight",
        &format!("(t => !t.following && !!t._read && t.body.scrollTop < t._metrics().liveTop - t.body.clientHeight)({T})"),
    );
    let focused = "document.activeElement?.classList.contains('xterm-helper-textarea') || false";
    drag(&phone, 0.0, 0.0, 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(phone.js(focused), json!(false), "a tap on earlier output");
    // A tap on the live screen takes it, as it always did; and what is
    // typed is typed at the end, wherever the view was.
    phone.js(&format!("{T}.toEnd()"));
    drag(&phone, 0.0, 0.0, 1);
    phone.wait("the keyboard for the terminal", focused);
    drag(&phone, 0.0, 250.0, 12);
    phone.wait(
        "earlier output, with the keyboard still the terminal's",
        &format!("(t => !t.following && {focused})({T})"),
    );
    phone.press_key("x");
    phone.wait(
        "the live screen after a key",
        &format!(
            "(t => t.following && Math.abs(t.body.scrollTop - t._metrics().liveTop) < 1)({T})"
        ),
    );
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
        json!([["lead from proj, idle", "scratch, idle"], "all › lead (from proj) · 1 of 2"])
    );
    page.goto("#/fleet");
    page.wait(
        "the layout's row under its station",
        "(r => r ? (r.click(), true) : false)(document.querySelector('main.phone .phone-station .lay-row'))",
    );
    // The layout's own controls are one line under the pane's bar; the map
    // opens from it, over the pane.
    page.wait(
        "the line, with the map closed",
        "document.querySelectorAll('.lay-map button').length === 3 && !!document.querySelector('.phone-layout .composer-box') && document.querySelector('.lay-panel').hidden",
    );
    page.js("(document.querySelector('.lay-where').click(), true)");
    page.wait(
        "the map",
        "document.querySelector('.lay-where').getAttribute('aria-expanded') === 'true' && document.querySelector('.lay-map button').getBoundingClientRect().height > 0",
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
    assert_eq!(page.js(showing), json!("build › dev · 1 of 3"));

    // Tap the second box: that pane is shown, the map closes, and the
    // message box is its.
    page.js("document.querySelectorAll('.lay-map button')[1].click()");
    page.wait(
        "the second pane",
        &format!(
            "{showing} === 'build › dev2 · 2 of 3' && document.querySelector('.lay-panel').hidden && document.querySelector('.phone-layout .pane')?.dataset.pane === {} && !!document.querySelector('.phone-layout .xterm-rows')",
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
        &format!("{showing} === 'build › lead · 3 of 3'"),
    );

    // The same station and the same phone, for the conversation view.
    the_conversation_view(&s, &page);
    the_pane_of_a_layout_is_the_pane_itself(&s, &page, loose["id"].as_str().unwrap());
}

/// The owner's report (16 Nov 2026), from a phone: "the window on mobile in a
/// layout is the old style vs a pane not in a layout. The panes in the layout
/// should work like the single panes." In a layout of an agent and a shell,
/// at a phone's width: the agent's pane is its conversation, under the same
/// bar, with its card and its message box; Chat and Terminal are one tap
/// apart and the draft goes along; the shell is its terminal, and the bar
/// says why; stepping away stops watching the conversation and lets go of
/// the terminal, and stepping back finds the conversation where it was
/// scrolled to, with what was being written. Chat sets no size.
fn the_pane_of_a_layout_is_the_pane_itself(s: &Station, page: &common::browser::Page, shell: &str) {
    let line = |v: Value| json!({"transcript": v});
    let said = |blocks: Value| {
        line(json!({"type": "assistant", "message": {"role": "assistant", "content": blocks}}))
    };
    let tallier = s.fake(
        json!({"steps": [
            {"prompt": "tally the wrens"},
            said(json!([{"type": "text", "text": "A line of the tally.\n\n".repeat(40) + "**Forty** lines."}])),
            said(json!([{"type": "tool_use", "id": "toolu_w", "name": "Bash", "input": {"command": "rm wrens.csv"}}])),
            {"permission": {"name": "Bash", "command": "rm wrens.csv"}},
            {"turn_end": "the wrens are tallied"}
        ]}),
        json!({"name": "tallier"}),
    );
    s.wait_state(&tallier, "needs_you");
    s.call(
        "deck.layout.save",
        json!({"layout": {"name": "pair", "tree": {"dir": "row", "ratio": 500,
            "a": {"id": tallier, "station": "alpha"}, "b": {"id": shell, "station": "alpha"}}}}),
    );
    // (A look at the page costs a tenth of a second here, so each one below
    // waits for what the step before led to, and takes the next step.)
    let face = |to: &str| {
        format!("[...document.querySelectorAll('.phone-layout > .chat-bar .chat-switch button')].find(b => b.innerText === '{to}')")
    };
    let showing = "document.querySelector('.lay-showing')?.innerText";
    let log = "document.querySelector('.phone-layout .chat-log')";
    // What the page asks the station to watch and to attach, and to let go
    // of, counted from here: the list, where nothing is watched.
    let watched = "((c, n) => n(c['pane.conversation.watch']) - n(c['pane.conversation.unwatch']) + ' watched, ' + (n(c['pane.attach']) - n(c['pane.detach'])) + ' attached')(window.__sent, x => x || 0)";
    page.goto("#/fleet");
    page.js("(window.__sent = {}, WebSocket.prototype.send = (send => function (m) { try { const k = JSON.parse(m).method; window.__sent[k] = (window.__sent[k] || 0) + 1; } catch (e) { /* not text */ } return send.call(this, m); })(WebSocket.prototype.send), location.hash = '#/l/pair', true)");

    // The agent's pane: its conversation under its own bar, and the request
    // its hook holds as a card, said once. Allowed there.
    let seen = page.wait(
        "the conversation in the layout",
        &format!("{showing} === 'pair › tallier · 1 of 2' && !!document.querySelector('.phone-layout .chat-text strong') && !!document.querySelector('.phone-layout .chat-box') && (allow => allow ? [document.querySelectorAll('.phone-layout .xterm, .phone-layout .composer-box').length, document.querySelectorAll('.lay-approvals .approval').length, document.querySelector('.phone-layout > .chat-bar .chat-title b').innerText, !!document.querySelector('.phone-layout > .chat-bar .chat-more'), allow.click()] : null)([...document.querySelectorAll('.phone-layout .chat-cards .approval button')].find(b => b.innerText === 'Allow'))"),
    );
    assert_eq!(seen, json!([0, 0, "tallier", true, null]));
    s.wait_text(&tallier, "[fake] permission: allow");

    // Terminal and back: the same switch, and the draft goes along.
    page.wait(
        "the reply, then Terminal with something half written",
        &format!("[...document.querySelectorAll('.phone-layout .chat-text')].some(t => t.innerText.includes('the wrens are tallied')) && !document.querySelector('.chat-cards .approval') && (b => (b.value = 'wrens, then', b.dispatchEvent(new Event('input')), {}.click(), true))(document.querySelector('.chat-box'))", face("Terminal")),
    );
    page.wait(
        "the terminal with the draft, then Chat",
        &format!("!!document.querySelector('.phone-layout .xterm-rows') && document.querySelector('.phone-layout .composer-box')?.value === 'wrens, then' && !document.querySelector('.phone-layout .chat-log') && ({}.click(), true)", face("Chat")),
    );
    page.wait(
        "the conversation with the draft, then Send",
        "document.querySelector('.phone-layout .chat-box')?.value === 'wrens, then' && !document.querySelector('.phone-layout .xterm') && (document.querySelector('.chat-send').click(), true)",
    );
    s.wait_text(&tallier, "[fake] got: wrens, then");

    // Scrolled up, and something half written: then on to the shell. It
    // opens as its terminal, with the same choice on its bar, and nothing of
    // the agent's pane is watched or attached any more (what is watched is
    // the shell's own conversation, as for any pane on screen).
    page.wait(
        "the message as a bubble, then the next pane",
        &format!("document.querySelectorAll('.phone-layout .chat-user').length === 2 && document.querySelector('.chat-box').value === '' && (async () => {{ {log}.scrollTop = 40; await new Promise(r => setTimeout(r, 60)); window.__top = {log}.scrollTop; const b = document.querySelector('.chat-box'); b.value = 'kept'; b.dispatchEvent(new Event('input')); document.querySelector('.lay-nav .lay-step:last-child').click(); return true; }})()"),
    );
    let shell_seen = page.wait(
        "the shell's terminal, then the pane before",
        &format!("{showing} === 'pair › scratch · 2 of 2' && !!document.querySelector('.phone-layout .xterm-rows') && !document.querySelector('.phone-layout .chat-log') && {watched} === '1 watched, 1 attached' && document.querySelectorAll('.phone-layout > .chat-bar .chat-switch button').length === 2 && [document.querySelector('.phone-layout > .chat-bar .chat-switch button.on').innerText, !!document.querySelector('.phone-layout .composer .keys'), document.querySelector('.lay-nav .lay-step').click()]"),
    );
    assert_eq!(shell_seen, json!(["Terminal", true, null]));
    // (Where it was scrolled to is whatever the browser made of 40: with
    // Windows' fonts the conversation may be too short to scroll that far.)
    page.wait(
        "the conversation where it was",
        &format!("{showing} === 'pair › tallier · 1 of 2' && Math.abs({log}?.scrollTop - window.__top) <= 2 && document.querySelector('.chat-box').value === 'kept' && {watched} === '1 watched, 0 attached'"),
    );
    // Its terminal was fitted to the phone while it showed, and the pane
    // was given its size back when the layout stepped on; its conversation,
    // which is the agent's own and not made from the screen, resizes nothing.
    wait_for("the pane at the size it had", 10, || {
        (s.pane(&tallier)["cols"].as_u64() == Some(120)).then_some(())
    });
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

    // ---- "Try again" after the step that asks (the owner's report of 17
    // Nov 2026: the join failed on a machine spandeck had just been
    // installed on). Here the request is refused, which ends the add at the
    // same step. The dialog is told what had worked, and trying again only
    // asks again: the machine is connected to, nothing is downloaded or
    // installed, and a new code arrives.
    a.call("trust.deny", json!({"request": v["request"]}));
    let v = rest_at(&a, &["failed"]);
    let problem = &v["problem"];
    assert_eq!(
        (&problem["kind"], &v["can_ask_again"], &problem["worked"]),
        (
            &json!("refused"),
            &json!(true),
            &json!(format!(
                "spandeck {} was installed on charlie and is running.",
                env!("CARGO_PKG_VERSION")
            ))
        ),
        "{v}"
    );
    assert!(
        problem["next"]
            .as_str()
            .unwrap()
            .starts_with("Try again: it only asks again, and nothing is downloaded or installed a second time. Or on that machine: `spandeck fleet --join <any station of your fleet>` (its stations: alpha)."),
        "{v}"
    );
    let binary = std::fs::metadata(at("charlie/bin/spandeck"))
        .unwrap()
        .modified()
        .unwrap();
    a.call("fleet.add", json!({"again": true, "user": "sam"}));
    rest_at(&a, &["needs_password"]);
    a.call(
        "fleet.add",
        json!({"again": true, "user": "sam", "password": RIGHT}),
    );
    let v = rest_at(&a, &["approve"]);
    assert_eq!(
        (&v["kind"], &v["rejoin"], &v["station"]),
        (&json!("join"), &json!(true), &json!("charlie")),
        "{v}"
    );
    let said = v["lines"].to_string();
    assert!(
        !said.contains("Downloading") && !said.contains("Installed spandeck"),
        "{said}"
    );
    assert_eq!(
        std::fs::metadata(at("charlie/bin/spandeck"))
            .unwrap()
            .modified()
            .unwrap(),
        binary
    );

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
    dialog_says("the password to be asked for", "Password on charlie");
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
    // From the station's menu, once the page has heard that charlie is in
    // the fleet (its tile no longer offers to add it): the dialog says what
    // the page knows. The dialog names the station, says what happens in
    // order, and opens on Cancel.
    page.wait(
        "charlie's tile as a station of the fleet",
        "(t => !!t && !t.querySelector('button.add-station'))([...document.querySelectorAll('a.tile.station')].find(x => x.innerText.includes('charlie')))",
    );
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
    // (The notice follows the request by a moment, on a loaded machine.)
    let notes = wait_for_or(
        "the notice of the request",
        10,
        || c.call("notify.list", json!({})).to_string(),
        || {
            let notes = c.call("notify.list", json!({})).to_string();
            notes.contains("ask to join this fleet").then_some(notes)
        },
    );
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
        "(localStorage.setItem('spandeck.token', {}), true)",
        json!(laptop_token)
    ));
    page.reload();
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
    assert_eq!(relayed(b), None, "bravo was asked nothing");
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
        "(localStorage.setItem('spandeck.token', {}), true)",
        json!(a.token())
    ));
    page.reload();
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

/// What a station's stand-in `tailscale` prints from now on.
fn restatus(s: &Station, change: impl FnOnce(&mut Value)) {
    let file = s.root.join("standin/status.json");
    let mut status: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    change(&mut status);
    std::fs::write(file, status.to_string()).unwrap();
}

/// The address a signed-out Tailscale client offers, in these tests.
const SIGN_IN: &str = "https://login.tailscale.com/a/1a2b3c4d5e6f";

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
///
/// And what the owner agreed the same day comes after the warning: the steps
/// to renew a key or stop it expiring, as Tailscale's help gives them, with a
/// link to its admin console and nothing done by spandeck itself; for a
/// machine that has already expired, the way back, its last address on its
/// own network included; and on a signed-out machine's own deck, and nowhere
/// else, the address to sign in at.
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
                    device(
                        "bravo-ts",
                        "linux",
                        Some(6),
                        json!({"Online": true, "CurAddr": "192.168.1.20:41641"}),
                    ),
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
            // bravo's own addresses: one at home, and its router's.
            let at = json!({"Online": true, "Addrs": ["192.168.1.21:41641", "203.0.113.4:41641"]});
            standin_tailnet(
                root,
                device("bravo-ts", "linux", Some(6), at),
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
    // (Each is read as it is found: asked again a moment later, a station
    // once answered with none, on a loaded Mac, 16 Nov 2026. Not explained.)
    let [at_a, at_b] = [a, b].map(|s| {
        wait_for_or(
            "the warning",
            30,
            || {
                s.call("notify.list", json!({})).to_string()
                    + &s.call("tailnet.view", json!({})).to_string()
            },
            || {
                s.call("fleet.refresh", json!({}));
                let seen = notices(s, "Tailscale key");
                (seen.len() == 1).then_some(seen)
            },
        )
    });
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
    for method in [
        "perf.get",
        "tailnet.view",
        "tailnet.report",
        "tailnet.signin",
        "fleet.view",
    ] {
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

    // ---- The steps, and the sign-in address.
    // Where bravo is on its own network: where alpha last reached it
    // there, then what bravo said of itself (not its router's address).
    let lan = |a: &Station| a.call("tailnet.view", json!({}))["stations"]["bravo"]["lan"].clone();
    wait_for("bravo's addresses at home", 30, || {
        a.call("fleet.refresh", json!({}));
        (lan(a) == json!(["192.168.1.20", "192.168.1.21"])).then_some(())
    });
    // alpha is not signed out, so there is no address to give; and one
    // station cannot ask another for it at all.
    let said = a.call("tailnet.signin", json!({}));
    assert_eq!(
        (&said["needed"], &said["url"]),
        (&json!(false), &Value::Null)
    );
    let refused = fleet[1]
        .try_call(
            "peer.call",
            json!({"station": "alpha", "method": "tailnet.signin", "params": {}}),
        )
        .unwrap_err();
    assert!(
        refused.message.contains("cannot be forwarded"),
        "{}",
        refused.message
    );

    // bravo's key expires and it drops off; alpha's own Tailscale is signed
    // out and offers an address; and a device turns up whose name is markup.
    restatus(a, |status| {
        status["BackendState"] = json!("NeedsLogin");
        status["AuthURL"] = json!(SIGN_IN);
        let bravo = &mut status["Peer"]["key-n-bravo-ts"];
        *bravo = device("bravo-ts", "linux", Some(-2), json!({"Online": false}));
        status["Peer"]["key-n-x"] = device(
            "x",
            "linux",
            Some(3),
            json!({"HostName": "<img src=x onerror=window.pwned=1>", "DNSName": "", "TailscaleIPs": ["100.64.0.12"]}),
        );
    });
    let (alpha, rest) = fleet.split_at_mut(1);
    let a = &alpha[0];
    rest[0].kill9();
    wait_for("bravo to be off, its key expired", 30, || {
        let view = a.call("fleet.refresh", json!({}));
        let off = view["stations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["station"] == json!("bravo") && s["state"] == json!("unreachable"));
        let key = a.call("tailnet.view", json!({}))["stations"]["bravo"].clone();
        (off && key["expired"] == json!(true)).then_some(())
    });
    // Its address at home is no longer in what Tailscale prints: it is the
    // one remembered.
    assert_eq!(lan(a), json!(["192.168.1.20", "192.168.1.21"]));
    // The address to sign in at goes to the holder of alpha's own token,
    // and is in nothing else alpha gives out or writes down.
    let said = a.call("tailnet.signin", json!({}));
    assert_eq!(
        (&said["needed"], &said["url"]),
        (&json!(true), &json!(SIGN_IN)),
        "{said}"
    );
    for (method, params) in [
        ("tailnet.view", json!({})),
        ("tailnet.report", json!({})),
        ("perf.get", json!({})),
        ("station.info", json!({})),
        ("fleet.view", json!({})),
        ("notify.list", json!({"limit": 100})),
        ("log.query", json!({"limit": 2000})),
    ] {
        let given = a.call(method, params).to_string();
        assert!(
            !given.contains("1a2b3c4d5e6f"),
            "{method} holds it: {given}"
        );
    }
    the_deck_gives_the_steps_and_the_sign_in_address(a);
}

/// The Control Deck opened on alpha itself, as `spandeck cc` opens it.
fn the_deck_gives_the_steps_and_the_sign_in_address(a: &Station) {
    let page = common::browser::Page::open(a, 1200, 900);
    // Signed out: the bar, with the address as a link and as a code to
    // scan that is drawn in the page.
    let bar = page.wait(
        "the sign-in bar",
        "(b => b && ({text: b.innerText, links: [...b.querySelectorAll('a')].map(x => x.href), code: !!b.querySelector('svg.qr path')}))(document.querySelector('.signin-bar'))",
    );
    assert!(
        bar["text"]
            .as_str()
            .unwrap()
            .contains("Tailscale is signed out on this machine (alpha)"),
        "{bar}"
    );
    assert_eq!(
        (&bar["links"], &bar["code"]),
        (&json!([SIGN_IN]), &json!(true))
    );
    // The code is the one a phone reads as that address: these modules were
    // read back with a QR reader when the test was written.
    let code = page.js(&format!(
        "import('./qr.js').then(q => (m => [m.length, m.flat().reduce((h, dark) => Math.imul(h ^ (dark ? 1 : 0), 16777619) >>> 0, 2166136261)])(q.qrMatrix({})))",
        json!(SIGN_IN)
    ));
    assert_eq!(code, json!(QR_OF_SIGN_IN), "the code changed: read it back");

    // bravo's tile says why it is away. Waking or starting it cannot help,
    // so neither is offered.
    let tile =
        "[...document.querySelectorAll('a.tile.station')].find(x => x.innerText.includes('bravo'))";
    page.wait(
        "bravo's tile to say its key expired",
        &format!("(t => !!t && t.innerText.includes('Its Tailscale key expired') && !!t.querySelector('.key-chip.bad'))({tile})"),
    );
    page.right_click(tile);
    let menu =
        page.js("[...document.querySelectorAll('.menu-item, .menu-note')].map(x => x.innerText)");
    let menu: Vec<&str> = menu
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    assert!(
        !menu
            .iter()
            .any(|i| i.starts_with("Wake it") || i.starts_with("Start spandeck there")),
        "{menu:?}"
    );
    assert!(
        menu.iter()
            .any(|i| i.starts_with("Its Tailscale key expired")),
        "{menu:?}"
    );
    page.js("document.body.dispatchEvent(new MouseEvent('mousedown', {bubbles: true}))");
    // The way back, in Tailscale's words for what to click, with its admin
    // console as a link and bravo's last address at home.
    page.js(&format!("{tile}.querySelector('.key-chip.bad').click()"));
    let steps = page.wait(
        "the steps",
        "(d => d && ({text: d.textContent, links: [...d.querySelectorAll('a')].map(x => x.href), cmds: [...d.querySelectorAll('code')].map(x => x.innerText)}))(document.querySelector('.dialog .key-steps'))",
    );
    let text = steps["text"].as_str().unwrap();
    let order: Vec<usize> = [
        "Temporarily extend key",
        "30 minutes",
        "Disable Key Expiry",
        "Or at the machine itself",
        "Or over its own network",
    ]
    .iter()
    .map(|words| {
        text.find(words)
            .unwrap_or_else(|| panic!("no {words:?} in {text}"))
    })
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}: {text}");
    assert_eq!(
        steps["links"],
        json!([
            "https://console.tailscale.com/admin/machines",
            "https://console.tailscale.com/admin/machines?q=disabled%3Aexpired",
            "https://console.tailscale.com/admin/machines"
        ])
    );
    assert_eq!(
        steps["cmds"],
        json!([
            "sudo tailscale up --force-reauth",
            "ssh 192.168.1.20",
            "sudo tailscale up --force-reauth"
        ])
    );
    page.js("document.querySelector('.dialog .primary').click()");

    // Which steps come first is what the owner agreed: a machine nobody
    // sits at should not expire, a phone keeps its expiry and is renewed.
    let first = page.js(
        "import('./perf.js').then(p => [{station: true}, {os: 'iOS'}].map(more => p.keySteps('it', {left: 6}, more).groups[0].title))",
    );
    assert_eq!(
        first,
        json!([
            "Stop it expiring: from here, on any device",
            "Renew it: on the device"
        ])
    );

    // A device's name is text from Tailscale: shown as text, in the list
    // and in its steps.
    page.goto("#/perf");
    let name = "<img src=x onerror=window.pwned=1>";
    page.wait(
        "the device on the Performance page",
        &format!(
            "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.perf-page li')].find(li => li.innerText.includes({}))?.querySelector('button'))",
            json!(name)
        ),
    );
    let shown = page.wait(
        "its steps",
        "(d => d && [d.innerText, document.querySelectorAll('img').length, window.pwned === undefined])(document.querySelector('.dialog'))",
    );
    assert!(shown[0].as_str().unwrap().contains(name), "{shown}");
    assert_eq!((&shown[1], &shown[2]), (&json!(0), &json!(true)));
}

/// The size and a hash of the QR code's modules for `SIGN_IN`.
const QR_OF_SIGN_IN: (u32, u32) = (29, 3_208_721_812);

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

/// Not a test of anything: pictures of the steps for a Tailscale key and of
/// the sign-in bar, at a computer's width and a phone's, and the modules of
/// a few QR codes for a reader to be tried on.
/// `SPANDECK_SHOTS=<folder> cargo test -p spandeck --test phase5 pictures_of_the_tailscale -- --ignored`
#[test]
#[ignore = "takes pictures for a person to look at; needs Chrome"]
fn pictures_of_the_tailscale_steps() {
    use common::browser::Page;
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let out = std::path::PathBuf::from(std::env::var("SPANDECK_SHOTS").expect("SPANDECK_SHOTS"));
    std::fs::create_dir_all(&out).unwrap();
    const WIDE: (u32, u32) = (1300, 900);
    const PHONE: (u32, u32) = (390, 844);
    let shot = |page: &Page, name: &str| {
        std::thread::sleep(Duration::from_millis(500));
        std::fs::write(
            out.join(format!("{name}.png")),
            page.tab.capture_screenshot(Png, None, None, true).unwrap(),
        )
        .unwrap();
        let said = page.js("(document.querySelector('.dialog') || document.querySelector('.signin-bar') || document.body).innerText");
        std::fs::write(
            out.join(format!("{name}.txt")),
            said.as_str().unwrap_or_default(),
        )
        .unwrap();
    };
    let names = ["alpha", "bravo", "charlie"];
    let mut fleet = Station::fleet(&names, |name, root| {
        let on = json!({"Online": true});
        let others: Vec<Value> = [
            ("alpha", None, json!({"Online": true})),
            ("bravo", Some(6), json!({"Online": true})),
            (
                "charlie",
                Some(-2),
                json!({"Online": true, "CurAddr": "192.168.1.20:41641"}),
            ),
        ]
        .iter()
        .filter(|(n, _, _)| *n != name)
        .map(|(n, days, more)| device(n, "linux", *days, more.clone()))
        .chain([device(
            "pocket-phone",
            "iOS",
            Some(2),
            json!({"TailscaleIPs": ["100.64.0.9"]}),
        )])
        .collect();
        let mine = (name == "bravo").then_some(6);
        standin_tailnet(root, device(name, "linux", mine, on), &others)
    });
    let online = |s: &Station| {
        s.call("fleet.refresh", json!({}))["stations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["state"] == json!("online"))
            .count()
    };
    wait_for("the three to find each other", 40, || {
        (online(&fleet[0]) == 3).then_some(())
    });
    fleet[2].kill9();
    wait_for("charlie to be seen as off", 40, || {
        (online(&fleet[0]) == 2).then_some(())
    });
    let a = &fleet[0];
    let chip = |station: &str| {
        format!("(c => c ? (c.click(), true) : false)([...document.querySelectorAll('.tile.station, .phone-station')].find(x => x.innerText.includes('{station}'))?.querySelector('.key-chip'))")
    };
    let by_station = "(b => (b && b.getAttribute('aria-pressed') !== 'true' && b.click(), document.querySelectorAll('.phone-station').length === 3))([...document.querySelectorAll('.home-switch button')].find(b => b.innerText === 'By station'))";
    for (size, width) in [(WIDE, "desktop"), (PHONE, "phone")] {
        for (station, what) in [("bravo", "before-expiry"), ("charlie", "expired")] {
            let page = Page::open(a, size.0, size.1);
            page.goto("#/fleet");
            if width == "phone" {
                page.wait("the phone's stations", by_station);
            }
            page.wait("the chip", &chip(station));
            page.wait("the steps", "!!document.querySelector('.dialog ol')");
            shot(&page, &format!("{what}-{width}"));
        }
    }
    // The expired station's menu: the reason, and no Wake or Start.
    let page = Page::open(a, WIDE.0, WIDE.1);
    page.right_click(
        "[...document.querySelectorAll('a.tile.station')].find(x => x.innerText.includes('charlie') && x.innerText.includes('Tailscale key'))",
    );
    shot(&page, "expired-menu-desktop");
    // A phone's key, from the Performance page.
    let page = Page::open(a, WIDE.0, WIDE.1);
    page.goto("#/perf");
    page.wait(
        "the phone on the Performance page",
        "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.perf-page li')].find(li => li.innerText.includes('pocket-phone'))?.querySelector('button'))",
    );
    page.wait("the steps", "!!document.querySelector('.dialog ol')");
    shot(&page, "phone-device-desktop");

    // alpha's own Tailscale is signed out: first with nothing offered, then
    // with an address; then its key expired instead.
    let bar = |name: &str, change: &dyn Fn(&mut Value), ready: &str| {
        restatus(a, change);
        a.call("fleet.refresh", json!({}));
        for (size, width) in [(WIDE, "desktop"), (PHONE, "phone")] {
            let page = Page::open(a, size.0, size.1);
            page.wait("the bar", ready);
            shot(&page, &format!("{name}-{width}"));
        }
    };
    bar(
        "signed-out-no-address",
        &|s| s["BackendState"] = json!("NeedsLogin"),
        "!!document.querySelector('.signin-bar') && !document.querySelector('.signin-bar svg')",
    );
    bar(
        "signed-out-address",
        &|s| s["AuthURL"] = json!(SIGN_IN),
        "!!document.querySelector('.signin-bar svg')",
    );
    bar(
        "own-key-expired",
        &|s| {
            s["BackendState"] = json!("Running");
            s["Self"] = device("alpha", "linux", Some(-1), json!({"Online": true}));
        },
        "!!document.querySelector('.signin-bar svg') && document.querySelector('.signin-bar').innerText.includes('has expired')",
    );
    // The modules of some codes, for a reader outside the browser.
    let page = Page::open(a, WIDE.0, WIDE.1);
    let texts = json!([
        SIGN_IN,
        "a",
        "https://login.tailscale.com/a/1a2b3c4d5e6f?with=a-longer-address&and=more-of-it-to-reach-a-bigger-code",
        "x".repeat(150),
        "y".repeat(200),
        "z".repeat(331),
    ]);
    let codes = page.js(&format!(
        "import('./qr.js').then(q => {texts}.map(t => (m => ({{text: t, size: m.length, hash: m.flat().reduce((h, dark) => Math.imul(h ^ (dark ? 1 : 0), 16777619) >>> 0, 2166136261), rows: m.map(r => r.map(Number).join(''))}}))(q.qrMatrix(t))))"
    ));
    std::fs::write(out.join("qr-codes.json"), codes.to_string()).unwrap();
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
        phone.wait("shell", "!!document.querySelector('.phone-pane .xterm-rows') && document.querySelectorAll('.phone-pane .chat-switch button').length === 2");
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
        wide.terminal_face();
        click(&wide, "Chat", "document.querySelector('.strip .face-btn')");
        wide.wait("heron", "!!document.querySelector('.chat-pane .chat-acts')");
        click(&wide, "the fold", "document.querySelector('.chat-fold')");
        shot(&wide, &format!("desktop-{theme}-1-pane-as-conversation"));
        wide.goto("#/l/survey");
        // (Every agent's cell opens as its conversation: Terminal is
        // picked in the last, for a picture of one of each.)
        wide.wait("the layout", "(t => (!document.querySelector('.workspace .xterm-rows') && t.length > 1 && t[t.length - 1].click(), !!document.querySelector('.workspace .chat-pane') && !!document.querySelector('.workspace .xterm-rows')))([...document.querySelectorAll('.workspace .chat-pane .chat-switch button')].filter(b => b.innerText === 'Terminal'))");
        shot(&wide, &format!("desktop-{theme}-2-layout-one-of-each"));
        wide.goto(&pane(&asker));
        wide.terminal_face();
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

    // A question that waits and that no hook holds (this one is only in the
    // session file) is shown, and is answered in the terminal, with the keys.
    let asks = s.fake(
        json!({"steps": [{"prompt": "tally"}, line(json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "q1", "name": "AskUserQuestion",
            "input": {"questions": [{"header": "Species", "question": "Which herons count?", "multiSelect": false, "options": [{"label": "Grey herons only"}, {"label": "All herons"}]}]}}]}})), {"sleep": 600000}]}),
        json!({"name": "asks"}),
    );
    phone.goto(&pane(&asks));
    phone.wait("the question", "document.querySelector('.chat-card.ask')?.innerText.includes('Which herons count?') && document.querySelectorAll('.chat-card.ask.terminal .key').length === 9 && !document.querySelector('.chat-card.held')");
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
    phone.wait("the new pane", "/#\\/s\\/alpha\\/p\\//.test(location.hash) && !!document.querySelector('.phone-pane .xterm-rows') && document.querySelector('.phone-pane .chat-switch button.on')?.innerText === 'Terminal'");
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
    // (An agent's cell opens as its conversation since 16 Nov 2026:
    // Terminal is picked in each, and it goes on from two terminals.)
    wide.wait(
        "two terminals",
        "([...document.querySelectorAll('.workspace .chat-pane .chat-switch button')].filter(b => b.innerText === 'Terminal').forEach(b => b.click()), document.querySelectorAll('.workspace .pane .xterm-rows').length === 2)",
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
    wide.type_str("from a computer");
    wide.press_key("Enter");
    s.wait_text(&long, "tally.csv from a computer");
    click(&wide, "Terminal", &button("Terminal"));
    wide.wait("the terminal again", "!!document.querySelector('.station-main.zoomed .pane .xterm-rows') && !document.querySelector('.chat-pane')");
}

/// Not a test of behaviour: pictures of a wide pane on a phone, as its
/// terminal and as its conversation, while a computer has it open and after
/// the computer lets go of it, and of the sheet, for a person to look at.
/// `SPANDECK_SHOTS=<folder> cargo test -p spandeck --test phase5 pictures_of_a_pane_at_the_phones_text_size -- --ignored`
#[test]
#[ignore = "takes pictures for a person to look at; needs Chrome"]
fn pictures_of_a_pane_at_the_phones_text_size() {
    use common::browser::Page;
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let out = std::path::PathBuf::from(std::env::var("SPANDECK_SHOTS").expect("SPANDECK_SHOTS"));
    std::fs::create_dir_all(&out).unwrap();
    let s = Station::start("spark2");
    s.call(
        "look.set",
        json!({"level": "deck", "choice": {"theme": "solarized-dark"}}),
    );
    let said = |blocks: Value| json!({"transcript": {"type": "assistant", "message": {"role": "assistant", "content": blocks}}});
    let heron = s.fake(
        json!({"steps": [
            {"prompt": "Count the herons in survey.csv and fix the tally script."},
            said(json!([{"type": "text", "text": "I'll count the rows first, then fix `tally.py`."}])),
            {"turn_end": "**Done: the tally now counts herons only.**\n\n```python\nherons = [r for r in rows if r.species.strip() == 'heron']\nreturn len(herons)\n```"}
        ]}),
        json!({"name": "heron"}),
    );
    let wide = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "wide", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let shot = |page: &Page, name: &str| {
        std::thread::sleep(Duration::from_millis(900));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("{name}.png")), png).unwrap();
    };
    let click = |page: &Page, what: &str, el: &str| {
        page.wait(what, &format!("(e => e ? (e.click(), true) : false)({el})"));
    };
    let face = |text: &str| {
        format!("[...document.querySelectorAll('.chat-switch button')].find(b => b.innerText.trim() === '{text}')")
    };
    let desk = Page::open(&s, 1300, 800);
    desk.goto(&format!("#/s/spark2/p/{wide}"));
    desk.wait(
        "the pane on the computer",
        "!!document.querySelector('.pane .xterm-rows')",
    );
    wait_for("the computer to size the pane", 10, || {
        (s.pane(&wide)["cols"].as_u64().unwrap() > 100).then_some(())
    });
    let print = "i=1; while [ $i -le 18 ]; do echo \"$i  the quick brown fox jumps over the lazy dog and keeps going, right across a terminal one hundred and twenty columns wide\"; i=$((i+1)); done";
    s.call(
        "pane.paste",
        json!({"pane": wide, "text": print, "enter": true}),
    );
    s.wait_text(&wide, "18  the quick");
    let phone = Page::open(&s, 390, 844);
    phone.goto(&format!("#/s/spark2/p/{wide}"));
    phone.wait(
        "the terminal",
        "!!document.querySelector('.phone-pane .xterm-rows')",
    );
    shot(&phone, "1-terminal-open-on-a-computer");
    click(&phone, "Chat", &face("Chat"));
    phone.wait(
        "what it printed",
        "!!document.querySelector('.phone-chat .chat-screen .chat-mono pre')",
    );
    shot(&phone, "2-conversation-open-on-a-computer");
    desk.goto("#/fleet");
    std::thread::sleep(Duration::from_millis(4000));
    s.call(
        "pane.paste",
        json!({"pane": wide, "text": format!("clear; {print}"), "enter": true}),
    );
    std::thread::sleep(Duration::from_millis(1500));
    shot(&phone, "3-conversation-nobody-else-has-it");
    click(&phone, "Terminal", &face("Terminal"));
    phone.wait(
        "the terminal",
        "!!document.querySelector('.phone-pane .xterm-rows')",
    );
    std::thread::sleep(Duration::from_millis(1500));
    s.call(
        "pane.paste",
        json!({"pane": wide, "text": format!("clear; {print}"), "enter": true}),
    );
    shot(&phone, "4-terminal-nobody-else-has-it");
    phone.goto(&format!("#/s/spark2/p/{heron}"));
    phone.wait(
        "the conversation",
        "!!document.querySelector('.phone-chat .chat-text')",
    );
    shot(&phone, "5-conversation-of-an-agent");
    click(
        &phone,
        "the pane's menu",
        "document.querySelector('.chat-more')",
    );
    click(&phone, "Appearance…", "[...document.querySelectorAll('.menu-item')].find(b => b.innerText.startsWith('Appearance'))");
    phone.wait("the sheet", "!!document.querySelector('.phone-look')");
    phone.js("(document.querySelector('.pl-size').scrollIntoView(), true)");
    shot(&phone, "6-sheet-text-size");
    phone.js("(document.querySelector('.backdrop').scrollTop = 99999, true)");
    shot(&phone, "7-sheet-fonts");
}

/// Not a test of behaviour: pictures of the phone's Appearance sheet, and of
/// a conversation and a terminal at the smallest, the deck's and the largest
/// text size, in a dark and a light theme, for a person to look at.
/// `SPANDECK_SHOTS=<folder> cargo test -p spandeck --test phase5 pictures_of_the_phones_appearance -- --ignored`
#[test]
#[ignore = "takes pictures for a person to look at; needs Chrome"]
fn pictures_of_the_phones_appearance() {
    use common::browser::Page;
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let out = std::path::PathBuf::from(std::env::var("SPANDECK_SHOTS").expect("SPANDECK_SHOTS"));
    std::fs::create_dir_all(&out).unwrap();
    let s = Station::start("spark2");
    let line = |v: Value| json!({"transcript": v});
    let said = |blocks: Value| json!({"type": "assistant", "message": {"role": "assistant", "content": blocks}});
    let answer = "**Done: the tally now counts herons only.**\n\n- `tally.py` filters the rows to the heron family before it counts\n- the test for mixed rows passes again (3 of 3)\n\n```python\nherons = [r for r in rows if r.species.strip() == 'heron']\nreturn len(herons)\n```\n\n| Species | Rows |\n|:--|--:|\n| heron | 37 |\n| egret | 112 |";
    let heron = s.fake(
        json!({"steps": [
            line(json!({"type": "ai-title", "aiTitle": "Heron counts for the marsh survey"})),
            {"prompt": "Count the herons in survey.csv and fix the tally script."},
            line(said(json!([{"type": "text", "text": "I'll count the rows first, then fix `tally.py`."}]))),
            line(said(json!([{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "wc -l survey.csv"}}]))),
            line(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "412 survey.csv"}]}, "toolUseResult": {"stdout": "412 survey.csv", "stderr": "", "interrupted": false}})),
            {"say": "$ wc -l survey.csv\n412 survey.csv\n$ python -m pytest -q tests/test_tally.py\n...                                              [100%]\n3 passed in 0.41s"},
            {"permission": {"name": "Bash", "command": "git commit -am 'Count herons only'"}},
            {"turn_end": answer}
        ]}),
        json!({"name": "heron"}),
    );
    s.wait_state(&heron, "needs_you");
    let sh = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "scratch", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let sh = sh.as_str();
    let shot = |page: &Page, name: &str| {
        std::thread::sleep(Duration::from_millis(700));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("{name}.png")), png).unwrap();
    };
    let click = |page: &Page, what: &str, el: &str| {
        page.wait(what, &format!("(e => e ? (e.click(), true) : false)({el})"));
    };
    let button = |text: &str| {
        format!("[...document.querySelectorAll('button')].find(b => b.innerText.trim().startsWith('{text}'))")
    };
    // Every step of a size, one way or the other, or back to the deck's.
    let size = |page: &Page, to: &str| {
        click(
            page,
            "the pane's menu",
            "document.querySelector('.chat-more')",
        );
        click(page, "Appearance…", "[...document.querySelectorAll('.menu-item')].find(b => b.innerText.startsWith('Appearance'))");
        page.wait("the sheet", "!!document.querySelector('.phone-look')");
        page.js(&match to {
            "deck" => "(document.querySelector('.pl-reset').click(), true)".to_string(),
            _ => format!("([...document.querySelectorAll('.pl-size .pl-step:{to}-of-type')].forEach(b => {{ while (!b.disabled) b.click(); }}), true)"),
        });
    };
    for theme in ["dark", "light"] {
        s.call(
            "look.set",
            json!({"level": "deck", "choice": {"theme": format!("spandeck-{theme}")}}),
        );
        let phone = Page::open(&s, 390, 844);
        phone.wait("the list", "!!document.querySelector('.session-row')");
        click(&phone, "More", "document.querySelector('.top-more')");
        shot(&phone, &format!("{theme}-0-more-menu"));
        click(&phone, "Appearance", "[...document.querySelectorAll('.menu-item')].find(b => b.innerText.startsWith('Appearance'))");
        phone.wait("the sheet", "!!document.querySelector('.phone-look')");
        shot(&phone, &format!("{theme}-1-sheet-top"));
        phone.js("(document.querySelector('.backdrop').scrollTop = 99999, true)");
        shot(&phone, &format!("{theme}-2-sheet-bottom"));
        click(&phone, "Done", &button("Done"));
        phone.goto(&format!("#/s/spark2/p/{heron}"));
        phone.wait(
            "the conversation",
            "!!document.querySelector('.phone-chat .chat-text')",
        );
        for (n, name, to) in [
            (3, "deck", "deck"),
            (5, "smallest", "first"),
            (7, "largest", "last"),
        ] {
            if to != "deck" {
                size(&phone, to);
                shot(&phone, &format!("{theme}-{n}-sheet-{name}"));
                click(&phone, "Done", &button("Done"));
            }
            shot(&phone, &format!("{theme}-{n}-conversation-{name}"));
            if to == "deck" {
                click(&phone, "Terminal", &button("Terminal"));
                phone.wait(
                    "the terminal",
                    "!!document.querySelector('.phone-pane .xterm-rows')",
                );
                shot(&phone, &format!("{theme}-{n}-terminal-of-the-agent"));
                click(&phone, "Chat", &button("Chat"));
            }
            // A terminal's size shows in a pane fitted to the phone.
            phone.goto(&format!("#/s/spark2/p/{sh}"));
            // (Fitted by itself: nobody else has the pane open.)
            phone.wait("fitted", &button("Fitted"));
            phone.wait(
                "the terminal",
                "!!document.querySelector('.phone-pane .xterm-rows')",
            );
            std::thread::sleep(Duration::from_millis(500));
            s.call(
                "pane.paste",
                json!({"pane": sh, "text": "clear; ls -l /; echo '=> != 0O 1lI'", "enter": true}),
            );
            shot(&phone, &format!("{theme}-{}-terminal-{name}", n + 1));
            phone.goto(&format!("#/s/spark2/p/{heron}"));
            phone.wait(
                "the conversation",
                "!!document.querySelector('.phone-chat .chat-text')",
            );
        }
    }
}

/// Not a test of behaviour: pictures of a question and a plan answered in
/// place (mobile plan, phase M3), on a phone and on a computer, in a dark
/// and a light theme.
/// `SPANDECK_SHOTS=<folder> cargo test -p spandeck --test phase5 pictures_of_answering -- --ignored`
#[test]
#[ignore = "takes pictures for a person to look at; needs Chrome"]
fn pictures_of_answering_in_place() {
    use common::browser::Page;
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let out = std::path::PathBuf::from(std::env::var("SPANDECK_SHOTS").expect("SPANDECK_SHOTS"));
    std::fs::create_dir_all(&out).unwrap();
    let s = Station::start("spark2");
    let line = |v: Value| json!({"transcript": v});
    let said = |text: &str| {
        line(
            json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}}),
        )
    };
    let title = |t: &str| line(json!({"type": "ai-title", "aiTitle": t}));
    let species = json!({"header": "Species", "question": "Which herons count?", "multiSelect": false,
        "options": [{"label": "Grey herons only", "description": "Skip egrets and bitterns"},
                    {"label": "All herons", "description": "Count every bird in the heron family"}]});
    let rivers = json!({"header": "Rivers", "question": "Which rivers should the tally cover?", "multiSelect": true,
        "options": [{"label": "Avon", "description": "42 survey rows"}, {"label": "Exe", "description": "17 survey rows"},
                    {"label": "Tamar", "description": "9 survey rows, two with no date"}]});
    let plan = "# Tally plan\n\n1. Filter the rows to **grey herons**\n2. Fix `tally.py` so that it strips the species name\n3. Run the tests again\n\n| File | Change |\n|:--|:--|\n| `tally.py` | filter, then count |\n| `tests/test_tally.py` | a row with a trailing space |\n\nRaw HTML stays text: <b>not bold</b>.";
    let spawn = |name: &str, titled: &str, prompt: &str, words: &str, ask: Value| {
        s.fake(
            json!({"steps": [title(titled), {"prompt": prompt}, said(words), ask, {"turn_end": "Done."}]}),
            json!({"name": name}),
        )
    };
    let shot = |page: &Page, name: &str| {
        std::thread::sleep(Duration::from_millis(700));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("{name}.png")), png).unwrap();
    };
    let pane = |id: &str| format!("#/s/spark2/p/{id}");
    let click = |page: &Page, what: &str, el: &str| {
        page.wait(what, &format!("(e => e ? (e.click(), true) : false)({el})"));
    };
    let opt = |text: &str| {
        format!("[...document.querySelectorAll('.chat-card.held .ask-opt')].find(b => b.innerText.includes('{text}'))")
    };
    let held = |id: &str| {
        wait_for("the held question", 15, || {
            s.call("pane.conversation", json!({"pane": id}))["live"]["questions"]
                .get(0)
                .cloned()
        })
    };
    for theme in ["dark", "light"] {
        s.call(
            "look.set",
            json!({"level": "deck", "choice": {"theme": format!("spandeck-{theme}")}}),
        );
        // Each theme has agents of its own: a card that was answered is gone.
        let one = spawn(
            &format!("herons-{theme}"),
            "Which herons to count",
            "Tally the survey.",
            "Before I count, one thing to settle.",
            json!({"ask": {"questions": [species.clone()]}}),
        );
        let two = spawn(
            &format!("rivers-{theme}"),
            "Herons by river",
            "Tally the survey by river.",
            "Two things to settle first.",
            json!({"ask": {"questions": [species.clone(), rivers.clone()]}}),
        );
        let planner = spawn(
            &format!("planner-{theme}"),
            "Plan for the tally fix",
            "Plan the fix for the tally script. Do not change anything yet.",
            "I've read the script and the survey. Here is the plan.",
            json!({"plan": {"plan": plan}}),
        );
        let moved = spawn(
            &format!("moved-{theme}"),
            "Egrets, yes or no",
            "Count the egrets too?",
            "I need to know one thing.",
            json!({"ask": {"questions": [species.clone()]}}),
        );
        for id in [&one, &two, &planner] {
            held(id);
        }
        s.call(
            "question.answer",
            json!({"id": held(&moved)["id"], "release": true}),
        );
        s.wait_text(&moved, "is in the terminal");

        let phone = Page::open(&s, 390, 844);
        phone.wait(
            "the list",
            "document.querySelectorAll('.session-row').length >= 4",
        );
        shot(&phone, &format!("phone-{theme}-1-list-needs-you"));
        phone.goto(&pane(&one));
        phone.wait("the question", "!!document.querySelector('.chat-card.held .ask-opt') && !!document.querySelector('.ask-wait')?.innerText");
        shot(&phone, &format!("phone-{theme}-2-question"));
        click(&phone, "Other", &opt("Other"));
        phone.wait("the field", "!!document.querySelector('.ask-other')");
        phone.js("(o => { o.value = 'Grey herons, and the two bitterns by the weir'; o.dispatchEvent(new Event('input')); return true; })(document.querySelector('.ask-other'))");
        shot(&phone, &format!("phone-{theme}-3-question-other"));
        click(&phone, "Other again", &opt("Other"));
        click(&phone, "an option", &opt("Grey herons only"));
        phone.wait(
            "the answer on the item",
            "!document.querySelector('.chat-card.held') && !!document.querySelector('.chat-answer')",
        );
        shot(&phone, &format!("phone-{theme}-4-question-answered"));
        phone.goto(&pane(&two));
        phone.wait(
            "step one",
            "!!document.querySelector('.chat-card.held .ask-opt')",
        );
        shot(&phone, &format!("phone-{theme}-5-two-questions-step-1"));
        click(&phone, "an option", &opt("All herons"));
        phone.wait(
            "step two",
            "document.querySelector('.chat-card.held .ask-head')?.innerText.includes('2 of 2')",
        );
        click(&phone, "Avon", &opt("Avon"));
        click(&phone, "Tamar", &opt("Tamar"));
        phone.wait(
            "two ticked",
            "document.querySelectorAll('.chat-card.held .ask-opt.on').length === 2",
        );
        shot(
            &phone,
            &format!("phone-{theme}-6-two-questions-step-2-multi"),
        );
        phone.goto(&pane(&planner));
        phone.wait(
            "the plan",
            "!!document.querySelector('.chat-card.held.plan .ask-plan table')",
        );
        shot(&phone, &format!("phone-{theme}-7-plan"));
        phone.goto(&pane(&moved));
        phone.wait(
            "in the terminal",
            "!!document.querySelector('.chat-card.ask.terminal')",
        );
        shot(&phone, &format!("phone-{theme}-8-moved-to-the-terminal"));
        drop(phone);

        let wide = Page::open(&s, 1400, 900);
        wide.goto(&pane(&two));
        wide.terminal_face();
        wide.wait(
            "the terminal",
            "!!document.querySelector('.strip .face-btn')",
        );
        shot(
            &wide,
            &format!("desktop-{theme}-1-terminal-says-answer-in-chat"),
        );
        click(&wide, "Chat", "document.querySelector('.strip .face-btn')");
        wide.wait(
            "the question",
            "!!document.querySelector('.chat-pane .chat-card.held .ask-opt')",
        );
        shot(&wide, &format!("desktop-{theme}-2-question"));
        wide.goto(&pane(&planner));
        wide.terminal_face();
        wide.wait(
            "the terminal",
            "!!document.querySelector('.strip .face-btn')",
        );
        click(&wide, "Chat", "document.querySelector('.strip .face-btn')");
        wide.wait(
            "the plan",
            "!!document.querySelector('.chat-pane .chat-card.held.plan .ask-plan table')",
        );
        wide.js("(n => { n.value = 'Count the egrets as well'; n.dispatchEvent(new Event('input')); return true; })(document.querySelector('.ask-note'))");
        shot(&wide, &format!("desktop-{theme}-3-plan"));
        click(
            &wide,
            "Keep planning",
            "document.querySelector('.chat-card.held .ask-keep')",
        );
        wide.wait(
            "the plan sent back",
            "!document.querySelector('.chat-card.held') && [...document.querySelectorAll('.chat-box-item .chat-from')].some(f => f.innerText.includes('sent back'))",
        );
        shot(&wide, &format!("desktop-{theme}-4-plan-sent-back"));
        drop(wide);
        for id in [&one, &two, &planner, &moved] {
            s.call("pane.close", json!({"pane": id}));
        }
    }
}

/// Not a test of behaviour: pictures of a layout on a phone, one pane at a
/// time, where the pane shown is the pane as it is when opened by itself (the
/// owner's report, 16 Nov 2026), in a dark and a light theme. It also prints
/// how much of the screen the pane's conversation gets.
/// `SPANDECK_SHOTS=<folder> cargo test -p spandeck --test phase5 pictures_of_the_phones_layout -- --ignored --nocapture`
#[test]
#[ignore = "takes pictures for a person to look at; needs Chrome"]
fn pictures_of_the_phones_layout_view() {
    use common::browser::Page;
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let out = std::path::PathBuf::from(std::env::var("SPANDECK_SHOTS").expect("SPANDECK_SHOTS"));
    std::fs::create_dir_all(&out).unwrap();
    let s = Station::start("spark2");
    let line = |v: Value| json!({"transcript": v});
    let said = |text: &str| {
        line(
            json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}}),
        )
    };
    let shell = |n: &str| json!({"name": n, "agent": "shell"});
    s.call(
        "project.new",
        json!({"path": "~/testProject1", "panes": [shell("dev2"), shell("logs")], "open": true}),
    );
    let shot = |page: &Page, name: &str| {
        std::thread::sleep(Duration::from_millis(700));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("{name}.png")), png).unwrap();
    };
    let click = |page: &Page, what: &str, el: &str| {
        page.wait(what, &format!("(e => e ? (e.click(), true) : false)({el})"));
    };
    let face = |to: &str| {
        format!("[...document.querySelectorAll('.phone-layout .chat-switch button')].find(b => b.innerText === '{to}')")
    };
    let reply = "The inbox notification is fixed. What changed:\n\n- **The count** is the fleet's, not one station's\n- A notice raised at `spark2` reaches a deck served by any other station at once\n- \"Mark all read\" goes to every station that answers\n\nThe tests pass. One file is left over from the old count, and I would like to remove it.";
    for theme in ["dark", "light"] {
        s.call(
            "look.set",
            json!({"level": "deck", "choice": {"theme": format!("spandeck-{theme}")}}),
        );
        let name = format!("claude-{theme}");
        let agent = s.fake(
            json!({"steps": [
                line(json!({"type": "ai-title", "aiTitle": "Spandeck inbox notification"})),
                {"prompt": "Make the inbox count the fleet's."},
                said(reply),
                line(json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "rm crates/core/src/old_count.rs"}}]}})),
                {"permission": {"name": "Bash", "command": "rm crates/core/src/old_count.rs"}},
                {"turn_end": "Removed. The inbox is the fleet's now."}
            ]}),
            json!({"name": name, "project": "testProject1", "cwd": s.root.join("home/testProject1")}),
        );
        s.wait_state(&agent, "needs_you");
        s.call(
            "project.layout.save",
            json!({"project": "testProject1", "layout": {"name": "build", "tree": {"dir": "col", "ratio": 600,
                "a": {"dir": "row", "ratio": 500, "a": {"pane": name}, "b": {"pane": "dev2"}}, "b": {"pane": "logs"}},
                "hidden": [{"pane": "logs"}]}}),
        );
        let phone = Page::open(&s, 390, 844);
        click(
            &phone,
            "the list by station",
            "[...document.querySelectorAll('.home-switch button')].find(b => b.innerText === 'By station')",
        );
        click(
            &phone,
            "the layout's row",
            "document.querySelector('main.phone .phone-station .lay-row')",
        );
        phone.wait(
            "the layout",
            "!!document.querySelector('.phone-layout .chat-log .chat-text, .phone-layout .xterm-rows')",
        );
        // How much of the screen the pane itself gets, under the layout's own
        // controls: the conversation's log, or the terminal where there is none.
        let room = phone.js(
            "(e => { const r = e.getBoundingClientRect(); return { what: e.className, top: Math.round(r.top), height: Math.round(r.height), screen: innerHeight }; })(document.querySelector('.phone-layout .chat-log') || document.querySelector('.phone-layout .pane'))",
        );
        println!("{theme}: the pane's room at 390x844, with a card waiting: {room}");
        phone.wait(
            "the card",
            "!!document.querySelector('.phone-layout .chat-cards .approval')",
        );
        shot(&phone, &format!("{theme}-1-agent-chat-card"));
        click(
            &phone,
            "Allow",
            "[...document.querySelectorAll('.chat-cards .approval button')].find(b => b.innerText === 'Allow')",
        );
        phone.wait(
            "the reply",
            "[...document.querySelectorAll('.chat-text')].some(t => t.innerText.includes('Removed.')) && !document.querySelector('.chat-cards .approval')",
        );
        let room = phone.js(
            "['.chat-bar', '.lay-strip', '.chat-log', '.chat-dock'].map(q => { const e = document.querySelector('.phone-layout ' + q), r = e.getBoundingClientRect(); return [q, Math.round(r.top), Math.round(r.height)]; })",
        );
        println!("{theme}: top and height of each part, nothing waiting: {room}");
        shot(&phone, &format!("{theme}-2-agent-chat-map-collapsed"));
        click(&phone, "the line", "document.querySelector('.lay-where')");
        phone.wait("the map", "!document.querySelector('.lay-panel').hidden");
        shot(&phone, &format!("{theme}-3-map-expanded"));
        click(
            &phone,
            "the line again",
            "document.querySelector('.lay-where')",
        );
        click(&phone, "Terminal", &face("Terminal"));
        phone.wait(
            "the terminal",
            "!!document.querySelector('.phone-layout .xterm-rows') && !!document.querySelector('.phone-layout .composer-box')",
        );
        shot(&phone, &format!("{theme}-4-agent-terminal"));
        click(&phone, "Chat", &face("Chat"));
        phone.wait(
            "the conversation",
            "!!document.querySelector('.phone-layout .chat-log .chat-text')",
        );
        click(
            &phone,
            "the next pane",
            "document.querySelector('.lay-nav .lay-step:last-child')",
        );
        phone.wait(
            "the shell",
            "!!document.querySelector('.phone-layout .xterm-rows') && document.querySelectorAll('.phone-layout .chat-switch button').length === 2",
        );
        shot(&phone, &format!("{theme}-5-shell"));
        drop(phone);
        s.call("pane.close", json!({"pane": agent}));
    }
}

/// The owner's report of 17 Nov 2026, from an iPhone, adding the Mac
/// "monitoring": "I don't see a place to enter a password ... What does turn
/// off spark2 mean? What if spark2 is down?", "I can't even see what I am
/// typing for username" (the box was behind the keyboard), a failure that
/// said only that it "could not ask to join this fleet" over a log ending
/// "no station called spark2 was found", and "aren't they all peers. It
/// sound like spark2 is some master machine".
///
/// At a phone's size, with a keyboard the page is told of as an iPhone
/// tells it (what is in sight gets shorter and begins further down): Add a
/// machine is reached from the Recent list; the dialog says what happens
/// with a password, who connects and why that station, and what to do if it
/// is down; the switch for the feature is not among the buttons that act;
/// a machine that is picked is one row, and it and the box being typed in
/// are both in sight above the keyboard, in the user name step and in the
/// password step, with the page itself not moved; the failed join says what
/// worked, what did not and what to do, naming the fleet and not a station
/// as its head; and "Try again" only asks again. The New session sheet
/// keeps its box in sight the same way.
///
/// With `SPANDECK_SHOTS=<folder>` it also writes a picture of each state.
#[test]
#[ignore = "merge tier: a browser"]
fn add_a_machine_on_a_phone_is_typed_in_sight_and_says_what_happened() {
    use common::browser::Page;
    const RIGHT: &str = "c0rrect-horse-staple-91";
    let dir = tempfile::tempdir().unwrap();
    let at = |p: &str| dir.path().join(p);
    std::fs::create_dir_all(at("standin")).unwrap();
    // A tailnet with a dozen machines to add: the one wanted is last.
    let names = [
        "atlas",
        "bench",
        "cedar",
        "delta",
        "ember",
        "forge",
        "garnet",
        "harbor",
        "iris",
        "juniper",
        "kestrel",
        "monitoring",
    ];
    let peers: serde_json::Map<String, Value> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            (
                format!("key-{name}"),
                json!({"ID": format!("n-{name}"), "HostName": name, "DNSName": format!("{name}.tail0000.ts.net."),
                    "OS": if i % 3 == 0 { "linux" } else { "macOS" }, "UserID": 7, "Online": true,
                    "TailscaleIPs": [format!("100.64.0.{}", i + 20)]}),
            )
        })
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
    // "ssh": it asks for the password as ssh does, and then says what a
    // Mac that cannot reach the release page says (the owner's decision of
    // 17 Nov 2026: it is sent the release over SSH): it asks for the
    // release, waits where the test can look at the dialog, takes it, and
    // then prints what the owner's Mac printed: installed, running, and
    // nobody found to ask. Told there is no release to send, it ends.
    executable(
        &at("standin/ssh"),
        &format!(
            r#"#!/bin/sh
user=$1 host=$2
pw=$("$SSH_ASKPASS" "$user@$host's password: ") || pw=
if [ "$pw" != "{RIGHT}" ]; then
  echo "$user@$host: Permission denied (publickey,password)." >&2
  exit 255
fi
while IFS= read -r line; do case "$line" in "main 3<&0"*) break ;; esac; done
echo "@spandeck step install"
echo "This machine could not reach the release page"
echo "@spandeck supply aarch64-apple-darwin network"
IFS= read -r answer
set -- $answer
if [ "$1" != release ]; then echo "@spandeck fail supply"; exit 1; fi
while [ ! -e {go} ]; do sleep 0.1; done
echo "@spandeck ready"
head -c "$3" >/dev/null
echo "Received $3 bytes, and their SHA-256 matches."
echo "Installed spandeck v0.4.10 to /Users/michael/.local/bin (checksum verified)"
echo "@spandeck step start"
echo "spandeck is running."
echo "@spandeck version spandeck 0.4.10"
echo "@spandeck step join"
echo "spandeck: no station called alpha was found"
echo "@spandeck why spandeck: no station called alpha was found"
echo "@spandeck fail join"
exit 1
"#,
            go = at("go").display()
        ),
    );
    // The release page, for the station: it holds a Mac release (a few
    // bytes; the stand-in above does not unpack it).
    let mac = format!(
        "spandeck-{}-aarch64-apple-darwin.tar.gz",
        common::fake_releases::TAG
    );
    let releases =
        common::fake_releases::Releases::start_with(true, false, &[(mac, vec![7u8; 2400])]);
    let path = format!(
        "{}:{}",
        at("standin").display(),
        std::env::var("PATH").unwrap()
    );
    let a = Station::start_with(
        "alpha",
        &format!(
            "add_machine_cmd = [\"{}\", \"{{user}}\", \"{{host}}\"]\n[update]\nurl = \"{}\"\n",
            at("standin/ssh").display(),
            releases.url()
        ),
        &[("PATH", &path)],
    );
    a.call("fleet.add_enable", json!({"on": true}));

    let page = Page::open(&a, 390, 844);
    page.phone_screen();
    let shots = std::env::var_os("SPANDECK_SHOTS").map(std::path::PathBuf::from);
    let shot = |name: &str| {
        use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
        if let Some(out) = &shots {
            std::fs::create_dir_all(out).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
            std::fs::write(out.join(format!("{name}.png")), png).unwrap();
        }
    };
    // The phone's keyboard, as the page is told of one (chat.rs does the
    // same): what is in sight is `tall` high and begins `top` down the page.
    let keyboard = |tall: u32, top: u32| {
        page.js(&format!(
            "(() => {{ const real = window.__vv || (window.__vv = window.visualViewport); \
               const now = {{ height: {tall}, width: window.innerWidth, offsetTop: {top}, offsetLeft: 0, pageTop: {top}, pageLeft: 0, scale: 1 }}; \
               Object.defineProperty(window, 'visualViewport', {{ configurable: true, get: () => now }}); \
               real.dispatchEvent(new Event('resize')); return true; }})()"
        ));
    };
    // Where the box being typed in is, and whether anything of the page moved.
    const PLACE: &str = r#"(() => {
        const vv = window.visualViewport, top = vv.offsetTop, bottom = vv.offsetTop + vv.height;
        const box = (e) => { const r = e.getBoundingClientRect(); return [Math.round(r.top), Math.round(r.bottom)]; };
        const within = (e) => { const r = e.getBoundingClientRect(); return r.height > 0 && r.top >= top - 0.5 && r.bottom <= bottom + 0.5; };
        const el = document.activeElement, dialog = document.querySelector('.dialog'), picked = document.querySelector('.add-picked');
        window.scrollTo(0, 300);
        return {
            typing: [el.tagName, el.getAttribute('aria-label') || el.className, within(el)],
            field: box(el), sight: [Math.round(top), Math.round(bottom)], dialog: [...box(dialog), within(dialog)],
            picked: picked ? within(picked) : null,
            page: [document.scrollingElement.scrollTop, window.scrollY, document.documentElement.scrollWidth <= document.documentElement.clientWidth],
            body: (b => [b.scrollHeight > b.clientHeight, getComputedStyle(b).overflowY])(dialog.querySelector('.dialog-body')),
        };
    })()"#;
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
    let press = |label: &str| {
        page.wait(
            &format!("the {label} button"),
            &format!(
                "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.dialog button')].find(b => b.innerText.trim() === {}))",
                json!(label)
            ),
        );
    };

    // From the Recent list, which is what a phone opens on: the top bar's menu.
    page.wait(
        "the phone's list of sessions, most recent first",
        "!!document.querySelector('.phone-recents') && !document.querySelector('.add-row')",
    );
    page.wait(
        "the top bar's menu",
        "(b => b ? (b.click(), true) : false)(document.querySelector('.top-more'))",
    );
    page.choose("Add a machine");
    let said = dialog_says("the list of machines", "monitoring");
    for words in [
        // Who connects, why that station, and what if it is down.
        "Any station of your fleet can add a machine. alpha does the connecting now because this page is open on it: if alpha is down, open the deck on another station and add the machine from there.",
        // The switch: this feature, not the station.
        "Adding machines is switched on at alpha (the station this page is open on)",
        "Stop alpha from adding machines",
        "That switches off only this feature: alpha and its panes keep running, and this dialog offers to turn it on again.",
    ] {
        assert!(said.contains(words), "{words:?} is not in: {said}");
    }
    assert!(!said.contains("Turn adding machines off"), "{said}");
    // It is a quiet last line, not one of the buttons that act.
    assert_eq!(
        page.js("[[...document.querySelectorAll('.dialog .row.end button')].map(b => b.innerText.trim()), document.querySelector('.dialog .add-off button').innerText, !!document.querySelector('.dialog .add-off').previousElementSibling, !document.querySelector('.dialog .add-off').nextElementSibling]"),
        json!([["Cancel"], "Stop alpha from adding machines", true, true])
    );
    // Twelve machines are more than the screen: the dialog is as tall as
    // what is in sight allows and its body is what scrolls.
    let open = page.js(PLACE);
    assert_eq!(
        (&open["dialog"][2], &open["body"], &open["page"]),
        (&json!(true), &json!([true, "auto"]), &json!([0, 0, true])),
        "{open}"
    );
    shot("1-as-it-opens");

    // The machine is picked: the list is that one row, and the box for the
    // user name has the keyboard. With the keyboard up, and the page slid
    // by the browser as iOS slides it, the row and the box are in sight.
    page.wait(
        "monitoring's row",
        "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.dialog .add-machine')].find(b => b.innerText.includes('monitoring')))",
    );
    let said = dialog_says("the user name", "Your user name on monitoring");
    assert!(
        said.contains("alpha (the station this page is open on) connects to monitoring over SSH as this user and runs the spandeck install script there. If monitoring asks for a password you will be asked for it next; it is used for that one connection and not kept."),
        "{said}"
    );
    assert_eq!(
        page.js("[document.querySelectorAll('.dialog .add-machine').length, document.querySelector('.add-picked .add-change').innerText, document.querySelector('.add-picked b').innerText]"),
        json!([1, "Change", "monitoring"])
    );
    for (tall, top) in [(420, 0), (400, 130)] {
        keyboard(tall, top);
        let up = page.js(PLACE);
        assert_eq!(
            (
                &up["typing"],
                &up["picked"],
                &up["dialog"][2],
                &up["page"],
                &up["sight"]
            ),
            (
                &json!(["INPUT", "User name on that machine", true]),
                &json!(true),
                &json!(true),
                &json!([0, 0, true]),
                &json!([top, top + tall])
            ),
            "{up}"
        );
    }
    shot("2-keyboard-up-machine-picked");
    // "Change" brings the list back, and the box is still the one typed in.
    press("Change");
    page.wait(
        "the whole list again",
        "document.querySelectorAll('.dialog .add-machine').length === 12",
    );
    let listed = page.js(PLACE);
    assert_eq!(
        (&listed["typing"], &listed["page"]),
        (
            &json!(["INPUT", "User name on that machine", true]),
            &json!([0, 0, true])
        ),
        "{listed}"
    );
    page.wait(
        "monitoring's row",
        "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.dialog .add-machine')].find(b => b.innerText.includes('monitoring')))",
    );
    page.wait(
        "the one row",
        "document.querySelectorAll('.dialog .add-machine').length === 1",
    );

    // The password step, keyboard still up: the password's box is in sight.
    page.js("(document.querySelector(\".dialog input[aria-label='User name on that machine']\").value = 'michael', true)");
    press("Connect and install");
    dialog_says("the password to be asked for", "Password on monitoring");
    let pass = page.js(PLACE);
    assert_eq!(
        (&pass["typing"], &pass["dialog"][2], &pass["page"]),
        (
            &json!(["INPUT", "Password on that machine", true]),
            &json!(true),
            &json!([0, 0, true])
        ),
        "{pass}"
    );
    shot("3-password-step");

    // The join fails as the owner's did. What worked is said first.
    page.js(&format!(
        "(document.querySelector('.dialog input[type=password]').value = {}, true)",
        json!(RIGHT)
    ));
    press("Connect and install");
    // The Mac cannot reach the release page: that is progress, not a
    // failure. The dialog says, under the steps and not in the warning
    // colour, that alpha is sending it the release, and how large it is.
    let sending = format!(
        "monitoring cannot reach 127.0.0.1:{}: sending spandeck {} from alpha over SSH (3 KB)…",
        releases.port,
        common::fake_releases::TAG
    );
    dialog_says("the release being sent", &sending);
    assert_eq!(
        page.js("[document.querySelector('.dialog .add-note').innerText, document.querySelector('.dialog .add-note').classList.contains('warn-text'), !!document.querySelector('.dialog .add-problem'), document.querySelector('.dialog .add-steps li.now').innerText.includes('Download and install spandeck')]"),
        json!([sending, false, false, true])
    );
    shot("3b-the-release-is-being-sent");
    std::fs::write(at("go"), "").unwrap();
    let said = dialog_says("what happened", "It has not asked to join your fleet");
    assert!(
        said.contains("Sent 3 KB to monitoring, which is checking and installing it.")
            && said.contains("Received 2400 bytes, and their SHA-256 matches."),
        "{said}"
    );
    keyboard(844, 0);
    assert_eq!(
        page.js("[document.querySelector('.dialog .add-worked').innerText, document.querySelector('.dialog .add-problem').innerText, document.querySelector('.dialog .add-next').innerText, [...document.querySelectorAll('.dialog .row.end button')].map(b => b.innerText.trim())]"),
        json!([
            "✓ spandeck 0.4.10 was installed on monitoring and is running.",
            "It has not asked to join your fleet: spandeck on monitoring looked several times for the fleet's stations (alpha) among the machines under your Tailscale login and saw none of them. Check that Tailscale is connected on monitoring and signed in to the same account as your other machines.",
            "Try again: it only asks again, and nothing is downloaded or installed a second time. Or on that machine: `spandeck fleet --join <any station of your fleet>` (its stations: alpha).",
            ["Close", "Try again"]
        ]),
        "{said}"
    );
    let failed = page.js(PLACE);
    assert_eq!(
        (&failed["dialog"][2], &failed["page"]),
        (&json!(true), &json!([0, 0, true])),
        "{failed}"
    );
    shot("4-the-join-failed");

    // "Try again" only asks again: the station is not sent a machine to
    // install on, and the job is the asking alone.
    press("Try again");
    let again = wait_for("the asking, again", 30, || {
        let v = a.call("fleet.add_status", json!({}));
        (v["rejoin"] == json!(true) && v["running"] == json!(false)).then_some(v)
    });
    assert_eq!(
        (
            &again["kind"],
            &again["stage"],
            &again["name"],
            &again["version"]
        ),
        (
            &json!("join"),
            &json!("needs_password"),
            &json!("monitoring"),
            &json!("0.4.10")
        ),
        "{again}"
    );
    dialog_says("the password, for the asking", "Password on monitoring");
    press("Cancel");
    a.call("fleet.add_cancel", json!({}));

    // Neither the Mac nor alpha can reach the release page, and alpha (not
    // a Mac) has no copy that would run there: the dialog says both, what
    // to do, and which stations are the same kind of machine. (The add is
    // started as the dialog starts it, with the password in the call.)
    releases.down(true);
    let state = std::path::PathBuf::from(
        a.call("station.info", json!({}))["dirs"]["state"]
            .as_str()
            .unwrap(),
    );
    std::fs::remove_dir_all(state.join("supply")).unwrap();
    a.call(
        "fleet.add",
        json!({"machine": "n-kestrel", "user": "michael", "password": RIGHT}),
    );
    wait_for("the add to fail", 30, || {
        (a.call("fleet.add_status", json!({}))["stage"] == json!("failed")).then_some(())
    });
    page.wait(
        "the top bar's menu",
        "(b => b ? (b.click(), true) : false)(document.querySelector('.top-more'))",
    );
    page.choose("Add a machine");
    dialog_says("why nothing was sent", "neither can alpha");
    assert_eq!(
        page.js("[document.querySelector('.dialog .add-problem').innerText, !!document.querySelector('.dialog .add-worked'), !!document.querySelector('.dialog .add-note')]"),
        json!([
            format!("kestrel cannot reach 127.0.0.1:{p}, and neither can alpha to fetch the macOS (Apple silicon) build. Add it from a station that is the same kind of machine, or one that can reach 127.0.0.1:{p}: any station of your fleet can add a machine. No station of your fleet is known to be the same kind of machine as kestrel (macOS (Apple silicon)).", p = releases.port),
            false,
            false
        ])
    );
    shot("5-nothing-to-send");
    press("Close");

    // Another dialog, a sheet: New session. Its Folder box, with the
    // keyboard up and the page slid, is in sight above the row of buttons
    // that stays at the sheet's foot; the sheet is what is in sight, no more.
    page.wait("no dialog", "!document.querySelector('.dialog')");
    page.wait(
        "the New session button",
        "(b => b ? (b.click(), true) : false)(document.querySelector('button.new-session'))",
    );
    page.wait(
        "the sheet's Folder box",
        "(i => i ? (i.focus(), true) : false)([...document.querySelectorAll('.dialog.sheet .field')].find(f => f.innerText.startsWith('Folder'))?.querySelector('input'))",
    );
    keyboard(400, 130);
    let sheet = page.js(&format!(
        "(() => {{ const p = {PLACE}; const foot = document.querySelector('.dialog.sheet .sheet-foot').getBoundingClientRect(); return {{ ...p, clear: document.activeElement.getBoundingClientRect().bottom <= foot.top + 0.5 }}; }})()"
    ));
    assert_eq!(
        (
            &sheet["typing"][0],
            &sheet["typing"][2],
            &sheet["dialog"],
            &sheet["clear"],
            &sheet["page"]
        ),
        (
            &json!("INPUT"),
            &json!(true),
            &json!([130, 530, true]),
            &json!(true),
            &json!([0, 0, true])
        ),
        "{sheet}"
    );
    shot("5-new-session-keyboard-up");
}

/// A fixed tailnet for the tests that send a release: machines by name,
/// system and address, all the owner's and online.
fn supply_tailnet(dir: &std::path::Path, machines: &[(&str, &str, &str)]) {
    let peers: serde_json::Map<String, Value> = machines
        .iter()
        .map(|(name, os, ip)| {
            (
                format!("key-{name}"),
                json!({"ID": format!("n-{name}"), "HostName": name, "DNSName": format!("{name}.tail0000.ts.net."),
                    "OS": os, "UserID": 7, "Online": true, "TailscaleIPs": [ip]}),
            )
        })
        .collect();
    std::fs::write(
        dir.join("status.json"),
        json!({"Self": {"UserID": 7, "HostName": "alpha", "TailscaleIPs": ["100.64.0.1"]},
            "User": {"7": {"LoginName": "owner@example.com"}}, "Peer": peers})
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
}

/// The owner's decision of 17 Nov 2026 ("Yes I want it"): a machine that
/// cannot download spandeck is sent it over SSH by the station adding it.
/// Here the machine is another kind from the station (a Mac, added from
/// this machine), so the station has to fetch that kind's release itself.
/// Done when:
///
/// - the station downloads that platform's archive from the release host
///   in its own config, checks it against the SHA-256 published there (as
///   `spandeck update` checks its own), and sends it; the machine checks
///   what arrived, installs it and starts it; the dialog is told as
///   progress, with the size;
/// - a second machine of that kind is sent the kept copy: the archive is
///   not downloaded again, and the published SHA-256 is still asked for;
/// - a kept copy that was changed is found out and replaced by a download;
/// - with the release host away, the kept copy is sent and the dialog says
///   so; with the kept copy spoiled as well there is nothing to send, and
///   the failure names both facts and what to do;
/// - a download that does not match its published SHA-256 is not sent;
/// - what a machine says decides nothing but which of the five platforms:
///   a platform that is not one, or not of the system Tailscale gives for
///   the machine, gets nothing, a second request in a session is not
///   answered, and no address but the configured one is ever asked.
///
/// SSH is a stand-in that runs what arrives on its standard input here, as
/// a Mac (its `uname` says so) whose `curl` reaches nothing. The release
/// for a Mac holds a stand-in for the `spandeck` command, so nothing of a
/// real station starts.
#[test]
#[cfg_attr(windows, ignore = "the machines are stand-ins run by sh")]
fn a_release_for_another_kind_of_machine_is_fetched_checked_kept_and_sent_over_ssh() {
    use common::fake_releases::{Releases, TAG};
    let dir = tempfile::tempdir().unwrap();
    let at = |p: &str| dir.path().join(p);
    for d in ["standin", "remote-bin", "pack"] {
        std::fs::create_dir_all(at(d)).unwrap();
    }
    // The Mac release: `spandeck` and `sd` that answer what the script asks.
    let name = format!("spandeck-{TAG}-aarch64-apple-darwin");
    std::fs::create_dir_all(at("pack").join(&name)).unwrap();
    for b in ["spandeck", "sd"] {
        executable(
            &at("pack").join(&name).join(b),
            "#!/bin/sh\ncase \"$1\" in --version) echo 'spandeck 9.9.9' ;; --json) echo '{ \"code\": \"K7QM2X\" }' ;; esac\nexit 0\n",
        );
    }
    let packed = std::process::Command::new("tar")
        .arg("-czf")
        .arg(at("mac.tar.gz"))
        .arg("-C")
        .arg(at("pack"))
        .arg(&name)
        .status()
        .unwrap();
    assert!(packed.success());
    let mac = std::fs::read(at("mac.tar.gz")).unwrap();
    let file = format!("{name}.tar.gz");
    let releases = Releases::start_with(true, false, &[(file.clone(), mac.clone())]);

    supply_tailnet(
        &at("standin"),
        &[
            ("studio", "macOS", "100.64.0.8"),
            ("mini", "macOS", "100.64.0.9"),
            ("rogue", "macOS", "100.64.0.10"),
        ],
    );
    // The machines: Macs, by their `uname`, with no way out.
    executable(
        &at("remote-bin/uname"),
        "#!/bin/sh\ncase \"$1\" in -m) echo arm64 ;; *) echo Darwin ;; esac\n",
    );
    executable(&at("remote-bin/curl"), "#!/bin/sh\nexit 7\n");
    executable(&at("remote-bin/sleep"), "#!/bin/sh\nexit 0\n");
    for tool in ["systemctl", "launchctl", "loginctl"] {
        executable(&at("remote-bin").join(tool), "#!/bin/sh\nexit 1\n");
    }
    // "ssh". The rogue machine does not run the script: it asks for things
    // it must not be given, and says what it was answered.
    executable(
        &at("standin/ssh"),
        &format!(
            r#"#!/bin/sh
user=$1 host=$2
case "$host" in
  100.64.0.8) m=studio ;;
  100.64.0.9) m=mini ;;
  *)
    while IFS= read -r line; do case "$line" in "main 3<&0"*) break ;; esac; done
    echo "@spandeck step install"
    echo "@spandeck supply ../../etc/passwd network"
    IFS= read -r answer; echo "first answer: $answer"
    echo "@spandeck supply aarch64-apple-darwin network"
    echo "second request sent"
    echo "@spandeck fail supply"
    exit 1 ;;
esac
mkdir -p {dir}/$m/home {dir}/$m/tmp
exec env -i PATH={dir}/remote-bin:/usr/bin:/bin HOME={dir}/$m/home TMPDIR={dir}/$m/tmp \
  SPANDECK_BIN_DIR={dir}/$m/bin sh -s
"#,
            dir = dir.path().display()
        ),
    );
    let path = format!(
        "{}:{}",
        at("standin").display(),
        std::env::var("PATH").unwrap()
    );
    let a = Station::start_with(
        "alpha",
        &format!(
            "add_machine_cmd = [\"{}\", \"{{user}}\", \"{{host}}\"]\n[update]\nurl = \"{}\"\nin_place = false\n",
            at("standin/ssh").display(),
            releases.url()
        ),
        &[("PATH", &path)],
    );
    a.call("fleet.add_enable", json!({"on": true}));
    // An add from its start to where it comes to rest, with what was said.
    let add = |machine: &str| {
        a.call("fleet.add_cancel", json!({}));
        a.call(
            "fleet.add",
            json!({"machine": format!("n-{machine}"), "user": "sam"}),
        );
        wait_for_or(
            "the add to come to rest",
            40,
            || a.call("fleet.add_status", json!({})).to_string(),
            || {
                let v = a.call("fleet.add_status", json!({}));
                (v["running"] == json!(false)).then_some(v)
            },
        )
    };
    let host = format!("127.0.0.1:{}", releases.port);
    let kept = std::path::PathBuf::from(
        a.call("station.info", json!({}))["dirs"]["state"]
            .as_str()
            .unwrap(),
    )
    .join("supply")
    .join(TAG)
    .join("aarch64-apple-darwin.tar.gz");
    let sent =
        format!("studio cannot reach {host}: sending spandeck {TAG} from alpha over SSH (1 KB)…");

    // ---- Fetched for a Mac, checked, sent, checked there, installed and
    // started. (The stand-in's join code is one alpha holds no request
    // for, so the add ends at the asking, having said what worked.)
    let v = add("studio");
    let said = v["lines"].to_string();
    for piece in [
        format!("studio cannot reach {host}. Getting spandeck for macOS (Apple silicon) on alpha to send it over SSH…"),
        sent.clone(),
        format!("Received {} bytes, and their SHA-256 matches.", mac.len()),
        format!("Installed spandeck {TAG} to "),
        "Sent 1 KB to studio, which is checking and installing it.".to_string(),
        "spandeck is running.".to_string(),
    ] {
        assert!(said.contains(&piece), "{piece:?} is not in: {v}");
    }
    assert_eq!(
        (&v["stage"], &v["problem"]["kind"], &v["problem"]["worked"]),
        (
            &json!("failed"),
            &json!("join"),
            &json!("spandeck 9.9.9 was installed on studio and is running.")
        ),
        "{v}"
    );
    assert!(at("studio/bin/spandeck").exists() && at("studio/bin/sd").exists());
    assert_eq!(std::fs::read_dir(at("studio/tmp")).unwrap().count(), 0);
    assert_eq!(
        (releases.asked(&file), std::fs::read(&kept).unwrap() == mac),
        (1, true)
    );
    // The ship's log says what was sent, by its SHA-256, and from where.
    let logged = a
        .log("fleet.add")
        .iter()
        .map(|e| e["data"].clone())
        .find(|d| d["outcome"] == json!("sending a release over SSH"));
    let logged = logged.expect("the sending is in the ship's log");
    assert_eq!(
        (&logged["release"], &logged["platform"], &logged["bytes"]),
        (
            &json!(TAG),
            &json!("aarch64-apple-darwin"),
            &json!(mac.len())
        ),
        "{logged}"
    );
    assert!(logged["from"]
        .as_str()
        .unwrap()
        .starts_with("the release host"));
    assert_eq!(logged["sha256"].as_str().unwrap().len(), 64);

    // ---- A second Mac: the kept copy, checked against the SHA-256 that is
    // published now. The archive is not downloaded a second time.
    let sums = releases.asked(&format!("{file}.sha256"));
    let v = add("mini");
    assert!(at("mini/bin/spandeck").exists(), "{v}");
    assert_eq!(
        (
            releases.asked(&file),
            releases.asked(&format!("{file}.sha256"))
        ),
        (1, sums + 1),
        "{v}"
    );

    // ---- The kept copy was changed: found out, thrown away, downloaded
    // again, and what is sent is the release.
    std::fs::remove_dir_all(at("studio")).unwrap();
    let mut spoiled = mac.clone();
    spoiled[10] ^= 0xff;
    std::fs::write(&kept, &spoiled).unwrap();
    let v = add("studio");
    assert!(at("studio/bin/spandeck").exists(), "{v}");
    assert_eq!(
        (releases.asked(&file), std::fs::read(&kept).unwrap() == mac),
        (2, true),
        "{v}"
    );

    // ---- The release host is away: the copy downloaded and checked
    // earlier is sent, and the dialog says that is what it is.
    releases.down(true);
    std::fs::remove_dir_all(at("studio")).unwrap();
    let v = add("studio");
    assert!(at("studio/bin/spandeck").exists(), "{v}");
    assert!(
        v["lines"].to_string().contains(&format!(
            "studio cannot reach {host}, and alpha cannot reach {host} now either: sending the release alpha downloaded and checked earlier, {TAG} (1 KB)…"
        )),
        "{v}"
    );
    // And with that copy spoiled there is nothing to send: this station is
    // another kind of machine, so its own copy would not run on a Mac. The
    // failure names both facts and what to do. Nothing is installed.
    std::fs::remove_dir_all(at("studio")).unwrap();
    std::fs::write(&kept, &spoiled).unwrap();
    let v = add("studio");
    assert_eq!(
        (&v["stage"], &v["problem"]["kind"], &v["problem"]["text"]),
        (
            &json!("failed"),
            &json!("no_release"),
            &json!(format!(
                "studio cannot reach {host}, and neither can alpha to fetch the macOS (Apple silicon) build. Add it from a station that is the same kind of machine, or one that can reach {host}: any station of your fleet can add a machine. No station of your fleet is known to be the same kind of machine as studio (macOS (Apple silicon))."
            ))
        ),
        "{v}"
    );
    assert!(!at("studio/bin").exists(), "{v}");

    // ---- A download that does not match its published SHA-256 is not
    // sent, and the spoiled copy is not either.
    releases.down(false);
    releases.wrong_sums(true);
    let v = add("studio");
    assert_eq!(
        (&v["stage"], &v["problem"]["kind"]),
        (&json!("failed"), &json!("supply_checksum")),
        "{v}"
    );
    assert!(
        v["problem"]["text"].as_str().unwrap().starts_with(&format!(
            "alpha downloaded the macOS (Apple silicon) build of {TAG} to send to studio, and it does not match its published checksum, so nothing was sent and nothing was installed."
        )),
        "{v}"
    );
    assert!(!at("studio/bin").exists() && !kept.exists(), "{v}");
    releases.wrong_sums(false);

    // ---- What a machine says decides nothing but the platform. A
    // platform that is no platform gets nothing; a second request in the
    // same session is not answered at all.
    let before = releases.all_asked().len();
    let v = add("rogue");
    let said = v["lines"].to_string();
    assert!(
        said.contains("first answer: none") && said.contains("second request sent"),
        "{v}"
    );
    assert_eq!(
        (&v["stage"], &v["problem"]["kind"]),
        (&json!("failed"), &json!("unsupported")),
        "{v}"
    );
    assert_eq!(releases.all_asked().len(), before, "{v}");
    // Everything that was ever asked of the release host was asked at the
    // address in alpha's config, for the Mac release or the newest name.
    for path in releases.all_asked() {
        assert!(
            path == "latest" || path == file || path == format!("{file}.sha256"),
            "{path}"
        );
    }
}

/// The same decision, for a machine of the station's own kind, with the
/// real program: when neither the machine nor the station can reach the
/// release host, the station sends its own copy, laid out as a release is,
/// and says so; the README's install script installs it, it starts, asks to
/// join, shows its code and is approved. And with the release host
/// answering the station (not the machine), the published release is
/// fetched, checked and sent the same way, and the machine joins.
#[test]
#[ignore = "merge tier: installs and starts two real stations (about half a minute)"]
fn a_machine_that_cannot_download_is_sent_this_stations_own_copy_and_joins() {
    let releases = common::fake_releases::Releases::start(true);
    let dir = tempfile::tempdir().unwrap();
    let at = |p: &str| dir.path().join(p);
    for d in ["standin", "remote-bin"] {
        std::fs::create_dir_all(at(d)).unwrap();
    }
    let system = if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "linux"
    };
    supply_tailnet(
        &at("standin"),
        &[
            ("charlie", system, "100.64.0.7"),
            ("delta", system, "100.64.0.8"),
        ],
    );
    executable(&at("remote-bin/curl"), "#!/bin/sh\nexit 7\n");
    for tool in ["systemctl", "launchctl", "loginctl"] {
        executable(&at("remote-bin").join(tool), "#!/bin/sh\nexit 1\n");
    }
    let (pa, pb) = (free_port(), free_port());
    let mut ends = vec![];
    let mut ports = vec![];
    for m in ["charlie", "delta"] {
        let port = free_port();
        std::fs::create_dir_all(at(m).join("config")).unwrap();
        std::fs::create_dir_all(at(m).join("home")).unwrap();
        std::fs::write(
            at(m).join("config/config.toml"),
            format!("station = \"{m}\"\nweb_port = {port}\nweb_bind = [\"127.0.0.1\"]\ndiscovery = false\nkeep_awake_cmd = [\"sleep\", \"300\"]\nprices.auto = false\npeers = [\"127.0.0.1:{pa}\", \"127.0.0.1:{pb}\"]\n"),
        )
        .unwrap();
        ends.push(KillAtEnd(at(m).join("state/spandeckd.pid")));
        ports.push(port);
    }
    executable(
        &at("standin/ssh"),
        &format!(
            r#"#!/bin/sh
case "$2" in 100.64.0.7) m=charlie ;; *) m=delta ;; esac
exec env -i PATH={dir}/remote-bin:/usr/bin:/bin HOME={dir}/$m/home SPANDECK_STATE_DIR={dir}/$m/state \
  SPANDECK_CONFIG_DIR={dir}/$m/config SPANDECK_BIN_DIR={dir}/$m/bin SPANDECK_LOG=warn sh -s
"#,
            dir = dir.path().display()
        ),
    );
    let path = format!(
        "{}:{}",
        at("standin").display(),
        std::env::var("PATH").unwrap()
    );
    let version = env!("CARGO_PKG_VERSION");
    let joins = |a: &Station, machine: &str, says: &str| {
        a.call("fleet.add_enable", json!({"on": true}));
        a.call(
            "fleet.add",
            json!({"machine": format!("n-{machine}"), "user": "sam"}),
        );
        let rest = |stage: &str| {
            wait_for_or(
                &format!("the add to reach {stage}"),
                120,
                || a.call("fleet.add_status", json!({})).to_string(),
                || {
                    let v = a.call("fleet.add_status", json!({}));
                    assert!(
                        v["stage"] == json!(stage) || v["running"] == json!(true),
                        "ended somewhere else: {v}"
                    );
                    (v["stage"] == json!(stage)).then_some(v)
                },
            )
        };
        let v = rest("approve");
        let said = v["lines"].to_string();
        assert!(said.contains(says), "{says:?} is not in: {v}");
        assert!(
            said.contains("and their SHA-256 matches.") && said.contains("(checksum verified)"),
            "{v}"
        );
        assert_eq!(v["version"], json!(version), "{v}");
        assert!(at(machine).join("bin/spandeck").exists() && at(machine).join("bin/sd").exists());
        a.call("trust.approve", json!({"request": v["request"]}));
        rest("done");
    };

    // Neither end reaches the release host: this station's own copy.
    let a = Station::start_on(
        "alpha",
        pa,
        &format!(
            "add_machine_cmd = [\"{}\", \"{{user}}\", \"{{host}}\"]\npeers = [\"127.0.0.1:{}\"]\n[update]\nurl = \"http://127.0.0.1:9\"\nin_place = false\n",
            at("standin/ssh").display(),
            ports[0]
        ),
        &[("PATH", &path)],
    );
    joins(
        &a,
        "charlie",
        &format!("charlie cannot reach 127.0.0.1:9, and neither can alpha: sending this station's own copy, {version} ("),
    );
    let from = a
        .log("fleet.add")
        .iter()
        .map(|e| e["data"].clone())
        .find(|d| d["outcome"] == json!("sending a release over SSH"));
    assert_eq!(
        from.expect("the sending is in the ship's log")["from"],
        json!("this station's own program")
    );
    // What was put together to send is not kept.
    let state = std::path::PathBuf::from(
        a.call("station.info", json!({}))["dirs"]["state"]
            .as_str()
            .unwrap(),
    );
    let left: Vec<_> = std::fs::read_dir(state.join("supply"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");

    // The release host answers the station: the published release.
    let b = Station::start_on(
        "bravo",
        pb,
        &format!(
            "add_machine_cmd = [\"{}\", \"{{user}}\", \"{{host}}\"]\npeers = [\"127.0.0.1:{}\"]\n[update]\nurl = \"{}\"\nin_place = false\n",
            at("standin/ssh").display(),
            ports[1],
            releases.url()
        ),
        &[("PATH", &path)],
    );
    joins(
        &b,
        "delta",
        &format!(
            "delta cannot reach 127.0.0.1:{}: sending spandeck {} from bravo over SSH (",
            releases.port,
            common::fake_releases::TAG
        ),
    );
    drop(ends);
}
