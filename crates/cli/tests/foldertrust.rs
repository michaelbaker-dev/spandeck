//! The folder-trust question (DECISIONS.md, "The folder-trust question"):
//! the owner's request of 16 Nov 2026, and the boundary around it. The
//! stand-in agent draws each program's screen as that program draws it
//! (agents/foldertrust.rs), in a pane of that program's kind.
//!
//! Two scenarios run with every change. The ones that wait to see that
//! nothing is pressed, or that start more than one station, are in the
//! merge tier (`--ignored`).

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

const TRUSTED: &str = "[fake] folder trusted";
const ASKS: &str = "asks whether to trust this folder: answer in the terminal";

fn folder(s: &Station, name: &str) -> PathBuf {
    let dir = s.root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    // Windows spells a resolved path with a `\\?\` in front; the station
    // names folders without it.
    let full = dir.canonicalize().unwrap();
    match full.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(plain) => PathBuf::from(plain),
        None => full,
    }
}

/// What a pane of `agent` is asked to run: the stand-in, showing `screen`
/// where the real program asks, then idling at its prompt.
fn program(s: &Station, screen: Value, steps: Value) -> Value {
    let script = s.script_file(
        &json!({"trust_screen": screen, "steps": steps, "on_input": [{"turn_end": "ok"}]}),
    );
    json!({"program": FAKE, "args": ["--script", script]})
}

/// Start a pane of `agent` in `dir` whose program asks about the folder.
fn start(s: &Station, agent: &str, dir: &Path, extra: Value) -> String {
    let mut params = program(s, json!({"as": agent}), json!([]));
    params["agent"] = json!(agent);
    params["cwd"] = json!(dir);
    for (k, v) in extra.as_object().unwrap() {
        params[k] = v.clone();
    }
    s.call("pane.spawn", params)["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn trust(s: &Station, pane: &str) -> Value {
    s.pane(pane)["trust"].clone()
}

/// The pane says the question is on its screen and nothing was pressed.
fn wait_asks(s: &Station, pane: &str) -> Value {
    wait_for_or(
        "the pane to say it asks about the folder",
        20,
        || format!("{}\n{}", s.pane(pane), s.text(pane)),
        || {
            let p = s.pane(pane);
            (!p["trust"]["asks"].is_null()).then_some(p)
        },
    )
}

fn answers(s: &Station, pane: &str) -> Vec<Value> {
    s.log("pane.trust_answered")
        .into_iter()
        .filter(|e| e["pane"] == json!(pane))
        .collect()
}

fn listed(s: &Station) -> Vec<(String, String)> {
    s.call("foldertrust.list", json!({}))["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["folder"].as_str().unwrap().to_string(),
                f["agent"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The owner's request (16 Nov 2026): "auto approve when a agent starts to
/// trust this folder question". Ticked: the question is answered once, by
/// the key the owner would press, it is in the ship's log, and the agent
/// goes on. Not ticked: nothing is pressed, the pane says so, and the
/// owner's one tap answers it. The remembered folders are listed and can
/// be taken off the list.
#[test]
fn a_ticked_folder_is_answered_once_and_an_unticked_one_waits_for_the_owner() {
    let s = Station::start("alpha");
    let (yes, no) = (folder(&s, "ticked"), folder(&s, "plain"));

    // Before the pane is started, the dialog is told what the tick means.
    let ask = |dir: &Path| {
        s.call(
            "foldertrust.choice",
            json!({"agent": "antigravity", "cwd": dir}),
        )
    };
    let before = ask(&yes);
    assert_eq!(before["asks"], json!(true));
    assert_eq!(before["on"], json!(false), "off for a folder never used");
    assert_eq!(before["folder"], json!(yes));
    assert!(before["allows"]
        .as_str()
        .unwrap()
        .contains("read, edit and run"));
    for agent in ["shell", "opencode"] {
        let c = s.call("foldertrust.choice", json!({"agent": agent, "cwd": yes}));
        assert_eq!(c["asks"], json!(false), "{agent}: no tick box");
    }

    let ticked = start(&s, "antigravity", &yes, json!({"trust_folder": true}));
    let plain = start(&s, "antigravity", &no, json!({}));

    // Ticked: answered, and the agent's session starts.
    s.wait_text(&ticked, TRUSTED);
    s.wait_text(&ticked, "[fake] session");
    let log = wait_for("the answer in the ship's log", 10, || {
        Some(answers(&s, &ticked)).filter(|a| !a.is_empty())
    });
    assert_eq!(log.len(), 1, "{log:?}");
    let data = &log[0]["data"];
    assert_eq!(data["how"], json!("standing"));
    assert_eq!(data["folder"], json!(yes));
    assert_eq!(data["agent"], json!("antigravity"));
    assert_eq!(data["started_by"], json!("you"));
    assert_eq!(log[0]["actor"], json!("spandeck"));
    let t = trust(&s, &ticked);
    assert_eq!(t["answered"]["how"], json!("standing"), "{t}");
    assert!(t["asks"].is_null());
    // It is not left at "needs you" for a question nobody has to answer,
    // and the owner was sent no notice about it.
    assert_ne!(s.pane(&ticked)["needs_reason"], json!(ASKS));
    let notices = s.call("notify.list", json!({}));
    assert!(
        !notices
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["pane"] == json!(ticked) && n["kind"] == json!("needs_you")),
        "a notice for a question spandeck answered: {notices}"
    );
    // The choice is remembered for that folder and that program only.
    assert_eq!(
        listed(&s),
        vec![(yes.display().to_string(), "antigravity".to_string())]
    );
    assert_eq!(ask(&yes)["on"], json!(true));
    assert_eq!(ask(&yes)["why"], json!("folder"));
    assert_eq!(
        s.call("foldertrust.choice", json!({"agent": "claude", "cwd": yes}))["on"],
        json!(false)
    );

    // Not ticked: nothing is pressed; the pane says what it waits for.
    let p = wait_asks(&s, &plain);
    assert_eq!(p["state"], json!("needs_you"));
    assert_eq!(p["needs_reason"], json!(ASKS));
    assert_eq!(p["trust"]["asks"]["why"], json!("not_chosen"));
    assert_eq!(p["trust"]["asks"]["button"], json!(true));
    assert!(!s.text(&plain).contains(TRUSTED));
    assert!(answers(&s, &plain).is_empty());

    // An agent cannot tap, start a pane with the tick, read the list or
    // change it: none of it is on an agent's list (rule 7).
    for (method, params) in [
        ("pane.trust_folder", json!({"pane": plain})),
        (
            "pane.spawn",
            json!({"agent": "antigravity", "cwd": no, "trust_folder": true}),
        ),
        ("foldertrust.list", json!({})),
        ("foldertrust.choice", json!({"agent": "claude"})),
        (
            "foldertrust.remove",
            json!({"folder": yes, "agent": "antigravity"}),
        ),
        ("foldertrust.project", json!({"project": "x", "on": true})),
    ] {
        let err = s.call_as_pane(&ticked, method, params).unwrap_err();
        assert!(
            err.to_string().contains("denied") || err.to_string().contains("not allowed"),
            "{method}: {err}"
        );
    }
    assert!(!s.text(&plain).contains(TRUSTED));

    // The owner's one tap, remembered because they said so.
    s.call(
        "pane.trust_folder",
        json!({"pane": plain, "remember": true}),
    );
    s.wait_text(&plain, TRUSTED);
    let log = answers(&s, &plain);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["data"]["how"], json!("tap"));
    assert!(trust(&s, &plain)["asks"].is_null());
    assert_eq!(listed(&s).len(), 2);
    // A second tap has nothing to answer.
    let again = s
        .try_call("pane.trust_folder", json!({"pane": plain}))
        .unwrap_err();
    assert!(again.to_string().contains("not showing"), "{again}");

    // Taking a folder off the list: the next pane there is asked again.
    let gone = s.call(
        "foldertrust.remove",
        json!({"folder": yes, "agent": "antigravity"}),
    );
    assert_eq!(gone["removed"], json!(1));
    assert_eq!(
        listed(&s),
        vec![(no.display().to_string(), "antigravity".to_string())]
    );
    assert_eq!(ask(&yes)["on"], json!(false));
    assert_eq!(s.log("foldertrust.removed").len(), 1);
    assert_eq!(s.log("foldertrust.remembered").len(), 2);

    // Codex's question is read since 18 Nov 2026 (its real screen was
    // captured for the owner's request that a pair starts without asking):
    // a ticked folder is answered, with the key Codex's own choice takes.
    let codex_dir = folder(&s, "codex-ticked");
    let codex = start(&s, "codex", &codex_dir, json!({"trust_folder": true}));
    s.wait_text(&codex, TRUSTED);
    let log = wait_for("Codex's answer in the ship's log", 10, || {
        Some(answers(&s, &codex)).filter(|a| !a.is_empty())
    });
    assert_eq!(log[0]["data"]["agent"], json!("codex"), "{log:?}");
}

/// Claude Code's own choice on its screen is "No, exit" (recorded from
/// 2.1.323): Enter alone would leave. The owner's decision of 16 Nov 2026
/// ("Make spandeck fully automatic with codex and Claude or any agent"):
/// for a ticked folder spandeck moves the marker itself, sees that it
/// moved, and only then confirms. With `folder_trust.move = false` it
/// presses nothing there; the pane says why, and the owner's one tap does
/// the same three things. A screen that is not the known one is never
/// answered on a standing choice either way.
#[test]
fn claude_codes_marker_is_moved_for_a_ticked_folder_and_an_unknown_screen_is_left_alone() {
    let s = Station::start("alpha");
    // A ticked folder whose program words its question differently, and
    // one where Claude Code lists what the folder's settings switch on:
    // nothing is pressed for either.
    let odd = folder(&s, "odd");
    let mut reworded = program(
        &s,
        json!({"as": "antigravity", "reword": "Do you trust the files in this folder?"}),
        json!([]),
    );
    reworded["agent"] = json!("antigravity");
    reworded["cwd"] = json!(odd);
    reworded["trust_folder"] = json!(true);
    let reworded = s.call("pane.spawn", reworded)["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut warned = program(
        &s,
        json!({"as": "claude", "focus": "trust", "extra": [
            "This folder pre-approves 2 tool permissions in .claude/settings.json:",
            "  Bash(curl *), Bash(sh *)",
            "These will apply without asking. Only proceed if you trust this configuration."]}),
        json!([]),
    );
    warned["agent"] = json!("claude");
    warned["cwd"] = json!(odd);
    warned["trust_folder"] = json!(true);
    let warned = s.call("pane.spawn", warned)["id"]
        .as_str()
        .unwrap()
        .to_string();

    // As it is unless the owner says otherwise: answered, by moving the
    // marker and confirming, and the program goes on.
    let auto_dir = folder(&s, "claude-auto");
    let c = s.call(
        "foldertrust.choice",
        json!({"agent": "claude", "cwd": auto_dir}),
    );
    assert_eq!(c["pressed"], json!(true), "the dialog promises no tap");
    let auto = start(&s, "claude", &auto_dir, json!({"trust_folder": true}));
    s.wait_text(&auto, TRUSTED);
    s.wait_text(&auto, "[fake] session");
    assert!(!s.text(&auto).contains("left: the folder was not trusted"));
    assert_eq!(answers(&s, &auto).len(), 1);
    assert_eq!(answers(&s, &auto)[0]["data"]["how"], json!("standing"));
    assert!(s.pane(&auto)["exit"].is_null());

    // The owner's switch against it: nothing is pressed, and the tap is
    // offered.
    s.call(
        "station.flags",
        json!({"set": {"folder_trust.move": false}}),
    );
    let dir = folder(&s, "claude-new");
    let pane = start(&s, "claude", &dir, json!({"trust_folder": true}));
    let p = wait_asks(&s, &pane);
    assert_eq!(p["trust"]["asks"]["why"], json!("own_choice_is_no"), "{p}");
    assert_eq!(p["trust"]["asks"]["button"], json!(true));
    assert_eq!(p["needs_reason"], json!(ASKS));
    assert!(answers(&s, &pane).is_empty());
    let c = s.call("foldertrust.choice", json!({"agent": "claude", "cwd": dir}));
    assert_eq!(c["on"], json!(true));
    assert_eq!(
        c["pressed"],
        json!(false),
        "the dialog says a tap is needed"
    );

    s.call("pane.trust_folder", json!({"pane": pane}));
    s.wait_text(&pane, TRUSTED);
    s.wait_text(&pane, "[fake] session");
    assert!(!s.text(&pane).contains("left: the folder was not trusted"));
    assert_eq!(answers(&s, &pane)[0]["data"]["how"], json!("tap"));
    assert!(s.pane(&pane)["exit"].is_null());

    // The two odd screens, which have stood all this while.
    for pane in [&reworded, &warned] {
        let p = wait_asks(&s, pane);
        assert_eq!(
            p["trust"]["asks"]["why"],
            json!("not_the_known_screen"),
            "{p}"
        );
        assert!(!s.text(pane).contains(TRUSTED));
        assert!(answers(&s, pane).is_empty());
    }
}

/// What must never be answered, each watched for long enough that a key
/// would have been pressed: the same words later in a session, the known
/// screen first shown long after the program started, Codex's review of
/// hooks, a permission prompt, a second question in the same pane, a pane
/// the owner has typed into, and a folder taken off the list while its
/// pane waited.
#[test]
#[ignore = "merge tier: waits several times for a key that must not be pressed"]
fn nothing_but_the_first_screen_of_a_chosen_folder_is_answered() {
    // Two of these panes wait with their marker on "No, exit" for what
    // the owner does next, so moving it for them is switched off here.
    let s = Station::start_with("alpha", "[flags]\nfolder_trust.move = false\n", &[]);
    let a_key_would_have_come = || std::thread::sleep(Duration::from_millis(3000));
    let spawn = |agent: &str, dir: &Path, script: Value, extra: Value| {
        let path = s.script_file(&script);
        let mut params = json!({"agent": agent, "program": FAKE, "args": ["--script", path], "cwd": dir, "trust_folder": true});
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        s.call("pane.spawn", params)["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let dir = folder(&s, "chosen");

    // The whole screen again, later in the session: the agent quoting it,
    // and then the program really asking a second time.
    let later = spawn(
        "antigravity",
        &dir,
        json!({"trust_screen": {"as": "antigravity"}, "steps": [
            {"trust_screen": {"as": "antigravity", "wait": false}},
            {"sleep": 2500},
            {"trust_screen": {"as": "antigravity"}}]}),
        json!({}),
    );
    // The known screen for a chosen folder, with no hook before it, but
    // first shown 21 seconds after the program started: not its first
    // screen, so it is not read at all.
    let late = spawn(
        "antigravity",
        &dir,
        json!({"trust_screen": {"as": "antigravity", "after_ms": 21000}, "steps": []}),
        json!({}),
    );
    // No hooks at all (a program that reports nothing), the screen as its
    // only content, well inside the first 20 seconds.
    let hooks = spawn(
        "codex",
        &dir,
        json!({"trust_screen": {"as": "codex-hooks"}, "steps": []}),
        json!({}),
    );
    let hooks_in_agy = spawn(
        "antigravity",
        &dir,
        json!({"trust_screen": {"as": "codex-hooks"}, "steps": []}),
        json!({}),
    );
    let permission = spawn(
        "claude",
        &dir,
        json!({"steps": [{"say": "Do you want to proceed?"}, {"say": "❯ 1. Yes"}, {"say": "  2. No"}, {"sleep": 60000}]}),
        json!({}),
    );
    // The owner is at the keyboard of this one: spandeck steps back.
    let typed_dir = folder(&s, "typed");
    let typed = spawn(
        "antigravity",
        &typed_dir,
        json!({"trust_screen": {"as": "antigravity", "focus": "exit"}, "steps": []}),
        json!({}),
    );
    wait_asks(&s, &typed);
    // (Up: the marker is on the trusting choice now, and the screen is
    // the known one for a chosen folder.)
    s.call("pane.input", json!({"pane": typed, "data": "\u{1b}[A"}));
    // Taken off the list while its pane waits at a screen spandeck would
    // otherwise answer after the owner moved the marker.
    let revoked_dir = folder(&s, "revoked");
    let revoked = spawn(
        "antigravity",
        &revoked_dir,
        json!({"trust_screen": {"as": "antigravity", "focus": "exit"}, "steps": []}),
        json!({}),
    );
    wait_asks(&s, &revoked);
    s.call(
        "foldertrust.remove",
        json!({"folder": revoked_dir, "agent": "antigravity"}),
    );

    s.wait_text(&later, TRUSTED);
    a_key_would_have_come();
    a_key_would_have_come();

    let screen = s.text(&later);
    assert_eq!(answers(&s, &later).len(), 1, "once, at the start");
    assert_eq!(screen.matches(TRUSTED).count(), 1, "{screen}");
    assert!(
        screen.contains("Do you trust the contents of this project?"),
        "the second question still stands: {screen}"
    );
    for pane in [&hooks, &hooks_in_agy] {
        let screen = s.text(pane);
        assert!(screen.contains("Hooks need review"), "{screen}");
        assert!(!screen.contains("[fake] hooks:"), "{screen}");
        assert!(trust(&s, pane)["asks"].is_null(), "not a folder's trust");
    }
    assert!(s.text(&permission).contains("Do you want to proceed?"));
    assert!(trust(&s, &permission)["asks"].is_null());
    for pane in [&typed, &revoked] {
        assert!(!s.text(pane).contains(TRUSTED), "{}", s.text(pane));
        assert!(
            !s.text(pane).contains("left: the folder"),
            "{}",
            s.text(pane)
        );
    }
    assert_eq!(trust(&s, &typed)["asks"]["why"], json!("switched_off"));
    assert_eq!(trust(&s, &revoked)["asks"]["why"], json!("not_chosen"));
    // Unticking at a start forgets the folder, and that pane waits.
    assert!(listed(&s).contains(&(dir.display().to_string(), "antigravity".to_string())));
    let unticked = spawn(
        "antigravity",
        &dir,
        json!({"trust_screen": {"as": "antigravity"}, "steps": []}),
        json!({"trust_folder": false}),
    );
    assert_eq!(
        wait_asks(&s, &unticked)["trust"]["asks"]["why"],
        json!("not_chosen")
    );
    assert!(!listed(&s).contains(&(dir.display().to_string(), "antigravity".to_string())));
    wait_for("the late screen", 40, || {
        s.text(&late)
            .contains("Do you trust the contents of this project?")
            .then_some(())
    });
    a_key_would_have_come();
    assert!(!s.text(&late).contains(TRUSTED), "{}", s.text(&late));
    assert!(trust(&s, &late)["asks"].is_null(), "{}", s.pane(&late));
    assert_eq!(
        s.log("pane.trust_answered").len(),
        1,
        "one answer on the whole station"
    );
}

/// A helper an agent asks for is answered only in a project the owner
/// switched on, whatever the agent sends; and the switch is the owner's,
/// kept by the station and not by the project's file.
#[test]
#[ignore = "merge tier: waits for a key that must not be pressed, and starts helpers"]
fn a_helper_is_answered_only_in_a_project_the_owner_switched_on() {
    let s = Station::start("alpha");
    let repo = s.git_repo("demo");
    let helper =
        s.script_file(&json!({"trust_screen": {"as": "antigravity"}, "steps": [{"sleep": 60000}]}));
    // A project file that claims the switch for itself changes nothing.
    std::fs::create_dir_all(repo.join(".spandeck")).unwrap();
    std::fs::write(
        repo.join(".spandeck/project.toml"),
        format!("name = \"demo\"\ntrust_folder = true\n[folder_trust]\non = true\n[roles.tester]\nagent = \"antigravity\"\nprogram = '{FAKE}'\nplacement = \"worktree\"\nargs = [\"--script\", '{}']\n", helper.display()),
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "project"]);
    s.call("project.new", json!({"path": repo}));
    let lead = s.fake(
        json!({"steps": []}),
        json!({"name": "lead", "project": "demo", "cwd": repo}),
    );
    s.wait_text(&lead, "[fake] session");
    let delegate = || {
        let out = s
            .call_as_pane(&lead, "delegate", json!({"role": "tester", "task": "look", "trust_folder": true, "trust_choice": {"by": "you", "why": "folder"}}))
            .unwrap();
        out["pane"].as_str().unwrap().to_string()
    };

    // Off (as it is until the owner says otherwise): the helper waits.
    let off = delegate();
    let p = wait_asks(&s, &off);
    assert_eq!(p["trust"]["asks"]["why"], json!("not_chosen"));
    assert!(p["trust"]["chosen"].is_null());
    std::thread::sleep(Duration::from_millis(2500));
    assert!(!s.text(&off).contains(TRUSTED));

    // On: the next helper, in a worktree spandeck made for the project,
    // is answered, and the log says for whom and why.
    assert!(s.call("foldertrust.list", json!({}))["projects"]
        .as_array()
        .unwrap()
        .is_empty());
    s.call(
        "foldertrust.project",
        json!({"project": "demo", "on": true}),
    );
    let on = delegate();
    s.wait_text(&on, TRUSTED);
    let log = answers(&s, &on);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["data"]["why"], json!("project"));
    assert_eq!(log[0]["data"]["how"], json!("standing"));
    // The owner's own pane in the project's folder, with no tick sent.
    let c = s.call(
        "foldertrust.choice",
        json!({"agent": "antigravity", "project": "demo"}),
    );
    assert_eq!(
        (c["on"].clone(), c["why"].clone()),
        (json!(true), json!("project"))
    );
    // Outside the project's folders the switch says nothing.
    let outside = folder(&s, "outside");
    let c = s.call(
        "foldertrust.choice",
        json!({"agent": "antigravity", "project": "demo", "cwd": outside}),
    );
    assert_eq!(c["on"], json!(false));

    // Off again: a helper started while it was on and not yet asked is
    // not answered either (the choice is looked at when the key would be
    // pressed), and the first helper was never answered.
    s.call(
        "foldertrust.project",
        json!({"project": "demo", "on": false}),
    );
    let after = delegate();
    wait_asks(&s, &after);
    std::thread::sleep(Duration::from_millis(2000));
    assert!(!s.text(&after).contains(TRUSTED));
    assert!(!s.text(&off).contains(TRUSTED));
    assert_eq!(s.log("pane.trust_answered").len(), 1);
    assert_eq!(s.log("foldertrust.project").len(), 2);
}

/// The program started again in the same pane (the owner's Restart, or the
/// station after a crash) is answered again only because the pane was
/// started with the choice; and Claude Code's marker is moved for a
/// chosen folder and for nothing more.
#[test]
#[ignore = "merge tier: restarts a pane and a station"]
fn a_pane_started_with_the_choice_keeps_it_through_a_restart() {
    // A restart starts the station's own program for the agent, with the
    // station's own arguments: here, the stand-in asking about the folder.
    let scripts = tempfile::tempdir().unwrap();
    let script = scripts.path().join("asks.json");
    std::fs::write(
        &script,
        json!({"trust_screen": {"as": "antigravity"}, "steps": []}).to_string(),
    )
    .unwrap();
    let mut s = Station::start_with(
        "alpha",
        &format!(
            "[agents.antigravity]\nprogram = '{FAKE}'\nargs = [\"--script\", '{}']\n",
            script.display()
        ),
        &[],
    );
    let begin = |dir: &Path, extra: Value| {
        let mut params = json!({"agent": "antigravity", "cwd": dir});
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        s.call("pane.spawn", params)["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let dir = folder(&s, "kept");
    let chosen = begin(&dir, json!({"trust_folder": true}));
    s.wait_text(&chosen, TRUSTED);
    // The same folder, started without the choice by an unticked start in
    // another folder's pane: it must not gain one from a restart.
    let other = folder(&s, "other");
    let plain = begin(&other, json!({}));
    wait_asks(&s, &plain);

    s.call("pane.restart", json!({"pane": chosen, "fresh": true}));
    s.call("pane.restart", json!({"pane": plain, "fresh": true}));
    wait_for("the restarted program to be answered", 20, || {
        (answers(&s, &chosen).len() == 2).then_some(())
    });
    wait_asks(&s, &plain);
    assert!(answers(&s, &plain).is_empty());
    // A mark on the pane is not lifted by any of this.
    s.call("pane.pause", json!({"pane": chosen, "paused": true}));

    s.kill9();
    s.launch();
    s.wait_ready();
    wait_for("the recovered program to be answered", 30, || {
        (answers(&s, &chosen).len() == 3).then_some(())
    });
    let p = s.pane(&chosen);
    assert_eq!(p["paused"], json!(true));
    assert!(
        !p["held"]["restored"].is_null(),
        "still waits for the owner: {p}"
    );
    assert!(answers(&s, &plain).is_empty());

    // Claude Code, whose marker spandeck moves for a chosen folder.
    let cdir = folder(&s, "claude-moved");
    let claude = start(&s, "claude", &cdir, json!({"trust_folder": true}));
    s.wait_text(&claude, TRUSTED);
    assert_eq!(answers(&s, &claude)[0]["data"]["how"], json!("standing"));
    assert!(s.pane(&claude)["exit"].is_null());
    // The whole thing switched off: nothing is pressed for anyone.
    s.call("station.flags", json!({"set": {"folder_trust": false}}));
    let off = s.call("pane.spawn", json!({"agent": "antigravity", "cwd": dir}))["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        wait_asks(&s, &off)["trust"]["asks"]["why"],
        json!("switched_off")
    );
}

/// The owner's request passed on by another station is the owner's, and
/// what it remembers is the far station's own. (That a station calling for
/// itself has none of these methods is `rpc.rs`'s own test: rule 7.)
#[test]
#[ignore = "merge tier: two stations"]
fn a_start_the_owner_makes_through_another_station_is_the_owners() {
    let (a, b) = Station::pair();
    let dir = folder(&b, "far");
    let script = b.script_file(&json!({"trust_screen": {"as": "antigravity"}, "steps": []}));
    let params = json!({"agent": "antigravity", "program": FAKE, "args": ["--script", script], "cwd": dir, "trust_folder": true});
    assert!(listed(&b).is_empty());
    // The owner, at a deck on alpha, starting a pane on bravo.
    let pane = a.call(
        "peer.call",
        json!({"station": "bravo", "method": "pane.spawn", "params": params}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    b.wait_text(&pane, TRUSTED);
    let log = answers(&b, &pane);
    assert_eq!(log[0]["data"]["started_by"], json!("you (via alpha)"));
    assert_eq!(listed(&b).len(), 1);
    assert!(listed(&a).is_empty(), "folders are a machine's own");
}

/// The owner's side of it in the Control Deck, on a desktop and on a
/// phone: the tick box says what it allows and for which folder, and is
/// off for a new folder; a pane says that its question was answered, or
/// offers the one tap; the folders are listed in Agent setup with Remove;
/// a project has its switch. With `SPANDECK_SHOTS=/some/dir` it also
/// writes pictures of each for a human to look at.
#[test]
#[ignore = "merge tier: a browser"]
fn the_deck_has_the_tick_the_note_the_one_tap_and_the_list() {
    use common::browser::Page;
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let shots = std::env::var("SPANDECK_SHOTS").ok().map(PathBuf::from);
    if let Some(dir) = &shots {
        std::fs::create_dir_all(dir).unwrap();
    }
    let shoot = |page: &Page, name: &str| {
        let Some(dir) = &shots else { return };
        std::thread::sleep(Duration::from_millis(700));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
    };
    let scripts = tempfile::tempdir().unwrap();
    let script = |name: &str| {
        let path = scripts.path().join(format!("{name}.json"));
        std::fs::write(
            &path,
            json!({"trust_screen": {"as": name}, "steps": []}).to_string(),
        )
        .unwrap();
        path
    };
    let s = Station::start_with(
        "spark2",
        // Moving Claude Code's marker is switched off here, so that the
        // deck's one tap and what it says of it are seen.
        &format!(
            "[flags]\nfolder_trust.move = false\n[agents.antigravity]\nprogram = '{FAKE}'\nargs = [\"--script\", '{}']\n[agents.claude]\nprogram = '{FAKE}'\nargs = [\"--script\", '{}']\n",
            script("antigravity").display(),
            script("claude").display()
        ),
        &[],
    );
    let repo = s.git_repo("webshop");
    s.call("project.new", json!({"path": repo}));
    let dir = folder(&s, "new-idea");
    let set = |page: &Page, sel: &str, value: &str| {
        page.js(&format!(
            "(el => {{ el.value = {}; el.dispatchEvent(new Event('input', {{bubbles: true}})); el.dispatchEvent(new Event('change', {{bubbles: true}})); return true; }})(document.querySelector({}))",
            json!(value),
            json!(sel)
        ));
    };
    let tick_shown = "(r => !!r && !r.hidden && r.innerText.includes('Trust this folder for'))(document.querySelector('.dialog .trust-row'))";
    let pane_named = |name: &str| {
        wait_for("the new pane", 20, || {
            s.call("pane.list", json!({}))
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == json!(name))
                .map(|p| p["id"].as_str().unwrap().to_string())
        })
    };

    // ---- desktop ----
    let page = Page::open(&s, 1280, 860);
    page.wait(
        "the sidebar",
        "!!document.querySelector('.side-actions button')",
    );
    page.js("document.querySelector('.side-actions button').click()");
    page.wait("the dialog", "!!document.querySelector('.dialog select')");
    // A shell has no such question: no tick box.
    set(&page, ".dialog select", "shell");
    assert_eq!(
        page.js("document.querySelector('.dialog .trust-row').hidden"),
        json!(true)
    );
    set(&page, ".dialog select", "antigravity");
    set(
        &page,
        ".dialog input[placeholder^='default']",
        &dir.display().to_string(),
    );
    page.wait("the tick box", tick_shown);
    assert_eq!(
        page.js("document.querySelector('.dialog .trust-row').innerText.includes('null')"),
        json!(false)
    );
    page.wait(
        "the folder, what it allows and the warning",
        &format!(
            "(t => t.includes({}) && t.includes('read, edit and run files') && t.includes('copied from somewhere you do not trust'))(document.querySelector('.dialog .trust-row').innerText)",
            json!(dir.display().to_string())
        ),
    );
    assert_eq!(
        page.js("document.querySelector('.dialog .trust-row input').checked"),
        json!(false),
        "off for a folder never used"
    );
    page.js("document.querySelector('.dialog .trust-row input').click()");
    set(&page, ".dialog input[aria-label=Name]", "idea");
    shoot(&page, "desktop-1-tick-box");
    page.js("document.querySelector('.dialog button[type=submit]').click()");
    let idea = pane_named("idea");
    s.wait_text(&idea, TRUSTED);
    page.terminal_face();
    page.wait(
        "the pane to say its question was answered",
        "(n => !!n && n.innerText.includes(\"spandeck answered Antigravity's folder-trust question, as you chose for this folder\"))(document.querySelector('.strip-note'))",
    );
    shoot(&page, "desktop-2-pane-note");

    // The same folder for Claude Code: its own tick, off; ticked, the
    // dialog says a tap will be needed, and the pane offers it.
    page.js("document.querySelector('.side-actions button').click()");
    page.wait("the dialog", "!!document.querySelector('.dialog select')");
    set(&page, ".dialog select", "claude");
    set(
        &page,
        ".dialog input[placeholder^='default']",
        &dir.display().to_string(),
    );
    page.wait("the tick box", tick_shown);
    // For this folder: a tick clicked before the dialog has asked about
    // the folder just typed is put back to what the station answers.
    page.wait(
        "the tick for Claude Code in this folder",
        &format!(
            "(t => t.includes('for Claude Code') && t.includes({}))(document.querySelector('.dialog .trust-row').innerText)",
            json!(dir.display().to_string())
        ),
    );
    assert_eq!(
        page.js("document.querySelector('.dialog .trust-row input').checked"),
        json!(false)
    );
    page.js("document.querySelector('.dialog .trust-row input').click()");
    assert_eq!(
        page.js("document.querySelector('.dialog .trust-row').innerText.includes('one tap')"),
        json!(true)
    );
    set(&page, ".dialog input[aria-label=Name]", "idea-claude");
    shoot(&page, "desktop-3-tick-box-claude");
    page.js("document.querySelector('.dialog button[type=submit]').click()");
    let claude = pane_named("idea-claude");
    wait_asks(&s, &claude);
    page.terminal_face();
    page.wait(
        "the one tap on the pane",
        "!!document.querySelector('.strip .trust-tap')",
    );
    assert_eq!(
        page.js("document.querySelector('.strip .strip-reason').innerText"),
        json!(ASKS)
    );
    shoot(&page, "desktop-4-one-tap");
    page.js("document.querySelector('.strip .trust-tap').click()");
    s.wait_text(&claude, TRUSTED);
    page.wait(
        "the button to go",
        "!document.querySelector('.strip .trust-tap')",
    );

    // The list, in Agent setup, and Remove.
    page.js(
        "[...document.querySelectorAll('button')].find(b => b.innerText === 'Agent setup').click()",
    );
    page.wait(
        "the list",
        "document.querySelectorAll('.trusted-folders tr').length === 2",
    );
    assert_eq!(
        page.js("document.querySelector('.trusted-all').innerText.includes('Folders trusted for agents on spark2')"),
        json!(true)
    );
    page.js("document.querySelector('.trusted-all').scrollIntoView()");
    shoot(&page, "desktop-5-trusted-folders");
    page.js("[...document.querySelectorAll('.trusted-folders tr')].find(r => r.innerText.includes('Claude Code')).querySelector('button').click()");
    page.wait(
        "one folder left",
        "document.querySelectorAll('.trusted-folders tr').length === 1",
    );
    assert_eq!(
        listed(&s),
        vec![(dir.display().to_string(), "antigravity".to_string())]
    );
    page.js("document.querySelector('.dialog .icon-btn').click()");

    // A project's switch.
    page.wait("the project", "!!document.querySelector('.side-project')");
    page.right_click("document.querySelector('.side-project')");
    page.choose("Folder trust for this project");
    page.wait(
        "the switch, off",
        "(b => !!b && !b.checked)([...document.querySelectorAll('.dialog')].pop().querySelector('input[type=checkbox]'))",
    );
    page.js("[...document.querySelectorAll('.dialog')].pop().querySelector('input[type=checkbox]').click()");
    wait_for("the project's switch on the station", 10, || {
        (s.call("foldertrust.list", json!({}))["projects"][0]["project"] == json!("webshop"))
            .then_some(())
    });
    page.wait(
        "the switch, on",
        "(b => !!b && b.checked)([...document.querySelectorAll('.dialog')].pop().querySelector('input[type=checkbox]'))",
    );
    shoot(&page, "desktop-6-project-switch");
    drop(page);

    // ---- phone ----
    let other = folder(&s, "phone-idea");
    let phone = Page::open(&s, 390, 844);
    // Tapped again until the sheet is there: a tap before the page has
    // heard from its station is answered "No station is answering".
    phone.wait(
        "New session, and its sheet",
        "!!document.querySelector('.sheet .tile-pick') || (document.querySelector('.new-session')?.click(), false)",
    );
    phone.js("[...document.querySelectorAll('.sheet .tile-pick')].find(b => b.innerText.includes('Antigravity')).click()");
    set(
        &phone,
        ".sheet input[aria-label=Folder]",
        &other.display().to_string(),
    );
    phone.wait(
        "the tick box",
        &tick_shown.replace(".dialog .trust-row", ".sheet .trust-row"),
    );
    phone.wait(
        "the folder and the warning",
        &format!(
            "(t => t.includes({}) && t.includes('copied from somewhere you do not trust'))(document.querySelector('.sheet .trust-row').innerText)",
            json!(other.display().to_string())
        ),
    );
    assert_eq!(
        phone.js("document.querySelector('.sheet .trust-row input').checked"),
        json!(false)
    );
    phone.js("document.querySelector('.sheet .trust-row').scrollIntoView({block: 'center'})");
    shoot(&phone, "phone-1-tick-box");
    // Left unticked: the pane waits, and offers the tap.
    phone.js("document.querySelector('.sheet button[type=submit]').click()");
    let asked = wait_for("the phone's pane", 20, || {
        s.call("pane.list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["cwd"] == json!(other))
            .map(|p| p["id"].as_str().unwrap().to_string())
    });
    wait_asks(&s, &asked);
    phone.wait("the one tap", "!!document.querySelector('.trust-tap')");
    shoot(&phone, "phone-2-one-tap");
    assert!(!s.text(&asked).contains(TRUSTED));
    phone.js("(c => c && c.click())(document.querySelector('.trust-card input[type=checkbox]'))");
    phone.js("document.querySelector('.trust-tap').click()");
    s.wait_text(&asked, TRUSTED);
    phone.wait("the tap to go", "!document.querySelector('.trust-tap')");
    // A pane that was answered on the standing choice says so on a phone.
    // (Since 16 Nov 2026 an Antigravity pane opens as Chat there, made
    // from its screen, and the note is under its conversation; before, it
    // was its terminal only, and the note was above that.)
    phone.goto(&format!("#/s/{}/p/{}", s.name, idea));
    phone.wait(
        "the note on the phone",
        "[...document.querySelectorAll('.trust-said, .chat-cards .chat-quiet')].some(n => n.innerText.includes(\"spandeck answered Antigravity's folder-trust question\"))",
    );
    shoot(&phone, "phone-3-pane-note");
    // Claude Code, whose pane a phone shows as a conversation: the card
    // is there too, and says why nothing was pressed.
    let third = folder(&s, "phone-claude");
    let claude = s.call(
        "pane.spawn",
        json!({"agent": "claude", "cwd": third, "trust_folder": true}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_asks(&s, &claude);
    phone.goto(&format!("#/s/{}/p/{}", s.name, claude));
    phone.wait(
        "the card for Claude Code",
        "(c => !!c && c.innerText.includes('Its own choice on that screen is \"No, exit\"') && !!c.querySelector('.trust-tap'))(document.querySelector('.trust-card'))",
    );
    shoot(&phone, "phone-4-claude-one-tap");
    phone.js("document.querySelector('.trust-tap').click()");
    s.wait_text(&claude, TRUSTED);
}

/// A bug that happened (the real Claude Code, 18 Nov 2026): a pane's first
/// message was sent as the pane was started; the nudge was typed one
/// second on, into "Is this a project you trust?", before that screen had
/// been read for what it is; its Enter chose "No, exit" and the program
/// ended. A program that has only just started, and has not said its
/// session began, is typed nothing.
#[test]
#[ignore = "merge tier: waits five seconds for a nudge that must not come"]
fn a_message_sent_as_a_pane_starts_is_not_typed_into_its_first_question() {
    let s = Station::start("alpha");
    let dir = folder(&s, "fresh");
    let pane = start(&s, "claude", &dir, json!({"name": "early"}));
    // At once: before the station can have read the screen.
    s.call("msg.send", json!({"to": "early", "body": "your role"}));
    std::thread::sleep(std::time::Duration::from_secs(5));
    assert!(s.log("msg.nudged").is_empty(), "{:?}", s.log("msg.nudged"));
    let p = s.pane(&pane);
    assert_eq!(p["exited"], json!(false), "{p}");
    assert_eq!(p["state"], json!("needs_you"), "{p}");
    assert!(trust(&s, &pane)["asks"].is_object(), "{p}");
}
