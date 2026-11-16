//! Phase 6 acceptance: agents working together.
mod common;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

/// A project whose roles run fake agents with the given scripts.
fn project(
    s: &Station,
    name: &str,
    roles: &[(&str, &str, Value)],
    extra: &str,
) -> std::path::PathBuf {
    let repo = s.git_repo(name);
    let mut toml = format!("name = \"{name}\"\n{extra}\n");
    for (role, placement, script) in roles {
        let path = s.script_file(script);
        toml.push_str(&format!("[roles.{role}]\nagent = \"fake\"\nplacement = \"{placement}\"\nargs = [\"--script\", '{}']\n", path.display()));
    }
    std::fs::create_dir_all(repo.join(".spandeck")).unwrap();
    std::fs::write(repo.join(".spandeck/project.toml"), toml).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "add project file"]);
    s.call("project.new", json!({"path": repo}));
    repo
}

fn agent(s: &Station, project: &str, repo: &Path, name: &str, script: Value) -> String {
    s.fake(
        script,
        json!({"name": name, "project": project, "cwd": repo}),
    )
}

fn reads_mail() -> Value {
    json!({"steps": [], "on_input": [{"rpc": {"method": "msg.inbox"}}, {"turn_end": "read my mail"}]})
}

#[test]
fn a_message_is_a_record_and_only_a_nudge_is_typed() {
    let s = Station::start("alpha");
    let repo = project(&s, "demo", &[], "");
    let reader = agent(&s, "demo", &repo, "reader", reads_mail());
    s.wait_text(&reader, "[fake] session");
    let writer = agent(
        &s,
        "demo",
        &repo,
        "writer",
        json!({"steps": [
            {"rpc": {"method": "msg.send", "params": {"to": "reader", "kind": "question", "body": "please check the heron module for races"}}}
        ]}),
    );
    s.wait_text(&writer, "[fake] msg.send ->");

    // The idle reader is woken by one typed line, then pulls the message itself.
    let screen = s.wait_text(&reader, "heron module");
    assert!(
        screen.contains("[fake] got: You have 1 new spandeck message from demo/writer@alpha. Read it with the spandeck inbox tool and act on it"),
        "the nudge tells the agent to act on its mail, not only to look: {screen}"
    );
    let typed: Vec<&str> = screen
        .lines()
        .filter(|l| l.contains("[fake] got:"))
        .collect();
    assert_eq!(
        typed.len(),
        1,
        "only the nudge was typed; the payload never was: {typed:?}"
    );
    assert!(!typed[0].contains("heron"));

    // Received, read and acted-on are tracked separately.
    let msgs = s.call("msg.recent", json!({"project": "demo"}));
    let m = &msgs[0];
    assert_eq!(
        (m["from"].as_str(), m["to"].as_str(), m["status"].as_str()),
        (
            Some("demo/writer@alpha"),
            Some("reader@alpha"),
            Some("read")
        )
    );
    s.call_as_pane(
        &reader,
        "msg.reply",
        json!({"id": m["id"], "body": "no races found"}),
    )
    .unwrap();
    assert_eq!(
        s.call("msg.thread", json!({"thread": m["thread"]}))
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        s.call("msg.thread", json!({"thread": m["thread"]}))[0]["status"],
        json!("answered")
    );
    // The ship's log records who talked to whom, not what was said.
    let log = s.log("msg.sent");
    assert!(!log.iter().any(|e| e.to_string().contains("heron")));
}

#[test]
fn a_working_agent_gets_mail_through_its_hooks_not_its_keyboard() {
    let s = Station::start("alpha");
    let repo = project(&s, "demo", &[], "");
    let worker = agent(
        &s,
        "demo",
        &repo,
        "worker",
        json!({"steps": [
            {"prompt": "long job"}, {"work": 1500}, {"tool": {"name": "Read", "file": "a.rs"}}, {"sleep": 400}, {"turn_end": "finished"}
        ]}),
    );
    s.wait_state(&worker, "working");
    s.call(
        "msg.send",
        json!({"to": "worker", "body": "also update the changelog"}),
    );
    let screen = s.wait_text(
        &worker,
        "[fake] context: You have 1 new spandeck message from you@alpha",
    );
    assert!(
        !screen.contains("[fake] got:"),
        "nothing was typed into a working agent: {screen}"
    );
}

/// Found with a real Codex on 16 Nov 2026: the nudge was typed as plain
/// text with Enter 30 ms later, and one time in five Codex did not take
/// the Enter, so the nudge sat in its input box until the next one a
/// minute later; a paste (marked as one, Enter after 60 ms) never failed.
/// So the nudge goes the way a paste goes to a program that asked for
/// pastes to be marked; and when its Enter is not taken, Enter is pressed
/// once more two seconds on. The agent here asks for marked pastes and
/// does not take the first Enter.
#[test]
fn the_nudge_is_sent_as_a_paste_and_an_enter_that_was_not_taken_is_pressed_again() {
    let s = Station::start("alpha");
    let mut script = reads_mail();
    script["paste"] = json!(true);
    script["drop_enters"] = json!(1);
    let pane = s.fake(script, json!({"name": "codexlike"}));
    s.wait_text(&pane, "[fake] session");
    s.call(
        "msg.send",
        json!({"to": "codexlike", "body": "is the egret count right?"}),
    );
    let screen = s.wait_text(&pane, "is the egret count right?");
    assert!(
        screen.contains("[fake] (that line was pasted)"),
        "the nudge came as a marked paste: {screen}"
    );
    // Once as it was typed at the prompt, once as the line that was taken.
    assert_eq!(
        screen.matches("You have 1 new spandeck message").count(),
        2,
        "typed once, not once and again on top: {screen}"
    );
    let enters = s
        .log("msg.nudge_enter")
        .iter()
        .filter(|e| e["pane"] == json!(pane))
        .count();
    assert_eq!(enters, 1, "one more Enter, once");
}

/// Found on 16 Nov 2026: a new pane given a closed pane's name inherited
/// that pane's unread mail and was nudged about it at once. A name is
/// free once its pane is closed, so the mail goes back: the sender is
/// told, the owner gets a notice, and the new pane has nothing. A pane
/// that is restarted keeps its id and its mail. And a pane a project
/// starts by name is a role: the next one the project starts under that
/// name takes the mail the last one left.
#[test]
fn a_new_pane_does_not_inherit_a_closed_panes_mail_but_a_restarted_one_and_a_projects_own_keep_theirs(
) {
    let s = Station::start("alpha");
    let never_reads = || json!({"steps": [], "on_input": [{"turn_end": "ok"}]});
    let writer = s.fake(reads_mail(), json!({"name": "writer"}));
    let reader = s.fake(never_reads(), json!({"name": "reader"}));
    s.wait_text(&reader, "[fake] session");
    s.call("pane.pause", json!({"pane": reader}));
    let sent = s
        .call_as_pane(
            &writer,
            "msg.send",
            json!({"to": "reader", "body": "how many egrets?"}),
        )
        .unwrap();
    // (Asked again for a moment: a pane just started or restarted has a
    // token the station's database has yet to be given.)
    let unread = |pane: &str| {
        wait_for("the pane's inbox", 10, || {
            s.call_as_pane(pane, "msg.inbox", json!({"peek": true}))
                .ok()
        })
        .as_array()
        .unwrap()
        .len()
    };
    // Restarted: the same pane, and its mail is still its own.
    // It is told of it once it is no longer paused, and does not read it.
    s.call("pane.restart", json!({"pane": reader, "fresh": true}));
    s.call("pane.pause", json!({"pane": reader, "paused": false}));
    s.wait_text(
        &reader,
        "[fake] got: You have 1 new spandeck message from writer@alpha",
    );

    s.call("pane.close", json!({"pane": reader}));
    let note = s.wait_text(&writer, "Not delivered to reader@alpha");
    assert!(
        note.replace('\n', "")
            .contains("its pane was closed before it read your note"),
        "{note}"
    );
    let unrelated = s.fake(reads_mail(), json!({"name": "reader"}));
    s.wait_text(&unrelated, "[fake] session");
    assert_eq!(
        unread(&unrelated),
        0,
        "another pane, though it has the name"
    );
    let row = s.call("msg.thread", json!({"thread": sent["thread"]}));
    assert_eq!(
        row[0]["status"],
        json!("undelivered: its pane was closed before it read this")
    );
    let notices = s.call("notify.list", json!({})).to_string();
    assert!(
        notices.contains("Mail for reader was not read before its pane was closed"),
        "{notices}"
    );

    // A pane the project starts by name: closed, and started again by the
    // project under a new id, it has the mail its role was sent.
    let repo = s.git_repo("demo");
    let script = s.script_file(&never_reads());
    s.call(
        "project.new",
        json!({"path": repo, "open": true, "panes": [
            {"name": "keeper", "agent": "fake", "args": ["--script", script]}
        ]}),
    );
    let keeper = |s: &Station| {
        s.call("pane.list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == json!("keeper") && p["exited"] != json!(true))
            .map(|p| p["id"].as_str().unwrap().to_string())
    };
    let first = wait_for("the project's pane", 10, || keeper(&s));
    s.call("pane.pause", json!({"pane": first}));
    s.call(
        "msg.send",
        json!({"to": "demo/keeper", "body": "keep the gate"}),
    );
    s.call("pane.close", json!({"pane": first}));
    s.call("project.open", json!({"name": "demo"}));
    let second = wait_for("the project's pane, started again", 10, || {
        keeper(&s).filter(|id| *id != first)
    });
    assert_eq!(unread(&second), 1, "the role's mail is the role's");
    // Never typed into the pane that only had the name. (It is idle for
    // long past the 1.2 s after which mail is announced.)
    assert!(
        !s.text(&unrelated).contains("new spandeck message"),
        "{}",
        s.text(&unrelated)
    );
}

#[test]
fn a_paused_agent_is_not_given_new_work_until_it_is_unpaused() {
    let s = Station::start("alpha");
    let repo = project(&s, "demo", &[], "");
    let pane = agent(&s, "demo", &repo, "idler", reads_mail());
    s.wait_text(&pane, "[fake] session");
    s.call("pane.pause", json!({"pane": pane}));
    s.call(
        "msg.send",
        json!({"to": "idler", "body": "new work for you"}),
    );
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !s.text(&pane).contains("new spandeck message"),
        "pause: no new work is delivered"
    );
    s.call("pane.pause", json!({"pane": pane, "paused": false}));
    s.wait_text(&pane, "new work for you");
}

/// Guarantee (Tasks): the states include waiting for approval and blocked.
/// A task is in them when its agent is, and back to running afterwards.
#[test]
fn a_task_says_when_it_is_waiting_for_an_approval() {
    let s = Station::start("alpha");
    let asks = json!({"steps": [{"prompt": "x"}, {"permission": {"name": "Bash", "command": "cargo publish"}}, {"work": 60000}]});
    let repo = project(&s, "demo", &[("tester", "same-tree", asks)], "");
    let lead = agent(&s, "demo", &repo, "lead", reads_mail());
    s.wait_text(&lead, "[fake] session");
    let out = s
        .call_as_pane(
            &lead,
            "delegate",
            json!({"role": "tester", "task": "publish it"}),
        )
        .unwrap();
    let task = out["task"].as_str().unwrap().to_string();
    let state = || s.call("task.get", json!({"task": task}))["state"].clone();
    wait_for("the task to wait for approval", 15, || {
        (state() == json!("waiting_for_approval")).then_some(())
    });
    let waiting = wait_for("the approval", 10, || {
        s.call("approval.list", json!({})).get(0).cloned()
    });
    s.call(
        "approval.answer",
        json!({"id": waiting["id"], "allow": true}),
    );
    wait_for("the task to run again", 15, || {
        (state() == json!("running")).then_some(())
    });
}

/// Done when: an agent going quiet does not complete its task.
#[test]
fn going_quiet_does_not_complete_a_task_only_a_result_does() {
    let s = Station::start("alpha");
    let quiet_helper = json!({"steps": [{"tool": {"name": "Bash", "command": "cargo test", "ok": true}}, {"turn_end": "all good I think"}]});
    let repo = project(&s, "demo", &[("tester", "same-tree", quiet_helper)], "");
    let lead = agent(&s, "demo", &repo, "lead", reads_mail());
    s.wait_text(&lead, "[fake] session");

    let out = s
        .call_as_pane(
            &lead,
            "delegate",
            json!({"role": "tester", "task": "run the test suite", "acceptance": "all tests pass"}),
        )
        .unwrap();
    let (task, helper) = (
        out["task"].as_str().unwrap().to_string(),
        out["pane"].as_str().unwrap().to_string(),
    );
    assert_eq!(out["workspace"]["kind"], json!("same-tree"));
    // The brief is the helper's first prompt, labelled with who asked.
    let brief = s.wait_text(&helper, "[fake] prompt:");
    assert!(
        brief.contains("working for demo/lead")
            && brief.contains("run the test suite")
            && brief.contains("all tests pass"),
        "{brief}"
    );

    s.wait_state(&helper, "done");
    std::thread::sleep(Duration::from_millis(600));
    let t = s.call("task.get", json!({"task": task}));
    assert_eq!(
        t["state"],
        json!("running"),
        "the helper went quiet; the task is not complete"
    );
    assert_eq!(
        (
            t["role"].as_str(),
            t["requester"].as_str(),
            t["station"].as_str()
        ),
        (Some("tester"), Some("demo/lead"), Some("alpha"))
    );

    // Found in review before release: an agent could start a helper in any
    // project by naming it, and so reach that project's files and keys.
    project(
        &s,
        "other",
        &[("tester", "same-tree", json!({"steps": []}))],
        "",
    );
    let err = s
        .call_as_pane(
            &lead,
            "delegate",
            json!({"role": "tester", "task": "look around", "project": "other"}),
        )
        .unwrap_err();
    assert_eq!(err.code, "denied");

    // Helpers cannot delegate further (depth 1).
    let err = s
        .call_as_pane(
            &helper,
            "delegate",
            json!({"role": "tester", "task": "do my job for me"}),
        )
        .unwrap_err();
    assert_eq!(err.code, "denied");
    // Only the pane doing the task can report on it.
    assert!(s
        .call_as_pane(
            &lead,
            "task.result",
            json!({"task": task, "summary": "done, trust me"})
        )
        .is_err());

    s.call_as_pane(
        &helper,
        "task.result",
        json!({"summary": "42 tests pass", "findings": ["flaky: test_timeout"]}),
    )
    .unwrap();
    let t = s.call("task.get", json!({"task": task}));
    assert_eq!(t["state"], json!("completed"));
    assert!(
        std::fs::read_to_string(t["outputs"]["report_file"].as_str().unwrap())
            .unwrap()
            .contains("flaky: test_timeout")
    );
    // The summary lands in the requester's inbox, and the finding on the board.
    let inbox = s.wait_text(&lead, "42 tests pass");
    assert!(inbox.contains("flaky: test_timeout"));
    let board = s
        .call_as_pane(&lead, "context.search", json!({"query": "flaky"}))
        .unwrap();
    assert_eq!(board[0]["kind"], json!("finding"));
}

/// Done when: a helper's crash leaves its worktree and a failed task with resume information.
#[test]
fn a_helper_that_crashes_leaves_its_worktree_and_a_failed_task_with_resume_info() {
    let s = Station::start("alpha");
    let crasher = json!({"steps": [{"tool": {"name": "Write", "file": "half-done.txt", "content": "work in progress"}}, {"sleep": 300}, {"exit": 3}]});
    let repo = project(&s, "demo", &[("builder", "worktree", crasher)], "");
    let out = s.call(
        "delegate",
        json!({"role": "builder", "task": "build the importer", "project": "demo"}),
    );
    let task = out["task"].as_str().unwrap().to_string();
    let ws = out["workspace"]["path"].as_str().unwrap().to_string();
    let branch = out["workspace"]["branch"].as_str().unwrap().to_string();
    assert_eq!(branch, format!("spandeck/{task}"));
    assert_ne!(
        Path::new(&ws),
        repo,
        "a writing helper gets its own worktree"
    );

    let t = wait_for("the task to fail", 15, || {
        let t = s.call("task.get", json!({"task": task}));
        (t["state"] == json!("failed")).then_some(t)
    });
    assert!(t["resume"]["reason"]
        .as_str()
        .unwrap()
        .contains("ended before it reported"));
    assert!(t["resume"]["session_id"]
        .as_str()
        .unwrap()
        .starts_with("fake-"));
    assert_eq!(t["resume"]["worktree"], json!(ws));
    assert_eq!(t["resume"]["branch"], json!(branch));
    // The worktree, its half-done work and the branch are all still there.
    assert_eq!(
        std::fs::read_to_string(Path::new(&ws).join("half-done.txt")).unwrap(),
        "work in progress"
    );
    assert!(git(&repo, &["branch", "--list", &branch]).contains(&branch));
    assert!(
        !repo.join("half-done.txt").exists(),
        "the main checkout was not touched"
    );
    assert!(s
        .call("notify.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["title"].as_str().unwrap().contains("Task failed")));
}

/// Done when: a sign-off goes stale when the code changes.
#[test]
fn a_sign_off_names_a_commit_and_goes_stale_when_the_code_changes() {
    let s = Station::start("alpha");
    let repo = project(&s, "demo", &[], "[review]\nsignoff_before_push = true\n");
    let pusher = agent(
        &s,
        "demo",
        &repo,
        "dev",
        json!({"steps": [], "on_input": [{"tool": {"name": "Bash", "command": "git push origin main"}}, {"turn_end": "tried"}]}),
    );
    s.wait_text(&pusher, "[fake] session");

    // No sign-off yet: the push is blocked before it runs.
    s.call("pane.send_text", json!({"pane": pusher, "text": "push it"}));
    let screen = s.wait_text(&pusher, "[fake] BLOCKED Bash");
    assert!(screen.contains("sign-off"));
    assert_eq!(
        s.log("order.blocked")[0]["data"]["order"],
        json!("signoff-before-push")
    );

    // Uncommitted changes cannot be signed off: a sign-off names a commit.
    std::fs::write(repo.join("wip.txt"), "x").unwrap();
    assert!(s
        .try_call("signoff.give", json!({"project": "demo"}))
        .unwrap_err()
        .message
        .contains("uncommitted"));
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "wip"]);
    let signed = s.call(
        "signoff.give",
        json!({"project": "demo", "note": "looks right"}),
    );
    assert_eq!(signed["commit"], json!(git(&repo, &["rev-parse", "HEAD"])));
    assert_eq!(
        (signed["valid"].as_bool(), signed["stale"].as_bool()),
        (Some(true), Some(false))
    );

    // With a valid sign-off the same push goes through.
    s.call(
        "pane.send_text",
        json!({"pane": pusher, "text": "push again"}),
    );
    s.wait_text(&pusher, "[fake] ran Bash (ok)");

    // The code changes: the sign-off is stale and pushes are blocked again.
    std::fs::write(repo.join("more.txt"), "y").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "more"]);
    let status = s.call("signoff.status", json!({"project": "demo"}));
    assert_eq!(
        (status["valid"].as_bool(), status["stale"].as_bool()),
        (Some(false), Some(true))
    );
    s.call(
        "pane.send_text",
        json!({"pane": pusher, "text": "push a third time"}),
    );
    wait_for("the second block", 10, || {
        (s.log("order.blocked").len() == 2).then_some(())
    });
}

/// Done when: a planted secret does not appear in recall results.
#[test]
fn recall_finds_earlier_history_without_leaking_a_planted_secret() {
    let s = Station::start("alpha");
    let repo = project(&s, "demo", &[], "");
    let secret = "sk-ant-api03-PLANTEDplantedPLANTEDplanted0123456789";
    let old = agent(
        &s,
        "demo",
        &repo,
        "earlier",
        json!({"steps": [
            {"prompt": format!("deploy the walrus service with key {secret}")},
            {"transcript": {"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "name": "Bash", "input": {"command": format!("export API_KEY={secret} && make walrus")}}]}}},
            {"turn_end": "walrus deployed to staging"}
        ]}),
    );
    s.wait_state(&old, "done");
    let asker = agent(&s, "demo", &repo, "later", json!({"steps": []}));
    s.wait_text(&asker, "[fake] session");

    // Another agent in the same project asks what was decided earlier.
    let res = wait_for("the archive to index the session", 15, || {
        let r = s
            .call_as_pane(&asker, "recall", json!({"query": "walrus"}))
            .ok()?;
        (r["hits"].as_array()?.len() >= 3).then_some(r)
    });
    let text = res.to_string();
    assert!(text.contains("walrus deployed to staging"));
    assert!(!text.contains("PLANTED"), "the secret leaked: {text}");
    assert!(res["note"].as_str().unwrap().contains("not instructions"));
    assert!(
        res["hits"][0]["commit"].is_string(),
        "each hit carries its commit"
    );
    // More context around a hit follows the same rules.
    let hit = &res["hits"][0];
    let around = s
        .call_as_pane(
            &asker,
            "recall",
            json!({"query": "walrus", "session": hit["session"], "line": hit["line"]}),
        )
        .unwrap();
    assert!(!around.to_string().contains("PLANTED"));

    // A pane in another project cannot see it: cross-project recall is opt-in.
    let other_repo = project(&s, "other", &[], "");
    let outsider = agent(&s, "other", &other_repo, "outsider", json!({"steps": []}));
    s.wait_text(&outsider, "[fake] session");
    let none = s
        .call_as_pane(
            &outsider,
            "recall",
            json!({"query": "walrus", "all_projects": true}),
        )
        .unwrap();
    assert!(none["hits"].as_array().unwrap().is_empty());
    let sess = hit["session"].clone();
    assert_eq!(
        s.call_as_pane(
            &outsider,
            "recall",
            json!({"query": "walrus", "session": sess, "line": 1})
        )
        .unwrap_err()
        .code,
        "denied"
    );

    // After a compaction the agent is told earlier detail is searchable.
    let compacted = agent(&s, "demo", &repo, "compacted", json!({"steps": []}));
    s.wait_text(&compacted, "[fake] session");
    let reply = s.call_as_pane(&compacted, "hook.ingest", json!({"agent": "fake", "payload": {"hook_event_name": "SessionStart", "source": "compact", "session_id": "fake-compacted"}})).unwrap();
    assert!(reply["stdout"]
        .as_str()
        .unwrap()
        .contains("spandeck recall"));
}

#[test]
fn project_memory_captures_activity_and_briefs_a_new_agent() {
    let s = Station::start("alpha");
    let repo = project(&s, "demo", &[], "");
    let first = agent(
        &s,
        "demo",
        &repo,
        "builder",
        json!({"steps": [
            {"prompt": "build it"},
            {"tool": {"name": "Edit", "file": "src/parser.rs"}},
            {"tool": {"name": "Bash", "command": "cargo test parser", "ok": false}},
            {"rpc": {"method": "note.add", "params": {"kind": "decision", "text": "Use the ibis tokenizer, not regex"}}},
            {"turn_end": "parser half done"}
        ]}),
    );
    s.wait_state(&first, "done");

    // Captured without the agent doing anything.
    let auto = s.call(
        "context.search",
        json!({"project": "demo", "query": "parser"}),
    );
    let texts: Vec<&str> = auto
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["text"].as_str())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("edited src/parser.rs")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("tests FAILED: cargo test parser")),
        "{texts:?}"
    );

    // A new agent starts with a capped digest: crew, decisions, recent activity.
    let second = agent(&s, "demo", &repo, "newcomer", json!({"steps": []}));
    let screen = s.wait_text(&second, "[fake] context:");
    assert!(screen.contains("project \"demo\""), "{screen}");
    assert!(
        screen.contains("builder") && screen.contains("ibis tokenizer"),
        "{screen}"
    );
    assert!(screen.contains("tests FAILED"), "{screen}");
}

#[test]
fn a_helper_gets_a_checkpoint_and_its_changes_show_as_a_read_only_diff() {
    let s = Station::start("alpha");
    let writer = json!({"steps": [{"tool": {"name": "Edit", "file": "README.md", "content": "hello\nadded by the helper\n"}}, {"turn_end": "edited"}]});
    project(&s, "demo", &[("writer", "worktree", writer)], "");
    let out = s.call(
        "delegate",
        json!({"role": "writer", "task": "extend the readme", "project": "demo"}),
    );
    let helper = out["pane"].as_str().unwrap().to_string();
    s.wait_state(&helper, "done");
    let d = s.call("diff", json!({"pane": helper}));
    assert!(
        d["diff"].as_str().unwrap().contains("+added by the helper"),
        "{d}"
    );
    assert!(d["stat"].as_str().unwrap().contains("README.md"));
    assert_eq!(d["task"], out["task"]);
    // The checkpoint is a real Git ref in the project.
    let t = s.call("task.get", json!({"task": out["task"]}));
    assert!(t["resume"]["checkpoint"].is_string());
}

fn review_roles(findings_by_round: Value) -> (Value, Value) {
    let reviewer = json!({"steps": [
        {"tool": {"name": "Bash", "command": "git show --stat HEAD", "run": true}},
        {"task_result": {"summary": "review finished", "findings_by_round": findings_by_round}},
        {"turn_end": "reported"}, {"sleep": 200}, {"exit": 0}
    ]});
    let author = json!({"steps": [], "on_input": [
        {"rpc": {"method": "msg.inbox"}},
        {"sh": "echo fix >> fixes.txt && git add -A && git commit -q -m 'address review'"},
        {"task_result": {"summary": "fixed and committed"}},
        {"turn_end": "fixed"}
    ]});
    (reviewer, author)
}

fn recipe(s: &Station, phase: &str) -> Value {
    wait_for(&format!("the review loop to reach {phase}"), 40, || {
        let r = s.call("recipe.list", json!({"project": "demo"}))[0].clone();
        (r["data"]["phase"] == json!(phase)).then_some(r)
    })
}

/// Done when: the review loop recipe finishes with two fake agents.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn the_review_loop_runs_until_clean_then_waits_for_sign_off() {
    let s = Station::start("alpha");
    let (reviewer, author) =
        review_roles(json!([["src/lib.rs:10 off-by-one in the loop bound"], []]));
    let repo = project(&s, "demo", &[("reviewer", "snapshot", reviewer)], "");
    let author_pane = agent(&s, "demo", &repo, "author", author);
    s.wait_text(&author_pane, "[fake] session");
    let first_commit = git(&repo, &["rev-parse", "HEAD"]);

    s.call("recipe.review", json!({"pane": author_pane}));
    let r = recipe(&s, "awaiting_signoff");
    assert_eq!(
        r["data"]["round"],
        json!(2),
        "one round of findings, one clean round"
    );
    // The author was told the finding through its inbox and committed a fix.
    assert!(s
        .text(&author_pane)
        .contains("off-by-one in the loop bound"));
    let fixed_commit = git(&repo, &["rev-parse", "HEAD"]);
    assert_ne!(fixed_commit, first_commit);
    assert_eq!(
        r["data"]["commit"],
        json!(fixed_commit),
        "the clean review is of the fixed commit"
    );
    // Each review read a frozen snapshot of the commit under review.
    let tasks = s.call("task.list", json!({"project": "demo"}));
    let reviews: Vec<&Value> = tasks
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["role"] == json!("reviewer"))
        .collect();
    assert_eq!(reviews.len(), 2);
    assert!(reviews
        .iter()
        .all(|t| t["workspace"]["kind"] == json!("snapshot") && t["state"] == json!("completed")));
    assert!(reviews
        .iter()
        .any(|t| t["workspace"]["commit"] == json!(first_commit)));

    // Nothing is pushed before your sign-off, even with no project setting.
    assert!(s
        .call("notify.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["title"]
            .as_str()
            .unwrap()
            .contains("ready for your sign-off")));
    s.call("signoff.give", json!({"project": "demo"}));
    let done = recipe(&s, "done");
    assert_eq!(done["state"], json!("done"));
}

#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn the_review_loop_stops_and_asks_when_the_reviewer_repeats_itself() {
    let s = Station::start("alpha");
    let same = "src/lib.rs:10 off-by-one in the loop bound";
    let (reviewer, author) = review_roles(json!([[same], [format!("{same}.")], []]));
    let repo = project(&s, "demo", &[("reviewer", "snapshot", reviewer)], "");
    let author_pane = agent(&s, "demo", &repo, "author", author);
    s.wait_text(&author_pane, "[fake] session");
    s.call("recipe.review", json!({"pane": author_pane}));
    let r = recipe(&s, "stalled");
    assert_eq!(r["data"]["round"], json!(2));
    assert!(r["data"]["note"]
        .as_str()
        .unwrap()
        .contains("same finding twice"));
    assert!(s
        .call("notify.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["title"].as_str().unwrap().contains("Review loop stopped")));
}

#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn the_review_loop_stops_at_its_round_limit() {
    let s = Station::start("alpha");
    let (reviewer, author) = review_roles(json!([
        ["a.rs:1 first problem"],
        ["b.rs:2 second problem"],
        ["c.rs:3 third problem"]
    ]));
    let repo = project(
        &s,
        "demo",
        &[("reviewer", "snapshot", reviewer)],
        "[review]\nmax_rounds = 2\n",
    );
    let author_pane = agent(&s, "demo", &repo, "author", author);
    s.wait_text(&author_pane, "[fake] session");
    s.call("recipe.review", json!({"pane": author_pane}));
    let r = recipe(&s, "limit");
    assert!(r["data"]["note"]
        .as_str()
        .unwrap()
        .contains("2 review rounds are done"));
}

#[test]
fn the_mcp_server_gives_an_agent_the_spandeck_tools() {
    let s = Station::start("alpha");
    let repo = project(&s, "demo", &[], "");
    let pane = agent(&s, "demo", &repo, "mcp-user", json!({"steps": []}));
    s.wait_text(&pane, "[fake] session");
    s.call(
        "msg.send",
        json!({"to": "mcp-user", "body": "hello over mcp"}),
    );
    let token = s.pane_token(&pane);
    let mut child = std::process::Command::new(SPANDECK)
        .arg("mcp")
        .env("SPANDECK_STATE_DIR", s.root.join("state"))
        .env("SPANDECK_CONFIG_DIR", s.root.join("config"))
        .env("SPANDECK_TOKEN", token.trim())
        .env("SPANDECK_PANE", &pane)
        .env_remove("SPANDECK_SOCKET")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |req: Value| -> Value {
        writeln!(stdin, "{req}").unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = ask(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], json!("spandeck"));
    let tools = ask(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for want in [
        "send",
        "inbox",
        "reply",
        "ack",
        "delegate",
        "task_result",
        "note_add",
        "context_search",
        "recall",
        "vault_list",
        "vault_describe",
        "vault_call",
    ] {
        assert!(names.contains(&want), "missing tool {want}");
    }
    let inbox = ask(
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "inbox", "arguments": {}}}),
    );
    assert_eq!(inbox["result"]["isError"], json!(false));
    assert!(inbox["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("hello over mcp"));
    // A tool the agent may not use comes back as a tool error, not a crash.
    let bad = ask(
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "wait", "arguments": {"pane": "nobody"}}}),
    );
    assert_eq!(bad["result"]["isError"], json!(true));
    drop(stdin);
    let _ = child.wait();
}

/// Across stations: an agent's first route needs one approval, then messages
/// are relayed; recall is answered by every station from its scrubbed copy.
#[test]
fn messages_and_recall_cross_stations() {
    let (a, b) = Station::pair();
    let repo_a = project(&a, "demo", &[], "");
    let repo_b = project(&b, "demo", &[], "");
    let sink = agent(&b, "demo", &repo_b, "sink", reads_mail());
    b.wait_text(&sink, "[fake] session");
    let sender = agent(
        &a,
        "demo",
        &repo_a,
        "sender",
        json!({"steps": [
            {"rpc": {"method": "msg.send", "params": {"to": "sink@bravo", "body": "results are in the otter branch"}}},
            {"rpc": {"method": "msg.send", "params": {"to": "sink@bravo", "body": "second message, same route"}}}
        ]}),
    );

    // The first route to another station waits for one approval.
    let route = wait_for("the route approval", 15, || {
        a.call("approval.list", json!({}))
            .as_array()
            .and_then(|l| l.first().cloned())
    });
    assert_eq!(route["kind"], json!("route"));
    assert!(route["summary"].as_str().unwrap().contains("bravo"));
    a.call("approval.answer", json!({"id": route["id"], "allow": true}));
    let screen = b.wait_text(&sink, "second message, same route");
    assert!(screen.contains("otter branch") && screen.contains("demo/sender@alpha"));
    assert_eq!(
        a.call("approval.list", json!({"all": true}))
            .as_array()
            .unwrap()
            .len(),
        1,
        "the second message needed no approval"
    );
    assert!(a.text(&sender).matches("msg.send ->").count() == 2);

    // History recorded on bravo is found from alpha, scrubbed, and labelled.
    let secret = "ghp_PLANTEDplantedPLANTEDplanted012345";
    let worker = agent(
        &b,
        "demo",
        &repo_b,
        "bravo-worker",
        json!({"steps": [{"prompt": format!("the pelican cache uses token {secret}")}, {"turn_end": "pelican cache tuned"}]}),
    );
    b.wait_state(&worker, "done");
    let hits = wait_for("recall across stations", 20, || {
        let r = a
            .call_as_pane(&sender, "recall", json!({"query": "pelican"}))
            .ok()?;
        let hits = r["hits"].as_array()?.clone();
        (!hits.is_empty()).then_some(hits)
    });
    assert!(hits.iter().all(|h| h["station"] == json!("bravo")));
    assert!(!json!(hits).to_string().contains("PLANTED"));
}

/// A bug that happened: mail that arrived while an agent was starting was
/// announced once, in a place the agent could not act on, and never again.
#[test]
fn mail_that_is_not_read_is_nudged_again_and_mail_sent_at_startup_is_not_lost() {
    // The waits between nudges are one `nudge_secs`, then two (wake.rs),
    // and the station looks once a second: with one second, the second
    // nudge comes one to two seconds after the first.
    let s = Station::start_with("alpha", "nudge_secs = 1\n", &[]);
    let repo = project(&s, "demo", &[], "");
    // This agent acknowledges input but never reads its inbox.
    let ignorer = agent(
        &s,
        "demo",
        &repo,
        "ignorer",
        json!({"steps": [], "on_input": [{"turn_end": "ok"}]}),
    );
    // Sent the instant the pane exists, before the agent has finished starting.
    s.call(
        "msg.send",
        json!({"to": "ignorer", "body": "please look at the stork ticket"}),
    );
    wait_for("a second nudge for the unread message", 20, || {
        (s.text(&ignorer)
            .matches("[fake] got: You have 1 new spandeck message from you@alpha")
            .count()
            >= 2)
            .then_some(())
    });
    assert!(
        !s.text(&ignorer).contains("stork"),
        "the payload is never typed"
    );
    // Once it is read, the nudging stops: nothing through the time in
    // which the next nudge would have come (two seconds after the last
    // one, and the second between two looks).
    s.call_as_pane(&ignorer, "msg.inbox", json!({})).unwrap();
    let count = s.text(&ignorer).matches("new spandeck message").count();
    std::thread::sleep(Duration::from_millis(3300));
    assert_eq!(
        s.text(&ignorer).matches("new spandeck message").count(),
        count
    );
}

/// Layouts, step 2 (agreed 14 Nov 2026): a pane shows the agents working
/// under it, and Watch shows every agent in a project with a line for each
/// task given and each recent message.
#[test]
fn the_deck_shows_who_works_for_whom_and_who_is_talking() {
    use common::browser::Page;
    let s = Station::start("alpha");
    let helper = json!({"steps": [
        {"subagent": {"type": "Explore"}},
        {"rpc": {"method": "msg.send", "params": {"to": "lead", "kind": "question", "body": "which branch should I test?"}}},
        {"work": 120000}
    ]});
    let repo = project(&s, "demo", &[("tester", "same-tree", helper)], "");
    let lead = agent(&s, "demo", &repo, "lead", reads_mail());
    s.wait_text(&lead, "[fake] session");
    let out = s
        .call_as_pane(
            &lead,
            "delegate",
            json!({"role": "tester", "task": "run the test suite"}),
        )
        .unwrap();
    let pane = out["pane"].as_str().unwrap().to_string();
    s.wait_text(&pane, "[fake] msg.send ->");
    // The helper's own sub-agent is known from its hooks.
    wait_for("the sub-agent", 10, || {
        (s.pane(&pane)["subagents"][0]["kind"] == json!("Explore")).then_some(())
    });

    let page = Page::open(&s, 1400, 860);
    // On the lead's pane: the agents under it, and theirs.
    page.goto(&format!("/s/alpha/p/{lead}"));
    page.wait("the agents button", "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.strip button')].find(b => b.innerText.includes('Agents 2')))");
    let drawer = page.wait(
        "the drawer",
        "document.querySelector('.pane.agents-open .agents-drawer')?.innerText",
    );
    let drawer = drawer.as_str().unwrap();
    assert!(
        drawer.contains("run the test suite")
            && drawer.contains("Explore")
            && drawer.contains("its own sub-agent"),
        "{drawer}"
    );

    // Watch: both agents, a line for the task and one for the message.
    // Watch is an entry under the project in the list, with its own address.
    page.wait(
        "Watch under the project",
        "(t => t ? (t.click(), true) : false)(document.querySelector('.side-proj .side-watch'))",
    );
    page.wait(
        "two agents",
        "location.hash === '#/s/alpha/w/demo' && document.querySelectorAll('.watch-card').length === 2 && !!document.querySelector('.side-watch.current')",
    );
    let labels = page.wait("the lines", "(x => x.length >= 2 ? x : null)([...document.querySelectorAll('.watch-map .link-label')].map(e => e.className + ' | ' + e.innerText))");
    let labels = labels.to_string();
    assert!(
        labels.contains("task | lead →") && labels.contains("task · run the test suite"),
        "{labels}"
    );
    assert!(
        labels.contains("message") && labels.contains("→ lead · question"),
        "{labels}"
    );
    // Waited for, not read once: the map is redrawn as events arrive, and a
    // read between two draws found no lines on a busy machine.
    page.wait(
        "two lines",
        "document.querySelectorAll('.watch-map .links .link').length === 2",
    );
    let feed = page.js("document.querySelector('.watch-feed').innerText");
    assert!(
        feed.as_str()
            .unwrap()
            .contains("which branch should I test?"),
        "{feed}"
    );
    // For a human to look at: SPANDECK_SHOTS=/some/dir
    if let Ok(dir) = std::env::var("SPANDECK_SHOTS") {
        use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
        std::thread::sleep(Duration::from_millis(1500));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(format!("{dir}/11-watch.png"), png).unwrap();
        page.goto(&format!("/s/alpha/p/{lead}"));
        page.wait(
            "the agents button",
            "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.strip button')].find(b => b.innerText.includes('Agents')))",
        );
        std::thread::sleep(Duration::from_millis(1200));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(format!("{dir}/12-agents-drawer.png"), png).unwrap();
    }
}

/// Project links (the owner's rules, 14 Nov 2026): only the owner links two
/// projects; across a link agents message each other and see each other's
/// findings; without one they do neither; session history stays put.
#[test]
fn linked_projects_message_and_share_findings_until_unlinked() {
    let s = Station::start("alpha");
    let deck = project(&s, "deck", &[], "");
    let vault = project(&s, "vault", &[], "");
    let keeper = agent(
        &s,
        "vault",
        &vault,
        "keeper",
        json!({"steps": [{"prompt": "plan the osprey migration"}, {"turn_end": "osprey plan drafted"}],
               "on_input": [{"rpc": {"method": "msg.inbox"}}, {"turn_end": "read my mail"}]}),
    );
    s.wait_state(&keeper, "done");
    let pilot = agent(&s, "deck", &deck, "pilot", json!({"steps": []}));
    s.wait_text(&pilot, "[fake] session");
    s.call_as_pane(
        &keeper,
        "note.add",
        json!({"kind": "finding", "text": "tokens are minted at /v1/grant"}),
    )
    .unwrap();
    let ask = json!({"to": "vault/keeper", "kind": "question", "body": "what does the puffin endpoint return?"});
    let board = |pane: &str| {
        s.call_as_pane(pane, "context.search", json!({"query": "grant"}))
            .unwrap()
    };

    // Before a link: no message across, and the other board is not seen.
    let refused = s.call_as_pane(&pilot, "msg.send", ask.clone()).unwrap_err();
    assert_eq!(refused.code, "denied", "{refused}");
    assert_eq!(board(&pilot), json!([]));

    // An agent can neither make a link nor record one as a station would.
    for method in ["project.link", "project.unlink", "project.link.record"] {
        let err = s
            .call_as_pane(&pilot, method, json!({"project": "deck", "with": "vault"}))
            .unwrap_err();
        assert_eq!(err.code, "denied", "{method}: {err}");
    }
    assert_eq!(s.call("project.links", json!({})), json!([]));

    // The owner links them; doing it twice changes nothing.
    for _ in 0..2 {
        let made = s.call("project.link", json!({"project": "deck", "with": "vault"}));
        assert_eq!(
            made,
            json!({"project": "deck", "with": "vault", "station": "alpha"})
        );
    }
    let links = s.call("project.links", json!({"project": "deck"}));
    assert_eq!(links.as_array().unwrap().len(), 1, "{links}");
    assert!(links[0]["since_ms"].is_number());
    assert_eq!(
        s.call("project.get", json!({"name": "vault"}))["links"],
        json!([{"with": "deck", "station": "alpha"}])
    );

    // The message is delivered and the reply comes back.
    s.call_as_pane(&pilot, "msg.send", ask.clone()).unwrap();
    s.wait_text(&keeper, "puffin endpoint");
    let sent = s.call("msg.recent", json!({"project": "deck"}));
    s.call_as_pane(
        &keeper,
        "msg.reply",
        json!({"id": sent[0]["id"], "body": "a signed grant, see the board"}),
    )
    .unwrap();
    let inbox = s.call_as_pane(&pilot, "msg.inbox", json!({})).unwrap();
    assert!(inbox.to_string().contains("a signed grant"), "{inbox}");

    // A finding posted in one is found from the other, labelled.
    let found = board(&pilot);
    assert_eq!(found[0]["project"], json!("vault"), "{found}");
    assert!(found[0]["text"].as_str().unwrap().contains("/v1/grant"));

    // Session history is not opened by a link.
    wait_for("the archive to index the session", 15, || {
        let r = s
            .call_as_pane(&keeper, "recall", json!({"query": "osprey"}))
            .ok()?;
        (!r["hits"].as_array()?.is_empty()).then_some(())
    });
    let theirs = s
        .call_as_pane(
            &pilot,
            "recall",
            json!({"query": "osprey", "all_projects": true}),
        )
        .unwrap();
    assert_eq!(theirs["hits"], json!([]), "{theirs}");

    // Unlinked: both stop.
    s.call(
        "project.unlink",
        json!({"project": "vault", "with": "deck"}),
    );
    assert_eq!(s.call("project.links", json!({})), json!([]));
    assert_eq!(
        s.call_as_pane(&pilot, "msg.send", ask).unwrap_err().code,
        "denied"
    );
    assert_eq!(board(&pilot), json!([]));
}

/// A link between two stations: both know it, messages cross on the link
/// alone (it is the owner's approval), findings are fetched from the other
/// station, and a search still answers when that station is gone.
#[test]
fn a_link_between_stations_is_known_on_both_and_carries_messages_and_findings() {
    let (a, mut b) = Station::pair();
    let deck = project(&a, "deck", &[], "");
    let vault = project(&b, "vault", &[], "");
    let keeper = agent(&b, "vault", &vault, "keeper", reads_mail());
    b.wait_text(&keeper, "[fake] session");
    let pilot = agent(&a, "deck", &deck, "pilot", json!({"steps": []}));
    a.wait_text(&pilot, "[fake] session");

    a.call(
        "project.link",
        json!({"project": "deck", "with": "vault", "station": "bravo"}),
    );
    let known = b.call("project.links", json!({}));
    assert_eq!(known.as_array().unwrap().len(), 1, "{known}");
    assert_eq!(
        (
            &known[0]["project"],
            &known[0]["with"],
            &known[0]["station"]
        ),
        (&json!("vault"), &json!("deck"), &json!("alpha"))
    );

    // Each way, with no route approval asked on either station.
    a.call_as_pane(
        &pilot,
        "msg.send",
        json!({"to": "vault/keeper@bravo", "body": "which fields does the gannet contract need?"}),
    )
    .unwrap();
    b.wait_text(&keeper, "gannet contract");
    b.call_as_pane(
        &keeper,
        "msg.send",
        json!({"to": "deck/pilot@alpha", "body": "name and scope, nothing else"}),
    )
    .unwrap();
    wait_for("the answer to cross", 15, || {
        let inbox = a
            .call_as_pane(&pilot, "msg.inbox", json!({"peek": true}))
            .ok()?;
        inbox.to_string().contains("name and scope").then_some(())
    });
    for s in [&a, &b] {
        assert_eq!(s.call("approval.list", json!({"all": true})), json!([]));
    }
    // Found 16 Nov 2026: the sender's copy stayed "delivered" while the
    // recipient's said "answered". Alpha's copy says what alpha knows
    // (stored at bravo; read is bravo's to say), and is answered when
    // the reply to it arrives.
    let asked = |s: &Station| {
        s.call("msg.recent", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["body"].as_str().unwrap().contains("gannet contract"))
            .cloned()
            .unwrap()
    };
    let mine = wait_for("alpha to know it was stored", 10, || {
        Some(asked(&a)).filter(|m| m["status"] == json!("delivered"))
    });
    assert_eq!(asked(&b)["status"], json!("read"));
    assert!(
        mine["status_words"]
            .as_str()
            .unwrap()
            .starts_with("stored at bravo, as alpha knows it"),
        "{mine}"
    );
    b.call_as_pane(
        &keeper,
        "msg.reply",
        json!({"id": mine["id"], "body": "name and scope, as I said"}),
    )
    .unwrap();
    wait_for("alpha's copy to be answered", 10, || {
        (asked(&a)["status"] == json!("answered")).then_some(())
    });
    assert_eq!(asked(&b)["status"], json!("answered"));

    // The link opens vault and nothing else on bravo. A pane of another
    // project there, addressed by its bare name, passes alpha's route
    // approval (alpha cannot know its project) and is refused by bravo.
    let ledger = project(&b, "ledger", &[], "");
    let clerk = agent(&b, "ledger", &ledger, "clerk", json!({"steps": []}));
    b.wait_text(&clerk, "[fake] session");
    let sent = std::thread::scope(|t| {
        let send = t.spawn(|| {
            a.call_as_pane(
                &pilot,
                "msg.send",
                json!({"to": "clerk@bravo", "body": "what is in the ledger?"}),
            )
            .unwrap()
        });
        let route = wait_for("the route approval", 15, || {
            a.call("approval.list", json!({}))
                .as_array()?
                .first()
                .cloned()
        });
        a.call("approval.answer", json!({"id": route["id"], "allow": true}));
        send.join().unwrap()
    });
    let status = wait_for("bravo to answer", 15, || {
        let m = a.call("msg.thread", json!({"thread": sent["thread"]}));
        m[0]["status"]
            .as_str()
            .filter(|s| *s != "queued")
            .map(String::from)
    });
    assert_eq!(
        status,
        "refused: project ledger is not linked with deck; only the owner can link two projects"
    );
    // A refusal used to be marked and nothing said (audit, gap 8). The
    // agent that sent it is told in its own inbox, in the same thread,
    // once; the owner gets a notice, and the ship's log an entry.
    let inbox = a
        .call_as_pane(&pilot, "msg.inbox", json!({"peek": true, "all": true}))
        .unwrap();
    let told: Vec<&Value> = inbox
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["thread"] == sent["thread"])
        .collect();
    assert_eq!(told.len(), 1, "{inbox}");
    assert_eq!(told[0]["from"], json!("spandeck@alpha"), "{inbox}");
    let words = told[0]["body"].as_str().unwrap();
    assert!(
        words.starts_with("Not delivered to clerk@bravo: project ledger is not linked with deck")
            && words.contains("what is in the ledger?"),
        "{words}"
    );
    let notices = a.call("notify.list", json!({"limit": 20}));
    assert_eq!(
        notices
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["title"] == json!("Not delivered to clerk@bravo"))
            .count(),
        1,
        "{notices}"
    );
    assert_eq!(a.log("msg.refused").len(), 1);

    // A finding posted on bravo is found from alpha.
    b.call_as_pane(
        &keeper,
        "note.add",
        json!({"kind": "finding", "text": "tokens are minted at /v1/grant"}),
    )
    .unwrap();
    let search = || {
        a.call_as_pane(&pilot, "context.search", json!({"query": "grant"}))
            .unwrap()
    };
    let found = search();
    assert_eq!(
        (&found[0]["project"], &found[0]["station"]),
        (&json!("vault"), &json!("bravo")),
        "{found}"
    );

    // With bravo gone the search still answers, and says what is missing.
    b.kill9();
    let found = search();
    assert_eq!(found.as_array().unwrap().len(), 1, "{found}");
    assert_eq!(found[0]["unreachable"], json!(true), "{found}");
    assert_eq!(found[0]["station"], json!("bravo"));
}
