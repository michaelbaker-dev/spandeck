//! Agent programs: every action says what it did, on its row; is this the
//! latest; and one job at a time.
//!
//! The reason for every test here is the owner's report of 17 Nov 2026,
//! from the Agent programs dialog on an iPhone: "I picked update all on
//! m4-MacBook, no feedback, don't know if it's doing it, if it worked,
//! don't know if the version listed is the latest or not"; and, the same
//! day, "I can't press update twice in a row, it should grey out until the
//! update process is complete". (DECISIONS.md, "Agent programs: every
//! action says what it did".)
//!
//! No real agent program, updater or installer is run: each is a stand-in
//! script, the release hosts are a server on this machine, and `npm` and
//! `brew` are stand-ins on the path.
#![cfg(unix)]
mod common;
use common::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Stand-ins for the agent programs, in one folder. What each does is in
/// files beside it: `<name>.version` (what it says its version is),
/// `<name>.next` (an update moves to this), `<name>.fail` (an update
/// prints this and fails), `<name>.sleep` (an update takes this long),
/// `<name>.slow` (it takes this long to say its version). Every run of its
/// updater adds a line to `<name>.runs`.
struct Tools {
    _dir: tempfile::TempDir,
    at: PathBuf,
}

impl Tools {
    fn new() -> Tools {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().to_path_buf();
        Tools { _dir: dir, at }
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.at.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\nd={}\n{body}\n", self.at.display()),
        )
        .unwrap();
        spandeck_terminal::sys::set_executable(&path).unwrap();
    }

    /// A program that says its version and updates itself with `word`.
    fn program(&self, name: &str, word: &str, version: &str) {
        self.set(name, "version", version);
        self.script(
            name,
            &format!(
                r#"n={name}
if [ "$1" = --version ]; then [ -f $d/$n.slow ] && sleep "$(cat $d/$n.slow)"; echo "$n $(cat $d/$n.version)"; exit 0; fi
if [ "$1" = {word} ]; then
  echo run >> $d/$n.runs
  echo "Current version: $(cat $d/$n.version)"
  echo "Checking for updates to latest version..."
  [ -f $d/$n.sleep ] && sleep "$(cat $d/$n.sleep)"
  if [ -f $d/$n.fail ]; then cat $d/$n.fail >&2; exit 1; fi
  if [ -f $d/$n.next ]; then mv $d/$n.next $d/$n.version; echo "Successfully updated to version $(cat $d/$n.version)"; else echo "$n is up to date ($(cat $d/$n.version))"; fi
  exit 0
fi
sleep 60"#
            ),
        );
    }

    fn set(&self, name: &str, what: &str, value: &str) {
        std::fs::write(self.at.join(format!("{name}.{what}")), value).unwrap();
    }

    fn unset(&self, name: &str, what: &str) {
        let _ = std::fs::remove_file(self.at.join(format!("{name}.{what}")));
    }

    /// How many times this program's updater has been run.
    fn runs(&self, name: &str) -> usize {
        std::fs::read_to_string(self.at.join(format!("{name}.runs")))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.at.display())
    }
}

/// The release hosts, on this machine: a path answers with what the test
/// put there, and every request is counted.
struct Hosts {
    port: u16,
    answers: Arc<Mutex<std::collections::HashMap<String, String>>>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Hosts {
    fn start() -> Hosts {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let answers: Arc<Mutex<std::collections::HashMap<String, String>>> = Default::default();
        let asked: Arc<Mutex<Vec<String>>> = Default::default();
        let (a, q) = (answers.clone(), asked.clone());
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                q.lock().unwrap().push(path.clone());
                let body = a.lock().unwrap().get(&path).cloned();
                let _ = match body {
                    Some(b) => write!(
                        s,
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}",
                        b.len()
                    ),
                    None => write!(
                        s,
                        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    ),
                };
            }
        });
        Hosts {
            port,
            answers,
            asked,
        }
    }

    fn say(&self, path: &str, body: &str) {
        self.answers
            .lock()
            .unwrap()
            .insert(path.to_string(), body.to_string());
    }

    fn asked(&self, path: &str) -> usize {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|p| *p == path)
            .count()
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

fn rows(s: &Station) -> Vec<Value> {
    s.call("agents.status", json!({}))
        .as_array()
        .cloned()
        .unwrap()
}

fn row(s: &Station, agent: &str) -> Value {
    rows(s)
        .into_iter()
        .find(|r| r["agent"] == json!(agent))
        .unwrap_or_else(|| panic!("no row for {agent}"))
}

/// The row once its job is over.
fn ended(s: &Station, agent: &str) -> Value {
    wait_for(&format!("{agent}'s job to end"), 30, || {
        let r = row(s, agent);
        (r["job"]["state"] == json!("done")).then_some(r)
    })
}

fn latest(s: &Station, agent: &str) -> Value {
    wait_for(&format!("{agent}'s release host to be asked"), 30, || {
        let l = row(s, agent)["latest"].clone();
        (l["checking"] == json!(false) && l["state"] != json!("not_checked")).then_some(l)
    })
}

/// Config for a station that must find none of the four programs: a test
/// never runs a real one, whatever is installed on the machine it runs on.
fn no_programs(tools: &Tools) -> String {
    ["claude", "codex", "opencode", "antigravity"]
        .iter()
        .map(|a| {
            format!(
                "[agents.{a}]\nprogram = \"{}\"\n",
                tools.at.join("missing").display()
            )
        })
        .collect()
}

/// The four programs as the owner's fleet had them, the release hosts, and
/// the config that points one at the other. `nowhere`: the program whose
/// release host is a port nothing listens on.
fn fleet_of_one(nowhere: &str) -> (Tools, Hosts, String) {
    let tools = Tools::new();
    tools.program("claude", "update", "2.1.326");
    tools.program("codex", "update", "0.169.0");
    tools.program("opencode", "upgrade", "2.3.0");
    tools.program("agy", "update", "1.5.9");
    let hosts = Hosts::start();
    hosts.say("/claude", "2.1.330");
    hosts.say("/codex", r#"{"assets":[],"tag_name":"rust-v0.169.0"}"#);
    hosts.say("/opencode", r#"{"channel":"latest","version":"2.3.0"}"#);
    hosts.say(
        "/antigravity",
        r#"{"version":"1.5.9","url":"https://example.invalid/cli.tar.gz"}"#,
    );
    let closed = free_port();
    let config: String = ["claude", "codex", "opencode", "antigravity"]
        .iter()
        .map(|a| {
            let url = match *a == nowhere {
                true => format!("http://127.0.0.1:{closed}/{a}"),
                false => hosts.url(&format!("/{a}")),
            };
            format!("[agents.{a}]\nlatest_url = \"{url}\"\n")
        })
        .collect();
    (tools, hosts, config)
}

/// One station: is each program the latest (asked of the release host or
/// the package manager it was installed with, kept, asked again on demand,
/// a host that cannot be reached); an update that changes the version, one
/// that finds nothing newer, one that fails; one job at a time; update
/// all; the time limit; a restart in the middle.
#[test]
#[ignore = "merge tier: starts a station and runs stand-in updaters for about half a minute"]
fn every_update_says_what_it_did_and_each_program_whether_it_is_the_latest() {
    let (tools, hosts, config) = fleet_of_one("antigravity");
    // Stand-in package managers: each only answers what it would install.
    tools.script(
        "npm",
        r#"echo "npm $*" >> $d/npm.log
[ "$1" = view ] && echo 2.4.0
exit 0"#,
    );
    tools.script(
        "brew",
        r#"echo "brew $*" >> $d/brew.log
[ "$1" = info ] && echo '{"formulae":[],"casks":[{"token":"codex","version":"0.170.0"}]}'
exit 0"#,
    );
    let path = tools.path();
    let mut s = Station::start_with("alpha", &config, &[("PATH", &path)]);

    // ---- is it the latest? ----
    // Nothing is asked until the owner asks.
    let before = row(&s, "claude");
    assert_eq!(before["version"], json!("2.1.326"));
    assert_eq!(before["latest"]["state"], json!("not_checked"), "{before}");
    assert_eq!(hosts.asked("/claude"), 0);
    let asking = s.call("agents.check", json!({}));
    assert_eq!(asking["asking"].as_array().unwrap().len(), 4, "{asking}");
    let claude = latest(&s, "claude");
    assert_eq!(
        (&claude["state"], &claude["version"], &claude["asked"]),
        (&json!("available"), &json!("2.1.330"), &json!("127.0.0.1")),
        "{claude}"
    );
    // A tag (`rust-v0.169.0`) and a field of a document are read alike.
    assert_eq!(latest(&s, "codex")["state"], json!("latest"));
    assert_eq!(latest(&s, "opencode")["state"], json!("latest"));
    // A host that cannot be reached is "could not check", with why.
    let agy = latest(&s, "antigravity");
    assert_eq!(
        (&agy["state"], &agy["why"]),
        (&json!("unknown"), &json!("no route to 127.0.0.1")),
        "{agy}"
    );
    // Kept: opening the dialog again asks nobody. "Check again" does.
    let again = s.call("agents.check", json!({}));
    assert_eq!(again["asking"], json!([]), "{again}");
    assert_eq!(hosts.asked("/claude"), 1);
    hosts.say("/claude", "2.1.331");
    s.call("agents.check", json!({"agent": "claude", "again": true}));
    wait_for("the newer answer", 20, || {
        (latest(&s, "claude")["version"] == json!("2.1.331")).then_some(())
    });
    assert_eq!(hosts.asked("/claude"), 2);
    assert_eq!(hosts.asked("/codex"), 1);
    hosts.say("/claude", "2.1.330");
    s.call("agents.check", json!({"agent": "claude", "again": true}));
    wait_for("the answer put back", 20, || {
        (latest(&s, "claude")["version"] == json!("2.1.330")).then_some(())
    });
    // Neither an agent nor anything but the owner makes a station ask.
    let pane = s.fake(json!({"steps": []}), json!({"name": "crew"}));
    for (method, params) in [
        ("agents.check", json!({})),
        ("agents.update", json!({})),
        ("agents.install", json!({"agent": "codex"})),
        ("agents.status", json!({})),
    ] {
        let err = s.call_as_pane(&pane, method, params).unwrap_err();
        assert_eq!(err.code, "denied", "{method}");
    }

    // ---- an update that changes the version ----
    tools.set("claude", "next", "2.1.330");
    tools.set("claude", "sleep", "1.5");
    let began = std::time::Instant::now();
    let started = s.call("agents.update", json!({"agent": "claude"}));
    assert!(
        began.elapsed() < std::time::Duration::from_secs(1),
        "the answer waits for nothing: {:?}",
        began.elapsed()
    );
    let job = started["started"][0]["job"].as_str().unwrap().to_string();
    assert_eq!(started["started"][0]["from"], json!("2.1.326"), "{started}");
    // A second press runs nothing more: it is told of the job that runs.
    let second = s.call("agents.update", json!({"agent": "claude"}));
    assert_eq!(second["started"], json!([]), "{second}");
    assert_eq!(
        (
            &second["running"]["id"],
            &second["running"]["by"],
            &second["running"]["action"]
        ),
        (&json!(job), &json!("you"), &json!("update")),
        "{second}"
    );
    assert!(second["running"]["started_ms"].as_u64().unwrap() > 0);
    // Nor does a reinstall start beside it.
    let install = s.call("agents.install", json!({"agent": "claude"}));
    assert_eq!(
        (&install["started"], &install["running"]["id"]),
        (&json!(false), &json!(job)),
        "{install}"
    );
    // Anyone who looks sees it running, with what is run and its last line.
    let running = wait_for("the updater's own words", 10, || {
        let r = row(&s, "claude");
        r["job"]["last_line"].is_string().then_some(r)
    });
    let j = &running["job"];
    assert_eq!(
        (
            &j["id"],
            &j["state"],
            &j["step"],
            &j["command"],
            &j["last_line"]
        ),
        (
            &json!(job),
            &json!("running"),
            &json!("running"),
            &json!("claude update"),
            &json!("Checking for updates to latest version...")
        ),
        "{j}"
    );
    assert_eq!(running["updating"], json!(true));
    let done = ended(&s, "claude");
    let j = &done["job"];
    assert_eq!(
        (&j["id"], &j["outcome"], &j["from"], &j["to"], &j["reason"]),
        (
            &json!(job),
            &json!("updated"),
            &json!("2.1.326"),
            &json!("2.1.330"),
            &Value::Null
        ),
        "{j}"
    );
    assert!(
        j["output"]
            .as_str()
            .unwrap()
            .contains("Successfully updated to version 2.1.330"),
        "{j}"
    );
    assert_eq!(tools.runs("claude"), 1, "one press, one updater");
    // What was published is now what is installed.
    assert_eq!(done["latest"]["state"], json!("latest"), "{done}");
    assert_eq!(done["updating"], json!(false));

    // ---- one that finds nothing newer: the updater's own answer ----
    tools.unset("claude", "sleep");
    s.call("agents.update", json!({"agent": "claude"}));
    let j = wait_for("the second update", 20, || {
        let j = row(&s, "claude")["job"].clone();
        (j["state"] == json!("done") && j["id"] != json!(job)).then_some(j)
    });
    assert_eq!(
        (&j["outcome"], &j["from"], &j["to"]),
        (&json!("latest"), &json!("2.1.330"), &json!("2.1.330")),
        "{j}"
    );
    assert!(j["output"]
        .as_str()
        .unwrap()
        .contains("claude is up to date (2.1.330)"));

    // ---- one that fails: why in a line, and all it printed ----
    tools.set(
        "codex",
        "fail",
        "request to releases.example failed\nError: could not reach the release host\n    at fetch (node:internal)\n",
    );
    s.call("agents.update", json!({"agent": "codex"}));
    let j = ended(&s, "codex")["job"].clone();
    assert_eq!(
        (&j["outcome"], &j["reason"], &j["to"]),
        (
            &json!("failed"),
            &json!("Error: could not reach the release host"),
            &json!("0.169.0")
        ),
        "{j}"
    );
    let output = j["output"].as_str().unwrap();
    assert!(
        output.contains("Current version: 0.169.0") && output.contains("at fetch (node:internal)"),
        "{output}"
    );
    let logged = &s.log("agents.updated")[0]["data"];
    assert_eq!(
        (&logged["agent"], &logged["ok"], &logged["outcome"]),
        (&json!("codex"), &json!(false), &json!("failed")),
        "{logged}"
    );

    // ---- update all: each row its own result, one batch ----
    tools.set("claude", "next", "2.1.331");
    for name in ["claude", "codex", "opencode", "agy"] {
        tools.set(name, "sleep", "1.2");
    }
    let all = s.call("agents.update", json!({}));
    let batch = all["batch"].as_str().unwrap().to_string();
    assert_eq!(all["started"].as_array().unwrap().len(), 4, "{all}");
    // Pressed again, and one program alone, while it runs: nothing more
    // is started, and each answer says which job is running.
    let twice = s.call("agents.update", json!({}));
    assert_eq!(twice["started"], json!([]), "{twice}");
    let skipped = twice["skipped"].as_array().unwrap();
    assert!(
        skipped.len() == 4
            && skipped
                .iter()
                .all(|k| k["why"] == json!("an update is already running")
                    && k["job"]["started_ms"].is_u64()),
        "{twice}"
    );
    for name in ["claude", "codex", "opencode", "antigravity"] {
        ended(&s, name);
    }
    let outcomes: Vec<(String, String, String)> = rows(&s)
        .iter()
        .map(|r| {
            (
                r["agent"].as_str().unwrap().to_string(),
                r["job"]["outcome"].as_str().unwrap().to_string(),
                r["job"]["batch"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let want = |a: &str, o: &str| (a.to_string(), o.to_string(), batch.clone());
    assert_eq!(
        outcomes,
        vec![
            want("claude", "updated"),
            want("codex", "failed"),
            want("opencode", "latest"),
            want("antigravity", "latest"),
        ]
    );
    assert_eq!(
        (
            tools.runs("claude"),
            tools.runs("codex"),
            tools.runs("opencode"),
            tools.runs("agy")
        ),
        (3, 2, 1, 1),
        "each updater ran once for each update that was started"
    );
    // An "update all" while one program is being updated skips that one.
    tools.set("opencode", "sleep", "2");
    for name in ["claude", "codex", "agy"] {
        tools.unset(name, "sleep");
    }
    let one = s.call("agents.update", json!({"agent": "opencode"}));
    let all = s.call("agents.update", json!({}));
    let names = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| x["agent"].as_str().or(x.as_str()).unwrap().to_string())
            .collect()
    };
    assert_eq!(
        names(&all["started"]),
        ["claude", "codex", "antigravity"],
        "{all}"
    );
    assert_eq!(
        (&all["skipped"][0]["agent"], &all["skipped"][0]["job"]["id"]),
        (&json!("opencode"), &one["started"][0]["job"]),
        "{all}"
    );
    for name in ["claude", "codex", "opencode", "antigravity"] {
        ended(&s, name);
    }
    assert_eq!(tools.runs("opencode"), 2);
    tools.unset("codex", "fail");
    tools.unset("opencode", "sleep");

    // ---- the package manager a copy was installed with is asked ----
    s.call(
        "agents.install",
        json!({"agent": "opencode", "method": "npm"}),
    );
    let j = ended(&s, "opencode")["job"].clone();
    assert_eq!(
        (&j["action"], &j["outcome"], &j["command"]),
        (
            &json!("install"),
            &json!("installed"),
            &json!("npm install -g @opencode/cli")
        ),
        "{j}"
    );
    s.call(
        "agents.install",
        json!({"agent": "codex", "method": "brew"}),
    );
    ended(&s, "codex");
    let asking = s.call("agents.check", json!({}));
    let asking = names(&asking["asking"]);
    assert!(
        asking.contains(&"codex".to_string())
            && asking.contains(&"opencode".to_string())
            && !asking.contains(&"claude".to_string()),
        "those whose way of installing changed are asked about again: {asking:?}"
    );
    let opencode = wait_for("npm's answer", 20, || {
        let l = latest(&s, "opencode");
        (l["method"] == json!("npm")).then_some(l)
    });
    assert_eq!(
        (&opencode["state"], &opencode["version"], &opencode["asked"]),
        (
            &json!("available"),
            &json!("2.4.0"),
            &json!("`npm view @opencode/cli version`")
        ),
        "{opencode}"
    );
    let codex = wait_for("Homebrew's answer", 20, || {
        let l = latest(&s, "codex");
        (l["method"] == json!("brew")).then_some(l)
    });
    assert_eq!(
        (&codex["state"], &codex["version"]),
        (&json!("available"), &json!("0.170.0")),
        "{codex}"
    );
    let asked = std::fs::read_to_string(tools.at.join("brew.log")).unwrap();
    assert!(
        asked.contains("brew info --json=v2 --cask codex"),
        "{asked}"
    );
    assert_eq!(
        hosts.asked("/opencode"),
        1,
        "npm was asked, not the script's host"
    );

    // ---- a job that hangs does not grey its row for ever ----
    s.call(
        "station.flags",
        json!({"set": {"agents.job_limit_secs": 2}}),
    );
    tools.set("agy", "sleep", "8");
    let began = std::time::Instant::now();
    s.call("agents.update", json!({"agent": "antigravity"}));
    let j = ended(&s, "antigravity")["job"].clone();
    assert!(began.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(
        (&j["outcome"], &j["reason"], &j["limit_secs"]),
        (
            &json!("failed"),
            &json!("it was still running after 2 seconds and was stopped"),
            &json!(2)
        ),
        "{j}"
    );
    assert!(
        j["output"]
            .as_str()
            .unwrap()
            .contains("Checking for updates to latest version..."),
        "what it had printed is kept: {j}"
    );
    s.call(
        "station.flags",
        json!({"set": {"agents.job_limit_secs": null}}),
    );
    // The row is usable again.
    tools.unset("agy", "sleep");
    s.call("agents.update", json!({"agent": "antigravity"}));
    wait_for("the next update", 20, || {
        (row(&s, "antigravity")["job"]["outcome"] == json!("latest")).then_some(())
    });

    // ---- a restart in the middle ends the job honestly ----
    tools.set("claude", "sleep", "6");
    let started = s.call("agents.update", json!({"agent": "claude"}));
    wait_for("the updater to be running", 10, || {
        (row(&s, "claude")["job"]["step"] == json!("running")).then_some(())
    });
    s.kill9();
    s.launch();
    let after = row(&s, "claude");
    let j = &after["job"];
    assert_eq!(
        (&j["id"], &j["state"], &j["outcome"]),
        (
            &started["started"][0]["job"],
            &json!("done"),
            &json!("interrupted")
        ),
        "{j}"
    );
    assert!(
        j["reason"]
            .as_str()
            .unwrap()
            .starts_with("interrupted by a restart of spandeck on alpha"),
        "{j}"
    );
    assert_eq!(after["updating"], json!(false));
    // What the release host said is kept across the restart, and not asked
    // for again by it.
    assert_eq!(after["latest"]["version"], json!("2.1.330"), "{after}");
    assert_eq!(hosts.asked("/claude"), 3);
}

/// Two stations, as the owner had: the deck is one station's, the programs
/// another's. An update asked for through the fleet is started there and
/// answered at once, whatever the programs take to say their versions (the
/// answer used to wait for each, and over the link the caller gave up
/// after five seconds and was told nothing it could use); what it does is
/// read there by anyone, the first caller gone; and a station whose config
/// turns the checks off asks nobody.
#[test]
#[ignore = "merge tier: two stations, programs that take seconds to answer"]
fn an_update_asked_through_another_station_starts_there_and_reports_from_there() {
    let tools = Tools::new();
    for (name, word) in [
        ("claude", "update"),
        ("codex", "update"),
        ("opencode", "upgrade"),
        ("agy", "update"),
    ] {
        tools.program(name, word, "1.0.0");
        // Slow to say its version: together, more than a call is given.
        tools.set(name, "slow", "1.6");
    }
    tools.set("claude", "next", "1.0.1");
    let hosts = Hosts::start();
    hosts.say("/claude", "9.9.9");
    let path = tools.path();
    let config = format!(
        "[agents]\ncheck_latest = false\n[agents.claude]\nlatest_url = \"{}\"\n",
        hosts.url("/claude")
    );
    let none = no_programs(&tools);
    let fleet = Station::fleet(&["alpha", "bravo"], |name, _| match name {
        "bravo" => (config.clone(), vec![("PATH".to_string(), path.clone())]),
        _ => (none.clone(), vec![]),
    });
    let (a, b) = (&fleet[0], &fleet[1]);
    wait_for("alpha to see bravo", 20, || {
        (a.sees("bravo").as_deref() == Some("online")).then_some(())
    });
    let on_bravo = |method: &str, params: Value| {
        let began = std::time::Instant::now();
        let out = a.try_call(
            "peer.call",
            json!({"station": "bravo", "method": method, "params": params}),
        );
        (out, began.elapsed())
    };
    // The rows are answered within what a call between stations is given,
    // though the four programs take 6.4 s between them to say a version.
    let (status, took) = on_bravo("agents.status", json!({}));
    let status = status.unwrap_or_else(|e| panic!("bravo's rows after {took:?}: {}", e.message));
    assert_eq!(status.as_array().unwrap().len(), 4);
    assert!(took < std::time::Duration::from_millis(4500), "{took:?}");

    let (started, took) = on_bravo("agents.update", json!({}));
    let started = started.unwrap_or_else(|e| panic!("update all after {took:?}: {}", e.message));
    assert!(took < std::time::Duration::from_secs(2), "{took:?}");
    assert_eq!(
        (
            started["station"].as_str(),
            started["started"].as_array().unwrap().len()
        ),
        (Some("bravo"), 4),
        "{started}"
    );
    assert_eq!(
        started["started"][0]["job"]
            .as_str()
            .map(|j| j.starts_with("job")),
        Some(true)
    );
    // Asked again through the fleet while they run: nothing more starts.
    let (twice, _) = on_bravo("agents.update", json!({}));
    assert_eq!(twice.unwrap()["started"], json!([]));
    // Read at bravo itself, by a caller that asked for nothing.
    for name in ["claude", "codex", "opencode", "antigravity"] {
        ended(b, name);
    }
    let claude = row(b, "claude");
    assert_eq!(
        (
            &claude["job"]["outcome"],
            &claude["job"]["from"],
            &claude["job"]["to"],
            &claude["job"]["by"]
        ),
        (
            &json!("updated"),
            &json!("1.0.0"),
            &json!("1.0.1"),
            &json!("you (via alpha)")
        ),
        "{claude}"
    );
    assert_eq!(row(b, "codex")["job"]["outcome"], json!("latest"));
    assert_eq!(b.log("agents.updated").len(), 4);
    for name in ["claude", "codex", "opencode", "agy"] {
        assert_eq!(tools.runs(name), 1, "{name}");
    }
    // And through alpha, as its deck reads them.
    let (rows, _) = on_bravo("agents.status", json!({}));
    assert_eq!(rows.unwrap()[0]["job"]["outcome"], json!("updated"));

    // Checks are off on bravo: it says so, and asks nobody.
    let (off, _) = on_bravo("agents.check", json!({"again": true}));
    assert_eq!(off.unwrap()["off"], json!(true));
    assert_eq!(claude["latest"], json!({"state": "off"}));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(hosts.asked("/claude"), 0);

    // The same from the command line: the list, and an update with its
    // progress and outcome.
    let cli = |args: &[&str]| {
        let out = a
            .command(SPANDECK)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();
        (
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            out.status.success(),
        )
    };
    let (list, ok) = cli(&["agents", "--station", "bravo"]);
    assert!(
        ok && list.contains("bravo")
            && list.contains("Claude Code   1.0.1")
            && list.contains("checks are off on this station")
            && list.contains("last: Updated 1.0.0 → 1.0.1"),
        "{list}"
    );
    tools.set("codex", "fail", "Error: no network\n");
    let (said, ok) = cli(&["agents", "update", "--station", "bravo", "codex"]);
    assert!(
        !ok && said.contains("bravo: updating codex")
            && said.contains("bravo: Codex: Could not update: Error: no network")
            && said.contains("1 failed: Codex on bravo"),
        "{said}"
    );
    tools.unset("codex", "fail");
    tools.set("opencode", "next", "1.2.0");
    let (said, ok) = cli(&["agents", "update", "--all"]);
    assert!(
        ok && said.contains("bravo: OpenCode: Updated 1.0.0 → 1.2.0")
            && said.contains("bravo: Claude Code: Already the latest: 1.0.1")
            && said.contains("1 updated, 3 already the latest"),
        "{said}"
    );
}

/// The dialog itself, at a phone's size and (for pictures) a desktop's.
/// With `SPANDECK_SHOTS` set, pictures are written there.
fn the_dialog(phone: bool) {
    use common::browser::Page;
    let (tools, _hosts, config) = fleet_of_one("opencode");
    // What a program prints is shown as text, markup and all.
    tools.set(
        "codex",
        "fail",
        "Error: could not reach the release host <b id=injected>not markup</b>\n    at fetch (node:internal)\n",
    );
    tools.set("claude", "next", "2.1.330");
    tools.set("claude", "sleep", "5");
    let missing = tools.at.join("missing");
    let path = tools.path();
    let s = Station::start_with(
        "m4-macbook",
        &format!("{config}program = \"{}\"\n", missing.display()),
        &[("PATH", &path)],
    );
    let tag = if phone { "phone" } else { "desktop" };
    let page = if phone {
        let p = Page::open(&s, 390, 844);
        p.phone_screen();
        p
    } else {
        Page::open(&s, 1200, 800)
    };
    let shots = std::env::var_os("SPANDECK_SHOTS").map(PathBuf::from);
    let shot = |name: &str| {
        use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
        if let Some(out) = &shots {
            std::fs::create_dir_all(out).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(350));
            let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
            std::fs::write(out.join(format!("{tag}-{name}.png")), png).unwrap();
        }
    };
    let open = || {
        if phone {
            let by_station = "[...document.querySelectorAll('.home-switch button')].find(b => b.innerText.trim() === 'By station')";
            page.wait("the home screen", &format!("!!{by_station}"));
            page.js(&format!("{by_station}.click()"));
            page.wait(
                "the station's menu button",
                "!!document.querySelector('.station-more')",
            );
            page.js("document.querySelector('.station-more').click()");
        } else {
            page.right_click("document.querySelector('.tile.station')");
        }
        page.choose("Agent programs and versions");
        page.wait(
            "the four cards",
            "document.querySelectorAll('.agent-program').length === 4",
        );
    };
    let card = |agent: &str, rest: &str| {
        format!("(c => {rest})(document.querySelector('.agent-program[data-agent={agent}]'))")
    };
    let update =
        "[...c.querySelectorAll('.ap-line3 button')].find(b => b.classList.contains('ap-update'))";

    open();
    // Idle, each asked whether it is the latest.
    page.wait(
        "the latest marks",
        &card("claude", "c.querySelector('.ap-available')?.innerText.includes('2.1.330 available') && document.querySelector('.agent-program[data-agent=codex] .ap-latest') && document.querySelector('.agent-program[data-agent=opencode] .ap-unknown')?.innerText.includes('could not check: no route to 127.0.0.1')"),
    );
    assert_eq!(
        page.js("document.querySelector('.ap-fleet-words').innerText.startsWith('1 update available on 1 station')"),
        json!(true),
        "{}",
        page.js("document.querySelector('.ap-fleet').innerText")
    );
    // Every card is the same lines in the same order, whatever it holds;
    // the actions are in one order, at the right.
    let shape = page.js(
        "JSON.stringify([...document.querySelectorAll('.agent-program')].map(c => [...c.children].map(x => x.className)))",
    );
    let same = json!(vec![
        json!(["ap-line1", "ap-status", "ap-line2", "ap-line3"]);
        4
    ])
    .to_string();
    assert_eq!(shape, json!(same));
    assert_eq!(
        page.js("JSON.stringify([...document.querySelectorAll('.agent-program')].map(c => [...c.querySelectorAll('.ap-line3 button')].map(b => b.innerText)))"),
        json!(json!([["Reinstall", "Update"], ["Reinstall", "Update"], ["Reinstall", "Update"], ["Install"]]).to_string())
    );
    assert_eq!(
        page.js("[...document.querySelectorAll('.agent-program')].every(c => { const a = c.querySelector('.ap-line3').getBoundingClientRect(), b = [...c.querySelectorAll('.ap-line3 button')].pop().getBoundingClientRect(), k = c.getBoundingClientRect(); return k.right - b.right < 16 && b.top >= c.querySelector('.ap-line1').getBoundingClientRect().bottom - 1 && a.width > 0; })"),
        json!(true),
        "the actions are at the right, under the name"
    );
    // The one with an update waiting is the one whose Update stands out.
    assert_eq!(
        page.js("JSON.stringify([...document.querySelectorAll('.agent-program .ap-update')].map(b => b.classList.contains('primary')))"),
        json!("[true,false,false]")
    );
    if phone {
        assert_eq!(
            page.js("(d => [...d.querySelectorAll('button, select')].filter(b => b.offsetParent && !b.closest('.dialog-head')).every(b => b.getBoundingClientRect().height >= 44) && [...d.querySelectorAll('select')].every(x => parseFloat(getComputedStyle(x).fontSize) >= 16) && d.scrollWidth <= d.clientWidth && d.getBoundingClientRect().right <= innerWidth)(document.querySelector('.dialog-body'))"),
            json!(true),
            "44 px targets, 16 px fields, nothing off the side"
        );
    }
    shot("1-idle");

    // Update, pressed twice quickly: one job, and the button is greyed
    // from the press until the outcome.
    page.js(&card(
        "claude",
        &format!("{{ const b = {update}; b.click(); b.click(); return true; }}"),
    ));
    assert_eq!(
        page.js(&card(
            "claude",
            &format!("(b => b.disabled && b.getAttribute('aria-disabled') === 'true')({update})")
        )),
        json!(true),
        "greyed at once"
    );
    page.wait(
        "the row to say what is running",
        &card(
            "claude",
            "c.querySelector('.ap-status').innerText.includes('Updating… running `claude update`')",
        ),
    );
    assert_eq!(
        page.js(&card("claude", &format!("(b => b.disabled && b.innerText === 'Updating…' && [...c.querySelectorAll('.ap-line3 button')].every(x => x.disabled))({update})"))),
        json!(true)
    );
    // Other rows stay usable; the station's and the fleet's "update all" do not.
    assert_eq!(
        page.js(&card("opencode", &format!("!({update}).disabled && document.querySelector('.ap-all').disabled && document.querySelector('.ap-fleet-all').disabled"))),
        json!(true)
    );
    page.wait(
        "its last line",
        &card(
            "claude",
            "c.querySelector('.ap-line')?.innerText.includes('Checking for updates')",
        ),
    );
    shot("2-updating");
    // Closed and opened again in the middle: the station says it is
    // running, so the row does, and the button is still greyed.
    page.js("[...document.querySelectorAll('.dialog .row.end button')].find(b => b.innerText === 'Close').click()");
    page.wait(
        "the dialog gone",
        "!document.querySelector('.agent-program')",
    );
    open();
    assert_eq!(
        page.js(&card("claude", &format!("c.querySelector('.ap-status').innerText.includes('Updating…') && ({update}).disabled && ({update}).getAttribute('aria-disabled') === 'true'"))),
        json!(true),
        "after reopening, mid-job"
    );
    page.wait(
        "the outcome on the row",
        &card(
            "claude",
            "c.querySelector('.ap-job.good')?.innerText.includes('Updated 2.1.326 → 2.1.330')",
        ),
    );
    assert_eq!(
        page.js(&card("claude", &format!("!({update}).disabled && ({update}).getAttribute('aria-disabled') === 'false' && !({update}).classList.contains('primary') && !!c.querySelector('.ap-latest')"))),
        json!(true),
        "usable again, and now the latest"
    );
    assert_eq!(tools.runs("claude"), 1, "two presses ran one updater");
    assert_eq!(s.log("agents.updated").len(), 1);
    shot("3-updated");

    // Update all here: progress under the station's name, then a summary,
    // and each row its own result.
    for name in ["codex", "opencode"] {
        tools.set(name, "sleep", "2");
    }
    tools.unset("claude", "sleep");
    page.js("document.querySelector('.ap-all').click()");
    page.wait(
        "the count under the station's name",
        "/Updating 3 programs on m4-macbook… \\d of 3/.test(document.querySelector('.ap-batch').innerText)",
    );
    shot("4-updating-all");
    page.wait(
        "the summary",
        "document.querySelector('.ap-batch').innerText.includes('2 already the latest, 1 failed: Codex')",
    );
    assert_eq!(
        page.js(&card("codex", "c.querySelector('.ap-job.failed').innerText.includes('Could not update: Error: could not reach the release host <b id=injected>not markup</b>') && !document.querySelector('#injected')")),
        json!(true),
        "the reason, as text"
    );
    assert_eq!(
        page.js(&card(
            "opencode",
            "c.querySelector('.ap-job.good').innerText.includes('Already the latest: 2.3.0')"
        )),
        json!(true)
    );
    shot("5-update-all-summary");
    page.js(&card(
        "codex",
        "{ c.querySelector('.ap-output summary').click(); return true; }",
    ));
    page.wait(
        "what it said",
        &card("codex", "c.querySelector('.ap-output').open && c.querySelector('.ap-output pre').innerText.includes('at fetch (node:internal)')"),
    );
    if phone {
        assert_eq!(
            page.js(
                "(d => d.scrollWidth <= d.clientWidth)(document.querySelector('.dialog-body'))"
            ),
            json!(true)
        );
    }
    page.js(&card(
        "codex",
        "{ c.scrollIntoView({block: 'center'}); return true; }",
    ));
    shot("6-failed-with-output");
}

#[test]
#[ignore = "merge tier: a browser at a phone's size"]
fn the_agent_programs_dialog_on_a_phone() {
    the_dialog(true);
}

/// Not a test of anything the phone's does not cover: the same scenario at
/// a desktop's width, for the pictures.
/// `SPANDECK_SHOTS=/some/dir cargo test --test agent_programs -- --ignored the_agent_programs_dialog`
#[test]
#[ignore = "pictures"]
fn the_agent_programs_dialog_on_a_desktop() {
    the_dialog(false);
}
