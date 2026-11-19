//! Tools for agents: added once, installed by each station it is chosen
//! for, given to an agent only where ready, pinned to a version and
//! upgraded one station first or across the fleet.
//!
//! The reason for every test here is the owner's request of 18 Nov 2026
//! and the acceptance criteria of its build (DECISIONS.md, "Tools for
//! agents").
//!
//! Nothing is downloaded and no real tool is installed: each station's
//! own config names a stand-in installer and a stand-in for the step that
//! starts the tool once (`[tools.playwright]`), and the registry is a
//! server on this machine.
#![cfg(unix)]
mod common;
use common::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The stand-in installer of one station. What it does is in files in
/// `<root>/toolctl`: `fail` (it prints this and fails), `sleep` (it takes
/// this long), `lacks` (the tool installs, and starting it once prints
/// this and fails). Every run adds a line to `runs`.
fn stand_in(root: &Path, latest: &str) -> String {
    let ctl = root.join("toolctl");
    std::fs::create_dir_all(&ctl).unwrap();
    let ctl = ctl.display();
    std::fs::write(
        root.join("tool-install.sh"),
        format!(
            r#"d={ctl}
echo run >> $d/runs
echo "fetching $SPANDECK_TOOL_NAME $SPANDECK_TOOL_VERSION"
[ -f $d/sleep ] && sleep "$(cat $d/sleep)"
if [ -f $d/fail ]; then cat $d/fail >&2; exit 1; fi
mkdir -p "$SPANDECK_TOOL_DIR/node_modules/@playwright/mcp"
echo "// $SPANDECK_TOOL_VERSION" > "$SPANDECK_TOOL_DIR/node_modules/@playwright/mcp/cli.js"
echo "added 3 packages"
"#
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("tool-check.sh"),
        format!("d={ctl}\nif [ -f $d/lacks ]; then cat $d/lacks >&2; exit 1; fi\necho started\n"),
    )
    .unwrap();
    format!(
        "[tools.playwright]\ninstall_cmd = [\"/bin/sh\", \"{0}/tool-install.sh\"]\ncheck_cmd = [\"/bin/sh\", \"{0}/tool-check.sh\"]\nruntime = \"/opt/stand-in/node\"\nlatest_url = \"{latest}\"\n",
        root.display()
    )
}

fn ctl(s: &Station, what: &str, value: Option<&str>) {
    let file = s.root.join("toolctl").join(what);
    match value {
        Some(v) => std::fs::write(file, v).unwrap(),
        None => {
            let _ = std::fs::remove_file(file);
        }
    }
}

fn runs(s: &Station) -> usize {
    std::fs::read_to_string(s.root.join("toolctl/runs"))
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

/// The registry, on this machine: it answers every request with what the
/// test put there, and counts them.
struct Registry {
    port: u16,
    says: Arc<Mutex<String>>,
    asked: Arc<Mutex<usize>>,
}

impl Registry {
    fn start() -> Registry {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let says: Arc<Mutex<String>> = Default::default();
        let asked: Arc<Mutex<usize>> = Default::default();
        let (s2, a2) = (says.clone(), asked.clone());
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                *a2.lock().unwrap() += 1;
                let b = s2.lock().unwrap().clone();
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}",
                    b.len()
                );
            }
        });
        Registry { port, says, asked }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/latest", self.port)
    }
}

fn row(s: &Station, name: &str) -> Option<Value> {
    s.call("tools.status", json!({}))["tools"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["name"] == json!(name)).cloned())
}

/// Wait until the tool's row on a station is in this state, and return it.
fn wait_row(s: &Station, state: &str, version: Option<&str>) -> Value {
    wait_for_or(
        &format!("playwright {state} {version:?} on {}", s.name),
        20,
        || format!("{:?}", row(s, "playwright")),
        || {
            row(s, "playwright").filter(|r| {
                r["state"] == json!(state)
                    && version.is_none_or(|v| r["wanted"] == json!(v))
                    && (state != "ready" || r["job"]["state"] != json!("running"))
            })
        },
    )
}

/// A stand-in for an agent's program that prints what it was started with.
fn printer(s: &Station) -> String {
    let path = s.root.join("print-args.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\nfor a in \"$@\"; do echo \"ARG<$a>\"; done\necho ALL-ARGS\nsleep 60\n",
    )
    .unwrap();
    spandeck_terminal::sys::set_executable(&path).unwrap();
    path.display().to_string()
}

/// Start a pane of an agent kind with the stand-in, and return its id and
/// what it was started with (its screen, as one line).
fn started_with(s: &Station, agent: &str) -> (String, String) {
    let id = s.call(
        "pane.spawn",
        json!({"agent": agent, "program": printer(s), "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let screen = s.wait_text(&id, "ALL-ARGS").replace('\n', "");
    (id, screen)
}

/// What `spandeck <args>` prints when run for a station.
fn cli(s: &Station, args: &[&str]) -> String {
    let out = s
        .command(SPANDECK)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fleet_of_two(registry: &Registry) -> (Station, Station) {
    let mut stations = Station::fleet(&["alpha", "bravo"], |_, root| {
        (stand_in(root, &registry.url()), vec![])
    });
    let b = stations.pop().unwrap();
    let a = stations.pop().unwrap();
    wait_for("the two stations see each other", 20, || {
        (a.sees("bravo").as_deref() == Some("online")
            && b.sees("alpha").as_deref() == Some("online"))
        .then_some(())
    });
    (a, b)
}

/// Acceptance: a tool chosen for one station is installed and wired there
/// only; widening its scope makes the other install it by itself; a pane
/// started while it is not ready does not get it and one started after
/// does; an upgrade of one station changes only that station, a rollout
/// brings the other, and the earlier version chosen again is honoured, at
/// once; the "without asking" tick writes the rule for that one server;
/// and an agent is refused every one of these calls.
#[test]
fn each_station_installs_for_itself_and_an_agent_gets_a_tool_only_where_it_is_ready() {
    let registry = Registry::start();
    let (a, b) = fleet_of_two(&registry);
    assert!(a.call("station.info", json!({}))["caps"]
        .as_array()
        .unwrap()
        .contains(&json!("tools")));

    // For alpha only, and slow enough to start a pane meanwhile.
    ctl(&a, "sleep", Some("2"));
    let added = a.call(
        "tools.add",
        json!({"catalogue": "playwright", "version": "0.0.84", "stations": ["alpha"]}),
    );
    assert_eq!(added["tool"]["name"], json!("playwright"));
    assert_eq!(added["tool"]["version"], json!("0.0.84"));
    assert_eq!(added["version_from"], json!("caller"));
    let installing = wait_row(&a, "installing", Some("0.0.84"));
    assert_eq!(installing["job"]["action"], json!("install"));
    assert_eq!(installing["wired"], json!(false));
    let (early, screen) = started_with(&a, "claude");
    assert!(!screen.contains("playwright"), "{screen}");
    let created = a.log("pane.created");
    let record = created
        .iter()
        .find(|e| e["pane"] == json!(early))
        .expect("the pane's record");
    assert_eq!(
        record["data"]["tools_without"][0]["tool"],
        json!("playwright")
    );
    assert!(record["data"]["tools_without"][0]["why"]
        .as_str()
        .unwrap()
        .contains("still being installed on alpha"));

    let ready = wait_row(&a, "ready", Some("0.0.84"));
    assert_eq!(ready["version"], json!("0.0.84"));
    assert_eq!(ready["job"]["says"], json!("Installed 0.0.84"));
    assert_eq!(ready["job"]["outcome"], json!("installed"));
    ctl(&a, "sleep", None);
    // A pane started after is given it: spandeck's Node and the entry
    // file by their whole paths, headless, isolated, the bundled browser.
    let (_, screen) = started_with(&a, "claude");
    let tool_dir = a.root.join("state/tools/playwright");
    assert!(screen.contains("\"playwright\":{"), "{screen}");
    assert!(
        screen.contains(&format!(
            "{}/0.0.84/node_modules/@playwright/mcp/cli.js\",\"--headless\",\"--isolated\",\"--browser\",\"chromium\"",
            tool_dir.display()
        )),
        "{screen}"
    );
    assert!(
        screen.contains("\"command\":\"/opt/stand-in/node\""),
        "{screen}"
    );
    assert!(
        screen.contains(&format!(
            "\"PLAYWRIGHT_BROWSERS_PATH\":\"{}/data\"",
            tool_dir.display()
        )),
        "{screen}"
    );
    // Not ticked: no rule lets an agent use it without asking.
    assert!(!screen.contains("mcp__playwright"), "{screen}");

    // Bravo holds the same document, is not chosen, and did nothing.
    let not_chosen = wait_row(&b, "not_chosen", None);
    assert_eq!(not_chosen["wired"], json!(false));
    let (_, screen) = started_with(&b, "claude");
    assert!(!screen.contains("playwright"), "{screen}");
    assert!(b.log("pane.created")[0]["data"]["tools_without"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(runs(&b), 0);

    // Widened to the fleet: bravo installs it, by itself.
    let scoped = a.call("tools.scope", json!({"name": "playwright", "fleet": true}));
    assert_eq!(scoped["tool"]["fleet"], json!(true));
    let ready = wait_row(&b, "ready", Some("0.0.84"));
    assert_eq!(ready["job"]["says"], json!("Installed 0.0.84"));
    assert!(
        ready["job"]["by"].as_str().unwrap().contains("alpha"),
        "{ready}"
    );
    assert_eq!((runs(&a), runs(&b)), (1, 1));
    let (_, screen) = started_with(&b, "codex");
    assert!(
        screen.contains("mcp_servers.playwright.command=\"/opt/stand-in/node\"")
            && screen.contains("0.0.84/node_modules/@playwright/mcp/cli.js"),
        "{screen}"
    );
    assert!(!screen.contains("mcp_servers.playwright.default_tools_approval_mode"));

    // Upgrade bravo first: alpha is left as it is.
    let (b_pane, _) = started_with(&b, "claude");
    let up = a.call(
        "tools.upgrade",
        json!({"name": "playwright", "station": "bravo", "to": "0.0.85"}),
    );
    assert_eq!(
        up["tool"]["station_versions"],
        json!({"bravo": "0.0.85"}),
        "{up}"
    );
    let ready = wait_row(&b, "ready", Some("0.0.85"));
    assert_eq!(ready["job"]["says"], json!("Upgraded 0.0.84 → 0.0.85"));
    assert_eq!(ready["own_pin"], json!(true));
    // As after an agent's update: its idle pane was started again on it.
    assert!(
        ready["job"]["restarted"]
            .as_array()
            .unwrap()
            .contains(&json!(b_pane)),
        "{ready}"
    );
    wait_for("bravo's pane runs the new version", 20, || {
        b.text(&b_pane)
            .replace('\n', "")
            .contains("0.0.85/node_modules/@playwright/mcp/cli.js")
            .then_some(())
    });
    let alpha = wait_row(&a, "ready", Some("0.0.84"));
    assert_eq!(alpha["version"], json!("0.0.84"));
    assert_eq!(runs(&a), 1);

    // Roll out: the version bravo runs becomes the fleet's.
    let out = a.call("tools.rollout", json!({"name": "playwright"}));
    assert_eq!(out["to"], json!("0.0.85"));
    assert_eq!(out["tool"]["version"], json!("0.0.85"));
    assert_eq!(out["tool"]["station_versions"], json!({}));
    let alpha = wait_row(&a, "ready", Some("0.0.85"));
    assert_eq!(alpha["job"]["says"], json!("Upgraded 0.0.84 → 0.0.85"));
    assert_eq!(
        alpha["on_disk"],
        json!(["0.0.84", "0.0.85"]),
        "the previous version is kept"
    );
    assert_eq!((runs(&a), runs(&b)), (2, 2));
    wait_row(&b, "ready", Some("0.0.85"));

    // The same as text, from the command line: tool by station.
    let grid = cli(&a, &["tools", "--no-check"]);
    assert!(
        grid.contains("playwright  Playwright 0.0.85  for the whole fleet"),
        "{grid}"
    );
    for station in ["alpha", "bravo"] {
        assert!(
            grid.lines().any(|l| l.trim_start().starts_with(station)
                && l.contains("ready 0.0.85")
                && l.contains("last: Upgraded 0.0.84 → 0.0.85")),
            "{grid}"
        );
    }

    // The earlier version again, on alpha: it is still on the disk, so
    // nothing is installed.
    let said = cli(
        &a,
        &[
            "tools",
            "upgrade",
            "playwright",
            "--to",
            "0.0.84",
            "--no-restart",
        ],
    );
    assert!(said.contains("alpha is to run playwright 0.0.84"), "{said}");
    let back = wait_for("alpha is back on 0.0.84", 20, || {
        row(&a, "playwright").filter(|r| r["job"]["outcome"] == json!("rolled_back"))
    });
    assert_eq!(back["job"]["says"], json!("Rolled back 0.0.85 → 0.0.84"));
    assert_eq!(
        (&back["state"], &back["version"]),
        (&json!("ready"), &json!("0.0.84"))
    );
    assert!(back["job"]["restarted"].as_array().unwrap().is_empty());
    assert_eq!(runs(&a), 2);

    // The tick: the rule each program has for one server, for this one.
    let set = a.call(
        "tools.set",
        json!({"name": "playwright", "allow_without_asking": true}),
    );
    assert_eq!(set["tool"]["allow_without_asking"], json!(true));
    let (_, screen) = started_with(&a, "claude");
    assert!(
        screen.contains("ARG<--allowedTools>ARG<mcp__spandeck>ARG<mcp__playwright>"),
        "{screen}"
    );
    assert!(screen.contains("0.0.84/node_modules"), "{screen}");
    // What the tool writes for the agent can be read by it without a
    // question (18 Nov 2026: three real agents stopped to ask before
    // reading a screenshot they had just taken).
    assert!(
        screen.contains("/tools/playwright/data/output/**)>"),
        "{screen}"
    );
    let (_, screen) = started_with(&a, "codex");
    assert!(
        screen.contains("mcp_servers.playwright.default_tools_approval_mode=\"approve\""),
        "{screen}"
    );

    // An agent has none of it: not a change, and not a look.
    let agent = a.fake(json!({"steps": []}), json!({}));
    a.wait_text(&agent, "[fake] session");
    for (method, params) in [
        (
            "tools.add",
            json!({"catalogue": "playwright", "fleet": true}),
        ),
        ("tools.remove", json!({"name": "playwright"})),
        (
            "tools.scope",
            json!({"name": "playwright", "stations": ["alpha"]}),
        ),
        (
            "tools.set",
            json!({"name": "playwright", "allow_without_asking": false}),
        ),
        ("tools.install", json!({"name": "playwright"})),
        (
            "tools.upgrade",
            json!({"name": "playwright", "to": "9.9.9"}),
        ),
        (
            "tools.rollout",
            json!({"name": "playwright", "to": "9.9.9"}),
        ),
        ("tools.check", json!({})),
        ("tools.push", json!({"tools": {}})),
        ("tools.status", json!({})),
        ("tools.list", json!({})),
    ] {
        assert_eq!(
            a.call_as_pane(&agent, method, params).unwrap_err().code,
            "denied",
            "{method}"
        );
    }
    assert_eq!(row(&a, "playwright").unwrap()["wanted"], json!("0.0.84"));

    // Removed: no pane started from now is given it, on either station.
    a.call("tools.remove", json!({"name": "playwright"}));
    wait_for("bravo has no such tool any more", 20, || {
        row(&b, "playwright").is_none().then_some(())
    });
    for s in [&a, &b] {
        let (_, screen) = started_with(s, "claude");
        assert!(!screen.contains("playwright"), "{screen}");
    }
}

/// Acceptance: a failed install says the program's own line and is not
/// tried again by itself; the owner's retry starts it, and a second
/// request while it runs starts nothing; a tool that installs and does
/// not start says what the station lacks, and is not given to an agent;
/// "is there a newer one" is asked when the owner asks and kept; an MCP
/// server of the agent setup behaves as before and is listed as a custom
/// tool for the whole fleet; a job cut short by a restart is run again.
#[test]
fn a_failed_install_says_why_and_the_owner_retries_it() {
    let registry = Registry::start();
    *registry.says.lock().unwrap() = r#"{"name":"@playwright/mcp","version":"0.0.85"}"#.into();
    let a = Station::fleet(&["alpha"], |_, root| {
        (stand_in(root, &registry.url()), vec![])
    })
    .pop()
    .unwrap();

    ctl(
        &a,
        "fail",
        Some("npm warn deprecated something\nnpm error 404 Not Found - GET https://registry.npmjs.org/@playwright%2fmcp\n"),
    );
    // No version named: the newest the registry publishes is pinned.
    let added = a.call(
        "tools.add",
        json!({"catalogue": "playwright", "fleet": true}),
    );
    assert_eq!(added["tool"]["version"], json!("0.0.85"));
    assert_eq!(added["version_from"], json!("registry"));
    let failed = wait_row(&a, "failed", Some("0.0.85"));
    assert!(
        failed["line"]
            .as_str()
            .unwrap()
            .starts_with("npm error 404 Not Found"),
        "{failed}"
    );
    assert_eq!(
        failed["job"]["says"],
        json!("Could not install: npm error 404 Not Found - GET https://registry.npmjs.org/@playwright%2fmcp")
    );
    // What it printed is behind the line.
    let output = failed["job"]["output"].as_str().unwrap();
    assert!(
        output.contains("npm warn deprecated something") && output.contains("tool-install.sh"),
        "{output}"
    );
    assert_eq!(failed["wired"], json!(false));
    let (pane, screen) = started_with(&a, "claude");
    assert!(!screen.contains("playwright"), "{screen}");
    let created = a.log("pane.created");
    let record = created.iter().find(|e| e["pane"] == json!(pane)).unwrap();
    assert!(record["data"]["tools_without"][0]["why"]
        .as_str()
        .unwrap()
        .contains("could not be installed on alpha: npm error 404"));
    // The owner was told, once.
    assert!(a
        .call("notify.list", json!({}))
        .to_string()
        .contains("The tool playwright is not ready on alpha"));
    // Not tried again by itself.
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(runs(&a), 1);

    // The owner's retry; a second one while it runs starts nothing.
    ctl(&a, "fail", None);
    ctl(&a, "sleep", Some("2"));
    let first = a.call("tools.install", json!({"name": "playwright"}));
    assert_eq!(first["started"], json!(true), "{first}");
    let second = a.call("tools.install", json!({"name": "playwright"}));
    assert_eq!(second["started"], json!(false), "{second}");
    assert_eq!(second["running"]["id"], first["job"]);
    let ready = wait_row(&a, "ready", Some("0.0.85"));
    assert_eq!(ready["job"]["says"], json!("Installed 0.0.85"));
    assert_eq!(ready["job"]["id"], first["job"]);
    assert_eq!(runs(&a), 2);
    ctl(&a, "sleep", None);
    let third = a.call("tools.install", json!({"name": "playwright"}));
    assert_eq!(
        (&third["started"], &third["ready"]),
        (&json!(false), &json!("0.0.85"))
    );

    // Is there a newer one? Asked when the owner asks, and kept.
    *registry.says.lock().unwrap() = r#"{"name":"@playwright/mcp","version":"0.0.86"}"#.into();
    let asked_before = *registry.asked.lock().unwrap();
    assert_eq!(asked_before, 1, "asked once, when the tool was added");
    assert_eq!(a.call("tools.check", json!({}))["asking"], json!([]));
    assert_eq!(
        a.call("tools.check", json!({"again": true}))["asking"],
        json!(["playwright"])
    );
    let newer = wait_for("the registry's answer", 20, || {
        row(&a, "playwright").filter(|r| r["latest"]["state"] == json!("available"))
    });
    assert_eq!(newer["latest"]["version"], json!("0.0.86"));
    assert_eq!(*registry.asked.lock().unwrap(), 2);

    // The newer one installs and does not start here: what it said is
    // what the station lacks, and agents keep the version that works.
    ctl(
        &a,
        "lacks",
        Some("chrome-headless-shell: error while loading shared libraries: libnspr4.so: cannot open shared object file\n"),
    );
    let up = a.call("tools.upgrade", json!({"name": "playwright"}));
    assert_eq!(up["to"], json!("0.0.86"));
    let needs = wait_row(&a, "needs", Some("0.0.86"));
    assert!(
        needs["line"].as_str().unwrap().contains("libnspr4.so"),
        "{needs}"
    );
    assert!(needs["job"]["says"].as_str().unwrap().starts_with(
        "Needs something this station lacks: chrome-headless-shell: error while loading"
    ));
    assert_eq!(needs["wired_version"], json!("0.0.85"));
    assert_eq!(needs["version"], Value::Null);
    let (_, screen) = started_with(&a, "claude");
    assert!(screen.contains("0.0.85/node_modules"), "{screen}");

    // An MCP server of the agent setup: as before, and listed.
    a.call(
        "setup.set",
        json!({"fleet": false, "setup": {"mcp_servers": {"docs": {"command": "no-such-program-here", "args": ["-y"]}}}}),
    );
    let list = a.call("tools.list", json!({}));
    let docs = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == json!("docs"))
        .cloned()
        .unwrap();
    assert_eq!(
        (&docs["kind"], &docs["fleet"], &docs["from_setup"]),
        (&json!("custom"), &json!(true), &json!(true))
    );
    let docs_row = row(&a, "docs").unwrap();
    assert_eq!(
        (&docs_row["state"], &docs_row["wired"]),
        (&json!("needs"), &json!(true)),
        "reported as not found, and given all the same, as before"
    );
    let (_, screen) = started_with(&a, "claude");
    assert!(
        screen.contains("\"docs\":{") && screen.contains("no-such-program-here"),
        "{screen}"
    );
    // A tool may not take its name.
    assert!(a
        .try_call(
            "tools.add",
            json!({"name": "docs", "command": "x", "fleet": true})
        )
        .unwrap_err()
        .message
        .contains("already an MCP server called docs"));
    // And what a caller may name is a catalogue entry and a version.
    for (params, words) in [
        (
            json!({"catalogue": "nmap", "fleet": true}),
            "catalogue has no tool",
        ),
        (
            json!({"name": "p2", "catalogue": "playwright", "version": "1; rm -rf", "fleet": true}),
            "is not a version",
        ),
        (
            json!({"name": "p2", "catalogue": "playwright"}),
            "say where",
        ),
        (
            json!({"name": "p2", "catalogue": "playwright", "stations": ["nowhere"]}),
            "no station called nowhere",
        ),
    ] {
        let err = a.try_call("tools.add", params).unwrap_err().message;
        assert!(err.contains(words), "{err}");
    }

    // Cut short by a restart: it is run again, and is not left "running".
    ctl(&a, "lacks", None);
    ctl(&a, "sleep", Some("3"));
    let mut a = a;
    let retry = a.call("tools.install", json!({"name": "playwright"}));
    assert_eq!(retry["started"], json!(true), "{retry}");
    wait_for("the installer is running", 20, || {
        (runs(&a) == 4).then_some(())
    });
    a.kill9();
    ctl(&a, "sleep", None);
    a.launch();
    let ready = wait_row(&a, "ready", Some("0.0.86"));
    assert_ne!(ready["job"]["id"], retry["job"]);
    assert_eq!(ready["job"]["says"], json!("Upgraded 0.0.85 → 0.0.86"));
    assert_eq!(runs(&a), 5);
}

fn folder_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => folder_bytes(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// The install as a station really does it, with no stand-in: spandeck's
/// own Node from nodejs.org (checked against the SHA-256 in the code),
/// the package with that Node's npm, its browser, and the browser started
/// once; then the server an agent would be given is started as written
/// and asked to open a page. Run by hand (`-- --ignored --nocapture`): it
/// downloads a few hundred megabytes. What it found on a machine is in
/// DECISIONS.md ("Tools for agents").
#[test]
#[ignore = "needs the internet: installs the real Playwright tool, a few hundred megabytes"]
fn the_real_playwright_tool_installs_and_its_server_opens_a_page() {
    let a = Station::start("alpha");
    let version = std::env::var("SPANDECK_TEST_PLAYWRIGHT").unwrap_or_else(|_| "0.0.85".into());
    let start = std::time::Instant::now();
    a.call(
        "tools.add",
        json!({"catalogue": "playwright", "version": version, "stations": ["alpha"]}),
    );
    let mut said = String::new();
    let done = loop {
        let r = row(&a, "playwright").unwrap();
        let now = format!(
            "{} | {} | {}",
            r["job"]["step"].as_str().unwrap_or(""),
            r["job"]["command"].as_str().unwrap_or(""),
            r["job"]["last_line"].as_str().unwrap_or("")
        );
        if now != said {
            println!("[{:>4} s] {now}", start.elapsed().as_secs());
            said = now;
        }
        if r["job"]["state"] == json!("done") {
            break r;
        }
        assert!(start.elapsed().as_secs() < 1000, "{r}");
        std::thread::sleep(std::time::Duration::from_millis(500));
    };
    println!(
        "ended after {} s: {}",
        start.elapsed().as_secs(),
        done["job"]["says"]
    );
    println!("{}", done["job"]["output"].as_str().unwrap_or(""));
    let tools = a.root.join("state/tools");
    for dir in ["runtime", "playwright", "playwright/data"] {
        println!("{dir}: {} MB", folder_bytes(&tools.join(dir)) >> 20);
    }
    assert_eq!(done["state"], json!("ready"), "{done}");

    // The server, exactly as a Claude Code pane is given it.
    let (_, screen) = started_with(&a, "claude");
    let from = screen.find("ARG<{\"mcpServers\"").unwrap() + 4;
    let to = from + screen[from..].find(">ARG<").unwrap();
    let config: Value = serde_json::from_str(&screen[from..to]).unwrap();
    let server = &config["mcpServers"]["playwright"];
    println!("{server}");
    let mut child = std::process::Command::new(server["command"].as_str().unwrap())
        .args(
            server["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap()),
        )
        .envs(
            server["env"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k, v.as_str().unwrap())),
        )
        // It writes what it reads of a page under the folder it runs in.
        .current_dir(a.root.join("home"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            let _ = tx.send(line);
        }
    });
    let mut ask = |v: Value| writeln!(stdin, "{v}").unwrap();
    let answer = |id: u64| loop {
        let line = rx
            .recv_timeout(std::time::Duration::from_secs(90))
            .expect("the server answers");
        let v: Value = serde_json::from_str(&line).unwrap_or_default();
        if v["id"] == json!(id) {
            break v;
        }
    };
    ask(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "spandeck-test", "version": "0"}}}),
    );
    let hello = answer(1);
    println!("server: {}", hello["result"]["serverInfo"]);
    ask(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    ask(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "browser_navigate", "arguments": {"url": "data:text/html,<title>spandeck was here</title><h1>hello</h1>"}}}),
    );
    let opened = answer(2);
    let _ = child.kill();
    let _ = child.wait();
    let said = opened["result"].to_string();
    println!("{}", said.chars().take(600).collect::<String>());
    assert!(opened["result"]["isError"] != json!(true), "{opened}");
    assert!(said.contains("spandeck was here"), "{opened}");
}
