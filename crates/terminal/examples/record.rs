//! Record a real program's terminal byte stream for the replay tests.
//!
//!   cargo run -p spandeck-terminal --example record -- OUT.bin COLS ROWS 'keys-script' -- program args…
//!
//! keys-script: comma-separated steps, `w500` waits 500 ms, anything else is
//! typed (\n = Enter, \e = Escape). Example: 'w800,ihello\e,w200,:q!\n,w300'

use std::sync::{Arc, Mutex};
use std::time::Duration;

use spandeck_terminal::{Pane, PaneEvent, SpawnSpec};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let split = args
        .iter()
        .position(|a| a == "--")
        .expect("-- before the program");
    let (out, cols, rows, keys) = (
        &args[0],
        args[1].parse().unwrap(),
        args[2].parse().unwrap(),
        &args[3],
    );
    let cmd = &args[split + 1..];
    let bytes = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink_bytes = bytes.clone();
    let spec = SpawnSpec {
        program: cmd[0].clone(),
        args: cmd[1..].to_vec(),
        cwd: std::env::current_dir().ok(),
        env: vec![
            ("LANG".into(), "C.UTF-8".into()),
            ("PS1".into(), "$ ".into()),
        ],
        env_remove: vec![],
        cols,
        rows,
    };
    let pane = Pane::spawn(
        &spec,
        Box::new(move |ev| {
            if let PaneEvent::Output(b) = ev {
                sink_bytes.lock().unwrap().extend_from_slice(&b);
            }
        }),
    )
    .expect("spawn");
    for step in keys.split(',').filter(|s| !s.is_empty()) {
        if let Some(ms) = step.strip_prefix('w').and_then(|n| n.parse::<u64>().ok()) {
            std::thread::sleep(Duration::from_millis(ms));
        } else {
            let typed = step.replace("\\n", "\r").replace("\\e", "\x1b");
            pane.write(typed.as_bytes()).unwrap();
        }
    }
    let data = bytes.lock().unwrap().clone();
    std::fs::write(out, &data).unwrap();
    eprintln!("{} bytes -> {out}", data.len());
    println!("{}", pane.text());
}
