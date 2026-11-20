//! A layout holds more than programs, in a real browser (DECISIONS.md, "A
//! layout holds more than programs: Files and Watch").
//!
//! The reason for every test here is the owner's request of 20 Nov 2026:
//! "can a pane be set as the file browser we built? can a pane show the
//! agents working? … its hard to tell the pane type". A layout's leaf can be
//! Files or Watch, which run nothing; a pane is added by its kind; the list
//! on the left says each pane's kind in a word; and the dots have a key. And
//! the rule that came with it: a deck or a station from before views that
//! meets one must not break, and must not lose it.
//!
//! Real stations, real files and the harness's fake agent. What is on disk
//! is read from here after each step.
//!
//! With `SPANDECK_SHOTS` set, `screenshots_of_view_panes` writes pictures
//! there: `SPANDECK_SHOTS=/some/dir cargo test -p spandeck --test viewpanes
//! screenshots_ -- --ignored`
#![cfg(unix)]
mod common;
use common::browser::Page;
use common::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const FILES: &str = "document.querySelector('.view-cell.view-files')";
const WATCH: &str = "document.querySelector('.view-cell.view-watch')";

fn click(page: &Page, what: &str, el: &str) {
    page.wait(
        what,
        &format!("(e => e && !e.disabled ? (e.dispatchEvent(new MouseEvent('click', {{bubbles: true, cancelable: true}})), true) : false)({el})"),
    );
}

/// A button of the dialog on top, by what it says.
fn dialog_button(page: &Page, says: &str) {
    click(
        page,
        says,
        &format!("[...document.querySelectorAll('.dialog button')].filter(b => b.innerText.trim() === {}).pop()", json!(says)),
    );
}

fn no_dialog(page: &Page) {
    page.wait("no dialog", "!document.querySelector('.dialog')");
}

/// "Add pane" on the layout's head, a kind chosen, and Next.
fn add_pane(page: &Page, kind: &str) {
    click(
        page,
        "Add pane",
        "document.querySelector('.lay-head .add-pane')",
    );
    click(
        page,
        kind,
        &format!("document.querySelector('.dialog input[name=pane-kind][value={kind}]')"),
    );
    dialog_button(page, "Next");
}

/// The listing of the Files cell.
fn listing() -> String {
    format!("{FILES}?.querySelector('.fx-view')?.listing")
}

fn row(name: &str) -> String {
    format!(
        "[...({FILES}?.querySelectorAll('.fx-row[data-i]') || [])].find(r => r.querySelector('.fx-name')?.textContent === {})",
        json!(name)
    )
}

fn at(page: &Page, path: &Path) {
    page.wait(
        &format!("the cell at {}", path.display()),
        &format!("{0}?.root === {1} && !{0}.loading", listing(), json!(path)),
    );
}

fn shows(page: &Page, name: &str) {
    page.wait(&format!("the row {name}"), &format!("!!{}", row(name)));
}

/// A key the cell's tree is handed.
fn key(page: &Page, key: &str) {
    page.wait(
        key,
        &format!("(t => t ? (t.focus(), t.dispatchEvent(new KeyboardEvent('keydown', {{key: {}, bubbles: true, cancelable: true}})), true) : false)({FILES}?.querySelector('.fx-tree'))", json!(key)),
    );
}

/// The saved layout of a project, as its station gives it.
fn project_layout(s: &Station, project: &str, name: &str) -> Value {
    let list = s.call("project.list", json!({}));
    let pr = list
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == project)
        .unwrap_or_else(|| panic!("no project {project}: {list}"));
    pr["definition"]["layout"]
        .as_array()
        .and_then(|a| a.iter().find(|l| l["name"] == name))
        .cloned()
        .unwrap_or_else(|| panic!("no layout {name}: {pr}"))
}

fn global_layout(s: &Station, name: &str) -> Value {
    let deck = s.call("deck.get", json!({}));
    deck["layouts"]
        .as_array()
        .and_then(|a| a.iter().find(|l| l["name"] == name))
        .cloned()
        .unwrap_or_else(|| panic!("no global layout {name}: {deck}"))
}

/// A layout's leaves, in the order of its tree.
fn leaves(node: &Value, out: &mut Vec<Value>) {
    if node.get("dir").is_some() {
        leaves(&node["a"], out);
        leaves(&node["b"], out);
    } else if node.is_object() {
        out.push(node.clone());
    }
}

fn leaves_of(layout: &Value) -> Vec<Value> {
    let mut out = vec![];
    leaves(&layout["tree"], &mut out);
    out
}

fn view<'a>(leaves: &'a [Value], kind: &str) -> &'a Value {
    leaves
        .iter()
        .find(|l| {
            l["id"]
                .as_str()
                .is_some_and(|id| id.starts_with(&format!("view.{kind}.")))
        })
        .unwrap_or_else(|| panic!("no {kind} view in {leaves:?}"))
}

/// A project `proj` on `s`, in a folder with a few files, with a shell
/// `dev` and a fake agent `claude` that waits to be told to work. Its
/// folder, and the agent's id.
fn project(s: &Station) -> (PathBuf, String) {
    let dir = s.root.join("home/proj");
    std::fs::create_dir_all(dir.join("out")).unwrap();
    std::fs::write(dir.join("notes.md"), b"# notes\n").unwrap();
    std::fs::write(dir.join("out/scene.txt"), b"scene 4\n").unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    s.call(
        "project.new",
        json!({"path": dir, "panes": [{"name": "dev", "agent": "shell"}], "open": true}),
    );
    let agent = s.fake(
        json!({"steps": [{"wait_file": "go"}, {"prompt": "render scene 4"}, {"say": "Rendering."}, {"wait_file": "ask"},
            {"permission": {"name": "Bash", "command": "rm -rf build"}}, {"turn_end": "Done."}]}),
        json!({"name": "claude", "project": "proj", "cwd": dir}),
    );
    (dir, agent)
}

/// The words at the ends of the rows of the list on the left, by row.
const TAGS: &str = "Object.fromEntries([...document.querySelectorAll('.sidebar .side-pane, .sidebar .side-view')].map(r => [r.querySelector('.side-name').innerText, r.querySelector('.kind-tag')?.innerText.toLowerCase() || null]))";

/// The owner adds a Files pane and a Watch pane to a project's layout, by
/// kind. They are saved in the project's file as leaves of the layout, so
/// they are there after a reload and from another station's deck; the list
/// on the left says each pane's kind in a word, and the dots have a key.
#[test]
#[ignore = "merge tier: a browser"]
fn files_and_watch_are_added_by_kind_and_are_the_same_from_every_station() {
    let (a, b) = Station::pair();
    let (dir, agent) = project(&a);
    a.call(
        "project.layout.save",
        json!({"project": "proj", "layout": {"name": "studio", "tree": {"dir": "row", "ratio": 500, "a": {"pane": "claude"}, "b": {"pane": "dev"}}}}),
    );
    let page = Page::open(&a, 1500, 900);
    page.goto("#/s/alpha/l/proj/studio");
    page.wait(
        "the layout",
        "document.querySelectorAll('.workspace .pane').length === 2",
    );

    // Files: the project's folder on its station, with nothing to fill in.
    add_pane(&page, "files");
    dialog_button(&page, "Add");
    no_dialog(&page);
    at(&page, &dir);
    shows(&page, "notes.md");
    shows(&page, "out");
    // Watch: the layout's own project.
    add_pane(&page, "watch");
    dialog_button(&page, "Add");
    no_dialog(&page);
    page.wait(
        "the agent in Watch",
        &format!("{WATCH}?.querySelectorAll('.watch-agent').length === 1 && {WATCH}.querySelector('.watch-agent-name').innerText === 'claude'"),
    );
    // Each head says its kind, and so does a terminal's strip.
    assert_eq!(
        page.js(&format!("[{FILES}.querySelector('.view-head .kind-tag').innerText.toLowerCase(), {FILES}.querySelector('.view-name').innerText, {WATCH}.querySelector('.view-head .kind-tag').innerText.toLowerCase(), {WATCH}.querySelector('.view-name').innerText, [...document.querySelectorAll('.workspace .pane .strip .kind-tag')].every(t => t.innerText.toLowerCase() === 'term')]")),
        json!(["files", "proj", "watch", "proj", true])
    );

    // Saved with the project, as leaves of the layout: an id no pane has,
    // and nothing else, since both are at their defaults. They are no panes
    // of the station.
    let saved = wait_for("the layout saved with both", 10, || {
        let l = project_layout(&a, "proj", "studio");
        (leaves_of(&l).len() == 4).then_some(l)
    });
    let ls = leaves_of(&saved);
    let (files, watch) = (view(&ls, "files").clone(), view(&ls, "watch").clone());
    assert_eq!(files.as_object().unwrap().len(), 1, "{files}");
    assert_eq!(watch.as_object().unwrap().len(), 1, "{watch}");
    assert!(saved.get("hidden").is_none(), "{saved}");
    let text = std::fs::read_to_string(dir.join(".spandeck/project.toml")).unwrap();
    assert!(
        text.contains(files["id"].as_str().unwrap())
            && text.contains(watch["id"].as_str().unwrap()),
        "{text}"
    );
    let panes = a.call("pane.list", json!({}));
    assert_eq!(
        panes.as_array().unwrap().len(),
        2,
        "a view is no pane: {panes}"
    );

    // The list on the left: a word for each pane's kind, and the layout's
    // views under it. The dots are states, and say nothing of the kind.
    let face = page.js(&format!("document.querySelector('.workspace .pane.chat-pane[data-pane=\"{agent}\"]') ? 'chat' : 'term'"));
    assert_eq!(
        page.js(TAGS),
        json!({"claude": face, "dev": "term", "proj": "watch"}),
        "(the Files view is named for its folder, proj, as the Watch is for its project)"
    );
    assert_eq!(
        page.js("[...document.querySelectorAll('.sidebar .side-view')].map(r => [r.getAttribute('aria-label'), r.querySelector('.kind-tag').getAttribute('aria-label')])"),
        json!([["proj, Files, in the layout studio", "Kind: Files. The file browser: it runs nothing"], ["proj, Watch, in the layout studio", "Kind: Watch. The agents of a project: it runs nothing"]])
    );
    // The tag does not crowd its row: the name keeps room, and nothing
    // runs out of the list.
    assert_eq!(
        page.js("[...document.querySelectorAll('.sidebar .side-pane, .sidebar .side-view')].every(r => r.scrollWidth <= r.clientWidth + 1 && r.querySelector('.side-name').getBoundingClientRect().width > 40 && r.querySelector('.kind-tag').getBoundingClientRect().right <= r.getBoundingClientRect().right)"),
        json!(true)
    );

    // The key to the dots: the eight states, each drawn by the class the
    // deck draws it with.
    click(
        &page,
        "the key",
        "document.querySelector('.sidebar .dot-key-link')",
    );
    assert_eq!(
        page.wait("the key's rows", "(r => r.length ? r : null)([...document.querySelectorAll('.dialog .dot-key-list li')].map(li => [li.querySelector('.dot').className, li.querySelector('b').innerText]))"),
        json!([["dot ", "Grey"], ["dot working", "Blue, pulsing"], ["dot needs_you", "Amber, with a ring"], ["dot done", "Green"], ["dot error", "Red"], ["dot message", "Violet"], ["dot exited", "Hollow"], ["dot unreachable", "Dashed red"]])
    );
    dialog_button(&page, "Done");
    no_dialog(&page);

    // After a reload, and from the other station's deck: the same layout,
    // with both.
    page.reload();
    at(&page, &dir);
    page.wait("Watch after a reload", &format!("{WATCH}?.querySelectorAll('.watch-agent').length === 1 && document.querySelectorAll('.workspace .pane').length === 2"));
    let other = Page::open(&b, 1500, 900);
    other.goto("#/s/alpha/l/proj/studio");
    at(&other, &dir);
    shows(&other, "notes.md");
    other.wait("Watch from the other station", &format!("{WATCH}?.querySelector('.watch-agent-name')?.innerText === 'claude' && document.querySelectorAll('.workspace .pane').length === 2"));
    assert_eq!(
        other.js("[...document.querySelectorAll('.view-head .chip.guest')].map(c => c.innerText)"),
        json!([]),
        "a view of the layout's own station is no guest"
    );

    // A global layout: the owner says the station and the folder, and the
    // station and the project. It is in the deck document, on both stations.
    b.call("pane.spawn", json!({"agent": "shell", "name": "scratch"}));
    a.call("deck.layout.save", json!({"layout": {"name": "wall"}}));
    page.goto("#/l/wall");
    page.wait(
        "the empty layout",
        "document.querySelector('.lay-head .lay-name')?.innerText === 'wall'",
    );
    add_pane(&page, "files");
    page.wait(
        "the folder typed, on alpha",
        &format!("(d => d && d.querySelector('select') ? (d.querySelector('select').value = 'alpha', d.querySelector('input.mono').value = {}, true) : false)(document.querySelector('.dialog'))", json!(dir.join("nowhere"))),
    );
    dialog_button(&page, "Add");
    // A folder that is not there is said, and nothing is added.
    page.wait("why not", "/did not show that folder/.test(document.querySelector('.dialog [role=alert]')?.innerText || '')");
    page.js(&format!(
        "(document.querySelector('.dialog input.mono').value = {}, true)",
        json!(dir.join("out"))
    ));
    dialog_button(&page, "Add");
    no_dialog(&page);
    at(&page, &dir.join("out"));
    add_pane(&page, "watch");
    page.wait("alpha's project", "(d => d && [...d.querySelectorAll('select')].length === 2 ? (d.querySelectorAll('select')[0].value = 'alpha', d.querySelectorAll('select')[0].dispatchEvent(new Event('change')), d.querySelectorAll('select')[1].value === 'proj') : false)(document.querySelector('.dialog'))");
    dialog_button(&page, "Add");
    no_dialog(&page);
    let wall = wait_for("the global layout on the other station", 15, || {
        let l = global_layout(&b, "wall");
        (leaves_of(&l).len() == 2).then_some(l)
    });
    let ls = leaves_of(&wall);
    assert_eq!(
        (
            view(&ls, "files")["station"].clone(),
            view(&ls, "files")["folder"].clone(),
            view(&ls, "watch")["station"].clone(),
            view(&ls, "watch")["watch"].clone()
        ),
        (
            json!("alpha"),
            json!(dir.join("out")),
            json!("alpha"),
            json!("proj")
        )
    );
    other.goto("#/l/wall");
    at(&other, &dir.join("out"));
    shows(&other, "scene.txt");
    other.wait(
        "Watch of alpha's project, from bravo",
        &format!("{WATCH}?.querySelector('.watch-agent-name')?.innerText === 'claude'"),
    );
    // In a global layout every view says its station, as a pane does.
    assert_eq!(
        other.js("[...document.querySelectorAll('.view-head .chip.guest')].map(c => c.innerText)"),
        json!(["on alpha", "on alpha"])
    );

    // An agent and a shell are added as before, beneath the first choice:
    // one that is running, from any station, or a new one from the New
    // pane dialog, which is as it was and starts at a shell.
    add_pane(&page, "shell");
    assert_eq!(
        page.wait("the running shells", "(r => r.length ? r : null)([...document.querySelectorAll('.dialog .list-row')].map(b => b.innerText.replace(/\\s+/g, ' ').trim()))"),
        json!(["dev alpha · proj", "scratch bravo"])
    );
    click(
        &page,
        "scratch",
        "[...document.querySelectorAll('.dialog .list-row')].find(b => b.innerText.includes('scratch'))",
    );
    no_dialog(&page);
    page.wait(
        "the shell of the other station, in the layout",
        "document.querySelectorAll('.workspace .pane').length === 1",
    );
    add_pane(&page, "shell");
    dialog_button(&page, "Start a new shell…");
    page.wait(
        "the New pane dialog, at a shell",
        "document.querySelector('.dialog h2')?.innerText === 'New pane on alpha' && document.querySelector('.dialog select').value === 'shell'",
    );
    dialog_button(&page, "Start");
    page.wait(
        "the new shell, in the layout",
        "location.hash === '#/l/wall' && document.querySelectorAll('.workspace .pane').length === 2 && document.querySelectorAll('.workspace .view-cell').length === 2",
    );
    let ls = wait_for("four leaves, saved", 10, || {
        let ls = leaves_of(&global_layout(&a, "wall"));
        (ls.len() == 4).then_some(ls)
    });
    assert_eq!(
        ls.iter()
            .filter(|l| l["id"].as_str().is_some_and(|id| id.starts_with("p-")))
            .map(|l| l["station"].clone())
            .collect::<Vec<_>>(),
        [json!("bravo"), json!("alpha")]
    );
}

/// A project's layout saved through the API, as the deck saves it.
fn save_layout(s: &Station, tree: Value, hidden: Value) {
    let mut layout = json!({"name": "studio", "tree": tree});
    if hidden.as_array().is_some_and(|h| !h.is_empty()) {
        layout["hidden"] = hidden;
    }
    s.call(
        "project.layout.save",
        json!({"project": "proj", "layout": layout}),
    );
}

fn split(dir: &str, a: Value, b: Value) -> Value {
    json!({"dir": dir, "ratio": 500, "a": a, "b": b})
}

/// The Files pane is the panel's own listing in a cell: a file is renamed
/// and deleted with Undo there, what a pane works in is refused there, a
/// name being typed is not taken away when the page is drawn again, and the
/// side panel beside it still works, with the one clipboard between them.
/// Where it was browsed to is this browser's; where it opens is the
/// layout's, and "Open here from now on" changes that for every device.
#[test]
#[ignore = "merge tier: a browser"]
fn everything_the_panel_does_works_in_a_files_pane_and_the_panel_still_works() {
    let s = Station::start("alpha");
    let (dir, _agent) = project(&s);
    save_layout(
        &s,
        split(
            "row",
            json!({"pane": "dev"}),
            json!({"id": "view.files.t1"}),
        ),
        json!([]),
    );
    let page = Page::open(&s, 1500, 900);
    page.goto("#/s/alpha/l/proj/studio");
    at(&page, &dir);
    shows(&page, "notes.md");
    // A cell says its station in its head, not in a chooser of its own.
    assert_eq!(
        page.js(&format!("!!{FILES}.querySelector('.fx-station')")),
        json!(false)
    );

    // Rename (F2). While the name is being typed the page hears of a change
    // and is not drawn again under it: the box is still there, with the
    // keyboard.
    click(&page, "notes.md", &row("notes.md"));
    key(&page, "F2");
    page.wait(
        "the name's box",
        &format!(
            "(b => !!b && document.activeElement === b)({FILES}.querySelector('.fx-name-box'))"
        ),
    );
    s.call("pane.set", json!({"pane": "dev", "muted": true}));
    std::thread::sleep(std::time::Duration::from_millis(900));
    page.wait(
        "the box, still",
        &format!("(b => b && document.activeElement === b ? (b.value = 'plan.md', b.dispatchEvent(new KeyboardEvent('keydown', {{key: 'Enter', bubbles: true, cancelable: true}})), true) : false)({FILES}.querySelector('.fx-name-box'))"),
    );
    shows(&page, "plan.md");
    assert!(dir.join("plan.md").is_file() && !dir.join("notes.md").exists());

    // Delete: to the trash, and Undo puts it back.
    click(&page, "plan.md", &row("plan.md"));
    key(&page, "Delete");
    wait_for("plan.md gone from disk", 10, || {
        (!dir.join("plan.md").exists()).then_some(())
    });
    click(
        &page,
        "Undo",
        "[...document.querySelectorAll('.toast .toast-act')].find(b => b.innerText === 'Undo')",
    );
    wait_for("plan.md back on disk", 10, || {
        dir.join("plan.md").is_file().then_some(())
    });
    shows(&page, "plan.md");

    // What a pane works in is not deleted: the station refuses, and the
    // page names the pane.
    s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": "inside", "cwd": dir.join("out")}),
    );
    click(&page, "out", &row("out"));
    key(&page, "Delete");
    let said = page.wait(
        "the notice",
        "document.querySelector('.dialog .fx-inuse')?.innerText",
    );
    assert!(said.as_str().unwrap().contains("inside"), "{said}");
    dialog_button(&page, "OK");
    no_dialog(&page);
    assert!(dir.join("out/scene.txt").is_file());

    // New, from the cell's own button.
    click(&page, "New", &format!("{FILES}.querySelector('.fx-new')"));
    page.choose("File");
    page.wait(
        "the new name",
        &format!("(b => b ? (b.value = 'todo.md', b.dispatchEvent(new KeyboardEvent('keydown', {{key: 'Enter', bubbles: true, cancelable: true}})), true) : false)({FILES}.querySelector('.fx-name-box'))"),
    );
    shows(&page, "todo.md");
    assert!(dir.join("todo.md").is_file());

    // The side panel, opened from a pane's strip as before, beside the
    // cell. A file copied there is pasted in the cell.
    click(
        &page,
        "the strip's folder button",
        "document.querySelector('.workspace .pane .strip .strip-files')",
    );
    let panel = "document.getElementById('files-panel')";
    let panel_row = |name: &str| {
        format!("[...({panel}?.querySelectorAll('.fx-row[data-i]') || [])].find(r => r.querySelector('.fx-name')?.textContent === {})", json!(name))
    };
    page.wait(
        "the panel beside the layout",
        &format!(
            "{panel}?.querySelector('.fx-view')?.listing.root === {} && !!{} && !!{}",
            json!(dir),
            panel_row("plan.md"),
            row("plan.md")
        ),
    );
    click(&page, "plan.md in the panel", &panel_row("plan.md"));
    page.wait(
        "Ctrl+C in the panel",
        &format!("(t => t ? (t.focus(), t.dispatchEvent(new KeyboardEvent('keydown', {{key: 'c', ctrlKey: true, bubbles: true, cancelable: true}})), true) : false)({panel}.querySelector('.fx-tree'))"),
    );
    // Into `out`, in the cell: browsed to there, never saved to the layout.
    page.wait(
        "out opened in the cell",
        &format!("(r => r ? (r.dispatchEvent(new MouseEvent('dblclick', {{bubbles: true, cancelable: true}})), true) : false)({})", row("out")),
    );
    at(&page, &dir.join("out"));
    page.wait(
        "Ctrl+V in the cell",
        &format!("(t => t ? (t.focus(), t.dispatchEvent(new KeyboardEvent('keydown', {{key: 'v', ctrlKey: true, bubbles: true, cancelable: true}})), true) : false)({FILES}.querySelector('.fx-tree'))"),
    );
    wait_for("the copy on disk", 15, || {
        (std::fs::read(dir.join("out/plan.md")).ok().as_deref() == Some(b"# notes\n")).then_some(())
    });
    shows(&page, "plan.md");
    click(
        &page,
        "close the panel",
        &format!("{panel}.querySelector('.fx-close')"),
    );
    page.wait("the panel gone", &format!("!{panel}"));
    at(&page, &dir.join("out"));

    // Where it was is this browser's: there after a reload, and the head
    // offers the way back. The layout is as it was saved.
    page.reload();
    at(&page, &dir.join("out"));
    assert_eq!(
        page.js(&format!("{FILES}.querySelector('.view-back')?.innerText")),
        json!("Back to proj")
    );
    assert_eq!(
        leaves_of(&project_layout(&s, "proj", "studio")),
        [json!({"pane": "dev"}), json!({"id": "view.files.t1"})]
    );
    // "Open here from now on": the layout's, so another browser opens there.
    click(
        &page,
        "the cell's menu",
        &format!("{FILES}.querySelector('.view-menu')"),
    );
    page.choose("Open here from now on");
    wait_for("the folder saved with the layout", 10, || {
        (leaves_of(&project_layout(&s, "proj", "studio"))[1]
            == json!({"id": "view.files.t1", "folder": dir.join("out")}))
        .then_some(())
    });
    let other = Page::open(&s, 1200, 800);
    other.goto("#/s/alpha/l/proj/studio");
    at(&other, &dir.join("out"));
    assert_eq!(
        other.js(&format!("[{FILES}.querySelector('.view-name').innerText, !!{FILES}.querySelector('.view-back')]")),
        json!(["out", false])
    );
}

/// The Watch pane is the project's Watch in a cell, kept current: an agent
/// goes from idle to working to needing the owner, and the head and the
/// list on the left say so; an agent started later appears; what they say
/// to each other is in the thread. A tap on an agent goes to it: its pane
/// in this layout when it is here (shown, if it was hidden), else its pane
/// by itself.
#[test]
#[ignore = "merge tier: a browser"]
fn a_watch_pane_follows_its_agents_and_a_tap_goes_to_the_agent() {
    let s = Station::start("alpha");
    let (dir, agent) = project(&s);
    let tree = || {
        split(
            "row",
            json!({"pane": "claude"}),
            split(
                "col",
                json!({"id": "view.watch.w1"}),
                json!({"pane": "dev"}),
            ),
        )
    };
    save_layout(&s, tree(), json!([]));
    let page = Page::open(&s, 1500, 900);
    page.goto("#/s/alpha/l/proj/studio");
    let agent_row = format!("{WATCH}?.querySelector('.watch-agent')");
    page.wait(
        "the agent, idle",
        &format!("{agent_row}?.classList.contains('idle') && {WATCH}.querySelector('.view-count').innerText === '1 agent'"),
    );

    std::fs::write(dir.join("go"), b"").unwrap();
    page.wait(
        "working",
        &format!("{agent_row}?.classList.contains('working')"),
    );
    std::fs::write(dir.join("ask"), b"").unwrap();
    page.wait(
        "needs you, in the cell's head and in the list",
        &format!("{agent_row}?.classList.contains('needs_you') && {WATCH}.querySelector('.view-head .chip.needs')?.innerText === '1 needs you' && document.querySelector('.sidebar .side-view .dot').classList.contains('needs_you') && document.querySelector('.sidebar .side-view').getAttribute('aria-label') === 'proj, Watch, in the layout studio, needs you'"),
    );

    // A tap on it: its pane is in this layout, so that is where it goes.
    click(&page, "the agent", &agent_row);
    page.wait(
        "its pane in the layout, marked",
        &format!("location.hash === '#/s/alpha/l/proj/studio' && !!document.querySelector('.workspace .pane.view-flash[data-pane=\"{agent}\"]')"),
    );
    // Hidden in the layout, it is shown again.
    save_layout(&s, tree(), json!([{"pane": "claude"}]));
    page.wait(
        "the agent's pane hidden",
        &format!("document.querySelectorAll('.workspace .pane').length === 1 && !!document.querySelector('.hid-chip') && !!{agent_row}"),
    );
    click(&page, "the agent", &agent_row);
    page.wait(
        "its pane shown again",
        &format!("!!document.querySelector('.workspace .pane[data-pane=\"{agent}\"]') && !document.querySelector('.hid-chip')"),
    );
    wait_for("shown, in the saved layout", 10, || {
        project_layout(&s, "proj", "studio")
            .get("hidden")
            .is_none()
            .then_some(())
    });

    // Another agent starts: it is in the Watch without a reload, and what
    // it says to the first is in the thread, as text.
    let critic = s.fake(
        json!({"steps": [{"rpc": {"method": "msg.send", "params": {"to": "claude", "kind": "question", "body": "which scene is <b>next</b>?"}}}]}),
        json!({"name": "critic", "project": "proj", "cwd": dir}),
    );
    page.wait(
        "two agents",
        &format!("{WATCH}?.querySelectorAll('.watch-agent').length === 2 && {WATCH}.querySelector('.view-count').innerText === '2 agents'"),
    );
    let said = page.wait(
        "the message",
        &format!("(m => m && m.querySelector('.watch-body').textContent.includes('<b>next</b>') ? m.querySelector('.watch-msg-head').innerText.replace(/\\s+/g, ' ') : null)({WATCH}.querySelector('.watch-thread .watch-msg'))"),
    );
    assert!(
        said.as_str().unwrap().starts_with("critic → claude"),
        "{said}"
    );
    // It is not in this layout: a tap goes to its pane.
    click(
        &page,
        "the critic",
        &format!("[...{WATCH}.querySelectorAll('.watch-agent')].find(a => a.dataset.agent === '{critic}')"),
    );
    page.wait(
        "the critic's pane",
        &format!("location.hash === '#/s/alpha/p/{critic}'"),
    );
    // Taking the Watch out of the layout stops nothing.
    page.goto("#/s/alpha/l/proj/studio");
    click(
        &page,
        "the cell's menu",
        &format!("{WATCH}?.querySelector('.view-menu')"),
    );
    page.choose("Take out of this layout");
    page.wait(
        "no Watch pane",
        &format!("!{WATCH} && document.querySelectorAll('.workspace .pane').length === 2"),
    );
    assert_eq!(s.pane(&agent)["state"], json!("needs_you"));
    assert_eq!(s.call("pane.list", json!({})).as_array().unwrap().len(), 3);
}

/// A view is hidden, shown, moved and taken out like any leaf, and what
/// walks a layout's tree keeps it whole: hiding and showing beside it, a
/// shape chosen in Arrange, a hidden pane that was closed leaving the
/// layout. Hidden is on the view's own leaf, not in the layout's list.
/// Taking it out deletes nothing.
#[test]
#[ignore = "merge tier: a browser"]
fn a_view_is_hidden_moved_and_taken_out_and_what_walks_the_tree_keeps_it() {
    let s = Station::start("alpha");
    let (dir, _agent) = project(&s);
    let scratch = s.call("pane.spawn", json!({"agent": "shell", "name": "scratch"}))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let files = json!({"id": "view.files.f1", "folder": dir.join("out"), "hidden": true});
    let watch = json!({"id": "view.watch.w1"});
    save_layout(
        &s,
        split(
            "row",
            split("col", json!({"pane": "claude"}), json!({"id": scratch})),
            split("col", files, split("row", watch, json!({"pane": "dev"}))),
        ),
        json!([{"id": scratch}]),
    );
    let saved = || project_layout(&s, "proj", "studio");
    let page = Page::open(&s, 1500, 900);
    page.goto("#/s/alpha/l/proj/studio");
    // Two chips: the hidden shell, and the hidden Files by its kind and folder.
    assert_eq!(
        page.wait("the chips", "(c => c.length === 2 ? c : null)([...document.querySelectorAll('.hid-chip .hid-show')].map(b => b.getAttribute('aria-label')))"),
        json!(["Show scratch, Shell, idle. Hidden.", "Show Files · out. Hidden."])
    );
    page.wait(
        "Watch showing, Files not",
        &format!("!{FILES} && !!{WATCH}"),
    );

    // Shown from its chip, where it was; then Watch hidden from its own head.
    click(
        &page,
        "the Files chip",
        "[...document.querySelectorAll('.hid-chip .hid-show')].find(b => b.innerText.includes('Files'))",
    );
    at(&page, &dir.join("out"));
    wait_for("shown, on its leaf", 10, || {
        let l = saved();
        (view(&leaves_of(&l), "files")
            == &json!({"id": "view.files.f1", "folder": dir.join("out")})
            && l["hidden"] == json!([{"id": scratch}]))
        .then_some(())
    });
    click(
        &page,
        "Hide, on the Watch pane",
        &format!("{WATCH}.querySelector('.strip-hide')"),
    );
    page.wait(
        "Watch hidden",
        &format!("!{WATCH} && document.querySelectorAll('.hid-chip').length === 2"),
    );
    wait_for("hidden, on its leaf and not in the list", 10, || {
        let l = saved();
        (view(&leaves_of(&l), "watch") == &json!({"id": "view.watch.w1", "hidden": true})
            && l["hidden"] == json!([{"id": scratch}]))
        .then_some(())
    });

    // A shape chosen in Arrange puts every leaf somewhere else, and keeps
    // each as it is.
    click(
        &page,
        "Arrange",
        "[...document.querySelectorAll('.lay-head button')].find(b => b.innerText === 'Arrange')",
    );
    click(
        &page,
        "Grid",
        "[...document.querySelectorAll('.arrange-bar button')].find(b => b.innerText === 'Grid')",
    );
    wait_for("the grid saved", 10, || {
        let l = saved();
        (l["tree"]["dir"] == "col" && leaves_of(&l).len() == 5).then_some(())
    });
    // Moved by a drag, as a pane is: Files to the left of the agent.
    page.wait(
        "the panes to be draggable",
        "document.querySelectorAll('.arr[data-ref]').length === 3",
    );
    page.js("(() => { const of = (n) => [...document.querySelectorAll('.arr')].find(a => a.dataset.ref === n);
        const a = of('#view.files.f1').getBoundingClientRect(), b = of('dev').getBoundingClientRect();
        const ev = (t, x, y, el) => (el || window).dispatchEvent(new MouseEvent(t, {bubbles: true, cancelable: true, clientX: x, clientY: y}));
        ev('mousedown', a.left + a.width / 2, a.top + a.height / 2, of('#view.files.f1'));
        ev('mousemove', b.left + b.width * 0.1, b.top + b.height / 2); ev('mouseup', b.left + b.width * 0.1, b.top + b.height / 2); return true; })()");
    wait_for("the move saved", 10, || {
        let ls = leaves_of(&saved());
        let i = |want: &Value| ls.iter().position(|l| l == want);
        i(&json!({"id": "view.files.f1", "folder": dir.join("out")}))
            .is_some_and(|f| Some(f + 1) == i(&json!({"pane": "dev"})))
            .then_some(())
    });
    click(
        &page,
        "Done arranging",
        "[...document.querySelectorAll('.lay-head button')].find(b => b.innerText === 'Done arranging')",
    );

    // A hidden pane that is closed leaves the layout (the deck takes it
    // out); the views beside it do not, hidden or showing.
    s.call("pane.close", json!({"pane": scratch}));
    wait_for("the closed pane out of the layout", 15, || {
        saved().get("hidden").is_none().then_some(())
    });
    let ls = leaves_of(&saved());
    assert_eq!(ls.len(), 4, "{ls:?}");
    assert_eq!(
        view(&ls, "files"),
        &json!({"id": "view.files.f1", "folder": dir.join("out")})
    );
    assert_eq!(
        view(&ls, "watch"),
        &json!({"id": "view.watch.w1", "hidden": true})
    );
    // A view's chip shows it, as a pane's does.
    page.wait(
        "the Watch chip",
        "document.querySelectorAll('.hid-chip').length === 1",
    );
    click(
        &page,
        "the Watch chip",
        "document.querySelector('.hid-chip .hid-show')",
    );
    page.wait(
        "Watch showing",
        &format!("!!{WATCH} && !document.querySelector('.hid-chip')"),
    );

    // Taken out: gone from the layout, and nothing on disk is touched.
    click(
        &page,
        "the Files menu",
        &format!("{FILES}.querySelector('.view-menu')"),
    );
    page.choose("Take out of this layout");
    page.wait("no Files pane", &format!("!{FILES}"));
    wait_for("the layout without it", 10, || {
        let ls = leaves_of(&saved());
        (ls.len() == 3 && ls.contains(&json!({"id": "view.watch.w1"}))).then_some(())
    });
    assert_eq!(
        std::fs::read(dir.join("out/scene.txt")).unwrap(),
        b"scene 4\n"
    );
    assert!(dir.join("notes.md").is_file());
}

/// The Control Deck of 0.4.19, reading a layout with views and writing it
/// again (its own code, kept in common/deck_0_4_19_layout.js). It does not
/// throw; it takes each view for a pane named by id; it marks none hidden,
/// so the rule by which it takes a hidden pane it cannot find out of a
/// layout never meets one; and what it writes is taken by the station, and
/// is still both views on a deck of this version: showing, at their
/// defaults.
#[test]
#[ignore = "merge tier: a browser"]
fn the_deck_of_0_4_19_reads_views_as_panes_by_id_and_writes_them_back() {
    let s = Station::start("alpha");
    let (dir, _agent) = project(&s);
    save_layout(
        &s,
        split(
            "row",
            json!({"pane": "claude"}),
            split(
                "col",
                json!({"id": "view.files.f1", "folder": dir.join("out"), "hidden": true}),
                split(
                    "row",
                    json!({"id": "view.watch.w1", "station": "alpha", "watch": "proj"}),
                    json!({"pane": "dev"}),
                ),
            ),
        ),
        json!([{"pane": "dev"}]),
    );
    let page = Page::open(&s, 1400, 900);
    let old = include_str!("common/deck_0_4_19_layout.js");
    let def = project_layout(&s, "proj", "studio");
    let wrote = page.js(&format!(
        "(() => {{ {old}\n const def = {def}; const tree = treeFrom(def); const marked = leaves(tree).filter(n => n.hidden).map(cleanLeaf); return {{ marked, body: layoutBody(def.name, {{ tree, lines: def.lines }}) }}; }})()"
    ));
    assert_eq!(
        wrote["marked"],
        json!([{"pane": "dev"}]),
        "only the pane is hidden to it"
    );
    assert_eq!(
        leaves_of(&wrote["body"]),
        [
            json!({"pane": "claude"}),
            json!({"id": "view.files.f1"}),
            json!({"id": "view.watch.w1", "station": "alpha"}),
            json!({"pane": "dev"})
        ]
    );
    assert_eq!(wrote["body"]["hidden"], json!([{"pane": "dev"}]));
    // What it wrote, saved: the station takes it, and this deck shows both.
    s.call(
        "project.layout.save",
        json!({"project": "proj", "layout": wrote["body"]}),
    );
    page.goto("#/s/alpha/l/proj/studio");
    at(&page, &dir);
    page.wait(
        "Watch, of the layout's own project",
        &format!("{WATCH}?.querySelector('.watch-agent-name')?.innerText === 'claude'"),
    );
}

/// On a phone a layout is stepped through one pane at a time, and all four
/// kinds are steps: an agent's conversation, a shell's terminal, the file
/// browser with the screen to itself, and Watch. The line says which, with
/// the kind of a view; the list says each pane's kind in a word; and a tap
/// on an agent in Watch steps to its pane.
#[test]
#[ignore = "merge tier: a browser"]
fn a_phone_steps_through_all_four_kinds() {
    let s = Station::start("alpha");
    let (dir, _agent) = project(&s);
    save_layout(
        &s,
        split(
            "row",
            split("col", json!({"pane": "claude"}), json!({"pane": "dev"})),
            split(
                "col",
                json!({"id": "view.files.f1"}),
                json!({"id": "view.watch.w1"}),
            ),
        ),
        json!([]),
    );
    let page = Page::open(&s, 390, 844);
    page.wait(
        "the list by station",
        "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.home-switch button')].find(b => b.innerText === 'By station'))",
    );
    // The list: the layout with its two views under it, then the panes,
    // each with its kind. Nothing runs off the screen.
    let list = page.wait(
        "the list",
        "(rows => rows.length >= 6 ? rows : null)([...document.querySelectorAll('main.phone .phone-station .phone-row')].map(r => [r.getAttribute('aria-label'), r.querySelector('.kind-tag')?.innerText.toLowerCase() || null]))",
    );
    assert_eq!(
        list,
        json!([
            ["studio, layout", null],
            ["proj, Files, in the layout studio", "files"],
            ["proj, Watch, in the layout studio", "watch"],
            ["Watch proj", null],
            ["claude, Agent, idle, Chat", "chat"],
            ["dev, Shell, idle, Term", "term"]
        ]),
    );
    assert_eq!(
        page.js("[...document.querySelectorAll('main.phone .phone-row')].every(r => r.getBoundingClientRect().right <= innerWidth && r.scrollWidth <= r.clientWidth + 1) && !!document.querySelector('main.phone .dot-key-link')"),
        json!(true)
    );
    page.wait(
        "the layout",
        "(r => r ? (r.click(), true) : false)(document.querySelector('main.phone .phone-station .lay-row'))",
    );
    let showing = "document.querySelector('.lay-showing')?.innerText";
    let next = || {
        click(
            &page,
            "Next",
            "[...document.querySelectorAll('.lay-step')].pop()",
        )
    };
    page.wait(
        "the agent, as its conversation",
        &format!("{showing} === 'studio › claude · 1 of 4' && !!document.querySelector('.phone-layout .chat-bar')"),
    );
    next();
    page.wait(
        "the shell, as its terminal",
        &format!("{showing} === 'studio › dev · 2 of 4' && !!document.querySelector('.phone-layout .xterm-rows')"),
    );
    next();
    // Files: the file browser with the screen to itself, as on a phone.
    page.wait(
        "Files",
        &format!("{showing} === 'studio › Files · proj · 3 of 4'"),
    );
    at(&page, &dir);
    shows(&page, "notes.md");
    assert_eq!(
        page.js(&format!("(c => {{ const r = c.getBoundingClientRect(), t = c.querySelector('.fx-tree').getBoundingClientRect(); return [r.left === 0 && Math.round(r.right) === innerWidth, t.height > 300, Math.round(r.bottom) <= innerHeight, !!c.querySelector('.fx-upload'), !document.querySelector('.phone-layout .xterm-rows, .phone-layout .chat-bar')]; }})({FILES})")),
        json!([true, true, true, true, true]),
        "full width, most of the height, the phone's buttons, and no pane's bar above it"
    );
    // A tap opens, as in the panel on a phone.
    click(&page, "out", &row("out"));
    at(&page, &dir.join("out"));
    shows(&page, "scene.txt");
    next();
    page.wait(
        "Watch",
        &format!("{showing} === 'studio › Watch · proj · 4 of 4' && {WATCH}?.querySelector('.watch-agent-name')?.innerText === 'claude' && !{FILES}"),
    );
    assert_eq!(
        page.js("[...document.querySelectorAll('.lay-map button')].map(b => b.getAttribute('aria-label'))"),
        json!(["claude, idle", "dev, idle", "Files · proj, a view", "Watch · proj, a view"])
    );
    // A tap on the agent steps to its pane in this layout.
    click(
        &page,
        "the agent",
        &format!("{WATCH}.querySelector('.watch-agent')"),
    );
    page.wait(
        "back at the agent",
        &format!("{showing} === 'studio › claude · 1 of 4' && location.hash === '#/s/alpha/l/proj/studio' && !!document.querySelector('.phone-layout .chat-bar')"),
    );
    // A view's row in the list opens the layout at that view.
    page.goto("#/fleet");
    click(
        &page,
        "the Files row",
        "document.querySelector('main.phone .phone-view-row')",
    );
    page.wait(
        "the layout, at Files",
        &format!("{showing} === 'studio › Files · proj · 3 of 4' && !!{FILES}"),
    );
    // A pane is added by its kind from the map's panel, and the dialog
    // fits the phone: four kinds, each big enough to tap.
    click(&page, "the line", "document.querySelector('.lay-where')");
    click(
        &page,
        "Add a pane",
        "document.querySelector('.lay-panel .lay-add')",
    );
    page.wait(
        "the four kinds, within the screen",
        "(d => !!d && d.querySelectorAll('input[name=pane-kind]').length === 4 && d.getBoundingClientRect().right <= innerWidth && [...d.querySelectorAll('.kind-card')].every(c => c.getBoundingClientRect().height >= 44 && c.scrollWidth <= c.clientWidth + 1))(document.querySelector('.dialog'))",
    );
    click(
        &page,
        "watch",
        "document.querySelector('.dialog input[name=pane-kind][value=watch]')",
    );
    dialog_button(&page, "Next");
    dialog_button(&page, "Add");
    no_dialog(&page);
    page.wait(
        "the new Watch, shown",
        &format!("{showing} === 'studio › Watch · proj · 5 of 5' && !!{WATCH}"),
    );
}

/// Not a test of behaviour: writes pictures of the result for a human to
/// look at. `SPANDECK_SHOTS=/some/dir cargo test -p spandeck --test viewpanes screenshots_ -- --ignored`
#[test]
#[ignore]
fn screenshots_of_view_panes() {
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    let out = PathBuf::from(std::env::var("SPANDECK_SHOTS").expect("set SPANDECK_SHOTS"));
    std::fs::create_dir_all(&out).unwrap();
    let shoot = |page: &Page, name: &str| {
        std::thread::sleep(std::time::Duration::from_millis(900));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("{name}.png")), png).unwrap();
    };
    let s = Station::start("alpha");
    let (dir, agent) = project(&s);
    for (name, bytes) in [
        ("out/scene4.mp4", 48_000usize),
        ("out/scene3.mp4", 61_000),
        ("HANDOFF.md", 6_000),
    ] {
        std::fs::write(dir.join(name), vec![b'x'; bytes]).unwrap();
    }
    s.fake(
        json!({"steps": [{"prompt": "review the cut"}, {"rpc": {"method": "msg.send", "params": {"to": "claude", "kind": "question", "body": "Three wide shots still use the first-round layout. Keep them?"}}}, {"turn_end": "Sent my notes to claude."}]}),
        json!({"name": "critic", "project": "proj", "cwd": dir}),
    );
    save_layout(
        &s,
        split(
            "row",
            split("col", json!({"pane": "claude"}), json!({"pane": "dev"})),
            split(
                "col",
                json!({"id": "view.watch.w1"}),
                json!({"id": "view.files.f1", "folder": dir.join("out")}),
            ),
        ),
        json!([]),
    );
    s.call(
        "pane.send_text",
        json!({"pane": "dev", "text": "ls -la out\n"}),
    );
    std::fs::write(dir.join("go"), b"").unwrap();
    std::fs::write(dir.join("ask"), b"").unwrap();
    s.wait_state(&agent, "needs_you");

    let page = Page::open(&s, 1500, 920);
    page.goto("#/s/alpha/l/proj/studio");
    at(&page, &dir.join("out"));
    page.wait(
        "Watch",
        &format!("{WATCH}?.querySelectorAll('.watch-agent').length === 2"),
    );
    shoot(&page, "01-desktop-four-kinds");
    click(
        &page,
        "the key",
        "document.querySelector('.sidebar .dot-key-link')",
    );
    page.wait(
        "the key",
        "!!document.querySelector('.dialog .dot-key-list')",
    );
    shoot(&page, "02-key-to-the-dots");
    dialog_button(&page, "Done");
    click(
        &page,
        "Add pane",
        "document.querySelector('.lay-head .add-pane')",
    );
    page.wait(
        "the dialog",
        "!!document.querySelector('.dialog .kind-card')",
    );
    shoot(&page, "03-add-pane-kind");
    click(
        &page,
        "files",
        "document.querySelector('.dialog input[name=pane-kind][value=files]')",
    );
    dialog_button(&page, "Next");
    page.wait(
        "the details",
        "!!document.querySelector('.dialog input.mono')",
    );
    shoot(&page, "04-add-pane-files");
    dialog_button(&page, "Cancel");
    no_dialog(&page);
    // The list on the left by itself: a narrow window is all list.
    let narrow = Page::open(&s, 800, 700);
    narrow.goto("#/s/alpha/l/proj/studio");
    narrow.wait(
        "the list",
        "document.querySelectorAll('.sidebar .side-view').length === 2",
    );
    shoot(&narrow, "05-left-list-at-800");
    click(
        &page,
        "Hide, on the Watch pane",
        &format!("{WATCH}.querySelector('.strip-hide')"),
    );
    page.wait("Watch hidden", &format!("!{WATCH}"));
    shoot(&page, "06-watch-hidden-as-a-chip");
    click(
        &page,
        "the Watch chip",
        "document.querySelector('.hid-chip .hid-show')",
    );
    page.wait("Watch showing", &format!("!!{WATCH}"));

    let phone = Page::open(&s, 390, 844);
    phone.wait(
        "the list by station",
        "(b => b ? (b.click(), true) : false)([...document.querySelectorAll('.home-switch button')].find(b => b.innerText === 'By station'))",
    );
    phone.wait(
        "the list",
        "document.querySelectorAll('main.phone .phone-view-row').length === 2",
    );
    shoot(&phone, "07-phone-list");
    click(
        &phone,
        "the Files row",
        "[...document.querySelectorAll('main.phone .phone-view-row')].find(r => r.innerText.includes('out'))",
    );
    at(&phone, &dir.join("out"));
    shoot(&phone, "08-phone-files");
    phone.goto("#/fleet");
    click(
        &phone,
        "the Watch row",
        "[...document.querySelectorAll('main.phone .phone-view-row')].find(r => r.innerText.includes('proj'))",
    );
    phone.wait(
        "Watch",
        &format!("{WATCH}?.querySelectorAll('.watch-agent').length === 2"),
    );
    shoot(&phone, "09-phone-watch");
    click(&phone, "the line", "document.querySelector('.lay-where')");
    shoot(&phone, "10-phone-map");
}
