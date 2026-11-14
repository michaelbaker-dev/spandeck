//! The pane host (DECISIONS.md, "Windows"): panes outlive the daemon. The
//! "daemon" here is a `HostClient`; dropping it is the daemon going away.
//! Runs on every system, with each system's own terminal and shell.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use spandeck_terminal::host::connect_or_start;
use spandeck_terminal::sys::{self, local};
use spandeck_terminal::{HostClient, Pane, Signal, SpawnSpec};

const HOST: &str = env!("CARGO_BIN_EXE_pane-host-probe");

fn pipe(dir: &std::path::Path, tag: &str) -> PathBuf {
    local::pipe_name(&format!("hosttest-{}-{tag}", std::process::id()))
        .unwrap_or_else(|| dir.join(format!("{tag}.sock")))
}

/// A shell that says `ready`, then echoes each line it is given as `got:<line>`.
fn echo_shell() -> SpawnSpec {
    let (program, args): (&str, Vec<&str>) = if sys::WINDOWS {
        (
            "cmd.exe",
            vec![
                "/v:on",
                "/d",
                "/q",
                "/c",
                "echo ready& for /l %i in (1,1,50) do @(set /p x=& echo got:!x!)",
            ],
        )
    } else {
        (
            "/bin/sh",
            vec!["-c", "echo ready; while read x; do echo got:$x; done"],
        )
    };
    SpawnSpec {
        program: program.into(),
        args: args.into_iter().map(String::from).collect(),
        cwd: None,
        env: vec![],
        env_remove: vec![],
        cols: 80,
        rows: 24,
    }
}

fn wait_until(pane: &Pane, what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timeout: {what}\nscreen:\n{}",
            pane.text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn enter(pane: &Pane, text: &str) {
    let end = if sys::WINDOWS { "\r" } else { "\n" };
    pane.write(format!("{text}{end}").as_bytes()).unwrap();
}

fn start(tag: &str) -> (tempfile::TempDir, PathBuf, Arc<HostClient>) {
    let dir = tempfile::tempdir().unwrap();
    let pipe = pipe(dir.path(), tag);
    let host = connect_or_start(&pipe, HOST.as_ref()).unwrap();
    (dir, pipe, host)
}

/// The upgrade: the old daemon describes each pane and goes; the new one
/// has the same screen, misses nothing written in between, and the same
/// program (same pid) is still at the other end.
#[test]
fn a_pane_outlives_its_daemon_and_the_next_one_has_its_screen() {
    let (_dir, pipe, host) = start("handoff");
    let pane = Pane::spawn_on(&host, "p-1", &echo_shell(), Box::new(|_| {})).unwrap();
    wait_until(&pane, "ready", || pane.text().contains("ready"));
    enter(&pane, "before");
    wait_until(&pane, "first echo", || pane.text().contains("got:before"));
    let pid = pane.pid();

    let handoff = pane.prepare_handoff(2000).unwrap();
    // Typed while no daemon is reading: the host keeps what comes back.
    enter(&pane, "between");
    drop(pane);
    drop(host);
    std::thread::sleep(Duration::from_millis(400));

    let host = HostClient::connect(&pipe).unwrap();
    let pane = Pane::adopt_on(&host, "p-1", Some(&handoff), Box::new(|_| {})).unwrap();
    assert_eq!(pane.pid(), pid, "the program was never restarted");
    wait_until(&pane, "output from the gap", || {
        let t = pane.text();
        t.contains("ready") && t.contains("got:before") && t.contains("got:between")
    });
    assert_eq!(
        pane.text().matches("got:before").count(),
        1,
        "nothing is shown twice"
    );
    enter(&pane, "after");
    wait_until(&pane, "live again", || pane.text().contains("got:after"));
    host.shutdown().unwrap();
}

/// The crash: nothing was handed over. The new daemon finds the pane in the
/// host's list and rebuilds its screen from the output the host kept.
#[test]
fn after_a_daemon_crash_the_screen_is_rebuilt_from_what_the_host_kept() {
    let (_dir, pipe, host) = start("crash");
    let pane = Pane::spawn_on(&host, "p-9", &echo_shell(), Box::new(|_| {})).unwrap();
    wait_until(&pane, "ready", || pane.text().contains("ready"));
    enter(&pane, "one");
    wait_until(&pane, "echo", || pane.text().contains("got:one"));
    let pid = pane.pid();
    drop(pane);
    drop(host);

    let host = HostClient::connect(&pipe).unwrap();
    let held = host.list().unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(
        (held[0].id.as_str(), held[0].pid, held[0].exit),
        ("p-9", pid, None)
    );
    let pane = Pane::adopt_on(&host, "p-9", None, Box::new(|_| {})).unwrap();
    wait_until(&pane, "the old screen", || {
        pane.text().contains("ready") && pane.text().contains("got:one")
    });
    enter(&pane, "two");
    wait_until(&pane, "live again", || pane.text().contains("got:two"));

    // Kill reaches the program through the host, and its end comes back.
    pane.signal(Signal::Kill).unwrap();
    wait_until(&pane, "exit", || pane.exit_info().is_some());
    pane.forget();
    assert!(host.list().unwrap().is_empty());
    host.shutdown().unwrap();
}

/// A host nobody comes back to does not keep panes for ever.
#[test]
fn a_host_left_alone_ends_its_panes_and_itself() {
    let dir = tempfile::tempdir().unwrap();
    let pipe = pipe(dir.path(), "alone");
    let mut cmd = std::process::Command::new(HOST);
    cmd.arg(&pipe).env("SPANDECK_HOST_LINGER", "1");
    let mut child = cmd.spawn().unwrap();
    let host = {
        let start = Instant::now();
        loop {
            match HostClient::connect(&pipe) {
                Ok(h) => break h,
                Err(e) => assert!(start.elapsed() < Duration::from_secs(10), "{e}"),
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    };
    let pane = Pane::spawn_on(&host, "p-1", &echo_shell(), Box::new(|_| {})).unwrap();
    wait_until(&pane, "ready", || pane.text().contains("ready"));
    drop(pane);
    drop(host);
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        assert!(start.elapsed() < Duration::from_secs(15), "the host stayed");
        std::thread::sleep(Duration::from_millis(50));
    }
}
