//! Phase 0 spike, kept as a release acceptance test: a running terminal
//! survives the process replacing its own image (exec-in-place upgrade).
//!
//!   handoff-probe run <statefile>     spawn a ticking child, then exec `adopt`
//!   handoff-probe adopt <statefile>   adopt the PTY, verify nothing was lost

use std::time::{Duration, Instant};

use spandeck_terminal::{ExitInfo, Pane, PaneHandoff, Signal, SpawnSpec};

#[derive(serde::Serialize, serde::Deserialize)]
struct State {
    daemon_pid: u32,
    pane: PaneHandoff,
}

fn ticks(pane: &Pane) -> Vec<u64> {
    pane.full_text(5000)
        .iter()
        .filter_map(|l| l.strip_prefix("tick-").and_then(|n| n.trim().parse().ok()))
        .collect()
}

fn wait_for(pane: &Pane, what: &str, pred: impl Fn(&[u64]) -> bool) -> Vec<u64> {
    let start = Instant::now();
    loop {
        let t = ticks(pane);
        if pred(&t) {
            return t;
        }
        if start.elapsed() > Duration::from_secs(10) {
            eprintln!("timeout waiting for {what}; ticks so far: {t:?}");
            std::process::exit(2);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn main() {
    if !spandeck_terminal::sys::LOCAL_HANDOFF {
        eprintln!("this system has no live handoff");
        std::process::exit(64);
    }
    let args: Vec<String> = std::env::args().collect();
    let (mode, statefile) = (args[1].as_str(), args[2].as_str());
    match mode {
        "run" => {
            let spec = SpawnSpec {
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "i=0; while :; do echo tick-$i; i=$((i+1)); sleep 0.03; done".into(),
                ],
                cwd: None,
                env: vec![],
                env_remove: vec![],
                cols: 40,
                rows: 10,
            };
            let pane = Pane::spawn(&spec, Box::new(|_| {})).expect("spawn");
            wait_for(&pane, "first ticks", |t| t.len() >= 15);
            let handoff = pane.prepare_handoff(5000).expect("prepare");
            let state = State {
                daemon_pid: std::process::id(),
                pane: handoff,
            };
            std::fs::write(statefile, serde_json::to_vec(&state).unwrap()).unwrap();
            // Let output pile up unread in the kernel while "the upgrade happens".
            std::thread::sleep(Duration::from_millis(300));
            let err = spandeck_terminal::sys::replace_process(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["adopt", statefile]),
            );
            eprintln!("exec failed: {err}");
            std::process::exit(3);
        }
        "adopt" => {
            let state: State = serde_json::from_slice(&std::fs::read(statefile).unwrap()).unwrap();
            assert_eq!(
                state.daemon_pid,
                std::process::id(),
                "exec-in-place keeps the pid"
            );
            // SAFETY: the fd was inherited across exec for exactly this purpose.
            let pane = unsafe { Pane::adopt(&state.pane, Box::new(|_| {})) }.expect("adopt");
            let before = ticks(&pane);
            let last_before = *before.last().expect("snapshot carried the screen");
            let after = wait_for(&pane, "ticks after adopt", |t| {
                t.last().is_some_and(|l| *l >= last_before + 20)
            });
            // Nothing lost or duplicated across the handoff.
            for w in after.windows(2) {
                assert_eq!(w[1], w[0] + 1, "gap or duplicate in {after:?}");
            }
            // Still the parent: we can signal the child and collect its status.
            pane.signal(Signal::Terminate).unwrap();
            let start = Instant::now();
            let exit = loop {
                if let Some(e) = pane.exit_info() {
                    break e;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "child did not exit"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(
                exit,
                ExitInfo::Signal(15),
                "exit status collected by the new image"
            );
            println!(
                "HANDOFF_OK ticks_before={} ticks_after={} exit={exit:?}",
                before.len(),
                after.len()
            );
        }
        _ => std::process::exit(64),
    }
}
