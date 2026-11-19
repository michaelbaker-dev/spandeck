//! The rules of the file explorer (DECISIONS.md, "The files of a station",
//! 19 Nov 2026), each on real folders.

use super::*;

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn s(path: &Path) -> String {
    path.display().to_string()
}

fn names(listing: &Value) -> Vec<String> {
    listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect()
}

fn parts(dir: &Path) -> Parts {
    Parts::at(dir.join("parts.log"))
}

fn put_piece(
    parts: &Parts,
    dir: &Path,
    id: &str,
    name: &str,
    at: u64,
    data: &[u8],
) -> Put<'static> {
    let _ = parts;
    // Leaked for the test's life: a `Put` borrows what a request holds.
    Put {
        id: Box::leak(id.to_string().into_boxed_str()),
        dir: Box::leak(s(dir).into_boxed_str()),
        name: Box::leak(name.to_string().into_boxed_str()),
        offset: at,
        data: Box::leak(data.to_vec().into_boxed_slice()),
        done: false,
        abort: false,
        conflict: Conflict::Fail,
        mode: None,
        mtime_ms: None,
    }
}

/// The rule: folders first, then by name without regard to case; at most
/// so many entries, and the answer says when there were more; a file
/// still being written is not shown; a link says what it leads to.
#[test]
fn a_folder_is_listed_folders_first_by_name_and_bounded() {
    let t = tmp();
    let d = t.path();
    for f in [
        "banana.txt",
        "Apple.txt",
        "cherry",
        ".hidden",
        ".spandeck-part.abc",
    ] {
        std::fs::write(d.join(f), b"x").unwrap();
    }
    std::fs::create_dir(d.join("zoo")).unwrap();
    std::fs::create_dir(d.join("Alpha")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(d.join("zoo"), d.join("link-to-zoo")).unwrap();
        std::os::unix::fs::symlink(d.join("nothing"), d.join("broken")).unwrap();
    }
    let all = list(&s(d), LIST_MAX).unwrap();
    let got = names(&all);
    #[cfg(unix)]
    assert_eq!(
        got,
        [
            "Alpha",
            "link-to-zoo",
            "zoo",
            ".hidden",
            "Apple.txt",
            "banana.txt",
            "broken",
            "cherry"
        ]
    );
    assert!(!got.iter().any(|n| n.starts_with(PART_PREFIX)));
    assert_eq!(all["truncated"], json!(false));
    assert_eq!(all["parent"], json!(s(d.parent().unwrap())));
    let of = |name: &str| {
        all["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == json!(name))
            .cloned()
            .unwrap()
    };
    assert_eq!(of("zoo")["kind"], json!("dir"));
    assert_eq!(of("Apple.txt")["kind"], json!("file"));
    assert_eq!(of("Apple.txt")["size"], json!(1));
    assert_eq!(of(".hidden")["hidden"], json!(true));
    #[cfg(unix)]
    {
        assert_eq!(of("link-to-zoo")["kind"], json!("link"));
        assert_eq!(of("link-to-zoo")["target"], json!("dir"));
        assert_eq!(of("broken")["target"], json!("missing"));
    }
    // Bounded: the first of them in the same order, and it says so.
    let some = list(&s(d), 3).unwrap();
    assert_eq!(names(&some), got[..3]);
    assert_eq!(some["truncated"], json!(true));
    assert_eq!(some["total"], json!(got.len()));
    // A path that is not absolute is not looked at.
    assert_eq!(list("relative/path", 10).unwrap_err().code(), "invalid");
}

/// Rule 2: a name has no separator of any system, no NUL, is not `.` or
/// `..`, is at most 255 bytes, and is not a part file's.
#[test]
fn a_name_is_checked_before_anything_is_touched() {
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "a\0b",
        "../etc",
        ".spandeck-part.x",
        &"x".repeat(256),
    ] {
        assert!(check_name(bad).is_err(), "{bad:?}");
    }
    for good in [
        "a",
        "a b.txt",
        "..a",
        "naïve.txt",
        "x.tar.gz",
        &"x".repeat(255),
    ] {
        assert!(check_name(good).is_ok(), "{good:?}");
    }
    let t = tmp();
    let dir = s(t.path());
    assert!(mkdir(&dir, "../up", Conflict::Fail, false).is_err());
    assert!(create(&dir, "a/b").is_err());
    assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 0);
}

/// The upload rule: pieces go to a part file beside where the file will
/// be; a piece sent twice does no harm; a piece that would leave a hole
/// is refused with what is held; and nothing has the file's name until
/// the last piece.
#[test]
fn an_upload_comes_in_pieces_and_a_piece_sent_twice_does_no_harm() {
    let t = tmp();
    let (d, reg) = (&t.path().join("up"), parts(t.path()));
    std::fs::create_dir(d).unwrap();
    let id = "upload-0123456789abcdef";
    let first = put(&reg, &put_piece(&reg, d, id, "notes.txt", 0, b"hello ")).unwrap();
    assert_eq!(first, json!({"ok": true, "received": 6}));
    assert!(!d.join("notes.txt").exists());
    assert!(
        names(&list(&s(d), 100).unwrap()).is_empty(),
        "a part is not listed"
    );
    // The second piece, and then the same piece again (its answer was lost).
    for _ in 0..2 {
        let again = put(&reg, &put_piece(&reg, d, id, "notes.txt", 6, b"wor")).unwrap();
        assert_eq!(again["received"], json!(9));
    }
    // A piece from further on than what is held.
    let gap = put(&reg, &put_piece(&reg, d, id, "notes.txt", 20, b"x")).unwrap();
    assert_eq!(
        (gap["ok"].clone(), gap["code"].clone(), gap["have"].clone()),
        (json!(false), json!("gap"), json!(9))
    );
    // A piece again from before the end, longer this time: from there on.
    let mut last = put_piece(&reg, d, id, "notes.txt", 6, b"world");
    last.done = true;
    last.mode = Some(0o640);
    last.mtime_ms = Some(1_600_000_000_000);
    let done = put(&reg, &last).unwrap();
    assert_eq!(done["path"], json!(s(&d.join("notes.txt"))));
    assert_eq!(done["size"], json!(11));
    assert_eq!(std::fs::read(d.join("notes.txt")).unwrap(), b"hello world");
    let meta = std::fs::metadata(d.join("notes.txt")).unwrap();
    assert_eq!(mtime_ms(&meta), 1_600_000_000_000);
    #[cfg(unix)]
    assert_eq!(mode_of(&meta), Some(0o640));
    assert_eq!(names(&list(&s(d), 100).unwrap()), ["notes.txt"]);
    assert!(
        reg.set.lock().unwrap().is_empty(),
        "no part is left on the list"
    );
    // An upload given up: its part is removed.
    let id2 = "upload-fedcba9876543210";
    put(&reg, &put_piece(&reg, d, id2, "other.bin", 0, b"abc")).unwrap();
    let mut abort = put_piece(&reg, d, id2, "other.bin", 0, b"");
    abort.abort = true;
    put(&reg, &abort).unwrap();
    assert_eq!(std::fs::read_dir(d).unwrap().count(), 1);
}

/// The three conflict rules of an upload: `fail` leaves what is there and
/// keeps the part, so the last piece can be sent again with a rule;
/// `keep_both` gives it a numbered name; `replace` puts it in place.
#[test]
fn a_name_that_is_taken_fails_is_replaced_or_both_are_kept() {
    let t = tmp();
    let (d, reg) = (t.path(), parts(t.path()));
    std::fs::write(d.join("report.pdf"), b"old").unwrap();
    let id = "upload-aaaaaaaaaaaaaaaa";
    let mut p = put_piece(&reg, d, id, "report.pdf", 0, b"new");
    p.done = true;
    let refused = put(&reg, &p).unwrap();
    assert_eq!(
        (refused["ok"].clone(), refused["code"].clone()),
        (json!(false), json!("exists"))
    );
    assert_eq!(std::fs::read(d.join("report.pdf")).unwrap(), b"old");
    // The end again, with nothing new, and a rule.
    let mut again = put_piece(&reg, d, id, "report.pdf", 3, b"");
    again.done = true;
    again.conflict = Conflict::KeepBoth;
    let kept = put(&reg, &again).unwrap();
    assert_eq!(kept["name"], json!("report (2).pdf"));
    assert_eq!(std::fs::read(d.join("report (2).pdf")).unwrap(), b"new");
    assert_eq!(std::fs::read(d.join("report.pdf")).unwrap(), b"old");
    // Replace.
    let mut r = put_piece(
        &reg,
        d,
        "upload-bbbbbbbbbbbbbbbb",
        "report.pdf",
        0,
        b"newest",
    );
    r.done = true;
    r.conflict = Conflict::Replace;
    assert_eq!(put(&reg, &r).unwrap()["ok"], json!(true));
    assert_eq!(std::fs::read(d.join("report.pdf")).unwrap(), b"newest");
    // The same for a folder, a new file and a new name.
    let dir = s(d);
    assert_eq!(
        mkdir(&dir, "src", Conflict::Fail, false).unwrap()["ok"],
        json!(true)
    );
    assert_eq!(
        mkdir(&dir, "src", Conflict::Fail, false).unwrap()["code"],
        json!("exists")
    );
    assert_eq!(
        mkdir(&dir, "src", Conflict::KeepBoth, false).unwrap()["name"],
        json!("src (2)")
    );
    assert_eq!(
        mkdir(&dir, "src", Conflict::Fail, true).unwrap()["existed"],
        json!(true)
    );
    assert_eq!(create(&dir, "report.pdf").unwrap()["code"], json!("exists"));
    assert_eq!(create(&dir, "empty.txt").unwrap()["ok"], json!(true));
    assert_eq!(
        rename(&d.join("empty.txt"), "report.pdf").unwrap()["code"],
        json!("exists")
    );
    assert!(d.join("empty.txt").exists());
    assert_eq!(
        rename(&d.join("empty.txt"), "full.txt").unwrap()["ok"],
        json!(true)
    );
    assert!(d.join("full.txt").exists() && !d.join("empty.txt").exists());
    assert_eq!(numbered(".bashrc", 2), ".bashrc (2)");
    assert_eq!(numbered("a.tar.gz", 3), "a.tar (3).gz");
}

/// Delete is a move to the trash, with Undo: what was trashed goes back
/// where it was; an item whose place has been taken stays in the trash
/// and is listed; the trash is emptied of what is older than a week; and
/// the home folder and spandeck's own state are never trashed.
#[test]
fn what_is_deleted_goes_to_a_trash_and_comes_back() {
    let t = tmp();
    let (home, state) = (t.path().join("home"), t.path().join("home/state"));
    let trash_dir = state.join("files/trash");
    std::fs::create_dir_all(home.join("work/sub")).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(home.join("work/sub/a.txt"), b"a").unwrap();
    std::fs::write(home.join("b.txt"), b"b").unwrap();
    let paths = vec![
        s(&home.join("work")),
        s(&home.join("b.txt")),
        s(&home.join("gone.txt")),
        s(&home),
    ];
    let out = trash(&trash_dir, &paths, "you", &home, &state).unwrap();
    assert_eq!(out["trashed"], json!([paths[0], paths[1]]));
    let codes: Vec<&str> = out["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["not_found", "refused"]);
    assert!(!home.join("work").exists() && !home.join("b.txt").exists());
    let id = out["trash"].as_str().unwrap().to_string();
    // Something new has b.txt's place: that one stays in the trash.
    std::fs::write(home.join("b.txt"), b"newer").unwrap();
    let back = restore(&trash_dir, &id).unwrap();
    assert_eq!(back["restored"], json!([paths[0]]));
    assert_eq!(back["failed"][0]["code"], json!("exists"));
    assert_eq!(back["left"], json!(1));
    assert_eq!(std::fs::read(home.join("work/sub/a.txt")).unwrap(), b"a");
    assert_eq!(std::fs::read(home.join("b.txt")).unwrap(), b"newer");
    // Out of the way, it comes back; and then the trash holds nothing.
    std::fs::remove_file(home.join("b.txt")).unwrap();
    assert_eq!(restore(&trash_dir, &id).unwrap()["left"], json!(0));
    assert_eq!(std::fs::read(home.join("b.txt")).unwrap(), b"b");
    assert_eq!(restore(&trash_dir, &id).unwrap_err().code(), "not_found");
    // Kept seven days, then removed.
    let out = trash(&trash_dir, &paths[1..2], "you", &home, &state).unwrap();
    let id = out["trash"].as_str().unwrap();
    assert_eq!(
        sweep_trash(&trash_dir, now_ms() + TRASH_KEEP_MS - 60_000, TRASH_KEEP_MS),
        0
    );
    assert!(trash_dir.join(id).exists());
    assert_eq!(
        sweep_trash(&trash_dir, now_ms() + TRASH_KEEP_MS + 60_000, TRASH_KEEP_MS),
        1
    );
    assert!(!trash_dir.join(id).exists());
    // For good, only when asked for.
    std::fs::write(home.join("c.txt"), b"c").unwrap();
    let gone = delete(
        &[s(&home.join("c.txt")), s(&home.join("work")), s(&home)],
        &home,
        &state,
    );
    assert_eq!(gone["deleted"].as_array().unwrap().len(), 2);
    assert_eq!(gone["failed"][0]["path"], json!(s(&home)));
    assert!(home.exists() && !home.join("work").exists());
}

fn tree(root: &Path) {
    std::fs::create_dir_all(root.join("src/deep")).unwrap();
    std::fs::write(root.join("src/main.rs"), b"fn main() {}").unwrap();
    std::fs::write(root.join("src/deep/data.bin"), vec![7u8; 3 << 20]).unwrap();
    std::fs::write(root.join("src/run.sh"), b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            root.join("src/run.sh"),
            std::fs::Permissions::from_mode(0o750),
        )
        .unwrap();
        std::os::unix::fs::symlink(root.join("src/main.rs"), root.join("src/link.rs")).unwrap();
        std::os::unix::fs::symlink(root.join("src/deep"), root.join("src/deeplink")).unwrap();
    }
    let old = UNIX_EPOCH + Duration::from_secs(1_500_000_000);
    std::fs::File::options()
        .write(true)
        .open(root.join("src/main.rs"))
        .unwrap()
        .set_modified(old)
        .unwrap();
}

fn run(job: &Job, reg: &Parts, paths: &[PathBuf]) -> Value {
    let nowhere = Path::new("/nonexistent-home");
    transfer_local(job, reg, paths, nowhere, nowhere);
    job.finish(None);
    job.view()
}

/// A copy walks folders, keeps permission bits and times, copies a link
/// to a file as the file, leaves out a link to a folder and says so; a
/// file that cannot be written is listed with its reason and the rest
/// carries on; and the top-level items follow the conflict rule.
#[test]
fn a_tree_is_copied_with_its_modes_and_times_and_what_fails_is_listed() {
    let t = tmp();
    let (from, to, reg) = (t.path().join("from"), t.path().join("to"), parts(t.path()));
    tree(&from);
    std::fs::create_dir_all(&to).unwrap();
    let job = Job::new(Op::Copy, None, &s(&to), Conflict::Fail);
    let v = run(&job, &reg, &[from.join("src")]);
    assert_eq!(v["state"], json!("done"), "{v}");
    assert_eq!(v["errors"], json!([]));
    assert_eq!(
        std::fs::read(to.join("src/main.rs")).unwrap(),
        b"fn main() {}"
    );
    assert_eq!(
        std::fs::metadata(to.join("src/deep/data.bin"))
            .unwrap()
            .len(),
        3 << 20
    );
    assert_eq!(v["files_done"], v["files_total"]);
    assert_eq!(v["bytes_done"], v["bytes_total"]);
    assert_eq!(v["created"], json!([s(&to.join("src"))]));
    let meta = std::fs::metadata(to.join("src/main.rs")).unwrap();
    assert_eq!(mtime_ms(&meta), 1_500_000_000_000);
    #[cfg(unix)]
    {
        let run_sh = std::fs::metadata(to.join("src/run.sh")).unwrap();
        assert_eq!(mode_of(&run_sh), Some(0o750));
        // The link to a file is a file now; the link to a folder is not there.
        let link = std::fs::symlink_metadata(to.join("src/link.rs")).unwrap();
        assert!(link.is_file());
        assert!(!to.join("src/deeplink").exists());
        assert_eq!(v["skipped"][0]["code"], json!("link"));
        assert_eq!(v["files_done"], json!(4));
    }
    assert!(reg.set.lock().unwrap().is_empty(), "no part file is left");
    // The name is taken now: fail lists it, skip passes over it, and
    // keep both makes "src (2)".
    let v = run(
        &Job::new(Op::Copy, None, &s(&to), Conflict::Fail),
        &reg,
        &[from.join("src")],
    );
    assert_eq!(v["errors"][0]["code"], json!("exists"));
    let v = run(
        &Job::new(Op::Copy, None, &s(&to), Conflict::Skip),
        &reg,
        &[from.join("src")],
    );
    assert_eq!(
        (v["errors"].clone(), v["skipped"][0]["code"].clone()),
        (json!([]), json!("exists"))
    );
    let v = run(
        &Job::new(Op::Copy, None, &s(&to), Conflict::KeepBoth),
        &reg,
        &[from.join("src")],
    );
    assert_eq!(v["created"], json!([s(&to.join("src (2)"))]));
    assert!(to.join("src (2)/deep/data.bin").exists());
    // Replace: the folder is added to and same-named files replaced. One
    // file cannot be: a folder has its name. It is listed; the rest is done.
    std::fs::write(to.join("src/main.rs"), b"changed here").unwrap();
    std::fs::write(to.join("src/only-here.txt"), b"kept").unwrap();
    std::fs::remove_file(to.join("src/run.sh")).unwrap();
    std::fs::create_dir(to.join("src/run.sh")).unwrap();
    let v = run(
        &Job::new(Op::Copy, None, &s(&to), Conflict::Replace),
        &reg,
        &[from.join("src")],
    );
    assert_eq!(v["state"], json!("done"));
    assert_eq!(v["errors"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["errors"][0]["path"], json!(s(&from.join("src/run.sh"))));
    assert_eq!(
        std::fs::read(to.join("src/main.rs")).unwrap(),
        b"fn main() {}"
    );
    assert_eq!(
        std::fs::read(to.join("src/only-here.txt")).unwrap(),
        b"kept"
    );
    // A copy into the folder it is in is a second copy beside it, and a
    // folder does not go into itself.
    let v = run(
        &Job::new(Op::Copy, None, &s(&from), Conflict::Fail),
        &reg,
        &[from.join("src")],
    );
    assert_eq!(v["created"], json!([s(&from.join("src (2)"))]));
    let inside = s(&from.join("src/deep"));
    let v = run(
        &Job::new(Op::Copy, None, &inside, Conflict::Fail),
        &reg,
        &[from.join("src")],
    );
    assert_eq!(v["errors"][0]["code"], json!("into_itself"));
    // A move takes it away from where it was.
    let v = run(
        &Job::new(Op::Move, None, &s(&to), Conflict::KeepBoth),
        &reg,
        &[from.join("src (2)")],
    );
    assert_eq!(v["created"], json!([s(&to.join("src (2) (2)"))]), "{v}");
    assert!(!from.join("src (2)").exists());
    assert!(to.join("src (2) (2)/deep/data.bin").exists());
    // And a move into a folder that is there (replace) adds to it and
    // then removes the source.
    std::fs::create_dir_all(from.join("more/src")).unwrap();
    std::fs::write(from.join("more/src/new.txt"), b"n").unwrap();
    let v = run(
        &Job::new(Op::Move, None, &s(&to), Conflict::Replace),
        &reg,
        &[from.join("more/src")],
    );
    assert_eq!(v["errors"], json!([]), "{v}");
    assert!(to.join("src/new.txt").exists() && to.join("src/only-here.txt").exists());
    assert!(!from.join("more/src").exists());
}

/// A job can be cancelled: it stops between pieces, says so, and leaves
/// no part file behind.
#[test]
fn a_cancelled_copy_stops_and_leaves_no_part_file() {
    let t = tmp();
    let (from, to, reg) = (t.path().join("from"), t.path().join("to"), parts(t.path()));
    tree(&from);
    std::fs::create_dir_all(&to).unwrap();
    let job = Job::new(Op::Copy, None, &s(&to), Conflict::Fail);
    job.cancel.store(true, Ordering::Relaxed);
    let v = run(&job, &reg, &[from.join("src")]);
    assert_eq!(v["state"], json!("cancelled"));
    assert_eq!(v["files_done"], json!(0));
    assert_eq!(std::fs::read_dir(&to).unwrap().count(), 0);
    assert!(reg.set.lock().unwrap().is_empty());
}

/// No part file is left for good: one nothing has written to for a day
/// is removed, across a restart of the station (the list is a file).
#[test]
fn a_part_file_nothing_writes_to_is_removed() {
    let t = tmp();
    let d = t.path();
    {
        let reg = parts(d);
        put(
            &reg,
            &put_piece(&reg, d, "upload-cccccccccccccccc", "big.iso", 0, b"abc"),
        )
        .unwrap();
    }
    let part = d.join(".spandeck-part.upload-cccccccccccccccc");
    assert!(part.exists());
    // The station was started again: the list is read from its file.
    let reg = parts(d);
    assert_eq!(reg.sweep(now_ms() + PART_IDLE_MS - 60_000, PART_IDLE_MS), 0);
    assert!(part.exists());
    assert_eq!(reg.sweep(now_ms() + PART_IDLE_MS + 60_000, PART_IDLE_MS), 1);
    assert!(!part.exists());
    assert!(parts(d).set.lock().unwrap().is_empty());
}

fn zip_of(dir: &Path, paths: &[PathBuf]) -> (Zip, std::result::Result<(), (&'static str, String)>) {
    let zip = Zip {
        id: "z1".into(),
        name: zip_name(paths),
        path: dir.join("out.zip"),
        cancel: AtomicBool::new(false),
        st: Mutex::new(ZipProgress {
            state: "building",
            ..Default::default()
        }),
    };
    let out = zip_build(&zip, paths);
    (zip, out)
}

/// Several files and folders are downloaded as one zip. It must be a zip
/// other programs read: checked with Python's zipfile or `unzip` where
/// the machine has one, and always by reading it back here.
#[test]
fn a_zip_of_files_and_folders_is_one_other_programs_can_read() {
    let t = tmp();
    let from = t.path().join("from");
    tree(&from);
    std::fs::write(from.join("ünï.txt"), b"accents").unwrap();
    let (zip, out) = zip_of(t.path(), &[from.join("src"), from.join("ünï.txt")]);
    out.unwrap();
    assert_eq!(zip.name, "files.zip");
    let bytes = std::fs::read(&zip.path).unwrap();
    assert_eq!(zip.st.lock().unwrap().size, bytes.len() as u64);
    // Read back: the end record, then every central entry and its bytes.
    let end = bytes.len() - 22;
    assert_eq!(&bytes[end..end + 4], &0x0605_4b50u32.to_le_bytes());
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let (count, mut at) = (u16_at(end + 10), u32_at(end + 16));
    let mut found = BTreeMap::new();
    for _ in 0..count {
        assert_eq!(u32_at(at), 0x0201_4b50);
        let (crc, size, name_len, extra_len) = (
            u32_at(at + 16),
            u32_at(at + 24),
            u16_at(at + 28),
            u16_at(at + 30),
        );
        let local = u32_at(at + 42);
        let name = String::from_utf8(bytes[at + 46..at + 46 + name_len].to_vec()).unwrap();
        let data = local + 30 + u16_at(local + 26) + u16_at(local + 28);
        assert_eq!(
            crc32fast::hash(&bytes[data..data + size]) as usize,
            crc,
            "{name}"
        );
        found.insert(name, bytes[data..data + size].to_vec());
        at += 46 + name_len + extra_len;
    }
    assert_eq!(found["src/main.rs"], b"fn main() {}");
    assert_eq!(found["src/deep/data.bin"].len(), 3 << 20);
    assert_eq!(found["ünï.txt"], b"accents");
    assert!(found.contains_key("src/") && found.contains_key("src/deep/"));
    #[cfg(unix)]
    assert!(found.contains_key("src/link.rs") && !found.keys().any(|k| k.contains("deeplink")));
    // And by a program that is not this one.
    let ran = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .arg(&zip.path)
            .output()
            .ok()
    };
    let script = "import sys, zipfile; z = zipfile.ZipFile(sys.argv[1]); bad = z.testzip(); \
                  assert bad is None, bad; print(len(z.read('src/deep/data.bin')), z.read('src/main.rs').decode())";
    if let Some(out) = ran("python3", &["-c", script]) {
        let said = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(said.trim(), format!("{} fn main() {{}}", 3 << 20));
    }
    if let Some(out) = ran("unzip", &["-tq"]) {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    // A single folder's zip has its name, and a time in a header is the
    // one it says.
    assert_eq!(zip_name(&[from.join("src")]), "src.zip");
    assert_eq!(
        dos_time(1_500_000_000),
        ((2 << 11) | (40 << 5), (37 << 9) | (7 << 5) | 14)
    );
}

/// A zip is bounded, since it is written whole before it is sent and no
/// zip64 is written: over the bound it is refused, saying what it would
/// have been; and one being built can be cancelled.
#[test]
fn a_zip_being_built_can_be_cancelled() {
    let t = tmp();
    let from = t.path().join("from");
    tree(&from);
    let zip = Zip {
        id: "z2".into(),
        name: "x.zip".into(),
        path: t.path().join("x.zip"),
        cancel: AtomicBool::new(true),
        st: Mutex::new(ZipProgress::default()),
    };
    assert_eq!(
        zip_build(&zip, &[from.join("src")]).unwrap_err().0,
        "cancelled"
    );
}
