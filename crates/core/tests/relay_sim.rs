//! Risk: stations lose each other. The message relay runs over simulated TCP
//! (Turmoil) with drops, partitions, lost replies and a receiver that crashes.
//!
//! Guarantees checked: at-least-once delivery with duplicates dropped by id
//! (a reconnect cannot start the same task twice), ordering within a thread,
//! and no remote task without a valid authorization.
//!
//! Since unit 08a of the performance plan (15 Nov 2026) each destination has
//! a sender of its own (`Relay`): the cases below run with it, and the last
//! ones hold it to the owner's rule, "the slow or absent station has to be
//! fixed with the right architecture for a system like this, never to slow
//! everyone else down": a station that hangs does not delay another
//! station's message, and nothing depends on a sender being told.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use spandeck_core::db::Db;
use spandeck_core::relay::{
    BoxFuture, Envelope, Link, MessageStore, NewMessage, Relay, RelayAck, TaskAuth, Transport,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use turmoil::net::{TcpListener, TcpStream};

/// The station link over simulated TCP: one JSON line each way.
#[derive(Clone)]
struct SimLink;

impl Link for SimLink {}

/// Until nothing waits at `store`, with the safety net run every `every`
/// (`None`: never, so only senders that were told deliver).
async fn drained(store: &MessageStore, relay: &Relay, every: Option<Duration>) {
    while store.queued() > 0 {
        if every.is_some() {
            relay.pump(&SimLink);
        }
        tokio::time::sleep(every.unwrap_or(Duration::from_millis(20))).await;
    }
}

impl Transport for SimLink {
    fn deliver<'a>(
        &'a self,
        station: &'a str,
        env: &'a Envelope,
    ) -> BoxFuture<'a, Result<RelayAck, String>> {
        Box::pin(async move {
            let attempt = async {
                let stream = TcpStream::connect((station, 7000))
                    .await
                    .map_err(|e| e.to_string())?;
                let (read, mut write) = stream.into_split();
                write
                    .write_all(format!("{}\n", serde_json::to_string(env).unwrap()).as_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
                let mut line = String::new();
                BufReader::new(read)
                    .read_line(&mut line)
                    .await
                    .map_err(|e| e.to_string())?;
                serde_json::from_str::<RelayAck>(&line).map_err(|e| format!("no reply: {e}"))
            };
            tokio::time::timeout(Duration::from_secs(2), attempt)
                .await
                .map_err(|_| "timed out".to_string())?
        })
    }
}

type Accepted = Arc<Mutex<Vec<(String, bool)>>>;

async fn receiver(
    store: MessageStore,
    accepted: Accepted,
    start: tokio::time::Instant,
) -> turmoil::Result {
    let listener = TcpListener::bind("0.0.0.0:7000").await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let (store, accepted) = (store.clone(), accepted.clone());
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            if BufReader::new(read).read_line(&mut line).await.is_err() {
                return;
            }
            let Ok(env) = serde_json::from_str::<Envelope>(&line) else {
                return;
            };
            let ack = store.accept(&env, start.elapsed().as_millis() as u64 + 1);
            if ack.received {
                accepted
                    .lock()
                    .unwrap()
                    .push((env.id.clone(), ack.duplicate));
            }
            // The reply itself may be lost; the sender then retries.
            let _ = write
                .write_all(format!("{}\n", serde_json::to_string(&ack).unwrap()).as_bytes())
                .await;
        });
    }
}

fn task(to: &str, body: &str, thread: Option<&str>, auth_until: Option<u64>) -> NewMessage {
    NewMessage {
        sender: "author".into(),
        recipient: to.into(),
        kind: "task".into(),
        body: body.into(),
        refs: json!([]),
        thread: thread.map(String::from),
        task: None,
        project: Some("demo".into()),
        auth: auth_until.map(|expires_ms| TaskAuth {
            approved_by: "you".into(),
            expires_ms,
        }),
    }
}

struct World {
    alpha: MessageStore,
    bravo: MessageStore,
    accepted: Accepted,
}

fn world() -> World {
    World {
        alpha: MessageStore::new(Arc::new(Db::memory().unwrap()), "alpha"),
        bravo: MessageStore::new(Arc::new(Db::memory().unwrap()), "bravo"),
        accepted: Arc::new(Mutex::new(vec![])),
    }
}

fn sim(seed: u64, fail_rate: f64) -> turmoil::Sim<'static> {
    turmoil::Builder::new()
        // Simulated time, not real. A sender backs off to 30 s between tries,
        // so an unlucky run of drops needs more than ten minutes for six
        // tasks (seed 114 did, in the nightly run of 15 Nov 2026).
        .simulation_duration(Duration::from_secs(1800))
        .tick_duration(Duration::from_millis(5))
        .rng_seed(seed)
        .fail_rate(fail_rate)
        .repair_rate(0.3)
        .build()
}

/// Done when: under simulated drops and reconnects a task is delivered exactly once.
#[test]
fn a_task_is_delivered_exactly_once_under_drops_and_reconnects() {
    // Nightly raises the seed count; the every-change tier keeps it short.
    let seeds: u64 = std::env::var("SPANDECK_SIM_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);
    for seed in 0..seeds {
        let w = world();
        let mut sim = sim(seed, 0.25);
        let (bravo, accepted) = (w.bravo.clone(), w.accepted.clone());
        let t0 = tokio::time::Instant::now();
        sim.host("bravo", move || {
            receiver(bravo.clone(), accepted.clone(), t0)
        });
        let alpha = w.alpha.clone();
        sim.client("alpha", async move {
            let start = tokio::time::Instant::now();
            let now = || start.elapsed().as_millis() as u64 + 1;
            // Every other seed with the switch off: no sender is told of
            // a message, and the half-second safety net alone delivers.
            let relay = Relay::new(alpha.clone());
            relay.set_push(seed % 2 == 0);
            let mut ids = vec![];
            for i in 0..6 {
                ids.push(
                    alpha
                        .send(
                            task(
                                "reviewer@bravo",
                                &format!("task {i}"),
                                Some("thread-1"),
                                Some(u64::MAX),
                            ),
                            now(),
                        )
                        .unwrap()
                        .id,
                );
                relay.push(&SimLink, "bravo");
            }
            drained(&alpha, &relay, Some(Duration::from_millis(500))).await;
            Ok(())
        });
        sim.run().unwrap_or_else(|e| panic!("seed {seed}: {e}"));

        let inbox = w.bravo.inbox(&["reviewer".to_string()], false, false, 1);
        let bodies: Vec<&str> = inbox.iter().map(|m| m["body"].as_str().unwrap()).collect();
        assert_eq!(
            bodies,
            (0..6).map(|i| format!("task {i}")).collect::<Vec<_>>(),
            "seed {seed}: each task once, in thread order"
        );
        let accepted = w.accepted.lock().unwrap();
        let first_time: Vec<&String> = accepted
            .iter()
            .filter(|(_, dup)| !dup)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            first_time.len(),
            6,
            "seed {seed}: no task was started twice"
        );
        // Every message the sender holds is marked delivered.
        assert!(
            w.alpha
                .recent(None, 100)
                .iter()
                .all(|m| m["status"] == json!("delivered")),
            "seed {seed}"
        );
    }
}

/// The reply is lost after the receiver stored the message: the retry must
/// not start the task a second time.
#[test]
fn a_lost_acknowledgment_does_not_start_the_task_twice() {
    let w = world();
    let mut sim = sim(7, 0.0);
    let (bravo, accepted) = (w.bravo.clone(), w.accepted.clone());
    let t0 = tokio::time::Instant::now();
    sim.host("bravo", move || {
        receiver(bravo.clone(), accepted.clone(), t0)
    });
    let alpha = w.alpha.clone();
    sim.client("alpha", async move {
        alpha
            .send(
                task("reviewer@bravo", "run the review", None, Some(u64::MAX)),
                1,
            )
            .unwrap();
        // Requests get through; replies do not.
        turmoil::partition_oneway("bravo", "alpha");
        let relay = Relay::new(alpha.clone());
        let mut tries = relay.tries();
        relay.push(&SimLink, "bravo");
        tries.changed().await.unwrap();
        assert_eq!(alpha.queued(), 1, "no acknowledgment came back: it stays");
        turmoil::repair_oneway("bravo", "alpha");
        // The sender tries again by itself when its wait is over: nobody
        // tells it, and the safety net is not run. The wait is counted on
        // a clock that only goes forward, never the wall clock. (A message
        // once waited 223 s after a clock was corrected: audit, section 10.)
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(alpha.queued(), 0);
        Ok(())
    });
    sim.run().unwrap();
    let accepted = w.accepted.lock().unwrap();
    assert_eq!(accepted.len(), 2, "the receiver saw it twice");
    assert_eq!(
        accepted.iter().filter(|(_, dup)| !dup).count(),
        1,
        "and stored it once"
    );
    assert_eq!(
        w.bravo
            .inbox(&["reviewer".to_string()], false, false, 1)
            .len(),
        1
    );
}

/// The receiving station restarts mid-stream: nothing is lost or repeated.
#[test]
fn the_receiver_can_crash_and_come_back() {
    let w = world();
    let mut sim = sim(11, 0.0);
    let (bravo, accepted) = (w.bravo.clone(), w.accepted.clone());
    let t0 = tokio::time::Instant::now();
    sim.host("bravo", move || {
        receiver(bravo.clone(), accepted.clone(), t0)
    });
    let alpha = w.alpha.clone();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    sim.client("alpha", async move {
        let start = tokio::time::Instant::now();
        let now = || start.elapsed().as_millis() as u64 + 1;
        for i in 0..4 {
            alpha
                .send(
                    task(
                        "reviewer@bravo",
                        &format!("task {i}"),
                        Some("t"),
                        Some(u64::MAX),
                    ),
                    now(),
                )
                .unwrap();
        }
        let relay = Relay::new(alpha.clone());
        let mut tries = relay.tries();
        relay.push(&SimLink, "bravo");
        tries.changed().await.unwrap(); // task 0 arrives
        let _ = tx.send(());
        drained(&alpha, &relay, Some(Duration::from_millis(200))).await;
        Ok(())
    });
    let mut crashed = false;
    let mut steps = 0u64;
    while !sim.step().unwrap() {
        steps += 1;
        if !crashed && rx.try_recv().is_ok() {
            sim.crash("bravo");
            crashed = true;
            steps = 0;
        }
        if crashed && steps == 600 {
            sim.bounce("bravo");
        }
    }
    let bodies: Vec<String> = w
        .bravo
        .inbox(&["reviewer".to_string()], false, false, 1)
        .iter()
        .map(|m| m["body"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(bodies, vec!["task 0", "task 1", "task 2", "task 3"]);
}

/// No new remote assignment is accepted without a valid task authorization.
#[test]
fn a_remote_task_needs_an_authorization_that_has_not_expired() {
    let w = world();
    let mut sim = sim(3, 0.0);
    let (bravo, accepted) = (w.bravo.clone(), w.accepted.clone());
    let t0 = tokio::time::Instant::now();
    sim.host("bravo", move || {
        receiver(bravo.clone(), accepted.clone(), t0)
    });
    let alpha = w.alpha.clone();
    sim.client("alpha", async move {
        let none = alpha
            .send(task("reviewer@bravo", "no authorization", None, None), 1)
            .unwrap();
        let expired = alpha
            .send(
                task("reviewer@bravo", "expired authorization", None, Some(1)),
                1,
            )
            .unwrap();
        let mut note = task("reviewer@bravo", "a note needs none", None, None);
        note.kind = "note".into();
        let note = alpha.send(note, 1).unwrap();
        let relay = Relay::new(alpha.clone());
        relay.push(&SimLink, "bravo");
        // A refusal is final, not retried for ever, and what is behind a
        // refused message goes on: told once, with no safety net.
        drained(&alpha, &relay, None).await;
        // Each refusal is told to its sender, once, in its own inbox and
        // in the thread of the message that was refused.
        let told = alpha.inbox(&["author".to_string()], false, false, 1);
        let about = |id: &str| -> Vec<&serde_json::Value> {
            told.iter().filter(|m| m["thread"] == json!(id)).collect()
        };
        assert_eq!(told.len(), 2, "{told:?}");
        assert_eq!(about(&none.id).len(), 1);
        assert_eq!(about(&expired.id).len(), 1);
        let words = about(&expired.id)[0]["body"].as_str().unwrap();
        assert!(
            words.starts_with("Not delivered to reviewer@bravo: its task authorization expired"),
            "{words}"
        );
        assert_eq!(about(&expired.id)[0]["from"], json!("spandeck@alpha"));
        assert!(alpha
            .recent(None, 10)
            .iter()
            .any(|m| m["id"] == json!(none.id)
                && m["status"].as_str().unwrap().starts_with("refused")));
        assert!(alpha
            .recent(None, 10)
            .iter()
            .any(|m| m["id"] == json!(expired.id)
                && m["status"].as_str().unwrap().contains("expired")));
        assert!(alpha
            .recent(None, 10)
            .iter()
            .any(|m| m["id"] == json!(note.id) && m["status"] == json!("delivered")));
        Ok(())
    });
    sim.run().unwrap();
    let inbox = w.bravo.inbox(&["reviewer".to_string()], false, false, 1);
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0]["body"], json!("a note needs none"));
}

// ---------------------------------------------------------------------------
// One sender per destination (unit 08a). Measured before it: messages were
// tried one station after another, once every half second, so a burst of ten
// arrived at 0.5, 1.0 … 5.0 s, and a station that hung held the next
// station's message up to five seconds at each try; a task whose
// authorization ran out while it waited was refused with nothing said to
// its sender.

/// A link with a fixed, short delay, so that times can be compared.
fn quick_sim() -> turmoil::Sim<'static> {
    turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(1800))
        .tick_duration(Duration::from_millis(1))
        .min_message_latency(Duration::from_millis(5))
        .max_message_latency(Duration::from_millis(5))
        .rng_seed(1)
        .build()
}

fn note(to: &str, body: &str, thread: &str) -> NewMessage {
    let mut m = task(to, body, Some(thread), None);
    m.kind = "note".into();
    m
}

fn bodies(store: &MessageStore, name: &str) -> Vec<String> {
    store
        .inbox(&[name.to_string()], false, false, 1)
        .iter()
        .map(|m| m["body"].as_str().unwrap().to_string())
        .collect()
}

/// The owner's rule: a station that hangs delays only its own messages.
/// With the senders told of each message, and with the switch off (the
/// safety net alone): in neither is one station tried after another.
#[test]
fn a_station_that_hangs_does_not_delay_another_stations_message() {
    for push in [true, false] {
        let w = world();
        let charlie = MessageStore::new(Arc::new(Db::memory().unwrap()), "charlie");
        let at_charlie: Accepted = Arc::new(Mutex::new(vec![]));
        let mut sim = quick_sim();
        let t0 = tokio::time::Instant::now();
        let (bravo, accepted) = (w.bravo.clone(), w.accepted.clone());
        sim.host("bravo", move || {
            receiver(bravo.clone(), accepted.clone(), t0)
        });
        let (c, seen) = (charlie.clone(), at_charlie.clone());
        sim.host("charlie", move || receiver(c.clone(), seen.clone(), t0));
        let (alpha, seen) = (w.alpha.clone(), at_charlie.clone());
        sim.client("alpha", async move {
            let relay = Relay::new(alpha.clone());
            relay.set_push(push);
            // bravo hangs: what is sent to it goes nowhere and nothing
            // comes back. Its messages are the older ones.
            turmoil::hold("alpha", "bravo");
            let began = tokio::time::Instant::now();
            for i in 0..3 {
                alpha
                    .send(note("reviewer@bravo", &format!("b{i}"), "tb"), 1)
                    .unwrap();
                relay.push(&SimLink, "bravo");
            }
            for i in 0..3 {
                alpha
                    .send(note("reviewer@charlie", &format!("c{i}"), "tc"), 1)
                    .unwrap();
                relay.push(&SimLink, "charlie");
            }
            while alpha.head("charlie").is_some() {
                relay.pump(&SimLink);
                tokio::time::sleep(Duration::from_millis(if push { 5 } else { 500 })).await;
            }
            // The link gives a try two seconds: charlie's three are there
            // before bravo's first try has even been given up. (One
            // station after another, they would begin after it.)
            let took = began.elapsed();
            let limit = if push { 200 } else { 1_700 };
            assert!(
                took < Duration::from_millis(limit),
                "push {push}: charlie's messages took {took:?}"
            );
            assert_eq!(seen.lock().unwrap().len(), 3);
            assert_eq!(alpha.queued(), 3, "bravo's still wait");
            // bravo answers again: its own, in order.
            turmoil::release("alpha", "bravo");
            relay.back("bravo");
            drained(&alpha, &relay, Some(Duration::from_millis(500))).await;
            Ok(())
        });
        sim.run().unwrap();
        assert_eq!(bodies(&charlie, "reviewer"), ["c0", "c1", "c2"]);
        assert_eq!(bodies(&w.bravo, "reviewer"), ["b0", "b1", "b2"]);
    }
}

/// A burst goes at once and in the order it was sent, whatever the thread;
/// a link cut in the middle of it loses nothing, repeats nothing and keeps
/// the order. The safety net is never run here: being told is enough.
#[test]
fn a_burst_drains_at_once_and_in_order_through_a_cut() {
    let w = world();
    let mut sim = quick_sim();
    let t0 = tokio::time::Instant::now();
    let (bravo, accepted) = (w.bravo.clone(), w.accepted.clone());
    sim.host("bravo", move || {
        receiver(bravo.clone(), accepted.clone(), t0)
    });
    let (alpha, seen) = (w.alpha.clone(), w.accepted.clone());
    sim.client("alpha", async move {
        let relay = Relay::new(alpha.clone());
        let send = |range: std::ops::Range<u32>| {
            let mut ids = vec![];
            for i in range {
                let thread = format!("t{}", i % 3);
                let env = alpha
                    .send(note("reviewer@bravo", &format!("n{i}"), &thread), 1)
                    .unwrap();
                relay.push(&SimLink, "bravo");
                ids.push(env.id);
            }
            ids
        };
        let began = tokio::time::Instant::now();
        let mut ids = send(0..12);
        drained(&alpha, &relay, None).await;
        // Twelve round trips of about 20 ms: before, one each half second.
        let took = began.elapsed();
        assert!(took < Duration::from_millis(600), "the burst took {took:?}");
        // A second burst, and the link is cut when part of it is there.
        ids.extend(send(12..24));
        while seen.lock().unwrap().len() < 16 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        turmoil::partition("alpha", "bravo");
        tokio::time::sleep(Duration::from_secs(20)).await;
        turmoil::repair("alpha", "bravo");
        drained(&alpha, &relay, None).await;
        let seen = seen.lock().unwrap();
        let stored: Vec<&String> = seen.iter().filter(|(_, dup)| !dup).map(|s| &s.0).collect();
        assert_eq!(
            stored,
            ids.iter().collect::<Vec<_>>(),
            "each once, in the order sent"
        );
        Ok(())
    });
    sim.run().unwrap();
    let got = bodies(&w.bravo, "reviewer");
    assert_eq!(got.len(), 24, "{got:?}");
}

/// Nothing depends on a sender being told, or on a sender staying alive:
/// a message nobody was told of is found by the safety net, and a sender
/// that ends in the middle of its queue is started again by it.
#[test]
fn the_safety_net_delivers_what_no_sender_was_told_of_and_restarts_a_sender_that_ended() {
    let w = world();
    let mut sim = quick_sim();
    let t0 = tokio::time::Instant::now();
    let (bravo, accepted) = (w.bravo.clone(), w.accepted.clone());
    sim.host("bravo", move || {
        receiver(bravo.clone(), accepted.clone(), t0)
    });
    let alpha = w.alpha.clone();
    sim.client("alpha", async move {
        let relay = Relay::new(alpha.clone());
        // Stored, and nobody told (as after a restart, or a lost wake).
        alpha.send(note("reviewer@bravo", "n0", "t"), 1).unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(alpha.queued(), 1, "nothing sends without being started");
        relay.pump(&SimLink);
        drained(&alpha, &relay, None).await;
        // A sender ends where it stands, in the middle of a try, with two
        // more behind it.
        turmoil::hold("alpha", "bravo");
        for i in 1..4 {
            alpha
                .send(note("reviewer@bravo", &format!("n{i}"), "t"), 1)
                .unwrap();
            relay.push(&SimLink, "bravo");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(relay.kill("bravo"));
        turmoil::release("alpha", "bravo");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(alpha.queued(), 3, "its queue waits: nothing sends");
        drained(&alpha, &relay, Some(Duration::from_millis(500))).await;
        assert_eq!(relay.view()["restarted"], json!(1));
        Ok(())
    });
    sim.run().unwrap();
    assert_eq!(bodies(&w.bravo, "reviewer"), ["n0", "n1", "n2", "n3"]);
    let accepted = w.accepted.lock().unwrap();
    assert_eq!(accepted.iter().filter(|(_, dup)| !dup).count(), 4);
}

/// A task that waits longer than its authorization lasts is refused when
/// it arrives ("these expire"), once, and its sender is told once, in its
/// own inbox; it is not sent again by itself, and what waits behind it
/// goes on. A task that did arrive, at a try whose answer was lost, is not
/// reported as undelivered when the try after it comes too late.
#[test]
fn a_task_held_past_its_authorization_is_refused_once_and_its_sender_is_told() {
    let w = world();
    let mut sim = quick_sim();
    let t0 = tokio::time::Instant::now();
    let (bravo, accepted) = (w.bravo.clone(), w.accepted.clone());
    sim.host("bravo", move || {
        receiver(bravo.clone(), accepted.clone(), t0)
    });
    let alpha = w.alpha.clone();
    sim.client("alpha", async move {
        let relay = Relay::new(alpha.clone());
        // The receiver's clock is the simulation's, in milliseconds.
        let until = t0.elapsed().as_millis() as u64 + 60_000;
        // Arrives, and its answer is lost.
        turmoil::partition_oneway("bravo", "alpha");
        let arrived = alpha
            .send(task("reviewer@bravo", "arrived", None, Some(until)), 1)
            .unwrap();
        let mut tries = relay.tries();
        relay.push(&SimLink, "bravo");
        tries.changed().await.unwrap();
        // Never arrives in time, and a note behind it.
        turmoil::partition("alpha", "bravo");
        let late = alpha
            .send(task("reviewer@bravo", "too late", None, Some(until)), 1)
            .unwrap();
        alpha
            .send(note("reviewer@bravo", "behind it", "other"), 1)
            .unwrap();
        relay.push(&SimLink, "bravo");
        tokio::time::sleep(Duration::from_secs(90)).await;
        turmoil::repair("alpha", "bravo");
        relay.back("bravo");
        drained(&alpha, &relay, Some(Duration::from_millis(500))).await;
        for _ in 0..4 {
            relay.pump(&SimLink);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let status = |id: &str| {
            alpha
                .recent(None, 100)
                .iter()
                .find(|m| m["id"] == json!(id))
                .and_then(|m| m["status"].as_str().map(String::from))
        };
        assert_eq!(status(&arrived.id).as_deref(), Some("delivered"));
        assert_eq!(
            status(&late.id).as_deref(),
            Some("refused: the task authorization has expired")
        );
        let told = alpha.inbox(&["author".to_string()], false, false, 1);
        assert_eq!(told.len(), 1, "told once, of the one refused: {told:?}");
        assert_eq!(told[0]["thread"], json!(late.thread));
        assert_eq!(told[0]["kind"], json!("note"));
        Ok(())
    });
    sim.run().unwrap();
    assert_eq!(bodies(&w.bravo, "reviewer"), ["arrived", "behind it"]);
    let accepted = w.accepted.lock().unwrap();
    assert_eq!(
        accepted.iter().filter(|(_, dup)| !dup).count(),
        2,
        "the refused task was never stored there: {accepted:?}"
    );
}

/// Received and acted-on are separate acknowledgments.
#[test]
fn received_read_and_acted_on_are_separate() {
    let store = MessageStore::new(Arc::new(Db::memory().unwrap()), "alpha");
    let env = store
        .send(task("reviewer", "local task", None, None), 10)
        .unwrap();
    let names = ["reviewer".to_string()];
    let peek = store.inbox(&names, true, false, 20);
    assert_eq!(
        (
            peek[0]["status"].as_str(),
            peek[0]["read"].is_null(),
            peek[0]["acted"].is_null()
        ),
        (Some("delivered"), true, true)
    );
    store.inbox(&names, true, true, 30);
    let after_read = &store.thread(&env.thread)[0];
    assert_eq!(
        (
            after_read["status"].as_str(),
            after_read["read"].as_i64(),
            after_read["acted"].is_null()
        ),
        (Some("read"), Some(30), true)
    );
    assert!(store.ack(&env.id, 40));
    let after_ack = &store.thread(&env.thread)[0];
    assert_eq!(
        (after_ack["status"].as_str(), after_ack["acted"].as_i64()),
        (Some("answered"), Some(40))
    );
    assert!(
        store.inbox(&names, true, false, 50).is_empty(),
        "nothing unread is left"
    );
}

// ---------------------------------------------------------------------------
// A `send` tried again (rids.rs). The audit: an agent's tool call that got no
// answer was a failure to the agent, and its second `send` a second message.
// The second review ("Reply store"): the request id must be written in the
// message's own transaction, or a crash between the two stores it twice.

mod tried_again {
    use super::*;
    use spandeck_core::relay::{sent_answer, SendMark};
    use spandeck_core::rids::{hash, new_rid, Begin, Kept, Mark, Retry, Rids};

    type Plan = Arc<Mutex<Vec<&'static str>>>;

    /// A station's daemon as far as `msg.send` goes, one request per
    /// connection. Each start is a new process: nothing in memory, the
    /// database as the last one left it. `plan` says where the next
    /// request it carries out is cut short: the test crashes the host when
    /// it is told that point was reached.
    async fn daemon(
        db: Arc<Db>,
        plan: Plan,
        reached: std::sync::mpsc::Sender<()>,
    ) -> turmoil::Result {
        let store = MessageStore::new(db.clone(), "alpha");
        let rids = Rids::new(db);
        rids.set_on(true);
        let listener = TcpListener::bind("0.0.0.0:7001").await?;
        loop {
            let (stream, _) = listener.accept().await?;
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            if BufReader::new(read).read_line(&mut line).await.is_err() {
                continue;
            }
            let Ok(req) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let params = json!({"to": "reviewer", "body": req["body"]});
            let h = hash(&params);
            let retry = Retry::parse(req.get("rid"), req.get("again_ms"))
                .unwrap()
                .unwrap();
            let stop_here = |at: &str| {
                let mut plan = plan.lock().unwrap();
                let hit = plan.first() == Some(&at);
                if hit {
                    plan.remove(0);
                    let _ = reached.send(());
                }
                hit
            };
            let answer = match rids.begin("pane:p-1", "msg.send", &h, &retry, Mark::InTx, 1) {
                Begin::Run(run) => {
                    if stop_here("before the message is stored") {
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                    }
                    let mark = SendMark {
                        key: run.key.clone(),
                        method: "msg.send".into(),
                        hash: h.clone(),
                    };
                    let mut note = task("reviewer", req["body"].as_str().unwrap(), None, None);
                    note.kind = "note".into();
                    let sent = store
                        .send_marked(note, 1, Some(&mark))
                        .map(|env| sent_answer(&env, false));
                    run.finish(sent.clone().map_err(|e| ("invalid".to_string(), e)), 1);
                    if stop_here("after it is stored, before the answer") {
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                    }
                    json!({"result": sent.unwrap()})
                }
                Begin::Done(Kept::Answer(Ok(v))) => json!({"result": v}),
                Begin::Unknown(_) => json!({"error": "outcome_unknown"}),
                _ => json!({"error": "other"}),
            };
            let _ = write.write_all(format!("{answer}\n").as_bytes()).await;
        }
    }

    /// The tool: one id per call, sent with every try until an answer
    /// comes. A try that was written says so on the next (`again_ms`).
    async fn send_tool(body: &str) -> serde_json::Value {
        let rid = new_rid();
        let mut written = false;
        loop {
            let attempt = async {
                let stream = TcpStream::connect(("station", 7001)).await.ok()?;
                let (read, mut write) = stream.into_split();
                let mut req = json!({"rid": rid, "body": body});
                if written {
                    req["again_ms"] = json!(500);
                }
                write.write_all(format!("{req}\n").as_bytes()).await.ok()?;
                written = true;
                let mut line = String::new();
                BufReader::new(read).read_line(&mut line).await.ok()?;
                serde_json::from_str::<serde_json::Value>(&line).ok()
            };
            match tokio::time::timeout(Duration::from_secs(2), attempt).await {
                Ok(Some(answer)) => return answer,
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }

    #[test]
    fn a_send_tried_again_across_a_crash_is_stored_once_or_said_to_be_unknown() {
        let db = Arc::new(Db::memory().unwrap());
        let plan: Plan = Arc::new(Mutex::new(vec![
            "after it is stored, before the answer",
            "before the message is stored",
        ]));
        let (reached, crash_now) = std::sync::mpsc::channel::<()>();
        let mut sim = sim(5, 0.0);
        {
            let (db, plan) = (db.clone(), plan.clone());
            sim.host("station", move || {
                daemon(db.clone(), plan.clone(), reached.clone())
            });
        }
        let inbox = {
            let store = MessageStore::new(db.clone(), "alpha");
            move || store.inbox(&["reviewer".to_string()], false, false, 1)
        };
        let seen = inbox.clone();
        sim.client("agent", async move {
            // The daemon dies after the message is stored and before the
            // answer is written. The second try gets the first answer.
            let one = send_tool("one").await;
            let stored = seen();
            assert_eq!(stored.len(), 1, "stored once: {stored:?}");
            assert_eq!(one["result"]["id"], stored[0]["id"], "{one}");
            // It dies before the message is stored. The daemon that starts
            // again has no record and has kept ids for no time: it cannot
            // tell, says so, and stores nothing.
            let two = send_tool("two").await;
            assert_eq!(two["error"], json!("outcome_unknown"), "{two}");
            assert_eq!(seen().len(), 1, "never carried out blind");
            // The agent, having checked, sends it as a new call.
            let again = send_tool("two").await;
            assert!(again["result"]["id"].is_string(), "{again}");
            Ok(())
        });
        let mut down: Option<u64> = None;
        while !sim.step().unwrap() {
            if crash_now.try_recv().is_ok() {
                sim.crash("station");
                down = Some(0);
            }
            if let Some(steps) = down.as_mut() {
                *steps += 1;
                if *steps == 300 {
                    sim.bounce("station");
                    down = None;
                }
            }
        }
        let bodies: Vec<String> = inbox()
            .iter()
            .map(|m| m["body"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(bodies, vec!["one", "two"]);
    }
}
