//! A change tried again is done once, and an agent's tool does not fail
//! because spandeck restarted (DECISIONS.md, "A retried change is done
//! once"). Merge tier: each test starts stations, restarts or kills them
//! and waits out a retry budget, so none runs in the every-change check.
//!
//!   cargo test -p spandeck --test retry -- --ignored
//!
//! The audit (FAILURES §6, §7; gaps 7 and 11): a tool call failed at once
//! with "spandeckd is not running" for as long as the daemon was away,
//! even the 20 ms of an upgrade; and a call through another station whose
//! answer was lost read as "unreachable" although it had been carried
//! out, so the owner's second click started a second pane.
#![cfg(unix)]
mod common;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};
use spandeck_core::client::Client;

/// One request line on the station's own socket, with whatever the test
/// puts beside the token (`rid`, `again_ms`). The whole answer.
fn raw(s: &Station, token: &str, method: &str, params: Value, beside: Value) -> Value {
    let mut stream = std::os::unix::net::UnixStream::connect(s.paths().socket()).unwrap();
    let mut req = json!({"id": 1, "token": token, "method": method, "params": params});
    for (k, v) in beside.as_object().unwrap() {
        req[k] = v.clone();
    }
    writeln!(stream, "{req}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn rid(n: u32) -> String {
    format!("{n:032x}")
}

fn code(answer: &Value) -> &str {
    answer["error"]["code"].as_str().unwrap_or("")
}

fn panes_named(s: &Station, name: &str) -> usize {
    s.call("pane.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["name"] == json!(name))
        .count()
}

fn messages_saying(s: &Station, body: &str) -> usize {
    s.call("msg.recent", json!({"limit": 1000}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["body"] == json!(body))
        .count()
}

fn project(s: &Station, name: &str) -> std::path::PathBuf {
    let repo = s.git_repo(name);
    std::fs::create_dir_all(repo.join(".spandeck")).unwrap();
    std::fs::write(
        repo.join(".spandeck/project.toml"),
        format!("name = \"{name}\"\n"),
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "add project file"]);
    s.call("project.new", json!({"path": repo}));
    repo
}

/// What the station's database holds, read by another program.
fn rows_like(s: &Station, like: &str) -> i64 {
    let conn = rusqlite::Connection::open_with_flags(
        s.paths().db(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM kv WHERE (k LIKE 'rid:%' OR k LIKE 'stoprid:%') AND (k LIKE ?1 OR v LIKE ?1)",
        [like],
        |r| r.get(0),
    )
    .unwrap()
}

/// The safety properties of the store, against a real station: a second
/// try is done once; an answer goes only to the same caller, method and
/// parameters; what is typed, a hook, a stop and an answer that carries a
/// token never go through it; and with the switch off nothing is tried
/// again by its id.
#[test]
#[ignore = "merge tier: starts a station"]
fn a_change_tried_again_is_done_once_and_only_its_caller_is_answered() {
    let s = Station::start("alpha");
    let owner = s.token();
    assert!(
        s.call("station.info", json!({}))["caps"]
            .as_array()
            .unwrap()
            .contains(&json!("rid")),
        "a station that keeps ids says so"
    );

    // A pane started, the answer lost, asked again: one pane, one answer.
    let spawn = json!({"agent": "shell", "name": "once"});
    let first = raw(
        &s,
        &owner,
        "pane.spawn",
        spawn.clone(),
        json!({"rid": rid(1)}),
    );
    let second = raw(
        &s,
        &owner,
        "pane.spawn",
        spawn.clone(),
        json!({"rid": rid(1), "again_ms": 300}),
    );
    assert!(first["result"]["id"].is_string(), "{first}");
    assert_eq!(first["result"]["id"], second["result"]["id"], "{second}");
    assert_eq!(panes_named(&s, "once"), 1);
    let pane = first["result"]["id"].as_str().unwrap().to_string();

    // The same id for other parameters, or another method: refused, and
    // neither run nor answered from the store.
    let other = raw(
        &s,
        &owner,
        "pane.spawn",
        json!({"agent": "shell", "name": "other"}),
        json!({"rid": rid(1)}),
    );
    assert_eq!(code(&other), "rid_reused", "{other}");
    assert_eq!(panes_named(&s, "other"), 0);
    let other = raw(
        &s,
        &owner,
        "pane.close",
        json!({"pane": pane}),
        json!({"rid": rid(1)}),
    );
    assert_eq!(code(&other), "rid_reused", "{other}");
    assert_eq!(panes_named(&s, "once"), 1);
    // An id that is not an id is refused before anything is looked at.
    let bad = raw(&s, &owner, "pane.spawn", spawn, json!({"rid": "x:y"}));
    assert_eq!(code(&bad), "invalid", "{bad}");

    // An agent's requests are its pane's. With the very id the owner used
    // it reads nothing of the owner's, and what it stores the owner's same
    // id does not read.
    let repo = project(&s, "demo");
    let worker = s.fake(
        json!({"steps": []}),
        json!({"name": "worker", "project": "demo", "cwd": repo}),
    );
    s.wait_text(&worker, "[fake] session");
    let agent = s.pane_token(&worker);
    let denied = raw(
        &s,
        &agent,
        "pane.spawn",
        json!({"agent": "shell", "name": "once"}),
        json!({"rid": rid(1), "again_ms": 300}),
    );
    assert_eq!(code(&denied), "denied", "{denied}");
    let note = json!({"to": "you", "body": "said once"});
    let mine = raw(&s, &agent, "msg.send", note.clone(), json!({"rid": rid(2)}));
    let mine_again = raw(
        &s,
        &agent,
        "msg.send",
        note.clone(),
        json!({"rid": rid(2), "again_ms": 200}),
    );
    assert!(mine["result"]["id"].is_string(), "{mine}");
    assert_eq!(mine["result"]["id"], mine_again["result"]["id"]);
    assert_eq!(messages_saying(&s, "said once"), 1);
    let owners = raw(&s, &owner, "msg.send", note, json!({"rid": rid(2)}));
    assert_ne!(owners["result"]["id"], mine["result"]["id"], "{owners}");
    assert_eq!(messages_saying(&s, "said once"), 2);

    // A hook is never answered from the store, not even with an id its
    // own agent used for something else: a first try runs as a hook, and
    // a second is told the station cannot confirm.
    let hook = json!({"agent": "claude", "payload": {"hook_event_name": "Notification"}});
    let ran = raw(
        &s,
        &agent,
        "hook.ingest",
        hook.clone(),
        json!({"rid": rid(2)}),
    );
    assert!(
        !["rid_reused", "outcome_unknown"].contains(&code(&ran)) && ran["result"] != mine["result"],
        "{ran}"
    );
    let again = raw(
        &s,
        &agent,
        "hook.ingest",
        hook,
        json!({"rid": rid(2), "again_ms": 100}),
    );
    assert_eq!(code(&again), "outcome_unknown", "{again}");

    // What is sent into a pane is never sent twice.
    let typed = json!({"pane": pane, "text": "echo typed-$((40+2))"});
    let sent = raw(
        &s,
        &owner,
        "pane.send_text",
        typed.clone(),
        json!({"rid": rid(3)}),
    );
    assert_eq!(sent["result"]["ok"], json!(true), "{sent}");
    let again = raw(
        &s,
        &owner,
        "pane.send_text",
        typed,
        json!({"rid": rid(3), "again_ms": 100}),
    );
    assert_eq!(code(&again), "outcome_unknown", "{again}");
    let screen = s.wait_text(&pane, "typed-42");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(s.text(&pane).matches("typed-42").count(), 1, "{screen}");

    // Pause on is as good a second time. Pause off is not: a late second
    // try would lift a pause set after the first.
    let pause = |on: bool, n: u32, again: bool| {
        let mut beside = json!({"rid": rid(n)});
        if again {
            beside["again_ms"] = json!(100);
        }
        raw(
            &s,
            &owner,
            "pane.pause",
            json!({"pane": pane, "paused": on}),
            beside,
        )
    };
    assert_eq!(pause(true, 4, false)["result"]["ok"], json!(true));
    assert_eq!(pause(true, 4, true)["result"]["ok"], json!(true));
    assert_eq!(code(&pause(false, 5, true)), "outcome_unknown");
    assert_eq!(s.pane(&pane)["paused"], json!(true));

    // "Stop everything", asked twice with one id, is one stop.
    let stop = raw(
        &s,
        &owner,
        "stop.all",
        json!({"scope": "station"}),
        json!({"rid": rid(6)}),
    );
    let stop_again = raw(
        &s,
        &owner,
        "stop.all",
        json!({"scope": "station"}),
        json!({"rid": rid(6), "again_ms": 100}),
    );
    assert_eq!(
        stop["result"]["stops"].as_array().unwrap().len(),
        1,
        "{stop}"
    );
    assert_eq!(stop_again["result"]["repeat"], json!(true), "{stop_again}");
    assert_eq!(
        stop["result"]["stops"][0]["id"],
        stop_again["result"]["stops"][0]["id"]
    );
    assert_eq!(s.call("stop.list", json!({})).as_array().unwrap().len(), 1);

    // A project added twice with one id: the second is the first's answer
    // (run again, it would be refused as already there).
    let repo2 = s.git_repo("second");
    let new = json!({"path": repo2, "name": "second"});
    let made = raw(
        &s,
        &owner,
        "project.new",
        new.clone(),
        json!({"rid": rid(7)}),
    );
    let made_again = raw(
        &s,
        &owner,
        "project.new",
        new,
        json!({"rid": rid(7), "again_ms": 100}),
    );
    assert!(made["result"].is_object(), "{made}");
    assert_eq!(made["result"], made_again["result"], "{made_again}");

    // An approval denied, the answer lost, denied again: the audit saw
    // the second read as a failure ("no longer waiting") though the first
    // had been carried out. The second try is answered from the
    // approval's own record, and the agent is answered once. The other
    // answer, tried the same way, is not taken for it.
    let asker = s.fake(
        json!({"steps": [
            {"prompt": "write it"},
            {"permission": {"name": "Write", "file": "/tmp/a.txt"}},
            {"turn_end": "done"}
        ]}),
        json!({}),
    );
    let approval = wait_for("an approval request", 10, || {
        s.call("approval.list", json!({}))
            .as_array()
            .and_then(|a| a.first().cloned())
    });
    let deny = json!({"id": approval["id"], "allow": false, "reason": "no"});
    let denied = raw(
        &s,
        &owner,
        "approval.answer",
        deny.clone(),
        json!({"rid": rid(10)}),
    );
    assert_eq!(denied["result"]["ok"], json!(true), "{denied}");
    let denied_again = raw(
        &s,
        &owner,
        "approval.answer",
        deny,
        json!({"rid": rid(10), "again_ms": 200}),
    );
    assert_eq!(denied_again["result"]["ok"], json!(true), "{denied_again}");
    assert_eq!(denied_again["result"]["repeat"], json!(true));
    let allowed = raw(
        &s,
        &owner,
        "approval.answer",
        json!({"id": approval["id"], "allow": true}),
        json!({"rid": rid(11), "again_ms": 200}),
    );
    assert_eq!(code(&allowed), "invalid", "{allowed}");
    s.wait_state(&asker, "done");
    assert_eq!(s.text(&asker).matches("[fake] permission: deny").count(), 1);

    // A call whose answer carries a token is never kept, and never run a
    // second time.
    let remove = raw(
        &s,
        &owner,
        "fleet.remove",
        json!({"station": "nobody"}),
        json!({"rid": rid(8), "again_ms": 100}),
    );
    assert_eq!(code(&remove), "outcome_unknown", "{remove}");
    // Kept in `kv`: the pane, the messages, the project and the stop. Of
    // the hook, the text, the pause and the removal, nothing.
    wait_for("the rows to be written", 5, || {
        (rows_like(&s, "%project.new%") == 1).then_some(())
    });
    assert_eq!(rows_like(&s, "%pane.spawn%"), 1);
    assert_eq!(rows_like(&s, "%msg.send%"), 2);
    assert_eq!(rows_like(&s, "stoprid:owner:%"), 1);
    for never in [
        "%hook.ingest%",
        "%send_text%",
        "%typed-%",
        "%pane.pause%",
        "%fleet.remove%",
    ] {
        assert_eq!(rows_like(&s, never), 0, "{never}");
    }

    // The switch off: the station no longer says it keeps ids, a first try
    // runs as it always did, and a second is not run.
    s.call("station.flags", json!({"set": {"request_ids": false}}));
    assert!(!s.call("station.info", json!({}))["caps"]
        .as_array()
        .unwrap()
        .contains(&json!("rid")));
    let off = json!({"agent": "shell", "name": "off"});
    let first = raw(
        &s,
        &owner,
        "pane.spawn",
        off.clone(),
        json!({"rid": rid(9)}),
    );
    assert!(first["result"]["id"].is_string(), "{first}");
    let second = raw(
        &s,
        &owner,
        "pane.spawn",
        off,
        json!({"rid": rid(9), "again_ms": 100}),
    );
    assert_eq!(code(&second), "outcome_unknown", "{second}");
    assert_eq!(panes_named(&s, "off"), 1);
}

/// `spandeck mcp` as an agent's pane: one tool call a line.
struct Tools {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    out: BufReader<std::process::ChildStdout>,
    asked: u64,
}

impl Tools {
    fn start(s: &Station, pane: &str) -> Tools {
        let mut child = std::process::Command::new(SPANDECK)
            .arg("mcp")
            .env("SPANDECK_STATE_DIR", s.root.join("state"))
            .env("SPANDECK_CONFIG_DIR", s.root.join("config"))
            .env("SPANDECK_TOKEN", s.pane_token(pane))
            .env("SPANDECK_PANE", pane)
            .env_remove("SPANDECK_SOCKET")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        Tools {
            stdin: child.stdin.take().unwrap(),
            out: BufReader::new(child.stdout.take().unwrap()),
            child,
            asked: 0,
        }
    }

    fn tell(&mut self, name: &str, args: Value) {
        self.asked += 1;
        let req = json!({"jsonrpc": "2.0", "id": self.asked, "method": "tools/call", "params": {"name": name, "arguments": args}});
        writeln!(self.stdin, "{req}").unwrap();
    }

    /// The answer to the oldest call not yet heard: whether it worked, and
    /// its text.
    fn hear(&mut self) -> (bool, String) {
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        (
            v["result"]["isError"] == json!(false),
            v["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
    }

    fn call(&mut self, name: &str, args: Value) -> (bool, String) {
        self.tell(name, args);
        self.hear()
    }
}

impl Drop for Tools {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn cli(s: &Station, args: &[&str]) -> std::process::Child {
    std::process::Command::new(SPANDECK)
        .args(args)
        .env("SPANDECK_STATE_DIR", s.root.join("state"))
        .env("SPANDECK_CONFIG_DIR", s.root.join("config"))
        .env_remove("SPANDECK_SOCKET")
        .env_remove("SPANDECK_TOKEN")
        .env_remove("SPANDECK_PANE")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

/// The audit's sections 6 and 1: an agent's `send` and `inbox` while the
/// station is restarted in place (as an upgrade does), while its daemon is
/// down for two seconds, and while it stays down past the tool's budget.
/// Every message is stored once and read once, and when spandeck cannot
/// be reached the agent is told which it was.
#[test]
#[ignore = "merge tier: restarts and kills a station"]
fn a_restart_loses_no_tool_call() {
    let mut s = Station::start("alpha");
    let repo = project(&s, "demo");
    let worker = s.fake(
        json!({"steps": []}),
        json!({"name": "worker", "project": "demo", "cwd": repo}),
    );
    s.wait_text(&worker, "[fake] session");
    let mut tools = Tools::start(&s, &worker);
    let (socket, token) = (s.paths().socket(), s.token());
    let mut owner = Client::waiting(&socket, &token, Duration::from_secs(20));
    // A station that has only just started does not vouch for what it has
    // no record of. One that has run a few seconds does.
    std::thread::sleep(Duration::from_secs(4));

    // ---- restarted in place, three times, while the tools are in use
    let started = |s: &Station| s.try_call("station.info", json!({})).ok()?["started_ms"].as_u64();
    // id -> (body, how many plain `inbox` calls returned it)
    let mut read: HashMap<String, (String, u32)> = HashMap::new();
    let mut take = |text: &str, plain: bool| {
        for m in serde_json::from_str::<Vec<Value>>(text).unwrap() {
            let entry = read
                .entry(m["id"].as_str().unwrap().to_string())
                .or_insert((m["body"].as_str().unwrap().to_string(), 0));
            entry.1 += plain as u32;
        }
    };
    let (mut sent, mut received, mut could_not_confirm, mut waited) = (0, 0, 0, 0);
    for round in 0..3 {
        let before = wait_for("the station to answer", 20, || started(&s));
        // A few calls at leisure; then the restart is asked for, and from
        // that moment the tools are called back to back until one of them
        // has found the daemon away (it then takes 25 ms at least).
        let mut asked_restart: Option<Instant> = None;
        let mut waited_now = 0;
        for n in 0.. {
            if n == 8 {
                s.call("station.restart", json!({}));
                asked_restart = Some(Instant::now());
            }
            let tight = asked_restart.is_some();
            if asked_restart.is_some_and(|at| waited_now > 0 || at.elapsed().as_secs() >= 5) {
                break;
            }
            let asked = Instant::now();
            if !tight {
                owner
                    .call(
                        "msg.send",
                        json!({"to": "worker", "body": format!("in-{received}")}),
                    )
                    .unwrap();
                received += 1;
            }
            if !tight || n % 2 == 0 {
                match tools.call("inbox", json!({})) {
                    (true, text) => take(&text, true),
                    // Carried out when the image was replaced, the answer
                    // lost with it: the agent is told to read everything,
                    // and does.
                    (false, text) => {
                        assert!(text.contains("call `inbox` with all set to true"), "{text}");
                        could_not_confirm += 1;
                        let (ok, all) = tools.call("inbox", json!({"all": true}));
                        assert!(ok, "{all}");
                        take(&all, false);
                    }
                }
            }
            if !tight || n % 2 == 1 {
                let (ok, text) =
                    tools.call("send", json!({"to": "you", "body": format!("out-{sent}")}));
                assert!(
                    ok,
                    "send {sent} failed although spandeck only restarted: {text}"
                );
                sent += 1;
            }
            if tight && asked.elapsed() >= Duration::from_millis(25) {
                waited_now += 1;
            }
            // Not so fast that the agent has more requests remembered
            // than a station keeps for one caller (1,024).
            std::thread::sleep(Duration::from_millis(if tight { 2 } else { 12 }));
        }
        // Without this the round would have tested nothing.
        assert!(
            waited_now > 0,
            "round {round}: no tool call was made while the daemon was away"
        );
        waited += waited_now;
        wait_for("the station to be back", 20, || {
            (started(&s)? != before).then_some(())
        });
    }
    let (ok, last) = tools.call("inbox", json!({}));
    assert!(ok, "{last}");
    take(&last, true);
    assert!(s.log("station.upgraded").len() >= 3, "restarted in place");
    let stored = s.call("msg.recent", json!({"limit": 100000}));
    for i in 0..sent {
        assert_eq!(
            stored
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["body"] == json!(format!("out-{i}")))
                .count(),
            1,
            "out-{i} is stored once"
        );
    }
    for i in 0..received {
        let got: Vec<&(String, u32)> = read
            .values()
            .filter(|(body, _)| *body == format!("in-{i}"))
            .collect();
        assert_eq!(got.len(), 1, "in-{i} reached the agent");
        assert!(got[0].1 <= 1, "in-{i} was handed out as unread once");
    }
    eprintln!("{sent} sends and {received} reads across 3 restarts in place; {waited} calls waited for the daemon; {could_not_confirm} reads could not be confirmed");

    // ---- the daemon is killed and is away for two seconds
    owner
        .call("msg.send", json!({"to": "worker", "body": "in-while-down"}))
        .unwrap();
    s.kill9();
    // A command that is told not to wait fails at once, as before.
    let no_wait = cli(&s, &["--no-wait", "send", "you", "not sent"])
        .wait_with_output()
        .unwrap();
    assert!(!no_wait.status.success());
    assert!(String::from_utf8_lossy(&no_wait.stderr).contains("spandeckd is not running"));
    let began = Instant::now();
    tools.tell("send", json!({"to": "you", "body": "out-while-down"}));
    tools.tell("inbox", json!({}));
    let waiting = cli(&s, &["send", "you", "cli-while-down"]);
    std::thread::sleep(Duration::from_secs(2));
    s.launch();
    let (ok, text) = tools.hear();
    assert!(ok, "a send made while the daemon was down 2 s: {text}");
    assert!(began.elapsed() >= Duration::from_secs(2));
    let (ok, text) = tools.hear();
    assert!(ok && text.contains("in-while-down"), "{text}");
    assert_eq!(text.matches("in-while-down").count(), 1);
    let waited = waiting.wait_with_output().unwrap();
    assert!(waited.status.success(), "{waited:?}");
    assert!(String::from_utf8_lossy(&waited.stderr).contains("waiting for spandeckd"));
    for body in ["out-while-down", "cli-while-down"] {
        assert_eq!(messages_saying(&s, body), 1, "{body}");
    }
    assert_eq!(messages_saying(&s, "not sent"), 0);

    // ---- it stays away longer than a tool waits: the agent is told the
    // message was not sent, and it was not.
    s.kill9();
    let (ok, text) = tools.call("send", json!({"to": "you", "body": "never"}));
    assert!(!ok);
    assert_eq!(
        text,
        "spandeck did not answer for 15 s. Your message was NOT sent."
    );
    let (ok, text) = tools.call("context_search", json!({"query": "x"}));
    assert!(!ok && text.contains("try again shortly"), "{text}");
    s.launch();
    assert_eq!(messages_saying(&s, "never"), 0);
}

/// Stands between two stations and can lose the answers to a pane being
/// started: the request reaches the far station and is carried out, and
/// the connection is closed before the answer goes back.
struct Proxy {
    port: u16,
    lose: Arc<AtomicUsize>,
    spawns: Arc<AtomicUsize>,
}

fn http_message(r: &mut BufReader<TcpStream>) -> Option<Vec<u8>> {
    let mut all = Vec::new();
    let mut len = 0;
    loop {
        let mut line = Vec::new();
        if r.read_until(b'\n', &mut line).ok()? == 0 {
            return None;
        }
        all.extend(&line);
        let text = String::from_utf8_lossy(&line).to_lowercase();
        if let Some(v) = text.strip_prefix("content-length:") {
            len = v.trim().parse().ok()?;
        }
        if line == b"\r\n" {
            break;
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    all.extend(body);
    Some(all)
}

impl Proxy {
    fn to(port_of_station: u16) -> Proxy {
        let port = free_port();
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        let (lose, spawns) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (l, n) = (lose.clone(), spawns.clone());
        std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let (lose, spawns) = (l.clone(), n.clone());
                std::thread::spawn(move || {
                    let Ok(station) = TcpStream::connect(("127.0.0.1", port_of_station)) else {
                        return;
                    };
                    let mut from_client = BufReader::new(client.try_clone().unwrap());
                    let mut from_station = BufReader::new(station.try_clone().unwrap());
                    let (mut to_client, mut to_station) = (client, station);
                    while let Some(req) = http_message(&mut from_client) {
                        let text = String::from_utf8_lossy(&req);
                        let spawn = text.contains("rpc.forward") && text.contains("pane.spawn");
                        if spawn {
                            spawns.fetch_add(1, Ordering::SeqCst);
                        }
                        if to_station.write_all(&req).is_err() {
                            return;
                        }
                        let Some(answer) = http_message(&mut from_station) else {
                            return;
                        };
                        // One fewer answer to lose, if any are left. (Written out: the
                        // one-call form has a different name in each compiler
                        // the stations build with.)
                        let lost = spawn && {
                            let mut left = lose.load(Ordering::SeqCst);
                            loop {
                                if left == 0 {
                                    break false;
                                }
                                match lose.compare_exchange(
                                    left,
                                    left - 1,
                                    Ordering::SeqCst,
                                    Ordering::SeqCst,
                                ) {
                                    Ok(_) => break true,
                                    Err(now) => left = now,
                                }
                            }
                        };
                        if lost {
                            return;
                        }
                        if to_client.write_all(&answer).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Proxy { port, lose, spawns }
    }

    fn lose_next(&self, answers: usize) {
        self.lose.store(answers, Ordering::SeqCst);
    }

    fn spawns(&self) -> usize {
        self.spawns.load(Ordering::SeqCst)
    }
}

/// The audit's gap 11, as it was measured: a pane started through another
/// station, the answer lost. With a station that keeps ids it is one pane
/// and the answer arrives; when no answer can be had it is still one pane
/// and the owner is told "could not confirm", not "unreachable". With a
/// station that keeps none (0.4.4, stood in for by the switch), the
/// request is sent once and never again, exactly as before.
#[test]
#[ignore = "merge tier: starts two stations"]
fn a_pane_started_through_another_station_whose_answer_is_lost_is_one_pane() {
    let (pa, pb) = (free_port(), free_port());
    let b = Station::in_fleet("bravo", pb, &[pa], "");
    let proxy = Proxy::to(pb);
    let a = Station::in_fleet("alpha", pa, &[proxy.port], "");
    wait_for("alpha to see bravo", 20, || {
        (a.sees("bravo")? == "online").then_some(())
    });
    let owner = a.token();
    let through = |name: &str| json!({"station": "bravo", "method": "pane.spawn", "params": {"agent": "shell", "name": name}});

    // The answer to the first try is lost. alpha sends the request once
    // more with the same id, and bravo gives the first answer again.
    proxy.lose_next(1);
    let pane = a.call("peer.call", through("once"));
    assert_eq!(pane["name"], json!("once"), "{pane}");
    assert_eq!(panes_named(&b, "once"), 1);
    assert_eq!(proxy.spawns(), 2, "sent twice, carried out once");

    // Both answers are lost. One pane, and the owner is told the truth.
    proxy.lose_next(2);
    let lost = a.try_call("peer.call", through("twice")).unwrap_err();
    assert_eq!(lost.code, "outcome_unknown", "{lost}");
    assert!(lost.message.contains("could not confirm"), "{lost}");
    assert_eq!(panes_named(&b, "twice"), 1);

    // The caller's own second try with its id, as a page that lost its
    // connection will make: the first answer, and still one pane.
    proxy.lose_next(2);
    let first = raw(
        &a,
        &owner,
        "peer.call",
        through("thrice"),
        json!({"rid": rid(1)}),
    );
    assert_eq!(code(&first), "outcome_unknown", "{first}");
    let second = raw(
        &a,
        &owner,
        "peer.call",
        through("thrice"),
        json!({"rid": rid(1), "again_ms": 1500}),
    );
    assert_eq!(second["result"]["name"], json!("thrice"), "{second}");
    assert_eq!(panes_named(&b, "thrice"), 1);

    // A station that keeps no ids. alpha learns so at its next look.
    b.call("station.flags", json!({"set": {"request_ids": false}}));
    a.call("fleet.refresh", json!({}));
    let before = proxy.spawns();
    proxy.lose_next(1);
    let lost = a.try_call("peer.call", through("old")).unwrap_err();
    assert!(lost.message.contains("unreachable"), "as before: {lost}");
    assert_eq!(proxy.spawns() - before, 1, "sent once, and never again");
    assert_eq!(panes_named(&b, "old"), 1);
    // And a caller's second try by id is refused here, never sent there.
    let before = proxy.spawns();
    let second = raw(
        &a,
        &owner,
        "peer.call",
        through("old"),
        json!({"rid": rid(2), "again_ms": 1500}),
    );
    assert_eq!(code(&second), "outcome_unknown", "{second}");
    assert_eq!(proxy.spawns(), before);
    assert_eq!(panes_named(&b, "old"), 1);
}
