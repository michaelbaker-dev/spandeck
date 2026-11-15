//! The Control Deck's socket: channel numbers that name nothing on another
//! connection, here through a second station (merge tier), and answers that
//! are not held back (a timing, nightly). Scenario tests with real
//! stations, run with `--ignored`.

mod common;

use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// A signed-in Control Deck, as a browser tab is one: a socket, and the
/// requests sent on it.
struct Deck {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
}

impl Deck {
    async fn open(s: &Station, client: &str) -> Deck {
        // As a browser does, it sends what it writes at once.
        let url = format!("ws://127.0.0.1:{}/api/ws", s.port);
        let (ws, _) = tokio_tungstenite::connect_async_with_config(url, None, true)
            .await
            .unwrap();
        let mut deck = Deck { ws, id: 0 };
        deck.say(json!({"auth": {"token": s.token(), "client": client}}))
            .await;
        let hello = deck.text().await;
        assert_eq!(hello["auth"]["ok"], json!(true), "{hello}");
        deck
    }

    async fn say(&mut self, v: Value) {
        self.ws
            .send(Message::Text(v.to_string().into()))
            .await
            .unwrap();
    }

    /// The next message that is text.
    async fn text(&mut self) -> Value {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(15), self.ws.next())
                .await
                .expect("the station to say something")
                .unwrap()
                .unwrap();
            if let Message::Text(t) = m {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }

    /// Ask, and wait for the answer: `Ok(result)` or `Err(error)`.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        self.id += 1;
        let id = self.id;
        self.say(json!({"id": id, "method": method, "params": params}))
            .await;
        loop {
            let v = self.text().await;
            if v["id"] == json!(id) {
                return match v.get("error") {
                    Some(e) => Err(e.clone()),
                    None => Ok(v["result"].clone()),
                };
            }
        }
    }

    /// Sent as a key is: no answer is asked for.
    async fn key(&mut self, chan: u64, data: &str) {
        self.say(
            json!({"id": null, "method": "pane.input", "params": {"chan": chan, "data": data}}),
        )
        .await;
    }

    async fn chan(&mut self, method: &str, params: Value) -> u64 {
        let answer = self.call(method, params.clone()).await;
        answer.unwrap_or_else(|e| panic!("{method} {params}: {e}"))["chan"]
            .as_u64()
            .unwrap()
    }
}

/// Something that waits on the two stations, kept off the runtime's threads.
type Blocking = Box<dyn FnOnce(&Station, &Station) -> String + Send>;

fn shell(s: &Station, name: &str) -> String {
    s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": name, "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The audit's gap 13 (FAILURES section 5, measured on 0.4.2: "a key
/// carrying pane B's old channel number was written to pane C, which got
/// that number on the new connection"), for every kind of channel a deck
/// has: a pane here, a pane on another station, and a watched conversation
/// either way. Done when: no number of the connection that dropped is given
/// again by the one that replaces it; typing, resizing and detaching with
/// an old number are refused or do nothing; and nothing typed with one
/// reaches any pane, at this station or through it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "merge tier: starts two daemons"]
async fn a_channel_number_from_before_a_reconnect_reaches_no_pane() {
    let (a, b) = tokio::task::spawn_blocking(Station::pair).await.unwrap();
    let (a, b) = (std::sync::Arc::new(a), std::sync::Arc::new(b));
    let blocking = |f: Blocking| {
        let (a, b) = (a.clone(), b.clone());
        async move {
            tokio::task::spawn_blocking(move || f(&a, &b))
                .await
                .unwrap()
        }
    };
    let (here, there_b, there_c) = (shell(&a, "here"), shell(&b, "bee"), shell(&b, "sea"));

    // The connection that will drop: a pane here, a pane at bravo through
    // this station, and a conversation watched each way.
    let mut deck = Deck::open(&a, "tab-1").await;
    let on_bravo = |pane: &str| json!({"pane": pane, "station": "bravo"});
    let old = [
        deck.chan("pane.attach", json!({"pane": here})).await,
        deck.chan("pane.attach", on_bravo(&there_b)).await,
        deck.chan("pane.conversation.watch", json!({"pane": here}))
            .await,
        deck.chan("pane.conversation.watch", on_bravo(&there_b))
            .await,
    ];
    // Through the gateway the number is this station's, not bravo's, and
    // what is typed with it arrives.
    deck.key(old[1], "echo first-$((1+1))\r").await;
    let pane = there_b.clone();
    blocking(Box::new(move |_, b| b.wait_text(&pane, "first-2"))).await;
    drop(deck);

    // The page comes back and shows other panes: bravo's second pane and
    // this station's own.
    let mut deck = Deck::open(&a, "tab-1").await;
    let new_c = deck.chan("pane.attach", on_bravo(&there_c)).await;
    let new = [
        new_c,
        deck.chan("pane.attach", json!({"pane": here})).await,
        deck.chan("pane.conversation.watch", on_bravo(&there_c))
            .await,
        deck.chan("pane.conversation.watch", json!({"pane": here}))
            .await,
    ];
    for n in old {
        assert!(!new.contains(&n), "{n} was given again: {old:?} {new:?}");
        // Asked for an answer, each is refused in words.
        for (method, params) in [
            (
                "pane.input",
                json!({"chan": n, "data": "echo stray-$((3+4))\r"}),
            ),
            ("pane.resize", json!({"chan": n, "cols": 31, "rows": 7})),
        ] {
            let refused = deck.call(method, params).await.unwrap_err();
            assert_eq!(refused["message"], json!("no such channel"), "{method}");
        }
        // Sent as the page sends a key, and as it lets go of the terminal
        // it had before it attaches again.
        deck.key(n, "echo stray-$((3+4))\r").await;
        deck.call("pane.detach", json!({"chan": n})).await.unwrap();
    }
    // The channels of this connection are untouched by all of that: what
    // is typed next arrives, at bravo and here.
    deck.key(new[0], "echo here-$((2+3))\r").await;
    deck.key(new[1], "echo here-$((2+3))\r").await;
    let texts = blocking(Box::new(move |a, b| {
        b.wait_text(&there_c, "here-5");
        a.wait_text(&here, "here-5");
        for (s, pane) in [(a, &here), (b, &there_b), (b, &there_c)] {
            assert_ne!(s.pane(pane)["cols"], json!(31), "{pane} was resized");
        }
        format!(
            "{}\n{}\n{}",
            a.text(&here),
            b.text(&there_b),
            b.text(&there_c)
        )
    }))
    .await;
    assert!(!texts.contains("stray"), "{texts}");
    drop(deck);
    // Stopping a station waits for it: not on the runtime's own threads.
    drop(tokio::task::spawn_blocking(move || drop((a, b))).await);
}

/// How long the answers to four things asked together take, right after an
/// answer: the middle one of ten goes, in milliseconds.
async fn four_together(s: &Station) -> f64 {
    let mut deck = Deck::open(s, "bench-1").await;
    let mut took = vec![];
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        deck.call("fleet.view", json!({})).await.unwrap();
        let began = std::time::Instant::now();
        let first = deck.id + 1;
        for (method, params) in [
            ("pane.list", json!({})),
            ("project.list", json!({})),
            ("task.list", json!({"active": true, "limit": 100})),
            ("msg.recent", json!({"limit": 60})),
        ] {
            deck.id += 1;
            deck.say(json!({"id": deck.id, "method": method, "params": params}))
                .await;
        }
        let mut waiting: Vec<u64> = (first..=deck.id).collect();
        while !waiting.is_empty() {
            let v = deck.text().await;
            waiting.retain(|id| v["id"] != json!(id));
        }
        took.push(began.elapsed().as_secs_f64() * 1000.0);
    }
    took.sort_by(f64::total_cmp);
    (took[4] + took[5]) / 2.0
}

/// The plan's target for step 5, and the fault it was measured from: on
/// 0.4.4 the answers to four requests a deck sent together took 42 ms,
/// because the station's system held the second answer back until the
/// browser's had acknowledged the first, which it does after 40 ms when it
/// has nothing to send (`nagle.py` in the audit: 41.8 ms before, 0.7 ms
/// after). Done when: with the switch as it comes, they take under 10 ms;
/// and the switch is real: off, the old wait is back (on Linux, where the
/// 40 ms was measured; other systems time their acknowledgements otherwise).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "nightly: a timing, of ten goes at two stations"]
async fn four_answers_together_are_not_held_back() {
    let (on, off) = tokio::task::spawn_blocking(|| {
        (
            Station::start("alpha"),
            Station::start_with("bravo", "[flags]\nnodelay = false\n", &[]),
        )
    })
    .await
    .unwrap();
    let (now, before) = (four_together(&on).await, four_together(&off).await);
    println!("four answers together: {now:.2} ms; with nodelay = false: {before:.2} ms");
    assert!(
        now < 10.0,
        "{now:.2} ms (with the switch off: {before:.2} ms)"
    );
    if cfg!(target_os = "linux") {
        assert!(before > 20.0, "the switch changed nothing: {before:.2} ms");
    }
    drop(tokio::task::spawn_blocking(move || drop((on, off))).await);
}
