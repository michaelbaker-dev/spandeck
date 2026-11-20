//! `spandeck link`: stations with no Tailscale, reached over a command of
//! the owner's own, usually their `ssh` (core: link/mod.rs). Every call
//! here is one only this machine's own command line may make.

use clap::Subcommand;
use serde_json::{json, Value};
use spandeck_core::client::Client;

#[derive(Subcommand)]
pub enum LinkCmd {
    /// Every link of this station: its port here, how it stands, the far
    /// station and its version, the round trip, and since when.
    List,
    /// Open a link again, or try it now (after `down`, or after signing
    /// in to ssh by hand).
    Up { name: String },
    /// Hold a link closed until `up`. The configuration is not changed.
    Down { name: String },
    /// Sign this machine's browser in to the far station, and print the
    /// address of its Control Deck here.
    Open { name: String },
}

/// One line, without anything that moves the cursor: names and versions
/// come from the far side, and what a carrier printed is in `said`.
fn line(v: &Value) -> String {
    v.as_str()
        .unwrap_or("")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

fn ago(now: &Value, then: &Value) -> String {
    let (Some(now), Some(then)) = (now.as_u64(), then.as_u64()) else {
        return String::new();
    };
    let s = now.saturating_sub(then) / 1000;
    let clock = spandeck_core::restore::clock_utc(then);
    match s {
        0..=119 => format!("{clock} ({s} s ago)"),
        120..=7199 => format!("{clock} ({} min ago)", s / 60),
        7200..=172_799 => format!("{clock} ({} h ago)", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
}

fn row(l: &Value) -> String {
    let or_dash = |v: &Value| match line(v) {
        s if s.is_empty() => "-".to_string(),
        s => s,
    };
    let mut out = format!(
        "{:<16} {:<6} {:<14} {:<16} {:<9} {:<10} {}",
        line(&l["name"]),
        l["port"]
            .as_u64()
            .filter(|p| *p != 0)
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into()),
        line(&l["state"]),
        or_dash(&l["station"]),
        or_dash(&l["version"]),
        l["rtt_ms"]
            .as_u64()
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_else(|| "-".into()),
        ago(&l["now_ms"], &l["since_ms"]),
    );
    for more in [&l["said"], &l["port_trouble"]] {
        let more = line(more);
        if !more.is_empty() {
            out.push_str(&format!("\n    {more}"));
        }
    }
    out
}

fn list(v: &Value) -> String {
    let rows = v.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "This station has no links. Add a [[link]] to its config.toml (name, cmd) and restart it.".into();
    }
    let mut out = vec![format!(
        "{:<16} {:<6} {:<14} {:<16} {:<9} {:<10} {}",
        "NAME", "PORT", "STATE", "STATION", "VERSION", "ROUND TRIP", "SINCE"
    )];
    out.extend(rows.iter().map(row));
    out.join("\n")
}

pub fn run(c: &mut Client, json_out: bool, cmd: Option<LinkCmd>) -> anyhow::Result<i32> {
    let show = |v: &Value, text: String| {
        if json_out {
            println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
        } else {
            println!("{text}");
        }
    };
    match cmd {
        None | Some(LinkCmd::List) => {
            let v = c.call("link.list", json!({}))?;
            show(&v, list(&v));
        }
        Some(LinkCmd::Up { name }) => {
            let v = c.call("link.up", json!({"name": name}))?;
            show(
                &v,
                format!("{name}: trying now. `spandeck link list` says how it stands."),
            );
        }
        Some(LinkCmd::Down { name }) => {
            let v = c.call("link.down", json!({"name": name}))?;
            show(
                &v,
                format!("{name}: held closed until `spandeck link up {name}`."),
            );
        }
        Some(LinkCmd::Open { name }) => {
            let v = c.call("link.open", json!({"name": name}))?;
            let station = match line(&v["station"]) {
                s if s.is_empty() => name.clone(),
                s => s,
            };
            show(
                &v,
                format!(
                    "{}\n\nOpen it in a browser on this machine within two minutes. It signs that browser in to {station}, once: the address is of no use afterwards.\n{station} lists the browser under Access{}, where it can be signed out.",
                    line(&v["url"]),
                    if v["again"] == json!(true) { " (the same entry as before)" } else { "" },
                ),
            );
        }
    }
    Ok(0)
}
