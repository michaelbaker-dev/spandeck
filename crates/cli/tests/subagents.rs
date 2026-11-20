//! Sub-agents in a drawer of the agent's pane, for every program
//! (DECISIONS.md, "Sub-agents are shown in a drawer of the agent's pane,
//! for every program"; `docs/conversation-api.md`, `tasks`). The owner, 20
//! Nov 2026: "I suppose having subagent view, could be inside a agent pane
//! that can pop in and out when you want to see what sub agents are
//! working on?"
//!
//! The stand-in agent writes its session file and sends its hooks in the
//! shapes of Claude Code 2.1.329 and codex-cli 0.170.0; the files a real
//! program keeps for each sub-agent are written here in those programs'
//! own layouts, with made-up words. Nothing here runs a real program.
//!
//! The two station scenarios run with every change. The one in a browser
//! is the merge tier:
//!
//!   cargo test -p spandeck --test subagents -- --ignored
mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::{json, Value};

const LAYOUT: &str = "a1000000000000001";
const COLOUR: &str = "a2000000000000002";
const REVIEW: &str = "a3000000000000003";
const COUNT: &str = "a4000000000000004";
const NESTED: &str = "b1000000000000001";

fn folder(s: &Station, name: &str) -> PathBuf {
    let dir = s.root.join("home").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn pane(s: &Station, agent: &str, name: &str, dir: &Path, script: Value) -> String {
    let path = s.script_file(&script);
    s.call(
        "pane.spawn",
        json!({"agent": agent, "name": name, "program": FAKE, "args": ["--script", path], "cwd": dir}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn live(s: &Station, pane: &str) -> Value {
    s.call("pane.conversation", json!({"pane": pane, "screen": true}))["live"].clone()
}

/// Every row of a pane's tasks, running first.
fn rows(s: &Station, pane: &str) -> Vec<Value> {
    let l = live(s, pane);
    let mut rows = l["tasks"]["running"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    rows.extend(
        l["tasks"]["finished"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    rows
}

fn row(rows: &[Value], name: &str) -> Value {
    rows.iter()
        .find(|r| r["name"] == json!(name))
        .cloned()
        .unwrap_or(Value::Null)
}

fn lines(lines: &[Value]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// An agent in the middle of a turn in which it began four sub-agents:
/// three in the background (one still at work, one that failed, one that
/// reported back) and one it waits for, whose call has no result yet. Its
/// hooks tell of each by the program's own id, and of a helper of the
/// program's own, which is nobody's sub-agent. The next message typed
/// ends the turn: the one in the background is then over too.
fn delegating() -> Value {
    let call = |id: &str, kind: &str, asked: &str, background: bool| {
        json!({"transcript": {"type": "assistant", "message": {"role": "assistant",
        "content": [{"type": "tool_use", "id": id, "name": "Agent", "input": {"description": asked, "prompt": "Do it.", "subagent_type": kind, "run_in_background": background}}]}}})
    };
    let launched = |id: &str, agent: &str| {
        json!({"transcript": {"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "Async agent launched."}]},
            "toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": agent}}})
    };
    let over = |id: &str, agent: &str, status: &str| json!({"transcript": {"type": "user", "message": {"role": "user", "content": format!("<task-notification>\n<task-id>{agent}</task-id>\n<tool-use-id>{id}</tool-use-id>\n<status>{status}</status>\n<summary>Agent {status}</summary>\n</task-notification>")}, "origin": {"kind": "task-notification"}}});
    let hook = |kind: &str, id: &str, stop: bool| json!({"subagent": {"type": kind, "id": id, "stop": stop}});
    json!({"steps": [
        {"prompt": "Fix the three wide shots and match the hull"},
        call("toolu_l", "layout", "Redo wide shots 3, 7 and 9", true), launched("toolu_l", LAYOUT), hook("layout", LAYOUT, false),
        call("toolu_c", "colour", "Match hull to <img src=x onerror=window.pwned=1> ref sheet", true), launched("toolu_c", COLOUR), hook("colour", COLOUR, false),
        call("toolu_r", "review", "Check continuity", true), launched("toolu_r", REVIEW), hook("review", REVIEW, false),
        hook("", "a9000000000000009", false),
        over("toolu_r", REVIEW, "completed"), hook("review", REVIEW, true),
        over("toolu_c", COLOUR, "failed"), hook("colour", COLOUR, true),
        call("toolu_w", "general-purpose", "Count the frames", false), hook("general-purpose", COUNT, false),
        {"say": "[fake] delegated"}],
      "on_input": [
        over("toolu_l", LAYOUT, "completed"), hook("layout", LAYOUT, true),
        {"turn_end": "ok"}]})
}

/// Where the stand-in agent keeps a pane's session file.
fn session_file(s: &Station, pane: &str) -> PathBuf {
    let session = s.call("pane.conversation", json!({"pane": pane, "screen": true}))["session"]
        .as_str()
        .unwrap()
        .to_string();
    let mut dirs = vec![s.root.clone()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                dirs.push(e.path());
            } else if e.file_name().to_string_lossy() == format!("{session}.jsonl") {
                return e.path();
            }
        }
    }
    panic!("no session file for {pane}");
}

fn side_said(agent: &str, text: &str) -> Value {
    json!({"type": "assistant", "isSidechain": true, "agentId": agent,
        "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}})
}

fn side_ran(agent: &str, id: &str, command: &str) -> Value {
    json!({"type": "assistant", "isSidechain": true, "agentId": agent,
        "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}]}})
}

/// What Claude Code 2.1.329 keeps beside a session for each sub-agent:
/// `subagents/agent-<id>.jsonl`, begun with what it was asked, and
/// `agent-<id>.meta.json`.
fn side(dir: &Path, agent: &str, meta: Value, asked: &str, then: &[Value]) {
    std::fs::create_dir_all(dir).unwrap();
    let mut all = vec![
        json!({"type": "user", "isSidechain": true, "agentId": agent,
            "message": {"role": "user", "content": asked}}),
    ];
    all.extend(then.iter().cloned());
    std::fs::write(dir.join(format!("agent-{agent}.jsonl")), lines(&all)).unwrap();
    std::fs::write(
        dir.join(format!("agent-{agent}.meta.json")),
        meta.to_string(),
    )
    .unwrap();
}

/// The files of the four, and of one that the first began itself.
fn write_sides(s: &Station, pane: &str) -> PathBuf {
    let dir = session_file(s, pane).with_extension("").join("subagents");
    let meta = |kind: &str, asked: &str, call: &str| {
        json!({"agentType": kind, "isBuiltIn": false, "launchedWithDefaults": true, "description": asked,
            "toolUseId": call, "spawnDepth": 1, "requestShape": "x", "requestNonInteractive": false})
    };
    side(
        &dir,
        LAYOUT,
        meta("layout", "Redo wide shots 3, 7 and 9", "toolu_l"),
        "Redo wide shots 3, 7 and 9 with the second-round layout; report which changed.",
        &[
            side_said(LAYOUT, "Shot 3 uses the old camera height."),
            side_ran(LAYOUT, "toolu_s1", "python -m studio.layout --shot 3"),
        ],
    );
    let mut done = meta("review", "Check continuity", "toolu_r");
    done["finished"] = json!(true);
    side(
        &dir,
        REVIEW,
        done,
        "Check continuity across the three shots.",
        &[side_said(REVIEW, "Two findings: shots 3 and 9.")],
    );
    // The one that is waited for: its call has no result in the session
    // file, and this file names the call.
    side(
        &dir,
        COUNT,
        meta("general-purpose", "Count the frames", "toolu_w"),
        "Count the frames of each shot.",
        &[side_said(COUNT, "Shot 3 has 240 frames.")],
    );
    let mut nested = meta("Explore", "Find shot 7's camera", "toolu_n1");
    nested["spawnDepth"] = json!(2);
    nested["parentAgentId"] = json!(LAYOUT);
    side(
        &dir,
        NESTED,
        nested,
        "Find the camera of shot 7.",
        &[side_ran(NESTED, "toolu_s2", "grep -n camera shots/7.json")],
    );
    dir
}

/// The owner's request of 20 Nov 2026, and its mock-up: each sub-agent is
/// a row with how it is (working, done, failed, stopped), what it is
/// called, what it was asked, since when, and "Now: <its latest step>";
/// its own conversation can be read; one a sub-agent began is under it.
///
/// What the station serves for it, for an agent that keeps its records as
/// Claude Code does: rows from the session file, tied to the program's
/// own file for each sub-agent before any result is written (the gap
/// DECISIONS named on 17 Nov: "a sub-agent that was waited for has ... no
/// 'View transcript'"), the step read from the end of that file, and
/// which rows are of the turn now. A helper of the program's own is not a
/// row, and no id can name another file.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn an_agents_sub_agents_are_rows_that_say_how_each_is_and_what_it_does_now() {
    let s = Station::start("alpha");
    let lead = pane(&s, "fake", "lead", &folder(&s, "mere"), delegating());
    s.wait_text(&lead, "[fake] delegated");

    // Before the program has written a file for any: what the session
    // file and the hooks say, and no more.
    let all = wait_for_or(
        "the four rows",
        10,
        || format!("{:?}", rows(&s, &lead)),
        || {
            let all = rows(&s, &lead);
            (all.len() == 4).then_some(all)
        },
    );
    let told = |r: &Value| {
        json!([
            r["state"],
            r["title"],
            r["transcript"],
            r["current"],
            r["depth"]
        ])
    };
    assert_eq!(
        (
            told(&row(&all, "layout")),
            told(&row(&all, "general-purpose")),
            told(&row(&all, "review")),
            told(&row(&all, "colour"))
        ),
        (
            json!(["working", "Redo wide shots 3, 7 and 9", false, true, 1]),
            json!(["working", "Count the frames", false, true, 1]),
            json!(["done", "Check continuity", false, true, 1]),
            json!([
                "failed",
                "Match hull to <img src=x onerror=window.pwned=1> ref sheet",
                false,
                true,
                1
            ])
        ),
        "{all:?}"
    );
    assert!(all.iter().all(|r| r["now"].is_null()), "{all:?}");
    // The hooks' own list is as it was: the two that run, by their ids.
    let l = live(&s, &lead);
    assert_eq!(
        l["subagents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![LAYOUT, COUNT]
    );

    // The program's own files: each row's step, the waited-for one tied
    // to its file, and the one a sub-agent began.
    let dir = write_sides(&s, &lead);
    let all = wait_for("the rows with their steps", 15, || {
        let all = rows(&s, &lead);
        (row(&all, "layout")["now"].is_string() && row(&all, "Explore")["now"].is_string())
            .then_some(all)
    });
    assert_eq!(all.len(), 5, "{all:?}");
    let layout = row(&all, "layout");
    assert_eq!(
        (
            &layout["now"],
            &layout["transcript"],
            &row(&all, "general-purpose")["now"],
            &row(&all, "general-purpose")["transcript"],
            &row(&all, "review")["said"],
            &row(&all, "colour")["transcript"]
        ),
        (
            &json!("Bash · python -m studio.layout --shot 3"),
            &json!(true),
            &json!("Shot 3 has 240 frames."),
            &json!(true),
            &json!("Two findings: shots 3 and 9."),
            &json!(false)
        ),
        "{all:?}"
    );
    let nested = row(&all, "Explore");
    assert_eq!(
        json!([
            nested["id"],
            nested["parent"],
            nested["depth"],
            nested["state"],
            nested["title"],
            nested["now"],
            nested["current"]
        ]),
        json!([
            format!("sub:{NESTED}"),
            layout["id"],
            2,
            "working",
            "Find shot 7's camera",
            "Bash · grep -n camera shots/7.json",
            true
        ])
    );

    // Its step, as its record grows.
    let file = dir.join(format!("agent-{LAYOUT}.jsonl"));
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str(&lines(&[
        json!({"type": "user", "isSidechain": true, "agentId": LAYOUT, "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_s1", "content": "ok"}]}}),
        side_ran(LAYOUT, "toolu_s3", "python -m studio.layout --shot 7"),
    ]));
    std::fs::write(&file, text).unwrap();
    wait_for("the next step", 10, || {
        (row(&rows(&s, &lead), "layout")["now"] == json!("Bash · python -m studio.layout --shot 7"))
            .then_some(())
    });

    // Each one's own conversation: the waited-for one's, and the nested.
    let read = |task: &Value| {
        let side = s.call(
            "pane.conversation.detail",
            json!({"pane": lead, "task": task}),
        );
        side["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                i["text"]
                    .as_str()
                    .or(i["summary"].as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        read(&row(&all, "general-purpose")["id"]),
        vec!["Count the frames of each shot.", "Shot 3 has 240 frames."]
    );
    assert_eq!(
        read(&nested["id"]),
        vec!["Find the camera of shot 7.", "grep -n camera shots/7.json"]
    );
    // No id names another file, and one with no file has none to read.
    std::fs::write(dir.join("../../outside.jsonl"), "{}\n").unwrap();
    let failed = row(&all, "colour")["id"].as_str().unwrap().to_string();
    for bad in [
        "sub:../../outside",
        "sub:zz",
        "hook:../../outside",
        &format!("hook:{REVIEW}"),
        &format!("sub:{REVIEW}x"),
        &failed,
    ] {
        let e = s
            .try_call(
                "pane.conversation.detail",
                json!({"pane": lead, "task": bad}),
            )
            .unwrap_err();
        assert_eq!(e.code, "not_found", "{bad}: {e}");
    }

    // The nested one is over when the program writes so.
    let meta = dir.join(format!("agent-{NESTED}.meta.json"));
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&meta).unwrap()).unwrap();
    v["finished"] = json!(true);
    std::fs::write(&meta, v.to_string()).unwrap();
    wait_for("the nested one over", 10, || {
        (row(&rows(&s, &lead), "Explore")["state"] == json!("done")).then_some(())
    });

    // The next message begins another turn: the one that was waited for
    // never returned, the one in the background reports, and none of
    // them is of the turn now.
    s.call(
        "pane.paste",
        json!({"pane": lead, "text": "and the hull", "enter": true}),
    );
    s.wait_state(&lead, "done");
    let all = wait_for("the turn after", 10, || {
        let all = rows(&s, &lead);
        all.iter()
            .all(|r| r["current"] == json!(false))
            .then_some(all)
    });
    assert_eq!(
        (
            &row(&all, "layout")["state"],
            &row(&all, "general-purpose")["state"],
            &row(&all, "colour")["state"],
            live(&s, &lead)["tasks"]["running"]
                .as_array()
                .unwrap()
                .len(),
            live(&s, &lead)["subagents"].as_array().unwrap().len()
        ),
        (&json!("done"), &json!("stopped"), &json!("failed"), 0, 0),
        "{all:?}"
    );
}

/// (A time after any turn of these tests began: each thread is of the
/// turn now.)
const LATER: &str = "2099-01-01T00:00:00.000Z";
const KID: &str = "019ab3c4-0000-7000-8000-0000000000b1";
const GHOST: &str = "019ab3c4-0000-7000-8000-0000000000b9";
const STRAY: &str = "019ab3c4-0000-7000-8000-0000000000b8";

/// One file of Codex's for a thread it began itself, as codex-cli 0.170.0
/// writes one.
fn codex_thread(day: &Path, id: &str, meta: Value, asked: &str, then: &[Value]) -> PathBuf {
    let at = LATER;
    let mut payload = json!({"id": id, "session_id": id, "timestamp": at, "cwd": "/w/reeds",
        "originator": "codex_cli_rs", "cli_version": "0.170.0", "model_provider": "openai",
        "history_mode": "full", "base_instructions": {"text": "made-up"}});
    payload
        .as_object_mut()
        .unwrap()
        .extend(meta.as_object().unwrap().clone());
    let mut all = vec![
        json!({"timestamp": at, "type": "session_meta", "payload": payload}),
        json!({"timestamp": at, "type": "event_msg", "payload": {"type": "task_started", "turn_id": "t1"}}),
        json!({"timestamp": at, "type": "response_item", "payload": {"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": asked}]}}),
    ];
    all.extend(then.iter().cloned());
    let file = day.join(format!("rollout-2026-11-20T10-00-00-{id}.jsonl"));
    std::fs::write(&file, lines(&all)).unwrap();
    file
}

/// "It must work for all four agent programs." Codex showed a count and
/// nothing else. Its sub-agents are threads with files of their own that
/// name the thread they were begun under: each is a row with its own
/// conversation, and its reviewer of approvals (such a thread too) is
/// not. Where only its hooks tell of one, the row says only that a
/// sub-agent runs and what kind: nothing is made up for it; and a record
/// its hook names is read only where it is one of Codex's own.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn codexs_sub_agents_are_rows_too_and_its_reviewer_is_not() {
    let s = Station::start("alpha");
    let sessions = s.root.join("home/.codex/sessions");
    let hooked = sessions.join(format!(
        "2020/01/01/rollout-2020-01-01T00-00-00-{GHOST}.jsonl"
    ));
    let stray = s.root.join(format!("home/rollout-{STRAY}.jsonl"));
    let hook = |kind: &str, id: &str, stop: bool, at: Option<&Path>| {
        let mut h = json!({"subagent": {"type": kind, "id": id, "stop": stop}});
        if let Some(at) = at {
            h["subagent"]["transcript"] = json!(at);
        }
        h
    };
    let script = json!({"codex": true, "steps": [], "on_input": [
        hook("worker", KID, false, None), hook("explorer", GHOST, false, None), hook("explorer", STRAY, false, None),
        {"say": "[fake] spawned"},
        {"wait_file": "over"},
        hook("explorer", GHOST, true, Some(&hooked)), hook("explorer", STRAY, true, Some(&stray)),
        {"say": "[fake] reported"},
        {"wait_file": "end"},
        {"codex_reply": "Counted."}, {"turn_end": "ok"}]});
    let dir = folder(&s, "weir");
    let counter = pane(&s, "codex", "counter", &dir, script);
    s.wait_text(&counter, "[fake] session");
    s.call(
        "pane.paste",
        json!({"pane": counter, "text": "Count the reeds", "enter": true}),
    );
    s.wait_text(&counter, "[fake] spawned");
    let session = wait_for("the pane's session", 15, || {
        let a = s.call(
            "pane.conversation",
            json!({"pane": counter, "screen": true}),
        );
        (a["source"] == json!("transcript")).then(|| a["session"].as_str().unwrap().to_string())
    });

    // Only its hooks, so far: a row for each says that it runs and what
    // kind it is, and nothing else.
    let all = wait_for("the hooks' rows", 10, || {
        let all = rows(&s, &counter);
        (all.len() == 3).then_some(all)
    });
    let worker = row(&all, "worker");
    assert_eq!(
        json!([
            worker["id"],
            worker["state"],
            worker["title"],
            worker["transcript"],
            worker["now"],
            worker["from"]
        ]),
        json!([format!("hook:{KID}"), "working", "", false, null, "hooks"])
    );

    // Codex's own files: the thread it began, and its reviewer.
    let mut day = None;
    let mut dirs = vec![sessions.clone()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                dirs.push(e.path());
            } else if e
                .file_name()
                .to_string_lossy()
                .ends_with(&format!("-{session}.jsonl"))
            {
                day = Some(d.clone());
            }
        }
    }
    let day = day.expect("the session's file");
    let ran = json!({"timestamp": LATER, "type": "response_item", "payload": {"type": "custom_tool_call", "status": "completed", "call_id": "call_1", "name": "exec", "input": "grep -c nest survey.csv"}});
    let kid = codex_thread(
        &day,
        KID,
        json!({"parent_thread_id": session, "thread_source": "subagent",
            "source": {"subagent": {"thread_spawn": {"parent_thread_id": session, "depth": 1, "agent_role": "worker"}}}}),
        "Count the nests on the east bank",
        std::slice::from_ref(&ran),
    );
    codex_thread(
        &day,
        "019ab3c4-0000-7000-8000-0000000000d1",
        json!({"parent_thread_id": session, "thread_source": "guardian_review",
            "source": {"subagent": {"other": "guardian"}}}),
        "Review this command",
        &[],
    );
    let all = wait_for("the thread's row", 15, || {
        let all = rows(&s, &counter);
        (row(&all, "worker")["transcript"] == json!(true)).then_some(all)
    });
    assert_eq!(all.len(), 3, "{all:?}");
    let worker = row(&all, "worker");
    assert_eq!(
        json!([
            worker["id"],
            worker["state"],
            worker["title"],
            worker["now"],
            worker["depth"],
            worker["current"],
            worker["sure"]
        ]),
        json!([
            format!("child:{KID}"),
            "working",
            "Count the nests on the east bank",
            "exec · grep -c nest survey.csv",
            1,
            true,
            true
        ])
    );
    let side = s.call(
        "pane.conversation.detail",
        json!({"pane": counter, "task": worker["id"]}),
    );
    assert_eq!(
        side["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| (
                i["kind"].as_str().unwrap(),
                i["text"].as_str().or(i["summary"].as_str()).unwrap_or("")
            ))
            .collect::<Vec<_>>(),
        vec![
            ("user", "Count the nests on the east bank"),
            ("tool", "grep -c nest survey.csv")
        ]
    );

    // It is over when its own file says so, with what it said last.
    let mut text = std::fs::read_to_string(&kid).unwrap();
    text.push_str(&lines(&[
        json!({"timestamp": LATER, "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Nine nests."}], "phase": "final_answer"}}),
        json!({"timestamp": LATER, "type": "event_msg", "payload": {"type": "task_complete", "turn_id": "t1"}}),
    ]));
    std::fs::write(&kid, text).unwrap();
    let worker = wait_for("the thread over", 15, || {
        let w = row(&rows(&s, &counter), "worker");
        (w["state"] == json!("done")).then_some(w)
    });
    assert_eq!(worker["said"], json!("Nine nests."));

    // Two the hooks say are over, each with where its record is: one
    // among Codex's own files, one elsewhere.
    for file in [&hooked, &stray] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            file,
            lines(&[
                json!({"timestamp": "2020-01-01T00:00:00.000Z", "type": "session_meta", "payload": {"id": GHOST, "timestamp": "2020-01-01T00:00:00.000Z", "cwd": "/w", "source": "cli"}}),
                json!({"timestamp": "2020-01-01T00:00:01.000Z", "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Mapped the bank."}], "phase": "final_answer"}}),
            ]),
        )
        .unwrap();
    }
    std::fs::write(dir.join("over"), "").unwrap();
    s.wait_text(&counter, "[fake] reported");
    let all = wait_for("the two over", 10, || {
        let all = rows(&s, &counter);
        (live(&s, &counter)["tasks"]["running"]
            .as_array()
            .unwrap()
            .is_empty())
        .then_some(all)
    });
    let by = |id: &str| {
        all.iter()
            .find(|r| r["id"] == json!(format!("hook:{id}")))
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(
        json!([
            by(GHOST)["state"],
            by(GHOST)["transcript"],
            by(GHOST)["said"],
            by(STRAY)["state"],
            by(STRAY)["transcript"],
            by(STRAY)["said"]
        ]),
        json!(["done", true, "Mapped the bank.", "done", false, null]),
        "{all:?}"
    );
    let ghost = s.call(
        "pane.conversation.detail",
        json!({"pane": counter, "task": format!("hook:{GHOST}")}),
    );
    assert_eq!(ghost["items"][0]["text"], json!("Mapped the bank."));
    let e = s
        .try_call(
            "pane.conversation.detail",
            json!({"pane": counter, "task": format!("hook:{STRAY}")}),
        )
        .unwrap_err();
    assert_eq!(e.code, "not_found", "{e}");
    std::fs::write(dir.join("end"), "").unwrap();
    s.wait_state(&counter, "done");
}

#[cfg(unix)]
mod browser {
    use super::*;
    use common::browser::Page;

    fn shoot(page: &Page, name: &str) {
        use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
        let Some(dir) = std::env::var_os("SPANDECK_SHOTS").map(PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
    }

    /// The owner's mock-up, in a browser. Closed: a chip in the pane's
    /// header, there only while the agent has sub-agents ("5 sub-agents ·
    /// 3 working"), and nothing else of it. Open: a drawer in the pane,
    /// the conversation beside it, a row for each with its "Now" that
    /// follows the sub-agent; open or closed is remembered for the pane.
    /// One opened: its own conversation, "read only", "‹ lead" back, and
    /// "You cannot write to a sub-agent. Write to lead to steer it.":
    /// nothing can be typed to it, nor into the pane under it. What a
    /// sub-agent was asked is text, whatever it holds (rule 6). On a
    /// phone the same rows are a sheet, and one opened fills the screen.
    /// After the owner's next message the chip is gone.
    #[test]
    #[ignore = "merge tier: a browser"]
    fn the_chip_the_drawer_and_a_sub_agents_own_conversation() {
        let s = Station::start("alpha");
        let lead = pane(&s, "fake", "lead", &folder(&s, "mere"), delegating());
        // (A pane with no sub-agent, for the header as it is today.)
        let lone = pane(
            &s,
            "fake",
            "lone",
            &folder(&s, "fen"),
            json!({"steps": [{"prompt": "Count"}, {"turn_end": "ok"}]}),
        );
        s.wait_text(&lead, "[fake] delegated");
        s.wait_state(&lone, "done");
        let dir = write_sides(&s, &lead);
        let page = Page::open(&s, 1280, 760);
        page.goto(&format!("#/s/alpha/p/{lead}"));

        // Closed: the chip, and no drawer.
        let chip = "document.querySelector('.chat-pane .chat-bar .chat-subs')";
        let log_width = "Math.round(document.querySelector('.chat-pane .chat-log').getBoundingClientRect().width)";
        let closed = page.wait(
            "the chip",
            &format!("(c => c && c.innerText.includes('5 sub-agents') && [c.innerText, c.getAttribute('aria-expanded'), !!document.querySelector('.chat-drawer'), {log_width}, !!document.querySelector('.chat-pane .chat-tasks')])({chip})"),
        );
        let wide = closed[3].as_i64().unwrap();
        assert_eq!(
            json!([closed[0], closed[1], closed[2], closed[4]]),
            json!(["5 sub-agents · 3 working", "false", false, false])
        );
        assert!(wide > 700, "{closed}");
        shoot(&page, "subagents-1-chip-closed");

        // Open: the drawer beside the conversation.
        page.js(&format!("{chip}.click()"));
        let rows_js = "[...document.querySelectorAll('.chat-drawer .sub-row')].map(r => [r.dataset.state, r.dataset.depth, r.querySelector('b').innerText, r.querySelectorAll('.sub-w')[0].innerText, r.querySelector('.sub-now')?.innerText || '', r.tagName])";
        let open = page.wait(
            "the drawer",
            &format!("(d => d && d.querySelectorAll('.sub-row').length === 5 && d.querySelectorAll('.sub-now').length === 4 && [{rows_js}, {chip}.innerText, {chip}.getAttribute('aria-expanded'), (l => l.right <= d.getBoundingClientRect().left + 1 && l.width > 400 && Math.round(l.width + d.getBoundingClientRect().width))(document.querySelector('.chat-pane .chat-log').getBoundingClientRect()), d.getBoundingClientRect().width, !!d.querySelector('img'), window.pwned === undefined])(document.querySelector('.chat-pane .chat-drawer'))"),
        );
        assert_eq!(
            open[0],
            json!([
                [
                    "working",
                    "0",
                    "layout",
                    "Redo wide shots 3, 7 and 9",
                    "Now: Bash · python -m studio.layout --shot 3",
                    "BUTTON"
                ],
                [
                    "working",
                    "1",
                    "Explore",
                    "Find shot 7's camera",
                    "Now: Bash · grep -n camera shots/7.json",
                    "BUTTON"
                ],
                [
                    "working",
                    "0",
                    "general-purpose",
                    "Count the frames",
                    "Now: Shot 3 has 240 frames.",
                    "BUTTON"
                ],
                [
                    "failed",
                    "0",
                    "colour",
                    "Match hull to <img src=x onerror=window.pwned=1> ref sheet",
                    "",
                    "DIV"
                ],
                [
                    "done",
                    "0",
                    "review",
                    "Check continuity",
                    "Reported back: Two findings: shots 3 and 9.",
                    "BUTTON"
                ]
            ]),
            "{open}"
        );
        // (The conversation and the drawer share the room the
        // conversation had.)
        assert_eq!(
            json!([open[1], open[2], open[3], open[5], open[6]]),
            json!(["5 sub-agents", "true", wide, false, true]),
            "{open}"
        );
        assert!(open[4].as_f64().unwrap() >= 240.0 && open[4].as_f64().unwrap() <= 320.0);
        shoot(&page, "subagents-2-drawer-open");

        // A row's "Now" follows the sub-agent.
        let file = dir.join(format!("agent-{LAYOUT}.jsonl"));
        let mut text = std::fs::read_to_string(&file).unwrap();
        text.push_str(&lines(&[side_ran(
            LAYOUT,
            "toolu_s3",
            "python -m studio.layout --shot 7",
        )]));
        std::fs::write(&file, text).unwrap();
        page.wait(
            "the next step in its row",
            "document.querySelector('.chat-drawer .sub-row .sub-now')?.innerText === 'Now: Bash · python -m studio.layout --shot 7'",
        );

        // Open is remembered for this pane, in this browser; another
        // pane's header is as it was.
        page.reload();
        page.wait(
            "the drawer after a reload",
            "document.querySelectorAll('.chat-pane .chat-drawer .sub-row').length === 5",
        );
        page.goto(&format!("#/s/alpha/p/{lone}"));
        let none = page.wait(
            "a pane with no sub-agent",
            "(b => b && !!b.querySelector('.chat-box') && [!!document.querySelector('.chat-pane .chat-subs'), !!document.querySelector('.chat-drawer')])(document.querySelector('.chat-pane'))",
        );
        assert_eq!(none, json!([false, false]));
        page.goto(&format!("#/s/alpha/p/{lead}"));
        page.wait(
            "the drawer again",
            "document.querySelectorAll('.chat-pane .chat-drawer .sub-row').length === 5",
        );

        // One opened: its own conversation, to read only.
        page.js("document.querySelector('.chat-drawer .sub-row').click()");
        let view = page.wait(
            "the sub-agent's conversation",
            "(v => v && v.querySelector('.side-log .chat-text') && [v.querySelector('.sub-back').innerText, v.querySelector('.sub-head b').innerText, v.querySelector('.sub-about').innerText.replace(/ · [^·]*$/, ''), v.querySelector('.sub-ro').innerText, v.querySelector('.sub-foot').innerText, v.querySelector('.side-log').innerText.includes('Shot 3 uses the old camera height.'), v.querySelector('.side-log').innerText.includes('Redo wide shots 3, 7 and 9 with the second-round layout'), !!v.querySelector('textarea, input, [contenteditable]'), ((r, b) => Math.abs(r.width - b.width) < 1 && Math.abs(r.height - b.height) < 1 && r.height > 400)(v.getBoundingClientRect(), v.closest('.chat-body').getBoundingClientRect())])(document.querySelector('.chat-pane .sub-view'))",
        );
        assert_eq!(
            view,
            json!([
                "‹ lead",
                "layout",
                "sub-agent of lead",
                "read only",
                "You cannot write to a sub-agent. Write to lead to steer it.",
                true,
                true,
                false,
                true
            ])
        );
        // Nothing can be typed into the pane under it either.
        let typed = page.js("(b => { b.focus(); return [document.activeElement === b, b.closest('.chat-dock').inert]; })(document.querySelector('.chat-pane .chat-box'))");
        assert_eq!(typed, json!([false, true]));
        shoot(&page, "subagents-3-one-opened");
        page.js("document.querySelector('.sub-view .sub-back').click()");
        let back = page.wait(
            "back to the agent's own conversation",
            "!document.querySelector('.sub-view') && [document.querySelector('.chat-pane .chat-dock').inert, document.querySelectorAll('.chat-drawer .sub-row').length, (b => { b.focus(); return document.activeElement === b; })(document.querySelector('.chat-pane .chat-box'))]",
        );
        assert_eq!(back, json!([false, 5, true]));

        // Closed again: it takes no room, and stays closed.
        page.js("document.querySelector('.chat-drawer .drawer-close').click()");
        page.wait(
            "the drawer closed",
            &format!("!document.querySelector('.chat-drawer') && {chip}.innerText === '5 sub-agents · 3 working' && {log_width} === {wide}"),
        );
        page.reload();
        page.wait(
            "closed after a reload",
            &format!("(c => c && c.innerText.includes('sub-agents') && !document.querySelector('.chat-drawer'))({chip})"),
        );

        // A pane too narrow for both: the drawer lies over the
        // conversation, inside the pane.
        let narrow = Page::open(&s, 800, 700);
        narrow.goto(&format!("#/s/alpha/p/{lead}"));
        narrow.wait("the chip in a narrow pane", &format!("!!{chip}"));
        narrow.js(&format!("{chip}.click()"));
        let over = narrow.wait(
            "the drawer over the conversation",
            "(d => d && d.querySelectorAll('.sub-row').length === 5 && (p => [getComputedStyle(d).position, d.getBoundingClientRect().right <= p.right + 1, d.getBoundingClientRect().left >= p.left - 1, document.querySelector('.chat-pane .chat-log').getBoundingClientRect().width > p.width - 40])(document.querySelector('.chat-pane .chat-body').getBoundingClientRect()))(document.querySelector('.chat-pane .chat-drawer'))",
        );
        if over[0] == json!("absolute") {
            assert_eq!(over, json!(["absolute", true, true, true]));
        } else {
            // (Wide enough at this size for both.)
            assert_eq!(json!([over[1], over[2]]), json!([true, true]), "{over}");
        }
        drop(narrow);

        // A phone: the same words above the message box, the same rows in
        // a sheet, and one opened fills the screen.
        let phone = Page::open(&s, 390, 844);
        phone.phone_screen();
        phone.goto(&format!("#/s/alpha/p/{lead}"));
        let link = phone.wait(
            "the link on a phone",
            "(b => b && b.innerText.includes('sub-agents') && [b.innerText, !!document.querySelector('.chat-subs')])(document.querySelector('.phone-chat .chat-tasks'))",
        );
        assert_eq!(link, json!(["5 sub-agents · 3 working", false]));
        phone.js("document.querySelector('.phone-chat .chat-tasks').click()");
        let sheet = phone.wait(
            "the sheet",
            "(l => l && l.querySelectorAll('.sub-row').length === 5 && [[...l.querySelectorAll('.sub-row')].map(r => r.dataset.state + ' ' + r.querySelector('b').innerText), l.querySelector('.sub-now').innerText, (r => r.width >= 380 && r.height >= 800)(l.closest('.dialog').getBoundingClientRect()), [...l.querySelectorAll('button.sub-row')].every(r => r.getBoundingClientRect().height >= 44), !!l.querySelector('img')])(document.querySelector('.dialog.chat-sheet .subs-list'))",
        );
        assert_eq!(
            sheet,
            json!([
                [
                    "working layout",
                    "working Explore",
                    "working general-purpose",
                    "failed colour",
                    "done review"
                ],
                "Now: Bash · python -m studio.layout --shot 7",
                true,
                true,
                false
            ])
        );
        shoot(&phone, "subagents-4-phone-sheet");
        phone.js("document.querySelector('.dialog.chat-sheet .sub-row').click()");
        let view = phone.wait(
            "one opened on a phone",
            "(v => v && v.querySelector('.side-log').innerText.includes('Shot 3 uses the old camera height.') && [!document.querySelector('.dialog.chat-sheet'), v.querySelector('.sub-back').innerText, v.querySelector('.sub-foot').innerText, (r => r.width >= 389 && r.bottom >= 840 && r.right <= 390.5)(v.getBoundingClientRect()), !!v.querySelector('textarea, input, [contenteditable]'), (b => b.getBoundingClientRect().height >= 44)(v.querySelector('.sub-back')), document.documentElement.scrollWidth <= 390])(document.querySelector('.phone-chat .sub-view'))",
        );
        assert_eq!(
            view,
            json!([
                true,
                "‹ lead",
                "You cannot write to a sub-agent. Write to lead to steer it.",
                true,
                false,
                true,
                true
            ])
        );
        shoot(&phone, "subagents-5-phone-one-opened");
        phone.js("document.querySelector('.sub-view .sub-back').click()");
        phone.wait(
            "back to the sheet",
            "!document.querySelector('.sub-view') && document.querySelectorAll('.dialog.chat-sheet .sub-row').length === 5",
        );
        drop(phone);

        // The owner's next message: the turn's sub-agents leave the
        // header, and it is as it was.
        s.call(
            "pane.paste",
            json!({"pane": lead, "text": "and the hull", "enter": true}),
        );
        s.wait_state(&lead, "done");
        page.wait(
            "the chip gone",
            &format!("!{chip} && !document.querySelector('.chat-drawer') && !!document.querySelector('.chat-pane .chat-box')"),
        );
        // What was is still one step away: the pane's menu.
        page.js("document.querySelector('.chat-pane .chat-more').click()");
        page.choose("Sub-agents and background tasks");
        let earlier = page.wait(
            "the drawer from the menu",
            "(d => d && d.querySelector('.task-fold') && [d.querySelector('.sub-none')?.innerText, d.querySelector('.task-fold').innerText.replace(/\\s+/g, ''), d.querySelectorAll('.sub-row').length])(document.querySelector('.chat-pane .chat-drawer'))",
        );
        assert_eq!(
            earlier,
            json!(["lead has no sub-agent at work.", "Earlier·5›", 0])
        );
        page.js("document.querySelector('.chat-drawer .task-fold').click()");
        page.wait(
            "the earlier ones",
            "document.querySelectorAll('.chat-drawer .sub-row').length === 5",
        );
        shoot(&page, "subagents-6-earlier");
    }
}
