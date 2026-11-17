//! Chat for every pane (DECISIONS.md, "Chat for every pane";
//! `docs/conversation-api.md`). The owner's report of 16 Nov 2026, from a
//! phone: "Every window should have the chat and terminal option, looks
//! like it's just on a Claude window. Even in layouts". A Codex pane showed
//! only "Terminal only: Codex has not said which session it is in yet
//! (nothing has come from its hooks)".
//!
//! The stand-in agent keeps its conversation as Codex 0.169.0 does (a
//! session file begun with the first message, in the shapes of
//! `crates/archive/fixtures/codex-0.169.0.jsonl`) and runs no hooks, as a
//! Codex whose hooks the owner has not reviewed. Nothing here runs the real
//! program.
//!
//! The two station scenarios run with every change. The two in a browser
//! are the merge tier:
//!
//!   cargo test -p spandeck --test chat -- --ignored
mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::{json, Value};

const CODEX_FIXTURE: &str = include_str!("../../archive/fixtures/codex-0.169.0.jsonl");

fn folder(s: &Station, name: &str) -> PathBuf {
    let dir = s.root.join("home").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A pane of `agent` that runs the stand-in program.
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

/// A Codex whose hooks never run: it answers each message in its session
/// file and on its screen.
fn hookless_codex() -> Value {
    json!({"codex": true, "no_hooks": true, "steps": [],
        "on_input": [{"codex_reply": "counted what you asked"}]})
}

fn send(s: &Station, pane: &str, text: &str) {
    s.call(
        "pane.paste",
        json!({"pane": pane, "text": text, "enter": true}),
    );
    s.wait_text(pane, &format!("[fake] got: {text}"));
}

/// What a deck from before this asks, and what one of today asks.
fn conversation(s: &Station, pane: &str, screen: bool) -> Value {
    let mut params = json!({"pane": pane});
    if screen {
        params["screen"] = json!(true);
    }
    s.call("pane.conversation", params)
}

/// `[kind, text]` of each item.
fn said(answer: &Value) -> Vec<(String, String)> {
    answer["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["kind"].as_str().unwrap_or("").to_string(),
                i["text"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn transcript_of(s: &Station, pane: &str) -> Value {
    wait_for_or(
        "the pane's conversation from its session file",
        15,
        || {
            let mut files = vec![];
            let mut dirs = vec![s.root.join("home/.codex")];
            while let Some(d) = dirs.pop() {
                for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                    if e.path().is_dir() {
                        dirs.push(e.path());
                    } else {
                        files.push(e.path().display().to_string());
                    }
                }
            }
            format!("{}\nfiles: {files:?}", conversation(s, pane, true))
        },
        || {
            let a = conversation(s, pane, true);
            (a["source"] == json!("transcript")
                && a["items"]
                    .as_array()
                    .is_some_and(|i| i.iter().any(|x| x["kind"] == json!("text"))))
            .then_some(a)
        },
    )
}

/// The report: a Codex pane had no Chat because its hooks had not run. Its
/// session is found among Codex's own files, strictly: begun since the
/// pane's program started, in the pane's folder, and no other pane's. Two
/// Codex panes in one folder are never given each other's conversation, a
/// session a hook names takes the place of one that was found, and until
/// there is a session the screen is the conversation.
#[test]
fn a_codex_pane_with_no_hooks_gets_its_conversation_from_its_session_file() {
    let s = Station::start("alpha");
    let weir = folder(&s, "weir");
    let first = pane(&s, "codex", "first", &weir, hookless_codex());
    s.wait_text(&first, "[fake] session");

    // Before anything is said Codex has no session and no file. A deck
    // from before is told why there is no conversation, as it always was;
    // a deck of today is given the screen, with the same reason.
    let old = conversation(&s, &first, false);
    assert_eq!(old["available"], json!(false), "{old}");
    let why = old["reason"].as_str().unwrap();
    assert!(
        why.starts_with("no conversation for this pane: Codex has not said which session it is in yet (nothing has come from its hooks), and no session file of Codex's for this folder has appeared since the pane started (Codex begins one with its first message)"),
        "{why}"
    );
    let new = conversation(&s, &first, true);
    assert_eq!(
        (&new["available"], &new["source"], new["reason"].as_str()),
        (&json!(true), &json!("screen"), Some(why)),
        "{new}"
    );
    assert!(new["session"].as_str().unwrap().starts_with("screen:"));

    // The first message begins its session file, and the conversation is
    // read from it: what was said, not what the screen shows.
    send(&s, &first, "How many otters?");
    let a = transcript_of(&s, &first);
    assert_eq!(
        said(&a),
        vec![
            ("user".to_string(), "How many otters?".to_string()),
            ("text".to_string(), "counted what you asked".to_string())
        ],
        "{a}"
    );
    assert_eq!(a["session_from"], json!("files"));
    assert_eq!(a["agent"], json!("codex"));
    // A deck that held the screen's conversation is told to start again.
    let moved = s.call(
        "pane.conversation",
        json!({"pane": first, "screen": true, "session": new["session"], "after": new["cursor"]}),
    );
    assert_eq!(moved["reset"], json!(true), "{moved}");
    // A deck from before gets the same conversation: nothing new is asked of it.
    assert_eq!(said(&conversation(&s, &first, false)), said(&a));
    // The pane list says it has something not read, as for any conversation.
    assert_eq!(s.pane(&first)["unread"], json!(true));

    // A second Codex in the same folder. Its session file is begun after
    // both programs started, so it could be either pane's: it is given to
    // neither. The second pane says why and shows its screen; the first
    // keeps its own conversation, and never shows the other's message.
    let second = pane(&s, "codex", "second", &weir, hookless_codex());
    s.wait_text(&second, "[fake] session");
    send(&s, &second, "And the voles?");
    // (Long enough for the files to be looked at again, were it to find one.)
    std::thread::sleep(std::time::Duration::from_millis(700));
    let b = conversation(&s, &second, true);
    assert_eq!(b["source"], json!("screen"), "{b}");
    assert!(
        b["reason"].as_str().unwrap().contains(
            "another pane runs Codex in this folder and reports nothing either, so a session file begun since both started could be either pane's: spandeck does not guess"
        ),
        "{b}"
    );
    let screen = said(&b);
    assert!(
        screen.contains(&("user".to_string(), "And the voles?".to_string())),
        "{b}"
    );
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let a = conversation(&s, &first, true);
    assert_eq!(a["source"], json!("transcript"), "{a}");
    let all = a.to_string();
    assert!(
        all.contains("How many otters?") && !all.contains("voles"),
        "{a}"
    );

    // A Codex in a folder of its own, beside those two: found. Then one of
    // its hooks runs and names another session (one saved before the pane
    // started, which the files would never have given it): the hook wins.
    let marsh = folder(&s, "marsh");
    let saved = s.root.join("saved-rollout.jsonl");
    std::fs::write(&saved, CODEX_FIXTURE).unwrap();
    let third = pane(
        &s,
        "codex",
        "third",
        &marsh,
        json!({"codex": true, "no_hooks": true, "steps": [
            {"prompt": "Count the herons"}, {"codex_reply": "nine herons"},
            {"wait_file": "go"},
            {"name_session": {"id": "01a1da7a-0000-7000-8000-00000000c0de", "path": saved}}]}),
    );
    let c = transcript_of(&s, &third);
    assert_eq!(said(&c)[0].1, "Count the herons", "{c}");
    assert_eq!(c["session_from"], json!("files"));
    std::fs::write(marsh.join("go"), "").unwrap();
    let named = wait_for("the session its hook named", 15, || {
        let a = s.call(
            "pane.conversation",
            json!({"pane": third, "screen": true, "session": c["session"], "after": c["cursor"]}),
        );
        (a["session"] == json!("01a1da7a-0000-7000-8000-00000000c0de")).then_some(a)
    });
    assert_eq!(
        (&named["reset"], &named["session_from"], &named["source"]),
        (&json!(true), &json!("hooks"), &json!("transcript")),
        "{named}"
    );
    assert_eq!(said(&named)[0].1, "How many otters are in the weir log?");
    assert!(!named.to_string().contains("herons"), "{named}");

    // An agent is refused its own pane's conversation, the screen's too.
    let err = s
        .call_as_pane(
            &second,
            "pane.conversation",
            json!({"pane": second, "screen": true}),
        )
        .unwrap_err();
    assert_eq!(err.code, "denied");
}

/// The report: "Every window should have the chat and terminal option." A
/// pane with no transcript (here a program that is no agent) is given as
/// what was sent to it and what it printed after each, in order; a block
/// keeps its id while it grows and after; a caller that asks from its
/// cursor is told only what is new.
#[test]
fn a_shell_pane_is_served_as_what_was_sent_and_what_it_printed() {
    let s = Station::start("alpha");
    let dir = folder(&s, "plain");
    let sh = pane(
        &s,
        "shell",
        "plain",
        &dir,
        json!({"no_hooks": true, "steps": [], "on_input": [{"say": "done with it"}]}),
    );
    s.wait_text(&sh, "[fake] session");
    // A deck from before is answered as before.
    let old = conversation(&s, &sh, false);
    assert_eq!(
        (&old["available"], old["reason"].as_str()),
        (
            &json!(false),
            Some("no conversation for this pane: it runs a shell, not an agent")
        )
    );
    send(&s, &sh, "list the reeds");
    s.wait_text(&sh, "done with it");
    let a = conversation(&s, &sh, true);
    assert_eq!(
        (&a["available"], &a["source"], &a["agent"]),
        (&json!(true), &json!("screen"), &json!("shell")),
        "{a}"
    );
    assert_eq!(a["reason"], old["reason"]);
    let items = said(&a);
    let kinds: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
    assert_eq!(kinds, vec!["screen", "user", "screen"], "{a}");
    assert!(items[0].1.contains("[fake] session"), "{a}");
    assert_eq!(items[1].1, "list the reeds");
    assert!(
        items[2].1.contains("[fake] got: list the reeds") && items[2].1.contains("done with it"),
        "{a}"
    );
    assert_eq!(a["items"][1]["origin"], json!("you"));
    assert_eq!(a["items"][2]["open"], json!(true));
    let open = a["items"][2]["id"].clone();

    // Asked again from its cursor with nothing printed since: nothing.
    let ask = |a: &Value| {
        s.call(
            "pane.conversation",
            json!({"pane": sh, "screen": true, "session": a["session"], "after": a["cursor"]}),
        )
    };
    let quiet = ask(&a);
    assert_eq!(
        (&quiet["items"], &quiet["updated"], &quiet["reset"]),
        (&json!([]), &json!([]), &json!(false)),
        "{quiet}"
    );
    // A second message: the block that was open comes once more, under the
    // id it had, as it was frozen; the message and what followed are new.
    send(&s, &sh, "count them");
    s.wait_text(&sh, "[fake] session");
    let next = wait_for("the second message's output", 10, || {
        let n = ask(&a);
        (said(&n).len() == 2 && said(&n)[1].1.contains("done with it")).then_some(n)
    });
    assert_eq!(next["updated"].as_array().unwrap().len(), 1, "{next}");
    assert_eq!(next["updated"][0]["id"], open);
    assert_eq!(next["updated"][0].get("open"), None);
    assert!(!next["updated"][0]["text"]
        .as_str()
        .unwrap()
        .contains("count them"));
    assert_eq!(
        said(&next)[0],
        ("user".to_string(), "count them".to_string())
    );
    assert!(
        said(&next)[1].1.contains("[fake] got: count them"),
        "{next}"
    );

    // One block in full.
    let detail = s.call(
        "pane.conversation.detail",
        json!({"pane": sh, "item": open, "session": a["session"]}),
    );
    assert!(
        detail["text"]["text"]
            .as_str()
            .unwrap()
            .contains("[fake] got: list the reeds"),
        "{detail}"
    );
    // An agent in another pane cannot read it.
    let other = pane(&s, "fake", "other", &dir, json!({"steps": []}));
    let err = s
        .call_as_pane(
            &other,
            "pane.conversation",
            json!({"pane": sh, "screen": true}),
        )
        .unwrap_err();
    assert_eq!(err.code, "denied");
}

/// Lines of a session file in Claude Code's shapes (the fixture's), for the
/// stand-in agent to write.
fn said_line(text: &str, model: &str) -> Value {
    json!({"transcript": {"type": "assistant", "message": {"id": format!("msg_{}", text.len()), "role": "assistant", "model": model,
        "content": [{"type": "text", "text": text}],
        "usage": {"input_tokens": 100, "output_tokens": 250, "cache_read_input_tokens": 700000, "cache_creation_input_tokens": 50}}}})
}

/// A pane whose agent has a sub-agent and a command running in the
/// background, has printed its recap, and answers the next thing typed by
/// reporting another model and its sub-agent over.
fn busy_script() -> Value {
    let call = |id: &str, name: &str, input: Value| {
        json!({"transcript": {"type": "assistant", "message": {"role": "assistant",
        "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}}})
    };
    json!({"effort": "medium", "steps": [
        {"prompt": "Count the herons"},
        call("toolu_a1", "Agent", json!({"description": "Look for other tally scripts", "prompt": "Search.", "subagent_type": "Explore", "run_in_background": true})),
        {"transcript": {"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_a1", "content": "Async agent launched."}]},
            "toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": "a1", "description": "Look for other tally scripts"}}},
        call("toolu_s1", "Bash", json!({"command": "cargo watch -x test", "run_in_background": true})),
        {"transcript": {"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_s1", "content": "Running in the background"}]},
            "toolUseResult": {"stdout": "", "stderr": "", "backgroundTaskId": "bash_1"}}},
        said_line("Nine herons, counted from `survey.csv`.", "claude-opus-5-5"),
        said_line("And **three** egrets.", "claude-opus-5-5"),
        {"transcript": {"type": "system", "subtype": "turn_duration", "durationMs": 15000}},
        {"transcript": {"type": "system", "subtype": "away_summary", "content": "You asked for a count of the herons, and it is done: nine, with three egrets beside them. Next, you can decide whether the tally script should skip the egrets. (disable recaps in /config)"}},
        {"turn_end": "ok"}],
      "on_input": [
        {"model_screen": {"name": "Fable 5.2"}},
        {"effort": "high"},
        {"transcript": {"type": "user", "message": {"role": "user", "content": "<task-notification>\n<task-id>a1</task-id>\n<tool-use-id>toolu_a1</tool-use-id>\n<status>completed</status>\n<summary>Agent completed</summary>\n</task-notification>"}, "origin": {"kind": "task-notification"}}},
        said_line("Now on another model.", "claude-fable-5-2"),
        {"turn_end": "ok"}]})
}

/// Where the stand-in agent keeps a pane's session file.
fn session_file(s: &Station, pane: &str) -> PathBuf {
    let session = conversation(s, pane, true)["session"]
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

/// The sub-agent's own transcript, where Claude Code keeps one.
fn write_side(s: &Station, pane: &str) {
    let file = session_file(s, pane);
    let dir = file.with_extension("").join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    let lines = [
        json!({"type": "user", "isSidechain": true, "message": {"role": "user", "content": "Search the repository for scripts that count birds."}}),
        json!({"type": "assistant", "isSidechain": true, "message": {"role": "assistant", "content": [{"type": "text", "text": "One script counts birds: `tally.py`."}]}}),
    ];
    std::fs::write(
        dir.join("agent-a1.jsonl"),
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
    .unwrap();
}

/// The owner's report of 17 Nov 2026, from an iPhone, against Anthropic's
/// own app: "a row that show how many agents are running as well as a
/// detail window ... a model picker, can we add that for all chats ? I
/// think they are just [calling] the model command and effort", and "can I
/// clear the while you were away message?", and "make sure this all this
/// is for any agent chat not just Claude".
///
/// What the station serves for it, and to whom: a pane's background tasks
/// from its agent's own session file (running, then over), a sub-agent's
/// own transcript, the model and the effort as the agent reports them,
/// and the notes the owner cleared. Choosing a model types the agent
/// program's own command, exactly, and only while the agent is idle; a
/// program with only a picker is typed nothing. No agent can do any of it.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn a_panes_tasks_its_model_and_its_cleared_notes_are_the_owners() {
    let s = Station::start("alpha");
    let dir = folder(&s, "mere");
    let lead = pane(&s, "fake", "lead", &dir, busy_script());
    s.wait_state(&lead, "done");
    let live = |pane: &str| conversation(&s, pane, true)["live"].clone();
    let rows = |v: &Value| -> Vec<(String, String, String)> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["kind"].as_str().unwrap().to_string(),
                    t["title"].as_str().unwrap().to_string(),
                    t["status"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let l = live(&lead);
    assert_eq!(
        rows(&l["tasks"]["running"]),
        vec![
            (
                "agent".to_string(),
                "Look for other tally scripts".to_string(),
                "running".to_string()
            ),
            (
                "shell".into(),
                "cargo watch -x test".into(),
                "running".into()
            )
        ],
        "{l}"
    );
    assert_eq!(
        (
            &l["tasks"]["finished_total"],
            &l["tasks"]["running"][0]["transcript"]
        ),
        (&json!(0), &json!(false)),
        "{l}"
    );
    // The model as its session file says (counted once a second), the
    // effort as its hooks say, and what its program takes.
    let l = wait_for("the model in use", 10, || {
        let l = live(&lead);
        l["model"]["model"].is_string().then_some(l)
    });
    let m = &l["model"];
    assert_eq!(
        (&m["model"], &m["effort"], &m["can_set"], &m["picker"]),
        (
            &json!("claude-opus-5-5"),
            &json!("medium"),
            &json!(true),
            &Value::Null
        ),
        "{m}"
    );
    assert!(m["models"].as_array().unwrap().iter().any(|x| x["id"] == json!("fable") && x["says"].as_str().is_some_and(|t| !t.is_empty())), "{m}");
    assert_eq!(
        m["efforts"],
        json!(["low", "medium", "high", "xhigh", "max"])
    );

    // Its sub-agent's own transcript, once the program has written one.
    write_side(&s, &lead);
    let task = l["tasks"]["running"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        live(&lead)["tasks"]["running"][0]["transcript"],
        json!(true)
    );
    let side = s.call(
        "pane.conversation.detail",
        json!({"pane": lead, "task": task}),
    );
    assert_eq!(
        said(&side),
        vec![
            (
                "user".to_string(),
                "Search the repository for scripts that count birds.".to_string()
            ),
            ("text".into(), "One script counts birds: `tally.py`.".into())
        ]
    );
    // A command has no transcript, and no other file can be named.
    for bad in ["0.0", "child:../../x", "pane:x"] {
        let e = s
            .try_call(
                "pane.conversation.detail",
                json!({"pane": lead, "task": bad}),
            )
            .unwrap_err();
        assert_eq!(e.code, "not_found", "{bad}: {e}");
    }

    // The recap is a note, and the owner clears it: for every device.
    let page = conversation(&s, &lead, true);
    let session = page["session"].as_str().unwrap().to_string();
    let recap = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["what"] == json!("away_summary"))
        .expect("the recap")
        .clone();
    let note = recap["id"].as_str().unwrap().to_string();
    assert_eq!(live(&lead)["cleared"], json!([]));
    s.call(
        "pane.notes.clear",
        json!({"pane": lead, "session": session, "item": note}),
    );
    s.call(
        "pane.notes.clear",
        json!({"pane": lead, "session": session, "item": note}),
    );
    assert_eq!(live(&lead)["cleared"], json!([note]));
    // Nothing was taken out of the conversation itself.
    assert!(conversation(&s, &lead, true)["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["id"] == json!(note)));
    for bad in [
        json!({"pane": lead, "session": session, "item": "../x"}),
        json!({"pane": lead, "item": note}),
    ] {
        assert_eq!(
            s.try_call("pane.notes.clear", bad).unwrap_err().code,
            "invalid"
        );
    }

    // No agent chooses a model, its own or another's, or clears a note.
    let other = pane(
        &s,
        "fake",
        "other",
        &dir,
        json!({"steps": [{"prompt": "x"}, {"work": 60_000}]}),
    );
    s.wait_state(&other, "working");
    for who in [&lead, &other] {
        for (method, params) in [
            ("pane.model", json!({"pane": lead, "model": "haiku"})),
            (
                "pane.notes.clear",
                json!({"pane": lead, "session": session, "restore": true}),
            ),
            (
                "agents.command",
                json!({"pane": lead, "command": "/model haiku"}),
            ),
        ] {
            let refused = s.call_as_pane(who, method, params).unwrap_err();
            assert_eq!(refused.code, "denied", "{method}: {refused}");
        }
    }
    assert_eq!(live(&lead)["cleared"], json!([note]));

    // In the middle of a turn nothing is typed, and the answer says so.
    let busy = s.call("pane.model", json!({"pane": other, "model": "opus"}));
    assert_eq!(
        (&busy["sent"], &busy["busy"], &busy["command"]),
        (&json!(false), &json!(true), &json!("/model opus")),
        "{busy}"
    );
    // A name is one word; a shell has no model.
    for bad in [
        json!({"pane": lead, "model": "opus; /clear"}),
        json!({"pane": lead, "effort": "extreme"}),
        json!({"pane": lead}),
        json!({"pane": lead, "model": "a", "effort": "low"}),
    ] {
        assert_eq!(
            s.try_call("pane.model", bad.clone()).unwrap_err().code,
            "invalid",
            "{bad}"
        );
    }

    // Idle: the program's own command, exactly, as the owner would type it.
    let sent = s.call("pane.model", json!({"pane": lead, "model": "fable"}));
    assert_eq!(
        (&sent["sent"], &sent["command"]),
        (&json!(true), &json!("/model fable")),
        "{sent}"
    );
    s.wait_text(&lead, "[fake] got: /model fable");
    // Found with the real Claude Code on 17 Nov 2026: it then asks
    // "Switch model?", and nothing noticed. The owner chose the model in
    // the sheet, which says what switching costs: the question that
    // follows spandeck's own command is answered, once, and it is in the
    // ship's log. The pane never reads as needing the owner for it.
    s.wait_text(&lead, "[fake] model switched to Fable 5.2");
    assert!(!s.text(&lead).contains("swallowed"), "{}", s.text(&lead));
    let logged = s
        .call("log.query", json!({"kind": "model.switch.confirmed"}))
        .to_string();
    assert!(logged.contains("Fable 5.2"), "{logged}");
    // The program's own word for the model it set is what the chip is
    // given until the agent has answered on it: a pane that says nothing
    // after the switch.
    let mute = pane(
        &s,
        "fake",
        "mute",
        &dir,
        json!({"steps": [{"prompt": "x"}, said_line("Counted.", "claude-opus-5-5"), {"turn_end": "ok"}],
            "on_input": [{"model_screen": {"name": "Sonnet 5.5"}}]}),
    );
    s.wait_state(&mute, "done");
    s.call("pane.model", json!({"pane": mute, "model": "sonnet"}));
    s.wait_text(&mute, "[fake] model switched to Sonnet 5.5");
    let named = wait_for("the model as the program named it", 10, || {
        let l = live(&mute);
        (l["model"]["set"] == json!("Sonnet 5.5")).then_some(l)
    });
    assert_eq!(named["model"]["model"], json!("claude-opus-5-5"), "{named}");
    assert!(
        !s.text(&other).contains("/model"),
        "nothing was typed into the busy pane"
    );
    // What the chip then shows is what the agent reports, and its
    // sub-agent is over.
    let l = wait_for("the agent's report", 10, || {
        let l = live(&lead);
        (l["model"]["model"] == json!("claude-fable-5-2") && l["model"]["effort"] == json!("high"))
            .then_some(l)
    });
    assert_eq!(
        rows(&l["tasks"]["running"]),
        vec![(
            "shell".to_string(),
            "cargo watch -x test".to_string(),
            "running".to_string()
        )],
        "{l}"
    );
    assert_eq!(
        rows(&l["tasks"]["finished"]),
        vec![(
            "agent".to_string(),
            "Look for other tally scripts".to_string(),
            "completed".to_string()
        )],
        "{l}"
    );
    s.call(
        "pane.notes.clear",
        json!({"pane": lead, "session": session, "restore": true}),
    );
    assert_eq!(live(&lead)["cleared"], json!([]));

    // The same question when it is not the answer to spandeck's own
    // command (the owner typed `/model` in the terminal): nothing is
    // pressed. The pane needs the owner and says what is asked; a
    // message sent now would be typed into the question and lost, as the
    // real one was, so it is refused and nothing of it reaches the pane;
    // keys still go, since they are the answer.
    s.wait_state(&lead, "done");
    s.call(
        "pane.input",
        json!({"pane": lead, "data": "/model fable\r"}),
    );
    let asks = wait_for("the pane to say what it asks", 10, || {
        let p = s.pane(&lead);
        (p["state"] == json!("needs_you") && p["prompt"].is_object()).then_some(p)
    });
    assert_eq!(
        (
            &asks["needs_reason"],
            &asks["prompt"]["kind"],
            &asks["prompt"]["name"],
            &asks["prompt"]["on_yes"]
        ),
        (
            &json!(
                "asks whether to switch model to Fable 5.2: answer it in Chat or in the terminal"
            ),
            &json!("model_switch"),
            &json!("Fable 5.2"),
            &json!(true)
        ),
        "{asks}"
    );
    assert_eq!(live(&lead)["prompt"]["name"], json!("Fable 5.2"));
    std::thread::sleep(std::time::Duration::from_millis(1200));
    assert!(
        !s.text(&lead)
            .contains("model switched to Fable 5.2\n[fake] model switched"),
        "nothing was pressed for the owner"
    );
    for (method, params) in [
        (
            "pane.paste",
            json!({"pane": lead, "text": "count the egrets too", "enter": true}),
        ),
        (
            "pane.send_text",
            json!({"pane": lead, "text": "count the egrets too"}),
        ),
        ("pane.model", json!({"pane": lead, "model": "haiku"})),
    ] {
        match s.try_call(method, params) {
            Err(e) => assert!(
                e.code == "invalid" && e.message.contains("Switch model?"),
                "{method}: {e}"
            ),
            Ok(v) => assert!(
                v["sent"] == json!(false) && v["why"].as_str().unwrap().contains("Switch model?"),
                "{method}: {v}"
            ),
        }
    }
    s.call("pane.input", json!({"pane": lead, "data": "\u{1b}"}));
    s.wait_text(&lead, "[fake] model kept");
    assert!(
        !s.text(&lead).contains("egrets too") && !s.text(&lead).contains("swallowed"),
        "{}",
        s.text(&lead)
    );
    let after = wait_for("the pane as it was", 10, || {
        let p = s.pane(&lead);
        p["prompt"].is_null().then_some(p)
    });
    assert_eq!(
        (&after["state"], &after["needs_reason"]),
        (&json!("done"), &Value::Null),
        "{after}"
    );
    // And a message goes again.
    wait_for("a message to go again", 5, || {
        s.try_call(
            "pane.paste",
            json!({"pane": other, "text": "", "enter": false}),
        )
        .ok()
    });

    // For this pane alone: its program is started again on the model
    // with its own flag and resumes; nothing is typed, and a later
    // restart keeps it. Refused in the middle of a turn.
    let busy = s.call(
        "pane.model",
        json!({"pane": other, "model": "haiku", "only_pane": true}),
    );
    assert_eq!(
        (&busy["sent"], &busy["busy"]),
        (&json!(false), &json!(true)),
        "{busy}"
    );
    s.wait_state(&lead, "done");
    let only = s.call(
        "pane.model",
        json!({"pane": lead, "model": "haiku", "only_pane": true}),
    );
    assert_eq!(
        (&only["restarted"], &only["args"]),
        (&json!(true), &json!(["--model", "haiku"])),
        "{only}"
    );
    s.wait_text(&lead, "[fake] started on the model haiku, resuming");
    assert!(!s.text(&lead).contains("/model haiku"), "nothing was typed");
    assert_eq!(
        live(&lead)["model"]["only_pane"]["args"],
        json!(["--model", "haiku"])
    );
    s.call("pane.restart", json!({"pane": lead}));
    wait_for("the model kept through a restart", 10, || {
        (s.text(&lead).matches("started on the model haiku").count() >= 1
            && s.pane(&lead)["exited"] != json!(true))
        .then_some(())
    });

    // Codex: its own command for each, and no task in its file. OpenCode:
    // a picker of its own, so nothing is typed, and the answer says where.
    let codex = pane(&s, "codex", "codex", &folder(&s, "weir"), hookless_codex());
    s.wait_text(&codex, "[fake] session");
    let c = live(&codex);
    assert_eq!(
        (
            &c["tasks"]["running"],
            &c["tasks"]["finished"],
            &c["model"]["can_set"]
        ),
        (&json!([]), &json!([]), &json!(true)),
        "{c}"
    );
    let sent = s.call("pane.model", json!({"pane": codex, "effort": "high"}));
    assert_eq!(sent["command"], json!("/model --reasoning=high"), "{sent}");
    s.wait_text(&codex, "[fake] got: /model --reasoning=high");
    let oc = pane(
        &s,
        "opencode",
        "oc",
        &folder(&s, "fen"),
        json!({"no_hooks": true, "steps": []}),
    );
    let o = live(&oc);
    assert_eq!(
        (
            &o["model"]["can_set"],
            &o["model"]["picker"],
            &o["model"]["efforts"]
        ),
        (&json!(false), &json!("/models"), &json!([])),
        "{o}"
    );
    let refused = s
        .try_call(
            "pane.model",
            json!({"pane": oc, "model": "anthropic/claude"}),
        )
        .unwrap_err();
    assert!(
        refused.code == "invalid" && refused.message.contains("/models"),
        "{refused}"
    );
    // A shell: no model, and nothing to type.
    let sh = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "sh", "cwd": dir}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(live(&sh)["model"], Value::Null);
    assert_eq!(
        s.try_call("pane.model", json!({"pane": sh, "model": "opus"}))
            .unwrap_err()
            .code,
        "invalid"
    );
}

/// OpenCode's database in a station's throwaway home, with the two tables
/// of OpenCode 2.3.0 that are read (their columns as its own schema has
/// them, the ones not read left out where it allows).
#[cfg(unix)]
fn opencode_store(s: &Station) -> rusqlite::Connection {
    let dir = s.root.join("home/.local/share/opencode");
    std::fs::create_dir_all(&dir).unwrap();
    let conn = rusqlite::Connection::open(dir.join("opencode.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_v2 (id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
            slug text NOT NULL, directory text NOT NULL, title text, version text NOT NULL,
            cost real DEFAULT 0 NOT NULL, tokens_input integer DEFAULT 0 NOT NULL, tokens_output integer DEFAULT 0 NOT NULL,
            tokens_cache_read integer DEFAULT 0 NOT NULL, tokens_cache_write integer DEFAULT 0 NOT NULL, model text,
            time_created integer NOT NULL, time_updated integer NOT NULL, time_archived integer);
         CREATE TABLE IF NOT EXISTS session_message (id text PRIMARY KEY, session_id text NOT NULL, type text NOT NULL,
            seq integer NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL);",
    )
    .unwrap();
    conn
}

#[cfg(unix)]
fn opencode_session(conn: &rusqlite::Connection, id: &str, dir: &Path, at: u64) {
    conn.execute(
        "INSERT INTO session_v2(id, project_id, slug, directory, version, time_created, time_updated)
         VALUES(?1, 'prj', 'slug', ?2, '2.3.0', ?3, ?3)",
        rusqlite::params![id, dir.display().to_string(), at as i64],
    )
    .unwrap();
}

#[cfg(unix)]
fn opencode_message(conn: &rusqlite::Connection, session: &str, seq: i64, ty: &str, data: Value) {
    let at = now() as i64;
    conn.execute(
        "INSERT OR REPLACE INTO session_message VALUES(?1, ?2, ?3, ?4, ?5, ?5, ?6)",
        rusqlite::params![
            format!("msg_{session}_{seq}"),
            session,
            ty,
            seq,
            at,
            data.to_string()
        ],
    )
    .unwrap();
    conn.execute(
        "UPDATE session_v2 SET time_updated = ?2 WHERE id = ?1",
        rusqlite::params![session, at],
    )
    .unwrap();
}

#[cfg(unix)]
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// One conversation of Antigravity's in a station's throwaway home, as
/// `agy` 1.5.9 keeps one: a row of its summaries that says which folder it
/// belongs to, and its transcript, cut (`transcript.jsonl`) and whole.
#[cfg(unix)]
fn antigravity_conversation(s: &Station, id: &str, dir: &Path, steps: &[Value], whole: &[Value]) {
    let agy = s.root.join("home/.gemini/antigravity-cli");
    let logs = agy.join("brain").join(id).join(".system_generated/logs");
    std::fs::create_dir_all(&logs).unwrap();
    let lines = |steps: &[Value]| steps.iter().map(|l| format!("{l}\n")).collect::<String>();
    std::fs::write(logs.join("transcript.jsonl"), lines(steps)).unwrap();
    std::fs::write(logs.join("transcript_full.jsonl"), lines(whole)).unwrap();
    let conn = rusqlite::Connection::open(agy.join("conversation_summaries.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS conversation_summaries (conversation_id text PRIMARY KEY, title text NOT NULL DEFAULT '',
            step_count integer NOT NULL DEFAULT 0, workspace_uris text NOT NULL, status text NOT NULL DEFAULT '',
            parent_conversation_id text NOT NULL DEFAULT '')",
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO conversation_summaries(conversation_id, workspace_uris) VALUES(?1, ?2)",
        rusqlite::params![id, json!([format!("file://{}", dir.display())]).to_string()],
    )
    .unwrap();
}

#[cfg(unix)]
fn agy_step(n: u64, ty: &str, at: &str, extra: Value) -> Value {
    let mut step = json!({"step_index": n, "type": ty, "status": "DONE", "created_at": at,
        "source": if ty == "USER_INPUT" { "USER_EXPLICIT" } else { "MODEL" }});
    step.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    step
}

/// What the owner wrote, as Antigravity wraps it.
#[cfg(unix)]
fn agy_asked(n: u64, at: &str, text: &str) -> Value {
    agy_step(
        n,
        "USER_INPUT",
        at,
        json!({"content": format!("<USER_REQUEST>\n{text}\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: {at}.\n</ADDITIONAL_METADATA>")}),
    )
}

#[cfg(unix)]
fn wait_transcript(s: &Station, pane: &str, has: &str) -> Value {
    wait_for_or(
        "the pane's own conversation",
        15,
        || conversation(s, pane, true).to_string(),
        || {
            let a = conversation(s, pane, true);
            (a["source"] == json!("transcript") && a["items"].to_string().contains(has))
                .then_some(a)
        },
    )
}

/// The owner's report of 16 Nov 2026, from a phone: "Antigravity chat ...
/// look different in chat and should be the same. Shouldn't chat be the
/// same code base for every chat?" Antigravity's and OpenCode's Chat was
/// their screen. Each now has its own conversation, read from the
/// program's own store (made here in the real layouts, with made-up
/// words), and a pane is tied to one as strictly as a Codex pane is: begun
/// in its folder since its program started, or named by its hook.
#[test]
#[cfg(unix)]
fn opencode_and_antigravity_panes_get_their_own_conversations() {
    let s = Station::start("alpha");
    let quiet =
        || json!({"no_hooks": true, "steps": [], "on_input": [{"say": "drawn on the screen"}]});

    // OpenCode. A conversation from before the pane, in its folder, is not
    // the pane's; until one is begun the screen is the conversation.
    let fen = folder(&s, "fen");
    let db = opencode_store(&s);
    opencode_session(&db, "ses_before", &fen, 1000);
    opencode_message(
        &db,
        "ses_before",
        1,
        "user",
        json!({"text": "an older question", "time": {"created": 1000}}),
    );
    let oc = pane(&s, "opencode", "oc", &fen, quiet());
    s.wait_text(&oc, "[fake] session");
    let a = conversation(&s, &oc, true);
    assert_eq!(a["source"], json!("screen"), "{a}");
    assert_eq!(
        a["reason"],
        json!("no conversation for this pane: OpenCode does not say which conversation it is in (it has no hooks), and no conversation of OpenCode's for this folder has appeared since the pane started (OpenCode begins one with its first message)")
    );
    opencode_session(&db, "ses_mine", &fen, now());
    opencode_message(
        &db,
        "ses_mine",
        1,
        "user",
        json!({"text": "How many bitterns?", "files": [], "agents": [], "time": {"created": now()}}),
    );
    opencode_message(
        &db,
        "ses_mine",
        2,
        "assistant",
        json!({"agent": "build", "content": [{"type": "text", "text": "Counting"}], "time": {"created": now()}}),
    );
    let a = wait_transcript(&s, &oc, "bitterns");
    // The answer is still being written: nothing of it yet.
    assert_eq!(
        said(&a),
        vec![("user".to_string(), "How many bitterns?".to_string())],
        "{a}"
    );
    assert_eq!(
        (&a["session"], &a["session_from"], &a["agent"]),
        (&json!("ses_mine"), &json!("files"), &json!("opencode"))
    );
    opencode_message(
        &db,
        "ses_mine",
        2,
        "assistant",
        json!({"agent": "build", "finish": "stop",
            "content": [{"type": "tool", "id": "call_1", "name": "shell", "time": {"created": 1, "ran": 2, "completed": 900},
                         "state": {"status": "completed", "input": {"command": "grep -c bittern fen.log"},
                                   "content": [{"type": "text", "text": "4"}], "metadata": {"exit": 0}}},
                        {"type": "text", "text": "There are **four** bitterns."}],
            "time": {"created": now(), "completed": now()}}),
    );
    opencode_message(
        &db,
        "ses_mine",
        3,
        "idle",
        json!({"outcome": "succeeded", "time": {"created": now()}}),
    );
    let more = wait_for("the settled answer", 15, || {
        let n = s.call(
            "pane.conversation",
            json!({"pane": oc, "screen": true, "session": a["session"], "after": a["cursor"]}),
        );
        (n["items"].as_array().is_some_and(|i| i.len() == 3)).then_some(n)
    });
    let kinds: Vec<&str> = more["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["tool", "text", "notice"], "{more}");
    assert_eq!(
        (&more["items"][0]["status"], &more["items"][2]["what"]),
        (&json!("ok"), &json!("turn_ended"))
    );
    assert_eq!(
        more["items"][1]["text"],
        json!("There are **four** bitterns.")
    );
    assert!(!more.to_string().contains("older question"));
    assert_eq!(s.pane(&oc)["unread"], json!(true));
    let detail = s.call(
        "pane.conversation.detail",
        json!({"pane": oc, "item": more["items"][0]["id"], "session": a["session"]}),
    );
    assert_eq!(detail["output"]["text"], json!("4"), "{detail}");

    // Antigravity. One conversation from before the pane, in its folder:
    // not the pane's. One begun since: the pane's, read from its whole
    // transcript. Then a hook names another, and the hook wins.
    let mere = folder(&s, "mere");
    let before = [agy_asked(0, "2020-01-01T00:00:00Z", "an older question")];
    antigravity_conversation(
        &s,
        "0aaaaaaa-0000-4000-8000-000000000001",
        &mere,
        &before,
        &before,
    );
    let agy = pane(&s, "antigravity", "agy", &mere, quiet());
    s.wait_text(&agy, "[fake] session");
    let a = conversation(&s, &agy, true);
    assert_eq!(a["source"], json!("screen"), "{a}");
    assert!(
        a["reason"].as_str().unwrap().ends_with("no conversation of Antigravity's for this folder has appeared since the pane started (Antigravity begins one with its first message)"),
        "{a}"
    );
    let since = "2099-01-01T00:00:00Z";
    let reply = "### Grebes\n\n1. two on the mere\n2. one in the reeds";
    let cut = [
        agy_asked(0, since, "How many grebes? (cut"),
        agy_step(1, "PLANNER_RESPONSE", since, json!({"content": reply})),
    ];
    let whole = [
        agy_asked(0, since, "How many grebes? (cut here by the program)"),
        cut[1].clone(),
    ];
    antigravity_conversation(
        &s,
        "0aaaaaaa-0000-4000-8000-000000000002",
        &mere,
        &cut,
        &whole,
    );
    let a = wait_transcript(&s, &agy, "grebes");
    assert_eq!(
        said(&a),
        vec![
            (
                "user".to_string(),
                "How many grebes? (cut here by the program)".to_string()
            ),
            ("text".to_string(), reply.to_string()),
            ("notice".to_string(), String::new()),
        ],
        "{a}"
    );
    assert_eq!(
        (&a["session_from"], &a["agent"]),
        (&json!("files"), &json!("antigravity"))
    );
    assert!(!a.to_string().contains("local time") && !a.to_string().contains("older question"));
    let named = "0aaaaaaa-0000-4000-8000-000000000003";
    let other = folder(&s, "elsewhere");
    let theirs = [agy_asked(
        0,
        "2020-01-01T00:00:00Z",
        "What the hook's conversation holds",
    )];
    antigravity_conversation(&s, named, &other, &theirs, &theirs);
    s.call_as_pane(
        &agy,
        "hook.ingest",
        json!({"agent": "antigravity", "payload": {"hookEventName": "PreInvocation", "conversationId": named,
            "transcriptPath": mere.join(".gemini/antigravity-cli/transcript.jsonl"), "cwd": mere, "lastUserInput": "x"}}),
    )
    .unwrap();
    let b = wait_transcript(&s, &agy, "the hook's conversation");
    assert_eq!(
        (&b["session"], &b["session_from"]),
        (&json!(named), &json!("hooks")),
        "{b}"
    );
    assert!(!b.to_string().contains("grebes"));
}

/// A program that draws its screen itself, 120 columns wide, as the
/// agent programs do: a title in a frame, what it answered (indented,
/// wrapped by the program at its own width, markdown as it was
/// written, bullets drawn as dots, a table), its message box with the
/// cursor in it, and a status row.
#[cfg(unix)]
pub fn drawn_screen() -> String {
    let rule = "─".repeat(112);
    let mut rows = vec![
        format!("  ╭{rule}╮"),
        format!("  │ {:<110} │", "Tern Code 9.9 · ~/mere"),
        format!("  ╰{rule}╯"),
        String::new(),
        "  ### Herons by the weir".into(),
        String::new(),
        "  The survey counts **nine nests** this year, three more than last spring. Most are high in the alders on the east".into(),
        "  bank, where the water is slow and the fishing is easy.".into(),
        String::new(),
        "  1. six on the east bank".into(),
        "  2. three on the island".into(),
        String::new(),
        "  ● they feed at dusk".into(),
        "  ● `survey.md` has the dates".into(),
        String::new(),
        "  bank          nests     seen at dusk".into(),
        "  east          6         yes".into(),
        "  island        3         no".into(),
    ];
    rows.resize(31, String::new());
    rows.push(format!("  ╭{rule}╮"));
    rows.push(format!("  │ {:<110} │", "> "));
    rows.push(format!("  ╰{rule}╯"));
    rows.push("  tern-2-low · 12% context left · /help for commands".into());
    // Drawn row by row, each at its place, and the cursor left in the box.
    let mut out = String::from("\x1b[?1049h\x1b[2J");
    for (n, row) in rows.iter().enumerate() {
        out.push_str(&format!("\x1b[{};1H{row}", n + 1));
    }
    out.push_str("\x1b[33;5H");
    out
}

/// A pane of `agent` whose program draws that screen, 120 columns by
/// 36 rows, and draws it again after each message.
#[cfg(unix)]
pub fn drawn_pane(s: &Station, agent: &str, name: &str, dir: &Path) -> String {
    let go = format!("go-{name}");
    let id = pane(
        s,
        agent,
        name,
        dir,
        json!({"no_hooks": true, "steps": [{"wait_file": go}, {"raw": drawn_screen()}], "on_input": [{"raw": drawn_screen()}]}),
    );
    s.wait_text(&id, "[fake] session");
    s.call("pane.resize", json!({"pane": id, "cols": 120, "rows": 36}));
    std::fs::write(dir.join(go), "").unwrap();
    s.wait_text(&id, "tern-2-low");
    id
}

/// The owner's report of 16 Nov 2026, with a picture of Antigravity's
/// Chat: one box of terminal text, "lines running off the right edge and
/// cut, wrapped fragments mixed with indentation, markdown shown raw". A
/// program that draws its own screen, 120 columns wide, is served as parts
/// to read: its paragraph one line again, its heading and its lists as
/// they were written, its table as rows, and none of its frame, its
/// message box or its status row; a long line that the terminal wrapped is
/// one line. What the rows were is still served beside them, for a deck
/// from before.
#[test]
#[cfg(unix)]
fn what_an_agent_draws_on_its_screen_is_served_as_prose() {
    let s = Station::start("alpha");
    let tui = drawn_pane(&s, "opencode", "tui", &folder(&s, "mere"));
    let a = conversation(&s, &tui, true);
    assert_eq!(a["source"], json!("screen"), "{a}");
    let item = a["items"].as_array().unwrap().last().unwrap();
    assert_eq!(
        (&item["kind"], &item["whole"], &item["open"]),
        (&json!("screen"), &json!(true), &json!(true)),
        "{a}"
    );
    assert_eq!(
        item["parts"],
        json!([
            {"prose": true, "text": "Tern Code 9.9 · ~/mere\n\n### Herons by the weir\n\nThe survey counts **nine nests** this year, three more than last spring. Most are high in the alders on the east bank, where the water is slow and the fishing is easy.\n\n1. six on the east bank\n2. three on the island\n\n- they feed at dusk\n- `survey.md` has the dates"},
            {"prose": false, "text": "bank          nests     seen at dusk\neast          6         yes\nisland        3         no"},
        ]),
        "{a}"
    );
    // The rows as they were drawn, for a deck that knows no parts.
    let rows = item["text"].as_str().unwrap();
    assert!(
        rows.contains("╭──")
            && rows.contains("  bank, where the water")
            && rows.contains("tern-2-low"),
        "{a}"
    );

    // A program that prints lines: one longer than the pane is wide is
    // wrapped by the terminal, which says so, and is one line here; the
    // lines the program ended stay lines.
    let long = "The heron stood in the shallows for most of the morning ".repeat(6);
    let log = pane(
        &s,
        "antigravity",
        "log",
        &folder(&s, "fen"),
        json!({"no_hooks": true, "steps": [{"say": long.trim()}, {"say": "and then it flew"}]}),
    );
    s.wait_text(&log, "and then it flew");
    let a = conversation(&s, &log, true);
    let said = a["items"][0]["parts"][0]["text"].as_str().unwrap_or("");
    assert_eq!(a["items"][0]["parts"][0]["prose"], json!(true), "{a}");
    assert!(
        said.contains(&format!("{}\nand then it flew", long.trim())),
        "{a}"
    );
}

/// What the two browser scenarios share: a Codex whose hooks never ran and
/// a real shell, side by side in a layout, and a way to write pictures for
/// a human to look at (`SPANDECK_SHOTS=/some/dir`).
#[cfg(unix)]
mod in_a_browser {
    use super::*;
    use common::browser::Page;

    pub struct Scene {
        pub s: Station,
        pub codex: String,
        pub sh: String,
    }

    pub fn scene() -> Scene {
        let s = Station::start("alpha");
        let weir = folder(&s, "weir");
        let codex = pane(&s, "codex", "counter", &weir, hookless_codex());
        s.wait_text(&codex, "[fake] session");
        let sh = s.call(
            "pane.spawn",
            json!({"program": "/bin/sh", "name": "scratch", "cwd": weir}),
        )["id"]
            .as_str()
            .unwrap()
            .to_string();
        s.call(
            "deck.layout.save",
            json!({"layout": {"name": "pair", "tree": {"dir": "row", "ratio": 500,
                "a": {"id": codex, "station": "alpha"}, "b": {"id": sh, "station": "alpha"}}}}),
        );
        Scene { s, codex, sh }
    }

    pub fn shoot(page: &Page, name: &str) {
        use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
        let Some(dir) = std::env::var_os("SPANDECK_SHOTS").map(PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
    }

    /// Write `text` in the message box under `root` and press Send.
    pub fn say(root: &str, text: &str) -> String {
        format!(
            "(b => b && !b.closest('[hidden]') && (b.value = {}, b.dispatchEvent(new Event('input')), document.querySelector('{root} .chat-send').click(), true))(document.querySelector('{root} .chat-box'))",
            json!(text)
        )
    }

    /// The owner's report, on the phone it came from: in a layout, a Codex
    /// pane whose hooks have not run and a shell both have "Chat |
    /// Terminal". Chat shows something for each (the screen, with why;
    /// then Codex's own conversation once its session file is there, with
    /// what was being written kept), a message sent from Chat reaches the
    /// pane, and Terminal is one tap away. What a pane printed is put on
    /// the page as text (rule 6).
    #[test]
    #[ignore = "merge tier: a browser"]
    fn on_a_phone_every_pane_of_a_layout_has_chat_and_terminal() {
        let Scene { s, codex, sh } = scene();
        let page = Page::open(&s, 390, 800);
        page.goto("#/l/pair");
        let showing = "document.querySelector('.lay-showing')?.innerText";
        let switch = "[...document.querySelectorAll('.phone-layout > .chat-bar .chat-switch button')].map(b => b.innerText + (b.classList.contains('on') ? '*' : '')).join(' ')";
        let face = |to: &str| {
            format!("[...document.querySelectorAll('.phone-layout > .chat-bar .chat-switch button')].find(b => b.innerText === '{to}')")
        };
        let src = "document.querySelector('.phone-layout .chat-src')?.innerText";
        let blocks = "[...document.querySelectorAll('.phone-layout .chat-screen')].map(x => x.innerText).join('\\n')";
        let bubbles = "[...document.querySelectorAll('.phone-layout .chat-user .chat-bubble')].map(x => x.innerText)";

        // The Codex pane, before it has a session: Chat, made from its
        // screen, and saying so where "Terminal only" was said.
        let seen = page.wait(
            "the Codex pane's screen as its conversation",
            &format!("{showing} === 'pair › counter · 1 of 2' && {blocks}.includes('[fake] session') && [{switch}, {src}, !!document.querySelector('.phone-layout .chat-why'), !!document.querySelector('.phone-layout .chat-keys:not([hidden])')]"),
        );
        assert_eq!(seen[0], json!("Chat* Terminal"), "{seen}");
        assert!(
            seen[1].as_str().unwrap().starts_with("From the screen: Codex has not said which session it is in yet (nothing has come from its hooks), and no session file of Codex's"),
            "{seen}"
        );
        // (The keys are in the message box, out of the way until asked for.)
        assert_eq!(
            (&seen[2], &seen[3]),
            (&json!(false), &json!(false)),
            "{seen}"
        );
        shoot(&page, "phone-layout-codex-before-a-session");

        // A message from Chat reaches the pane. Codex begins its session
        // file with it, and the conversation becomes Codex's own: the
        // message as a bubble, the answer as text, no note, and what was
        // being written meanwhile still in the box.
        page.wait(
            "the message sent",
            &say(".phone-layout", "How many otters?"),
        );
        s.wait_text(&codex, "[fake] got: How many otters?");
        page.wait(
            "the box emptied, then something half written",
            "(b => b.value === '' && (b.value = 'and the voles', b.dispatchEvent(new Event('input')), true))(document.querySelector('.phone-layout .chat-box'))",
        );
        let seen = page.wait(
            "Codex's own conversation",
            &format!("[...document.querySelectorAll('.phone-layout .chat-text')].some(t => t.innerText.includes('counted what you asked')) && !document.querySelector('.phone-layout .chat-src') && [{bubbles}, document.querySelector('.phone-layout .chat-box').value, {switch}, document.querySelectorAll('.phone-layout .chat-screen').length]"),
        );
        assert_eq!(
            seen,
            json!([["How many otters?"], "and the voles", "Chat* Terminal", 0])
        );
        shoot(&page, "phone-layout-codex-found-session");

        // Terminal is one tap away, and Chat one tap back.
        page.wait(
            "the terminal, then Chat again",
            &format!("({}.click(), true)", face("Terminal")),
        );
        page.wait(
            "the terminal",
            &format!("!!document.querySelector('.phone-layout .xterm-rows') && !document.querySelector('.phone-layout .chat-log') && ({}.click(), true)", face("Chat")),
        );
        page.wait(
            "the conversation again, then the next pane",
            "!!document.querySelector('.phone-layout .chat-log') && !document.querySelector('.phone-layout .xterm-rows') && (document.querySelector('.lay-nav .lay-step:last-child').click(), true)",
        );

        // The shell: it has the switch too. It opens as its terminal (a
        // shell's default), and Chat is one tap.
        let seen = page.wait(
            "the shell with the switch",
            &format!("{showing} === 'pair › scratch · 2 of 2' && !!document.querySelector('.phone-layout .xterm-rows') && {switch} !== '' && [{switch}, !!document.querySelector('.phone-layout .chat-why')]"),
        );
        assert_eq!(seen, json!(["Chat Terminal*", false]));
        page.js(&format!("{}.click()", face("Chat")));
        let seen = page.wait(
            "the shell's screen as its conversation",
            &format!("!!document.querySelector('.phone-layout .chat-log') && !!{src} && [{src}, {switch}]"),
        );
        assert_eq!(
            seen,
            json!([
                "From the screen: it runs a shell, not an agent",
                "Chat* Terminal"
            ])
        );
        page.wait(
            "a command sent from Chat",
            &say(".phone-layout", "echo reed-$((40+2)) '<b id=evil>bold</b>'"),
        );
        s.wait_text(&sh, "reed-42");
        let seen = page.wait(
            "the command and what it printed",
            &format!("{blocks}.includes('reed-42 <b id=evil>bold</b>') && [{bubbles}, !!document.querySelector('#evil'), !!document.querySelector('.phone-layout .chat-keys:not([hidden])')]"),
        );
        assert_eq!(
            seen,
            json!([["echo reed-$((40+2)) '<b id=evil>bold</b>'"], false, false])
        );
        shoot(&page, "phone-layout-shell-chat");
        // A key of the row reaches the pane: Ctrl-C ends a command.
        page.wait(
            "a command that waits",
            &say(".phone-layout", "sleep 300; echo slept"),
        );
        s.wait_text(&sh, "sleep 300");
        page.js("document.querySelector('.phone-layout .chat-keys-btn').click()");
        page.wait(
            "the keys, asked for",
            "!!document.querySelector('.phone-layout .chat-keys:not([hidden])') && ([...document.querySelectorAll('.phone-layout .chat-keys .key')].find(b => b.innerText === '^C').click(), true)",
        );
        page.wait(
            "another command",
            &say(".phone-layout", "echo back-$((20+3))"),
        );
        s.wait_text(&sh, "back-23");
        // Terminal, one tap; and the choice is this pane's from now on.
        page.js(&format!("{}.click()", face("Terminal")));
        page.wait(
            "the shell's terminal",
            &format!("!!document.querySelector('.phone-layout .xterm-rows') && {switch} === 'Chat Terminal*'"),
        );
        shoot(&page, "phone-layout-shell-terminal");

        // A pane by itself is the same pane.
        page.goto(&format!("#/s/alpha/p/{sh}"));
        page.wait(
            "the shell by itself, with the switch",
            "[...document.querySelectorAll('.phone-pane .chat-switch button')].length === 2",
        );
        page.js("[...document.querySelectorAll('.chat-switch button')].find(b => b.innerText === 'Chat').click()");
        page.wait(
            "the shell's conversation by itself",
            "[...document.querySelectorAll('.phone-chat .chat-screen')].some(x => x.innerText.includes('back-23'))",
        );
        shoot(&page, "phone-shell-chat");
        page.goto(&format!("#/s/alpha/p/{codex}"));
        page.wait(
            "the Codex pane by itself",
            "[...document.querySelectorAll('.phone-chat .chat-text')].some(t => t.innerText.includes('counted what you asked'))",
        );
        shoot(&page, "phone-codex-found-session");
    }

    /// The same on a computer: each cell of a layout has both faces,
    /// whatever runs in it: "Chat" on a terminal's title bar, and the
    /// conversation's bar leads back.
    #[test]
    #[ignore = "merge tier: a browser"]
    fn on_a_computer_every_cell_of_a_layout_has_chat_and_terminal() {
        let Scene { s, codex, sh } = scene();
        let page = Page::open(&s, 1400, 900);
        page.goto("#/l/pair");
        let cell = |id: &str| format!(".chat-pane[data-pane=\"{id}\"]");
        // As they open: the agent as its conversation, the shell as its
        // terminal with "Chat" on its title bar ("Chat default for agents
        // terminal for terminals").
        page.wait(
            "the agent's cell as Chat and the shell's as its terminal",
            "document.querySelectorAll('.split .chat-pane').length === 1 && document.querySelectorAll('.split .face-btn').length === 1 && document.querySelector('.split .face-btn').innerText === 'Chat' && !!document.querySelector('.split .xterm-rows')",
        );
        shoot(&page, "desktop-layout-as-it-opens");
        page.js("document.querySelector('.split .face-btn').click()");
        let (c, t) = (cell(&codex), cell(&sh));
        let seen = page.wait(
            "both cells as conversations made from the screen",
            &format!("document.querySelectorAll('.split .chat-pane').length === 2 && !!document.querySelector('{c} .chat-src') && !!document.querySelector('{t} .chat-src') && [document.querySelector('{c} .chat-src').innerText.slice(0, 60), document.querySelector('{t} .chat-src').innerText, document.querySelectorAll('.split .chat-switch').length]"),
        );
        assert_eq!(
            seen,
            json!([
                "From the screen: Codex has not said which session it is in y",
                "From the screen: it runs a shell, not an agent",
                2
            ])
        );
        shoot(&page, "desktop-layout-codex-before-a-session");
        // A message from each cell reaches its own pane.
        page.wait(
            "a command from the shell's cell",
            &say(&t, "echo reed-$((40+2))"),
        );
        s.wait_text(&sh, "reed-42");
        page.wait(
            "a message from the Codex cell",
            &say(&c, "How many otters?"),
        );
        s.wait_text(&codex, "[fake] got: How many otters?");
        let seen = page.wait(
            "what each printed, in its own cell",
            &format!("[...document.querySelectorAll('{t} .chat-screen')].some(x => x.innerText.includes('reed-42')) && [...document.querySelectorAll('{c} .chat-text:not(.chat-screen)')].some(x => x.innerText.includes('counted what you asked')) && [[...document.querySelectorAll('{t} .chat-bubble')].map(x => x.innerText), [...document.querySelectorAll('{c} .chat-bubble')].map(x => x.innerText), !!document.querySelector('{c} .chat-src')]"),
        );
        assert_eq!(
            seen,
            json!([["echo reed-$((40+2))"], ["How many otters?"], false])
        );
        shoot(&page, "desktop-layout-codex-found-session");
        // Terminal is one click back, in the cell.
        page.js(&format!("[...document.querySelectorAll('{t} .chat-switch button')].find(b => b.innerText === 'Terminal').click()"));
        page.wait(
            "the shell's cell as its terminal again",
            "document.querySelectorAll('.split .chat-pane').length === 1 && document.querySelectorAll('.split .face-btn').length === 1 && !!document.querySelector('.split .xterm-rows')",
        );
        // A pane at full size, for the pictures.
        page.js("[...document.querySelectorAll('.split .face-btn')][0].click()");
        page.goto(&format!("#/s/alpha/p/{sh}"));
        page.wait(
            "the shell's conversation at full size",
            "[...document.querySelectorAll('.zoomed .chat-screen')].some(x => x.innerText.includes('reed-42'))",
        );
        shoot(&page, "desktop-shell-chat");
        page.goto(&format!("#/s/alpha/p/{codex}"));
        page.wait(
            "the Codex conversation at full size",
            "[...document.querySelectorAll('.zoomed .chat-text')].some(x => x.innerText.includes('counted what you asked'))",
        );
        shoot(&page, "desktop-codex-found-session");
    }

    // ---- one chat for every pane (the owner's report of 16 Nov 2026) ----

    /// What every agent of the scene answers: a heading, a paragraph, a
    /// numbered list, bullets, code. The same words, so that the pictures
    /// can be laid side by side.
    const REPLY: &str = "### Herons by the weir\n\nThe survey counts **nine nests** this year, three more than last spring. Most are high in the alders on the east bank, where the water is slow and the fishing is easy.\n\n1. six on the east bank\n2. three on the island\n\n- they feed at dusk\n- `survey.md` has the dates\n\n```sh\ngrep -c heron survey.md\n```";
    const ASKED: [&str; 3] = [
        "How many herons nest by the weir?",
        "And last spring?",
        "Where do they feed?",
    ];

    pub struct Chats {
        pub s: Station,
        /// A session file of Claude Code's kind, named by its hooks.
        pub claude: String,
        /// Codex's session file, found with no hook.
        pub codex: String,
        /// Antigravity's transcript, from its own store.
        pub agy: String,
        /// An agent with no conversation to read: its screen.
        pub tui: String,
        pub sh: String,
    }

    fn id_of(v: Value) -> String {
        v["id"].as_str().unwrap().to_string()
    }

    /// One pane of each kind, each with the same three questions asked and
    /// answered, where it is an agent.
    pub fn chats() -> Chats {
        let s = Station::start("alpha");
        let dir = folder(&s, "mere");
        let said = |text: &str| json!({"transcript": {"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}}});
        let turn = |asked: &str| {
            json!([
                {"prompt": asked},
                said("I will look at the survey notes."),
                {"transcript": {"type": "assistant", "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "id": format!("toolu_{}", asked.len()), "name": "Bash", "input": {"command": "grep -c heron survey.md"}}]}}},
                {"transcript": {"type": "user", "toolUseResult": {"stdout": "9", "stderr": ""}, "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": format!("toolu_{}", asked.len()), "content": "9"}]}}},
                said(REPLY),
                {"transcript": {"type": "system", "subtype": "turn_duration", "durationMs": 15000}},
            ])
        };
        let mut steps: Vec<Value> = ASKED
            .iter()
            .flat_map(|a| turn(a).as_array().unwrap().clone())
            .collect();
        steps.push(json!({"turn_end": "ok"}));
        let claude = pane(&s, "fake", "claude", &dir, json!({"steps": steps}));
        let codex = pane(
            &s,
            "codex",
            "codex",
            &folder(&s, "weir"),
            json!({"codex": true, "no_hooks": true, "steps": [], "on_input": [
                {"codex_reply": REPLY},
                {"transcript": {"type": "event_msg", "payload": {"type": "task_complete", "duration_ms": 15000}}}]}),
        );
        s.wait_text(&codex, "[fake] session");
        for asked in ASKED {
            send(&s, &codex, asked);
        }
        // Antigravity: nothing on its screen but the program; what was
        // said is in its own store.
        let fen = folder(&s, "fen");
        let agy = drawn_pane(&s, "antigravity", "agy", &fen);
        let at = "2099-01-01T00:00:00Z";
        let steps: Vec<Value> = ASKED
            .iter()
            .enumerate()
            .flat_map(|(n, asked)| {
                let n = n as u64 * 4;
                [
                    agy_asked(n, at, asked),
                    agy_step(n + 1, "PLANNER_RESPONSE", at, json!({"content": "I will look at the survey notes.",
                        "tool_calls": [{"name": "run_command", "args": {"CommandLine": "grep -c heron survey.md"}}]})),
                    agy_step(n + 2, "GENERIC", at, json!({"content": format!("Created At: {at}\nCompleted At: {at}\n9")})),
                    agy_step(n + 3, "PLANNER_RESPONSE", at, json!({"content": REPLY})),
                ]
            })
            .collect();
        antigravity_conversation(
            &s,
            "0aaaaaaa-0000-4000-8000-00000000000a",
            &fen,
            &steps,
            &steps,
        );
        // An agent whose conversation cannot be read: its screen.
        let tui = drawn_pane(&s, "opencode", "tui", &dir);
        send_quietly(&s, &tui, ASKED[0]);
        let sh = id_of(s.call(
            "pane.spawn",
            json!({"program": "/bin/sh", "name": "shell", "cwd": dir}),
        ));
        s.call(
            "pane.paste",
            json!({"pane": sh, "text": "seq 1 40; printf '%s\\n' 'name          size     modified' 'survey.md     2 KB     today' 'a line that is much longer than a phone is wide, so that it has to be scrolled sideways inside its own block'", "enter": true}),
        );
        s.wait_text(&sh, "survey.md     2 KB");
        s.call(
            "deck.layout.save",
            json!({"layout": {"name": "pair", "tree": {"dir": "row", "ratio": 500,
                "a": {"id": claude, "station": "alpha"}, "b": {"id": tui, "station": "alpha"}}}}),
        );
        Chats {
            s,
            claude,
            codex,
            agy,
            tui,
            sh,
        }
    }

    /// A message to a program that draws its own screen: nothing to wait
    /// for but the screen drawn again.
    fn send_quietly(s: &Station, pane: &str, text: &str) {
        s.call(
            "pane.paste",
            json!({"pane": pane, "text": text, "enter": true}),
        );
        wait_for("the message taken", 10, || {
            let a = conversation(s, pane, true);
            a["items"]
                .as_array()
                .is_some_and(|i| i.iter().any(|x| x["kind"] == json!("user")))
                .then_some(())
        });
    }

    /// What makes a conversation the component it is: the classes and the
    /// drawn type of the owner's bubble, an agent's prose, the marks under
    /// it and the message box. Read from the page for the pane on screen.
    const LOOK: &str = r#"(() => {
        const root = document.querySelector('.phone-chat, .phone-layout, .zoomed .chat-pane');
        const q = (s) => root.querySelector(s);
        const css = (e, ...props) => { const c = getComputedStyle(e); return props.map(p => c[p]); };
        const bubble = q('.chat-user:not(.other) .chat-bubble'), text = [...root.querySelectorAll('.chat-text')].find(t => t.querySelector('.md h5')), prose = text.querySelector('.md p');
        const acts = text.querySelector('.chat-acts'), row = q('.chat-row');
        return {
            bubble: [bubble.className, bubble.parentNode.className, ...css(bubble, 'fontFamily', 'fontSize', 'borderTopLeftRadius', 'backgroundColor', 'color')],
            prose: [text.classList.contains('chat-text'), prose.parentNode.className, ...css(prose, 'fontFamily', 'fontSize', 'lineHeight', 'color')],
            heading: [text.querySelector('.md h5').className, ...css(text.querySelector('.md h5'), 'fontSize', 'fontWeight')],
            lists: [text.querySelector('.md ol').className, text.querySelectorAll('.md ol > li').length, text.querySelectorAll('.md ul > li').length, !!text.querySelector('.md .md-code-in')],
            acts: [acts.className, ...[...acts.children].map(b => b.className + ': ' + b.innerText), ...css(acts.firstChild, 'width', 'height')],
            composer: [q('.chat-composer').tagName, row.className, ...css(row, 'borderTopLeftRadius', 'backgroundColor'), q('.chat-box').className, q('.chat-box').placeholder,
                // (What the model chip says is the agent's own; that it is there, and where, is the component's.)
                [...q('.chat-tools').children].map(b => b.className.replace(/ ?(on|danger|primary)\b/g, '').trim() + ': ' + (b.classList.contains('chat-model') ? String(b.hidden) : b.innerText))],
            keys: !!q('.chat-keys:not([hidden])'),
            // (How full the ring is, is the agent's own; that every agent's chat has it, is the component's.)
            bar: [...root.querySelectorAll(':scope > .chat-bar > *, .chat-pane > .chat-bar > *')].map(e => e.className.replace(/ ?(working|idle|done|needs_you|error|none|count|high|full)\b/g, '')).filter(c => c !== 'chat-next'),
            copies: [root.querySelectorAll('.chat-text').length > 0, [...root.querySelectorAll('.chat-text')].every(t => t.hidden || !!t.querySelector('.chat-acts .act-copy')), [...root.querySelectorAll('.chat-user')].every(t => !!t.querySelector('.chat-acts.mine .act-copy'))],
        };
    })()"#;

    /// Where things are, and what can scroll, for the conversation on screen.
    const PLACE: &str = r#"(() => {
        const root = document.querySelector('.phone-chat, .phone-layout'), log = root.querySelector('.chat-log'), vv = window.visualViewport;
        const box = (e) => { const r = e.getBoundingClientRect(); return { top: Math.round(r.top), bottom: Math.round(r.bottom) }; };
        const last = [...log.children].filter(e => e.getBoundingClientRect().height > 0).pop();
        // Everything that could be scrolled up or down: the log, and nothing else.
        const scrollers = [document.scrollingElement, document.body, document.getElementById('app'), root, ...root.querySelectorAll('*')]
            .filter(e => e.scrollHeight > e.clientHeight + 1 && (e === document.scrollingElement || ['auto', 'scroll'].includes(getComputedStyle(e).overflowY)))
            .map(e => e.className || e.tagName);
        const wide = [...root.querySelectorAll('*')].filter(e => e.getBoundingClientRect().right > window.innerWidth + 0.5 && !e.closest('pre, .md-table')).map(e => e.className || e.tagName);
        window.scrollTo(0, 300);
        return {
            page: [document.scrollingElement.scrollTop, window.scrollY, document.documentElement.scrollWidth <= document.documentElement.clientWidth],
            log: [log.scrollHeight > log.clientHeight, log.scrollWidth <= log.clientWidth, Math.round(log.scrollHeight - log.scrollTop - log.clientHeight)],
            edge: Math.round(log.scrollTop + log.clientHeight),
            scrollers, wide,
            vv: [Math.round(vv.offsetTop), Math.round(vv.offsetTop + vv.height)],
            app: box(document.getElementById('app')), bar: box(root.querySelector(':scope > .chat-bar')), dock: box(root.querySelector('.chat-dock')), row: box(root.querySelector('.chat-row')),
            last: box(last), logBox: box(log), top: !!document.querySelector('#app > .topbar') && document.querySelector('#app > .topbar').getBoundingClientRect().height,
            kbd: document.getElementById('app').classList.contains('kbd'),
        };
    })()"#;

    /// The phone's keyboard, as the page is told of one: what is in sight
    /// becomes `tall` high and begins `top` down the page (iOS slides the
    /// page under what is in sight to show the box being typed in).
    fn keyboard(page: &Page, tall: u32, top: u32) {
        page.js(&format!(
            "(() => {{ const real = window.__vv || (window.__vv = window.visualViewport); \
               const now = {{ height: {tall}, width: window.innerWidth, offsetTop: {top}, offsetLeft: 0, pageTop: {top}, pageLeft: 0, scale: 1 }}; \
               Object.defineProperty(window, 'visualViewport', {{ configurable: true, get: () => now }}); \
               real.dispatchEvent(new Event('resize')); return true; }})()"
        ));
    }

    /// The owner's report of 16 Nov 2026, from an iPhone, with five
    /// pictures: "Antigravity chat just scrolls the entire pane and not
    /// the text inside the pane. Also they look different in chat and
    /// should be the same. Shouldn't chat be the same code base for every
    /// chat?" and, with the keyboard up, a message box in the middle of the
    /// screen over an empty gap, then at the very top with everything
    /// under it blank.
    ///
    /// At a phone's size: a pane with a session file of Claude Code's
    /// kind, a Codex pane, an Antigravity pane and an agent whose Chat is
    /// made from its screen (a program that draws 120 columns itself) are
    /// one component: the same bubble, prose, marks under it, turn marker
    /// and message box, drawn in the same type. The log is the only thing
    /// that scrolls, and nothing is wider than the phone. With the
    /// keyboard up, the message box is directly above it and the pane's
    /// bar at the top of what is in sight, wherever the browser slid the
    /// page; the page itself has not moved; and the log keeps its place.
    /// The same holds in a phone's layout view, and a cell of a layout on
    /// a computer is the same component.
    #[test]
    #[ignore = "merge tier: a browser"]
    fn every_pane_is_one_chat() {
        let c = chats();
        // A turn of the agent whose Chat is its screen: the station sees it end.
        let say = |state: &str| {
            c.s.call_as_pane(&c.tui, "pane.report_agent", json!({"state": state}))
                .unwrap()
        };
        say("working");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        say("idle");

        let page = Page::open(&c.s, 390, 844);
        // (A full Chrome's window is never as narrow as a phone.)
        page.phone_screen();
        let open = |id: &str| {
            page.goto(&format!("#/s/alpha/p/{id}"));
            page.wait(
                "the pane's conversation",
                &format!("location.hash.endsWith('{id}') && !!document.querySelector('.phone-chat .chat-user .chat-bubble') && [...document.querySelectorAll('.phone-chat .chat-text')].some(t => t.querySelector('.md h5') && t.querySelector('.chat-acts')) && [...document.querySelectorAll('.phone-chat .chat-quiet')].some(e => e.innerText.startsWith('Turn ended'))"),
            );
        };
        let mut looks = vec![];
        for (name, id) in [
            ("claude", &c.claude),
            ("codex", &c.codex),
            ("antigravity", &c.agy),
            ("screen", &c.tui),
        ] {
            open(id);
            let look = page.js(LOOK);
            assert!(
                look.is_object(),
                "{name}: the page did not say how it looks"
            );
            let place = page.js(PLACE);
            // The page cannot be scrolled, and has not been; the log can.
            assert_eq!(place["page"], json!([0, 0, true]), "{name}: {place}");
            // (The log scrolls where there is more than a screen of it.)
            let scrollers = if place["log"][0] == json!(true) {
                json!(["chat-log"])
            } else {
                json!([])
            };
            assert_eq!(place["scrollers"], scrollers, "{name}: {place}");
            assert_eq!(place["wide"], json!([]), "{name}: {place}");
            assert_eq!(place["log"][1], json!(true), "{name}: {place}");
            // On a phone the conversation has the screen: its bar is at the top.
            assert_eq!(
                (&place["top"], &place["bar"]["top"]),
                (&json!(0), &json!(0)),
                "{name}: {place}"
            );
            assert_eq!(place["dock"]["bottom"], json!(844), "{name}: {place}");
            shoot(&page, &format!("one-chat-{name}"));
            looks.push((name, look, place));
        }
        // Three conversations read from three stores, and one made from a
        // screen: one component.
        for (name, look, _) in &looks[1..] {
            assert_eq!(look, &looks[0].1, "{name} is not drawn as claude is");
        }
        assert_eq!(looks[0].1["keys"], json!(false));
        // The owner, 17 Nov 2026: "make sure this all this is for any
        // agent chat not just Claude". The ring is in every agent's bar,
        // the model chip in every message box, and copy under every reply
        // and every message of the owner's: the comparison above holds
        // them equal, and this says they are there.
        let bar = looks[0].1["bar"].as_array().unwrap();
        assert!(bar.contains(&json!("chat-ring")), "{bar:?}");
        assert!(
            looks[0].1["composer"][6]
                .as_array()
                .unwrap()
                .contains(&json!("chat-model: false")),
            "{}",
            looks[0].1["composer"]
        );
        assert_eq!(looks[0].1["copies"], json!([true, true, true]));
        assert_eq!(
            looks[0].1["acts"],
            json!([
                "chat-acts",
                "chat-act act-copy: Copy",
                "chat-act act-speak: Read aloud",
                "40px",
                "36px"
            ])
        );
        // The three with enough said have a log that scrolls.
        for (name, _, place) in &looks[..3] {
            assert_eq!(place["log"][0], json!(true), "{name}: {place}");
        }
        // What the screen's conversation is made of: prose and one block
        // of rows that are a layout, which scrolls sideways inside itself;
        // none of the program's frame, message box or status row.
        let screen = page.js("(t => [t.innerText, [...t.querySelectorAll('.chat-mono pre')].map(p => [p.innerText, getComputedStyle(p).whiteSpace, getComputedStyle(p).overflowX]), [...document.querySelectorAll('.phone-chat .chat-quiet')].map(e => e.innerText).filter(x => x.startsWith('Turn ended'))])([...document.querySelectorAll('.phone-chat .chat-screen')].find(t => t.querySelector('.md h5')))");
        let said = screen[0].as_str().unwrap();
        assert!(
            said.contains("The survey counts nine nests this year, three more than last spring. Most are high in the alders on the east bank, where the water is slow and the fishing is easy."),
            "{screen}"
        );
        for chrome in ["╭", "│", "tern-2-low", "> "] {
            assert!(!said.contains(chrome), "{chrome:?} in {screen}");
        }
        assert_eq!(
            screen[1],
            json!([["bank          nests     seen at dusk\neast          6         yes\nisland        3         no", "pre", "auto"]])
        );
        assert!(
            screen[2][0]
                .as_str()
                .unwrap()
                .starts_with("Turn ended · 1 s"),
            "{screen}"
        );

        // A shell: the same bubble and message box, and its rows as they
        // were, in the block a reply's code has, wider than the phone
        // inside itself only.
        page.goto(&format!("#/s/alpha/p/{}", c.sh));
        page.wait("the shell", &format!("location.hash.endsWith('{}') && document.querySelectorAll('.chat-switch button').length === 2", c.sh));
        page.js("[...document.querySelectorAll('.chat-switch button')].find(b => b.innerText === 'Chat').click()");
        let sh = page.wait(
            "the shell's conversation",
            "(p => p && p.innerText.includes('survey.md     2 KB') && [p.parentNode.className, p.scrollWidth > p.clientWidth, getComputedStyle(p).whiteSpace, document.querySelector('.phone-chat .chat-row').className, document.querySelector('.phone-chat .chat-bubble').className])(document.querySelector('.phone-chat .chat-screen .chat-mono pre'))",
        );
        assert_eq!(
            sh,
            json!(["md-code chat-mono", true, "pre", "chat-row", "chat-bubble"])
        );
        let place = page.js(PLACE);
        assert_eq!(
            (&place["page"], &place["scrollers"], &place["wide"]),
            (&json!([0, 0, true]), &json!(["chat-log"]), &json!([])),
            "{place}"
        );
        shoot(&page, "one-chat-shell");

        // The keyboard. First at the end of the conversation: the page is
        // slid 150 px up, 480 px are in sight.
        open(&c.claude);
        let before = page.js(PLACE);
        assert_eq!(before["log"][2], json!(0), "{before}");
        keyboard(&page, 480, 150);
        let up = page.js(PLACE);
        assert_eq!(up["vv"], json!([150, 630]), "{up}");
        assert_eq!(
            (
                &up["bar"]["top"],
                &up["dock"]["bottom"],
                &up["page"],
                &up["kbd"]
            ),
            (&json!(150), &json!(630), &json!([0, 0, true]), &json!(true)),
            "{up}"
        );
        // Nothing between the message box and the keyboard: the owner's
        // report of 17 Nov 2026 was a gap there. The card's own bottom
        // edge is the bottom of what is in sight, to the pixel.
        assert_eq!(up["row"]["bottom"], json!(630), "{up}");
        // The last thing said is still in sight above it.
        assert_eq!(up["log"][2], json!(0), "{up}");
        assert!(
            up["last"]["bottom"].as_i64() <= up["logBox"]["bottom"].as_i64()
                && up["last"]["top"].as_i64() >= up["logBox"]["top"].as_i64(),
            "{up}"
        );
        shoot(&page, "one-chat-keyboard-up");
        keyboard(&page, 844, 0);
        let down = page.js(PLACE);
        assert_eq!(
            (
                &down["bar"]["top"],
                &down["dock"]["bottom"],
                &down["log"][2],
                &down["kbd"],
                &down["page"]
            ),
            (
                &json!(0),
                &json!(844),
                &json!(0),
                &json!(false),
                &json!([0, 0, true])
            ),
            "{down}"
        );
        // Further up: the line at the bottom edge stays at the bottom edge.
        page.js("(l => { l.scrollTop = Math.round(l.scrollHeight / 2); return true; })(document.querySelector('.phone-chat .chat-log'))");
        let reading = page.js(PLACE);
        keyboard(&page, 480, 0);
        let up = page.js(PLACE);
        assert_eq!(
            (&up["edge"], &up["dock"]["bottom"], &up["bar"]["top"]),
            (&reading["edge"], &json!(480), &json!(0)),
            "{up}"
        );
        keyboard(&page, 844, 0);
        assert_eq!(page.js(PLACE)["edge"], reading["edge"]);

        // The window itself made short, as a browser that makes the page
        // smaller for its keyboard does: the same.
        page.js("(Object.defineProperty(window, 'visualViewport', { configurable: true, get: () => window.__vv }), true)");
        page.protocol(
            "Emulation.setDeviceMetricsOverride",
            json!({"width": 390, "height": 500, "deviceScaleFactor": 1, "mobile": false}),
        );
        page.wait(
            "the short window",
            "window.innerHeight === 500 && document.getElementById('app').getBoundingClientRect().height === 500",
        );
        let small = page.js(PLACE);
        assert_eq!(
            (&small["dock"]["bottom"], &small["bar"]["top"], &small["page"], &small["scrollers"]),
            (&json!(500), &json!(0), &json!([0, 0, true]), &json!(["chat-log"])),
            "{small} {}",
            page.js("[window.innerHeight, window.visualViewport.height, document.getElementById('app').style.height]")
        );
        page.phone_screen();

        // A phone's layout view: the same pane, the same component, the
        // same place for everything, and the same with the keyboard up.
        page.goto("#/l/pair");
        page.wait(
            "the layout's first pane",
            "!!document.querySelector('.phone-layout .chat-user .chat-bubble') && [...document.querySelectorAll('.phone-layout .chat-text')].some(t => t.querySelector('.md h5') && t.querySelector('.chat-acts'))",
        );
        assert_eq!(
            page.js(LOOK),
            looks[0].1,
            "in a layout it is not drawn as by itself"
        );
        let place = page.js(PLACE);
        assert_eq!(
            (
                &place["page"],
                &place["scrollers"],
                &place["wide"],
                &place["top"]
            ),
            (
                &json!([0, 0, true]),
                &json!(["chat-log"]),
                &json!([]),
                &json!(0)
            ),
            "{place}"
        );
        // (It was left halfway up, above: back to its end.)
        page.js("(l => { l.scrollTop = l.scrollHeight; return true; })(document.querySelector('.phone-layout .chat-log'))");
        keyboard(&page, 480, 150);
        let up = page.js(PLACE);
        assert_eq!(
            (
                &up["bar"]["top"],
                &up["dock"]["bottom"],
                &up["page"],
                &up["log"][2]
            ),
            (&json!(150), &json!(630), &json!([0, 0, true]), &json!(0)),
            "{up}"
        );
        assert_eq!(up["row"]["bottom"], json!(630), "in a layout: {up}");
        shoot(&page, "one-chat-layout-keyboard-up");

        // A computer: a cell of a layout is the same component.
        let wide = Page::open(&c.s, 1400, 900);
        wide.goto(&format!("#/s/alpha/p/{}", c.tui));
        wide.wait(
            "the pane at full size",
            "[...document.querySelectorAll('.zoomed .chat-text')].some(t => t.querySelector('.md h5') && t.querySelector('.chat-acts'))",
        );
        let big = wide.js(LOOK);
        for part in ["bubble", "prose", "heading", "lists", "composer"] {
            let mut theirs = looks[0].1[part].clone();
            let mut ours = big[part].clone();
            // (A computer's own sizes: the marks and the buttons are smaller.)
            if part == "composer" {
                theirs[6] = json!(null);
                ours[6] = json!(null);
            }
            assert_eq!(ours, theirs, "{part} on a computer");
        }
        assert_eq!(big["acts"][1], json!("chat-act act-copy: Copy"));
        // Where the deck's top bar is, on a computer: it stays.
        assert_eq!(wide.js("!!document.querySelector('#app > .topbar') && document.querySelector('#app > .topbar').getBoundingClientRect().height > 20"), json!(true));
        wide.goto("#/l/pair");
        wide.wait(
            "the layout",
            "document.querySelectorAll('.split .chat-pane').length === 2",
        );
        shoot(&wide, "one-chat-desktop-layout");
    }

    /// The owner's report of 17 Nov 2026, from an iPhone, with four
    /// pictures against Anthropic's own app: "On the top right, there is a
    /// circle that show usage. Can we do that? Then there is a row that
    /// show how many agents are running as well as a detail window pop
    /// that lets you see details of the sub agents/ tasks running. And a
    /// small copy icon that you can pick that copies each return to the
    /// clipboard. Also there is a model picker, can we add that for all
    /// chats ?" And: "Also can I clear the while you were away message?"
    ///
    /// At a phone's size, at the plain address a phone uses today: the
    /// ring says how full the context is; "2 running tasks" opens the
    /// sheet, with a sub-agent's transcript one tap in; copy under each
    /// reply copies that reply's own source; the recap is one line that
    /// opens, is cleared for every device and comes back from the menu;
    /// and the model chip shows what the agent reports, lists its models
    /// with a tick, sends the program's own command and then follows the
    /// agent's report.
    #[test]
    #[ignore = "merge tier: a browser"]
    fn the_ring_the_tasks_copy_notes_and_the_model_on_a_phone() {
        let prices = fake_prices::Prices::start();
        let s = Station::start_with("alpha", &prices.config(), &[]);
        let dir = folder(&s, "mere");
        let lead = pane(&s, "fake", "lead", &dir, busy_script());
        s.wait_state(&lead, "done");
        write_side(&s, &lead);
        // An agent that reports nothing has the ring all the same: empty.
        let quiet = pane(
            &s,
            "opencode",
            "quiet",
            &folder(&s, "fen"),
            json!({"no_hooks": true, "steps": []}),
        );
        let page = Page::open(&s, 390, 844);
        page.phone_screen();
        page.goto(&format!("#/s/alpha/p/{lead}"));
        let chat = ".phone-chat";

        // The ring: 700,150 of the list's 1,000,000.
        let ring = page.wait(
            "the ring",
            "(r => r && r.dataset.pct ? [r.dataset.pct, r.getAttribute('aria-label'), r.className, getComputedStyle(r.firstChild).getPropertyValue('--pct').trim(), r.getBoundingClientRect().right <= 390, r.nextSibling.className] : false)(document.querySelector('.phone-chat .chat-bar .chat-ring'))",
        );
        assert_eq!(
            ring,
            json!([
                "70",
                "Context 70% used",
                "chat-ring high",
                "70",
                true,
                "icon-btn chat-more"
            ])
        );
        // (The sheet says what the conversation's own answer brought: wait for it.)
        page.wait(
            "the conversation's answer",
            "(c => !!c && !c.hidden && c.innerText.includes('Medium'))(document.querySelector('.phone-chat .chat-model'))",
        );
        page.js("document.querySelector('.phone-chat .chat-ring').click()");
        let facts = page.wait("the ring's sheet", "(d => d && [...d.querySelectorAll('.fact')].map(f => f.innerText.replace(/\\s+/g, ' ')))(document.querySelector('.dialog.chat-sheet .usage-sheet'))");
        let facts: Vec<&str> = facts
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap())
            .collect();
        assert!(
            facts.contains(&"Model claude-opus-5-5")
                && facts.contains(&"Effort Medium")
                && facts
                    .iter()
                    .any(|f| f.starts_with("Context 700k of 1M (70% used)"))
                && facts.contains(&"Tokens out 500"),
            "{facts:?}"
        );
        shoot(&page, "extras-ring-sheet");
        page.js("document.querySelector('.dialog .icon-btn').click()");
        shoot(&page, "extras-bar-ring-tasks-chip");

        // The tasks.
        let link = page.wait(
            "the tasks link",
            "(b => b && b.innerText)(document.querySelector('.phone-chat .chat-tasks'))",
        );
        assert_eq!(link, json!("2 running tasks"));
        page.js("document.querySelector('.phone-chat .chat-tasks').click()");
        let rows = page.wait("the tasks sheet", "(l => l && l.querySelectorAll('.task-row').length === 2 && [...l.querySelectorAll('.task-row')].map(r => [r.dataset.kind, r.querySelector('b').innerText, r.querySelector('.task-what .muted').innerText.replace(/ · \\d.*$/, ''), !!r.querySelector('.task-open'), !!r.querySelector('.task-stop')]))(document.querySelector('.dialog.chat-sheet .task-list'))");
        assert_eq!(
            rows,
            json!([
                [
                    "agent",
                    "Look for other tally scripts",
                    "Agent · Explore",
                    true,
                    false
                ],
                ["shell", "cargo watch -x test", "Shell", false, false]
            ])
        );
        shoot(&page, "extras-tasks-sheet");
        page.js("document.querySelector('.task-row .task-open').click()");
        let side = page.wait("the sub-agent's transcript", "(l => l && l.querySelector('.chat-text') && [l.querySelector('.chat-bubble').innerText, l.querySelector('.chat-text .md').innerText.trim(), !!l.closest('.dialog').querySelector('.chat-box'), (r => r.height > 10 && r.top > 40 && r.bottom < 844)(l.querySelector('.chat-text .md').getBoundingClientRect())])(document.querySelector('.dialog .side-log'))");
        assert_eq!(
            side,
            json!([
                "Search the repository for scripts that count birds.",
                "One script counts birds: tally.py.",
                false,
                true
            ])
        );
        shoot(&page, "extras-task-transcript");
        page.js(
            "[...document.querySelectorAll('.dialog .icon-btn')].reverse().forEach(b => b.click())",
        );

        // Copy, at a plain address: each reply's own source, and the
        // owner's own message. (A phone reaches a station at http://, where
        // a browser has no `navigator.clipboard`: the test's own address
        // is 127.0.0.1, which has one, so it is taken away here.)
        let copied = page.js("(() => { const out = []; Object.defineProperty(navigator, 'clipboard', { configurable: true, get: () => undefined }); document.execCommand = (c) => { if (c === 'copy') out.push(document.activeElement.value); return c === 'copy'; };             for (const b of document.querySelectorAll('.phone-chat .chat-log .act-copy')) b.click();             return [!!navigator.clipboard, out]; })()");
        assert_eq!(
            copied[1],
            json!([
                "Count the herons",
                "Nine herons, counted from `survey.csv`.",
                "And **three** egrets.",
                "ok"
            ]),
            "{copied}"
        );
        page.wait(
            "it says Copied",
            "[...document.querySelectorAll('.toast')].some(t => t.innerText === 'Copied')",
        );

        // The recap: one quiet line, without Claude Code's hint; it opens.
        let note = "document.querySelector('.phone-chat .chat-note[data-note=\"away_summary\"]')";
        let line = page.wait("the recap", &format!("(n => n && [n.querySelector('.note-text').innerText, getComputedStyle(n.querySelector('.note-text')).whiteSpace, n.getBoundingClientRect().height < 50, !!n.querySelector('.chat-note-body')])({note})"));
        assert_eq!(line, json!(["While you were away: You asked for a count of the herons, and it is done: nine, with three egrets beside them. Next, you can decide whether the tally script should skip the egrets.", "nowrap", true, false]));
        shoot(&page, "extras-recap-one-line");
        page.js(&format!("{note}.querySelector('.chat-note-line').click()"));
        let open = page.wait(
            "the recap opened",
            &format!("(b => b && b.innerText)({note}.querySelector('.chat-note-body'))"),
        );
        assert!(
            open.as_str().unwrap().contains("skip the egrets.")
                && open
                    .as_str()
                    .unwrap()
                    .contains("type /config in the terminal"),
            "{open}"
        );
        // Cleared: gone here, after a reload, and on another device.
        page.js(&format!("{note}.querySelector('.chat-note-x').click()"));
        page.wait("the recap cleared", &format!("!{note}"));
        wait_for("the station keeps it", 5, || {
            (conversation(&s, &lead, true)["live"]["cleared"]
                .as_array()
                .is_some_and(|c| c.len() == 1))
            .then_some(())
        });
        page.js("(location.reload(), true)");
        page.wait("the page again", "!!document.querySelector('.phone-chat .chat-text .chat-acts') && [...document.querySelectorAll('.phone-chat .chat-quiet')].some(e => e.innerText.startsWith('Turn ended'))");
        assert_eq!(
            page.js(&format!("!!{note}")),
            json!(false),
            "after a reload"
        );
        let second = Page::open(&s, 390, 844);
        second.phone_screen();
        second.goto(&format!("#/s/alpha/p/{lead}"));
        second.wait("the conversation on a second device", "[...document.querySelectorAll('.phone-chat .chat-quiet')].some(e => e.innerText.startsWith('Turn ended'))");
        assert_eq!(
            second.js(&format!("!!{note}")),
            json!(false),
            "on a second device"
        );
        // And back, from the pane's menu.
        page.js("document.querySelector('.phone-chat .chat-more').click()");
        page.wait("the menu", "(b => b && (b.click(), true))([...document.querySelectorAll('.menu .menu-item')].find(b => b.innerText.startsWith('Show cleared notes')))");
        page.wait("the recap shown again", &format!("!!{note}"));
        page.js("document.querySelector('.phone-chat .chat-more').click()");
        page.wait("the menu", "(b => b && (b.click(), true))([...document.querySelectorAll('.menu .menu-item')].find(b => b.innerText.startsWith('Bring cleared notes back')))");
        second.wait("the recap back on the second device", &format!("!!{note}"));

        // The model chip: what the agent reports.
        let chip = "document.querySelector('.phone-chat .chat-tools .chat-model')";
        assert_eq!(page.wait("the chip", &format!("(c => c && !c.hidden && c.innerText !== 'Model' && [c.innerText, c.previousSibling.className.includes('chat-keys-btn'), c.getBoundingClientRect().width < 200])({chip})")), json!(["Opus 5.5 · Medium", true, true]));
        page.js(&format!("{chip}.click()"));
        let sheet = page.wait("the model sheet", "(d => d && d.querySelectorAll('.model-opt').length > 3 && [[...d.querySelectorAll('.model-opt')].map(o => [o.dataset.model, o.querySelector('b').innerText, o.querySelector('.ask-label .muted').innerText.length > 10, o.getAttribute('aria-checked'), o.querySelector('.model-tick').innerText]), [...d.querySelectorAll('.effort-switch button')].map(b => [b.innerText, b.getAttribute('aria-checked')]), !!d.querySelector('.model-other')])(document.querySelector('.dialog.chat-sheet .model-sheet'))");
        assert_eq!(
            sheet[0][2],
            json!(["opus", "Opus", true, "true", "✓"]),
            "{sheet}"
        );
        assert_eq!(
            sheet[0][1],
            json!(["fable", "Fable", true, "false", ""]),
            "{sheet}"
        );
        assert_eq!(
            sheet[1],
            json!([
                ["Low", "false"],
                ["Medium", "true"],
                ["High", "false"],
                ["Xhigh", "false"],
                ["Max", "false"]
            ])
        );
        assert_eq!(sheet[2], json!(true));
        shoot(&page, "extras-model-sheet");
        // Choosing sends the program's own command, and the chip waits
        // for the agent's word: it is not changed by the tap.
        page.js("document.querySelector('.model-opt[data-model=\"fable\"]').click()");
        // Found with the real program, 17 Nov 2026: its command also
        // saves the owner's default, and switching re-reads the history.
        // The sheet says both before anything is typed, and offers the
        // way that changes this pane only.
        let confirm = page.wait("what the choice changes", "(c => c && [c.querySelector('.ask-q').innerText, c.querySelector('.model-saves').innerText, c.querySelector('.model-go').innerText, c.querySelector('.model-only').innerText])(document.querySelector('.dialog .model-confirm'))");
        assert_eq!(confirm[0], json!("Switch lead to fable?"));
        let saves = confirm[1].as_str().unwrap();
        assert!(
            saves.contains("saves this as your default")
                && saves.contains("every new Claude Code session on this station")
                && saves.contains("slower and uses more tokens"),
            "{saves}"
        );
        assert_eq!(
            (&confirm[2], &confirm[3]),
            (
                &json!("Switch, and keep it as the default"),
                &json!("Only this pane: start it again and resume")
            )
        );
        assert!(
            !s.text(&lead).contains("/model"),
            "nothing is typed before the owner confirms"
        );
        shoot(&page, "extras-model-confirm");
        page.js("document.querySelector('.dialog .model-go').click()");
        s.wait_text(&lead, "[fake] got: /model fable");
        // The program's own question after spandeck's command is answered.
        s.wait_text(&lead, "[fake] model switched to Fable 5.2");
        assert_eq!(
            page.wait(
                "the chip follows the agent's report",
                &format!("(c => c.innerText.startsWith('Fable') && c.innerText)({chip})")
            ),
            json!("Fable 5.2 · High")
        );
        // Its sub-agent reported over: one task runs.
        assert_eq!(page.wait("one task left", "(b => b && b.innerText === '1 running task' && b.innerText)(document.querySelector('.phone-chat .chat-tasks'))"), json!("1 running task"));
        shoot(&page, "extras-after-model");

        // The same question when the owner typed the command in the
        // terminal: a card in Chat, and a message written meanwhile is
        // not typed into the question: it stays in the box, which says why.
        s.wait_state(&lead, "done");
        s.call(
            "pane.input",
            json!({"pane": lead, "data": "/model fable\r"}),
        );
        let card = page.wait("the card", "(c => c && [c.querySelector('b').innerText, [...c.querySelectorAll('.wait-keys button')].map(b => b.innerText)])(document.querySelector('.phone-chat .chat-card.model-ask'))");
        assert_eq!(
            card,
            json!([
                "Fake agent asks: switch model to Fable 5.2?",
                ["Yes, switch", "No, go back", "Open terminal"]
            ])
        );
        page.js(&say(chat, "count the egrets too"));
        let kept = page.wait("the message kept", "(p => p && !p.hidden && [p.innerText, document.querySelector('.phone-chat .chat-box').value])(document.querySelector('.phone-chat .send-said'))");
        assert!(
            kept[0]
                .as_str()
                .unwrap()
                .contains("is asking Switch model?")
                && kept[0].as_str().unwrap().contains("still in the box"),
            "{kept}"
        );
        assert_eq!(kept[1], json!("count the egrets too"));
        shoot(&page, "extras-switch-model-card");
        page.js("document.querySelector('.phone-chat .model-ask .model-yes').click()");
        s.wait_text(&lead, "[fake] model switched to Fable 5.2\n");
        page.wait(
            "the card gone",
            "!document.querySelector('.phone-chat .chat-card.model-ask')",
        );
        assert!(
            !s.text(&lead).contains("egrets too") && !s.text(&lead).contains("swallowed"),
            "{}",
            s.text(&lead)
        );
        page.js("(b => { b.value = ''; b.dispatchEvent(new Event('input')); return true; })(document.querySelector('.phone-chat .chat-box'))");
        page.wait("one task still", "(b => b && b.innerText === '1 running task')(document.querySelector('.phone-chat .chat-tasks'))");

        // With the keyboard up the message box, chip and all, stands on
        // the bottom edge of what is in sight.
        keyboard(&page, 480, 150);
        let up = page.js(PLACE);
        assert_eq!(
            (
                &up["row"]["bottom"],
                &up["dock"]["bottom"],
                &up["bar"]["top"],
                &up["kbd"]
            ),
            (&json!(630), &json!(630), &json!(150), &json!(true)),
            "{up}"
        );
        shoot(&page, "extras-keyboard-up");
        // The numbers the owner can read out say the same, and reach the
        // station's log.
        page.js("document.querySelector('.phone-chat .chat-more').click()");
        page.wait("the menu", "(b => b && (b.click(), true))([...document.querySelectorAll('.menu .menu-item')].find(b => b.innerText.startsWith('About this screen')))");
        let about = page.wait(
            "the numbers",
            "(o => o && JSON.parse(o.innerText))(document.querySelector('.dialog .about-out'))",
        );
        assert_eq!(
            (
                &about["under_card"],
                &about["vv"]["h"],
                &about["vv"]["top"],
                &about["dock_pad_bottom"],
                &about["kbd"]
            ),
            (
                &json!(0),
                &json!(480),
                &json!(150),
                &json!("0px"),
                &json!([true, true])
            ),
            "{about}"
        );
        page.js("document.querySelector('.dialog .about-copy').click()");
        wait_for("the report in the station's log", 5, || {
            let rows = s.call("log.query", json!({"kind": "client.report"}));
            rows.to_string().contains("layout-numbers").then_some(())
        });
        page.js("document.querySelector('.dialog .icon-btn').click()");
        keyboard(&page, 844, 0);

        // An agent that reports no usage and has only a picker: the same
        // ring, empty, which says so; and the chip, which leads to the
        // picker in its terminal.
        page.goto(&format!("#/s/alpha/p/{quiet}"));
        let bare = page.wait("the quiet agent's chat", &format!("location.hash.endsWith('{quiet}') && (r => r && (c => c && !c.hidden && [r.className, r.getAttribute('aria-label'), c.innerText])(document.querySelector('{chat} .chat-model')))(document.querySelector('{chat} .chat-ring'))"));
        assert_eq!(
            bare,
            json!(["chat-ring none", "Context not reported", "Model"])
        );
        page.js(&format!(
            "document.querySelector('{chat} .chat-model').click()"
        ));
        let picker = page.wait("its sheet", "(d => d && [d.querySelectorAll('.model-opt').length, d.querySelector('.model-picker').innerText, !!d.querySelector('.effort-switch')])(document.querySelector('.dialog.chat-sheet .model-sheet'))");
        assert_eq!(
            picker,
            json!([0, "Open its picker in the terminal (/models)", false])
        );
        shoot(&page, "extras-picker-only");
    }

    /// Pictures of each kind of pane's Chat, for a human to lay side by
    /// side (`SPANDECK_SHOTS=/some/dir`): nothing is asserted.
    #[test]
    #[ignore = "pictures only: set SPANDECK_SHOTS"]
    fn pictures_of_every_kind_of_chat() {
        let c = chats();
        let page = Page::open(&c.s, 390, 844);
        for (name, id) in [
            ("claude", &c.claude),
            ("codex", &c.codex),
            ("antigravity", &c.agy),
            ("screen-agent", &c.tui),
            ("shell", &c.sh),
        ] {
            page.goto(&format!("#/s/alpha/p/{id}"));
            page.wait(
                "the pane",
                &format!("!!document.querySelector('.chat-bar') && location.hash.endsWith('{id}')"),
            );
            page.js("(b => b && b.click())([...document.querySelectorAll('.chat-switch button')].find(b => b.innerText === 'Chat'))");
            page.wait("its conversation", "!!document.querySelector('.phone-chat .chat-log') && document.querySelectorAll('.chat-log > *').length > 1");
            shoot(&page, &format!("phone-{name}"));
            page.js("(l => { l.scrollTop = 0; })(document.querySelector('.chat-log'))");
            shoot(&page, &format!("phone-{name}-top"));
        }
        let wide = Page::open(&c.s, 1400, 900);
        wide.goto("#/l/pair");
        wide.wait(
            "the layout",
            "document.querySelectorAll('.split .chat-pane').length === 2",
        );
        shoot(&wide, "desktop-layout");
    }
}
