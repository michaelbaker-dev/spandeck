//! Nothing typed, stopped or answered waits for the database (DECISIONS.md,
//! "Nothing waits for the database"). Merge tier: each test holds a
//! station's database locked from another process for many seconds, as the
//! audit did (FAILURES §9, `s9_resource.py`), so none runs in the
//! every-change check.
//!
//!   cargo test -p spandeck --test dblock -- --ignored
#![cfg(unix)]
mod common;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

/// Another program holding the station's database for writing.
struct Lock(Option<rusqlite::Connection>);

impl Lock {
    fn take(s: &Station) -> Lock {
        let conn = rusqlite::Connection::open(s.paths().db()).unwrap();
        conn.busy_timeout(Duration::from_secs(10)).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        Lock(Some(conn))
    }

    fn release(&mut self) {
        if let Some(conn) = self.0.take() {
            conn.execute_batch("COMMIT").unwrap();
        }
    }
}

/// What the station's database holds, read by another program.
fn in_db<T: rusqlite::types::FromSql>(s: &Station, sql: &str) -> Option<T> {
    let conn = rusqlite::Connection::open_with_flags(
        s.paths().db(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    conn.query_row(sql, [], |r| r.get(0)).ok()
}

/// Run `f` and say how long it took.
fn timed<T>(what: &str, took: &mut Vec<(String, Duration)>, f: impl FnOnce() -> T) -> T {
    let began = Instant::now();
    let out = f();
    took.push((what.to_string(), began.elapsed()));
    out
}

fn database_figures(s: &Station) -> Value {
    s.call("perf.get", json!({}))["database"].clone()
}

/// FAILURES §9 and gap 12, measured on 0.4.2 with the database locked by
/// another program for 12 s: one keystroke took 10 s, a hook 10.3 s,
/// starting a pane 10 s, and that pane's record was never written. And
/// REVIEW2, "Acknowledged before durable": a pause, a stop and an answer
/// to an approval were reported done while only queued.
///
/// Before the fix every step below marked "at once" took 5 to 10 seconds.
#[test]
#[ignore = "merge tier: holds a station's database locked for 12 seconds"]
fn nothing_typed_stopped_or_answered_waits_for_a_locked_database() {
    let s = Station::start("alpha");
    let cat = s.call("pane.spawn", json!({"program": "/bin/cat"}))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let agent = s.fake(json!({"steps": []}), json!({}));
    let agent_token = s.pane_token(&agent);
    let as_agent = |method: &str, params: Value| {
        let mut c =
            spandeck_core::client::Client::connect(&s.paths().socket(), &agent_token).unwrap();
        c.set_timeout(Some(Duration::from_secs(60)));
        c.call(method, params)
    };
    wait_for("the database to have taken everything", 10, || {
        (database_figures(&s)["waiting"] == json!(0)
            && database_figures(&s)["log_waiting"] == json!(0))
        .then_some(())
    });

    let mut lock = Lock::take(&s);
    let locked = Instant::now();
    let mut took: Vec<(String, Duration)> = vec![];

    // A keystroke is echoed.
    timed("a keystroke and its echo", &mut took, || {
        s.call(
            "pane.input",
            json!({"pane": cat, "data": "typed-while-locked\r"}),
        );
        s.wait_text(&cat, "typed-while-locked");
    });
    // A hook answers: one that is logged, and captured in project memory.
    timed("a hook", &mut took, || {
        as_agent(
            "hook.ingest",
            json!({"agent": "fake", "payload": {"hook_event_name": "PostToolUse", "session_id": "s-locked",
                "tool_name": "Bash", "tool_input": {"command": "cargo test"}, "tool_response": {"exit_code": 0}}}),
        )
        .unwrap()
    });
    // A pane is started, and says its record is not saved.
    let started = timed("starting a pane (told: not saved)", &mut took, || {
        s.call(
            "pane.spawn",
            json!({"program": "/bin/cat", "name": "born-locked"}),
        )
    });
    assert_eq!(started["saved"], json!(false), "{started}");
    let started = started["id"].as_str().unwrap().to_string();
    assert_eq!(
        in_db::<i64>(
            &s,
            &format!("SELECT COUNT(*) FROM panes WHERE id='{started}'")
        ),
        Some(0),
        "the database is locked: its record cannot be there yet"
    );
    // Reads answer, and show what was just done although none of it is written.
    timed("lists", &mut took, || {
        assert!(s
            .call("pane.list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == json!(started)));
        let typed = s.call("log.query", json!({"kind": "pane.typed"}));
        assert_eq!(typed.as_array().unwrap().len(), 1, "{typed}");
    });

    // An agent asks for permission; the request is listed, the owner
    // answers, the agent has its answer, and the owner is told it is not
    // saved yet. It never shows as waiting again.
    let hook = std::thread::scope(|scope| {
        let asking = scope.spawn(|| {
            as_agent(
                "hook.ingest",
                json!({"agent": "fake", "payload": {"hook_event_name": "PermissionRequest", "session_id": "s-locked",
                    "tool_name": "Bash", "tool_input": {"command": "rm -rf build"}}}),
            )
        });
        let waiting = timed("an approval being listed", &mut took, || {
            wait_for("the approval", 10, || {
                s.call("approval.list", json!({})).get(0).cloned()
            })
        });
        assert_eq!(waiting["pane"], json!(agent), "{waiting}");
        let answered = timed("answering an approval (told: not saved)", &mut took, || {
            s.call(
                "approval.answer",
                json!({"id": waiting["id"], "allow": true}),
            )
        });
        assert_eq!(answered["saved"], json!(false), "{answered}");
        assert_eq!(s.call("approval.list", json!({})), json!([]));
        let all = s.call("approval.list", json!({"all": true}));
        assert_eq!(
            (&all[0]["status"], &all[0]["answered_by"], &all[0]["saved"]),
            (&json!("allowed"), &json!("you"), &json!(false)),
            "{all}"
        );
        (
            waiting["id"].as_str().unwrap().to_string(),
            asking.join().unwrap(),
        )
    });
    let (approval, decision) = hook;
    let decision = decision.unwrap().to_string();
    assert!(decision.contains("allow"), "{decision}");

    // Pause holds at once, and says it is not saved.
    let paused = timed("pause (told: not saved)", &mut took, || {
        s.call("pane.pause", json!({"pane": agent, "paused": true}))
    });
    assert_eq!(paused["saved"], json!(false), "{paused}");
    assert_eq!(s.pane(&agent)["paused"], json!(true));

    // Stop is applied, and its row says it is not saved.
    let stop = timed("stop (told: not saved)", &mut took, || {
        s.call("stop.all", json!({"scope": "station"}))
    });
    let row = &stop["stops"][0];
    assert_eq!(
        (&row["status"], &row["saved"]),
        (&json!("confirmed"), &json!(false)),
        "{stop}"
    );

    // A message must be stored before its sender is answered: it is
    // refused, the sender is told why, and nothing is stored.
    let refused = timed("a message (waits 5 s, then is refused)", &mut took, || {
        s.try_call("msg.send", json!({"to": agent, "body": "while locked"}))
    })
    .unwrap_err();
    assert!(
        refused.message.contains("database is locked"),
        "{}",
        refused.message
    );
    // The station answered other things while that one waited.
    let figures = database_figures(&s);
    assert!(figures["waiting"].as_u64().unwrap() >= 3, "{figures}");
    assert_eq!(figures["healthy"], json!(true), "a short lock is no notice");

    // As long as the audit held it.
    let held = locked.elapsed();
    if held < Duration::from_secs(12) {
        std::thread::sleep(Duration::from_secs(12) - held);
    }
    // Still typing.
    timed("a keystroke after 12 s", &mut took, || {
        s.call("pane.input", json!({"pane": cat, "data": "still-here\r"}));
        s.wait_text(&cat, "still-here");
    });
    for (what, t) in &took {
        println!("{:>8.1} ms  {what}", t.as_secs_f64() * 1000.0);
    }
    // Generous, for a busy machine: each of these took 5 to 10 s before.
    // Those that say "not saved" wait one second for the database by
    // design; the message waits its five.
    for (what, t) in &took {
        let limit = match what.as_str() {
            w if w.contains("message") => 9.0,
            w if w.contains("not saved") => 3.0,
            _ => 1.5,
        };
        assert!(t.as_secs_f64() < limit, "{what} took {t:?}");
    }

    lock.release();
    // Everything that was held is written: the pane started meanwhile,
    // the pause, who answered the approval, the stop, the log.
    wait_for("the database to take what waited", 15, || {
        (database_figures(&s)["waiting"] == json!(0)
            && database_figures(&s)["log_waiting"] == json!(0))
        .then_some(())
    });
    assert_eq!(
        in_db::<i64>(
            &s,
            &format!("SELECT COUNT(*) FROM panes WHERE id='{started}' AND closed IS NULL")
        ),
        Some(1)
    );
    assert_eq!(
        in_db::<i64>(
            &s,
            &format!("SELECT json_extract(meta,'$.paused') FROM panes WHERE id='{agent}'")
        ),
        Some(1)
    );
    assert_eq!(
        in_db::<String>(
            &s,
            &format!("SELECT status || ' by ' || answered_by FROM approvals WHERE id='{approval}'")
        )
        .as_deref(),
        Some("allowed by you")
    );
    assert_eq!(
        in_db::<String>(&s, "SELECT status FROM stops").as_deref(),
        Some("confirmed")
    );
    assert_eq!(
        in_db::<i64>(
            &s,
            "SELECT COUNT(*) FROM messages WHERE body='while locked'"
        ),
        Some(0)
    );
    // The ship's log lost nothing, and reads the same from the database
    // as it did from memory.
    let typed = s.log("pane.typed").len() as i64;
    assert!(typed >= 1);
    assert_eq!(
        in_db::<i64>(&s, "SELECT COUNT(*) FROM log WHERE kind='pane.typed'"),
        Some(typed)
    );
    assert_eq!(
        in_db::<i64>(&s, "SELECT COUNT(*) FROM notes WHERE kind='auto'"),
        Some(0),
        "a pane in no project captures nothing"
    );
    let stops = s.call("stop.list", json!({}));
    assert_eq!(stops[0]["saved"], json!(true), "{stops}");
    let figures = database_figures(&s);
    assert_eq!(
        (&figures["log_dropped"], &figures["state_dropped"]),
        (&json!(0), &json!(0)),
        "{figures}"
    );
    assert!(figures["failures"].as_u64().unwrap() >= 1, "{figures}");
    // And a message is taken again.
    s.call("msg.send", json!({"to": agent, "body": "after"}));
}

/// REVIEW2: "nothing says the queue is flushed or handed over before an
/// in-place upgrade"; and "a crash loses it" of anything acknowledged
/// before it is durable. Two things, one station:
///
/// 1. A restart in place while the database is locked: the record of a
///    pane started during the lock, and a pause, are handed to the new
///    image and written by it.
/// 2. What `saved: false` means: the station is killed before the
///    database takes a pause; the pane comes back not paused. That loss is
///    the one the owner was told of, and no other.
#[test]
#[ignore = "merge tier: a restart in place and a kill with the database locked"]
fn what_the_database_had_not_taken_survives_an_upgrade_and_is_lost_only_as_said() {
    let mut s = Station::start("alpha");
    let agent = s.fake(json!({"steps": []}), json!({}));
    wait_for("the database to have taken everything", 10, || {
        (database_figures(&s)["waiting"] == json!(0)).then_some(())
    });

    let mut lock = Lock::take(&s);
    let born = s.call(
        "pane.spawn",
        json!({"program": "/bin/cat", "name": "born-locked"}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let paused = s.call("pane.pause", json!({"pane": agent, "paused": true}));
    assert_eq!(paused["saved"], json!(false), "{paused}");
    let before = s.call("station.info", json!({}));
    s.call("station.restart", json!({}));
    wait_for("the new image", 30, || {
        let i = s.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(())
    });
    // The new image holds what the old one could not write, and both
    // panes were kept.
    assert!(
        database_figures(&s)["waiting"].as_u64().unwrap() >= 2,
        "{}",
        database_figures(&s)
    );
    assert_eq!(s.pane(&agent)["paused"], json!(true));
    assert_eq!(s.pane(&born)["exited"], json!(false));
    assert_eq!(
        in_db::<i64>(&s, &format!("SELECT COUNT(*) FROM panes WHERE id='{born}'")),
        Some(0)
    );
    lock.release();
    wait_for("the new image to write it", 15, || {
        (database_figures(&s)["waiting"] == json!(0)).then_some(())
    });
    assert_eq!(
        in_db::<i64>(
            &s,
            &format!("SELECT COUNT(*) FROM panes WHERE id='{born}' AND closed IS NULL")
        ),
        Some(1)
    );
    assert_eq!(
        in_db::<i64>(
            &s,
            &format!("SELECT json_extract(meta,'$.paused') FROM panes WHERE id='{agent}'")
        ),
        Some(1)
    );
    let upgraded = s.log("station.upgraded");
    assert_eq!(upgraded.len(), 1, "{upgraded:?}");

    // 2. Unpause, saved. Then pause with the database locked, and the
    // station is killed before it can be written.
    let unpaused = s.call("pane.pause", json!({"pane": agent, "paused": false}));
    assert_eq!(unpaused["saved"], json!(true), "{unpaused}");
    let mut lock = Lock::take(&s);
    let paused = s.call("pane.pause", json!({"pane": agent, "paused": true}));
    assert_eq!(paused["saved"], json!(false), "the owner is told: {paused}");
    s.kill9();
    lock.release();
    s.launch();
    s.wait_ready();
    let after = wait_for("the pane to be listed again", 20, || {
        s.try_call("pane.get", json!({"pane": agent})).ok()
    });
    assert_eq!(
        after["paused"],
        json!(false),
        "what was not saved is what was lost: {after}"
    );
}

/// REVIEW2: "a full disk grows memory for ever with no notice". A
/// database that takes nothing for 30 seconds: the owner is told once,
/// the Performance page's figures say so, what is only worth doing if it
/// is stored is refused at once, and Stop, Pause and typing go on. When
/// the database is back the station says so and takes messages again.
#[test]
#[ignore = "merge tier: holds a station's database locked for over 30 seconds"]
fn a_station_that_cannot_write_its_database_says_so_and_still_stops() {
    let s = Station::start("alpha");
    let cat = s.call("pane.spawn", json!({"program": "/bin/cat"}))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let agent = s.fake(json!({"steps": []}), json!({}));
    wait_for("the database to have taken everything", 10, || {
        (database_figures(&s)["waiting"] == json!(0)).then_some(())
    });
    let mut lock = Lock::take(&s);
    // Something for the writer to fail at.
    s.call("pane.pause", json!({"pane": agent, "paused": true}));
    wait_for("the station to say it cannot write", 45, || {
        (database_figures(&s)["healthy"] == json!(false)).then_some(())
    });
    let notices = wait_for("the notice", 5, || {
        let all = s.call("notify.list", json!({}));
        let mine: Vec<Value> = all
            .as_array()?
            .iter()
            .filter(|n| n["kind"] == json!("station.database"))
            .cloned()
            .collect();
        (!mine.is_empty()).then_some(mine)
    });
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(
        notices[0]["title"],
        json!("alpha cannot write its database")
    );

    let began = Instant::now();
    let refused = s
        .try_call("msg.send", json!({"to": agent, "body": "now?"}))
        .unwrap_err();
    assert!(
        refused.message.contains("cannot write its database"),
        "{}",
        refused.message
    );
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "refused at once, not after waiting for the lock: {:?}",
        began.elapsed()
    );
    assert!(s
        .try_call("pane.spawn", json!({"program": "/bin/cat"}))
        .is_err());
    // Stop, Pause and typing are never refused for this.
    s.call(
        "pane.input",
        json!({"pane": cat, "data": "typed-unhealthy\r"}),
    );
    s.wait_text(&cat, "typed-unhealthy");
    let unpaused = s.call("pane.pause", json!({"pane": agent, "paused": false}));
    assert_eq!(unpaused["saved"], json!(false));
    let stop = s.call("stop.all", json!({"scope": "station"}));
    assert_eq!(stop["stops"][0]["status"], json!("confirmed"), "{stop}");
    s.call("pane.kill", json!({"pane": cat}));

    lock.release();
    wait_for("the station to say it can write again", 15, || {
        (database_figures(&s)["healthy"] == json!(true)
            && database_figures(&s)["waiting"] == json!(0))
        .then_some(())
    });
    s.call("msg.send", json!({"to": agent, "body": "after"}));
    let said = wait_for("both entries in the ship's log", 10, || {
        let said = s.log("station.database");
        (said.len() == 2).then_some(said)
    });
    assert_eq!(
        (&said[1]["data"]["writes"], &said[0]["data"]["writes"]),
        (&json!(false), &json!(true)),
        "{said:?}"
    );
    // Told once.
    let notices = s.call("notify.list", json!({}));
    assert_eq!(
        notices
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["kind"] == json!("station.database"))
            .count(),
        1
    );
    assert_eq!(
        in_db::<i64>(
            &s,
            &format!("SELECT json_extract(meta,'$.paused') FROM panes WHERE id='{agent}'")
        ),
        Some(0)
    );

    // The switch: off, a change is written by its caller before the call
    // returns (no wait for it to appear); taken back, the writer again.
    let off = s.call("station.flags", json!({"set": {"db_writer": false}}));
    assert_eq!(off["flags"]["db_writer"], json!(false), "{off}");
    assert_eq!(database_figures(&s)["queued"], json!(false));
    s.call("pane.pause", json!({"pane": agent, "paused": true}));
    assert_eq!(
        in_db::<i64>(
            &s,
            &format!("SELECT json_extract(meta,'$.paused') FROM panes WHERE id='{agent}'")
        ),
        Some(1)
    );
    s.call("station.flags", json!({"set": {"db_writer": null}}));
    assert_eq!(database_figures(&s)["queued"], json!(true));
}
