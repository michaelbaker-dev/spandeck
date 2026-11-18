//! Agent setup: MCP servers, skills and model defaults set once in the
//! Control Deck and applied to every agent on every station; and running a
//! command in agents from the deck. And the other document every station
//! shares: the deck's global layouts and pins. And the agent programs
//! themselves: installed when missing, their versions shown, updated.
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
    spandeck_terminal::sys::set_executable(&path).unwrap();
    path.display().to_string()
}

#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn one_setup_reaches_every_station_and_every_new_agent() {
    let (a, b) = Station::pair_told();
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
        // Wait for the last argument: the screen fills in pieces, and reading it
        // at the first sight of --mcp-config sometimes caught it half printed.
        let screen = s.wait_text(&claude, "ARG<mcp__spandeck>").replace('\n', "");
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
    let (a, mut b) = Station::pair_told();
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
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
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
    // The agent has to have finished the turn the first command began, or
    // this one is rightly skipped as "in the middle of a turn".
    // (Asked again until it is taken: looked at too soon, the agent had not
    // yet been seen to start that turn, and began it between the look and
    // the command. Seen on a CI runner, 17 Nov 2026.)
    wait_for("the command to be taken by the one pane", 10, || {
        let one = a.call(
            "agents.command",
            json!({"command": "/login", "pane": idle_a}),
        );
        (one["stations"][0]["sent"].as_array().unwrap().len() == 1).then_some(())
    });
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

/// The permission mode of each program is set in Agent setup (until 18 Nov
/// 2026 only a raw call could set it, and for Codex it was dropped). Codex
/// is given its own three, each with what it means; nothing is set until
/// the owner chooses; a program whose modes spandeck does not know is given
/// no list, and a word the station would refuse is not sent.
#[test]
#[ignore = "merge tier: a station and a browser (about five seconds)"]
fn the_permission_mode_is_chosen_in_agent_setup() {
    use common::browser::Page;
    let s = Station::start("alpha");
    let page = Page::open(&s, 1400, 900);
    let open = |page: &Page| {
        page.wait(
            "Agent setup",
            "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.topbar button')].find(b => b.innerText === 'Agent setup'))",
        );
        page.wait(
            "Codex's permission mode",
            "!!document.querySelector('.dialog .perm-row[data-agent=\"codex\"] select')",
        );
    };
    const CODEX: &str = "document.querySelector('.dialog .perm-row[data-agent=\"codex\"] select')";
    const CLAUDE: &str = "document.querySelector('.dialog .perm-row[data-agent=\"claude\"]')";
    open(&page);
    // Nothing is set, and that is what is chosen; the screen says whom it
    // applies to.
    assert_eq!(
        page.js(&format!(
            "[{CODEX}.value, [...{CODEX}.options].map(o => o.textContent)]"
        )),
        json!([
            "",
            [
                "Not set (Codex's own setting)",
                "Ask me",
                "Review for itself",
                "Never ask"
            ]
        ])
    );
    let rule = page.js("document.querySelector('.dialog .perm-rule').textContent");
    let rule = rule.as_str().unwrap();
    assert!(
        rule.contains("panes started from now on, on every station of the fleet")
            && rule.contains("A station running an older spandeck may ignore it"),
        "{rule}"
    );
    // Claude Code: its own setting, or a word typed; a word the station
    // would refuse is marked, and is not sent.
    assert_eq!(
        page.js(&format!(
            "[...{CLAUDE}.querySelectorAll('option')].map(o => o.textContent)"
        )),
        json!(["Not set (Claude Code's own setting)", "A mode I type"])
    );
    page.js(&format!(
        "(() => {{ const row = {CLAUDE}, pick = row.querySelector('select'), box = row.querySelector('input'); \
           pick.value = 'typed'; pick.dispatchEvent(new Event('change', {{bubbles: true}})); \
           box.value = 'two words'; box.dispatchEvent(new Event('input', {{bubbles: true}})); \
           [...document.querySelectorAll('.dialog button')].find(b => b.innerText === 'Save for every station').click(); return true; }})()"
    ));
    page.wait(
        "the word refused on the page",
        &format!("{CLAUDE}.querySelector('input').getAttribute('aria-invalid') === 'true' && [...document.querySelectorAll('.toast.error')].some(t => t.textContent.includes('two words'))"),
    );
    assert_eq!(
        s.call("setup.get", json!({}))["setup"]["updated_ms"],
        json!(0)
    );
    page.js(&format!(
        "(() => {{ const pick = {CLAUDE}.querySelector('select'); pick.value = ''; pick.dispatchEvent(new Event('change', {{bubbles: true}})); return true; }})()"
    ));

    // Codex reviews for itself.
    page.js(&format!(
        "(() => {{ const pick = {CODEX}; pick.value = [...pick.options].find(o => o.textContent === 'Review for itself').value; pick.dispatchEvent(new Event('change', {{bubbles: true}})); return true; }})()"
    ));
    let on = page.js(
        "document.querySelector('.dialog .perm-row[data-agent=\"codex\"] .perm-says .on').textContent",
    );
    assert!(
        on.as_str()
            .unwrap()
            .starts_with("Review for itself: Codex's own reviewer decides"),
        "{on}"
    );
    page.js("[...document.querySelectorAll('.dialog button')].find(b => b.innerText === 'Save for every station').click()");
    let saved = wait_for("the setup to be saved", 10, || {
        let v = s.call("setup.get", json!({}));
        (v["setup"]["updated_ms"] != json!(0)).then_some(v)
    });
    assert_eq!(
        saved["setup"]["defaults"]["codex"]["permission_mode"],
        json!("auto")
    );
    assert_eq!(
        saved["setup"]["defaults"]["claude"]["permission_mode"],
        json!(null)
    );
    // And it is what the screen shows when it is opened again.
    page.reload();
    open(&page);
    assert_eq!(
        page.js(&format!(
            "[{CODEX}.value, {CODEX}.selectedOptions[0].textContent, {CLAUDE}.querySelector('select').value]"
        )),
        json!(["auto", "Review for itself", ""])
    );
}

// ---------------------------------------------------------------- the deck
// The owner's rule (14 Nov 2026): "every station has the exact same left
// window". Global layouts and pins are one document on every station, kept
// the same the way the agent setup is.

fn everything() -> Value {
    json!({"name": "everything", "lines": true, "tree": {"dir": "row", "ratio": 500,
        "a": {"pane": "build", "project": "api", "station": "bravo"},
        "b": {"dir": "col", "ratio": 400, "a": {"id": "p-01abc"}, "b": {"id": "p-01abd", "station": "alpha"}}}})
}

#[test]
fn global_layouts_and_pins_are_the_same_on_every_station_and_only_the_owner_changes_them() {
    let (a, b) = Station::pair();
    let saved = a.call("deck.layout.save", json!({"layout": everything()}));
    assert_eq!(saved, json!({"layout": "everything", "changed": true}));
    assert_eq!(
        a.call("deck.layout.save", json!({"layout": everything()}))["changed"],
        json!(false)
    );
    let pins = [
        json!({"kind": "pane", "station": "bravo", "id": "p-01abc"}),
        json!({"kind": "layout", "name": "everything"}),
        json!({"kind": "layout", "station": "bravo", "project": "api", "name": "build"}),
    ];
    for pin in &pins {
        a.call("deck.pin", json!({"pin": pin}));
    }
    // Pinning twice pins once.
    assert_eq!(a.call("deck.pin", json!({"pin": pins[0]})), json!(pins));

    let same_on_both = |what: &str| {
        wait_for(what, 15, || {
            let (mine, theirs) = (a.call("deck.get", json!({})), b.call("deck.get", json!({})));
            (mine == theirs).then_some(theirs)
        })
    };
    let deck = same_on_both("bravo to have the same deck");
    assert_eq!(deck["layouts"], json!([everything()]));
    assert_eq!(deck["pins"], json!(pins));
    assert!(b
        .log("deck.received")
        .iter()
        .any(|e| e["actor"] == json!("alpha")));

    // A change made on the other station comes back the same way. Removing a
    // global layout unpins it, and nothing else.
    b.call("deck.unpin", json!({"pin": pins[0]}));
    assert_eq!(
        b.call("deck.layout.remove", json!({"layout": "everything"})),
        json!({"removed": "everything"})
    );
    let deck = same_on_both("alpha to have bravo's changes");
    assert_eq!(
        (&deck["layouts"], &deck["pins"]),
        (&json!([]), &json!([pins[2]]))
    );

    // An agent changes none of it, and cannot stand in for a station.
    let agent = a.fake(json!({"steps": []}), json!({}));
    a.wait_text(&agent, "[fake] session");
    for (method, params) in [
        ("deck.layout.save", json!({"layout": everything()})),
        ("deck.pin", json!({"pin": pins[0]})),
        ("deck.unpin", json!({"pin": pins[2]})),
        ("deck.layout.remove", json!({"layout": "everything"})),
        (
            "deck.push",
            json!({"deck": {"updated_ms": u64::MAX, "pins": [pins[0]]}}),
        ),
    ] {
        assert_eq!(
            a.call_as_pane(&agent, method, params).unwrap_err().code,
            "denied",
            "{method}"
        );
    }
    // Nor does the owner's own token push a whole document past the change path.
    assert_eq!(
        a.try_call("deck.push", json!({"deck": {"updated_ms": u64::MAX}}))
            .unwrap_err()
            .code,
        "denied"
    );

    // What is not a layout or a pin is refused and changes nothing.
    let bad = |layout: Value| {
        a.try_call("deck.layout.save", json!({"layout": layout}))
            .unwrap_err()
            .message
    };
    assert!(bad(json!({"name": "x", "tree": {"pane": "build", "id": "p-1"}})).contains("not both"));
    assert!(
        bad(json!({"name": "x", "tree": {"id": "p-1", "project": "api"}})).contains("no project")
    );
    assert!(
        bad(json!({"name": "x", "tree": {"pane": "build", "station": "no such/station"}}))
            .contains("station name")
    );
    assert!(bad(json!({"name": "../x"})).contains("layout names"));
    assert!(a
        .try_call(
            "deck.pin",
            json!({"pin": {"kind": "layout", "project": "api", "name": "build"}})
        )
        .unwrap_err()
        .message
        .contains("names its station and its project"));
    assert_eq!(a.call("deck.get", json!({})), deck);
}

#[test]
fn a_station_that_was_away_gets_the_deck_when_it_is_seen_again() {
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
    a.call("deck.layout.save", json!({"layout": everything()}));
    a.call(
        "deck.pin",
        json!({"pin": {"kind": "layout", "name": "everything"}}),
    );
    b.launch();
    wait_for("bravo to receive the deck", 30, || {
        a.call("fleet.refresh", json!({}));
        (b.try_call("deck.get", json!({})).ok()? == a.call("deck.get", json!({}))).then_some(())
    });
}

/// An executable script in `dir`.
fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    spandeck_terminal::sys::set_executable(&path).unwrap();
    path.display().to_string()
}

/// The owner: "we need an auto install for any station that doesn't have
/// these installed: they should run the install script if it's not installed
/// before trying to launch the agent", and the new agents "need to be set up
/// with MCP/skills automatically".
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn a_missing_agent_is_installed_in_its_pane_and_then_started_with_spandecks_tools() {
    let tools = tempfile::tempdir().unwrap();
    let t = tools.path();
    let source = script(
        t,
        "agent.src",
        "[ \"$1\" = --version ] && { echo 1.2.3; exit 0; }\nfor a in \"$@\"; do echo \"ARG<$a>\"; done\necho \"CONFIG<$OPENCODE_CONFIG_CONTENT>\"\necho AGENT-STARTED\nsleep 60",
    );
    let (opencode, agy) = (t.join("opencode"), t.join("agy"));
    let s = Station::start_with(
        "alpha",
        &format!(
            "agent_home = true\n[agents.opencode]\nprogram = \"{}\"\ninstall_cmd = [\"cp\", \"{source}\", \"{}\"]\n[agents.antigravity]\nprogram = \"{}\"\ninstall_cmd = [\"sh\", \"-c\", \"echo started-$((1+1)); sleep 30\"]\n",
            opencode.display(),
            opencode.display(),
            agy.display()
        ),
        &[],
    );
    let home = s.root.join("home");
    let spawn = |agent: &str| {
        s.call("pane.spawn", json!({"agent": agent, "cwd": home}))["id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // Not installed: the pane says so, shows the command, runs it, and then
    // starts the agent, wired to spandeck.
    let pane = spawn("opencode");
    let screen = s.wait_text(&pane, "AGENT-STARTED");
    assert!(
        screen.contains("OpenCode is not installed on this station"),
        "{screen}"
    );
    let flat = screen.replace('\n', "");
    assert!(
        flat.contains(&format!("cp {source} {}", opencode.display())),
        "the install command is shown as it is run: {screen}"
    );
    let session = s.pane(&pane)["session_id"].as_str().unwrap().to_string();
    assert!(session.starts_with("ses_"), "{session}");
    assert!(
        flat.contains(&format!("ARG<--session>ARG<{session}>ARG<--standalone>")),
        "{screen}"
    );
    assert!(
        flat.contains("CONFIG<{\"mcp\":{\"spandeck\":{")
            && flat.contains(&format!("\"SPANDECK_PANE\":\"{pane}\"")),
        "{screen}"
    );
    let skill = s.root.join("state/agent-skills/opencode/spandeck/SKILL.md");
    assert!(flat.contains(skill.parent().unwrap().parent().unwrap().to_str().unwrap()));
    assert!(std::fs::read_to_string(skill)
        .unwrap()
        .contains("name: spandeck"));

    // Ctrl-C skips the install: the pane ends, and says why.
    let pane = spawn("antigravity");
    // Pressed once the installer is running: a Ctrl-C in the instant the
    // shell is starting it is held until the installer ends.
    assert!(s
        .wait_text(&pane, "started-2")
        .contains("Press Ctrl-C to skip"));
    s.call(
        "pane.send_text",
        json!({"pane": pane, "text": "\u{3}", "enter": false}),
    );
    s.wait_text(&pane, "Skipped: Antigravity was not installed");
    wait_for("the skipped pane to end", 10, || {
        (s.pane(&pane)["exited"] == json!(true)).then_some(())
    });
    // Antigravity is wired through a plugin folder of spandeck's own.
    let plugin = home.join(".gemini/config/plugins/spandeck");
    let read = |f: &str| std::fs::read_to_string(plugin.join(f)).unwrap();
    assert!(read("mcp_config.json").contains("\"spandeck\""));
    assert!(read("hooks.json").contains("hook antigravity"));
    assert!(read("skills/spandeck/SKILL.md").contains("name: spandeck"));

    // No install command known: refused, in words.
    let err = s
        .try_call(
            "pane.spawn",
            json!({"agent": "fake", "program": t.join("nothing"), "cwd": home}),
        )
        .unwrap_err();
    assert!(
        err.message.contains("knows no install command"),
        "{}",
        err.message
    );
}

/// Every file under `dir`, with what it holds.
fn tree(dir: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut found = std::collections::BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = e.path();
            if path.is_dir() {
                found.insert(format!("{}/", path.display()), vec![]);
                todo.push(path);
            } else {
                found.insert(
                    path.display().to_string(),
                    std::fs::read(&path).unwrap_or_default(),
                );
            }
        }
    }
    found
}

/// What happened on the owner's machine on 16 Nov 2026: a throwaway
/// station, started for a test under its own state and config directories
/// but with the owner's real HOME, rewrote `~/.gemini/config/plugins/
/// spandeck/mcp_config.json` and `hooks.json` to point at its own binary,
/// and a setup applied there removes the skills it does not list. The
/// rule: only the station of the account (the usual state directory, with
/// the login service), or one whose config says `agent_home = true`,
/// writes anything into the agents' settings under the home folder. This
/// station is neither. Its home has what the owner's real station keeps
/// there; after a pane of every agent kind, a setup with a skill, and a
/// try at `integration install`, not one file of it has changed. Its
/// panes still get spandeck's tools, on their own command line.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn a_station_that_is_not_the_one_of_its_account_writes_nothing_into_the_agents_settings() {
    let tools = tempfile::tempdir().unwrap();
    let t = tools.path();
    let program = script(
        t,
        "agent",
        "[ \"$1\" = --version ] && { echo 1.2.3; exit 0; }\nfor a in \"$@\"; do echo \"ARG<$a>\"; done\necho \"CONFIG<$OPENCODE_CONFIG_CONTENT>\"\necho AGENT-STARTED\nsleep 60",
    );
    let agents: String = ["claude", "codex", "opencode", "antigravity"]
        .iter()
        .map(|a| format!("[agents.{a}]\nprogram = \"{program}\"\n"))
        .collect();
    let mut s = Station::not_started("trial", &agents);
    let home = s.root.join("home");
    // What the station of this account keeps in the home folder.
    let theirs = "/home/owner/.local/bin/spandeck";
    let plugin = home.join(".gemini/config/plugins/spandeck");
    for (file, content) in [
        (".gemini/config/plugins/spandeck/plugin.json", "{\"name\": \"spandeck\"}\n".to_string()),
        (".gemini/config/plugins/spandeck/mcp_config.json", format!("{{\"mcpServers\": {{\"spandeck\": {{\"command\": \"{theirs}\", \"args\": [\"mcp\"]}}}}}}\n")),
        (".gemini/config/plugins/spandeck/hooks.json", format!("{{\"spandeck\": {{\"Stop\": [{{\"type\": \"command\", \"command\": \"'{theirs}' hook antigravity\"}}]}}}}\n")),
        (".gemini/config/plugins/spandeck/.spandeck-managed", "Made by spandeck.\n".to_string()),
        (".gemini/config/plugins/spandeck/skills/old-notes/SKILL.md", "---\nname: old-notes\n---\n".to_string()),
        (".gemini/config/plugins/spandeck/skills/old-notes/.spandeck-managed", "x\n".to_string()),
        (".claude/skills/old-notes/SKILL.md", "---\nname: old-notes\n---\n".to_string()),
        (".claude/skills/old-notes/.spandeck-managed", "x\n".to_string()),
        (".codex/skills/old-notes/SKILL.md", "---\nname: old-notes\n---\n".to_string()),
        (".codex/skills/old-notes/.spandeck-managed", "x\n".to_string()),
        (".claude/settings.json", "{\"model\": \"opus\"}\n".to_string()),
        (".codex/hooks.json", "{\"hooks\": {}}\n".to_string()),
    ] {
        let path = home.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let before = tree(&home);
    s.launch();

    // A setup with a skill for every agent: saved, with the reason the
    // home folder's agents did not get it.
    let mut setup = setup();
    setup["skills"] = json!({"release-notes": {"description": "How we write release notes", "content": "Keep them short."}});
    let saved = s.call("setup.set", json!({"setup": setup, "fleet": false}));
    let problems = saved["stations"][0]["problems"].to_string();
    assert!(
        problems.contains("skills were not written for Claude Code, Codex or Antigravity")
            && problems.contains("agent_home = true"),
        "{problems}"
    );
    // A pane of each kind, each wired on its own command line or environment.
    let spawn = |agent: &str| {
        let pane = s.call("pane.spawn", json!({"agent": agent, "cwd": home}))["id"]
            .as_str()
            .unwrap()
            .to_string();
        (s.wait_text(&pane, "AGENT-STARTED").replace('\n', ""), pane)
    };
    let (claude, _) = spawn("claude");
    assert!(
        claude.contains("ARG<--settings>") && claude.contains("ARG<--mcp-config>"),
        "{claude}"
    );
    let (codex, _) = spawn("codex");
    assert!(codex.contains("mcp_servers.spandeck.command="), "{codex}");
    let (opencode, _) = spawn("opencode");
    assert!(opencode.contains("CONFIG<{\"mcp\":{"), "{opencode}");
    // Antigravity: the plugin the station of the account put there is
    // used as it is, and nothing is said against it.
    let (_, agy) = spawn("antigravity");
    assert_eq!(s.pane(&agy)["needs_reason"], json!(null));
    // The owner's settings are not this station's to change either way.
    for (method, agent) in [
        ("integration.install", "claude"),
        ("integration.install", "codex"),
        ("integration.uninstall", "claude"),
    ] {
        let err = s.try_call(method, json!({"agent": agent})).unwrap_err();
        assert!(
            err.code == "denied" && err.message.contains("nothing was changed"),
            "{method} {agent}: {} {}",
            err.code,
            err.message
        );
    }
    let doctor = s.call("station.doctor", json!({})).to_string();
    assert!(
        doctor.contains("does not write into the agent settings in your home folder"),
        "{doctor}"
    );
    assert_eq!(
        tree(&home),
        before,
        "nothing under the home folder was written, changed or removed"
    );

    // With no plugin there, an Antigravity pane has no spandeck tools:
    // the pane and a notice say so, and no plugin is made.
    std::fs::remove_dir_all(&plugin).unwrap();
    let (_, bare) = spawn("antigravity");
    let reason = s.pane(&bare)["needs_reason"].as_str().unwrap().to_string();
    assert!(
        reason.starts_with("started without spandeck's tools"),
        "{reason}"
    );
    let notices = s.call("notify.list", json!({})).to_string();
    assert!(notices.contains("has no spandeck tools"), "{notices}");
    assert!(!plugin.exists());
}

/// What happened on the owner's Mac: `/opt/homebrew/bin/codex` was an old npm
/// install whose real binary was gone. spandeck saw a file called `codex`,
/// called Codex installed, and a new pane only printed `Error: spawn …
/// ENOENT`. Earlier the same Mac's `codex --version` never answered, and
/// spandeck said Codex was not found. So: a program that is there but does
/// not start is told apart from one that is missing and from one that is
/// slow to answer; a copy that answers is used before one that does not; a
/// pane for a broken one offers a reinstall and then starts the copy that
/// works; and reinstalling by a chosen way is the owner's to ask for.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn a_program_that_is_there_but_does_not_start_is_reinstalled_not_called_installed_or_missing() {
    let tools = tempfile::tempdir().unwrap();
    let t = tools.path();
    let (first, second) = (t.join("first"), t.join("second"));
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let enoent = "echo 'node:internal/child_process:285'\necho 'Error: spawn /opt/homebrew/lib/vendor/aarch64-apple-darwin/codex/codex ENOENT' >&2\necho '    at ChildProcess._handle.onexit' >&2\nexit 1";
    // Codex: the only copy does not start. A working one arrives later on
    // the path, when the pane reinstalls it.
    script(&first, "codex", enoent);
    let good_codex = script(
        t,
        "codex.src",
        "[ \"$1\" = --version ] && { echo codex-cli 9.9.9; exit 0; }\necho CODEX-STARTED\nsleep 60",
    );
    // OpenCode: the first copy on the path is broken, a later one works.
    script(&first, "opencode", enoent);
    script(&second, "opencode", "echo opencode v2.3.0");
    // Antigravity: there, and slow to say its version.
    script(&first, "agy", "sleep 8");
    // A stand-in npm that only writes down what it was asked to do.
    let npm_log = t.join("npm.log");
    script(
        &first,
        "npm",
        &format!("echo \"$@\" >> {}", npm_log.display()),
    );
    let path = format!("{}:{}:/usr/bin:/bin", first.display(), second.display());
    let s = Station::start_with(
        "alpha",
        &format!(
            "[agents.claude]\nprogram = \"{missing}\"\n[agents.codex]\ninstall_cmd = [\"cp\", \"{good_codex}\", \"{to}\"]\n",
            missing = t.join("missing").display(),
            to = second.join("codex").display(),
        ),
        &[("PATH", &path)],
    );
    let row = |agent: &str| {
        s.call("agents.status", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["agent"] == json!(agent))
            .unwrap_or_else(|| panic!("no row for {agent}"))
            .clone()
    };

    // The four cases, each called what it is.
    let codex = row("codex");
    assert_eq!(
        (&codex["health"], &codex["installed"], &codex["version"]),
        (&json!("broken"), &json!(true), &Value::Null),
        "{codex}"
    );
    let problem = codex["problem"].as_str().unwrap();
    assert!(
        problem.starts_with("Error: spawn") && problem.ends_with("ENOENT"),
        "the program's own error line: {problem}"
    );
    let opencode = row("opencode");
    assert_eq!(
        (&opencode["health"], &opencode["version"], &opencode["path"]),
        (
            &json!("ok"),
            &json!("2.3.0"),
            &json!(second.join("opencode"))
        ),
        "the copy that answers is used, not the first that exists: {opencode}"
    );
    let agy = row("antigravity");
    assert_eq!(
        (&agy["health"], &agy["installed"], &agy["problem"]),
        (&json!("no_answer"), &json!(true), &Value::Null),
        "{agy}"
    );
    assert_eq!(row("claude")["health"], json!("missing"));
    // The slow one was left to finish: it is never killed.
    let alive = std::process::Command::new("pgrep")
        .args(["-f", &format!("{} --version", first.join("agy").display())])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(alive.success(), "the slow program was stopped");

    // `spandeck doctor` says the same.
    let checks = s.call("station.doctor", json!({}))["checks"].clone();
    let detail = |name: &str| {
        let c = checks
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == json!(name))
            .unwrap_or_else(|| panic!("no check {name}"));
        (
            c["ok"] == json!(true),
            c["detail"].as_str().unwrap().to_string(),
        )
    };
    let (ok, said) = detail("Codex installed");
    assert!(
        !ok && said.contains("does not start") && said.contains("ENOENT"),
        "{said}"
    );
    let (ok, said) = detail("Antigravity installed");
    assert!(ok && said.contains("did not answer"), "{said}");
    let (ok, said) = detail("Claude Code installed");
    assert!(!ok && said.starts_with("not installed"), "{said}");

    // The ways to install: the vendor's script unless the owner chose
    // another; one whose tool is here is offered.
    let ways = opencode["methods"].as_array().unwrap();
    let way = |id: &str| ways.iter().find(|m| m["id"] == json!(id)).unwrap();
    assert_eq!(opencode["method"], json!("script"));
    assert_eq!(way("npm")["available"], json!(true));
    // Homebrew is looked for in its usual folders as well as on the path, so
    // a machine that really has it (the Mac CI runner does) says so.
    let brew_here = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
        .iter()
        .any(|p| std::path::Path::new(p).exists());
    assert_eq!(way("brew")["available"], json!(brew_here));

    // A pane for the broken one says what was found and what it said, shows
    // the command, runs it, and starts the copy that answers.
    let home = s.root.join("home");
    let pane = s.call("pane.spawn", json!({"agent": "codex", "cwd": home}))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let screen = s.wait_text(&pane, "CODEX-STARTED");
    let flat = screen.replace('\n', "");
    assert!(
        screen.contains("Codex is installed on this station but does not start")
            && flat.contains(&format!("found:   {}", first.join("codex").display()))
            && flat.contains("it said: Error: spawn")
            && flat.contains(&format!(
                "cp {good_codex} {}",
                second.join("codex").display()
            )),
        "{screen}"
    );
    let codex = row("codex");
    assert_eq!(
        (&codex["health"], &codex["version"], &codex["path"]),
        (&json!("ok"), &json!("9.9.9"), &json!(second.join("codex"))),
        "{codex}"
    );

    // Reinstalling is the owner's to ask for, never an agent's. The way the
    // owner picks is run and remembered for this station.
    let err = s
        .call_as_pane(&pane, "agents.install", json!({"agent": "opencode"}))
        .unwrap_err();
    assert_eq!(err.code, "denied");
    if !brew_here {
        let err = s
            .try_call(
                "agents.install",
                json!({"agent": "opencode", "method": "brew"}),
            )
            .unwrap_err();
        assert!(
            err.message.contains("Homebrew is not on alpha"),
            "{}",
            err.message
        );
    }
    let started = s.call(
        "agents.install",
        json!({"agent": "opencode", "method": "npm"}),
    );
    assert_eq!(started["command"], json!("npm install -g @opencode/cli"));
    let done = wait_for("the install to finish", 20, || {
        s.log("agents.installed").first().cloned()
    });
    assert_eq!(
        (&done["data"]["ok"], &done["data"]["method"]),
        (&json!(true), &json!("npm")),
        "{done}"
    );
    assert_eq!(
        std::fs::read_to_string(&npm_log).unwrap().trim(),
        "install -g @opencode/cli"
    );
    assert_eq!(row("opencode")["method"], json!("npm"));
}

/// Not a test of behaviour: writes screenshots of the Agent programs dialog
/// for a human to look at, with a program in each of the four cases.
/// `SPANDECK_SHOTS=/some/dir cargo test --test agent_setup -- --ignored screenshots`
/// Nothing is installed: the confirmation is shown and cancelled.
#[test]
#[ignore]
fn screenshots_of_agent_programs() {
    use common::browser::Page;
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let dir =
        std::path::PathBuf::from(std::env::var("SPANDECK_SHOTS").expect("set SPANDECK_SHOTS"));
    std::fs::create_dir_all(&dir).unwrap();
    let tools = tempfile::tempdir().unwrap();
    let t = tools.path();
    let (first, second) = (t.join("first"), t.join("second"));
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    // What a program says is shown as text, markup and all.
    let enoent = "echo 'Error: spawn /opt/homebrew/lib/node_modules/@openai/codex/vendor/aarch64-apple-darwin/codex/codex ENOENT <b id=injected>not markup</b>' >&2\nexit 1";
    script(&first, "codex", enoent);
    script(&first, "opencode", enoent);
    script(&second, "opencode", "echo opencode v2.3.0");
    script(&first, "agy", "sleep 8");
    script(&first, "npm", "true");
    let path = format!("{}:{}:/usr/bin:/bin", first.display(), second.display());
    let s = Station::start_with(
        "m4-macbook",
        &format!(
            "[agents.claude]\nprogram = \"{}\"\n",
            t.join("missing").display()
        ),
        &[("PATH", &path)],
    );
    let shoot = |page: &Page, name: &str| {
        std::thread::sleep(std::time::Duration::from_millis(600));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
    };
    let page = Page::open(&s, 1200, 760);
    page.right_click("document.querySelector('.tile.station')");
    page.choose("Agent programs and versions");
    page.wait(
        "the four cases",
        "['broken', 'ok', 'no_answer', 'missing'].every(c => document.querySelector(`.agent-program[data-health=${c}]`))",
    );
    assert_eq!(
        page.js("!document.querySelector('#injected') && document.querySelector('.agent-problem').innerText.includes('<b id=injected>')"),
        json!(true),
        "a program's error line became markup"
    );
    shoot(&page, "agent-programs");
    page.js("[...document.querySelectorAll('.agent-program[data-agent=codex] button')].find(b => b.innerText === 'Reinstall').click()");
    page.wait(
        "the confirmation",
        "[...document.querySelectorAll('.dialog h2')].some(x => x.innerText.startsWith('Reinstall Codex'))",
    );
    shoot(&page, "agent-programs-reinstall");
    page.js("[...document.querySelectorAll('.dialog')].pop().querySelector('button:not(.primary):not(.icon-btn)').click()");
    assert!(s.log("agents.installed").is_empty());
}

/// The owner: "show the version of any agent and have a way to update them
/// ... then restart them automatically and reload the session". A pane in the
/// middle of a turn is not interrupted: it restarts when the turn is over.
/// And updating is the owner's to do, never an agent's.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
fn an_update_shows_the_new_version_and_restarts_each_pane_into_its_conversation() {
    let tools = tempfile::tempdir().unwrap();
    let t = tools.path();
    let version = t.join("version");
    std::fs::write(&version, "fake-agent 1.0.0\n").unwrap();
    let missing = t.join("missing").display().to_string();
    let s = Station::start_with(
        "alpha",
        &format!(
            "[agents.fake]\nversion_cmd = [\"cat\", \"{v}\"]\nupdate_cmd = [\"sh\", \"-c\", \"echo fake-agent 2.0.0 > {v}\"]\n[agents.claude]\nprogram = \"{missing}\"\n[agents.codex]\nprogram = \"{missing}\"\n[agents.opencode]\nprogram = \"{missing}\"\n[agents.antigravity]\nprogram = \"{missing}\"\n",
            v = version.display()
        ),
        &[],
    );
    let row = |agent: &str| {
        s.call("agents.status", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["agent"] == json!(agent))
            .unwrap_or_else(|| panic!("no row for {agent}"))
            .clone()
    };

    let idle = s.fake(json!({"steps": []}), json!({"name": "idle"}));
    let busy = s.fake(
        json!({"steps": [{"prompt": "go"}, {"work": 2500}, {"turn_end": "done"}]}),
        json!({"name": "busy"}),
    );
    s.wait_state(&busy, "working");
    let session = |pane: &str| {
        wait_for("the pane's session", 10, || {
            s.pane(pane)["session_id"].as_str().map(String::from)
        })
    };
    let (idle_session, busy_session) = (session(&idle), session(&busy));

    let fake = row("fake");
    assert_eq!(
        (&fake["installed"], &fake["version"], &fake["panes"]),
        (&json!(true), &json!("1.0.0"), &json!(2)),
        "{fake}"
    );
    let agy = row("antigravity");
    assert_eq!(
        (&agy["installed"], &agy["version"]),
        (&json!(false), &Value::Null)
    );
    // The fleet view gets the same from station.info, with no extra call.
    let brief = s.call("station.info", json!({}))["agents"].clone();
    assert!(
        brief.as_array().unwrap().contains(
            &json!({"agent": "fake", "installed": true, "health": "ok", "version": "1.0.0"})
        ),
        "{brief}"
    );

    // An agent cannot update the station's programs.
    let err = s
        .call_as_pane(&idle, "agents.update", json!({}))
        .unwrap_err();
    assert_eq!(err.code, "denied");

    let started = s.call("agents.update", json!({"agent": "fake"}));
    let job = started["started"][0]["job"].clone();
    assert_eq!(
        started["started"],
        json!([{"agent": "fake", "name": "Fake agent", "from": "1.0.0", "job": job}])
    );
    // The idle pane comes back in its conversation; the busy one is left
    // alone until its turn ends, and then does the same.
    s.wait_text(&idle, &format!("[fake] RESUMED {idle_session}"));
    let busy_screen = s.text(&busy);
    assert!(
        busy_screen.contains("[fake] working") && !busy_screen.contains("RESUMED"),
        "{busy_screen}"
    );
    s.wait_text(&busy, &format!("[fake] RESUMED {busy_session}"));

    let fake = row("fake");
    assert_eq!(fake["version"], json!("2.0.0"));
    let last = &fake["last_update"];
    assert_eq!(
        (&last["ok"], &last["from"], &last["to"]),
        (&json!(true), &json!("1.0.0"), &json!("2.0.0")),
        "{last}"
    );
    let logged = &s.log("agents.updated")[0]["data"];
    assert_eq!(
        (&logged["restarted"], &logged["restart_when_idle"]),
        (&json!([idle]), &json!([busy])),
        "{logged}"
    );
    // The row says the same of its job (17 Nov 2026: every action says what
    // it did): which panes went back to their conversations, and which do
    // when their turn ends.
    let done = &fake["job"];
    assert_eq!(
        (
            &done["id"],
            &done["outcome"],
            &done["restarted"],
            &done["restart_when_idle"]
        ),
        (&job, &json!("updated"), &json!([idle]), &json!([busy])),
        "{done}"
    );
}
