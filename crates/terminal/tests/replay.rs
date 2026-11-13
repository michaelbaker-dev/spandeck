//! Risk: the terminal draws wrongly. Recorded byte streams from real programs
//! (including a real Claude Code session) are replayed and checked against a
//! saved screen. Record more with `examples/record.rs`.

use std::fs;
use std::path::Path;

use spandeck_terminal::Screen;

fn recordings() -> Vec<(String, u16, u16, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("recordings");
    let mut out: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().into_string().unwrap();
            let stem = name.strip_suffix(".bin")?;
            let (label, size) = stem.rsplit_once('.')?;
            let (cols, rows) = size.split_once('x')?;
            Some((
                label.to_string(),
                cols.parse().ok()?,
                rows.parse().ok()?,
                fs::read(dir.join(&name)).unwrap(),
            ))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(out.len() >= 12, "recordings are missing");
    out
}

fn replay(bytes: &[u8], cols: u16, rows: u16, chunk: usize) -> Screen {
    let mut s = Screen::new(cols, rows);
    for c in bytes.chunks(chunk) {
        s.feed(c);
    }
    s
}

#[test]
fn recordings_match_their_saved_screens() {
    for (name, cols, rows, bytes) in recordings() {
        let screen = replay(&bytes, cols, rows, 4096);
        let info = screen.info();
        let rendered = format!(
            "{}\n--- cursor {},{} visible={} alt={} title={:?}",
            screen.text(),
            info.cursor_row,
            info.cursor_col,
            info.cursor_visible,
            info.alt_screen,
            info.title
        );
        insta::assert_snapshot!(name, rendered);
    }
}

/// However the bytes are split across reads, the screen is the same.
#[test]
fn chunking_does_not_change_the_result() {
    for (name, cols, rows, bytes) in recordings() {
        let whole = replay(&bytes, cols, rows, usize::MAX);
        for chunk in [1, 7, 64] {
            let split = replay(&bytes, cols, rows, chunk);
            assert_eq!(
                whole.full_text(500),
                split.full_text(500),
                "{name} differs at chunk size {chunk}"
            );
            assert_eq!(
                whole.info(),
                split.info(),
                "{name} differs at chunk size {chunk}"
            );
        }
    }
}

/// A late joiner (or the next daemon image) that replays the snapshot of a
/// real session sees exactly what the server sees.
#[test]
fn a_snapshot_of_every_recording_reproduces_it() {
    for (name, cols, rows, bytes) in recordings() {
        // Join at many points, including the middle of escape sequences.
        let step = (bytes.len() / 60).max(1);
        for cut in (0..=bytes.len()).step_by(step).chain([bytes.len()]) {
            let mut server = replay(&bytes[..cut], cols, rows, 4096);
            let snap = server.snapshot_ansi(1000);
            let mut client = Screen::new(cols, rows);
            client.feed(&snap);
            assert_eq!(
                server.full_text(1000),
                client.full_text(1000),
                "{name} cut at {cut}: text differs"
            );
            assert_eq!(
                server.info(),
                client.info(),
                "{name} cut at {cut}: cursor or modes differ"
            );
            // Then the live stream continues identically on both.
            let rest = &bytes[cut..];
            server.feed(rest);
            client.feed(rest);
            assert_eq!(
                server.text(),
                client.text(),
                "{name} cut at {cut}: diverged after the snapshot"
            );
        }
    }
}
