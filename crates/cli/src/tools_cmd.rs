//! `spandeck tools`: the tools the fleet's agents have, as a grid of tool
//! by station (where each is chosen, its state and version), and the
//! owner's changes: add, scope, upgrade, rollout, remove, check.
//!
//! Everything a station or a program said is printed as text.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spandeck_core::client::{Client, ClientError};

/// How long the grid waits for the registry's answers.
const CHECK_WAIT: Duration = Duration::from_secs(12);

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// One line, without anything that moves the cursor.
fn line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

fn call(
    c: &mut Client,
    here: &str,
    station: &str,
    method: &str,
    params: Value,
) -> Result<Value, ClientError> {
    if station == here {
        c.call(method, params)
    } else {
        c.call(
            "peer.call",
            json!({"station": station, "method": method, "params": params}),
        )
    }
}

/// This station's name, and every station that answers now.
fn stations(c: &mut Client) -> anyhow::Result<(String, Vec<String>)> {
    let here = text(&c.call("station.info", json!({}))?["station"]).to_string();
    let view = c.call("fleet.view", json!({}))?;
    let mut all: Vec<String> = view["stations"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|s| s["state"] == json!("online"))
                .filter_map(|s| s["station"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if !all.contains(&here) {
        all.insert(0, here.clone());
    }
    Ok((here, all))
}

/// What one station says of its tools, or why it says nothing.
fn status_of(c: &mut Client, here: &str, station: &str) -> Result<Value, String> {
    let info = call(c, here, station, "station.info", json!({})).map_err(|e| line(&e.message))?;
    let has = info["caps"]
        .as_array()
        .is_some_and(|caps| caps.iter().any(|c| c == "tools"));
    if !has {
        return Err(format!(
            "needs a newer spandeck (it runs {})",
            text(&info["version"])
        ));
    }
    call(c, here, station, "tools.status", json!({})).map_err(|e| line(&e.message))
}

/// A tool's cell for one station: its state and version.
pub fn cell(row: &Value) -> String {
    let job = &row["job"];
    match text(&row["state"]) {
        "not_chosen" => "not chosen".into(),
        "ready" if row["kind"] == json!("custom") => "ready".into(),
        "ready" => format!("ready {}", text(&row["version"])),
        "installing" => {
            let mut s = format!("installing {}", text(&row["wanted"]));
            if let Some(l) = job["last_line"].as_str() {
                s.push_str(&format!(": {}", line(l)));
            }
            s
        }
        "failed" => format!("failed: {}", line(text(&row["line"]))),
        "needs" => format!("needs: {}", line(text(&row["line"]))),
        other => other.to_string(),
    }
}

/// `spandeck tools`: the grid. With `check`, each station is first asked
/// to ask the registry (an answer under three hours old is reused; `again`
/// asks anew).
pub fn grid(c: &mut Client, json_out: bool, check: bool, again: bool) -> anyhow::Result<()> {
    let (here, stations) = stations(c)?;
    let list = c.call("tools.list", json!({}))?;
    if check {
        for st in &stations {
            let _ = call(c, &here, st, "tools.check", json!({"again": again}));
        }
    }
    let start = Instant::now();
    let mut all: Vec<(String, Result<Value, String>)>;
    loop {
        all = stations
            .iter()
            .map(|st| (st.clone(), status_of(c, &here, st)))
            .collect();
        let checking = all.iter().any(|(_, s)| {
            s.as_ref().is_ok_and(|s| {
                s["tools"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|r| r["latest"]["checking"] == json!(true)))
            })
        });
        if !check || !checking || start.elapsed() > CHECK_WAIT {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    if json_out {
        let stations: Vec<Value> = all
            .iter()
            .map(|(st, s)| match s {
                Ok(s) => s.clone(),
                Err(e) => json!({"station": st, "error": e}),
            })
            .collect();
        let v = json!({"tools": list["tools"], "updated_ms": list["updated_ms"], "updated_by": list["updated_by"], "stations": stations});
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    let tools = list["tools"].as_array().cloned().unwrap_or_default();
    if tools.is_empty() {
        println!(
            "No tools yet. `spandeck tools add playwright --fleet` adds one for every station."
        );
        return Ok(());
    }
    let mut available = 0;
    for tool in &tools {
        let name = text(&tool["name"]);
        let what = match (text(&tool["kind"]), tool["from_setup"] == json!(true)) {
            ("catalogue", _) => format!("{} {}", text(&tool["title"]), text(&tool["version"])),
            (_, true) => "custom (an MCP server of Agent setup)".into(),
            _ => "custom".into(),
        };
        let scope = if tool["fleet"] == json!(true) {
            "the whole fleet".to_string()
        } else {
            let names: Vec<&str> = tool["stations"]
                .as_array()
                .into_iter()
                .flatten()
                .map(text)
                .collect();
            names.join(", ")
        };
        let mut head = format!("{name}  {what}  for {scope}");
        if tool["allow_without_asking"] == json!(true) {
            head.push_str("  · agents use it without asking");
        }
        println!("{head}");
        for (st, status) in &all {
            let said = match status {
                Err(e) => e.clone(),
                Ok(s) => {
                    let row = s["tools"]
                        .as_array()
                        .and_then(|rows| rows.iter().find(|r| r["name"] == json!(name)));
                    match row {
                        // Its copy of the document has not arrived yet.
                        None => "not known there yet".to_string(),
                        Some(row) => {
                            let mut s = cell(row);
                            if row["latest"]["state"] == json!("available") {
                                available += 1;
                                s.push_str(&format!(
                                    "  ({} available)",
                                    text(&row["latest"]["version"])
                                ));
                            }
                            if let Some(says) = row["job"]["says"].as_str() {
                                s.push_str(&format!("  · last: {}", line(says)));
                            }
                            s
                        }
                    }
                }
            };
            println!("  {st:<16} {said}");
        }
    }
    if available > 0 {
        println!(
            "\nA newer version is published. `spandeck tools upgrade NAME [--station X]` upgrades one station; `spandeck tools rollout NAME` brings the rest."
        );
    }
    Ok(())
}

/// A change to the tools document, made at this station, and what it
/// answered: the tool as it now is.
pub fn change(c: &mut Client, json_out: bool, method: &str, params: Value) -> anyhow::Result<()> {
    let v = c.call(method, params)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    let tool = &v["tool"];
    let name = text(&tool["name"]);
    let scope = if tool["fleet"] == json!(true) {
        "the whole fleet".to_string()
    } else {
        tool["stations"]
            .as_array()
            .into_iter()
            .flatten()
            .map(text)
            .collect::<Vec<_>>()
            .join(", ")
    };
    match method {
        "tools.remove" => println!(
            "Removed {}. No station gives it to an agent started from now; what stations installed stays on their disks.",
            text(&v["removed"])
        ),
        "tools.add" => {
            let version = match (tool["version"].as_str(), text(&v["version_from"])) {
                (Some(ver), "catalogue") => format!(
                    " {ver} (the version spandeck was tried with: the registry was not asked or did not answer)"
                ),
                (Some(ver), _) => format!(" {ver}"),
                _ => String::new(),
            };
            println!("Added {name}{version} for {scope}. Each station installs it for itself: `spandeck tools` shows how it goes.");
        }
        "tools.scope" => println!("{name} is now for {scope}."),
        "tools.upgrade" => println!(
            "{} is to run {name} {}. `spandeck tools` shows how it goes; `spandeck tools rollout {name}` brings the other stations.",
            text(&v["on"]),
            text(&v["to"])
        ),
        "tools.rollout" => println!(
            "Every station {name} is chosen for is to run {}. `spandeck tools` shows how it goes.",
            text(&v["to"])
        ),
        _ => println!("{name}: done."),
    }
    Ok(())
}

/// `spandeck tools install NAME [--station X]`: try again there.
pub fn install(
    c: &mut Client,
    json_out: bool,
    name: &str,
    station: Option<&str>,
) -> anyhow::Result<()> {
    let here = text(&c.call("station.info", json!({}))?["station"]).to_string();
    let station = station.unwrap_or(&here).to_string();
    let v = call(c, &here, &station, "tools.install", json!({"name": name}))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else if v["started"] == json!(true) {
        println!(
            "Installing {name} {} on {station}. `spandeck tools` shows how it goes.",
            text(&v["to"])
        );
    } else if v["running"].is_object() {
        println!("{name} is already being installed on {station}: nothing more was started.");
    } else if let Some(ready) = v["ready"].as_str() {
        println!("{name} {ready} is ready on {station}: nothing to do.");
    } else {
        println!("Nothing was started on {station}.");
    }
    Ok(())
}
