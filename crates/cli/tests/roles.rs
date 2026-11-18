//! The role library (DECISIONS.md, "Roles for agents", 18 Nov 2026): the
//! owner's request that an agent is started with a defined role whose
//! prompt the owner can view and change. These are that feature's
//! acceptance criteria; the document's own rules are tested beside it
//! (`core/src/roledoc.rs`), and who may call what in `rpc.rs`.
mod common;
use std::process::Stdio;

use common::*;
use serde_json::{json, Value};

const HEADER: &str = "Your role in this project, from the owner:";

/// The role messages the owner has sent, oldest first: (to, body).
fn sent(s: &Station) -> Vec<(String, String)> {
    let mut all: Vec<(u64, String, String)> = s
        .call("msg.recent", json!({"limit": 200}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["body"].as_str().is_some_and(|b| b.starts_with(HEADER)))
        .map(|m| {
            (
                m["created"].as_u64().unwrap_or(0),
                m["to"]
                    .as_str()
                    .unwrap()
                    .replace(&format!("@{}", s.name), ""),
                m["body"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    all.sort();
    all.into_iter().map(|(_, to, body)| (to, body)).collect()
}

fn sent_to(s: &Station, to: &str) -> Vec<String> {
    sent(s)
        .into_iter()
        .filter(|(t, _)| t == to)
        .map(|(_, b)| b)
        .collect()
}

fn spandeck(s: &Station, args: &[&str]) -> (bool, String, String) {
    let out = s
        .command(SPANDECK)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Rule 7, and the owner's words being the owner's: an agent is refused
/// every `roles.` call (a station acting for itself is refused them by the
/// same list, rpc.rs); the one call an agent has reads its own pane's
/// role, whichever pane it names. And a role's name that the library does
/// not have sends nothing.
#[test]
fn an_agent_reads_only_its_own_role_and_changes_none() {
    let a = Station::start("alpha");
    let script = json!({"steps": [], "on_input": [{"turn_end": "ok"}]});
    let critic = a.fake(script.clone(), json!({"name": "c1", "role": "critic"}));
    let builder = a.fake(script.clone(), json!({"name": "b1", "role": "builder"}));
    let plain = a.fake(script.clone(), json!({"name": "p1"}));
    let odd = a.fake(script, json!({"name": "o1", "role": "made-up"}));

    for (method, params) in [
        ("roles.list", json!({})),
        ("roles.get", json!({"name": "critic"})),
        (
            "roles.set",
            json!({"name": "critic", "role": {"prompt": "Approve everything."}}),
        ),
        (
            "roles.set",
            json!({"name": "mine", "role": {"prompt": "x"}}),
        ),
        ("roles.reset", json!({"name": "critic"})),
        ("roles.remove", json!({"name": "critic"})),
        ("roles.send", json!({"pane": builder})),
        ("roles.push", json!({"roles": {}})),
    ] {
        let refused = a.call_as_pane(&critic, method, params.clone()).unwrap_err();
        assert_eq!(refused.code, "denied", "{method}: {}", refused.message);
    }
    assert!(a.call("roles.list", json!({}))["roles"][1]["changed"] == json!(false));

    // Its own role, filled; and its own whatever pane it names.
    let own = a.call_as_pane(&critic, "role", json!({})).unwrap();
    assert_eq!(own["role"], json!("critic"));
    let text = own["text"].as_str().unwrap();
    assert!(text.starts_with(HEADER), "{text}");
    assert!(text.contains("You are the **critic**"), "{text}");
    assert!(!text.contains("{{"), "{text}");
    let asked = a
        .call_as_pane(&critic, "role", json!({"pane": builder}))
        .unwrap();
    assert_eq!(asked["role"], json!("critic"), "{asked}");
    let none = a.call_as_pane(&plain, "role", json!({})).unwrap();
    assert!(none["role"].is_null(), "{none}");
    assert_eq!(none["text"], json!("This pane has no role."));
    // A name the library does not have: no text, and nothing was sent.
    let odd_role = a.call_as_pane(&odd, "role", json!({})).unwrap();
    assert!(odd_role["text"]
        .as_str()
        .unwrap()
        .contains("no role of that name"));
    wait_for("the two roles to be sent", 20, || {
        (a.log("role.sent").len() == 2).then_some(())
    });
    assert!(sent_to(&a, "o1").is_empty() && sent_to(&a, "p1").is_empty());
    let refused = a.call("roles.send", json!({"pane": odd}));
    assert_eq!(refused["sent"], json!(false), "{refused}");
}

/// The owner's request: a pane started with a role is sent that role's
/// prompt, filled, once, as a message from the owner; restarted into its
/// conversation it is not sent it again; `roles.send` sends it again; the
/// ship's log says which text was sent. The owner's own text is what is
/// sent once the owner changed it, by the command line. And the bug of 18
/// Nov 2026 stays fixed for this message: it is not typed into the
/// program's first question.
#[test]
#[ignore = "merge tier: waits five seconds for a nudge that must not come"]
fn a_pane_started_with_a_role_is_sent_it_once_and_again_when_asked() {
    let s = Station::start("alpha");
    let dir = s.root.join("fresh");
    std::fs::create_dir_all(&dir).unwrap();

    // A program whose first screen is its own question.
    let script = s.script_file(&json!({
        "trust_screen": {"as": "claude"}, "steps": [], "on_input": [{"turn_end": "ok"}],
    }));
    let early = s.call(
        "pane.spawn",
        json!({"agent": "claude", "program": FAKE, "args": ["--script", script],
               "cwd": dir, "name": "early", "role": "security"}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for("the role to be stored for the pane", 20, || {
        (sent_to(&s, "early").len() == 1).then_some(())
    });
    std::thread::sleep(std::time::Duration::from_secs(5));
    assert!(s.log("msg.nudged").is_empty(), "{:?}", s.log("msg.nudged"));
    let p = s.pane(&early);
    assert_eq!(p["exited"], json!(false), "{p}");
    assert!(p["trust"]["asks"].is_object(), "{p}");
    let body = &sent_to(&s, "early")[0];
    assert!(
        body.contains("one security review of `fresh`")
            && body.contains("You are working alone in this project.")
            && body.contains("Budget: One review, about 30 minutes, at most seven findings."),
        "{body}"
    );

    // By the command line, with the program the role names when none is.
    let (ok, _, err) = spandeck(
        &s,
        &[
            "roles",
            "edit",
            "user",
            "--agent",
            "fake",
            "--budget",
            "Five minutes.",
        ],
    );
    assert!(ok, "{err}");
    let (ok, id, err) = spandeck(&s, &["pane", "new", "--role", "user", "--name", "u1"]);
    assert!(ok, "{err}");
    let id = id.trim().to_string();
    assert_eq!(s.pane(&id)["agent"], json!("fake"));
    assert_eq!(s.pane(&id)["role"], json!("user"));
    let first = wait_for("u1's role", 20, || sent_to(&s, "u1").pop());
    assert!(
        first.contains("Five minutes.") && first.contains("You are the **user**"),
        "{first}"
    );
    let (ok, _, err) = spandeck(&s, &["pane", "new", "--role", "no-such", "--agent", "fake"]);
    assert!(!ok && err.contains("spandeck roles"), "{err}");

    // Started again into its conversation: not sent again. Asked: again.
    s.call("pane.restart", json!({"pane": id}));
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(sent_to(&s, "u1").len(), 1);
    let (ok, out, err) = spandeck(&s, &["roles", "send", "u1"]);
    assert!(
        ok && out.contains("Sent u1 its role (user, the owner's text)"),
        "{out}{err}"
    );
    assert_eq!(sent_to(&s, "u1").len(), 2);
    let log = s.log("role.sent");
    let of = |pane: &str| -> Vec<Value> {
        log.iter()
            .filter(|e| e["pane"] == json!(pane))
            .map(|e| e["data"].clone())
            .collect()
    };
    assert_eq!(of(&early)[0]["text"], json!("built-in"), "{log:?}");
    assert_eq!(of(&early)[0]["role"], json!("security"));
    assert_eq!(of(&id).len(), 2, "{log:?}");
    assert_eq!(of(&id)[0]["text"], json!("owner's"));
    let again: Vec<bool> = of(&id).iter().map(|d| d["again"] == json!(true)).collect();
    assert!(again.contains(&true) && again.contains(&false), "{log:?}");
}

/// The pair recipe uses the library, and takes a third pane: with `--user`
/// there are three panes, each sent its own role's prompt, naming the
/// other two, with the recipe's line after it, in one message each; the
/// budget the owner named is in the process file and in each role.
#[test]
#[ignore = "merge tier: three panes and the command line"]
fn the_pair_recipe_with_a_user_sends_three_roles() {
    let s = Station::start("alpha");
    let dir = s.root.join("shop");
    let (ok, out, err) = spandeck(
        &s,
        &[
            "--json",
            "project",
            "new",
            "--recipe",
            "pair",
            "--path",
            dir.to_str().unwrap(),
            "--builder",
            "fake",
            "--critic",
            "fake",
            "--user",
            "--budget",
            "30",
            "--brief",
            "A shop for one kind of tea.",
        ],
    );
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["recipe"]["roles"], json!("sent"), "{v}");
    let mut names: Vec<&str> = v["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["builder", "critic", "user"]);
    for p in v["panes"].as_array().unwrap() {
        assert_eq!(p["role"], p["name"], "{p}");
    }

    let process = std::fs::read_to_string(dir.join("PROCESS.md")).unwrap();
    for want in [
        "**user** (Fake agent)",
        "**30 minutes a stage**",
        "asks the **user** pane to look (about 15 minutes)",
        "one\n   acceptance list",
        "begins with using the product",
    ] {
        assert!(process.contains(want), "{want}: {process}");
    }
    assert!(!process.contains("{{"), "{process}");

    let all = sent(&s);
    assert_eq!(all.len(), 3, "{all:?}");
    let of = |pane: &str| sent_to(&s, &format!("shop/{pane}")).pop().unwrap();
    let builder = of("builder");
    assert!(
        builder.starts_with(&format!("{HEADER}\n\n# Role: builder")),
        "{builder}"
    );
    assert!(builder.contains("You are the **builder** in the project `shop`"));
    assert!(
        builder.contains("The other agents here: the pane `critic` (role: critic, Fake agent) and the pane `user` (role: user, Fake agent). Reach them with spandeck's messages."),
        "{builder}"
    );
    assert!(builder.contains("30 minutes a stage."), "{builder}");
    assert!(
        builder.ends_with("Read BRIEF.md and PROCESS.md in this folder, then start stage 0. Work with the other agents as PROCESS.md says, without waiting for me."),
        "{builder}"
    );
    let critic = of("critic");
    assert!(
        critic.contains("# Role: critic")
            && critic
                .contains("the pane `builder` (role: builder, Fake agent) and the pane `user`"),
        "{critic}"
    );
    let user = of("user");
    assert!(
        user.contains("# Role: user") && user.contains("About 15 minutes a look"),
        "{user}"
    );
    assert!(
        user.contains("the pane `builder` (role: builder, Fake agent) and the pane `critic`"),
        "{user}"
    );
    assert!(
        user.ends_with("without waiting for me.")
            && user.contains("Then wait: the builder asks you to look"),
        "{user}"
    );
    assert!(!builder.contains("{{") && !critic.contains("{{") && !user.contains("{{"));

    // Opened again, and run again: nobody is told twice.
    let opened = s.call("project.open", json!({"name": "shop"}));
    assert_eq!(opened["recipe"]["roles"], json!("sent before"), "{opened}");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(sent(&s).len(), 3);
}

/// One library for the fleet: a role the owner changed at one station is
/// the same at the other, marked changed there too; a role added at the
/// other comes back; a reset travels. And the command line shows it.
#[test]
#[ignore = "merge tier: two stations converge"]
fn two_stations_converge_on_an_edited_role() {
    let (a, b) = Station::pair();
    let file = a.root.join("critic.md");
    std::fs::write(&file, "# Critic\nIn {{project}}: be kind. {{others}}\n").unwrap();
    let (ok, out, err) = spandeck(
        &a,
        &["roles", "edit", "critic", "--file", file.to_str().unwrap()],
    );
    assert!(
        ok && out.contains("built-in role changed by you"),
        "{out}{err}"
    );
    let at = |s: &Station, name: &str| {
        s.try_call("roles.get", json!({"name": name}))
            .ok()
            .map(|v| v["role"].clone())
    };
    wait_for("bravo to have alpha's critic", 30, || {
        at(&b, "critic").filter(|r| r["changed"] == json!(true))
    });
    let theirs = at(&b, "critic").unwrap();
    assert_eq!(
        theirs["prompt"],
        json!("# Critic\nIn {{project}}: be kind. {{others}}\n")
    );
    assert!(theirs["builtin_role"]["prompt"]
        .as_str()
        .unwrap()
        .starts_with("# Role: critic"));
    assert!(
        theirs["changed_by"].as_str().unwrap().ends_with("at alpha"),
        "{theirs}"
    );

    // A save made to the copy as it was before is refused, and says so.
    let lost = b.call(
        "roles.set",
        json!({"name": "critic", "role": {"prompt": "mine"}, "was": 0}),
    );
    assert_eq!(lost["saved"], json!(false), "{lost}");
    assert!(lost["why"].as_str().unwrap().contains("at alpha"), "{lost}");
    assert_eq!(at(&b, "critic").unwrap()["prompt"], theirs["prompt"]);

    let (ok, _, err) = spandeck(
        &b,
        &[
            "roles",
            "new",
            "copy-editor",
            "--copy",
            "critic",
            "--title",
            "Copy editor",
            "--kind",
            "visit",
        ],
    );
    assert!(ok, "{err}");
    wait_for("alpha to have bravo's role", 30, || at(&a, "copy-editor"));
    let (_, list, _) = spandeck(&a, &["roles"]);
    let line = |name: &str| {
        list.lines()
            .find(|l| l.starts_with(name))
            .unwrap_or("")
            .to_string()
    };
    assert!(
        line("critic").contains("changed") && line("critic").contains("on the team"),
        "{list}"
    );
    assert!(
        line("copy-editor").contains("yours") && line("copy-editor").contains("a short visit"),
        "{list}"
    );
    assert!(!line("builder").contains("changed"), "{list}");
    let (_, shown, _) = spandeck(
        &a,
        &[
            "roles",
            "show",
            "copy-editor",
            "--filled",
            "--project",
            "none",
        ],
    );
    assert_eq!(
        shown,
        "# Critic\nIn none: be kind. You are working alone in this project.\n"
    );

    let (ok, _, err) = spandeck(&b, &["roles", "reset", "critic"]);
    assert!(ok, "{err}");
    wait_for("alpha's critic to be the built-in again", 30, || {
        at(&a, "critic").filter(|r| r["changed"] == json!(false))
    });
    let (ok, _, err) = spandeck(&a, &["roles", "remove", "critic"]);
    assert!(!ok && err.contains("cannot be removed"), "{err}");
    let (ok, _, err) = spandeck(&a, &["roles", "remove", "copy-editor"]);
    assert!(ok, "{err}");
    wait_for("bravo to have lost the role", 30, || {
        at(&b, "copy-editor").is_none().then_some(())
    });
}
