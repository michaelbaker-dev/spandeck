//! A Control Deck's requests are handled in lanes (DECISIONS.md, "Typing,
//! Stop and Allow never wait behind other requests"): a station that hangs
//! slows only what is for it, and what is sent to one pane keeps its order
//! by every road. Scenario tests with real stations (merge tier), and one
//! timing (nightly). Run with `--ignored`.

// A station is frozen with a Unix signal, and the panes are `/bin/sh`.
#![cfg(unix)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// What a page has been sent so far.
#[derive(Default)]
struct Seen {
    /// Answers by request id, in the order they came.
    answers: Vec<(u64, Result<Value, Value>)>,
    /// A pane's output, by channel.
    output: HashMap<u64, Vec<u8>>,
    /// Messages on a channel that are not output.
    said: Vec<Value>,
}

/// A signed-in Control Deck, as a browser tab is one: it sends without
/// waiting, and takes answers in whatever order they come.
struct Deck {
    sink: futures_util::stream::SplitSink<Socket, Message>,
    seen: Arc<Mutex<Seen>>,
    hello: Value,
    id: u64,
}

impl Deck {
    /// `caps`: what the page says it understands (a page from before says
    /// nothing).
    async fn open(s: &Station, client: &str, caps: Value) -> Deck {
        let url = format!("ws://127.0.0.1:{}/api/ws", s.port);
        let (ws, _) = tokio_tungstenite::connect_async_with_config(url, None, true)
            .await
            .unwrap();
        let (mut sink, mut stream) = ws.split();
        let auth = json!({"auth": {"token": s.token(), "client": client, "caps": caps}});
        sink.send(Message::Text(auth.to_string().into()))
            .await
            .unwrap();
        let hello = match stream.next().await {
            Some(Ok(Message::Text(t))) => serde_json::from_str::<Value>(&t).unwrap(),
            other => panic!("no hello: {other:?}"),
        };
        assert_eq!(hello["auth"]["ok"], json!(true), "{hello}");
        let seen: Arc<Mutex<Seen>> = Arc::default();
        let into = seen.clone();
        tokio::spawn(async move {
            while let Some(Ok(m)) = stream.next().await {
                let mut seen = into.lock().unwrap();
                match m {
                    Message::Binary(b) if b.len() >= 5 => {
                        let chan = u32::from_le_bytes([b[1], b[2], b[3], b[4]]) as u64;
                        seen.output.entry(chan).or_default().extend(&b[5..]);
                    }
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        match v["id"].as_u64() {
                            Some(id) => {
                                let answer = match v.get("error") {
                                    Some(e) => Err(e.clone()),
                                    None => Ok(v["result"].clone()),
                                };
                                seen.answers.push((id, answer));
                            }
                            None if v.get("chan").is_some() => seen.said.push(v),
                            None => {}
                        }
                    }
                    _ => {}
                }
            }
        });
        Deck {
            sink,
            seen,
            hello,
            id: 0,
        }
    }

    /// Send a request and do not wait: its id.
    async fn ask(&mut self, method: &str, params: Value) -> u64 {
        self.id += 1;
        let req = json!({"id": self.id, "method": method, "params": params});
        self.sink
            .send(Message::Text(req.to_string().into()))
            .await
            .unwrap();
        self.id
    }

    /// Through this station to another, as the page's `api.on` does.
    async fn ask_at(&mut self, station: &str, method: &str, params: Value) -> u64 {
        let call = json!({"station": station, "method": method, "params": params});
        self.ask("peer.call", call).await
    }

    /// Sent as a key is: no answer is asked for.
    async fn key(&mut self, chan: u64, data: &str) {
        let req =
            json!({"id": null, "method": "pane.input", "params": {"chan": chan, "data": data}});
        self.sink
            .send(Message::Text(req.to_string().into()))
            .await
            .unwrap();
    }

    fn answered(&self, id: u64) -> Option<Result<Value, Value>> {
        let seen = self.seen.lock().unwrap();
        seen.answers
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, a)| a.clone())
    }

    /// Which of these have been answered by now.
    fn answered_of(&self, ids: &[u64]) -> Vec<u64> {
        ids.iter()
            .copied()
            .filter(|id| self.answered(*id).is_some())
            .collect()
    }

    async fn until<T>(&self, what: &str, f: impl Fn(&Seen) -> Option<T>) -> T {
        let began = Instant::now();
        loop {
            if let Some(v) = f(&self.seen.lock().unwrap()) {
                return v;
            }
            assert!(began.elapsed() < Duration::from_secs(30), "no {what}");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    async fn answer(&self, id: u64) -> Result<Value, Value> {
        self.until(&format!("answer to request {id}"), |seen| {
            seen.answers
                .iter()
                .find(|(i, _)| *i == id)
                .map(|(_, a)| a.clone())
        })
        .await
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.ask(method, params.clone()).await;
        self.answer(id)
            .await
            .unwrap_or_else(|e| panic!("{method} {params}: {e}"))
    }

    async fn attach(&mut self, params: Value) -> u64 {
        self.call("pane.attach", params).await["chan"]
            .as_u64()
            .unwrap()
    }

    /// Wait until the pane's output on this channel has shown `needle`.
    async fn shows(&self, chan: u64, needle: &str) {
        self.until(&format!("{needle} on channel {chan}"), |seen| {
            seen.output
                .get(&chan)
                .is_some_and(|o| String::from_utf8_lossy(o).contains(needle))
                .then_some(())
        })
        .await
    }
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

/// Something that waits on a station, kept off the runtime's threads.
async fn off<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

const SIGSTOP: i32 = if cfg!(target_os = "macos") { 17 } else { 19 };
const SIGCONT: i32 = if cfg!(target_os = "macos") { 19 } else { 18 };

/// The questions a page asks one station at each refresh.
const REFRESH: [&str; 7] = [
    "pane.list",
    "project.list",
    "task.list",
    "msg.recent",
    "approval.list",
    "notify.list",
    "stop.list",
];

/// The audit's gap 1 (FAILURES section 4, measured on 0.4.2): with one
/// station cut off or hung, a key typed into a pane on the station in
/// front of the owner took up to 19.5 s, because a page's requests were
/// taken one at a time and its refresh asks every station seven things.
/// Done when, with a station frozen and three refreshes' worth of
/// questions to it unanswered: a key typed here is echoed, Stop on an
/// agent here is confirmed, Pause and a key reach a third station, and a
/// read of it is answered, each while those questions are still
/// unanswered; the question past the station's limit is refused at once in
/// words that name the station; a fleet-wide stop holds up typing nowhere
/// but at the frozen station; and every question is answered in the end.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts three daemons and freezes one"]
async fn a_station_that_hangs_slows_only_itself() {
    let (a, b, c) = off(Station::three).await;
    let (a, b, c) = (Arc::new(a), Arc::new(b), Arc::new(c));
    let (sa, sb) = (a.clone(), b.clone());
    let (here, bee, worker) = off(move || {
        let worker = sa.fake(
            json!({"steps": [{"prompt": "build"}, {"work": 600_000}]}),
            json!({"name": "worker"}),
        );
        sa.wait_state(&worker, "working");
        (shell(&sa, "here"), shell(&sb, "bee"), worker)
    })
    .await;

    let mut deck = Deck::open(&a, "tab-1", json!(["busy"])).await;
    assert_eq!(deck.hello["auth"]["lanes"], json!(true));
    deck.call("events.subscribe", json!({})).await;
    let at_here = deck.attach(json!({"pane": here})).await;
    let at_bee = deck.attach(json!({"pane": bee, "station": "bravo"})).await;

    let sc = c.clone();
    off(move || {
        sc.signal(SIGSTOP);
        sc.wait_frozen();
    })
    .await;

    // Three refreshes of the page, none waiting for the one before.
    let mut held = vec![];
    for _ in 0..3 {
        for method in REFRESH {
            held.push(deck.ask_at("charlie", method, json!({})).await);
        }
    }
    // One station may have twenty reads waiting; the next is not taken,
    // and the page is told at once.
    let over = held.pop().unwrap();
    let refused = deck.answer(over).await.unwrap_err();
    assert_eq!(refused["code"], json!("busy"), "{refused}");
    let said = refused["message"].as_str().unwrap();
    assert!(said.contains("charlie is busy"), "{said}");
    assert!(said.contains("Nothing was changed"), "{said}");

    // A key typed into a pane here.
    deck.key(at_here, "echo here-$((40+2))\r").await;
    deck.shows(at_here, "here-42").await;
    assert_eq!(deck.answered_of(&held), [0u64; 0], "after the local key");
    // Stop on an agent here.
    let stop = deck.call("pane.stop", json!({"pane": worker})).await;
    assert_eq!(stop["stopped"], json!(true), "{stop}");
    assert_eq!(deck.answered_of(&held), [0u64; 0], "after the local stop");
    // A third station: Pause, a key, a read.
    let pause = deck
        .ask_at("bravo", "pane.pause", json!({"pane": bee, "paused": true}))
        .await;
    assert_eq!(deck.answer(pause).await.unwrap()["ok"], json!(true));
    deck.key(at_bee, "echo bee-$((1+2))\r").await;
    deck.shows(at_bee, "bee-3").await;
    let list = deck.ask_at("bravo", "pane.list", json!({})).await;
    assert!(deck.answer(list).await.unwrap().is_array());
    // A change to this station, too.
    deck.call("pane.rename", json!({"pane": here, "name": "renamed"}))
        .await;
    assert_eq!(deck.answered_of(&held), [0u64; 0], "after bravo");

    // What this station counted: a request refused, and nothing typed
    // waited (the page that shows it reads the same numbers).
    let perf = deck.call("perf.get", json!({})).await;
    let waited = &perf["requests"]["waited"];
    assert!(
        waited["busy"]["since_start"].as_u64() >= Some(1),
        "{waited}"
    );
    assert!(
        waited["lanes"]["input"]["count"].as_u64() >= Some(2),
        "{waited}"
    );
    assert!(waited["lanes"]["urgent"]["max_ms"].as_f64() < Some(1000.0));

    // "Stop every agent in the fleet", with one station frozen: its part
    // for this station and for bravo is done at once, and what is typed
    // after it here and at bravo arrives, while the frozen station's part
    // (and so the stop's answer) is still waited for.
    let fleet = deck.ask("stop.all", json!({"scope": "fleet"})).await;
    deck.key(at_here, "echo after-$((2+5))\r").await;
    deck.key(at_bee, "echo after-$((3+5))\r").await;
    deck.shows(at_here, "after-7").await;
    deck.shows(at_bee, "after-8").await;
    assert!(
        deck.answered(fleet).is_none(),
        "the fleet stop was answered"
    );
    assert_eq!(deck.answered_of(&held), [0u64; 0], "after the fleet stop");

    // The frozen station comes back: every question is answered, and the
    // stop says what became of each station.
    c.signal(SIGCONT);
    let stops = deck.answer(fleet).await.unwrap();
    let rows = stops["stops"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{stops}");
    for row in rows.iter().filter(|r| r["station"] != json!("charlie")) {
        assert_eq!(row["status"], json!("confirmed"), "{stops}");
    }
    for id in &held {
        let _ = deck.answer(*id).await;
    }
    drop(deck);
    off(move || drop((a, b, c))).await;
}

/// REVIEW B5: "lanes ... reorder input": a paste and the Enter after it
/// go by different roads; a phone typing into a pane it watches sends each
/// key through `peer.call`; Stop must not be followed by what it was meant
/// to stop, and what is typed after it must arrive; Pause is urgent. Done
/// when everything addressed to one pane is applied in the order it was
/// sent on the connection, with nothing waited for between sends:
/// through another station (ten pastes each followed by Enter; a line
/// typed a key at a time through `peer.call`), here by name and by
/// channel, around a Stop of a program that reads and of one that does
/// not, and for Pause and Resume; and `ws_lanes = false` gives a
/// connection that still works.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts two daemons; waits out a stop that cannot be delivered"]
async fn what_is_sent_to_one_pane_keeps_its_order_by_every_road() {
    let (a, b) = off(Station::pair).await;
    let (a, b) = (Arc::new(a), Arc::new(b));
    let (sa, sb) = (a.clone(), b.clone());
    let (here, bee, sea, deaf, worker) = off(move || {
        let deaf = sa.call(
            "pane.spawn",
            json!({"agent": "fake", "program": "/bin/sh", "args": ["-c", "sleep 600"], "name": "deaf", "cwd": sa.root.join("home")}),
        )["id"]
            .as_str()
            .unwrap()
            .to_string();
        let worker = sa.fake(
            json!({"steps": [{"prompt": "build"}, {"work": 600_000}], "on_input": [{"turn_end": "done"}]}),
            json!({"name": "worker"}),
        );
        sa.wait_state(&worker, "working");
        (
            shell(&sa, "here"),
            shell(&sb, "bee"),
            shell(&sb, "sea"),
            deaf,
            worker,
        )
    })
    .await;
    let mut deck = Deck::open(&a, "tab-1", json!(["busy"])).await;

    // ---- through another station: a paste goes as a call of its own,
    // the Enter after it down the pane's channel.
    let at_bee = deck.attach(json!({"pane": bee, "station": "bravo"})).await;
    for i in 0..10 {
        let text = format!("echo p{i}-$(({i}+1))");
        let paste = json!({"pane": bee, "text": text, "enter": false});
        deck.ask_at("bravo", "pane.paste", paste).await;
        deck.key(at_bee, "\r").await;
    }
    deck.shows(at_bee, "p9-10").await;
    let (sb, pane) = (b.clone(), bee.clone());
    let screen = off(move || sb.text(&pane)).await;
    let mut from = 0;
    for i in 0..10 {
        let line = format!("p{i}-{}", i + 1);
        let at = screen[from..].find(&line);
        from += at.unwrap_or_else(|| panic!("{line} is missing or out of order:\n{screen}"));
    }

    // ---- a phone typing into a pane it only watches: every key is a
    // call through this station to the pane's.
    let at_sea = deck
        .attach(json!({"pane": sea, "station": "bravo", "mode": "watch"}))
        .await;
    for key in "echo w-$((6+7))\r".chars() {
        let typed = json!({"pane": sea, "data": key.to_string()});
        deck.ask_at("bravo", "pane.input", typed).await;
    }
    deck.shows(at_sea, "w-13").await;

    // ---- here: by the pane's name, by its id and by its channel.
    let at_here = deck.attach(json!({"pane": here})).await;
    for i in 0..5 {
        let text = format!("echo h{i}-$(({i}+1))");
        deck.ask("pane.paste", json!({"pane": "here", "text": text}))
            .await;
        deck.key(at_here, "\r").await;
        deck.ask("pane.send_text", json!({"pane": here, "text": "true"}))
            .await;
    }
    deck.shows(at_here, "h4-5").await;

    // ---- Stop, then a line typed after it, with no wait between: the
    // turn is interrupted first, the line arrives, and because the owner
    // typed after stopping, the pane is not left held.
    let stop = deck.ask("pane.stop", json!({"pane": worker})).await;
    let after = json!({"pane": worker, "text": "typed after the stop"});
    let typed = deck.ask("pane.send_text", after).await;
    assert_eq!(deck.answer(stop).await.unwrap()["stopped"], json!(true));
    deck.answer(typed).await.unwrap();
    let (sa, pane) = (a.clone(), worker.clone());
    let (screen, held) = off(move || {
        let screen = sa.wait_text(&pane, "[fake] got: typed after the stop");
        (screen, sa.pane(&pane)["held"].clone())
    })
    .await;
    let stopped = screen.find("[fake] interrupted").expect("interrupted");
    assert!(
        stopped < screen.find("typed after the stop").unwrap(),
        "{screen}"
    );
    assert_eq!(held, Value::Null, "typed after the stop, and still held");

    // ---- a program that is not reading: what was typed before the Stop
    // is thrown away and its sender told; what was typed after is not.
    let at_deaf = deck.attach(json!({"pane": deaf})).await;
    let lines = "0123456789abcdef0123456789abcdef0123456789abcdef\r".repeat(420);
    deck.key(at_deaf, &lines).await;
    deck.key(at_deaf, "typed before the stop").await;
    let stop = deck.ask("pane.stop", json!({"pane": deaf})).await;
    deck.key(at_deaf, "typed after the stop").await;
    let refused = deck.answer(stop).await.unwrap_err();
    assert_eq!(refused["code"], json!("not_reading"), "{refused}");
    let told = deck
        .until("word of what was thrown away", |seen| {
            seen.said
                .iter()
                .find(|v| v["chan"] == json!(at_deaf) && v.get("input_discarded").is_some())
                .cloned()
        })
        .await;
    assert_eq!(told["input_discarded"], json!(2), "{told}");
    let sa = a.clone();
    let log = off(move || sa.log("pane.stop_not_reading")).await;
    assert_eq!(log[0]["data"]["discarded"], json!(2), "{log:?}");
    // Kill is not held up by the program, nor by the stop before it.
    deck.call("pane.kill", json!({"pane": deaf})).await;

    // ---- Pause and Resume, sent together, end as the last one said.
    for round in 0..6 {
        let last = round % 2 == 0;
        let mut id = 0;
        for paused in [true, false, true, false, last] {
            id = deck
                .ask("pane.pause", json!({"pane": here, "paused": paused}))
                .await;
        }
        // One pane's requests are done in order: the last one's answer
        // comes after all of them.
        deck.answer(id).await.unwrap();
        let pane = deck.call("pane.get", json!({"pane": here})).await;
        assert_eq!(pane["paused"], json!(last), "round {round}");
    }

    // ---- the switch: off, a new connection takes its requests one at a
    // time, as before, and still works; a connection keeps what it began
    // with.
    deck.call("station.flags", json!({"set": {"ws_lanes": false}}))
        .await;
    let mut plain = Deck::open(&a, "tab-2", json!([])).await;
    assert_eq!(plain.hello["auth"]["lanes"], json!(false));
    let at = plain.attach(json!({"pane": here})).await;
    plain.key(at, "echo plain-$((2+2))\r").await;
    plain.shows(at, "plain-4").await;
    let at = plain.attach(json!({"pane": bee, "station": "bravo"})).await;
    plain.key(at, "echo plain-$((3+3))\r").await;
    plain.shows(at, "plain-6").await;
    assert!(plain.call("pane.list", json!({})).await.is_array());
    deck.key(at_here, "echo lanes-$((4+4))\r").await;
    deck.shows(at_here, "lanes-8").await;

    drop((deck, plain));
    off(move || drop((a, b))).await;
}

/// The target of the performance plan's step 2: "keystroke to echo, p95,
/// local pane, one station cut off: 15 ms or less" (19.5 s at worst on
/// 0.4.2). A page refreshes every 2.5 s without waiting for the last; one
/// of its two stations is frozen throughout.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "nightly: a timing, 12 s with a frozen station"]
async fn a_local_key_is_echoed_at_once_while_a_station_is_frozen() {
    let (a, b) = off(Station::pair).await;
    let (a, b) = (Arc::new(a), Arc::new(b));
    let sa = a.clone();
    let here = off(move || shell(&sa, "here")).await;
    let mut deck = Deck::open(&a, "tab-1", json!(["busy"])).await;
    let at = deck.attach(json!({"pane": here})).await;
    let sb = b.clone();
    off(move || {
        sb.signal(SIGSTOP);
        sb.wait_frozen();
    })
    .await;
    let mut took = vec![];
    for round in 0..4 {
        for method in REFRESH {
            deck.ask_at("bravo", method, json!({})).await;
        }
        for n in 0..12 {
            let word = format!("k{round}x{n}");
            let began = Instant::now();
            deck.key(at, &format!("echo {word}-$((1+1))\r")).await;
            deck.shows(at, &format!("{word}-2")).await;
            took.push(began.elapsed());
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    took.sort();
    let p95 = took[took.len() * 95 / 100];
    eprintln!(
        "keystroke to echo with a station frozen: p50 {:?}, p95 {p95:?}, max {:?} (n={})",
        took[took.len() / 2],
        took[took.len() - 1],
        took.len()
    );
    assert!(p95 <= Duration::from_millis(15), "p95 {p95:?}");
    b.signal(SIGCONT);
    drop(deck);
    off(move || drop((a, b))).await;
}
