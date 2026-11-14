//! Phase 4 acceptance: projects.
//! Done when: a project created, closed and reopened restores its panes and agents.
mod common;
use common::*;
use serde_json::json;

/// Pane names are unique (the owner's rule, 14 Nov 2026: "if panes are the
/// same name it messes up the layout, we should not allow creating duplicate
/// pane names"): within a project, and among the panes with no project.
#[test]
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
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
#[cfg_attr(windows, ignore = "runs a Unix shell script (/bin/sh)")]
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
    // The layout is a row under its project in the list; selecting it shows
    // it on the right, and the row says it is the one selected.
    page.goto("/s/alpha");
    let row = |name: &str| {
        format!("(t => t ? (t.click(), true) : false)([...document.querySelectorAll('.side-proj .side-layout')].find(t => t.querySelector('.side-name').innerText === '{name}'))")
    };
    let head = |label: &str| {
        format!("[...document.querySelectorAll('.lay-head button')].find(b => b.innerText === '{label}').click()")
    };
    page.wait("the layout's row", &row("build"));
    page.wait(
        "three panes",
        "location.hash === '#/s/alpha/l/proj/build' && document.querySelectorAll('.workspace .pane').length === 3",
    );
    assert_eq!(
        page.js("[document.querySelector('.side-layout.current .side-name').innerText, document.querySelector('.lay-head .lay-name').innerText, document.querySelector('.lay-head .lay-saved').innerText]"),
        json!(["build", "build", "Saved with proj on alpha"])
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
    page.js(&head("Arrange"));
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
    page.wait("the second layout's row", &row("both"));
    page.wait(
        "the guest and its host",
        "document.querySelectorAll('.workspace .pane').length === 2 && !!document.querySelector('.workspace .pane .chip.guest')",
    );
    assert_eq!(
        page.js("[...document.querySelectorAll('.workspace .pane')].map(el => [el.querySelector('.strip-name').innerText, el.querySelector('.chip.guest')?.innerText || ''])"),
        json!([["dev", ""], ["logs", "from other"]]),
        "only the pane of the other project carries the chip"
    );
    page.js(&head("Arrange"));
    let card = page.wait(
        "the guest's card in Arrange",
        "document.querySelector('.arr[data-ref=\"other/logs\"] .arr-card')?.innerText",
    );
    assert!(card.as_str().unwrap().contains("from other"), "{card}");

    // A layout's own menu (the owner's rule: "right-click a layout and select
    // edit or delete"): duplicate it, rename the copy, delete it.
    let names = || {
        let layouts =
            s.call("project.get", json!({"name": "proj"}))["definition"]["layout"].clone();
        let names: Vec<String> = layouts
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["name"].as_str().unwrap().to_string())
            .collect();
        names.join(" ")
    };
    let current = "document.querySelector('.side-proj .side-layout.current')";
    page.right_click(current);
    page.choose("Duplicate");
    page.wait("the copy", "location.hash === '#/s/alpha/l/proj/both-copy' && document.querySelectorAll('.workspace .pane').length === 2");
    page.right_click(current);
    page.choose("Rename");
    page.wait("the new name", "(f => f ? (f.querySelector('input').value = 'pair', f.querySelector('button[type=submit]').click(), true) : false)(document.querySelector('.dialog form'))");
    page.wait("the renamed layout", "location.hash === '#/s/alpha/l/proj/pair' && document.querySelector('.lay-head .lay-name')?.innerText === 'pair'");
    assert_eq!(names(), "build both pair");
    page.right_click(current);
    page.choose("Delete");
    page.wait("the question", "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.dialog button')].find(b => b.innerText === 'Delete'))");
    page.wait("the station again", "location.hash === '#/s/alpha' && document.querySelectorAll('.side-proj .side-layout').length === 2");
    assert_eq!(names(), "build both");
}

/// The list on the left (the owner's rules, 14 Nov 2026): a layout is made
/// from its project's menu, or outside any project as a global one; a layout
/// holds any pane, from any project or none, dragged in from the list; panes
/// and layouts can be pinned; each pane says by an icon what it is; and every
/// station shows the same Pinned and Layouts. A workspace an older deck kept
/// in the browser becomes a global layout.
#[test]
fn the_list_makes_layouts_in_projects_and_global_ones_takes_any_pane_and_pins() {
    use common::browser::Page;
    let (a, b) = Station::pair();
    let shell = |n: &str| json!({"name": n, "agent": "shell"});
    a.call(
        "project.new",
        json!({"path": "~/api", "panes": [shell("dev")], "open": true}),
    );
    a.call(
        "project.new",
        json!({"path": "~/web", "panes": [shell("logs")], "open": true}),
    );
    let helper = a.fake(
        json!({"steps": []}),
        json!({"name": "helper", "project": "api"}),
    );
    let scratch = a.call("pane.spawn", json!({"agent": "shell", "name": "scratch"}))["id"].clone();
    let dev = a.pane("dev")["id"].clone();
    let global = |s: &Station, name: &str| {
        s.call("deck.get", json!({}))["layouts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["name"] == json!(name))
            .cloned()
    };

    let page = Page::open(&a, 1400, 860);
    page.goto("/s/alpha");
    page.wait("the list", "!!document.querySelector('.side-pane')");

    // A workspace from the browser: its panes that still exist, named the new way.
    page.js(&format!(
        "(localStorage.setItem('spandeck.layout.alpha', JSON.stringify({{active: 0, tabs: [{{name: 'my work!', tree: {{dir: 'row', ratio: 0.5, a: {{pane: {dev}}}, b: {{dir: 'col', ratio: 0.5, a: {{pane: {scratch}}}, b: {{pane: 'p-gone'}}}}}}}}]}})), true)"
    ));
    let moved = wait_for("the workspace to become a global layout", 15, || {
        global(&a, "my-work")
    });
    assert_eq!(
        moved["tree"],
        json!({"dir": "row", "ratio": 500, "a": {"pane": "dev", "project": "api", "station": "alpha"}, "b": {"id": scratch, "station": "alpha"}})
    );
    page.wait(
        "the browser's own copy to be deleted",
        "localStorage.getItem('spandeck.layout.alpha') === null",
    );

    // What each pane is: an icon, and its name where a screen reader finds it.
    let pane_row = |id: &serde_json::Value| {
        format!("document.querySelector('.side-stations .side-pane[data-id={id}]')")
    };
    assert_eq!(
        page.js(&format!("[{}.getAttribute('aria-label'), {}.querySelector('.ico-wrap').title, {}.querySelector('svg.ico').getAttribute('aria-hidden'), {}.getAttribute('aria-label').split(', ').slice(0, 2)]",
            pane_row(&dev), pane_row(&dev), pane_row(&dev), pane_row(&json!(helper)))),
        json!(["dev, Shell, idle", "Shell", "true", ["helper", "Agent"]])
    );

    // A project's menu makes a layout in that project.
    let submit = |name: &str| {
        page.wait("the name", &format!("(f => f ? (f.querySelector('input').value = '{name}', f.querySelector('button[type=submit]').click(), true) : false)(document.querySelector('.dialog form'))"));
    };
    page.right_click("[...document.querySelectorAll('.side-project')].find(x => x.querySelector('.side-name').innerText === 'api')");
    page.choose("New layout");
    submit("build");
    page.wait(
        "the project's layout on the right",
        "location.hash === '#/s/alpha/l/api/build' && document.querySelectorAll('.workspace .pane').length === 2 && document.querySelector('.lay-head .lay-saved').innerText === 'Saved with api on alpha'",
    );
    assert_eq!(
        page.js("document.querySelector('.side-layout.current').closest('.side-proj').querySelector('.side-project .side-name').innerText"),
        json!("api"),
        "the layout is listed in its project"
    );

    // Outside any project, the menu makes a global one.
    page.right_click("document.querySelector('.sidebar')");
    page.choose("New global layout");
    submit("all");
    page.wait(
        "the empty global layout on the right",
        "location.hash === '#/l/all' && !!document.querySelector('.workspace .arr[data-empty]') && document.querySelector('.lay-head .lay-saved').innerText.startsWith('Global layout')",
    );

    // Drag a pane of a project into it, then (out of Arrange) one with no project.
    let drag = |id: &serde_json::Value, onto: &str, across: f64| {
        page.js(&format!("(() => {{ const row = {}, from = row.getBoundingClientRect(), to = document.querySelector('{onto}').getBoundingClientRect();
            const ev = (t, x, y, el) => (el || window).dispatchEvent(new MouseEvent(t, {{bubbles: true, cancelable: true, clientX: x, clientY: y}}));
            const x = to.left + to.width * {across}, y = to.top + to.height / 2;
            ev('mousedown', from.left + 30, from.top + 8, row); ev('mousemove', x, y); ev('mouseup', x, y); return true; }})()", pane_row(id)));
    };
    drag(&a.pane("logs")["id"], ".workspace .arr[data-empty]", 0.5);
    page.wait(
        "the pane from web",
        "document.querySelector('.workspace .arr[data-ref=\"alpha:web/logs\"] .chip.guest')?.innerText === 'from web'",
    );
    page.js("[...document.querySelectorAll('.lay-head button')].find(b => b.innerText === 'Done arranging').click()");
    page.wait(
        "the layout out of Arrange",
        "document.querySelectorAll('.workspace .pane').length === 1 && !document.querySelector('.arr')",
    );
    drag(&scratch, ".workspace .pane", 0.9);
    wait_for("both panes to be saved for the fleet", 10, || {
        global(&a, "all").filter(|l| {
            l["tree"] == json!({"dir": "row", "ratio": 500, "a": {"pane": "logs", "project": "web", "station": "alpha"}, "b": {"id": scratch, "station": "alpha"}})
        })
    });
    page.wait(
        "both panes on the right",
        "JSON.stringify([...document.querySelectorAll('.workspace .pane .strip-name')].map(x => x.innerText)) === '[\"logs\",\"scratch\"]' && !document.querySelector('.arr')",
    );

    // Pin a pane and a layout: both are listed under Pinned.
    page.right_click(&pane_row(&dev));
    page.choose("Pin");
    page.right_click("[...document.querySelectorAll('.side-proj .side-layout')].find(x => x.querySelector('.side-name').innerText === 'build')");
    page.choose("Pin");
    let pinned = "JSON.stringify([...document.querySelectorAll('.side-pinned .side-row')].map(x => x.getAttribute('aria-label')))";
    page.wait(
        "the pinned rows",
        &format!("{pinned} === '[\"dev, Shell, idle\",\"build, layout\"]'"),
    );
    assert_eq!(
        a.call("deck.get", json!({}))["pins"],
        json!([{"kind": "pane", "station": "alpha", "id": dev}, {"kind": "layout", "station": "alpha", "project": "api", "name": "build"}])
    );

    // The other station's deck: the same Pinned and the same Layouts, and a
    // global layout shows the panes where they run.
    page.visit(&b);
    page.wait(
        "the same list on bravo",
        &format!("{pinned} === '[\"dev, Shell, idle\",\"build, layout\"]' && JSON.stringify([...document.querySelectorAll('.side-globals .side-layout .side-name')].map(x => x.innerText)) === '[\"my-work\",\"all\"]'"),
    );
    page.js("[...document.querySelectorAll('.side-globals .side-layout')].find(x => x.querySelector('.side-name').innerText === 'all').click()");
    page.wait(
        "alpha's panes in the layout on bravo",
        "JSON.stringify([...document.querySelectorAll('.workspace .pane')].map(el => [el.querySelector('.strip-name').innerText, el.querySelector('.chip.guest')?.innerText, !!el.querySelector('.xterm-rows')])) === '[[\"logs\",\"from web on alpha\",true],[\"scratch\",\"on alpha\",true]]'",
    );
}
