//! Unix: a pane is a PTY. `tests/conpty.rs` is the same for Windows.
#![cfg(unix)]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use spandeck_terminal::{ExitInfo, Pane, PaneEvent, SpawnSpec};

fn sh(script: &str) -> SpawnSpec {
    SpawnSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: None,
        env: vec![],
        env_remove: vec![],
        cols: 80,
        rows: 24,
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn spawn_write_and_exit_code() {
    let (tx, rx) = mpsc::channel();
    let pane = Pane::spawn(
        &sh("read x; echo got:$x; exit 7"),
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
    )
    .unwrap();
    pane.write(b"hello\n").unwrap();
    wait_until("output", || pane.text().contains("got:hello"));
    wait_until("exit", || pane.exit_info().is_some());
    assert_eq!(pane.exit_info(), Some(ExitInfo::Code(7)));
    let events: Vec<PaneEvent> = rx.try_iter().collect();
    assert!(matches!(
        events.last(),
        Some(PaneEvent::Exited(ExitInfo::Code(7)))
    ));
}

#[test]
fn resize_reaches_the_child() {
    let pane = Pane::spawn(&sh("sleep 0.3; stty size; sleep 5"), Box::new(|_| {})).unwrap();
    pane.resize(100, 30).unwrap();
    wait_until("stty size", || pane.text().contains("30 100"));
    assert_eq!((pane.info().cols, pane.info().rows), (100, 30));
}

#[test]
fn env_and_cwd_are_applied() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = sh("echo $SPANDECK_T; pwd; sleep 5");
    spec.env.push(("SPANDECK_T".into(), "tok123".into()));
    spec.cwd = Some(dir.path().to_path_buf());
    let pane = Pane::spawn(&spec, Box::new(|_| {})).unwrap();
    let name = dir
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    wait_until("env+cwd", || {
        let t = pane.text();
        t.contains("tok123") && t.contains(&name)
    });
}

/// Backpressure: an output flood with nobody consuming must not stall the PTY
/// or grow memory without bound (the sink is called and returns immediately).
#[test]
fn output_flood_does_not_stall() {
    let pane = Pane::spawn(
        &sh("head -c 30000000 /dev/zero | tr '\\0' 'x'; echo; echo FLOOD_DONE; sleep 5"),
        Box::new(|_| {}),
    )
    .unwrap();
    wait_until("flood drained", || pane.text().contains("FLOOD_DONE"));
    assert!(pane.bytes_out() >= 30_000_000);
}
