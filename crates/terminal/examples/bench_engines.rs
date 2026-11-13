//! Phase 0 spike: 50 panes streaming at once, each with server-side terminal
//! state. Compares alacritty_terminal (via spandeck's Screen) with vt100.
//!
//!   cargo run --release -p spandeck-terminal --example bench_engines

use std::thread;
use std::time::Instant;

use spandeck_terminal::Screen;

/// Output shaped like an agent session: coloured lines, a spinner that redraws
/// in place, cursor moves, a full-screen repaint now and then, some unicode.
fn agent_stream(seed: u64, target_bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(target_bytes + 4096);
    let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let words = [
        "fn",
        "let",
        "match",
        "error:",
        "warning:",
        "Compiling",
        "tests",
        "passed",
        "src/main.rs",
        "→",
        "✓",
        "日本語",
    ];
    let mut i = 0u64;
    while out.len() < target_bytes {
        i += 1;
        match next() % 20 {
            0 => {
                // spinner redraw
                for f in ["⠋", "⠙", "⠹", "⠸"] {
                    out.extend_from_slice(
                        format!("\r\x1b[2K\x1b[36m{f}\x1b[0m Working… ({i}s · esc to interrupt)")
                            .as_bytes(),
                    );
                }
                out.extend_from_slice(b"\r\n");
            }
            1 => {
                // box repaint with cursor addressing
                out.extend_from_slice(b"\x1b[s");
                for r in 1..6 {
                    out.extend_from_slice(format!("\x1b[{r};1H\x1b[48;5;236m\x1b[38;2;200;180;90m│ status row {r} #{i} \x1b[0m\x1b[K").as_bytes());
                }
                out.extend_from_slice(b"\x1b[u");
            }
            _ => {
                let colour = 31 + (next() % 6);
                out.extend_from_slice(format!("\x1b[{colour}m").as_bytes());
                for _ in 0..(4 + next() % 12) {
                    out.extend_from_slice(words[(next() % words.len() as u64) as usize].as_bytes());
                    out.push(b' ');
                }
                out.extend_from_slice(b"\x1b[0m\r\n");
            }
        }
    }
    out
}

fn run(
    name: &str,
    panes: usize,
    mb_per_pane: usize,
    feed: impl Fn(&[u8]) + Send + Sync + Copy + 'static,
) {
    let streams: Vec<Vec<u8>> = (0..panes)
        .map(|i| agent_stream(i as u64 + 1, mb_per_pane << 20))
        .collect();
    let total: usize = streams.iter().map(|s| s.len()).sum();
    let start = Instant::now();
    let handles: Vec<_> = streams
        .into_iter()
        .map(|s| thread::spawn(move || feed(&s)))
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{name:22} {panes} panes x {mb_per_pane} MB: {:.2}s wall, {:.0} MB/s total, {:.1} MB/s per pane",
        secs,
        total as f64 / secs / 1e6,
        total as f64 / secs / 1e6 / panes as f64
    );
}

fn main() {
    let panes = 50;
    let mb = 8;
    println!(
        "cores: {}",
        thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0)
    );
    run("alacritty_terminal", panes, mb, |s| {
        let mut screen = Screen::new(120, 40);
        for chunk in s.chunks(16 * 1024) {
            screen.feed(chunk);
        }
        std::hint::black_box(screen.text());
    });
    run("vt100", panes, mb, |s| {
        let mut p = vt100::Parser::new(
            std::num::NonZero::new(40).unwrap(),
            std::num::NonZero::new(120).unwrap(),
            5000,
        );
        for chunk in s.chunks(16 * 1024) {
            p.process(chunk);
        }
        std::hint::black_box(p.screen().contents());
    });
    // Cost of a late-joiner snapshot and of a 1 Hz text thumbnail.
    let mut screen = Screen::new(120, 40);
    screen.feed(&agent_stream(7, 4 << 20));
    let t = Instant::now();
    let mut n = 0;
    for _ in 0..1000 {
        n += screen.snapshot_ansi(5000).len();
    }
    println!(
        "snapshot with 5000 lines of history: {:.2} ms each, {} KB",
        t.elapsed().as_secs_f64(),
        n / 1000 / 1024
    );
    let t = Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(screen.text());
    }
    println!(
        "visible-grid text (thumbnail):       {:.3} ms each",
        t.elapsed().as_secs_f64()
    );
}
