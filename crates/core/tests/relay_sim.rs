//! Risk: stations lose each other. The message relay runs over simulated TCP
//! (Turmoil) with drops, partitions, lost replies and a receiver that crashes.
//!
//! Guarantees checked: at-least-once delivery with duplicates dropped by id
//! (a reconnect cannot start the same task twice), ordering within a thread,
//! and no remote task without a valid authorization.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use spandeck_core::db::Db;
use spandeck_core::relay::{
    BoxFuture, Envelope, MessageStore, NewMessage, RelayAck, TaskAuth, Transport,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use turmoil::net::{TcpListener, TcpStream};

/// The station link over simulated TCP: one JSON line each way.
struct SimLink;

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
            }
            while alpha.queued() > 0 {
                alpha.pump(&SimLink, now()).await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
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
        assert_eq!(
            alpha.pump(&SimLink, 2).await,
            0,
            "no acknowledgment came back"
        );
        assert_eq!(alpha.queued(), 1, "so the message stays queued");
        turmoil::repair_oneway("bravo", "alpha");
        tokio::time::sleep(Duration::from_secs(40)).await;
        // The wall clock was meanwhile set back (given here as 1, before
        // the failed try): the next try is counted on a clock that only
        // goes forward, so it is still made. (A message once waited 223 s
        // after a clock was corrected: audit, section 10.)
        assert_eq!(alpha.pump(&SimLink, 1).await, 1);
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
        alpha.pump(&SimLink, now()).await; // task 0 arrives
        let _ = tx.send(());
        while alpha.queued() > 0 {
            alpha.pump(&SimLink, now()).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
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
        for _ in 0..6 {
            alpha.pump(&SimLink, 10_000).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(alpha.queued(), 0, "a refusal is final, not retried forever");
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
