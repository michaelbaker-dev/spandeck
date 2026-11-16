//! A question the agent asks, and a plan it wants approved, answered from
//! the Control Deck (DECISIONS.md, "The conversation view", phase M3;
//! `docs/conversation-api.md`). Merge tier: one station, one agent that
//! asks nine times.
//!
//!   cargo test -p spandeck --test questions -- --ignored
//!
//! The stand-in agent asks the way Claude Code does, through `PreToolUse`
//! for `AskUserQuestion` and `ExitPlanMode`, with the two hooks spandeck
//! installs, and prints the input a hook gave the call back. Its hooks are
//! `spandeck hook`'s own code. Nothing here runs the real agent.
//!
//! The card itself is drawn in a browser in `phase5.rs`; a question that
//! waits through a restart is in `away.rs`.
#![cfg(unix)]
mod common;

use std::io::{BufRead, BufReader, Write};

use common::*;
use serde_json::{json, Value};

/// One request line on the station's own socket, with whatever the test
/// puts beside the token (`rid`, `again_ms`). The whole answer.
fn raw(s: &Station, method: &str, params: Value, beside: Value) -> Value {
    let mut stream = std::os::unix::net::UnixStream::connect(s.paths().socket()).unwrap();
    let mut req = json!({"id": 1, "token": s.token(), "method": method, "params": params});
    for (k, v) in beside.as_object().unwrap() {
        req[k] = v.clone();
    }
    writeln!(stream, "{req}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn wait_go(n: u32) -> Value {
    json!({"sh": format!("until [ -e asker.go{n} ]; do sleep 0.05; done")})
}

fn go(s: &Station, n: u32) {
    std::fs::write(s.root.join("home").join(format!("asker.go{n}")), "").unwrap();
}

/// The pane's screen with every space and line end taken out: what the
/// agent printed is longer than a line of its terminal.
fn flat(s: &Station, pane: &str) -> String {
    s.text(pane).split_whitespace().collect()
}

fn squeezed(v: &Value) -> String {
    v.to_string().split_whitespace().collect()
}

fn live(s: &Station, pane: &str) -> Value {
    s.call("pane.conversation", json!({"pane": pane}))["live"].clone()
}

/// The question or plan held for the owner now, once its item is in the
/// conversation too.
fn held(s: &Station, pane: &str, kind: &str) -> Value {
    wait_for(&format!("a held {kind}"), 15, || {
        let q = live(s, pane)["questions"].get(0).cloned()?;
        (q["kind"] == json!(kind) && q["item"].is_string()).then_some(q)
    })
}

fn item(s: &Station, pane: &str, id: &Value) -> Value {
    s.call("pane.conversation", json!({"pane": pane}))["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| &i["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

fn herons() -> Value {
    json!([
        {"header": "Species", "question": "Which herons count?", "multiSelect": false,
         "options": [{"label": "Grey herons only", "description": "Skip egrets and bitterns"},
                     {"label": "All herons", "description": "Every bird in the heron family"}]},
        {"header": "Rivers", "question": "Which rivers?", "multiSelect": true,
         "options": [{"label": "Avon"}, {"label": "Exe"}, {"label": "Tamar"}]}
    ])
}

fn one(question: &str) -> Value {
    json!([{"header": "Go on", "question": question, "multiSelect": false,
            "options": [{"label": "Yes"}, {"label": "No"}]}])
}

/// Mobile plan, phase M3 ("Answer in place"), and rule 7.
///
/// - A question answered from the deck reaches the agent as the call's own
///   input with `answers` in it, once: single choice, several choices, and
///   an answer of the owner's own.
/// - A plan approved, and a plan sent back with a note.
/// - Nobody answers in time: the prompt is the terminal's, and the pane
///   says so. "Answer in the terminal instead" does the same at once.
///   Neither answers anything for the owner.
/// - The owner's alone: an agent is refused for its own question and for
///   another pane's, cannot read it, and no list an agent or a read-only
///   caller has says what was asked.
/// - Pause, Stop and the holds: a paused agent's question is answered and
///   it stays paused; a Stop ends the hold with no answer; an answer from
///   the card releases a stopped agent as typing would.
#[test]
#[ignore = "merge tier: an agent asks ten times, and one question waits out its time"]
fn a_question_and_a_plan_are_answered_from_the_deck() {
    let s = Station::start("alpha");
    let plan = "# Tally plan\n\n1. Filter the rows to grey herons\n2. Run the tests again";
    let mut steps = vec![json!({"prompt": "tally the birds"})];
    let asks = [
        json!({"ask": {"questions": herons(), "id": "toolu_q1"}}),
        json!({"plan": {"plan": plan, "id": "toolu_p1"}}),
        // Its record holds an earlier plan, as Claude Code's can.
        json!({"plan": {"plan": plan, "id": "toolu_p2", "recorded": "# An earlier plan"}}),
        json!({"ask": {"questions": one("Lapse?"), "id": "toolu_q2"}}),
        json!({"ask": {"questions": one("Let go?"), "id": "toolu_q3"}}),
        json!({"ask": {"questions": one("While paused?"), "id": "toolu_q4"}}),
        json!({"ask": {"questions": one("Stopped?"), "id": "toolu_q5"}}),
        json!({"ask": {"questions": one("After a stop?"), "id": "toolu_q6"}}),
        json!({"ask": {"questions": one("Twice?"), "id": "toolu_q7"}}),
        json!({"permission": {"name": "Bash", "command": "rm -rf build"}}),
    ];
    for (n, ask) in asks.iter().enumerate() {
        steps.push(wait_go(n as u32 + 1));
        steps.push(ask.clone());
        steps.push(json!({"say": format!("settled {}", n + 1)}));
    }
    steps.push(json!({"turn_end": "counted"}));
    let asker = s.fake(json!({"steps": steps}), json!({"name": "asker"}));
    let other = s.fake(json!({"steps": []}), json!({"name": "other"}));
    assert!(s.call("station.info", json!({}))["caps"]
        .as_array()
        .unwrap()
        .contains(&json!("question_answer")));

    // 1. A question with a single choice and a multiple one.
    go(&s, 1);
    let q = held(&s, &asker, "question");
    let id = q["id"].clone();
    assert_eq!(q["attached"], json!(true), "{q}");
    assert_eq!(q["questions"][0]["header"], json!("Species"));
    assert_eq!(q["questions"][1]["multi"], json!(true));
    assert_eq!(q["questions"][1]["options"][2]["label"], json!("Tamar"));
    let waits = q["until_ms"].as_u64().unwrap() - q["ts_ms"].as_u64().unwrap();
    assert_eq!(
        waits, 600_000,
        "ten minutes unless the owner says otherwise"
    );
    assert!(
        q.get("tool_use_id").is_none() && q.get("detail").is_none(),
        "{q}"
    );
    let asked = item(&s, &asker, &q["item"]);
    assert_eq!(
        (&asked["kind"], &asked["status"]),
        (&json!("question"), &json!("waiting")),
        "{asked}"
    );
    // The pane needs the owner, and says which kind of thing it asks; what
    // it asks is in no list an agent or a read-only caller reads.
    let pane = s.wait_state(&asker, "needs_you");
    assert_eq!(pane["asks"]["kind"], json!("question"), "{pane}");
    assert!(
        pane["needs_reason"]
            .as_str()
            .unwrap()
            .starts_with("asks you a question: answer it in Chat (in the terminal from "),
        "{pane}"
    );
    assert_eq!(s.call("approval.list", json!({"all": true})), json!([]));
    for (method, params) in [
        ("pane.list", json!({})),
        ("pane.get", json!({"pane": asker})),
    ] {
        let seen = s.call_as_pane(&asker, method, params).unwrap().to_string();
        assert!(seen.contains("\"asks\""), "{method}");
        assert!(
            !seen.contains("heron") && !seen.contains("Avon"),
            "{method}: {seen}"
        );
    }
    for what in ["notify.list", "deck.snapshot", "log.query"] {
        let seen = s.call(what, json!({})).to_string();
        assert!(
            !seen.contains("herons count") && !seen.contains("Avon"),
            "{what}"
        );
    }
    // Rule 7: no agent answers a question, its own or another pane's, or
    // reads one by its id; an approval's call does not answer one either.
    let answer =
        json!({"id": id, "answers": [{"picked": [1]}, {"picked": [0, 2], "other": "Dart"}]});
    for pane in [&asker, &other] {
        for (method, params) in [
            ("question.answer", answer.clone()),
            ("question.answer", json!({"id": id, "release": true})),
            ("pane.conversation", json!({"pane": asker})),
            ("approval.list", json!({})),
        ] {
            let refused = s.call_as_pane(pane, method, params).unwrap_err();
            assert_eq!(refused.code, "denied", "{method}: {refused}");
        }
    }
    let wrong = s
        .try_call("approval.answer", json!({"id": id, "allow": true}))
        .unwrap_err();
    assert!(wrong.message.contains("question.answer"), "{wrong}");
    // An answer is made of the call's own choices: anything else is
    // refused, and the question still waits.
    for bad in [
        json!({"id": id}),
        json!({"id": id, "answers": [{"picked": [0]}]}),
        json!({"id": id, "answers": [{"picked": [0, 1]}, {"picked": [0]}]}),
        json!({"id": id, "answers": [{"picked": [7]}, {"picked": [0]}]}),
        json!({"id": id, "approve": true}),
    ] {
        let refused = s.try_call("question.answer", bad.clone()).unwrap_err();
        assert_eq!(refused.code, "invalid", "{bad}: {refused}");
    }
    assert_eq!(
        s.try_call(
            "question.answer",
            json!({"id": "a_nothing", "release": true})
        )
        .unwrap_err()
        .code,
        "not_found"
    );
    assert!(!s.text(&asker).contains("answered"), "still held");

    let done = s.call("question.answer", answer.clone());
    assert_eq!(
        (&done["ok"], &done["delivered"]),
        (&json!(true), &json!(true)),
        "{done}"
    );
    s.wait_text(&asker, "settled 1");
    // The call's whole input, given back with the answers in it.
    let given = json!({"questions": herons(), "answers": {
        "Which herons count?": "All herons", "Which rivers?": "Avon, Tamar, Dart"}});
    let screen = flat(&s, &asker);
    assert!(
        screen.contains(&format!(
            "[fake]questionansweredbythehook:{}",
            squeezed(&given)
        )),
        "{screen}"
    );
    // Once: a second answer finds nothing waiting.
    let again = s.try_call("question.answer", answer.clone()).unwrap_err();
    assert!(again.message.contains("no longer waiting"), "{again}");
    let asked = wait_for("the item to show the answer", 10, || {
        let i = item(&s, &asker, &q["item"]);
        (i["status"] == json!("answered")).then_some(i)
    });
    assert_eq!(
        asked["answers"],
        json!([
            ["Which herons count?", "All herons"],
            ["Which rivers?", "Avon, Tamar, Dart"]
        ])
    );
    assert!(live(&s, &asker)["questions"].as_array().unwrap().is_empty());
    assert_eq!(s.pane(&asker)["asks"], Value::Null);
    // The ship's log says that it was asked and answered, never what.
    let logged = json!([s.log("question.asked"), s.log("question.answered")]).to_string();
    assert_eq!(s.log("question.answered").len(), 1);
    assert!(
        !logged.contains("heron") && !logged.contains("Dart"),
        "{logged}"
    );

    // 2. A plan, approved: the call runs with its own input.
    go(&s, 2);
    let p = held(&s, &asker, "plan");
    assert_eq!(p["text"], json!(plan));
    assert_eq!(s.pane(&asker)["asks"]["kind"], json!("plan"));
    let refused = s
        .try_call("question.answer", json!({"id": p["id"], "answers": []}))
        .unwrap_err();
    assert_eq!(refused.code, "invalid", "{refused}");
    s.call("question.answer", json!({"id": p["id"], "approve": true}));
    s.wait_text(&asker, "settled 2");
    assert!(
        flat(&s, &asker).contains(
            "[fake]plananswered\u{200b}bythehook:{\"plan\":\"#Tallyplan\\n\\n1.Filtertherowstogreyherons\\n2.Runthetestsagain\",\"planFilePath\":\""
                .replace('\u{200b}', "")
                .as_str()
        ),
        "{}",
        s.text(&asker)
    );
    wait_for("the plan to read approved", 10, || {
        (item(&s, &asker, &p["item"])["status"] == json!("approved")).then_some(())
    });

    // 3. A plan sent back, with the owner's note for the agent.
    go(&s, 3);
    let p = held(&s, &asker, "plan");
    s.call(
        "question.answer",
        json!({"id": p["id"], "approve": false, "text": "count the egrets too"}),
    );
    s.wait_text(&asker, "settled 3");
    assert!(
        flat(&s, &asker).contains(
            "[fake]plansentback:Theownerreadthisplaninspandeckanddidnotapproveit:keepplanning,anddonotstartthework.Theowner'snote:counttheegretstoo"
        ),
        "{}",
        s.text(&asker)
    );
    let sent_back = wait_for("the plan to read not approved", 10, || {
        let i = item(&s, &asker, &p["item"]);
        (i["status"] == json!("rejected")).then_some(i)
    });
    // Found with the real Claude Code: the call's record can hold an
    // earlier plan. The item shows the plan its hook was given, which is
    // the one the owner read and answered; so does its detail.
    assert_eq!(p["text"], json!(plan));
    assert_eq!(sent_back["text"], json!(plan));
    let detail = s.call(
        "pane.conversation.detail",
        json!({"pane": asker, "item": p["item"]}),
    );
    assert_eq!(detail["text"]["text"], json!(plan));
    assert_eq!(detail["item"]["text"], json!(plan));
    assert!(detail.get("tool_use_id").is_none(), "{detail}");

    // 4. Nobody answers in time: the prompt moves to the terminal, the
    // pane says so, and nothing was answered for the owner.
    s.call("station.flags", json!({"set": {"question_hold_s": 1}}));
    go(&s, 4);
    let q = held(&s, &asker, "question");
    assert_eq!(
        q["until_ms"].as_u64().unwrap() - q["ts_ms"].as_u64().unwrap(),
        1_000
    );
    s.wait_text(&asker, "[fake] question is in the terminal");
    let pane = s.wait_state(&asker, "needs_you");
    assert_eq!(
        pane["needs_reason"],
        json!("its question is in the terminal now (nobody answered it here in time)")
    );
    let after = live(&s, &asker);
    assert_eq!(after["questions"], json!([]));
    assert_eq!(item(&s, &asker, &q["item"])["status"], json!("waiting"));
    let late = s
        .try_call(
            "question.answer",
            json!({"id": q["id"], "answers": [{"picked": [0]}]}),
        )
        .unwrap_err();
    assert!(late.message.contains("no longer waiting"), "{late}");
    assert_eq!(s.log("question.lapsed").len(), 1);
    // Found with the real Claude Code: when it shows its own prompt it
    // also asks permission for that call, and says it needs permission.
    // That is not an approval (nothing to allow or deny), is listed
    // nowhere, and does not replace the reason above.
    assert_eq!(s.call("approval.list", json!({})), json!([]));
    assert!(!s.text(&asker).contains("the permission hook answered"));
    assert_eq!(
        s.pane(&asker)["needs_reason"],
        json!("its question is in the terminal now (nobody answered it here in time)")
    );
    s.call("pane.send_text", json!({"pane": asker, "text": "Yes"}));
    s.wait_text(&asker, "[fake] question answered in the terminal: Yes");
    s.wait_text(&asker, "settled 4");
    s.call("station.flags", json!({"set": {"question_hold_s": 600}}));

    // 5. "Answer in the terminal instead": the hold ends at once.
    go(&s, 5);
    let q = held(&s, &asker, "question");
    let done = s.call("question.answer", json!({"id": q["id"], "release": true}));
    assert_eq!(done["released"], json!(true), "{done}");
    wait_for("the second prompt in the terminal", 10, || {
        (s.text(&asker)
            .matches("[fake] question is in the terminal")
            .count()
            == 2)
            .then_some(())
    });
    assert_eq!(
        s.wait_state(&asker, "needs_you")["needs_reason"],
        json!("its question is in the terminal now (you chose to answer it there)")
    );
    assert_eq!(live(&s, &asker)["questions"], json!([]));
    assert!(s
        .try_call("question.answer", json!({"id": q["id"], "release": true}))
        .is_err());
    s.call("pane.send_text", json!({"pane": asker, "text": "No"}));
    s.wait_text(&asker, "settled 5");

    // 6. Pause is "no new work": the turn goes on, its question is held
    // and answered, and the pane is still paused afterwards.
    s.call("pane.pause", json!({"pane": asker, "paused": true}));
    go(&s, 6);
    let q = held(&s, &asker, "question");
    s.call(
        "question.answer",
        json!({"id": q["id"], "answers": [{"other": "after lunch"}]}),
    );
    s.wait_text(&asker, "settled 6");
    assert!(flat(&s, &asker).contains("\"answers\":{\"Whilepaused?\":\"afterlunch\"}"));
    assert_eq!(s.pane(&asker)["paused"], json!(true));
    s.call("pane.pause", json!({"pane": asker, "paused": false}));

    // 7. Stop while a question is held: the hold ends with no answer, the
    // card goes, and the pane is held as any stopped pane is.
    go(&s, 7);
    let q = held(&s, &asker, "question");
    s.call("pane.stop", json!({"pane": asker}));
    wait_for("the stopped question to leave", 10, || {
        (live(&s, &asker)["questions"] == json!([])).then_some(())
    });
    assert!(s
        .try_call(
            "question.answer",
            json!({"id": q["id"], "answers": [{"picked": [0]}]})
        )
        .is_err());
    assert!(!s.pane(&asker)["held"]["stopped"].is_null());
    // The stand-in agent, unlike Claude Code, is not interrupted by the
    // key while its hook runs: it shows its prompt. Typing releases.
    wait_for("the third prompt in the terminal", 10, || {
        (s.text(&asker)
            .matches("[fake] question is in the terminal")
            .count()
            == 3)
            .then_some(())
    });
    s.call("pane.send_text", json!({"pane": asker, "text": "Yes"}));
    s.wait_text(&asker, "settled 7");
    assert_eq!(s.pane(&asker)["held"], Value::Null);

    // 8. A stopped agent that asks (its own turn went on): answering from
    // the card is the owner's input, as typing the answer would be, so it
    // releases the pane. It never ends a Pause (6).
    s.call("pane.stop", json!({"pane": asker}));
    assert!(!s.pane(&asker)["held"]["stopped"].is_null());
    go(&s, 8);
    let q = held(&s, &asker, "question");
    s.call(
        "question.answer",
        json!({"id": q["id"], "answers": [{"picked": [1]}]}),
    );
    s.wait_text(&asker, "settled 8");
    assert_eq!(s.pane(&asker)["held"], Value::Null);
    assert_eq!(s.log("pane.released").len(), 2, "typed, then answered");

    // 9. The deck tries again because the answer to its first try was
    // lost: by the request's id and the question's own record it is told
    // "done", and the agent is given one answer.
    go(&s, 9);
    let q = held(&s, &asker, "question");
    let params = json!({"id": q["id"], "answers": [{"picked": [0]}]});
    let ask = |params: &Value, beside: Value| raw(&s, "question.answer", params.clone(), beside);
    let first = ask(&params, json!({"rid": "question-answer-0001"}));
    assert_eq!(first["result"]["delivered"], json!(true), "{first}");
    let second = ask(
        &params,
        json!({"rid": "question-answer-0001", "again_ms": 300}),
    );
    assert_eq!(second["result"]["repeat"], json!(true), "{second}");
    // Another answer, tried "again": not the first one's, and refused.
    let mut other_answer = params.clone();
    other_answer["answers"] = json!([{"picked": [1]}]);
    let third = ask(
        &other_answer,
        json!({"rid": "question-answer-0002", "again_ms": 300}),
    );
    assert!(third.get("error").is_some(), "{third}");
    s.wait_text(&asker, "settled 9");
    assert!(flat(&s, &asker).contains("\"answers\":{\"Twice?\":\"Yes\"}"));

    // 10. Found with the real Claude Code: a permission request whose
    // prompt the owner answered in the terminal stayed listed as waiting.
    // When the tool call it was for has run (its hook says so), the
    // request is over and leaves the list; a call of another command
    // does not end it.
    go(&s, 10);
    let waiting = || s.call("approval.list", json!({}));
    let asked = wait_for("the permission request", 10, || waiting().get(0).cloned());
    assert_eq!(
        (&asked["tool"], &asked["summary"]),
        (&json!("Bash"), &json!("Bash: rm -rf build")),
        "a row says what it is: {asked}"
    );
    let ran = |command: &str| {
        s.call_as_pane(
            &asker,
            "hook.ingest",
            json!({"agent": "fake", "payload": {"hook_event_name": "PostToolUse", "tool_name": "Bash",
                "tool_input": {"command": command}, "tool_use_id": "toolu_x", "tool_response": {}}}),
        )
        .unwrap();
    };
    ran("ls");
    assert_eq!(waiting().as_array().unwrap().len(), 1, "another call");
    ran("rm -rf build");
    assert_eq!(waiting(), json!([]));
    s.wait_text(&asker, "[fake] permission: none");
    assert!(s
        .try_call("approval.answer", json!({"id": asked["id"], "allow": true}))
        .is_err());

    s.wait_state(&asker, "done");
    assert_eq!(
        s.text(&asker).matches("answered by the hook").count(),
        5,
        "each answer once: {}",
        s.text(&asker)
    );
    assert_eq!(s.log("question.asked").len(), 9);
}
