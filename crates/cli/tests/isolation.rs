//! Nightly: a station that is frozen and one behind a slow link slow down
//! nobody else. Real stations, real time, so it measures and prints.
//! Run with `cargo test -p spandeck --test isolation -- --ignored --nocapture`.
//!
//! The owner's rule (15 Nov 2026): "the slow or absent station has to be
//! fixed with the right architecture for a system like this, never to slow
//! everyone else down." Measured before (performance report, 15 Nov 2026):
//! with one station that did not answer and one message queued for it, an
//! agent on the same station as its sender was told of its mail after 1.68 s
//! at the median and 8.4 s at worst.
mod common;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

// The numbers differ between Linux and macOS.
const SIGSTOP: i32 = if cfg!(target_os = "macos") { 17 } else { 19 };
const SIGCONT: i32 = if cfg!(target_os = "macos") { 19 } else { 18 };

/// A TCP relay that holds everything it passes for `delay` milliseconds,
/// each way: a station behind a slow link.
fn slow_link(to: u16, delay: Arc<AtomicU64>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for from in listener.incoming().flatten() {
            let Ok(onward) = TcpStream::connect(("127.0.0.1", to)) else {
                continue;
            };
            let pairs = [
                (from.try_clone().unwrap(), onward.try_clone().unwrap()),
                (onward, from),
            ];
            for (mut src, mut dst) in pairs {
                let delay = delay.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 16 * 1024];
                    while let Ok(n) = src.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(delay.load(Ordering::SeqCst)));
                        if dst.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    let _ = dst.shutdown(std::net::Shutdown::Both);
                });
            }
        }
    });
    port
}

struct Timings {
    /// Mail stored at alpha, to its idle agent there being typed the nudge.
    wake: Vec<f64>,
    /// The owner's change to the preferences at alpha: the call itself.
    change: Vec<f64>,
    /// And from the change being asked for to delta, which is healthy, holding it.
    sync: Vec<f64>,
}

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn worst(v: &[f64]) -> f64 {
    v.iter().copied().fold(0.0, f64::max)
}

fn measure(tag: &str, alpha: &Station, delta: &Station, worker: &str, times: usize) -> Timings {
    let mut t = Timings {
        wake: vec![],
        change: vec![],
        sync: vec![],
    };
    for i in 0..times {
        // The agent has been idle for longer than the 1.2 s the nudge
        // waits for, so what is timed is the station's own delay.
        alpha.wait_state(worker, "done");
        std::thread::sleep(Duration::from_millis(1500));
        let before = alpha.log("msg.nudged").len();
        let began = Instant::now();
        alpha.call(
            "msg.send",
            json!({"to": "worker", "body": format!("{tag} {i}")}),
        );
        wait_for("the agent to be told of its mail", 30, || {
            (alpha.log("msg.nudged").len() > before).then_some(())
        });
        t.wake.push(began.elapsed().as_secs_f64());

        let value = json!([format!("{tag}-{i}")]);
        let began = Instant::now();
        alpha.call("prefs.set", json!({"key": "side.folded", "value": value}));
        t.change.push(began.elapsed().as_secs_f64());
        wait_for("the healthy station to hold the change", 30, || {
            (delta.call("prefs.get", json!({}))["values"]["side.folded"] == value).then_some(())
        });
        t.sync.push(began.elapsed().as_secs_f64());
    }
    println!(
        "{tag:>28}: mail wake-up median {:.2} s, worst {:.2} s | change median {:.3} s, worst {:.3} s | at the healthy station median {:.3} s, worst {:.3} s",
        median(&t.wake), worst(&t.wake), median(&t.change), worst(&t.change), median(&t.sync), worst(&t.sync),
    );
    t
}

#[test]
#[ignore = "nightly: four stations for two minutes, and it asserts on measured times"]
fn a_frozen_station_and_a_slow_one_delay_nobody_else() {
    let ports: Vec<u16> = (0..4).map(|_| free_port()).collect();
    let delay = Arc::new(AtomicU64::new(0));
    // alpha reaches charlie through the slow link; everyone else is direct.
    let slow = slow_link(ports[2], delay.clone());
    let key = "5a".repeat(32);
    let names = ["alpha", "bravo", "charlie", "delta"];
    let stations: Vec<Station> = (0..4)
        .map(|i| {
            let others: Vec<String> = (0..4)
                .filter(|j| *j != i)
                .map(|j| {
                    let port = if (i, j) == (0, 2) { slow } else { ports[j] };
                    format!("\"127.0.0.1:{port}\"")
                })
                .collect();
            Station::start_keyed(
                names[i],
                ports[i],
                &format!("peers = [{}]\n", others.join(", ")),
                Some(&key),
            )
        })
        .collect();
    let (alpha, bravo, delta) = (&stations[0], &stations[1], &stations[3]);
    wait_for("alpha to see the other three", 30, || {
        let v = alpha.call("fleet.refresh", json!({}));
        let on = |x: &&Value| x["state"] == json!("online");
        (v["stations"].as_array().unwrap().iter().filter(on).count() == 4).then_some(())
    });
    let worker = alpha.fake(
        json!({"steps": [{"prompt": "hello"}, {"turn_end": "done"}], "on_input": [{"turn_end": "done"}]}),
        json!({"name": "worker"}),
    );
    let times = 10;
    let healthy = measure("all four healthy", alpha, delta, &worker, times);

    // bravo stops answering (its name sorts first: whatever went through
    // the stations in order met it first), charlie's link takes a quarter
    // of a second each way, and a message waits for bravo.
    bravo.signal(SIGSTOP);
    bravo.wait_frozen();
    delay.store(250, Ordering::SeqCst);
    alpha.call(
        "msg.send",
        json!({"to": "nobody@bravo", "body": "waits for bravo"}),
    );
    let degraded = measure("one frozen, one slow", alpha, delta, &worker, times);

    let perf = alpha.call("perf.get", json!({}));
    let late = &perf["periodic"]["late"];
    println!(
        "alpha's local periodic work, last 5 min: late p50 {} ms, p95 {} ms, worst {} ms; rounds: {}",
        late["p50_ms"],
        late["p95_ms"],
        late["max_ms"],
        perf["links"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| format!(
                "{} {} round p50 {} ms, worst {} ms, silent {}",
                l["station"],
                l["state"],
                l["round"]["took"]["p50_ms"],
                l["round"]["took"]["max_ms"],
                l["round"]["silent"]["since_start"]
            ))
            .collect::<Vec<_>>()
            .join("; ")
    );
    bravo.signal(SIGCONT);

    // The local wake-up is the 1 s poll whatever the others do: the brief's
    // bound (the poll plus the idle rule, generous), and no worse than
    // healthy by more than a poll's jitter.
    assert!(worst(&degraded.wake) < 2.5, "{:?}", degraded.wake);
    assert!(
        median(&degraded.wake) < median(&healthy.wake) + 0.4,
        "{:?} against {:?}",
        degraded.wake,
        healthy.wake
    );
    // The owner's change is answered, and reaches the healthy station, as
    // when all are healthy: neither waits for the frozen station's five
    // seconds or the slow one's half second.
    assert!(worst(&degraded.change) < 0.4, "{:?}", degraded.change);
    assert!(
        worst(&degraded.sync) < worst(&healthy.sync) + 0.4,
        "{:?} against {:?}",
        degraded.sync,
        healthy.sync
    );
    // The Performance page's figure for local work.
    assert!(late["p95_ms"].as_f64().unwrap() < 50.0, "{late}");
    // And the page says which station is the slow one.
    let silent = |station: &str| {
        perf["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["station"] == json!(station))
            .map(|l| l["round"]["silent"]["since_start"].as_u64().unwrap())
    };
    assert!(silent("bravo") > Some(0), "{perf}");
    assert_eq!(silent("delta"), Some(0), "{perf}");
}

/// Unit 08a of the performance plan: a message for another station goes at
/// once, by that station's own sender. Measured before (15 Nov 2026):
/// sent to received 330 ms at the median with no delay on the link, one
/// message per station each half second (a burst of ten arrived at 0.5,
/// 1.0 … 5.0 s), and with a station frozen a message for a healthy one took
/// up to 5.2 s. The brief's bounds, with `r` the link's round trip: sent to
/// received within r + 20 ms at the 95th percentile, and a hundred queued
/// messages there within 100 × (r + 5 ms); here with a station frozen and
/// messages waiting for it, which may cost the healthy one nothing.
#[test]
#[ignore = "nightly: three stations, and it asserts on measured times"]
fn a_message_goes_at_once_and_a_frozen_station_delays_only_its_own() {
    let ports: Vec<u16> = (0..3).map(|_| free_port()).collect();
    // alpha reaches charlie through a link that takes 10 ms each way.
    let delay = Arc::new(AtomicU64::new(10));
    let slow = slow_link(ports[2], delay.clone());
    let key = "5b".repeat(32);
    let names = ["alpha", "bravo", "charlie"];
    let stations: Vec<Station> = (0..3)
        .map(|i| {
            let others: Vec<String> = (0..3)
                .filter(|j| *j != i)
                .map(|j| {
                    let port = if (i, j) == (0, 2) { slow } else { ports[j] };
                    format!("\"127.0.0.1:{port}\"")
                })
                .collect();
            Station::start_keyed(
                names[i],
                ports[i],
                &format!("peers = [{}]\n", others.join(", ")),
                Some(&key),
            )
        })
        .collect();
    let (alpha, bravo, charlie) = (&stations[0], &stations[1], &stations[2]);
    wait_for("alpha to see the other two", 30, || {
        let v = alpha.call("fleet.refresh", json!({}));
        let on = |x: &&Value| x["state"] == json!("online");
        (v["stations"].as_array().unwrap().iter().filter(on).count() == 3).then_some(())
    });
    // The link's round trip, as a call over it takes.
    let mut trips: Vec<f64> = (0..15)
        .map(|_| {
            let began = Instant::now();
            alpha.call(
                "peer.call",
                json!({"station": "charlie", "method": "station.info", "params": {}}),
            );
            began.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    trips.sort_by(|a, b| a.total_cmp(b));
    let r = trips[trips.len() / 2];
    let waiting_for = |station: &str| {
        alpha.call("perf.get", json!({}))["messages"]["waiting"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["station"] == json!(station))
            .map_or(0, |w| w["count"].as_u64().unwrap())
    };
    let received = |s: &Station| -> Vec<String> {
        s.call("log.query", json!({"kind": "msg.received", "limit": 500}))
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["data"]["id"].as_str().unwrap().to_string())
            .collect()
    };
    let send = |to: &str, body: &str| -> String {
        alpha.call("msg.send", json!({"to": to, "body": body, "kind": "note"}))["id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // bravo stops answering, with messages waiting for it.
    bravo.signal(SIGSTOP);
    bravo.wait_frozen();
    for i in 0..3 {
        send("nobody@bravo", &format!("waits for bravo {i}"));
    }
    std::thread::sleep(Duration::from_millis(1200));

    // One at a time: the station's own figure, from sent here to stored
    // there.
    for i in 0..40 {
        send("nobody@charlie", &format!("one {i}"));
        std::thread::sleep(Duration::from_millis(60));
    }
    wait_for("the forty to arrive", 20, || {
        (waiting_for("charlie") == 0).then_some(())
    });
    let there = alpha.call("perf.get", json!({}))["messages"]["received"].clone();
    // A hundred at once.
    let began = Instant::now();
    let sent: Vec<String> = (0..100)
        .map(|i| send("nobody@charlie", &format!("burst {i}")))
        .collect();
    wait_for("the hundred to arrive", 60, || {
        (waiting_for("charlie") == 0).then_some(())
    });
    let burst = began.elapsed().as_secs_f64() * 1000.0;
    let at_charlie = received(charlie);
    println!(
        "round trip {r:.1} ms | sent to received (40, one at a time, bravo frozen): p50 {} ms, p95 {} ms, worst {} ms | 100 at once: all there after {burst:.0} ms | still waiting for bravo: {}",
        there["p50_ms"], there["p95_ms"], there["max_ms"], waiting_for("bravo"),
    );
    assert_eq!(there["count"], json!(40), "{there}");
    assert!(there["p95_ms"].as_f64().unwrap() <= r + 20.0, "{there}");
    // The loop that looks is 40 ms coarse: the brief's bound, and that.
    assert!(burst <= 100.0 * (r + 5.0) + 100.0, "{burst} ms, r {r} ms");
    assert!(
        sent.iter().all(|id| at_charlie.contains(id)),
        "every one of the hundred is there"
    );
    assert_eq!(waiting_for("bravo"), 3, "bravo's wait, for bravo alone");

    // bravo is back: its own arrive without anyone asking.
    bravo.signal(SIGCONT);
    wait_for("bravo's three to arrive", 45, || {
        (waiting_for("bravo") == 0).then_some(())
    });
    assert_eq!(received(bravo).len(), 3);
}
