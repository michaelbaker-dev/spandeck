//! Phase 4 acceptance: projects.
//! Done when: a project created, closed and reopened restores its panes and agents.
mod common;
use common::*;
use serde_json::json;

/// Pane names are unique (the owner's rule, 14 Nov 2026: "if panes are the
/// same name it messes up the layout, we should not allow creating duplicate
/// pane names"): within a project, and among the panes with no project.
#[test]
fn a_pane_name_is_used_once_in_a_project_and_once_outside_any() {
    let s = Station::start("alpha");
    let repo = s.git_repo("proj");
    s.call(
        "project.new",
        json!({"path": repo, "name": "proj", "panes": [{"name": "build", "agent": "shell"}], "open": true}),
    );
    let spawn = |extra: serde_json::Value| {
        let mut params = json!({"program": "/bin/sh", "cwd": s.root.join("home")});
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        s.try_call("pane.spawn", params)
    };

    // A name somebody chose: refused when taken, with the name and the place.
    let first = spawn(json!({"name": "logs"})).unwrap();
    let refused = spawn(json!({"name": "logs"})).unwrap_err();
    assert_eq!(refused.code, "invalid");
    assert!(
        refused.message.contains("\"logs\"")
            && refused.message.contains("no project")
            && refused.message.contains("choose another name"),
        "{}",
        refused.message
    );
    let refused = spawn(json!({"name": "build", "project": "proj"})).unwrap_err();
    assert!(
        refused.message.contains("in project proj"),
        "{}",
        refused.message
    );
    // The same name in another place is another pane.
    let guest = spawn(json!({"name": "logs", "project": "proj"})).unwrap();
    assert_eq!(guest["name"], json!("logs"));

    // A name nobody chose is numbered.
    let names: Vec<String> = (0..3)
        .map(|_| {
            spawn(json!({})).unwrap()["name"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(names, ["shell", "shell-2", "shell-3"]);

    // Renaming follows the same rule, and a pane may keep its own name.
    let rename = |name: &str| s.try_call("pane.rename", json!({"pane": first["id"], "name": name}));
    assert!(rename("shell-2")
        .unwrap_err()
        .message
        .contains("choose another name"));
    assert_eq!(rename("logs").unwrap()["name"], json!("logs"));
    assert_eq!(rename("tail").unwrap()["name"], json!("tail"));

    // A pane restarted in place is still itself, and a project still reopens.
    let again = s.call("pane.restart", json!({"pane": guest["id"]}));
    assert_eq!(
        (&again["id"], &again["name"]),
        (&guest["id"], &json!("logs"))
    );
    s.call("project.close", json!({"name": "proj"}));
    let reopened = s.call("project.open", json!({"name": "proj"}));
    assert_eq!(reopened["started"][0]["slot"], json!("build"));
}

#[test]
fn a_project_created_closed_and_reopened_restores_its_panes_and_agents() {
    let s = Station::start("alpha");
    let repo = s.git_repo("demo");
    let script = s.script_file(&json!({"steps": []}));
    let v = s.call(
        "project.new",
        json!({"path": repo, "name": "demo", "open": true, "panes": [
            {"name": "dev", "agent": "fake", "role": "author", "args": ["--script", script]},
            {"name": "term", "agent": "shell"}
        ]}),
    );
    assert_eq!(v["address"], json!("demo@alpha"));
    assert_eq!(v["started"].as_array().unwrap().len(), 2);
    // The definition lives in the repo, so it travels through git.
    let toml = std::fs::read_to_string(repo.join(".spandeck/project.toml")).unwrap();
    assert!(
        toml.contains("name = \"demo\"") && toml.contains("agent = \"fake\""),
        "{toml}"
    );

    let dev = s.pane("dev");
    let dev_id = dev["id"].as_str().unwrap().to_string();
    assert_eq!(dev["project"], json!("demo"));
    assert_eq!(
        dev["cwd"].as_str().unwrap(),
        repo.canonicalize().unwrap().to_str().unwrap()
    );
    s.wait_text(&dev_id, "[fake] session");
    let session = wait_for("session id", 10, || {
        s.pane(&dev_id)["session_id"].as_str().map(String::from)
    });

    s.call("project.close", json!({"name": "demo"}));
    assert!(s
        .call("pane.list", json!({}))
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        s.call("project.get", json!({"name": "demo"}))["open"],
        json!(false)
    );

    // Reopen: the agent resumes its conversation; the shell starts fresh.
    let v = s.call("project.open", json!({"name": "demo"}));
    let started = v["started"].as_array().unwrap();
    assert_eq!(started.len(), 2);
    let dev2 = s.pane("dev")["id"].as_str().unwrap().to_string();
    s.wait_text(&dev2, &format!("[fake] RESUMED {session}"));
    assert_eq!(s.pane("term")["agent"], json!("shell"));
    // Opening twice starts nothing twice.
    assert!(s.call("project.open", json!({"name": "demo"}))["started"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn a_folder_that_already_has_a_definition_is_registered_as_it_is() {
    let s = Station::start("alpha");
    let repo = s.git_repo("kept");
    std::fs::create_dir_all(repo.join(".spandeck")).unwrap();
    std::fs::write(repo.join(".spandeck/project.toml"), "name = \"kept\"\n\n[[pane]]\nname = \"sh\"\nagent = \"shell\"\n\n[roles.reviewer]\nagent = \"fake\"\nplacement = \"snapshot\"\n").unwrap();
    let v = s.call("project.new", json!({"path": repo}));
    assert_eq!(v["name"], json!("kept"));
    assert_eq!(
        v["definition"]["roles"]["reviewer"]["placement"],
        json!("snapshot")
    );
    // A second project cannot take the same name from another folder.
    let other = s.git_repo("other");
    let err = s
        .try_call("project.new", json!({"path": other, "name": "kept"}))
        .unwrap_err();
    assert!(err.message.contains("already exists"), "{}", err.message);
}

#[test]
fn the_command_line_creates_a_project_in_the_current_folder() {
    let s = Station::start("alpha");
    let repo = s.git_repo("cli-proj");
    let out = std::process::Command::new(SPANDECK)
        .args(["project", "new", "--pane", "sh:shell"])
        .current_dir(&repo)
        .env("SPANDECK_STATE_DIR", s.root.join("state"))
        .env("SPANDECK_CONFIG_DIR", s.root.join("config"))
        .env_remove("SPANDECK_TOKEN")
        .env_remove("SPANDECK_PANE")
        .env_remove("SPANDECK_SOCKET")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("cli-proj@alpha"));
    assert_eq!(s.pane("sh")["project"], json!("cli-proj"));
}

/// A bug that happened: a new project in `~/some/new/folder` was refused,
/// because `~` was taken literally and the folder did not exist yet.
#[test]
fn a_project_can_be_made_in_a_home_folder_that_does_not_exist_yet() {
    let s = Station::start("alpha");
    let v = s.call(
        "project.new",
        json!({"path": "~/code/test/fresh", "panes": [{"name": "sh", "agent": "shell"}], "open": true}),
    );
    assert_eq!(v["name"], json!("fresh"));
    let dir = s.root.join("home/code/test/fresh");
    assert!(dir.join(".spandeck/project.toml").is_file());
    assert!(
        dir.join(".git").exists(),
        "a new folder starts as a git repository"
    );
    // A pane's folder understands ~ too.
    let pane = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "cwd": "~/code/test"}),
    );
    assert!(
        pane["cwd"].as_str().unwrap().ends_with("home/code/test"),
        "{pane}"
    );
    // Two panes cannot share a name, and the message says so.
    let err = s
        .try_call(
            "project.new",
            json!({"path": "~/twins", "panes": [{"name": "dev", "agent": "shell"}, {"name": "dev", "agent": "shell"}]}),
        )
        .unwrap_err();
    assert!(err.message.contains("its own name"), "{}", err.message);
    assert!(
        !s.root.join("home/twins").exists(),
        "nothing is left behind"
    );
}

/// Layouts (agreed 14 Nov 2026): a project's layouts are saved with the
/// project; the line between two panes resizes at any time; in Arrange a
/// pane is dragged onto another to move it.
#[test]
fn a_project_layout_is_saved_with_the_project_resized_and_arranged() {
    use common::browser::Page;
    let s = Station::start("alpha");
    let shell = |n: &str| json!({"name": n, "agent": "shell"});
    s.call(
        "project.new",
        json!({"path": "~/proj", "panes": [shell("dev"), shell("dev2"), shell("lead")], "open": true}),
    );
    let pane = |n: &str| json!({"pane": n});
    let save = |layout: serde_json::Value| {
        s.try_call(
            "project.layout.save",
            json!({"project": "proj", "layout": layout}),
        )
    };
    save(json!({"name": "build", "tree": {"dir": "col", "ratio": 600,
        "a": {"dir": "row", "ratio": 500, "a": pane("dev"), "b": pane("dev2")}, "b": pane("lead")}}))
    .unwrap();
    let file = std::fs::read_to_string(s.root.join("home/proj/.spandeck/project.toml")).unwrap();
    assert!(
        file.contains("[[layout]]") && file.contains("build"),
        "{file}"
    );
    // What is not a layout is refused and changes nothing.
    assert!(save(json!({"name": "bad name", "tree": pane("dev")})).is_err());
    assert!(save(json!({"name": "x", "tree": {"dir": "row", "ratio": 0, "a": pane("dev"), "b": pane("dev2")}})).is_err());
    assert!(save(json!({"name": "x", "tree": {"dir": "diagonal", "ratio": 500, "a": pane("dev"), "b": pane("dev2")}})).is_err());
    let tree = || {
        s.call("project.get", json!({"name": "proj"}))["definition"]["layout"][0]["tree"].clone()
    };

    let page = Page::open(&s, 1400, 860);
    page.goto("/s/alpha");
    page.wait("the layout's tab", "(t => t ? (t.click(), true) : false)([...document.querySelectorAll('.tab.proj')].find(t => t.innerText === 'build'))");
    page.wait(
        "three panes",
        "document.querySelectorAll('.workspace .pane').length === 3",
    );
    // Where each pane is on screen, by its name.
    let ids: std::collections::HashMap<String, String> = s
        .call("pane.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_string(),
                p["name"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let rects = format!(
        "Object.fromEntries([...document.querySelectorAll('.workspace .pane')].map(el => {{ const r = el.getBoundingClientRect(); return [({})[el.dataset.pane], [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)]]; }}))",
        serde_json::to_string(&ids).unwrap()
    );
    let rects = rects.as_str();
    let at = |name: &str, i: usize| page.js(rects)[name][i].as_i64().unwrap();
    assert!(at("dev", 0) < at("dev2", 0), "dev is left of dev2");
    assert!(at("lead", 1) > at("dev", 1), "lead is below");
    assert!(
        at("lead", 2) > at("dev", 2) * 3 / 2,
        "lead spans the bottom"
    );
    // A bug found here: panes side by side were not full height.
    assert!(
        (at("dev", 3) - at("dev2", 3)).abs() < 3 && at("dev", 3) > 300,
        "{}",
        page.js(rects)
    );
    assert!(
        at("lead", 1) - (at("dev", 1) + at("dev", 3)) < 12,
        "the top row reaches down to the bottom row: {}",
        page.js(rects)
    );

    // Drag the line between dev and dev2: no Arrange needed, and it is saved.
    let before = at("dev", 2);
    page.js("(() => { const g = document.querySelector('.workspace .split.row > .gutter'); const r = g.getBoundingClientRect();
        const ev = (t, x, y, el) => (el || window).dispatchEvent(new MouseEvent(t, {bubbles: true, cancelable: true, clientX: x, clientY: y}));
        ev('mousedown', r.left + 2, r.top + 20, g); ev('mousemove', r.left + 160, r.top + 20); ev('mouseup', r.left + 160, r.top + 20); return true; })()");
    wait_for("the new size to be saved with the project", 10, || {
        tree()["a"]["ratio"].as_u64().filter(|r| *r > 560)
    });
    page.wait(
        "the pane to be wider",
        &format!("({rects}).dev[2] > {}", before + 100),
    );

    // Arrange: drag lead onto the right edge of dev.
    page.js("[...document.querySelectorAll('.tabs button')].find(b => b.innerText === 'Arrange').click()");
    page.wait(
        "the panes to be draggable",
        "document.querySelectorAll('.arr[data-ref]').length === 3",
    );
    page.js("(() => { const of = (n) => document.querySelector(`.arr[data-ref=\"${n}\"]`).getBoundingClientRect();
        const a = of('lead'), b = of('dev');
        const ev = (t, x, y, el) => (el || window).dispatchEvent(new MouseEvent(t, {bubbles: true, cancelable: true, clientX: x, clientY: y}));
        ev('mousedown', a.left + a.width / 2, a.top + a.height / 2, document.querySelector('.arr[data-ref=\"lead\"]'));
        ev('mousemove', b.left + b.width * 0.9, b.top + b.height / 2); ev('mouseup', b.left + b.width * 0.9, b.top + b.height / 2); return true; })()");
    let moved = wait_for("the move to be saved with the project", 10, || {
        let t = tree();
        (t["dir"] == json!("row") && t["a"]["b"] == json!({"pane": "lead"})).then_some(t)
    });
    assert_eq!(moved["a"]["a"], json!({"pane": "dev"}), "{moved}");
    assert_eq!(moved["b"], json!({"pane": "dev2"}), "{moved}");
    let shown = wait_for_or(
        "lead beside dev on screen",
        10,
        || format!("{} tree {}", page.js(rects), tree()),
        || {
            let r = page.js(rects);
            let v = |n: &str, i: usize| r[n][i].as_i64().unwrap_or(-1);
            ((v("lead", 1) - v("dev", 1)).abs() < 4
                && v("lead", 0) > v("dev", 0)
                && v("lead", 0) < v("dev2", 0))
            .then_some(r)
        },
    );
    assert!(shown.is_object());

    // A pane of another project is a guest in a layout, and says whose it is
    // (agreed 14 Nov 2026).
    s.call(
        "project.new",
        json!({"path": "~/other", "panes": [shell("logs")], "open": true}),
    );
    save(json!({"name": "both", "tree": {"dir": "row", "ratio": 500,
        "a": pane("dev"), "b": {"pane": "logs", "project": "other"}}}))
    .unwrap();
    page.wait("the second layout's tab", "(t => t ? (t.click(), true) : false)([...document.querySelectorAll('.tab.proj')].find(t => t.innerText.endsWith('both')))");
    page.wait(
        "the guest and its host",
        "document.querySelectorAll('.workspace .pane').length === 2 && !!document.querySelector('.workspace .pane .chip.guest')",
    );
    assert_eq!(
        page.js("[...document.querySelectorAll('.workspace .pane')].map(el => [el.querySelector('.strip-name').innerText, el.querySelector('.chip.guest')?.innerText || ''])"),
        json!([["dev", ""], ["logs", "from other"]]),
        "only the pane of the other project carries the chip"
    );
    page.js("[...document.querySelectorAll('.tabs button')].find(b => b.innerText === 'Arrange').click()");
    let card = page.wait(
        "the guest's card in Arrange",
        "document.querySelector('.arr[data-ref=\"other/logs\"] .arr-card')?.innerText",
    );
    assert!(card.as_str().unwrap().contains("from other"), "{card}");
}
