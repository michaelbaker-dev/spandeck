//! `spandeck agents`: the agent programs on each station, whether each is
//! the newest published version, and updating them with the same progress
//! and outcome the Agent programs dialog shows. (The owner, 17 Nov 2026:
//! "How do I know if an agent is up to date? ... is there an update
//! command somewhere?")
//!
//! Everything a station or a program said is printed as text.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spandeck_core::client::Client;

/// How long the list waits for the release hosts' answers.
const CHECK_WAIT: Duration = Duration::from_secs(14);

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn ago(ms: &Value) -> String {
    let Some(ms) = ms.as_u64() else {
        return String::new();
    };
    let s = spandeck_core::paths::now_ms().saturating_sub(ms) / 1000;
    match s {
        0..=59 => format!("{s} s ago"),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=172_799 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
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
) -> Result<Value, spandeck_core::client::ClientError> {
    if station == here {
        c.call(method, params)
    } else {
        c.call(
            "peer.call",
            json!({"station": station, "method": method, "params": params}),
        )
    }
}

/// Is this the newest? The words beside a version.
pub fn latest_words(row: &Value) -> String {
    let l = &row["latest"];
    match text(&l["state"]) {
        "latest" => format!("latest (checked {})", ago(&l["checked_ms"])),
        "available" => format!("{} available", text(&l["version"])),
        "unknown" => format!("could not check: {}", line(text(&l["why"]))),
        "off" => "checks are off on this station".into(),
        "not_checked" => "not checked yet".into(),
        _ if row["installed"] == json!(true) => {
            "this station's spandeck is older and cannot say whether this is the latest".into()
        }
        _ => String::new(),
    }
}

/// What a job is doing, or how it ended: the words the deck shows.
pub fn job_words(row: &Value) -> Option<String> {
    let j = &row["job"];
    let action = text(&j["action"]);
    let doing = if action == "install" {
        "Installing"
    } else {
        "Updating"
    };
    if j["state"] == json!("running") {
        let mut s = match (text(&j["step"]), j["command"].as_str()) {
            ("running", Some(cmd)) => format!("{doing}… running `{}`", line(cmd)),
            _ => format!("{doing}… reading its version first"),
        };
        if let Some(l) = j["last_line"].as_str() {
            s.push_str(&format!(": {}", line(l)));
        }
        return Some(s);
    }
    let (from, to) = (j["from"].as_str(), j["to"].as_str());
    let reason = line(text(&j["reason"]));
    let mut s = match (text(&j["outcome"]), action) {
        ("updated", _) => format!("Updated {} → {}", from.unwrap_or("?"), to.unwrap_or("?")),
        ("latest", _) => match (
            text(&row["latest"]["state"]),
            row["latest"]["version"].as_str(),
        ) {
            ("available", Some(v)) => format!(
                "Still {}: the updater finished without installing {v}",
                to.unwrap_or("?")
            ),
            _ => format!("Already the latest: {}", to.unwrap_or("?")),
        },
        ("installed", _) => format!(
            "{} {}",
            if from.is_some() {
                "Reinstalled"
            } else {
                "Installed"
            },
            to.unwrap_or("")
        ),
        ("failed", "install") => format!("Could not install: {reason}"),
        ("failed", _) => format!("Could not update: {reason}"),
        ("interrupted", _) => format!("Not known whether it finished: {reason}"),
        _ => return None,
    };
    let (now, later) = (
        j["restarted"].as_array().map_or(0, |a| a.len()),
        j["restart_when_idle"].as_array().map_or(0, |a| a.len()),
    );
    if now > 0 {
        s.push_str(&format!(" · {now} restarted into the conversation"));
    }
    if later > 0 {
        s.push_str(&format!(" · {later} restart when the turn ends"));
    }
    Some(s)
}

fn stations(
    c: &mut Client,
    only: Option<&str>,
    fleet: bool,
) -> anyhow::Result<(String, Vec<String>)> {
    let here = text(&c.call("station.info", json!({}))?["station"]).to_string();
    if let Some(st) = only {
        return Ok((here, vec![st.to_string()]));
    }
    if !fleet {
        return Ok((here.clone(), vec![here]));
    }
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

fn row_line(row: &Value) -> String {
    let name = text(&row["name"]);
    let health = row["health"]
        .as_str()
        .unwrap_or(if row["installed"] == json!(true) {
            "ok"
        } else {
            "missing"
        });
    let version = match health {
        "missing" => "not installed".to_string(),
        "broken" => format!("installed but not working: {}", line(text(&row["problem"]))),
        "no_answer" => "did not answer when asked its version".to_string(),
        _ => row["version"]
            .as_str()
            .unwrap_or("version not read")
            .to_string(),
    };
    let mut s = format!("  {name:<13} {version:<10}");
    if health != "missing" {
        let latest = latest_words(row);
        if !latest.is_empty() {
            s.push_str(&format!("  {latest}"));
        }
        if let Some(n) = row["panes"].as_u64().filter(|n| *n > 0) {
            s.push_str(&format!("  · {n} running"));
        }
    }
    if let Some(job) = job_words(row) {
        let when = match row["job"]["ended_ms"].as_u64() {
            Some(_) => format!(" ({})", ago(&row["job"]["ended_ms"])),
            None => String::new(),
        };
        s.push_str(&format!("\n  {:<13} last: {job}{when}", ""));
    } else if row["last_update"].is_object() {
        let u = &row["last_update"];
        s.push_str(&format!(
            "\n  {:<13} last: {} ({})",
            "",
            if u["ok"] == json!(true) {
                "updated"
            } else {
                "the last update failed"
            },
            ago(&u["at_ms"])
        ));
    }
    s
}

/// `spandeck agents`: the list. With `check`, each station is first asked
/// to ask the release hosts (it reuses an answer under three hours old;
/// `again` asks anew), and their answers are waited for.
pub fn list(
    c: &mut Client,
    json_out: bool,
    station: Option<&str>,
    check: bool,
    again: bool,
) -> anyhow::Result<()> {
    let (here, stations) = stations(c, station, true)?;
    if check {
        for st in &stations {
            // A station from before does not know the method: its rows say so.
            let _ = call(c, &here, st, "agents.check", json!({"again": again}));
        }
    }
    let start = Instant::now();
    let mut all = vec![];
    loop {
        all = stations
            .iter()
            .map(|st| (st.clone(), call(c, &here, st, "agents.status", json!({}))))
            .collect();
        let waiting = all.iter().any(|(_, rows)| {
            rows.as_ref().is_ok_and(|rows| {
                rows.as_array().is_some_and(|rows| {
                    rows.iter().any(|r| {
                        r["reading"] == json!(true) || r["latest"]["checking"] == json!(true)
                    })
                })
            })
        });
        if !waiting || start.elapsed() > CHECK_WAIT {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    if json_out {
        let v: Vec<Value> = all
            .iter()
            .map(|(st, rows)| match rows {
                Ok(rows) => json!({"station": st, "agents": rows}),
                Err(e) => json!({"station": st, "error": e.message}),
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    let mut available = 0;
    for (st, rows) in &all {
        println!("{st}");
        match rows {
            Err(e) => println!("  {}", line(&e.message)),
            Ok(rows) => {
                for row in rows.as_array().into_iter().flatten() {
                    println!("{}", row_line(row));
                    available += usize::from(row["latest"]["state"] == json!("available"));
                }
            }
        }
    }
    if available > 0 {
        println!(
            "\n{available} update{} available. `spandeck agents update --all` updates every station; `spandeck agents update --station NAME [AGENT]` one.",
            if available == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

/// `spandeck agents update`: start the jobs, then say how each goes until
/// all have ended. The jobs are the stations': closing this changes
/// nothing, and `spandeck agents` shows them again.
pub fn update(
    c: &mut Client,
    json_out: bool,
    station: Option<&str>,
    agent: Option<&str>,
    fleet: bool,
    restart: bool,
) -> anyhow::Result<bool> {
    let (here, stations) = stations(c, station, fleet)?;
    let mut params = json!({"restart": restart});
    if let Some(a) = agent {
        params["agent"] = json!(a);
    }
    // station -> the jobs to follow there (by agent), or all its rows for
    // a station from before that names none.
    let mut following: Vec<(String, Vec<String>)> = vec![];
    let mut failed = false;
    for st in &stations {
        match call(c, &here, st, "agents.update", params.clone()) {
            Err(e) => {
                failed = true;
                println!("{st}: could not start: {}", line(&e.message));
            }
            Ok(v) => {
                let mut agents = vec![];
                for s in v["started"].as_array().into_iter().flatten() {
                    agents.push(text(&s["agent"]).to_string());
                }
                for s in v["skipped"].as_array().into_iter().flatten() {
                    let running = s["job"].is_object();
                    if running {
                        agents.push(text(&s["agent"]).to_string());
                    }
                    if running
                        || agent.is_some()
                        || text(&s["why"]) != "not installed on this station"
                    {
                        println!(
                            "{st}: {}: not started, {}{}",
                            text(&s["name"]),
                            text(&s["why"]),
                            if running {
                                format!(
                                    " (asked by {} {}): following it",
                                    text(&s["job"]["by"]),
                                    ago(&s["job"]["started_ms"])
                                )
                            } else {
                                String::new()
                            }
                        );
                    }
                }
                if agents.is_empty() {
                    println!("{st}: nothing to update");
                } else {
                    println!("{st}: updating {}", agents.join(", "));
                    following.push((st.clone(), agents));
                }
            }
        }
    }
    let mut said: std::collections::HashMap<(String, String), String> = Default::default();
    let mut finals = vec![];
    let (mut updated, mut latest, mut bad) = (0, 0, vec![]);
    while !following.is_empty() {
        std::thread::sleep(Duration::from_millis(700));
        let mut still = vec![];
        for (st, agents) in following {
            let rows = match call(c, &here, &st, "agents.status", json!({})) {
                Ok(rows) => rows,
                // The station holds the jobs: look again.
                Err(_) => {
                    still.push((st, agents));
                    continue;
                }
            };
            let mut left = vec![];
            for a in agents {
                let Some(row) = rows
                    .as_array()
                    .and_then(|r| r.iter().find(|r| r["agent"] == json!(a)))
                else {
                    continue;
                };
                let old = !row["job"].is_object();
                let running = if old {
                    row["updating"] == json!(true)
                } else {
                    row["job"]["state"] == json!("running")
                };
                let words =
                    job_words(row).unwrap_or_else(|| match (running, &row["last_update"]) {
                        (true, _) => "Updating…".to_string(),
                        (false, u) if u["ok"] == json!(true) => {
                            format!("Updated: now {}", row["version"].as_str().unwrap_or("?"))
                        }
                        (false, u) => format!("Could not update: {}", line(text(&u["message"]))),
                    });
                let key = (st.clone(), a.clone());
                if said.get(&key) != Some(&words) && !json_out {
                    println!("{st}: {}: {words}", text(&row["name"]));
                }
                said.insert(key, words);
                if running {
                    left.push(a);
                    continue;
                }
                match text(&row["job"]["outcome"]) {
                    "updated" => updated += 1,
                    "latest" => latest += 1,
                    "" if row["last_update"]["ok"] == json!(true) => updated += 1,
                    _ => {
                        bad.push(format!("{} on {st}", text(&row["name"])));
                        if !json_out {
                            if let Some(out) = row["job"]["output"]
                                .as_str()
                                .filter(|o| !o.trim().is_empty())
                            {
                                println!("  what it said:");
                                for l in out.lines().filter(|l| !l.trim().is_empty()) {
                                    println!("    {}", line(l));
                                }
                            }
                        }
                    }
                }
                finals.push(json!({"station": st, "agent": a, "job": row["job"], "version": row["version"]}));
            }
            if !left.is_empty() {
                still.push((st, left));
            }
        }
        following = still;
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&finals).unwrap_or_default()
        );
    } else if updated + latest + bad.len() > 0 {
        let mut parts = vec![];
        if updated > 0 {
            parts.push(format!("{updated} updated"));
        }
        if latest > 0 {
            parts.push(format!("{latest} already the latest"));
        }
        if !bad.is_empty() {
            parts.push(format!("{} failed: {}", bad.len(), bad.join(", ")));
        }
        println!("{}", parts.join(", "));
    }
    Ok(!failed && bad.is_empty())
}
