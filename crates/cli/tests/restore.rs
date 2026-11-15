//! After a restart, nothing is left wrong and the owner is told
//! (DECISIONS.md, "After a restart"). Merge tier: each test starts a
//! station several times, so none runs in the every-change check.
//!
//!   cargo test -p spandeck --test restore -- --ignored
#![cfg(unix)]
mod common;
use std::path::Path;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

fn alive(pid: i32) -> bool {
    spandeck_terminal::sys::pid_alive(pid)
}

fn pid_in(file: &Path) -> i32 {
    wait_for("a job's pid file", 10, || {
        std::fs::read_to_string(file).ok()?.trim().parse().ok()
    })
}

/// The recovery notices in the station's inbox.
fn recovery_notices(s: &Station) -> Vec<Value> {
    s.call("notify.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == json!("station.recovered"))
        .cloned()
        .collect()
}

/// The audit's section 2 and its gaps 4 and 5, measured on 0.4.2: after
/// `kill -9` of the daemon, 49 of 50 restored agents' first hook failed; a
/// waiting approval stayed "pending" for ever; a task kept the state it
/// had; a job that ignored hang-up ran on in no pane; a shell vanished
/// from the list; nobody was told; and a crash loop would have started
/// every agent at every start.
#[test]
#[ignore = "merge tier: kills and starts a station four times"]
fn after_a_crash_nothing_is_left_wrong() {
    let mut s = Station::start("alpha");
    let home = s.root.join("home");

    // A helper with a task, in a worktree of its own.
    let repo = s.git_repo("demo");
    let works = s.script_file(&json!({"steps": [{"prompt": "build"}, {"work": 600_000}]}));
    std::fs::create_dir_all(repo.join(".spandeck")).unwrap();
    std::fs::write(
        repo.join(".spandeck/project.toml"),
        format!(
            "name = \"demo\"\n[roles.builder]\nagent = \"fake\"\nplacement = \"worktree\"\nargs = [\"--script\", '{}']\n",
            works.display()
        ),
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "add project file"]);
    s.call("project.new", json!({"path": repo}));
    let out = s.call(
        "delegate",
        json!({"role": "builder", "task": "build the importer", "project": "demo"}),
    );
    let task = out["task"].as_str().unwrap().to_string();
    let helper = out["pane"].as_str().unwrap().to_string();
    let worktree = out["workspace"]["path"].as_str().unwrap().to_string();
    let task_state = |s: &Station| s.call("task.get", json!({"task": task}))["state"].clone();
    wait_for("the task to run", 15, || {
        (task_state(&s) == json!("running")).then_some(())
    });

    // One waiting on an approval.
    let asker = s.fake(
        json!({"steps": [{"prompt": "x"}, {"permission": {"name": "Bash", "command": "cargo publish"}}, {"work": 60_000}]}),
        json!({"name": "asker"}),
    );
    let approval = wait_for("the approval", 10, || {
        s.call("approval.list", json!({})).get(0).cloned()
    })["id"]
        .as_str()
        .unwrap()
        .to_string();

    // One whose job ignores the hang-up that ends its agent.
    let trapped_pid = home.join("trapped.pid");
    let trapper = s.fake(
        json!({"steps": [{"sh": format!("(trap '' HUP; exec sleep 240) >/dev/null 2>&1 & echo $! > {}", trapped_pid.display())}]}),
        json!({"name": "trapper"}),
    );
    // Three that are just open, and a shell with a job of its own that the
    // owner detached.
    let idle: Vec<String> = (1..=3)
        .map(|i| s.fake(json!({"steps": []}), json!({"name": format!("idle-{i}")})))
        .collect();
    let shell = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "sh", "cwd": home}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let detached_pid = home.join("detached.pid");
    s.call(
        "pane.send_text",
        json!({"pane": shell, "text": format!("(trap '' HUP; exec sleep 241) >/dev/null 2>&1 & echo $! > {}", detached_pid.display())}),
    );
    let (trapped, detached) = (pid_in(&trapped_pid), pid_in(&detached_pid));

    let mut agents = vec![helper.clone(), asker.clone(), trapper.clone()];
    agents.extend(idle.iter().cloned());
    let sessions: Vec<String> = agents
        .iter()
        .map(|a| {
            wait_for("a session id", 10, || {
                s.pane(a)["session_id"].as_str().map(String::from)
            })
        })
        .collect();
    let restored = |s: &Station| {
        for (a, session) in agents.iter().zip(&sessions) {
            s.wait_text(a, &format!("[fake] RESUMED {session}"));
            wait_for_or(
                "a restored agent's first hook",
                10,
                || s.text(a),
                || s.pane(a)["hooks_seen"].as_bool().filter(|seen| *seen),
            );
            assert!(!s.text(a).contains("hook error"), "{}", s.text(a));
        }
    };

    // ---------------------------------------------------- the first crash
    s.kill9();
    s.launch();
    restored(&s);

    // The approval nobody can answer is expired, not pending for ever.
    let all = s.call("approval.list", json!({"all": true}));
    let was = all
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == json!(approval))
        .unwrap();
    assert_eq!(was["status"], json!("expired"), "{was}");
    assert!(s
        .call("approval.list", json!({}))
        .as_array()
        .unwrap()
        .is_empty());

    // Guarantee (Workspaces): after a crash the worktree and branch are
    // kept, and the task is marked failed with its resume information.
    let t = s.call("task.get", json!({"task": task}));
    assert_eq!(t["state"], json!("failed"), "{t}");
    let resume = &t["resume"];
    assert_eq!(resume["reason"], json!("spandeck restarted"));
    assert_eq!(resume["pane"], json!(helper));
    assert_eq!(resume["session_id"], json!(sessions[0]));
    assert_eq!(resume["worktree"], json!(worktree));
    assert_eq!(resume["branch"], json!(format!("spandeck/{task}")));
    assert!(Path::new(&worktree).is_dir());

    // Rule 7: sending an agent back to a task, and the station's switches,
    // are the owner's. An agent has neither.
    for (method, params) in [
        ("task.continue", json!({"task": task})),
        ("station.flags", json!({})),
        ("station.flags", json!({"set": {"holds.restored": false}})),
    ] {
        let refused = s.call_as_pane(&idle[0], method, params).unwrap_err();
        assert_eq!(refused.code, "denied", "{method}: {}", refused.message);
    }
    // Pause means no new work: a paused pane is not sent back to its task.
    s.call("pane.pause", json!({"pane": helper, "paused": true}));
    let refused = s
        .try_call("task.continue", json!({"task": task}))
        .unwrap_err();
    assert!(refused.message.contains("paused"), "{}", refused.message);
    assert_eq!(task_state(&s), json!("failed"));
    s.call("pane.pause", json!({"pane": helper, "paused": false}));
    // The owner continues it: running again, and the agent is told why.
    let again = s.call("task.continue", json!({"task": task}));
    assert_eq!(again["state"], json!("running"));
    s.wait_text(
        &helper,
        "[fake] got: spandeck: your task was interrupted by a restart at",
    );

    // The job of the agent that was started again is gone; the job the
    // owner detached from a shell that was not started again is not
    // touched, and is named in the notice.
    wait_for("the trapped job to be ended", 5, || {
        (!alive(trapped)).then_some(())
    });
    assert!(alive(detached), "a detached job is the owner's to end");

    // The shell is listed as ended, with the reason, and can be brought back.
    let gone = s.pane(&shell);
    assert_eq!(
        (&gone["exited"], &gone["ended_by"], &gone["held_back"]),
        (&json!(true), &json!("restart"), &json!(false)),
        "{gone}"
    );
    assert!(gone["ended_ms"].as_u64().unwrap() > 0);
    assert!(s.text(&shell).contains("Ended when spandeck restarted at"));

    // One entry in the ship's log, and exactly one notice.
    let logged = s.log("station.recovered");
    assert_eq!(logged.len(), 1, "{logged:?}");
    let d = &logged[0]["data"];
    assert_eq!(
        (
            &d["why"],
            &d["resumed"],
            &d["closed"],
            &d["tasks_failed"],
            &d["approvals_expired"],
            &d["held_back"]
        ),
        (
            &json!("crash"),
            &json!(6),
            &json!(1),
            &json!(1),
            &json!(1),
            &json!(0)
        ),
        "{d}"
    );
    assert_eq!(d["leftovers_ended"], json!(1), "{d}");
    assert_eq!(
        d["leftovers"],
        json!([format!("sleep (pid {detached})")]),
        "{d}"
    );
    assert!(d["away_ms"].as_u64().is_some(), "{d}");
    let notices = recovery_notices(&s);
    assert_eq!(notices.len(), 1, "{notices:?}");
    let title = notices[0]["title"].as_str().unwrap();
    let body = notices[0]["body"].as_str().unwrap();
    assert!(title.starts_with("alpha restarted at "), "{title}");
    assert!(
        body.starts_with(
            "6 sessions were started again, 1 shell ended, 1 task failed. They wait for you."
        ),
        "{body}"
    );
    assert!(
        body.contains(&format!("not touched: sleep (pid {detached}).")),
        "{body}"
    );
    // The Performance page's line is what the station says here.
    let perf = s.call("perf.get", json!({}));
    assert_eq!(
        perf["recovered"]["resumed"],
        json!(6),
        "{}",
        perf["recovered"]
    );

    // "Bring back" is the restart every ended pane has: a new shell.
    s.call("pane.restart", json!({"pane": shell}));
    let back = s.pane(&shell);
    assert_eq!(
        (&back["exited"], &back["ended_by"]),
        (&json!(false), &Value::Null)
    );
    // A pane that was listed and is closed is not listed at the next start.
    let extra = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "sh2", "cwd": home}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();

    // --------------------------------------------------- the second crash
    s.kill9();
    s.launch();
    restored(&s);
    assert_eq!(s.pane(&extra)["ended_by"], json!("restart"));
    s.call("pane.close", json!({"pane": extra}));
    // Still one notice: the one that stands was rewritten.
    let notices = recovery_notices(&s);
    assert_eq!(notices.len(), 1, "{notices:?}");
    let title = notices[0]["title"].as_str().unwrap();
    assert!(
        title.starts_with("alpha restarted 2 times since "),
        "{title}"
    );
    assert_eq!(notices[0]["read"], json!(false));

    // ---------------------------------------------------- the third crash
    // Three starts in ten minutes that followed no clean stop: sessions
    // are listed and none is started.
    s.kill9();
    s.launch();
    assert!(s.try_call("pane.get", json!({"pane": extra})).is_err());
    for a in &agents {
        let p = s.pane(a);
        assert_eq!(
            (&p["exited"], &p["held_back"], &p["pid"]),
            (&json!(true), &json!(true), &json!(0)),
            "{p}"
        );
    }
    assert!(s
        .text(&agents[1])
        .contains("Not started again: spandeck restarted 3 times in 10 minutes"));
    let logged = s.log("station.recovered");
    assert_eq!(logged.len(), 3);
    let d = &logged[0]["data"];
    assert_eq!(
        (&d["resumed"], &d["held_back"]),
        (&json!(0), &json!(6)),
        "{d}"
    );
    let notices = recovery_notices(&s);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0]["title"]
        .as_str()
        .unwrap()
        .starts_with("alpha restarted 3 times since "));
    assert!(
        notices[0]["body"].as_str().unwrap().starts_with(
            "6 sessions were not started again, because spandeck restarted 3 times in 10 minutes"
        ),
        "{}",
        notices[0]["body"]
    );
    // One is brought back by hand, into its conversation.
    s.call("pane.restart", json!({"pane": agents[1]}));
    s.wait_text(&agents[1], &format!("[fake] RESUMED {}", sessions[1]));
    assert_eq!(s.pane(&agents[1])["held_back"], json!(false));

    // ------------------------------------- a clean stop, and a start
    // The count starts again, and the sessions held back are started.
    s.signal(15);
    wait_for("the station to stop", 10, || {
        s.try_call("station.info", json!({})).is_err().then_some(())
    });
    s.kill9();
    s.launch();
    restored(&s);
    let d = &s.log("station.recovered")[0]["data"];
    assert_eq!(
        (&d["why"], &d["resumed"], &d["held_back"]),
        (&json!("stop"), &json!(6), &json!(0)),
        "{d}"
    );

    // The job this test detached is its own to end.
    kill_pid(detached, 9);
}

/// The audit's gap 14: requests to join or sign in were kept in memory
/// only, so an upgrade or a restart in place dropped them and whoever had
/// asked waited for nothing.
#[test]
#[ignore = "merge tier: two stations and a restart in place"]
fn a_join_request_is_still_waiting_after_a_restart_in_place() {
    let (a, b) = Station::strangers(None);
    let asked = b.call("fleet.join", json!({"station": "alpha"}));
    let waiting = a.call("trust.status", json!({}));
    assert_eq!(waiting["requests"][0]["code"], asked["code"]);

    let before = a.call("station.info", json!({}));
    a.call("station.restart", json!({}));
    wait_for("alpha's new image", 15, || {
        let i = a.try_call("station.info", json!({})).ok()?;
        (i["started_ms"] != before["started_ms"]).then_some(())
    });
    let waiting = a.call("trust.status", json!({}));
    let req = &waiting["requests"][0];
    assert_eq!(
        (&req["kind"], &req["name"], &req["code"]),
        (&json!("station"), &json!("bravo"), &asked["code"]),
        "{waiting}"
    );
    // And it can still be answered: bravo collects the key it asked for.
    a.call("trust.approve", json!({"request": asked["code"]}));
    wait_for("bravo to take the fleet key", 20, || {
        (a.call("trust.status", json!({}))["fleet"] == b.call("trust.status", json!({}))["fleet"])
            .then_some(())
    });

    // The owner's new calls reach another station as the owner's forwarded
    // request, from a deck opened on this one.
    wait_for("alpha to see bravo in its fleet", 20, || {
        (a.sees("bravo").as_deref() == Some("online")).then_some(())
    });
    let via = |method: &str, params: Value| {
        a.try_call(
            "peer.call",
            json!({"station": "bravo", "method": method, "params": params}),
        )
    };
    let set = via("station.flags", json!({"set": {"holds.restored": false}})).unwrap();
    assert_eq!(set["flags"]["holds.restored"], json!(false), "{set}");
    assert_eq!(
        b.call("station.flags", json!({}))["set"]["holds.restored"],
        json!(false)
    );
    assert_eq!(
        b.call("perf.get", json!({}))["flags"]["holds.restored"],
        json!(false)
    );
    let no_task = via("task.continue", json!({"task": "t-none"})).unwrap_err();
    assert!(no_task.message.contains("not found"), "{}", no_task.message);
    let info = b.call("station.info", json!({}));
    assert!(
        info["caps"]
            .as_array()
            .unwrap()
            .contains(&json!("task_continue")),
        "{}",
        info["caps"]
    );
    // What is not a flag is refused, and a flag taken back is gone.
    assert!(b
        .try_call("station.flags", json!({"set": {"Not A Flag": true}}))
        .is_err());
    assert!(b
        .try_call("station.flags", json!({"set": {"x": [1, 2]}}))
        .is_err());
    let back = b.call("station.flags", json!({"set": {"holds.restored": null}}));
    assert_eq!(back["flags"], json!({}), "{back}");
}

/// The status of an API call made at `addr` under the name `host`, as the
/// owner on this machine: 200 when that name is answered to there, 403 when
/// it is not. `None`: nothing listens at `addr`.
fn http_status(s: &Station, addr: &str, host: &str) -> Option<u16> {
    use std::io::{Read, Write};
    let mut c =
        std::net::TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_secs(2)).ok()?;
    c.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    let body = r#"{"method":"station.info","params":{}}"#;
    c.write_all(
        format!(
            "POST /api/rpc HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            s.token(),
            body.len()
        )
        .as_bytes(),
    )
    .ok()?;
    let mut answer = String::new();
    let _ = c.read_to_string(&mut answer);
    answer.split_whitespace().nth(1)?.parse().ok()
}

/// The audit's gap 15: the web server bound its addresses once. A station
/// that started before Tailscale was up, or whose Tailscale address
/// changed, was never served there until spandeck was restarted.
///
/// The stand-in `tailscale` gives this machine 127.0.0.2, which only Linux
/// lets a program listen on without setting it up first.
#[test]
#[ignore = "merge tier: waits for two looks at the fleet (ten seconds each)"]
fn a_tailscale_address_that_appears_is_served_and_one_that_goes_is_not() {
    if std::env::consts::OS != "linux" {
        eprintln!("not run here: 127.0.0.2 is ready to listen on only on Linux");
        return;
    }
    let status = |root: &Path, ips: Value| {
        std::fs::write(
            root.join("standin/status.json"),
            json!({"BackendState": "Running",
                "Self": {"ID": "n-alpha", "HostName": "alpha", "DNSName": "alpha.tail0.ts.net.", "UserID": 7, "OS": "linux", "Online": true, "TailscaleIPs": ips},
                "User": {"7": {"LoginName": "owner@example.com"}}, "Peer": {}})
            .to_string(),
        )
        .unwrap();
    };
    let fleet = Station::fleet(&["alpha"], |_, root| {
        let dir = root.join("standin");
        std::fs::create_dir_all(&dir).unwrap();
        status(root, json!([]));
        let program = dir.join("tailscale");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ncase \"$1\" in version) echo 1.0 ;; status) cat {} ;; *) exit 1 ;; esac\n",
                dir.join("status.json").display()
            ),
        )
        .unwrap();
        spandeck_terminal::sys::set_executable(&program).unwrap();
        let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap());
        (
            "discovery = true\nweb_bind = []\n".to_string(),
            vec![("PATH".to_string(), path)],
        )
    });
    let s = &fleet[0];
    let there = format!("127.0.0.2:{}", s.port);
    let web = |s: &Station| s.call("station.info", json!({}))["web"].clone();
    assert_eq!(web(s), json!([format!("127.0.0.1:{}", s.port)]));
    assert_eq!(
        http_status(s, &there, &there),
        None,
        "nothing listens there yet"
    );

    // Tailscale comes up: the address is served within 15 seconds, under
    // its own Host and under the machine's Tailscale name.
    status(&s.root, json!(["127.0.0.2", "fd7a:115c:a1e0::2"]));
    wait_for("the new address to be served", 15, || {
        (http_status(s, &there, &there) == Some(200)).then_some(())
    });
    assert_eq!(
        http_status(s, &there, &format!("alpha.tail0.ts.net:{}", s.port)),
        Some(200)
    );
    // A name that is not this station's is still refused there.
    assert_eq!(http_status(s, &there, "evil.example:80"), Some(403));
    assert!(
        web(s).as_array().unwrap().contains(&json!(there)),
        "{}",
        web(s)
    );

    // The address goes: it is no longer served, and localhost still is.
    status(&s.root, json!([]));
    wait_for("the address to be let go", 15, || {
        http_status(s, &there, &there).is_none().then_some(())
    });
    assert_eq!(web(s), json!([format!("127.0.0.1:{}", s.port)]));
    let here = format!("127.0.0.1:{}", s.port);
    assert_eq!(http_status(s, &here, &here), Some(200));
    // The address it no longer has is not a name it answers to either.
    assert_eq!(http_status(s, &here, &there), Some(403));
}

/// The audit's gap 5: a shell that ended with a daemon crash vanished from
/// the list, and a terminal open on it asked for it every 3 seconds for
/// ever. In the Control Deck it now says why it ended, and brings it back.
#[test]
#[ignore = "merge tier: drives a browser across a crash"]
fn the_deck_says_why_a_pane_ended_and_brings_it_back() {
    let mut s = Station::start("alpha");
    let shell = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "sh", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.kill9();
    s.launch();
    let page = browser::Page::open(&s, 1200, 800);
    page.goto(&format!("#/s/alpha/p/{shell}"));
    let note = "document.querySelector('.term-note')";
    let said = page.wait(
        "the reason on the pane",
        &format!(
            "(n => n && !n.hidden && n.innerText.includes('Bring back') && n.innerText)({note})"
        ),
    );
    assert!(
        said.as_str()
            .unwrap()
            .starts_with("Ended when spandeck restarted at "),
        "{said}"
    );
    assert!(
        page.js("document.querySelector('.strip').innerText")
            .as_str()
            .unwrap()
            .contains("ended when spandeck restarted at "),
        "the strip says it too"
    );
    page.js(&format!("{note}.querySelector('button').click()"));
    wait_for("the shell to be brought back", 10, || {
        (s.pane(&shell)["exited"] == json!(false)).then_some(())
    });
    page.wait("the note to go", &format!("{note}.hidden"));
    // And it is a shell that works.
    s.call(
        "pane.send_text",
        json!({"pane": shell, "text": "echo back-$((40+2))"}),
    );
    wait_for("the new shell on the page", 10, || {
        page.term_text().contains("back-42").then_some(())
    });
}
