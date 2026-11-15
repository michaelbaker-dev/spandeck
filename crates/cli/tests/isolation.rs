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
    bravo.signal(19);
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
    bravo.signal(18);

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
