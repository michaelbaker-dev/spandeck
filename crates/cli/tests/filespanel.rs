//! The Control Deck's file explorer, in a real browser (DECISIONS.md, "The
//! files of a station", "The panel").
//!
//! The reason for every test here is the owner's request of 19 Nov 2026: a
//! panel at the right of the window that shows a station's files, with a
//! tree, new, rename, delete with Undo, copy and paste, uploads, downloads,
//! dragging between stations ("these create copies"), and a "Move agent
//! here" button; browsing never moves an agent; it works in every theme, in
//! a layout of several panes and on a phone. And rule 6 of AGENTS.md: a
//! file's name and what it holds are put on the page as text.
//!
//! Real stations, real files: after each step the files on disk are read
//! from here. What a browser cannot be made to do from a test is said where
//! it is stood in for: a `drop` and a drag are events the page is handed
//! (carrying real `File`s and a real `DataTransfer`), and a folder dropped
//! from the desktop is a stand-in for the browser's own entries.
//!
//! With `SPANDECK_SHOTS` set, pictures are written there:
//! `SPANDECK_SHOTS=/some/dir cargo test -p spandeck --test filespanel -- --ignored`
#![cfg(unix)]
mod common;
use common::browser::Page;
use common::*;
use serde_json::json;
use std::path::{Path, PathBuf};

const FX: &str = "document.getElementById('files-panel')";

fn shot(page: &Page, name: &str) {
    use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png;
    if let Some(out) = std::env::var_os("SPANDECK_SHOTS").map(PathBuf::from) {
        std::fs::create_dir_all(&out).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(450));
        let png = page.tab.capture_screenshot(Png, None, None, true).unwrap();
        std::fs::write(out.join(format!("files-{name}.png")), png).unwrap();
    }
}

/// The listing `i` of the panel (0, and 1 with the second view).
fn v(i: usize) -> String {
    format!("{FX}?.querySelectorAll('.fx-view')[{i}]")
}

/// The row called `name` in listing `i`.
fn row(i: usize, name: &str) -> String {
    format!(
        "[...({}?.querySelectorAll('.fx-row[data-i]') || [])].find(r => r.querySelector('.fx-name')?.textContent === {})",
        v(i),
        json!(name)
    )
}

fn names(page: &Page, i: usize) -> Vec<String> {
    page.js(&format!(
        "[...({}?.querySelectorAll('.fx-row[data-i] .fx-name') || [])].map(e => e.textContent)",
        v(i)
    ))
    .as_array()
    .map(|a| a.iter().map(|x| x.as_str().unwrap().to_string()).collect())
    .unwrap_or_default()
}

fn shows(page: &Page, i: usize, name: &str) {
    page.wait(&format!("the row {name}"), &format!("!!{}", row(i, name)));
}

fn shows_no(page: &Page, i: usize, name: &str) {
    page.wait(
        &format!("no row {name}"),
        &format!("!!{}?.listing.root && !{}", v(i), row(i, name)),
    );
}

/// A click the page is handed, on the element `el` (an expression), with
/// the keys held that `held` says (`ctrlKey: true`).
fn click_with(page: &Page, what: &str, el: &str, held: &str) {
    page.wait(
        what,
        &format!("(e => e && !e.disabled ? (e.dispatchEvent(new MouseEvent('click', {{bubbles: true, cancelable: true, {held}}})), true) : false)({el})"),
    );
}

fn click(page: &Page, what: &str, el: &str) {
    click_with(page, what, el, "");
}

fn double_click(page: &Page, what: &str, el: &str) {
    page.wait(
        what,
        &format!("(e => e ? (e.dispatchEvent(new MouseEvent('dblclick', {{bubbles: true, cancelable: true}})), true) : false)({el})"),
    );
}

/// Press a button of listing `i` by its class (`fx-mkdir`).
fn press(page: &Page, i: usize, class: &str) {
    click(page, class, &format!("{}?.querySelector('.{class}')", v(i)));
}

/// A key the tree of listing `i` is handed.
fn key(page: &Page, i: usize, key: &str, held: &str) {
    page.wait(
        key,
        &format!("(t => t ? (t.focus(), t.dispatchEvent(new KeyboardEvent('keydown', {{key: {}, bubbles: true, cancelable: true, {held}}})), true) : false)({}?.querySelector('.fx-tree'))", json!(key), v(i)),
    );
}

/// Type a name into the box of a row being named, and press Enter.
fn name_it(page: &Page, i: usize, name: &str) {
    page.wait(
        "the name's box",
        &format!("(b => b ? (b.value = {}, b.dispatchEvent(new KeyboardEvent('keydown', {{key: 'Enter', bubbles: true, cancelable: true}})), true) : false)({}?.querySelector('.fx-name-box'))", json!(name), v(i)),
    );
}

fn root(page: &Page, i: usize) -> String {
    page.js(&format!("{}?.listing.root || ''", v(i)))
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn at(page: &Page, i: usize, path: &Path) {
    page.wait(
        &format!("the folder {}", path.display()),
        &format!(
            "{}?.listing.root === {} && !{0}.listing.loading",
            v(i),
            json!(path)
        ),
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

fn shell(s: &Station, name: &str, cwd: &Path) -> String {
    s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "name": name, "cwd": cwd}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn home(s: &Station) -> PathBuf {
    let h = s.root.join("home");
    std::fs::create_dir_all(&h).unwrap();
    std::fs::canonicalize(&h).unwrap()
}

/// A pane at full size on a computer, and the panel opened from the folder
/// button of its strip.
fn open_from_strip(page: &Page, s: &Station, pane: &str) {
    page.goto(&format!("#/s/{}/p/{pane}", s.name));
    click(
        page,
        "the strip's folder button",
        "document.querySelector('.pane .strip .strip-files')",
    );
    page.wait("the panel", &format!("!!{FX} && !!{}?.listing.root", v(0)));
}

/// The same bytes here and in the page: `n` of them.
fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * 31 + 7) % 251) as u8).collect()
}
const PATTERN: &str = "((n) => Uint8Array.from({ length: n }, (_, i) => (i * 31 + 7) % 251))";

fn wait_file(path: &Path, want: &[u8]) {
    wait_for_or(
        &format!("{} on disk", path.display()),
        30,
        || {
            format!(
                "it holds {:?} bytes",
                std::fs::read(path).map(|b| b.len()).ok()
            )
        },
        || (std::fs::read(path).ok().as_deref() == Some(want)).then_some(()),
    );
}

/// Text that is hard to read in the panel: each `[what, contrast]` under
/// what it needs. Names, buttons and the path need 4.5:1; the quiet words
/// (sizes, times, a transfer's progress), which a theme sets in its
/// palette's quiet colour, need 3:1, as the look tests ask of a
/// conversation.
const LOW: &str = r#"(() => {
    const root = document.getElementById('files-panel');
    if (!root) return null;
    const ctx = (window.__ctx ||= document.createElement('canvas').getContext('2d', { willReadFrequently: true }));
    const rgba = (css) => { ctx.clearRect(0, 0, 1, 1); ctx.fillStyle = '#000'; ctx.fillStyle = css; ctx.fillRect(0, 0, 1, 1); return [...ctx.getImageData(0, 0, 1, 1).data]; };
    const behind = (e) => { for (; e; e = e.parentElement) { const c = rgba(getComputedStyle(e).backgroundColor); if (c[3] > 250) return c; } return rgba(getComputedStyle(document.body).backgroundColor); };
    const lum = (c) => { const [r, g, b] = c.slice(0, 3).map(v => { const s = v / 255; return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4; }); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
    const contrast = (a, b) => { const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x); return (hi + 0.05) / (lo + 0.05); };
    const low = [], seen = [];
    const READ = ['.fx-row:not(.sel):not(.odd) .fx-name', '.fx-row.sel .fx-name', '.fx-title', '.fx-seg.here', '.fx-tools button:not(:disabled)', '.fx-station', '.fx-xfer-what', '.fx-clip span'];
    const QUIET = ['.fx-size', '.fx-when', '.fx-sub', '.fx-seg:not(.here)', '.fx-cols span', '.fx-xfer-says', '.fx-row.odd .fx-name'];
    for (const [sels, least] of [[READ, 4.5], [QUIET, 3]]) for (const sel of sels) {
        for (const e of root.querySelectorAll(sel)) {
            if (!e.getClientRects().length || !(e.innerText || e.value || '').trim()) continue;
            const c = contrast(rgba(getComputedStyle(e).color), behind(e));
            seen.push(sel);
            if (c < least) { low.push([sel, Number(c.toFixed(2))]); break; }
        }
    }
    return { low, seen: [...new Set(seen)], panel: getComputedStyle(root).backgroundColor, row: getComputedStyle(root.querySelector('.fx-tree')).backgroundColor };
})()"#;

/// Opened from a pane's strip, the panel shows that pane's folder on that
/// pane's station; the owner goes into a folder, opens one in place, goes
/// up, to a part of the path, and to a path typed in, where a path that is
/// not there is said in place. None of it moves the pane. The folder's own
/// words in the strip and the pane's menu open it too, and Escape closes it.
#[test]
#[ignore = "merge tier: a browser"]
fn the_panel_opens_at_a_panes_folder_and_browsing_never_moves_the_pane() {
    let s = Station::start("alpha");
    let work = home(&s).join("work");
    std::fs::create_dir_all(work.join("src/deep")).unwrap();
    std::fs::write(work.join("src/main.rs"), b"fn main() {}").unwrap();
    std::fs::write(work.join("src/deep/leaf.txt"), b"leaf").unwrap();
    std::fs::write(work.join("notes.md"), b"# notes").unwrap();
    std::fs::write(work.join(".secret"), b"hidden").unwrap();
    std::fs::write(work.join("Zeta.txt"), b"z").unwrap();
    let sh = shell(&s, "scratch", &work);
    let page = Page::open(&s, 1400, 900);
    open_from_strip(&page, &s, &sh);

    // The pane's folder, on the pane's station, folders first.
    at(&page, 0, &work);
    assert_eq!(names(&page, 0), ["src", "notes.md", "Zeta.txt"]);
    assert_eq!(
        page.js(&format!(
            "[{}.querySelector('.fx-station').value, {FX}.querySelector('.fx-for').innerText, {0}.querySelector('.fx-seg.here').innerText]",
            v(0)
        )),
        json!(["alpha", "scratch", "work"])
    );
    // The panes are beside the panel, not under it.
    page.wait(
        "the pane beside the panel, which is at the window's right edge",
        &format!("(r => document.querySelector('.pane').getBoundingClientRect().right <= r.left + 1 && Math.round(r.right) === innerWidth && Math.round(r.width) === 440)({FX}.getBoundingClientRect())"),
    );
    // A tree, with its roles.
    assert_eq!(
        page.js(&format!(
            "(r => [{}.querySelector('.fx-tree').getAttribute('role'), r.getAttribute('role'), r.getAttribute('aria-expanded'), r.getAttribute('aria-selected'), r.getAttribute('aria-level')])({})",
            v(0),
            row(0, "src")
        )),
        json!(["tree", "treeitem", "false", "false", "1"])
    );

    // Hidden files, when asked for.
    press(&page, 0, "fx-hidden");
    shows(&page, 0, ".secret");
    press(&page, 0, "fx-hidden");
    shows_no(&page, 0, ".secret");

    // A folder opened in place: its files are rows of the same tree.
    click(
        &page,
        "the triangle of src",
        &format!("{}?.querySelector('.fx-tri')", row(0, "src")),
    );
    shows(&page, 0, "main.rs");
    assert_eq!(
        page.js(&format!(
            "[{}.getAttribute('aria-expanded'), {}.getAttribute('aria-level')]",
            row(0, "src"),
            row(0, "main.rs")
        )),
        json!(["true", "2"])
    );
    assert_eq!(root(&page, 0), work.display().to_string());

    // Double-click: that folder is the one shown. Up: back.
    double_click(&page, "src", &row(0, "src"));
    at(&page, 0, &work.join("src"));
    assert_eq!(names(&page, 0), ["deep", "main.rs"]);
    press(&page, 0, "fx-nav-up");
    at(&page, 0, &work);
    // A part of the path.
    click(
        &page,
        "the path's part for the home folder",
        &format!(
            "[...{}.querySelectorAll('.fx-seg')].find(b => b.title === {})",
            v(0),
            json!(home(&s))
        ),
    );
    at(&page, 0, &home(&s));
    shows(&page, 0, "work");
    // "The pane's folder" goes back to where the pane is.
    press(&page, 0, "fx-nav-pane");
    at(&page, 0, &work);

    // A path typed in. One that is not there is said in place, and the
    // folder shown stays.
    let type_path = |to: &Path| {
        press(&page, 0, "fx-edit");
        page.wait(
            "the path's box",
            &format!("(b => b ? (b.value = {}, b.dispatchEvent(new KeyboardEvent('keydown', {{key: 'Enter', bubbles: true, cancelable: true}})), true) : false)({}?.querySelector('.fx-path-box'))", json!(to), v(0)),
        );
    };
    type_path(&work.join("nowhere"));
    let said = page.wait(
        "what is wrong with the path",
        &format!("{}?.querySelector('.fx-path-err')?.innerText", v(0)),
    );
    let said = said.as_str().unwrap();
    assert!(
        !said.contains('{') && said.to_lowercase().contains("no"),
        "{said}"
    );
    assert_eq!(root(&page, 0), work.display().to_string());
    page.wait(
        "the box, with another path",
        &format!("(b => b ? (b.value = {}, b.dispatchEvent(new KeyboardEvent('keydown', {{key: 'Enter', bubbles: true, cancelable: true}})), true) : false)({}?.querySelector('.fx-path-box'))", json!(work.join("src/deep")), v(0)),
    );
    at(&page, 0, &work.join("src/deep"));
    assert_eq!(names(&page, 0), ["leaf.txt"]);

    // The arrow keys and Enter.
    press(&page, 0, "fx-nav-up");
    at(&page, 0, &work.join("src"));
    key(&page, 0, "ArrowDown", "");
    key(&page, 0, "Enter", "");
    at(&page, 0, &work.join("src/deep"));

    // None of that moved the pane, and it offers to.
    assert_eq!(s.pane(&sh)["cwd"], json!(work));
    assert!(s.log("pane.restarted").is_empty());
    page.wait(
        "the offer to move",
        &format!("{FX}.querySelector('.fx-move')?.textContent === 'Restart scratch here'"),
    );

    // Escape closes it when it has the keyboard, and the pane has the
    // window's width again.
    key(&page, 0, "Escape", "");
    page.wait("the panel gone", &format!("!{FX}"));
    page.wait(
        "the pane as wide as before",
        "innerWidth - document.querySelector('.pane').getBoundingClientRect().right < 40",
    );
    // The folder's own words in the strip open it, and so does the menu.
    click(
        &page,
        "the strip's folder",
        "document.querySelector('.pane .strip .strip-cwd')",
    );
    at(&page, 0, &work);
    click(&page, "Close", &format!("{FX}.querySelector('.fx-close')"));
    page.wait("the panel gone", &format!("!{FX}"));
    page.wait(
        "Files… in the pane's menu",
        "(i => i ? (i.click(), true) : (b => (b && b.click(), false))(!document.querySelector('.menu') && document.querySelector('.pane [aria-label=\"Pane menu\"]')))([...document.querySelectorAll('.menu-item')].find(i => i.innerText.startsWith('Files')))",
    );
    at(&page, 0, &work);
}

/// A folder and a file are made, a name that is taken is said in place, a
/// file is renamed in place, and a deleted file is in the trash until Undo
/// brings it back. Each is on the station's disk.
#[test]
#[ignore = "merge tier: a browser"]
fn files_are_made_renamed_and_deleted_with_undo() {
    let s = Station::start("alpha");
    let work = home(&s).join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("keep.txt"), b"kept").unwrap();
    let sh = shell(&s, "scratch", &work);
    let page = Page::open(&s, 1400, 900);
    open_from_strip(&page, &s, &sh);

    press(&page, 0, "fx-mkdir");
    name_it(&page, 0, "drafts");
    shows(&page, 0, "drafts");
    assert!(work.join("drafts").is_dir());

    press(&page, 0, "fx-mkfile");
    name_it(&page, 0, "todo.md");
    shows(&page, 0, "todo.md");
    assert_eq!(std::fs::read(work.join("todo.md")).unwrap(), b"");

    // A name that is taken: said in the row, and nothing is replaced.
    press(&page, 0, "fx-mkfile");
    name_it(&page, 0, "keep.txt");
    let said = page.wait(
        "the name is taken",
        &format!("{}?.querySelector('.fx-name-err')?.innerText", v(0)),
    );
    assert!(!said.as_str().unwrap().contains('{'), "{said}");
    shot(&page, "a-name-that-is-taken");
    assert_eq!(std::fs::read(work.join("keep.txt")).unwrap(), b"kept");
    page.wait(
        "Escape in the box",
        &format!("(b => b ? (b.dispatchEvent(new KeyboardEvent('keydown', {{key: 'Escape', bubbles: true, cancelable: true}})), true) : false)({}?.querySelector('.fx-name-box'))", v(0)),
    );
    page.wait(
        "the box gone",
        &format!("!{}.querySelector('.fx-name-box')", v(0)),
    );

    // Rename, in place (F2).
    click(&page, "todo.md", &row(0, "todo.md"));
    key(&page, 0, "F2", "");
    name_it(&page, 0, "done.md");
    shows(&page, 0, "done.md");
    assert!(work.join("done.md").is_file() && !work.join("todo.md").exists());

    // Delete: to the trash, and Undo puts it back.
    click(&page, "keep.txt", &row(0, "keep.txt"));
    key(&page, 0, "Delete", "");
    shows_no(&page, 0, "keep.txt");
    assert!(!work.join("keep.txt").exists());
    click(
        &page,
        "Undo",
        "[...document.querySelectorAll('.toast .toast-act')].find(b => b.innerText === 'Undo')",
    );
    shows(&page, 0, "keep.txt");
    assert_eq!(std::fs::read(work.join("keep.txt")).unwrap(), b"kept");
    // Two at once from the toolbar, a folder among them.
    click(&page, "drafts", &row(0, "drafts"));
    click_with(&page, "done.md too", &row(0, "done.md"), "ctrlKey: true");
    press(&page, 0, "fx-delete");
    shows_no(&page, 0, "drafts");
    assert!(!work.join("drafts").exists() && !work.join("done.md").exists());
    assert_eq!(names(&page, 0), ["keep.txt"]);
    assert_eq!(s.log("files.trashed").len(), 2);
    assert_eq!(s.log("files.restored").len(), 1);
}

/// Files come in by the Upload button and by a drop: a file larger than
/// one piece arrives whole, a name that is taken is asked about once and
/// "Keep both" keeps both, and a folder that is dropped arrives as a folder.
#[test]
#[ignore = "merge tier: a browser"]
fn files_are_uploaded_by_the_button_and_by_a_drop() {
    let s = Station::start("alpha");
    let work = home(&s).join("work");
    std::fs::create_dir_all(work.join("inbox")).unwrap();
    std::fs::write(work.join("report.txt"), b"the old report").unwrap();
    let sh = shell(&s, "scratch", &work);
    let page = Page::open(&s, 1400, 900);
    open_from_strip(&page, &s, &sh);

    // The Upload button: what the file box is given goes to the folder shown.
    press(&page, 0, "fx-upload");
    page.js(&format!(
        "(() => {{ const p = {FX}.panel, dt = new DataTransfer(); dt.items.add(new File(['picked from the device'], 'picked.txt')); dt.items.add(new File([new Uint8Array([0, 1, 2, 255])], 'four.bin')); p.input.files = dt.files; p.input.dispatchEvent(new Event('change')); return true; }})()"
    ));
    wait_file(&work.join("picked.txt"), b"picked from the device");
    wait_file(&work.join("four.bin"), &[0, 1, 2, 255]);
    shows(&page, 0, "picked.txt");

    // A drop on the listing: a file of 3.5 MiB (the first piece is 1 MiB)
    // and one whose name is taken.
    let big = pattern(3 * 1024 * 1024 + 524_288 + 17);
    page.js(&format!(
        "(() => {{ const dt = new DataTransfer(); dt.items.add(new File([{PATTERN}({})], 'big.bin')); dt.items.add(new File(['the new report'], 'report.txt')); dt.items.add(new File([], 'empty.txt'));
            const tree = {}.querySelector('.fx-tree');
            tree.dispatchEvent(new DragEvent('dragover', {{ bubbles: true, cancelable: true, dataTransfer: dt }}));
            window.__badge = document.querySelector('.fx-badge').innerText;
            tree.dispatchEvent(new DragEvent('drop', {{ bubbles: true, cancelable: true, dataTransfer: dt }})); return true; }})()",
        big.len(),
        v(0)
    ));
    assert_eq!(page.js("window.__badge"), json!("Upload to work on alpha"));
    // Asked: Replace, Keep both or Skip.
    page.wait(
        "the question",
        "document.querySelector('.dialog .fx-clash')?.innerText.includes('report.txt')",
    );
    assert_eq!(
        std::fs::read(work.join("report.txt")).unwrap(),
        b"the old report"
    );
    dialog_button(&page, "Keep both");
    wait_file(&work.join("report (2).txt"), b"the new report");
    assert_eq!(
        std::fs::read(work.join("report.txt")).unwrap(),
        b"the old report"
    );
    wait_file(&work.join("big.bin"), &big);
    wait_file(&work.join("empty.txt"), b"");
    shows(&page, 0, "report (2).txt");
    let done = page.wait(
        "the upload's row, done",
        &format!("[...{FX}.querySelectorAll('.fx-xfer[data-kind=upload][data-state=done] .fx-xfer-says')].map(e => e.innerText).find(t => t.includes('3 files'))"),
    );
    assert!(
        done.as_str().unwrap().starts_with("done: 3 files"),
        "{done}"
    );
    // No part file is left, and none was ever listed.
    let left: Vec<String> = std::fs::read_dir(&work)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".spandeck-part."))
        .collect();
    assert!(left.is_empty(), "{left:?}");

    // A drop on a folder's row goes into that folder; "Replace" replaces.
    std::fs::write(work.join("inbox/report.txt"), b"in the way").unwrap();
    page.js(&format!(
        "(() => {{ const dt = new DataTransfer(); dt.items.add(new File(['replaced'], 'report.txt'));
            {}.dispatchEvent(new DragEvent('drop', {{ bubbles: true, cancelable: true, dataTransfer: dt }})); return true; }})()",
        row(0, "inbox")
    ));
    page.wait(
        "the question",
        "!!document.querySelector('.dialog .fx-clash')",
    );
    dialog_button(&page, "Replace");
    wait_file(&work.join("inbox/report.txt"), b"replaced");

    // A folder dropped from the desktop. A test cannot hand the page a
    // real folder: these stand in for the browser's own entries, with real
    // `File`s in them. The folders are made, the empty one too.
    page.js(&format!(
        "(() => {{ const file = (name, text) => ({{ isFile: true, isDirectory: false, name, file: (ok) => ok(new File([text], name)) }});
            const dir = (name, kids) => ({{ isFile: false, isDirectory: true, name, createReader: () => {{ let given = false; return {{ readEntries: (ok) => {{ const now = given ? [] : kids; given = true; ok(now); }} }}; }} }});
            const view = {}.listing;
            {FX}.panel.uploadDropped({{ items: [{{ kind: 'file', webkitGetAsEntry: () => dir('site', [file('index.html', '<h1>hello</h1>'), dir('img', [file('a.txt', 'A')]), dir('empty', [])]) }}] }}, view, view.root); return true; }})()",
        v(0)
    ));
    wait_file(&work.join("site/index.html"), b"<h1>hello</h1>");
    wait_file(&work.join("site/img/a.txt"), b"A");
    wait_for("the empty folder", 20, || {
        work.join("site/empty").is_dir().then_some(())
    });
    shows(&page, 0, "site");

    // Files pasted from the device's clipboard, with the panel having the keyboard.
    page.js(&format!(
        "(() => {{ const dt = new DataTransfer(); dt.items.add(new File(['from the clipboard'], 'pasted.txt')); const tree = {}.querySelector('.fx-tree'); tree.focus();
            tree.dispatchEvent(new ClipboardEvent('paste', {{ bubbles: true, cancelable: true, clipboardData: dt }})); return true; }})()",
        v(0)
    ));
    wait_file(&work.join("pasted.txt"), b"from the clipboard");
}

/// Several things are selected (a click, Ctrl-click, Shift-click, Ctrl+A),
/// copied and pasted into another folder of the same station: copies. Cut
/// and paste moves. The chip says what the clipboard holds.
#[test]
#[ignore = "merge tier: a browser"]
fn several_files_are_copied_and_cut_and_pasted_on_one_station() {
    let s = Station::start("alpha");
    let work = home(&s).join("work");
    std::fs::create_dir_all(work.join("album/inner")).unwrap();
    std::fs::create_dir_all(work.join("dest")).unwrap();
    std::fs::write(work.join("album/inner/x.txt"), b"x").unwrap();
    for n in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        std::fs::write(work.join(n), n.as_bytes()).unwrap();
    }
    let sh = shell(&s, "scratch", &work);
    let page = Page::open(&s, 1400, 900);
    open_from_strip(&page, &s, &sh);
    let selected = || {
        page.js(&format!(
            "[...{}.querySelectorAll('.fx-row.sel .fx-name')].map(e => e.textContent)",
            v(0)
        ))
    };

    // A click, Shift-click to the third, Ctrl-click takes one out and
    // adds a folder.
    click(&page, "a.txt", &row(0, "a.txt"));
    click_with(&page, "to c.txt", &row(0, "c.txt"), "shiftKey: true");
    assert_eq!(selected(), json!(["a.txt", "b.txt", "c.txt"]));
    click_with(&page, "not b.txt", &row(0, "b.txt"), "ctrlKey: true");
    click_with(&page, "album too", &row(0, "album"), "metaKey: true");
    assert_eq!(selected(), json!(["album", "a.txt", "c.txt"]));
    key(&page, 0, "c", "ctrlKey: true");
    page.wait(
        "the chip",
        &format!("{FX}.querySelector('.fx-clip span')?.innerText === '3 items copied from alpha'"),
    );
    shot(&page, "selection-and-clipboard");

    // Into dest: pasted there as copies.
    double_click(&page, "dest", &row(0, "dest"));
    at(&page, 0, &work.join("dest"));
    press(&page, 0, "fx-paste");
    wait_file(&work.join("dest/a.txt"), b"a.txt");
    wait_file(&work.join("dest/c.txt"), b"c.txt");
    wait_file(&work.join("dest/album/inner/x.txt"), b"x");
    assert!(work.join("a.txt").exists() && work.join("album/inner/x.txt").exists());
    assert!(!work.join("dest/b.txt").exists());
    // The listing reads itself again when the copy ends, and the copy has a row.
    shows(&page, 0, "album");
    page.wait(
        "the copy's row",
        &format!("[...{FX}.querySelectorAll('.fx-xfer[data-kind=job]')].some(r => r.innerText.includes('Copy on alpha') && r.innerText.includes('done: 3 files'))"),
    );

    // Everything here, cut, and pasted one folder up: moved, and a name
    // that is taken there is asked about.
    key(&page, 0, "a", "ctrlKey: true");
    assert_eq!(selected(), json!(["album", "a.txt", "c.txt"]));
    press(&page, 0, "fx-cut");
    page.wait(
        "the chip",
        &format!("{FX}.querySelector('.fx-clip span')?.innerText === '3 items cut from alpha'"),
    );
    std::fs::create_dir_all(work.join("moved")).unwrap();
    press(&page, 0, "fx-nav-up");
    at(&page, 0, &work);
    shows(&page, 0, "moved");
    double_click(&page, "moved", &row(0, "moved"));
    at(&page, 0, &work.join("moved"));
    key(&page, 0, "v", "ctrlKey: true");
    wait_file(&work.join("moved/a.txt"), b"a.txt");
    wait_file(&work.join("moved/album/inner/x.txt"), b"x");
    wait_for("the originals gone", 20, || {
        (!work.join("dest/a.txt").exists()
            && !work.join("dest/c.txt").exists()
            && !work.join("dest/album").exists())
        .then_some(())
    });
    // What was cut is not on the clipboard any more.
    page.wait("no chip", &format!("!{FX}.querySelector('.fx-clip')"));
    assert_eq!(s.log("files.copied").len(), 1);
    assert_eq!(s.log("files.moved").len(), 1);
}

/// Two stations. The second view shows the other station; several files
/// selected on one are dragged onto the other's listing and arrive there as
/// copies, with a row that says so; a name that is taken is asked about; a
/// drag inside one station moves, and with Alt copies; a cut pasted on the
/// other station is a copy, and says so; and a file dropped on the other
/// station's listing is uploaded through this one, in pieces.
#[test]
#[ignore = "merge tier: a browser"]
fn a_selection_is_dragged_from_one_station_to_another() {
    let (a, b) = Station::pair();
    let from = home(&a).join("project");
    let to = home(&b);
    std::fs::create_dir_all(from.join("assets/deep")).unwrap();
    std::fs::create_dir_all(to.join("inbox")).unwrap();
    std::fs::write(from.join("assets/deep/logo.bin"), pattern(300_000)).unwrap();
    std::fs::write(from.join("one.txt"), b"one").unwrap();
    std::fs::write(from.join("two.txt"), b"two").unwrap();
    std::fs::write(from.join("three.txt"), b"three").unwrap();
    wait_for("the stations to know each other", 30, || {
        (a.sees("bravo").is_some() && b.sees("alpha").is_some()).then_some(())
    });
    let sh = shell(&a, "scratch", &from);
    let page = Page::open(&a, 1500, 1000);
    page.wait(
        "bravo, with the file explorer",
        "document.getElementById('app').api && [...document.querySelectorAll('.sidebar')].some(s => s.innerText.includes('bravo'))",
    );
    open_from_strip(&page, &a, &sh);
    at(&page, 0, &from);

    // The second view: the other station, at its home folder.
    click(
        &page,
        "Second view",
        &format!("{FX}.querySelector('.fx-second')"),
    );
    page.wait(
        "bravo in the second view",
        &format!("{}?.querySelector('.fx-station')?.value === 'bravo'", v(1)),
    );
    at(&page, 1, &to);
    shows(&page, 1, "inbox");

    // A drag the page is handed: from a row of the selection to `onto`,
    // with the keys held that `held` says. What the badge said is kept.
    let drag = |from_row: &str, onto: &str, held: &str| {
        page.js(&format!(
            "(() => {{ const dt = new DataTransfer(), src = {from_row}, onto = {onto};
                src.dispatchEvent(new DragEvent('dragstart', {{ bubbles: true, cancelable: true, dataTransfer: dt }}));
                onto.dispatchEvent(new DragEvent('dragover', {{ bubbles: true, cancelable: true, dataTransfer: dt, clientX: 900, clientY: 500, {held} }}));
                window.__badge = document.querySelector('.fx-badge').innerText;
                window.__marked = !!{FX}.querySelector('.fx-drop');
                onto.dispatchEvent(new DragEvent('drop', {{ bubbles: true, cancelable: true, dataTransfer: dt, {held} }}));
                src.dispatchEvent(new DragEvent('dragend', {{ bubbles: true }}));
                return true; }})()"
        ));
        page.js("window.__badge").as_str().unwrap().to_string()
    };

    // Three things selected on alpha, dragged onto bravo's folder "inbox".
    click(&page, "assets", &row(0, "assets"));
    click_with(&page, "one.txt", &row(0, "one.txt"), "ctrlKey: true");
    click_with(&page, "two.txt", &row(0, "two.txt"), "ctrlKey: true");
    let badge = drag(&row(0, "one.txt"), &row(1, "inbox"), "");
    assert_eq!(badge, "Copy 3 to bravo");
    assert_eq!(page.js("window.__marked"), json!(true));
    wait_file(&to.join("inbox/one.txt"), b"one");
    wait_file(&to.join("inbox/two.txt"), b"two");
    wait_file(&to.join("inbox/assets/deep/logo.bin"), &pattern(300_000));
    // Copies: the originals are where they were.
    assert!(from.join("one.txt").exists() && from.join("assets/deep/logo.bin").exists());
    assert!(!to.join("inbox/three.txt").exists());
    page.wait(
        "the copy's row, done",
        &format!("[...{FX}.querySelectorAll('.fx-xfer[data-kind=job]')].some(r => r.innerText.includes('Copy alpha to bravo') && r.innerText.includes('done: 3 files'))"),
    );
    // A drop on the listing's own ground goes to the folder shown, and
    // the listing reads itself again when the copy ends.
    click(&page, "three.txt", &row(0, "three.txt"));
    let badge = drag(
        &row(0, "three.txt"),
        &format!("{}.querySelector('.fx-tree')", v(1)),
        "",
    );
    assert_eq!(badge, "Copy 1 to bravo");
    wait_file(&to.join("three.txt"), b"three");
    shows(&page, 1, "three.txt");
    shot(&page, "second-view-after-a-drag");

    // The same again: the name is taken, and "Keep both" keeps both.
    click(&page, "three.txt", &row(0, "three.txt"));
    drag(
        &row(0, "three.txt"),
        &format!("{}.querySelector('.fx-tree')", v(1)),
        "",
    );
    page.wait(
        "the question",
        "document.querySelector('.dialog .fx-clash')?.innerText.includes('three.txt')",
    );
    shot(&page, "a-name-taken-on-the-other-station");
    dialog_button(&page, "Keep both");
    wait_file(&to.join("three (2).txt"), b"three");
    no_dialog(&page);

    // From bravo to alpha (asked of bravo, through alpha).
    std::fs::write(to.join("from-bravo.txt"), b"made on bravo").unwrap();
    shows(&page, 1, "from-bravo.txt");
    click(&page, "from-bravo.txt", &row(1, "from-bravo.txt"));
    let badge = drag(&row(1, "from-bravo.txt"), &row(0, "assets"), "");
    assert_eq!(badge, "Copy 1 to alpha");
    wait_file(&from.join("assets/from-bravo.txt"), b"made on bravo");
    assert!(to.join("from-bravo.txt").exists());

    // Inside one station a drag onto a folder moves; with Alt it copies.
    click(&page, "one.txt", &row(0, "one.txt"));
    let badge = drag(&row(0, "one.txt"), &row(0, "assets"), "altKey: true");
    assert_eq!(badge, "Copy 1");
    wait_file(&from.join("assets/one.txt"), b"one");
    assert!(from.join("one.txt").exists());
    click(&page, "two.txt", &row(0, "two.txt"));
    let badge = drag(&row(0, "two.txt"), &row(0, "assets"), "");
    assert_eq!(badge, "Move 1");
    wait_file(&from.join("assets/two.txt"), b"two");
    wait_for("two.txt moved away", 20, || {
        (!from.join("two.txt").exists()).then_some(())
    });
    shows_no(&page, 0, "two.txt");

    // A cut pasted on the other station is a copy, and the page says so.
    click(&page, "one.txt", &row(0, "one.txt"));
    press(&page, 0, "fx-cut");
    double_click(&page, "inbox", &row(1, "inbox"));
    at(&page, 1, &to.join("inbox"));
    std::fs::remove_file(to.join("inbox/one.txt")).unwrap();
    shows_no(&page, 1, "one.txt");
    press(&page, 1, "fx-paste");
    wait_file(&to.join("inbox/one.txt"), b"one");
    page.wait(
        "what was said",
        "[...document.querySelectorAll('.toast')].some(t => t.innerText.includes('between stations files are never moved'))",
    );
    assert!(from.join("one.txt").exists());

    // A file dropped on bravo's listing: uploaded through alpha, in
    // pieces of 256 KiB to begin with.
    let big = pattern(900_000);
    page.js(&format!(
        "(() => {{ const dt = new DataTransfer(); dt.items.add(new File([{PATTERN}({})], 'through.bin'));
            {}.querySelector('.fx-tree').dispatchEvent(new DragEvent('drop', {{ bubbles: true, cancelable: true, dataTransfer: dt }})); return true; }})()",
        big.len(),
        v(1)
    ));
    wait_file(&to.join("inbox/through.bin"), &big);
    shows(&page, 1, "through.bin");
    // Nothing was moved between the stations, whatever was asked.
    assert!(a.log("files.moved").len() == 1 && b.log("files.moved").is_empty());
    // A station that does not answer is said in its own listing, with
    // Retry, and the other listing goes on.
    drop(b);
    page.wait(
        "bravo said to be away, in its listing",
        &format!(
            "!!{}.querySelector('.fx-stale, .fx-fail') && !!{0}.querySelector('.fx-retry')",
            v(1)
        ),
    );
    std::fs::write(from.join("still-here.txt"), b"1").unwrap();
    shows(&page, 0, "still-here.txt");
}

/// A file's name and what it holds are text on the page (AGENTS.md rule 6):
/// markup in either runs nothing. A picture is shown only when the station
/// says it is one. A download is a ticket's link, which gives the bytes as
/// an attachment; several things come as one zip.
#[test]
#[ignore = "merge tier: a browser"]
fn names_and_previews_are_text_and_a_download_is_a_tickets_link() {
    let s = Station::start("alpha");
    let work = home(&s).join("work");
    std::fs::create_dir_all(work.join("pics")).unwrap();
    let markup = "<img src=x onerror=\"window.__ran = 1\"><script>window.__ran = 2</script>";
    let named = "<img src=x onerror=window.__named=1>.txt";
    std::fs::write(work.join("page.html"), markup).unwrap();
    std::fs::write(work.join(named), b"named").unwrap();
    std::fs::write(work.join("blob.dat"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
    std::fs::write(work.join("long.log"), "line of a long log\n".repeat(20_000)).unwrap();
    // A real PNG of one pixel, and a page that only says it is one.
    let png: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0,
        0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0d, 0x49, 0x44,
        0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0, 5, 0, 1, 0xff, 0x89, 0x99,
        0x3d, 0x1d, 0, 0, 0, 0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    std::fs::write(work.join("pics/dot.png"), png).unwrap();
    std::fs::write(work.join("pics/liar.png"), markup).unwrap();
    let sh = shell(&s, "scratch", &work);
    let page = Page::open(&s, 1400, 900);
    open_from_strip(&page, &s, &sh);

    // The name is a row's text, and no element came of it.
    shows(&page, 0, named);
    assert_eq!(
        page.js(&format!(
            "[{FX}.querySelectorAll('img, script, iframe').length, window.__named === undefined]"
        )),
        json!([0, true])
    );

    // A preview of markup: the same characters, in a box of text.
    let preview = |name: &str| {
        no_dialog(&page);
        double_click(&page, name, &row(0, name));
        page.wait(
            "the preview",
            "!!document.querySelector('.dialog .fx-preview')",
        );
    };
    let close = || {
        click(
            &page,
            "the dialog's ✕",
            "[...document.querySelectorAll('.dialog-head .icon-btn')].pop()",
        );
        no_dialog(&page);
    };
    preview("page.html");
    page.wait(
        "the file's text",
        &format!(
            "document.querySelector('.dialog .fx-text')?.textContent === {}",
            json!(markup)
        ),
    );
    shot(&page, "preview-of-markup-as-text");
    assert_eq!(
        page.js("[document.querySelectorAll('.dialog img, .dialog script').length, window.__ran === undefined, document.querySelector('.dialog h2').textContent]"),
        json!([0, true, "page.html"])
    );
    close();
    preview(named);
    page.wait(
        "the file's text",
        "document.querySelector('.dialog .fx-text')?.textContent === 'named'",
    );
    assert_eq!(
        page.js("[document.querySelectorAll('.dialog img').length, window.__named === undefined, document.querySelector('.dialog h2').textContent]"),
        json!([0, true, named])
    );
    close();
    // Not text: said, and nothing of it shown.
    preview("blob.dat");
    page.wait(
        "not text",
        "document.querySelector('.dialog .fx-preview-note')?.innerText.includes('not text')",
    );
    close();
    // A long file is cut off, and says so.
    preview("long.log");
    page.wait(
        "cut off",
        "document.querySelector('.dialog .fx-text')?.textContent.length === 262144 && document.querySelector('.dialog .fx-preview-note')?.innerText.startsWith('Cut off')",
    );
    close();
    // A picture is shown; a page named like one is not.
    double_click(&page, "pics", &row(0, "pics"));
    at(&page, 0, &work.join("pics"));
    preview("dot.png");
    page.wait(
        "the picture",
        "(i => i && i.complete && i.naturalWidth === 1 && i.src.includes('/api/files/get?ticket='))(document.querySelector('.dialog .fx-img'))",
    );
    close();
    preview("liar.png");
    page.wait(
        "no picture",
        "!document.querySelector('.dialog .fx-img') && document.querySelector('.dialog .fx-preview-note')?.innerText.includes('cannot be shown')",
    );
    assert_eq!(page.js("window.__ran === undefined"), json!(true));
    close();
    press(&page, 0, "fx-nav-up");
    at(&page, 0, &work);

    // Download, one file: a ticket's link, and what it gives.
    let fetched = |link: &str| {
        page.js(&format!(
            "fetch({}).then(async r => [r.status, r.headers.get('content-disposition'), r.headers.get('content-type'), Array.from(new Uint8Array(await r.arrayBuffer()).slice(0, 80))])",
            json!(link)
        ))
    };
    click(&page, "page.html", &row(0, "page.html"));
    press(&page, 0, "fx-download");
    let link = page.wait(
        "the download's link",
        &format!("{FX}.querySelector('.fx-xfer[data-kind=link] a.fx-link')?.getAttribute('href')"),
    );
    let link = link.as_str().unwrap();
    assert!(link.starts_with("/api/files/get?ticket="), "{link}");
    let got = fetched(link);
    assert_eq!(got[0], json!(200), "{got}");
    assert!(
        got[1].as_str().unwrap().starts_with("attachment")
            && got[1].as_str().unwrap().contains("page.html"),
        "{got}"
    );
    assert_eq!(got[2], json!("application/octet-stream"));
    assert_eq!(got[3], json!(markup.as_bytes()));
    // The link went to the browser too, and nothing ran.
    assert_eq!(page.js("window.__ran === undefined"), json!(true));
    click(
        &page,
        "the row's ✕",
        &format!("{FX}.querySelector('.fx-xfer[data-kind=link] .fx-dismiss')"),
    );

    // Several things: one zip, built at the station, then its link.
    click(&page, "pics", &row(0, "pics"));
    click_with(&page, "long.log", &row(0, "long.log"), "ctrlKey: true");
    press(&page, 0, "fx-download");
    let link = page.wait(
        "the zip's link",
        &format!("{FX}.querySelector('.fx-xfer[data-kind=link] a.fx-link')?.getAttribute('href')"),
    );
    let got = fetched(link.as_str().unwrap());
    assert_eq!(got[0], json!(200), "{got}");
    assert!(
        got[1].as_str().unwrap().starts_with("attachment")
            && got[1].as_str().unwrap().contains(".zip"),
        "{got}"
    );
    assert_eq!(
        got[3].as_array().unwrap()[0..4],
        [json!(0x50), json!(0x4b), json!(3), json!(4)],
        "{got}"
    );
    page.wait(
        "the zip's row",
        &format!("[...{FX}.querySelectorAll('.fx-xfer[data-kind=zip]')].some(r => r.innerText.includes('ready'))"),
    );

    // A file is dragged out to the desktop with a link the browser can
    // fetch (Chrome and Edge): held once the pointer has rested on its row.
    page.js(&format!(
        "{}.dispatchEvent(new MouseEvent('mouseover', {{ bubbles: true }}))",
        row(0, "long.log")
    ));
    page.wait(
        "the link, held",
        &format!(
            "!!{FX}.panel.ticketHeld('alpha', {})",
            json!(work.join("long.log"))
        ),
    );
    click(&page, "long.log", &row(0, "long.log"));
    let carried = page.js(&format!(
        "(() => {{ const dt = new DataTransfer(), r = {}; r.dispatchEvent(new DragEvent('dragstart', {{ bubbles: true, cancelable: true, dataTransfer: dt }})); r.dispatchEvent(new DragEvent('dragend', {{ bubbles: true }})); return dt.getData('DownloadURL'); }})()",
        row(0, "long.log")
    ));
    let carried = carried.as_str().unwrap();
    assert!(
        carried.starts_with("application/octet-stream:long.log:http://127.0.0.1:")
            && carried.contains("/api/files/get?ticket="),
        "{carried}"
    );
}

/// "Move agent here": the one thing in the panel that moves a pane. It
/// asks first, the pane's program is started again in the folder shown, and
/// the strip says the new folder. A refusal is shown in the station's own
/// words. (A shell stands for the agent: no agent program is started.)
#[test]
#[ignore = "merge tier: a browser"]
fn move_here_restarts_the_pane_in_the_folder_shown() {
    let s = Station::start("alpha");
    let work = home(&s).join("work");
    std::fs::create_dir_all(work.join("deep")).unwrap();
    let sh = shell(&s, "scratch", &work);
    let page = Page::open(&s, 1400, 900);
    open_from_strip(&page, &s, &sh);
    // At the pane's own folder there is nothing to move to.
    assert_eq!(
        page.js(&format!("!!{FX}.querySelector('.fx-move')")),
        json!(false)
    );
    double_click(&page, "deep", &row(0, "deep"));
    at(&page, 0, &work.join("deep"));
    click(
        &page,
        "Restart scratch here",
        &format!("{FX}.querySelector('.fx-move')"),
    );
    let asks = page.wait(
        "the question",
        "document.querySelector('.dialog')?.innerText",
    );
    let asks = asks.as_str().unwrap();
    assert!(
        asks.contains("scratch will be restarted in") && asks.contains("work/deep"),
        "{asks}"
    );
    shot(&page, "move-here-asks");
    // Nothing happened yet.
    assert_eq!(s.pane(&sh)["cwd"], json!(work));
    click(
        &page,
        "Restart here",
        "document.querySelector('.dialog .fx-move-go')",
    );
    wait_for("the pane in the new folder", 20, || {
        (s.pane(&sh)["cwd"] == json!(work.join("deep"))).then_some(())
    });
    no_dialog(&page);
    page.wait(
        "the strip's folder",
        &format!(
            "document.querySelector('.pane .strip .strip-cwd')?.getAttribute('aria-label') === {}",
            json!(format!("Folder: {}", work.join("deep").display()))
        ),
    );
    // Where the pane is now, there is nothing to move to.
    page.wait("no offer", &format!("!{FX}.querySelector('.fx-move')"));
    let moved = s.log("pane.restarted");
    assert_eq!(moved.len(), 1, "{moved:?}");
    // The shell really is there.
    s.call(
        "pane.send_text",
        json!({"pane": sh, "text": "echo in:$(basename $(pwd))-$((40+2))"}),
    );
    s.wait_text(&sh, "in:deep-42");

    // A pane of a project cannot leave its project: the station's sentence.
    let repo = s.git_repo("proj");
    s.call(
        "project.new",
        json!({"path": repo, "name": "proj", "panes": [{"name": "build", "agent": "shell"}], "open": true}),
    );
    let build = wait_for("the project's pane", 10, || {
        s.call("pane.list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == json!("build"))
            .and_then(|p| p["id"].as_str().map(String::from))
    });
    click(&page, "Close", &format!("{FX}.querySelector('.fx-close')"));
    page.wait("the panel gone", &format!("!{FX}"));
    page.goto(&format!("#/s/alpha/p/{build}"));
    page.wait(
        "the project's pane",
        &format!(
            "document.querySelector('.pane .strip .strip-cwd')?.getAttribute('aria-label') === {}",
            json!(format!("Folder: {}", repo.display()))
        ),
    );
    click(
        &page,
        "the strip's folder button",
        "document.querySelector('.pane .strip .strip-files')",
    );
    at(&page, 0, &repo);
    press(&page, 0, "fx-nav-up");
    page.wait(
        "the offer",
        &format!("{FX}.querySelector('.fx-move')?.textContent === 'Restart build here'"),
    );
    click(
        &page,
        "Restart build here",
        &format!("{FX}.querySelector('.fx-move')"),
    );
    click(
        &page,
        "Restart here",
        "document.querySelector('.dialog .fx-move-go')",
    );
    let said = page.wait(
        "the refusal",
        "document.querySelector('.dialog .fx-move-err')?.innerText",
    );
    let said = said.as_str().unwrap();
    assert!(
        said.contains("project proj") && said.contains("inside its project") && !said.contains('{'),
        "{said}"
    );
    assert_eq!(s.pane(&build)["cwd"], json!(repo));
}

/// On a phone the panel is a sheet over the whole screen, opened from the
/// folder line of a pane's bar: rows a finger can hit, a tap goes into a
/// folder, "Select" puts ticks on the rows and what to do with them at the
/// foot, and Close closes it.
#[test]
#[ignore = "merge tier: a browser"]
fn on_a_phone_the_panel_is_a_sheet_with_ticks_to_select() {
    let s = Station::start("alpha");
    let work = home(&s).join("work");
    std::fs::create_dir_all(work.join("photos")).unwrap();
    std::fs::write(work.join("photos/cat.jpg"), b"not really").unwrap();
    for n in ["a.txt", "b.txt", "notes.md", "data.json", "run.sh"] {
        std::fs::write(work.join(n), n.as_bytes()).unwrap();
    }
    let sh = shell(&s, "scratch", &work);
    let page = Page::open(&s, 390, 844);
    page.phone_screen();
    page.touches();
    page.goto(&format!("#/s/alpha/p/{sh}"));
    // The folder line of the bar opens it.
    click(
        &page,
        "the folder line",
        "document.querySelector('.chat-bar .chat-cwd.opens')",
    );
    at(&page, 0, &work);
    shows(&page, 0, "photos");
    page.wait(
        "the sheet over the whole screen",
        &format!("(r => r.left === 0 && r.top === 0 && Math.round(r.width) === innerWidth && Math.round(r.height) >= innerHeight - 1 && {FX}.querySelector('.fx-close').innerText === 'Close')({FX}.getBoundingClientRect())"),
    );
    // Rows and buttons a finger can hit; nothing runs off the side.
    let small = page.js(&format!(
        "[...{FX}.querySelectorAll('.fx-row[data-i], .fx-tools button, .fx-nav, .fx-close, .fx-station')].filter(e => getComputedStyle(e).display !== 'none' && e.getBoundingClientRect().height < 44).map(e => e.className)"
    ));
    assert_eq!(small, json!([]));
    assert_eq!(
        page.js(&format!(
            "[document.documentElement.scrollWidth <= innerWidth, [...{FX}.querySelectorAll('*')].filter(e => e.getBoundingClientRect().right > innerWidth + 1 && !e.closest('.fx-crumbs')).length]"
        )),
        json!([true, 0])
    );
    // Under a name: its size and age.
    let sub = page.js(&format!(
        "{}.querySelector('.fx-sub').innerText",
        row(0, "notes.md")
    ));
    assert!(sub.as_str().unwrap().starts_with("8 B · "), "{sub}");
    shot(&page, "phone-sheet");

    // A tap goes into a folder.
    click(&page, "photos", &row(0, "photos"));
    at(&page, 0, &work.join("photos"));
    shows(&page, 0, "cat.jpg");
    press(&page, 0, "fx-nav-up");
    at(&page, 0, &work);

    // Select: ticks, and what to do with what is ticked.
    press(&page, 0, "fx-select");
    page.wait(
        "ticks",
        &format!("{}.querySelectorAll('.fx-row .fx-tick').length === 6 && !{0}.querySelector('.fx-actbar').hidden", v(0)),
    );
    click(&page, "a.txt", &row(0, "a.txt"));
    click(&page, "b.txt", &row(0, "b.txt"));
    page.wait(
        "two ticked",
        &format!("{}.querySelector('.fx-count').innerText === '2 ticked' && {0}.querySelectorAll('.fx-tick:checked').length === 2", v(0)),
    );
    // Still here: a tap ticked, it did not open.
    assert_eq!(root(&page, 0), work.display().to_string());
    shot(&page, "phone-select");
    click(
        &page,
        "Delete",
        &format!("{}.querySelector('.fx-actbar .fx-delete')", v(0)),
    );
    wait_for("the two files in the trash", 20, || {
        (!work.join("a.txt").exists() && !work.join("b.txt").exists()).then_some(())
    });
    shows_no(&page, 0, "a.txt");
    click(
        &page,
        "Undo",
        "[...document.querySelectorAll('.toast .toast-act')].find(b => b.innerText === 'Undo')",
    );
    wait_file(&work.join("a.txt"), b"a.txt");
    press(&page, 0, "fx-select");
    page.wait(
        "no ticks",
        &format!(
            "!{}.querySelector('.fx-tick') && {0}.querySelector('.fx-actbar').hidden",
            v(0)
        ),
    );
    // A row's own "⋯" has the rest.
    click(
        &page,
        "the row's ⋯",
        &format!("{}.querySelector('.fx-dots')", row(0, "notes.md")),
    );
    page.wait(
        "the row's menu",
        "['Preview', 'Rename', 'Download', 'Copy path', 'Delete'].every(l => [...document.querySelectorAll('.menu-item')].some(i => i.innerText.startsWith(l)))",
    );
    page.js("document.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }))");
    // A tap on a file shows it.
    page.wait("no menu", "!document.querySelector('.menu')");
    click(&page, "notes.md", &row(0, "notes.md"));
    page.wait(
        "the preview",
        "document.querySelector('.dialog .fx-text')?.textContent === 'notes.md'",
    );
    click(
        &page,
        "the dialog's ✕",
        "[...document.querySelectorAll('.dialog-head .icon-btn')].pop()",
    );
    no_dialog(&page);

    // Close: the pane is back.
    click(&page, "Close", &format!("{FX}.querySelector('.fx-close')"));
    page.wait(
        "the pane again",
        &format!("!{FX} && !!document.querySelector('.chat-bar')"),
    );
    // And the pane's menu has it too.
    page.wait(
        "Files… in the pane's menu",
        "(i => i ? (i.click(), true) : (b => (b && b.click(), false))(!document.querySelector('.menu') && document.querySelector('.chat-bar .chat-more')))([...document.querySelectorAll('.menu-item')].find(i => i.innerText.startsWith('Files')))",
    );
    at(&page, 0, &work);
    assert_eq!(s.pane(&sh)["cwd"], json!(work));
}

/// The panel in a layout of two panes and under three looks: Spandeck Dark,
/// a light theme, and a theme with colours of the owner's own. It is beside
/// the panes (both get narrower, neither is covered), it takes the theme's
/// colours, and its text can be read against what is behind it. The width
/// the owner drags it to is kept.
#[test]
#[ignore = "merge tier: a browser"]
fn the_panel_stands_beside_a_layout_and_follows_every_theme() {
    let s = Station::start("alpha");
    let work = home(&s).join("work");
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::create_dir_all(work.join("docs")).unwrap();
    for (n, size) in [
        ("main.rs", 2_300),
        ("README.md", 812),
        ("logo.png", 48_211),
        ("theme.mp3", 3_400_000),
        ("clip.mp4", 12_000_000),
        ("backup.tar.gz", 880_000),
        ("paper.pdf", 120_000),
        ("config.json", 96),
        ("core", 4096),
    ] {
        std::fs::write(work.join(n), vec![b'x'; size]).unwrap();
    }
    std::os::unix::fs::symlink(work.join("docs"), work.join("link-to-docs")).unwrap();
    use std::os::unix::ffi::OsStrExt;
    std::fs::write(
        work.join(std::ffi::OsStr::from_bytes(b"odd-\xff-name.txt")),
        b"x",
    )
    .unwrap();
    let left = shell(&s, "left", &work);
    let right = shell(&s, "right", &work);
    s.call(
        "deck.layout.save",
        json!({"layout": {"name": "pair", "tree": {"dir": "row", "ratio": 500,
            "a": {"id": left, "station": "alpha"}, "b": {"id": right, "station": "alpha"}}}}),
    );
    let page = Page::open(&s, 1500, 900);
    page.goto("#/l/pair");
    page.wait(
        "both panes",
        "document.querySelectorAll('.pane .strip .strip-files').length === 2",
    );
    let cols = "[...document.querySelectorAll('.pane')].map(p => p.paneTerm.term.cols)";
    let before = page.js(cols);
    // Opened from the right-hand pane of the layout.
    click(
        &page,
        "the right pane's folder button",
        "[...document.querySelectorAll('.pane')].find(p => p.paneTerm.pane.name === 'right').querySelector('.strip-files')",
    );
    at(&page, 0, &work);
    shows(&page, 0, "main.rs");
    assert_eq!(
        page.js(&format!("{FX}.querySelector('.fx-for').innerText")),
        json!("right")
    );
    // Both panes are beside it, and both terminals have fewer columns.
    page.wait(
        "both panes narrower, neither covered",
        &format!("(edge => [...document.querySelectorAll('.pane')].every((p, i) => p.getBoundingClientRect().right <= edge + 1 && p.paneTerm.term.cols < {before}[i]))({FX}.getBoundingClientRect().left)"),
    );
    // A file whose name is not UTF-8 is listed, greyed, and cannot be chosen.
    let odd = page.js(&format!(
        "(r => r && [r.getAttribute('aria-disabled'), r.querySelector('.fx-sub').innerText, r.draggable])([...{}.querySelectorAll('.fx-row.odd')][0])",
        v(0)
    ));
    assert_eq!(
        odd,
        json!(["true", "not UTF-8: cannot be opened here", false])
    );
    // Each kind of file has its own picture.
    let kinds = page.js(&format!(
        "Object.fromEntries([...{}.querySelectorAll('.fx-row[data-i]')].map(r => [r.querySelector('.fx-name').textContent, [...r.querySelector('.fx-ico').classList].find(c => c.startsWith('k-'))]))",
        v(0)
    ));
    for (name, kind) in [
        ("src", "k-folder"),
        ("link-to-docs", "k-link"),
        ("main.rs", "k-code"),
        ("README.md", "k-text"),
        ("logo.png", "k-image"),
        ("theme.mp3", "k-audio"),
        ("clip.mp4", "k-video"),
        ("backup.tar.gz", "k-archive"),
        ("paper.pdf", "k-pdf"),
        ("config.json", "k-data"),
        ("core", "k-binary"),
    ] {
        assert_eq!(kinds[name], json!(kind), "{name}: {kinds}");
    }

    // Something selected, something on the clipboard and a finished copy,
    // so that every kind of text is on screen for each look.
    click(&page, "main.rs", &row(0, "main.rs"));
    press(&page, 0, "fx-copy");
    click(
        &page,
        "the triangle of src",
        &format!("{}?.querySelector('.fx-tri')", row(0, "src")),
    );
    double_click(&page, "docs", &row(0, "docs"));
    at(&page, 0, &work.join("docs"));
    press(&page, 0, "fx-paste");
    wait_file(&work.join("docs/main.rs"), &vec![b'x'; 2_300]);
    press(&page, 0, "fx-nav-up");
    at(&page, 0, &work);
    click(&page, "README.md", &row(0, "README.md"));
    page.wait(
        "the copy's row",
        &format!("!!{FX}.querySelector('.fx-xfer[data-kind=job][data-state=done]')"),
    );

    let check = |look: &str| {
        let seen = page.js(LOW);
        assert_eq!(seen["low"], json!([]), "{look}: text that is hard to read");
        for sel in [
            ".fx-row:not(.sel):not(.odd) .fx-name",
            ".fx-row.sel .fx-name",
            ".fx-size",
            ".fx-when",
            ".fx-seg.here",
            ".fx-tools button:not(:disabled)",
            ".fx-xfer-says",
            ".fx-clip span",
            ".fx-row.odd .fx-name",
        ] {
            assert!(
                seen["seen"].as_array().unwrap().contains(&json!(sel)),
                "{look}: {sel} was not on screen to be checked: {seen}"
            );
        }
        seen
    };
    let dark = check("Spandeck Dark");
    shot(&page, "beside-a-layout-dark");

    // A light theme.
    s.call(
        "look.set",
        json!({"level": "deck", "choice": {"theme": "spandeck-light"}}),
    );
    page.wait(
        "the light theme on the panel",
        &format!("getComputedStyle({FX}).backgroundColor === 'rgb(255, 255, 255)'"),
    );
    let light = check("Spandeck Light");
    assert_ne!(light["row"], dark["row"]);
    shot(&page, "beside-a-layout-light");

    // A theme with colours of the owner's own over it.
    s.call(
        "look.set",
        json!({"level": "deck", "choice": {"theme": "solarized-dark", "set": {"deck.bg": "#2a0f3d", "deck.panel": "#3b1a55", "deck.panel2": "#4a2569", "deck.line": "#6a3d8f", "deck.text": "#f3e9ff", "deck.muted": "#c9b3e6", "deck.blue": "#ff9de6"}}}),
    );
    page.wait(
        "the owner's colours on the panel",
        &format!("getComputedStyle({FX}).backgroundColor === 'rgb(59, 26, 85)' && getComputedStyle({FX}.querySelector('.fx-tree')).backgroundColor === 'rgb(42, 15, 61)'"),
    );
    check("Solarized Dark with the owner's colours");
    shot(&page, "beside-a-layout-custom");
    s.call(
        "look.set",
        json!({"level": "deck", "choice": {"theme": "solarized-light"}}),
    );
    page.wait(
        "Solarized Light on the panel",
        &format!("getComputedStyle({FX}).backgroundColor === 'rgb(238, 232, 213)'"),
    );
    check("Solarized Light");
    shot(&page, "beside-a-layout-solarized-light");

    // The width: dragged by its left edge, and kept for the next time.
    let grip = format!("{FX}.querySelector('.fx-grip')");
    page.js(&format!(
        "(() => {{ {grip}.dispatchEvent(new PointerEvent('pointerdown', {{ bubbles: true, cancelable: true, clientX: 1060 }})); dispatchEvent(new PointerEvent('pointermove', {{ clientX: 900 }})); dispatchEvent(new PointerEvent('pointerup', {{ clientX: 900 }})); return true; }})()"
    ));
    page.wait(
        "the panel 600 wide, and the panes beside it",
        &format!("Math.round({FX}.getBoundingClientRect().width) === 600 && [...document.querySelectorAll('.pane')].every(p => p.getBoundingClientRect().right <= 901)"),
    );
    page.reload();
    page.wait(
        "both panes",
        "document.querySelectorAll('.pane .strip .strip-files').length === 2",
    );
    click(
        &page,
        "the left pane's folder button",
        "[...document.querySelectorAll('.pane')].find(p => p.paneTerm.pane.name === 'left').querySelector('.strip-files')",
    );
    page.wait(
        "the width that was kept",
        &format!("Math.round({FX}?.getBoundingClientRect().width) === 600"),
    );

    // 5,000 files in a folder: a first part of them is drawn, and the
    // rest as the owner scrolls.
    let many = work.join("many");
    std::fs::create_dir_all(&many).unwrap();
    for i in 0..5_200 {
        std::fs::write(many.join(format!("f{i:05}.txt")), b"").unwrap();
    }
    shows(&page, 0, "many");
    double_click(&page, "many", &row(0, "many"));
    at(&page, 0, &many);
    let drawn = page.js(&format!(
        "[{}.querySelectorAll('.fx-row[data-i]').length, {0}.listing.flat.length, {0}.querySelector('.fx-more-rows')?.innerText]",
        v(0)
    ));
    assert_eq!(drawn, json!([300, 5001, "Show more (4701 below)"]));
    page.js(&format!(
        "(t => {{ t.scrollTop = t.scrollHeight; t.dispatchEvent(new Event('scroll')); return true; }})({}.querySelector('.fx-tree'))",
        v(0)
    ));
    page.wait(
        "more rows",
        &format!(
            "{}.querySelectorAll('.fx-row[data-i]').length === 600",
            v(0)
        ),
    );
    // The station stops at 5,000, and the page says how many there are.
    let note = page.js(&format!("{}.listing.flat[5000].note", v(0)));
    assert_eq!(note, json!("… 200 more not shown: this folder has 5200"));
}
