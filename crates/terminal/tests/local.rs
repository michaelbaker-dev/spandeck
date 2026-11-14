//! The local socket is one interface on every system (a Unix socket, a
//! Windows named pipe): the daemon's listener, and the blocking client the
//! CLI and hooks use. Reason (DECISIONS.md, "Windows"): server, client and
//! tests share it, so it is tested once, here, on each system CI runs.

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use spandeck_terminal::sys::local;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn place(dir: &std::path::Path, tag: &str) -> std::path::PathBuf {
    local::pipe_name(&format!("test-{}-{tag}", std::process::id()))
        .unwrap_or_else(|| dir.join(format!("{tag}.sock")))
}

#[tokio::test(flavor = "multi_thread")]
async fn this_user_connects_and_lines_go_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    let path = place(dir.path(), "echo");
    let mut listener = local::Listener::bind(&path).unwrap();
    tokio::spawn(async move {
        // Two clients, one after the other: the listener keeps listening.
        for _ in 0..2 {
            let stream = listener.accept().await.unwrap();
            let (read, mut write) = tokio::io::split(stream);
            let mut lines = tokio::io::BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                write
                    .write_all(format!("echo:{line}\n").as_bytes())
                    .await
                    .unwrap();
            }
        }
    });
    for n in 0..2 {
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = local::connect(&path).unwrap();
            let mut writer = conn.try_clone().unwrap();
            let mut reader = BufReader::new(conn);
            writer.write_all(format!("hello {n}\n").as_bytes()).unwrap();
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, format!("echo:hello {n}\n"));
        })
        .await
        .unwrap();
    }
}

/// A hook must not wait for ever on a daemon that does not answer.
#[tokio::test(flavor = "multi_thread")]
async fn a_read_gives_up_after_its_time_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = place(dir.path(), "quiet");
    let mut listener = local::Listener::bind(&path).unwrap();
    let held = tokio::spawn(async move {
        let stream = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(stream);
    });
    tokio::task::spawn_blocking(move || {
        let conn = local::connect(&path).unwrap();
        conn.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut line = String::new();
        let start = std::time::Instant::now();
        assert!(BufReader::new(conn).read_line(&mut line).is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
    })
    .await
    .unwrap();
    held.abort();
}

#[test]
fn nothing_listening_is_an_error_not_a_wait() {
    let dir = tempfile::tempdir().unwrap();
    assert!(local::connect(&place(dir.path(), "nobody")).is_err());
}
