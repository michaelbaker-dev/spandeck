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

/// An agent's report from a run of seven agents on one station (18 Nov
/// 2026): its `inbox`, `ack` and `reply` "often took two minutes or
/// more". The station was not the cause, and this is the measurement that
/// says so: seven agents take turns without a pause (a prompt, a tool
/// call, a message to the next agent, the turn's end: hooks, state
/// changes, mail and nudges all the time) while an eighth pane's calls
/// are timed, as its tools make them and as the command line does. (The
/// cause was in `spandeck mcp`, which answered one call at a time: see
/// `the_mcp_server_gives_an_agent_the_spandeck_tools`.) Measured on the
/// machine it was written on, with other test runs going: see the numbers
/// this prints. The bound is far above them and far below a tool's limit.
#[test]
#[ignore = "merge tier: a measurement under load, about ten seconds"]
fn an_agents_calls_are_answered_at_once_while_seven_agents_work() {
    let s = Station::start("alpha");
    let names: Vec<String> = (0..7).map(|n| format!("w{n}")).collect();
    let workers: Vec<String> = names
        .iter()
        .enumerate()
        .map(|(n, name)| {
            let next = &names[(n + 1) % names.len()];
            let turn = json!([
                {"prompt": "work"},
                {"tool": {"name": "Bash", "command": "true"}},
                {"rpc": {"method": "msg.send", "params": {"to": next, "body": "over to you"}}},
                {"rpc": {"method": "msg.inbox", "params": {}}},
                {"turn_end": "done"},
                {"sleep": 20},
            ]);
            s.fake(
                json!({"steps": [{"repeat": 400, "steps": turn}], "on_input": [{"turn_end": "ok"}]}),
                json!({"name": name}),
            )
        })
        .collect();
    let caller = s.fake(
        json!({"steps": [], "on_input": [{"turn_end": "ok"}]}),
        json!({"name": "caller"}),
    );
    s.wait_state(&workers[6], "working");

    let mut took: std::collections::BTreeMap<&str, Vec<Duration>> = Default::default();
    let mut time = |what: &'static str, f: &mut dyn FnMut()| {
        let began = Instant::now();
        f();
        took.entry(what).or_default().push(began.elapsed());
    };
    let token = s.pane_token(&caller);
    let began = Instant::now();
    let mut rounds = 0;
    while began.elapsed() < Duration::from_secs(8) {
        rounds += 1;
        let sent = s
            .call_as_pane(
                &workers[0],
                "msg.send",
                json!({"to": "caller", "body": "x"}),
            )
            .unwrap();
        time("inbox", &mut || {
            s.call_as_pane(&caller, "msg.inbox", json!({})).unwrap();
        });
        time("reply", &mut || {
            s.call_as_pane(&caller, "msg.reply", json!({"id": sent["id"], "body": "y"}))
                .unwrap();
        });
        time("ack", &mut || {
            s.call_as_pane(&caller, "msg.ack", json!({"id": sent["id"]}))
                .unwrap();
        });
        time("wait, for what is already so", &mut || {
            s.call_as_pane(
                &caller,
                "wait",
                json!({"pane": "caller", "states": ["idle", "done", "working"], "timeout_ms": 60_000}),
            )
            .unwrap();
        });
        if rounds % 10 == 0 {
            time("spandeck inbox (the command)", &mut || {
                let out = s
                    .command(SPANDECK)
                    .arg("inbox")
                    .env("SPANDECK_TOKEN", &token)
                    .env("SPANDECK_PANE", &caller)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{out:?}");
            });
        }
    }
    // (The log gives its newest hundred of a kind.)
    let turns = s.log("pane.state").len();
    eprintln!("{rounds} rounds in 8 s, the agents changing state throughout");
    for (what, times) in &mut took {
        times.sort();
        let (n, max) = (times.len(), times[times.len() - 1]);
        eprintln!(
            "{what}: {n} calls, median {:?}, 99th {:?}, slowest {max:?}",
            times[n / 2],
            times[n * 99 / 100]
        );
        assert!(max < Duration::from_secs(2), "{what}: {max:?}");
    }
    assert!(turns >= 100, "the agents were working: {turns}");
}
