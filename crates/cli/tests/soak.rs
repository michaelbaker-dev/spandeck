//! Nightly: 50 panes streaming at once through a real daemon, with a slow
//! client attached. The PTYs must not stall and the daemon must stay small.
//! Run with `cargo test -p spandeck --test soak --release -- --ignored`.
mod common;
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;

fn rss_kb(pid: u32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

#[test]
#[ignore]
fn fifty_streaming_panes_do_not_stall_or_balloon() {
    let s = Station::start("soak");
    let flood = "i=0; while [ $i -lt 40000 ]; do printf '\\033[3%dmline %d of pane output with some text to parse\\033[0m\\n' $((i%7+1)) $i; i=$((i+1)); done; echo SOAK_DONE; sleep 30";
    let started = Instant::now();
    let panes: Vec<String> = (0..50)
        .map(|_| {
            s.call(
                "pane.spawn",
                json!({"program": "/bin/sh", "args": ["-c", flood], "cwd": s.root.join("home")}),
            )["id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    // One client that never reads its socket: backpressure must not reach the PTYs.
    let stalled = std::net::TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    for id in &panes {
        let start = Instant::now();
        loop {
            if s.text(id).contains("SOAK_DONE") {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(240),
                "pane {id} stalled"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let secs = started.elapsed().as_secs_f64();
    let rss = rss_kb(s.pid());
    println!(
        "50 panes x 40k lines in {secs:.1}s; daemon RSS {} MB",
        rss / 1024
    );
    if rss > 0 {
        assert!(rss < 1_500_000, "daemon memory grew to {} MB", rss / 1024);
    }
    // The daemon still answers promptly afterwards.
    let t = Instant::now();
    s.call("pane.list", json!({}));
    assert!(t.elapsed() < Duration::from_secs(2));
    drop(stalled);
}
