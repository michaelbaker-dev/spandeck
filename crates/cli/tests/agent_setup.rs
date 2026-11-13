//! Agent setup: MCP servers, skills and model defaults set once in the
//! Control Deck and applied to every agent on every station; and running a
//! command in agents from the deck.
mod common;
use common::*;
use serde_json::{json, Value};

fn setup() -> Value {
    json!({
        "mcp_servers": {
            "docs": {"command": "npx", "args": ["-y", "docs-mcp"], "env": {"DOCS_ROOT": "/srv/docs"}},
            "tracker": {"url": "https://mcp.example.com/tracker", "agents": ["claude"]}
        },
        "skills": {
            "release-notes": {"description": "Write release notes in the house style", "content": "# Release notes\n\nLead with what changed for users."}
        },
        "defaults": {"claude": {"model": "opus", "effort": "high"}, "codex": {"model": "gpt-5.5-codex", "effort": "medium"}}
    })
}

/// A stand-in for an agent binary that prints the arguments it was started with.
fn arg_printer(s: &Station) -> String {
    let path = s.root.join("print-args.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\nfor a in \"$@\"; do echo \"ARG<$a>\"; done\nsleep 60\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

#[test]
fn one_setup_reaches_every_station_and_every_new_agent() {
    let (a, b) = Station::pair();
    let out = a.call("setup.set", json!({"setup": setup()}));
    let stations: Vec<&str> = out["stations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["station"].as_str().unwrap())
        .collect();
    assert_eq!(stations, vec!["alpha", "bravo"]);
    assert!(
        out["stations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["ok"] == json!(true)),
        "{out}"
    );

    for s in [&a, &b] {
        // The same list on both stations.
        let view = s.call("setup.get", json!({}));
        assert_eq!(
            view["setup"]["mcp_servers"]["docs"]["command"],
            json!("npx")
        );
        assert!(view["setup"]["updated_by"]
            .as_str()
            .unwrap()
            .contains("alpha"));
        // Skills are written where each agent looks for them.
        for dir in [".claude/skills", ".codex/skills"] {
            let skill = std::fs::read_to_string(
                s.root.join("home").join(dir).join("release-notes/SKILL.md"),
            )
            .unwrap();
            assert!(skill.starts_with("---\nname: release-notes\ndescription: Write release notes in the house style\n---"), "{skill}");
            assert!(skill.contains("Lead with what changed for users."));
        }
        // A new Claude Code pane starts with the model, effort and servers.
        let printer = arg_printer(s);
        let claude = s.call(
            "pane.spawn",
            json!({"agent": "claude", "program": printer, "cwd": s.root.join("home")}),
        )["id"]
            .as_str()
            .unwrap()
            .to_string();
        let screen = s.wait_text(&claude, "ARG<--mcp-config>").replace('\n', "");
        assert!(
            screen.contains("ARG<--model>ARG<opus>ARG<--effort>ARG<high>"),
            "{screen}"
        );
        assert!(
            screen.contains("\"docs\":{")
                && screen.contains("\"tracker\":{\"type\":\"http\"")
                && screen.contains("\"spandeck\":{"),
            "{screen}"
        );
        // Codex gets its own form of the same thing, without the Claude-only server.
        let codex = s.call(
            "pane.spawn",
            json!({"agent": "codex", "program": printer, "cwd": s.root.join("home")}),
        )["id"]
            .as_str()
            .unwrap()
            .to_string();
        let screen = s
            .wait_text(&codex, "mcp_servers.spandeck.command")
            .replace('\n', "");
        assert!(
            screen.contains("ARG<-m>ARG<gpt-5.5-codex>")
                && screen.contains("model_reasoning_effort=\"medium\""),
            "{screen}"
        );
        assert!(
            screen.contains("mcp_servers.docs.command=\"npx\"") && !screen.contains("tracker"),
            "{screen}"
        );
    }

    // Removing a skill removes the copies spandeck wrote, and nothing else.
    let hand_made = b.root.join("home/.claude/skills/my-own");
    std::fs::create_dir_all(&hand_made).unwrap();
    std::fs::write(hand_made.join("SKILL.md"), "mine").unwrap();
    let mut next = setup();
    next["skills"] = json!({});
    a.call("setup.set", json!({"setup": next}));
    assert!(!b.root.join("home/.claude/skills/release-notes").exists());
    assert_eq!(
        std::fs::read_to_string(hand_made.join("SKILL.md")).unwrap(),
        "mine"
    );
}

#[test]
fn a_station_that_was_away_catches_up_when_it_is_seen_again() {
    let (a, mut b) = Station::pair();
    b.kill9();
    wait_for("bravo to be seen as unreachable", 20, || {
        let v = a.call("fleet.refresh", json!({}));
        v["stations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["station"] == json!("bravo") && s["state"] == json!("unreachable"))
            .then_some(())
    });
    let out = a.call("setup.set", json!({"setup": setup()}));
    let bravo = out["stations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["station"] == json!("bravo"))
        .unwrap()
        .clone();
    assert_eq!(
        (bravo["ok"].as_bool(), bravo["pending"].as_bool()),
        (Some(false), Some(true)),
        "said plainly: not applied yet"
    );

    b.launch();
    wait_for("bravo to receive the setup", 30, || {
        a.call("fleet.refresh", json!({}));
        (b.call("setup.get", json!({}))["setup"]["mcp_servers"]["docs"]["command"] == json!("npx"))
            .then_some(())
    });
    assert!(b
        .root
        .join("home/.claude/skills/release-notes/SKILL.md")
        .exists());
}

#[test]
fn a_command_is_typed_into_idle_agents_and_busy_ones_are_skipped() {
    let (a, b) = Station::pair();
    let idle_a = a.fake(json!({"steps": []}), json!({"name": "idle-a"}));
    let busy_a = a.fake(
        json!({"steps": [{"prompt": "x"}, {"work": 60000}]}),
        json!({"name": "busy-a"}),
    );
    let idle_b = b.fake(json!({"steps": []}), json!({"name": "idle-b"}));
    let shell = a.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "cwd": a.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    a.wait_text(&idle_a, "[fake] session");
    b.wait_text(&idle_b, "[fake] session");
    a.wait_state(&busy_a, "working");

    let out = a.call(
        "agents.command",
        json!({"command": "/model opus", "fleet": true}),
    );
    a.wait_text(&idle_a, "[fake] got: /model opus");
    b.wait_text(&idle_b, "[fake] got: /model opus");
    let alpha = &out["stations"][0];
    assert_eq!(alpha["sent"].as_array().unwrap().len(), 1);
    assert_eq!(alpha["skipped"][0]["name"], json!("busy-a"));
    assert!(alpha["skipped"][0]["why"]
        .as_str()
        .unwrap()
        .contains("middle of a turn"));
    assert!(
        !a.text(&busy_a).contains("/model"),
        "a working agent was left alone"
    );
    assert!(!a.text(&shell).contains("/model"), "shells are not agents");

    // One pane, by name: how you run /login in the agent that needs it.
    a.call(
        "agents.command",
        json!({"command": "/login", "pane": idle_a}),
    );
    a.wait_text(&idle_a, "[fake] got: /login");
    assert!(!b.text(&idle_b).contains("/login"));
}

#[test]
fn only_the_user_can_change_the_setup_and_bad_entries_are_refused() {
    let s = Station::start("alpha");
    let agent = s.fake(json!({"steps": []}), json!({}));
    s.wait_text(&agent, "[fake] session");
    assert_eq!(
        s.call_as_pane(&agent, "setup.set", json!({"setup": setup()}))
            .unwrap_err()
            .code,
        "denied"
    );
    assert_eq!(
        s.call_as_pane(&agent, "agents.command", json!({"command": "/clear"}))
            .unwrap_err()
            .code,
        "denied"
    );

    let bad = |patch: Value| {
        s.try_call("setup.set", json!({"setup": patch, "fleet": false}))
            .unwrap_err()
            .message
    };
    assert!(
        bad(json!({"mcp_servers": {"spandeck": {"command": "x"}}})).contains("spandeck"),
        "cannot shadow spandeck's own server"
    );
    assert!(bad(json!({"mcp_servers": {"x": {}}})).contains("command or an http(s) URL"));
    assert!(
        bad(json!({"skills": {"../escape": {"description": "d", "content": "c"}}}))
            .contains("skill name")
    );
    assert!(
        bad(json!({"defaults": {"claude": {"model": "opus; rm -rf /"}}}))
            .contains("not a valid setting")
    );
    assert_eq!(
        s.call("setup.get", json!({}))["setup"]["updated_ms"],
        json!(0),
        "nothing was saved"
    );
}

#[test]
fn the_control_deck_edits_the_setup_and_runs_a_command_in_an_agent() {
    use common::browser::Page;
    let s = Station::start("alpha");
    let pane = s.fake(json!({"steps": []}), json!({"name": "helper"}));
    s.wait_text(&pane, "[fake] session");
    let page = Page::open(&s, 1400, 900);
    page.wait(
        "the top bar",
        "[...document.querySelectorAll('.topbar button')].some(b => b.innerText === 'Agent setup')",
    );

    // Add an MCP server and a default model in the Agent setup dialog.
    page.js("[...document.querySelectorAll('.topbar button')].find(b => b.innerText === 'Agent setup').click()");
    page.wait(
        "the dialog",
        "!!document.querySelector('.dialog [aria-label=\"Server name\"]')",
    );
    page.js("(() => { const set = (sel, v) => { const el = document.querySelector(sel); el.value = v; el.dispatchEvent(new Event('input', {bubbles: true})); }; \
              set('[aria-label=\"Server name\"]', 'docs'); set('[aria-label=\"Command or URL\"]', 'npx -y docs-mcp'); \
              [...document.querySelectorAll('.dialog button')].find(b => b.innerText === 'Add').click(); return true; })()");
    page.wait(
        "the server in the list",
        "[...document.querySelectorAll('.dialog td b')].some(b => b.innerText === 'docs')",
    );
    page.js("(() => { const el = document.querySelector('[aria-label=\"claude model\"]'); el.value = 'opus'; el.dispatchEvent(new Event('input', {bubbles: true})); \
              [...document.querySelectorAll('.dialog button')].find(b => b.innerText === 'Save for every station').click(); return true; })()");
    let saved = wait_for("the setup to be saved", 10, || {
        let v = s.call("setup.get", json!({}));
        (v["setup"]["updated_ms"] != json!(0)).then_some(v)
    });
    assert_eq!(
        saved["setup"]["mcp_servers"]["docs"],
        json!({"command": "npx", "args": ["-y", "docs-mcp"], "env": {}, "url": null, "agents": []})
    );
    assert_eq!(saved["setup"]["defaults"]["claude"]["model"], json!("opus"));

    // Right-click an agent, run /login in it: the command is typed and its terminal opens.
    page.goto("/s/alpha");
    page.wait("the deck", "!!document.querySelector('.pane-tile')");
    page.js("document.querySelector('.pane-tile').dispatchEvent(new MouseEvent('contextmenu', {bubbles: true, clientX: 300, clientY: 200}))");
    page.js("[...document.querySelectorAll('.menu-item')].find(i => i.innerText.includes('Run a command in it')).click()");
    page.wait(
        "the command dialog",
        "!!document.querySelector('.dialog [aria-label=\"Command\"]')",
    );
    page.js("(() => { [...document.querySelectorAll('.dialog button.mini')].find(b => b.innerText === '/login').click(); \
              [...document.querySelectorAll('.dialog button')].find(b => b.innerText === 'Run and open the terminal').click(); return true; })()");
    s.wait_text(&pane, "[fake] got: /login");
    page.wait("the agent's terminal", &format!("location.hash === '#/s/alpha/p/{pane}' && !!document.querySelector('.pane .xterm-rows')"));
}
