//! Trusting spandeck's own hooks (DECISIONS.md, "Trusting spandeck's own
//! hooks"): the owner's decision of 16 Nov 2026 and its safety condition.
//! The stand-in agent draws Codex's "Hooks need review" as the real 0.169.0
//! drew it (agents/hookstrust.rs), in a pane of Codex's kind.
//!
//! Every station here has a throwaway home folder (`Station::command`), so
//! the Codex settings that are read and written are under the test's own
//! folder, never the machine's.
//!
//! One scenario runs with every change. The ones that wait to see that
//! nothing is pressed, restart a station or drive a browser are in the
//! merge tier (`--ignored`).

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

const TRUSTED: &str = "[fake] hooks: trusted all";
const ANY_ANSWER: &str = "[fake] hooks:";
const ASKS: &str = "asks you to trust spandeck's hooks: answer in the terminal";

/// A station that keeps spandeck's hooks in the agents' settings of its
/// (throwaway) home, with Codex's hooks installed there.
fn station(name: &str, extra: &str) -> Station {
    let s = Station::start_with(name, &format!("{AGENT_HOME}{extra}"), &[]);
    let st = s.call("integration.install", json!({"agent": "codex"}));
    assert_eq!(st["installed"], json!(true), "{st}");
    assert!(codex_home(&s).join("hooks.json").starts_with(&s.root));
    s
}

fn codex_home(s: &Station) -> PathBuf {
    s.root.join("home").join(".codex")
}

fn folder(s: &Station, name: &str) -> PathBuf {
    let dir = s.root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// Start a Codex pane in `dir` whose program shows the review of hooks
/// (`screen`: what the stand-in is told about it) where the real one does.
fn start(s: &Station, dir: &Path, screen: Value, extra: Value) -> String {
    let mut screen = screen;
    screen["as"] = json!("codex-hooks");
    let script = s.script_file(
        &json!({"trust_screen": screen, "steps": [], "on_input": [{"turn_end": "ok"}]}),
    );
    let mut params =
        json!({"agent": "codex", "program": FAKE, "args": ["--script", script], "cwd": dir});
    for (k, v) in extra.as_object().unwrap() {
        params[k] = v.clone();
    }
    s.call("pane.spawn", params)["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The pane says the review is on its screen and nothing was pressed.
fn wait_asks(s: &Station, pane: &str) -> Value {
    wait_for_or(
        "the pane to say it asks about hooks",
        20,
        || format!("{}\n{}", s.pane(pane), s.text(pane)),
        || {
            let p = s.pane(pane);
            (!p["hooks_trust"]["asks"].is_null()).then_some(p)
        },
    )
}

fn answers(s: &Station, pane: &str) -> Vec<Value> {
    s.log("pane.hooks_trusted")
        .into_iter()
        .filter(|e| e["pane"] == json!(pane))
        .collect()
}

fn standing(s: &Station) -> Value {
    s.call("hookstrust.choice", json!({"agent": "codex"}))
}

/// The owner's decision (16 Nov 2026): "When starting a new pane, would it
/// be a checkmark for any agent to trust spandeck hooks? Should be checked
/// on by default". On, and every hook spandeck's own: the review is
/// answered once with "Trust all and continue" (the marker is moved from
/// "1. Review hooks", seen on the second choice, and only then confirmed),
/// it is in the ship's log, and the agent goes on. Unticked: nothing is
/// pressed, that is remembered for the station, the pane says so and the
/// owner's one tap answers it. None of it is an agent's to use.
#[test]
fn spandecks_own_hooks_are_trusted_once_when_ticked_and_wait_for_the_owner_when_not() {
    let s = station("alpha", "");
    let before = standing(&s);
    assert_eq!(before["asks"], json!(true));
    assert_eq!(before["on"], json!(true), "on until the owner says no");
    assert_eq!(before["answers"], json!(true));
    assert_eq!(before["answer"], json!("Trust all and continue"));
    for agent in ["shell", "claude", "antigravity", "opencode"] {
        let c = s.call("hookstrust.choice", json!({"agent": agent}));
        assert_eq!(c["asks"], json!(false), "{agent}: no tick box");
    }

    // Nothing sent, as from a deck from before this: on.
    let ticked = start(&s, &folder(&s, "one"), json!({}), json!({}));
    s.wait_text(&ticked, TRUSTED);
    s.wait_text(&ticked, "[fake] session");
    let log = wait_for("the answer in the ship's log", 10, || {
        Some(answers(&s, &ticked)).filter(|a| !a.is_empty())
    });
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["data"]["how"], json!("standing"));
    assert_eq!(log[0]["data"]["started_by"], json!("you"));
    assert_eq!(log[0]["actor"], json!("spandeck"));
    let screen = s.text(&ticked);
    assert_eq!(screen.matches(ANY_ANSWER).count(), 1, "{screen}");
    let p = s.pane(&ticked);
    assert_eq!(
        p["hooks_trust"]["answered"]["how"],
        json!("standing"),
        "{p}"
    );
    assert!(p["hooks_trust"]["asks"].is_null());
    assert_ne!(p["needs_reason"], json!(ASKS));
    // Codex's own records are the program's to write: spandeck wrote none.
    assert!(!codex_home(&s).join("config.toml").exists());

    // Unticked: nothing is pressed, and the station remembers.
    let plain = start(
        &s,
        &folder(&s, "two"),
        json!({}),
        json!({"trust_hooks": false}),
    );
    let p = wait_asks(&s, &plain);
    assert_eq!(p["state"], json!("needs_you"));
    assert_eq!(p["needs_reason"], json!(ASKS));
    assert_eq!(p["hooks_trust"]["asks"]["why"], json!("not_chosen"));
    assert_eq!(p["hooks_trust"]["asks"]["button"], json!(true));
    assert!(p["hooks_trust"]["chosen"].is_null());
    assert_eq!(standing(&s)["on"], json!(false));
    assert_eq!(s.log("hookstrust.set").len(), 1);
    assert!(!s.text(&plain).contains(ANY_ANSWER));

    // An agent cannot tap, set the choice, read it, or start a pane with
    // it: none of it is on an agent's list (rule 7).
    for (method, params) in [
        ("pane.trust_hooks", json!({"pane": plain})),
        ("hookstrust.set", json!({"on": true})),
        ("hookstrust.choice", json!({"agent": "codex"})),
        ("pane.spawn", json!({"agent": "codex", "trust_hooks": true})),
    ] {
        let err = s.call_as_pane(&ticked, method, params).unwrap_err();
        assert!(
            err.to_string().contains("denied") || err.to_string().contains("not allowed"),
            "{method}: {err}"
        );
    }
    assert_eq!(standing(&s)["on"], json!(false));
    assert!(!s.text(&plain).contains(ANY_ANSWER));

    // The owner's one tap.
    s.call("pane.trust_hooks", json!({"pane": plain}));
    s.wait_text(&plain, TRUSTED);
    let log = answers(&s, &plain);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["data"]["how"], json!("tap"));
    assert!(s.pane(&plain)["hooks_trust"]["asks"].is_null());
    assert_eq!(standing(&s)["on"], json!(false), "a tap is for this once");
    let again = s
        .try_call("pane.trust_hooks", json!({"pane": plain}))
        .unwrap_err();
    assert!(again.to_string().contains("not showing"), "{again}");
}

/// The safety condition: "Trust all" trusts every hook Codex found, so
/// nothing is pressed, and no one-tap answer is offered, unless the hooks
/// under review can only be spandeck's. Each case is watched for long
/// enough that a key would have been pressed, with the choice on.
#[test]
#[ignore = "merge tier: waits several times for a key that must not be pressed"]
fn nothing_is_pressed_unless_the_hooks_can_only_be_spandecks_and_the_screen_is_the_known_one() {
    let s = station("alpha", "");
    let a_key_would_have_come = || std::thread::sleep(Duration::from_millis(3500));
    let not_ours = |pane: &str, words: &str| {
        let p = wait_asks(&s, pane);
        let asks = &p["hooks_trust"]["asks"];
        assert_eq!(asks["why"], json!("not_only_spandecks"), "{p}");
        assert_eq!(asks["button"], json!(false), "no one-tap trust all: {p}");
        let reason = p["needs_reason"].as_str().unwrap();
        assert!(
            reason.starts_with("asks you to trust hooks: ") && reason.contains(words),
            "{reason}"
        );
        assert!(reason.ends_with("Review them in the terminal"), "{reason}");
        // The owner's tap is refused too, with the reason.
        let err = s
            .try_call("pane.trust_hooks", json!({"pane": pane}))
            .unwrap_err();
        assert!(err.to_string().contains("nothing was pressed"), "{err}");
    };
    let hooks_file = codex_home(&s).join("hooks.json");
    let ours = std::fs::read_to_string(&hooks_file).unwrap();
    let mut never: Vec<String> = vec![];

    // Someone else's hook in the hooks file, beside spandeck's.
    let mut file: Value = serde_json::from_str(&ours).unwrap();
    file["hooks"]["Stop"]
        .as_array_mut()
        .unwrap()
        .push(json!({"hooks": [{"type": "command", "command": "say done"}]}));
    std::fs::write(&hooks_file, file.to_string()).unwrap();
    let foreign = start(&s, &folder(&s, "foreign"), json!({"count": 13}), json!({}));
    not_ours(&foreign, "1 hook in its hooks file is not spandeck's");
    a_key_would_have_come();
    assert!(!s.text(&foreign).contains(ANY_ANSWER));
    // Put right while the pane waits: it is the same first screen, the
    // files are read anew, and the count on it is still one too many.
    std::fs::write(&hooks_file, &ours).unwrap();
    wait_for("the reason to follow the files", 10, || {
        s.pane(&foreign)["needs_reason"]
            .as_str()
            .unwrap()
            .contains("it asks about 13, and spandeck has 12")
            .then_some(())
    });
    never.push(foreign);

    // A table of hooks in Codex's own config.
    let config = codex_home(&s).join("config.toml");
    std::fs::write(
        &config,
        "[[hooks.Stop]]\nhooks = [{type = \"command\", command = \"say done\"}]\n",
    )
    .unwrap();
    let table = start(&s, &folder(&s, "table"), json!({}), json!({}));
    not_ours(&table, "has hooks of its own");
    // Only Codex's own trust records there: not a source. But the program
    // in this pane read its hooks when it started, so what it asks about
    // is still what it found then: this pane stays refused, tap and all.
    std::fs::write(
        &config,
        "[hooks.state.\"x:stop:0:0\"]\ntrusted_hash = \"sha256:00\"\n",
    )
    .unwrap();
    wait_for("the reason to say the files changed", 10, || {
        s.pane(&table)["needs_reason"]
            .as_str()
            .unwrap()
            .contains("were not only spandeck's when it started")
            .then_some(())
    });
    not_ours(&table, "were not only spandeck's when it started");
    never.push(table);

    // A project's own hooks file, in the pane's folder and in one above.
    let project = folder(&s, "project/deep");
    std::fs::create_dir_all(project.join(".codex")).unwrap();
    std::fs::write(project.join(".codex/hooks.json"), "{}").unwrap();
    let in_project = start(&s, &project, json!({}), json!({}));
    let above = folder(&s, "repo/src/deep");
    let repo = above.parent().unwrap().parent().unwrap();
    std::fs::create_dir_all(repo.join(".codex")).unwrap();
    std::fs::write(repo.join(".codex/hooks.json"), "{}").unwrap();
    let in_repo = start(&s, &above, json!({}), json!({}));
    // More hooks on the screen than spandeck has.
    let many = start(&s, &folder(&s, "many"), json!({"count": 13}), json!({}));
    // Hook settings on the program's own command line.
    let script = s.script_file(&json!({"trust_screen": {"as": "codex-hooks"}, "steps": []}));
    let by_args = s.call(
        "pane.spawn",
        json!({"agent": "codex", "program": FAKE, "cwd": folder(&s, "args"),
               "args": ["--script", script, "-c", "hooks.Stop=[]"]}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    for (pane, words) in [
        (&in_project, "another hooks file is in this project"),
        (&in_repo, "another hooks file is in this project"),
        (&many, "it asks about 13, and spandeck has 12"),
        (&by_args, "hook settings on its command line"),
    ] {
        not_ours(pane, words);
        never.push(pane.clone());
    }

    // spandeck's own hooks, and a screen that is not the known one: the
    // owner can tap, and nothing is pressed for them.
    let reworded = start(
        &s,
        &folder(&s, "reworded"),
        json!({"reword": "Hooks need your review"}),
        json!({}),
    );
    let extra = start(
        &s,
        &folder(&s, "extra"),
        json!({"above": ["  OpenAI Codex (v0.170.0)"]}),
        json!({}),
    );
    for pane in [&reworded, &extra] {
        let p = wait_asks(&s, pane);
        let asks = &p["hooks_trust"]["asks"];
        assert_eq!(asks["why"], json!("not_the_known_screen"), "{p}");
        assert_eq!(asks["button"], json!(true));
        assert_eq!(p["needs_reason"], json!(ASKS));
        never.push(pane.clone());
    }
    // The owner at the keyboard of this one before its screen came.
    let typed = start(
        &s,
        &folder(&s, "typed"),
        json!({"after_ms": 3000}),
        json!({}),
    );
    std::thread::sleep(Duration::from_millis(1500));
    s.call("pane.input", json!({"pane": typed, "data": "x"}));
    assert_eq!(
        wait_asks(&s, &typed)["hooks_trust"]["asks"]["why"],
        json!("you_typed")
    );
    never.push(typed);
    // First shown 21 seconds after the program started: not its first
    // screen, so it is not read at all.
    let late = start(
        &s,
        &folder(&s, "late"),
        json!({"after_ms": 21000}),
        json!({}),
    );
    // The same words later in a session, quoted and then asked again.
    let script = s.script_file(&json!({"trust_screen": {"as": "codex-hooks"}, "steps": [
        {"trust_screen": {"as": "codex-hooks", "wait": false}},
        {"sleep": 2500},
        {"trust_screen": {"as": "codex-hooks"}}]}));
    let later = s.call(
        "pane.spawn",
        json!({"agent": "codex", "program": FAKE, "cwd": folder(&s, "later"),
               "args": ["--script", script]}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.wait_text(&later, TRUSTED);

    // A station that does not keep the agents' settings answers nothing,
    // whatever is in the hooks file of its home.
    let other = Station::start("bravo");
    assert_eq!(standing(&other)["answers"], json!(false));
    std::fs::create_dir_all(codex_home(&other)).unwrap();
    std::fs::write(codex_home(&other).join("hooks.json"), &ours).unwrap();
    let elsewhere = start(&other, &folder(&other, "work"), json!({}), json!({}));
    let p = wait_asks(&other, &elsewhere);
    assert_eq!(p["hooks_trust"]["asks"]["button"], json!(false), "{p}");
    assert!(
        p["needs_reason"]
            .as_str()
            .unwrap()
            .contains("this station does not keep spandeck's hooks"),
        "{p}"
    );

    wait_for("the late screen", 40, || {
        s.text(&late).contains("Hooks need review").then_some(())
    });
    a_key_would_have_come();
    for pane in &never {
        let screen = s.text(pane);
        assert!(screen.contains("need"), "{screen}");
        assert!(!screen.contains(ANY_ANSWER), "{pane}: {screen}");
        assert!(answers(&s, pane).is_empty());
    }
    assert!(!s.text(&late).contains(ANY_ANSWER));
    assert!(s.pane(&late)["hooks_trust"]["asks"].is_null());
    assert!(!other.text(&elsewhere).contains(ANY_ANSWER));
    let screen = s.text(&later);
    assert_eq!(screen.matches(ANY_ANSWER).count(), 1, "{screen}");
    assert!(
        screen.contains("Hooks need review"),
        "asked again: {screen}"
    );
    assert_eq!(
        s.log("pane.hooks_trusted").len(),
        1,
        "one answer on the whole station"
    );
    // With everything as spandeck wrote it, the next pane is answered: the
    // cases above were not left alone for some other reason.
    let good = start(&s, &folder(&s, "good"), json!({}), json!({}));
    s.wait_text(&good, TRUSTED);
    // And the tap, where the screen said one row more than the known one.
    s.call("pane.trust_hooks", json!({"pane": extra}));
    s.wait_text(&extra, TRUSTED);
}

/// A pane an agent asks for, and a program started again in the same pane
/// (the owner's Restart, the station after a crash), use the owner's
/// standing choice and nothing the agent sends; a pane started while the
/// choice was off gains none from a restart; and nothing here lifts a
/// Pause or a hold.
#[test]
#[ignore = "merge tier: restarts a pane and a station, and starts helpers"]
fn a_helper_and_a_restarted_pane_use_the_owners_standing_choice() {
    let scripts = tempfile::tempdir().unwrap();
    let script = scripts.path().join("asks.json");
    std::fs::write(
        &script,
        json!({"trust_screen": {"as": "codex-hooks"}, "steps": []}).to_string(),
    )
    .unwrap();
    let mut s = station(
        "alpha",
        &format!(
            "[agents.codex]\nprogram = '{FAKE}'\nargs = [\"--script\", '{}']\n",
            script.display()
        ),
    );
    let repo = s.git_repo("demo");
    std::fs::create_dir_all(repo.join(".spandeck")).unwrap();
    // A project file that claims the choice for itself changes nothing.
    std::fs::write(
        repo.join(".spandeck/project.toml"),
        "name = \"demo\"\ntrust_hooks = true\n[hooks_trust]\non = true\n[roles.tester]\nagent = \"codex\"\nplacement = \"worktree\"\n",
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
            .call_as_pane(&lead, "delegate", json!({"role": "tester", "task": "look", "trust_hooks": true, "hooks_trust_choice": {"by": "you", "why": "station"}}))
            .unwrap();
        out["pane"].as_str().unwrap().to_string()
    };
    let begin = |dir: &Path| {
        s.call("pane.spawn", json!({"agent": "codex", "cwd": dir}))["id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // The owner turned it off: the helper waits, whatever the agent sent,
    // and so does the owner's own pane started with nothing said.
    s.call("hookstrust.set", json!({"on": false}));
    let off = delegate();
    let p = wait_asks(&s, &off);
    assert_eq!(p["hooks_trust"]["asks"]["why"], json!("not_chosen"), "{p}");
    assert!(p["hooks_trust"]["chosen"].is_null());
    let plain = begin(&folder(&s, "plain"));
    wait_asks(&s, &plain);
    std::thread::sleep(Duration::from_millis(3000));
    assert!(!s.text(&off).contains(ANY_ANSWER));

    // On again: the next helper, in a worktree spandeck made, is answered.
    s.call("hookstrust.set", json!({"on": true}));
    let on = delegate();
    s.wait_text(&on, TRUSTED);
    assert_eq!(answers(&s, &on)[0]["data"]["how"], json!("standing"));
    let chosen = begin(&folder(&s, "kept"));
    s.wait_text(&chosen, TRUSTED);
    // Switching it on does not reach back to panes started while it was
    // off.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(answers(&s, &plain).is_empty() && answers(&s, &off).is_empty());

    s.call("pane.restart", json!({"pane": chosen, "fresh": true}));
    s.call("pane.restart", json!({"pane": plain, "fresh": true}));
    wait_for("the restarted program to be answered", 20, || {
        (answers(&s, &chosen).len() == 2).then_some(())
    });
    wait_asks(&s, &plain);
    assert!(answers(&s, &plain).is_empty());
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
    // Off while a pane started with the choice is at the screen again:
    // the choice no longer stands, and it is not answered.
    s.call("hookstrust.set", json!({"on": false}));
    s.call("pane.restart", json!({"pane": chosen, "fresh": true}));
    let p = wait_asks(&s, &chosen);
    assert_eq!(p["hooks_trust"]["asks"]["why"], json!("not_chosen"), "{p}");
    assert_eq!(answers(&s, &chosen).len(), 3);
}

/// The owner's side of it in the Control Deck, on a desktop and on a
/// phone: the tick is on by default and says what it does; unticked, that
/// is remembered; a pane says that its review was answered, offers the one
/// tap, or says why the hooks may not be spandeck's and offers none; and
/// Agent setup has the station's switch. With `SPANDECK_SHOTS=/some/dir`
/// it also writes pictures of each for a human to look at.
#[test]
#[ignore = "merge tier: a browser"]
fn the_deck_has_the_tick_on_by_default_and_remembers_off() {
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
    let script = scripts.path().join("asks.json");
    std::fs::write(
        &script,
        json!({"trust_screen": {"as": "codex-hooks"}, "steps": []}).to_string(),
    )
    .unwrap();
    let s = station(
        "spark2",
        &format!(
            "[agents.codex]\nprogram = '{FAKE}'\nargs = [\"--script\", '{}']\n",
            script.display()
        ),
    );
    let set = |page: &Page, sel: &str, value: &str| {
        page.js(&format!(
            "(el => {{ el.value = {}; el.dispatchEvent(new Event('input', {{bubbles: true}})); el.dispatchEvent(new Event('change', {{bubbles: true}})); return true; }})(document.querySelector({}))",
            json!(value),
            json!(sel)
        ));
    };
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
    let tick = |root: &str, checked: bool| {
        format!(
            "(r => !!r && !r.hidden && r.innerText.includes(\"Trust spandeck's own hooks in Codex if it asks\") && r.querySelector('input').checked === {checked})(document.querySelector('{root} .hooks-row'))"
        )
    };
    let new_pane = |page: &Page, dir: &Path, name: &str| {
        page.js("document.querySelector('.side-actions button').click()");
        page.wait("the dialog", "!!document.querySelector('.dialog select')");
        set(page, ".dialog select", "codex");
        set(
            page,
            ".dialog input[placeholder^='default']",
            &dir.display().to_string(),
        );
        set(page, ".dialog input[aria-label=Name]", name);
    };

    // ---- desktop ----
    let page = Page::open(&s, 1280, 860);
    page.wait(
        "the sidebar",
        "!!document.querySelector('.side-actions button')",
    );
    page.js("document.querySelector('.side-actions button').click()");
    page.wait("the dialog", "!!document.querySelector('.dialog select')");
    // A shell and Claude Code have no such review: no tick box.
    for other in ["shell", "claude"] {
        set(&page, ".dialog select", other);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            page.js("document.querySelector('.dialog .hooks-row').hidden"),
            json!(true),
            "{other}"
        );
    }
    page.js("[...document.querySelectorAll('.dialog button')].find(b => b.innerText === 'Cancel').click()");
    new_pane(&page, &folder(&s, "first"), "hooked");
    page.wait("the tick, on by default", &tick(".dialog", true));
    assert_eq!(
        page.js("(t => t.includes('only when every hook it can be asking about is spandeck') && !t.includes('null') && !t.includes('undefined'))(document.querySelector('.dialog .hooks-row').innerText)"),
        json!(true)
    );
    shoot(&page, "desktop-1-tick-on-by-default");
    page.js("document.querySelector('.dialog button[type=submit]').click()");
    let hooked = pane_named("hooked");
    s.wait_text(&hooked, TRUSTED);
    page.wait(
        "the pane to say its review was answered",
        "(n => !!n && n.innerText.includes(\"spandeck answered Codex's review of hooks with \\\"Trust all and continue\\\"\"))(document.querySelector('.strip .hooks-said'))",
    );
    shoot(&page, "desktop-2-pane-note");

    // Unticked: the pane waits and offers the one tap, and the next
    // dialog remembers.
    new_pane(&page, &folder(&s, "second"), "waiting");
    page.wait("the tick", &tick(".dialog", true));
    page.js("document.querySelector('.dialog .hooks-row input').click()");
    shoot(&page, "desktop-3-unticked");
    page.js("document.querySelector('.dialog button[type=submit]').click()");
    let waiting = pane_named("waiting");
    wait_asks(&s, &waiting);
    assert_eq!(standing(&s)["on"], json!(false));
    page.wait(
        "the one tap on the pane",
        "!!document.querySelector('.strip .hooks-tap')",
    );
    assert_eq!(
        page.js("document.querySelector('.strip .strip-reason').innerText"),
        json!(ASKS)
    );
    shoot(&page, "desktop-4-one-tap");
    new_pane(&page, &folder(&s, "third"), "unused");
    page.wait("the tick, remembered off", &tick(".dialog", false));
    page.wait(
        "why it is off",
        "document.querySelector('.dialog .hooks-row').innerText.includes('Off because you turned it off on spark2')",
    );
    shoot(&page, "desktop-5-remembered-off");
    page.js("[...document.querySelectorAll('.dialog button')].find(b => b.innerText === 'Cancel').click()");
    page.goto(&format!("#/s/{}/p/{}", s.name, waiting));
    page.wait(
        "the one tap on the pane",
        "!!document.querySelector('.strip .hooks-tap')",
    );
    page.js("document.querySelector('.strip .hooks-tap').click()");
    s.wait_text(&waiting, TRUSTED);
    page.wait(
        "the button to go",
        "!document.querySelector('.strip .hooks-tap')",
    );

    // Hooks that may not be spandeck's: the reason, and no tap.
    let mixed = folder(&s, "mixed");
    std::fs::create_dir_all(mixed.join(".codex")).unwrap();
    std::fs::write(mixed.join(".codex/hooks.json"), "{}").unwrap();
    let unsafe_pane = s.call(
        "pane.spawn",
        json!({"agent": "codex", "cwd": mixed, "name": "mixed"}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_asks(&s, &unsafe_pane);
    page.goto(&format!("#/s/{}/p/{}", s.name, unsafe_pane));
    page.wait(
        "the reason on the pane",
        "(r => !!r && r.innerText.includes('asks you to trust hooks: another hooks file is in this project'))(document.querySelector('.strip .strip-reason'))",
    );
    assert_eq!(
        page.js("!!document.querySelector('.strip .hooks-tap')"),
        json!(false)
    );
    shoot(&page, "desktop-6-not-only-spandecks");

    // The station's switch, in Agent setup.
    page.js(
        "[...document.querySelectorAll('button')].find(b => b.innerText === 'Agent setup').click()",
    );
    page.wait(
        "the switch, off",
        "(l => !!l && l.innerText.includes('spark2: off') && !l.querySelector('input').checked)(document.querySelector('.hooks-switch'))",
    );
    page.js("document.querySelector('.hooks-switch').scrollIntoView()");
    shoot(&page, "desktop-7-agent-setup-off");
    page.js("document.querySelector('.hooks-switch input').click()");
    wait_for("the switch on the station", 10, || {
        (standing(&s)["on"] == json!(true)).then_some(())
    });
    page.wait(
        "the switch, on",
        "(l => !!l && l.innerText.includes('spark2: on') && l.querySelector('input').checked)(document.querySelector('.hooks-switch'))",
    );
    shoot(&page, "desktop-8-agent-setup-on");
    drop(page);

    // ---- phone ----
    let phone = Page::open(&s, 390, 844);
    let sheet = |phone: &Page| {
        // Tapped again until the sheet is there: a tap before the page
        // has heard from its station is answered "No station is
        // answering".
        phone.wait(
            "New session, and its sheet",
            "!!document.querySelector('.sheet .tile-pick') || (document.querySelector('.new-session')?.click(), false)",
        );
        phone.js("[...document.querySelectorAll('.sheet .tile-pick')].find(b => b.innerText.includes('Codex')).click()");
    };
    sheet(&phone);
    phone.wait("the tick, on", &tick(".sheet", true));
    phone.js("document.querySelector('.sheet .hooks-row').scrollIntoView({block: 'center'})");
    shoot(&phone, "phone-1-tick-on");
    phone.js("document.querySelector('.sheet .hooks-row input').click()");
    let other = folder(&s, "phone");
    set(
        &phone,
        ".sheet input[aria-label=Folder]",
        &other.display().to_string(),
    );
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
    assert_eq!(standing(&s)["on"], json!(false));
    assert!(!s.text(&asked).contains(ANY_ANSWER));
    shoot(&phone, "phone-2-pane-waits");
    phone.goto("#/");
    sheet(&phone);
    phone.wait("the tick, remembered off", &tick(".sheet", false));
    phone.js("document.querySelector('.sheet .hooks-row').scrollIntoView({block: 'center'})");
    shoot(&phone, "phone-3-remembered-off");
}
