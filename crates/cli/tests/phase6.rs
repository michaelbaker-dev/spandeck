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
        toml.push_str(&format!("[roles.{role}]\nagent = \"fake\"\nplacement = \"{placement}\"\nargs = [\"--script\", \"{}\"]\n", path.display()));
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
        screen.contains("[fake] got: You have mail from demo/writer@alpha"),
        "{screen}"
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
        !s.text(&pane).contains("You have mail"),
        "pause: no new work is delivered"
    );
    s.call("pane.pause", json!({"pane": pane, "paused": false}));
    s.wait_text(&pane, "new work for you");
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
    let token = String::from_utf8(
        std::process::Command::new("sqlite3")
            .arg(s.root.join("state/spandeck.db"))
            .arg(format!("SELECT token FROM panes WHERE id='{pane}'"))
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
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
    let s = Station::start_with("alpha", "nudge_secs = 2\n", &[]);
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
            .matches("You have mail from you@alpha")
            .count()
            >= 2)
            .then_some(())
    });
    assert!(
        !s.text(&ignorer).contains("stork"),
        "the payload is never typed"
    );
    // Once it is read, the nudging stops.
    s.call_as_pane(&ignorer, "msg.inbox", json!({})).unwrap();
    let count = s.text(&ignorer).matches("You have mail").count();
    std::thread::sleep(Duration::from_millis(4500));
    assert_eq!(s.text(&ignorer).matches("You have mail").count(), count);
}
