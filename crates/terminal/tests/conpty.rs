//! Windows acceptance (DECISIONS.md, "Windows"): a pane is a pseudoconsole
//! with its program in a job. These are `tests/pane.rs` for Windows, plus
//! what only Windows has: `.cmd` files, and ending a pane by its job.
#![cfg(windows)]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use spandeck_terminal::{ExitInfo, Pane, PaneEvent, Signal, SpawnSpec};

fn spec(program: &str, args: &[&str]) -> SpawnSpec {
    SpawnSpec {
        program: program.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        cwd: None,
        env: vec![],
        env_remove: vec![],
        cols: 80,
        rows: 24,
    }
}

fn cmd(line: &str) -> SpawnSpec {
    spec("cmd.exe", &["/v:on", "/d", "/c", line])
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

#[test]
fn spawn_write_and_exit_code() {
    let (tx, rx) = mpsc::channel();
    let pane = Pane::spawn(
        &cmd("set /p x=& echo got:!x!& exit 7"),
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
    )
    .unwrap();
    pane.write(b"hello\r").unwrap();
    wait_until(&pane, "output", || pane.text().contains("got:hello"));
    wait_until(&pane, "exit", || pane.exit_info().is_some());
    assert_eq!(pane.exit_info(), Some(ExitInfo::Code(7)));
    let events: Vec<PaneEvent> = rx.try_iter().collect();
    assert!(matches!(
        events.last(),
        Some(PaneEvent::Exited(ExitInfo::Code(7)))
    ));
}

#[test]
fn resize_reaches_the_child() {
    let pane = Pane::spawn(
        &spec(
            "powershell.exe",
            &[
                "-NoLogo",
                "-NoProfile",
                "-Command",
                "Start-Sleep -Milliseconds 1500; Write-Output ('size=' + [Console]::WindowWidth + 'x' + [Console]::WindowHeight); Start-Sleep 10",
            ],
        ),
        Box::new(|_| {}),
    )
    .unwrap();
    pane.resize(100, 30).unwrap();
    wait_until(&pane, "console size", || {
        pane.text().contains("size=100x30")
    });
    assert_eq!((pane.info().cols, pane.info().rows), (100, 30));
}

#[test]
fn env_and_cwd_are_applied() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = cmd("echo !SPANDECK_T!& cd& ping -n 10 127.0.0.1 >nul");
    s.env.push(("SPANDECK_T".into(), "tok123".into()));
    s.cwd = Some(dir.path().to_path_buf());
    let pane = Pane::spawn(&s, Box::new(|_| {})).unwrap();
    let name = dir
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    wait_until(&pane, "env+cwd", || {
        let t = pane.text();
        t.contains("tok123") && t.contains(&name)
    });
}

/// npm installs agents as `.cmd` files. One must start by its bare name, and
/// each argument must reach the script whole and still quoted, so that
/// `cmd.exe` reads `&` in it as text. (The script prints `%1`, as npm's own
/// launchers pass `%*` on. A script that strips the quotes itself with
/// `%~1` and then uses the result unquoted runs what is in it: that is the
/// script's doing, and no way of calling it can prevent it.)
#[test]
fn a_cmd_file_is_found_by_name_and_gets_its_arguments_whole() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("fakeagent.cmd"),
        "@echo off\r\necho one=[%1] two=[%2] three=[%3]\r\nping -n 10 127.0.0.1 >nul\r\n",
    )
    .unwrap();
    let mut s = spec("fakeagent", &["a&echo INJECTED", "c d", "100%PATH%"]);
    let path = std::env::var("PATH").unwrap_or_default();
    s.env
        .push(("PATH".into(), format!("{};{path}", dir.path().display())));
    let pane = Pane::spawn(&s, Box::new(|_| {})).unwrap();
    wait_until(&pane, "arguments", || {
        pane.text()
            .contains(r#"one=["a&echo INJECTED"] two=["c d"] three=["100%PATH%"]"#)
    });
    assert!(!pane.text().lines().any(|l| l.trim() == "INJECTED"));
}

/// Kill: the process ends, and so does everything it started.
#[test]
fn kill_ends_the_whole_tree() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("alive.txt");
    // A second cmd, started by the pane's, that keeps writing a file for as
    // long as it lives.
    std::fs::write(
        dir.path().join("inner.cmd"),
        "@echo off\r\n:again\r\necho x>alive.txt\r\nping -n 2 127.0.0.1 >nul\r\ngoto again\r\n",
    )
    .unwrap();
    let mut s = cmd("start /b inner.cmd& ping -n 60 127.0.0.1 >nul");
    s.cwd = Some(dir.path().to_path_buf());
    let pane = Pane::spawn(&s, Box::new(|_| {})).unwrap();
    wait_until(&pane, "the grandchild is running", || marker.exists());
    pane.signal(Signal::Kill).unwrap();
    wait_until(&pane, "exit after kill", || pane.exit_info().is_some());
    std::thread::sleep(Duration::from_millis(500));
    let _ = std::fs::remove_file(&marker);
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        !marker.exists(),
        "a program the pane started outlived the pane's job"
    );
}

/// Hang-up: the pseudoconsole closes and the program on it ends.
#[test]
fn hangup_ends_the_program() {
    let pane = Pane::spawn(&cmd("ping -n 120 127.0.0.1 >nul"), Box::new(|_| {})).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    pane.signal(Signal::Hangup).unwrap();
    wait_until(&pane, "exit after hang-up", || pane.exit_info().is_some());
}

/// Backpressure: an output flood with nobody consuming must not stall.
#[test]
fn output_flood_does_not_stall() {
    let pane = Pane::spawn(
        &cmd("for /l %i in (1,1,20000) do @echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx %i& echo FLOOD_DONE& ping -n 10 127.0.0.1 >nul"),
        Box::new(|_| {}),
    )
    .unwrap();
    let start = Instant::now();
    while !pane.text().contains("FLOOD_DONE") {
        assert!(start.elapsed() < Duration::from_secs(120), "flood stalled");
        std::thread::sleep(Duration::from_millis(50));
    }
    // A pseudoconsole sends what the screen needs, not every byte written,
    // so there is no byte count to hold it to: the flood ending is the test.
    assert!(pane.bytes_out() > 0);
}
