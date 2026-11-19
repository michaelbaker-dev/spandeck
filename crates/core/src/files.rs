//! The files of a station, for the Control Deck's file explorer (the
//! owner's request of 19 Nov 2026; DECISIONS.md, "The files of a station").
//!
//! Every method here is the owner's alone (rpc.rs): a file can hold a
//! secret. Another station reaches them only as the owner's forwarded
//! request. What can be reached is what the station's own user can reach;
//! paths are absolute; a name is checked and is never interpreted.
//!
//! Three things take long and are jobs or pieces, so that nothing waits
//! for them: an upload comes in pieces (`files.put`), a copy or a move is
//! a job that is answered at once (`files.transfer`), and so is one zip
//! of several files (`files.zip`). Whatever writes a file writes a part
//! file beside where it is going and renames it into place.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{json, Value};

use crate::daemon::{invalid, Daemon, Error, Result};
use crate::paths::{new_id, now_ms, random_token};

/// Said in `station.info`: this station has the `files.` methods.
pub const CAP: &str = "files";

/// Entries of one folder in one answer.
pub const LIST_MAX: usize = 5000;
/// Names read of one folder before it is sorted; past it, the rest is
/// not looked at.
const LIST_SCAN: usize = 100_000;
/// The most bytes of a file in one call, either way.
pub const PIECE_MAX: u64 = 4 << 20;
/// A file being written, beside where it will be: `.spandeck-part.<id>`.
pub const PART_PREFIX: &str = ".spandeck-part.";
/// A part file nothing has been written to for this long is removed.
pub const PART_IDLE_MS: u64 = 24 * 3600 * 1000;
/// What is in the trash is removed after this.
pub const TRASH_KEEP_MS: u64 = 7 * 24 * 3600 * 1000;
/// A finished job is listed this long, and a finished upload's id kept.
const DONE_KEEP_MS: u64 = 10 * 60 * 1000;
/// A zip nobody has read from for this long is removed.
pub const ZIP_IDLE_MS: u64 = 10 * 60 * 1000;
/// One zip: no zip64 is written, so under 4 GiB with its headers, and
/// under the 65,535 entries the format counts.
pub const ZIP_MAX_BYTES: u64 = 3584 << 20;
pub const ZIP_MAX_ENTRIES: u64 = 20_000;
/// Jobs (copies, moves, zips) running at once on one station.
const JOBS_MAX: usize = 8;
/// Paths in one call.
const PATHS_MAX: usize = 1000;
/// A download's ticket is taken this long after it was given.
const TICKET_MS: u64 = 60_000;
/// One piece sent to another station, and how long it may take there.
const SEND_START: usize = 256 * 1024;
const SEND_MIN: usize = 32 * 1024;
const SEND_MAX: usize = 1024 * 1024;
pub const BULK_LIMIT: Duration = Duration::from_secs(30);

// ------------------------------------------------------------ names, paths

/// A file's or a folder's own name, as a caller gives one: no separator
/// of any system, no NUL, not `.` or `..`, at most 255 bytes, and not the
/// name of a part file.
pub fn check_name(name: &str) -> Result<()> {
    let bad = |why: &str| Err(invalid(format!("{name:?} cannot be a name: {why}")));
    if name.is_empty() {
        return Err(invalid("a name cannot be empty"));
    }
    if name.len() > 255 {
        return Err(invalid("a name is at most 255 bytes"));
    }
    if name == "." || name == ".." {
        return bad("it means a folder, not a name");
    }
    if name.contains(['/', '\\']) {
        return bad("it has a separator in it");
    }
    if name.contains('\0') {
        return bad("it has a NUL in it");
    }
    if name.starts_with(PART_PREFIX) {
        return bad("names that begin so are kept for files still being written");
    }
    Ok(())
}

/// A path as a caller gives one: absolute, with no NUL.
pub fn abs(path: &str) -> Result<PathBuf> {
    let p = Path::new(path);
    if path.contains('\0') || !p.is_absolute() {
        return Err(invalid(format!(
            "{path:?} is not an absolute path on this station"
        )));
    }
    Ok(p.to_path_buf())
}

fn shown(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// An error of the operating system, with what it was about.
fn io_err(path: &Path, e: std::io::Error) -> Error {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => Error::NotFound(shown(path)),
        K::PermissionDenied => Error::Denied(format!(
            "this station's user may not do that to {}",
            shown(path)
        )),
        _ => Error::Other(anyhow::anyhow!("{}: {e}", shown(path))),
    }
}

fn refused(code: &str, message: impl Into<String>) -> Value {
    json!({"ok": false, "code": code, "message": message.into()})
}

fn is_part(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with(PART_PREFIX)
}

fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

fn mode_of(meta: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// The permission bits, where the system has them. Never more than them.
fn set_mode(path: &Path, mode: Option<u32>) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = mode {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

fn set_mtime(path: &Path, at: Option<SystemTime>) {
    if let Some(at) = at {
        let _ = std::fs::File::open(path).and_then(|f| f.set_modified(at));
    }
}

fn hidden(name: &str, meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 2 != 0 {
            return true;
        }
    }
    let _ = meta;
    name.starts_with('.')
}

fn kind_of(ft: std::fs::FileType) -> &'static str {
    if ft.is_symlink() {
        "link"
    } else if ft.is_dir() {
        "dir"
    } else if ft.is_file() {
        "file"
    } else {
        "other"
    }
}

/// One entry of a folder, as the explorer shows it. A link says what it
/// leads to (`target`: dir, file, other or missing) and has the size and
/// the time of that.
fn entry(path: &Path, name: &std::ffi::OsStr) -> Option<Value> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let kind = kind_of(meta.file_type());
    let text = name.to_string_lossy();
    let mut v = json!({
        "name": text,
        "kind": kind,
        "size": if meta.is_file() { meta.len() } else { 0 },
        "mtime_ms": mtime_ms(&meta),
        "hidden": hidden(&text, &meta),
    });
    if let Some(mode) = mode_of(&meta) {
        v["mode"] = json!(mode & 0o777);
    }
    if kind == "link" {
        match std::fs::metadata(path) {
            Ok(to) => {
                v["target"] = json!(kind_of(to.file_type()));
                v["size"] = json!(if to.is_file() { to.len() } else { 0 });
                v["mtime_ms"] = json!(mtime_ms(&to));
            }
            Err(_) => v["target"] = json!("missing"),
        }
    }
    // A name that is not UTF-8 cannot be said in a request: it is shown,
    // marked, and reached only as part of its folder.
    if name.to_str().is_none() {
        v["odd_name"] = json!(true);
    }
    Some(v)
}

/// `files.stat`.
pub fn stat(path: &str) -> Result<Value> {
    let p = abs(path)?;
    let name = p
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| p.as_os_str().to_os_string());
    let mut v = entry(&p, &name).ok_or_else(|| Error::NotFound(path.to_string()))?;
    v["path"] = json!(path);
    v["parent"] = json!(p.parent().map(shown));
    Ok(v)
}

/// `files.list`: folders first, then by name without regard to case; at
/// most `max` entries, and `truncated` says there were more.
pub fn list(path: &str, max: usize) -> Result<Value> {
    let dir = abs(path)?;
    let rd = std::fs::read_dir(&dir).map_err(|e| io_err(&dir, e))?;
    let mut names: Vec<(bool, String, OsString)> = vec![];
    let mut more = false;
    for e in rd.flatten() {
        let name = e.file_name();
        if is_part(&name) {
            continue;
        }
        if names.len() >= LIST_SCAN {
            more = true;
            break;
        }
        // A link to a folder is among the folders.
        let is_dir = match e.file_type() {
            Ok(ft) if ft.is_symlink() => std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir()),
            Ok(ft) => ft.is_dir(),
            Err(_) => false,
        };
        names.push((!is_dir, name.to_string_lossy().to_lowercase(), name));
    }
    names.sort();
    let total = names.len();
    let entries: Vec<Value> = names
        .iter()
        .take(max)
        .filter_map(|(_, _, name)| entry(&dir.join(name), name))
        .collect();
    Ok(json!({
        "path": path,
        "parent": dir.parent().map(shown),
        "entries": entries,
        "total": total,
        "truncated": more || total > max,
    }))
}

/// The places a listing can start from: `/`, or the drives of a Windows
/// machine.
pub fn roots() -> Vec<String> {
    if !cfg!(windows) {
        return vec!["/".to_string()];
    }
    ('C'..='Z')
        .map(|d| format!("{d}:\\"))
        .filter(|d| Path::new(d).is_dir())
        .collect()
}

/// `files.read`: `len` bytes from `offset` of a file.
pub fn read(path: &Path, offset: u64, len: u64) -> Result<Value> {
    let meta = std::fs::metadata(path).map_err(|e| io_err(path, e))?;
    if !meta.is_file() {
        return Err(invalid(format!("{} is not a file", shown(path))));
    }
    let size = meta.len();
    let len = len.min(PIECE_MAX).min(size.saturating_sub(offset));
    let mut bytes = vec![0u8; len as usize];
    let mut file = std::fs::File::open(path).map_err(|e| io_err(path, e))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| io_err(path, e))?;
    let mut got = 0;
    while got < bytes.len() {
        match file.read(&mut bytes[got..]).map_err(|e| io_err(path, e))? {
            0 => break,
            n => got += n,
        }
    }
    bytes.truncate(got);
    Ok(json!({
        "data_b64": base64::engine::general_purpose::STANDARD.encode(&bytes),
        "offset": offset,
        "size": size,
        "mtime_ms": mtime_ms(&meta),
        "eof": offset + got as u64 >= size,
    }))
}

/// What may not be deleted, trashed, renamed or moved away through here:
/// a root, the home folder, and anything spandeck's own state is in.
fn guarded(path: &Path, home: &Path, state: &Path) -> Option<&'static str> {
    if path.parent().is_none() {
        return Some("it is the root of a disk");
    }
    // A link is the link: removing it removes nothing it leads to.
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return None;
    }
    let p = real(path);
    if p == real(home) {
        return Some("it is the home folder");
    }
    if real(state).starts_with(&p) {
        return Some("spandeck's own state is in it");
    }
    None
}

/// A pane this station lists (running or ended, not closed) and the
/// folder it was started in or moved to. Where a shell has since `cd`'d is
/// not known to the station.
#[derive(Debug, Clone)]
pub struct Busy {
    pub id: String,
    pub name: String,
    pub project: Option<String>,
    pub ended: bool,
    cwd: PathBuf,
    real: PathBuf,
}

impl Busy {
    pub fn new(id: &str, name: &str, project: Option<&str>, ended: bool, cwd: &str) -> Busy {
        let cwd = PathBuf::from(cwd);
        Busy {
            id: id.to_string(),
            name: name.to_string(),
            project: project.map(String::from),
            ended,
            real: real(&cwd),
            cwd,
        }
    }
}

/// The panes whose folder is `path` or below it. Nothing in use is
/// trashed, deleted, renamed or moved away (the owner, 19 Nov 2026), and
/// nothing forces it: the owner closes those panes first. Paths are
/// compared a part at a time, so `/a/b` in use does not hold `/a/bc`.
fn in_use<'a>(path: &Path, busy: &'a [Busy]) -> Vec<&'a Busy> {
    // A link is the link: a pane is in its way only if the pane's folder
    // is written through it.
    let link = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    let p = real(path);
    busy.iter()
        .filter(|b| b.cwd.starts_with(path) || (!link && b.real.starts_with(&p)))
        .collect()
}

/// "builder and critic (project tally), and sh1 (ended, not closed)".
fn busy_names(who: &[&Busy]) -> String {
    let mut groups: Vec<(Option<&str>, Vec<String>)> = vec![];
    for b in who {
        let name = match b.ended {
            true => format!("{} (ended, not closed)", b.name),
            false => b.name.clone(),
        };
        match groups.iter_mut().find(|(p, _)| *p == b.project.as_deref()) {
            Some((_, names)) => names.push(name),
            None => groups.push((b.project.as_deref(), vec![name])),
        }
    }
    let and = |list: &[String], last: &str| match list {
        [] => String::new(),
        [one] => one.clone(),
        [most @ .., end] => format!("{}{last}{end}", most.join(", ")),
    };
    let groups: Vec<String> = groups
        .iter()
        .map(|(project, names)| match project {
            Some(p) => format!("{} (project {p})", and(names, " and ")),
            None => and(names, " and "),
        })
        .collect();
    and(&groups, ", and ")
}

/// The refusal for something in use, as an item of `failed` or of a job's
/// `errors`: the page tells it by `in_use` and names the `panes`.
fn in_use_item(path: &str, who: &[&Busy]) -> Value {
    let verb = match who {
        [one] if one.ended => "is still open",
        [_] => "is working",
        _ => "are working",
    };
    json!({
        "path": path,
        "code": "in_use",
        "message": format!(
            "{} {verb} in {path} or a folder inside it. Close {} first",
            busy_names(who),
            if who.len() == 1 { "that pane" } else { "those panes" },
        ),
        "panes": who.iter().map(|b| json!({
            "id": b.id, "name": b.name, "project": b.project, "ended": b.ended,
        })).collect::<Vec<_>>(),
    })
}

/// `files.rename`'s refusal for a folder in use, or None.
pub fn rename_in_use(path: &Path, busy: &[Busy]) -> Option<Value> {
    let who = in_use(path, busy);
    if who.is_empty() {
        return None;
    }
    let mut v = in_use_item(&shown(path), &who);
    v["ok"] = json!(false);
    Some(v)
}

// -------------------------------------------------------------- conflicts

/// What to do when the name is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    Fail,
    Replace,
    KeepBoth,
    Skip,
}

impl Conflict {
    fn parse(v: Option<&str>, skip: bool) -> Result<Conflict> {
        match v {
            None | Some("fail") => Ok(Conflict::Fail),
            Some("replace") => Ok(Conflict::Replace),
            Some("keep_both") => Ok(Conflict::KeepBoth),
            Some("skip") if skip => Ok(Conflict::Skip),
            Some(other) => Err(invalid(format!(
                "conflict is fail, replace{} or keep_both, not {other:?}",
                if skip { ", skip" } else { "" }
            ))),
        }
    }

    fn word(self) -> &'static str {
        match self {
            Conflict::Fail | Conflict::Skip => "fail",
            Conflict::Replace => "replace",
            Conflict::KeepBoth => "keep_both",
        }
    }
}

/// "name (2).ext", "name (3).ext", …
fn numbered(name: &str, n: u32) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(at) if at > 0 => name.split_at(at),
        _ => (name, ""),
    };
    let tail = format!(" ({n}){ext}");
    let mut keep = stem.len().min(255usize.saturating_sub(tail.len()));
    while !stem.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}{tail}", &stem[..keep])
}

fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// The first of "name", "name (2)", … that nothing has.
fn free_name(dir: &Path, name: &str) -> Option<String> {
    (1..10_000u32)
        .map(|n| match n {
            1 => name.to_string(),
            n => numbered(name, n),
        })
        .find(|n| !exists(&dir.join(n)))
}

/// Give a finished part file the name `to`, only if nothing has it.
/// A link is made and the part removed, which no other writer can get in
/// the way of; where the disk has no links, it is looked at and renamed.
fn place_new(part: &Path, to: &Path) -> std::io::Result<bool> {
    match std::fs::hard_link(part, to) {
        Ok(()) => {
            let _ = std::fs::remove_file(part);
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(_) if exists(to) => Ok(false),
        Err(_) => std::fs::rename(part, to).map(|()| true),
    }
}

/// Where a finished part file went, or that the name is taken.
fn place(part: &Path, dir: &Path, name: &str, conflict: Conflict) -> Result<Option<PathBuf>> {
    let to = dir.join(name);
    match conflict {
        Conflict::Fail | Conflict::Skip => Ok(place_new(part, &to)
            .map_err(|e| io_err(&to, e))?
            .then_some(to)),
        Conflict::KeepBoth => {
            for n in 1..10_000u32 {
                let to = dir.join(match n {
                    1 => name.to_string(),
                    n => numbered(name, n),
                });
                if place_new(part, &to).map_err(|e| io_err(&to, e))? {
                    return Ok(Some(to));
                }
            }
            Ok(None)
        }
        Conflict::Replace => {
            if std::fs::symlink_metadata(&to).is_ok_and(|m| m.is_dir()) {
                return Ok(None);
            }
            std::fs::rename(part, &to).map_err(|e| io_err(&to, e))?;
            Ok(Some(to))
        }
    }
}

// ------------------------------------------------------------- part files

/// The part files this station has begun and not finished, so that none
/// is left for good: kept in memory and in a file that is only added to
/// (a line per change), read again at the start.
pub struct Parts {
    log: PathBuf,
    set: Mutex<BTreeSet<String>>,
}

impl Parts {
    pub fn at(log: PathBuf) -> Parts {
        let mut set = BTreeSet::new();
        for line in std::fs::read_to_string(&log).unwrap_or_default().lines() {
            let (sign, rest) = line.split_at(line.len().min(1));
            let Ok(path) = serde_json::from_str::<String>(rest) else {
                continue;
            };
            match sign {
                "+" => set.insert(path),
                _ => set.remove(&path),
            };
        }
        Parts {
            log,
            set: Mutex::new(set),
        }
    }

    fn note(&self, sign: char, path: &Path) {
        let Some(text) = path.to_str() else { return };
        let mut set = self.set.lock().unwrap();
        let changed = match sign {
            '+' => set.insert(text.to_string()),
            _ => set.remove(text),
        };
        if !changed {
            return;
        }
        if let Some(dir) = self.log.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let line = format!("{sign}{}\n", json!(text));
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }

    fn add(&self, path: &Path) {
        self.note('+', path);
    }

    fn done(&self, path: &Path) {
        self.note('-', path);
    }

    /// Remove every part file nothing was written to for `idle_ms`, forget
    /// those that are gone, and write the list afresh.
    pub fn sweep(&self, now_ms: u64, idle_ms: u64) -> usize {
        let mut set = self.set.lock().unwrap();
        let before = set.len();
        set.retain(|path| match std::fs::symlink_metadata(path) {
            Ok(meta) if now_ms.saturating_sub(mtime_ms(&meta)) > idle_ms => {
                let _ = std::fs::remove_file(path);
                false
            }
            Ok(_) => true,
            Err(_) => false,
        });
        let text: String = set.iter().map(|p| format!("+{}\n", json!(p))).collect();
        if let Some(dir) = self.log.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.log, text);
        before - set.len()
    }
}

// ----------------------------------------------------------------- upload

/// One call of `files.put`.
pub struct Put<'a> {
    pub id: &'a str,
    pub dir: &'a str,
    pub name: &'a str,
    pub offset: u64,
    pub data: &'a [u8],
    pub done: bool,
    pub abort: bool,
    pub conflict: Conflict,
    pub mode: Option<u32>,
    pub mtime_ms: Option<u64>,
}

fn valid_id(id: &str) -> bool {
    (16..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A piece of an upload. The piece is written to the part file at its
/// offset, and the part ends where the piece ends: a piece sent a second
/// time writes the same bytes to the same place. A piece that would leave
/// a hole is refused with how much is held (`gap`). With `done` the part
/// is given its name, by the conflict rule; when the name is taken and
/// the rule is `fail`, the part stays, and the last piece (or an empty
/// one at the end) can be sent again with another rule.
pub fn put(parts: &Parts, p: &Put) -> Result<Value> {
    if !valid_id(p.id) {
        return Err(invalid(
            "an upload's id is 16 to 64 letters, digits, - or _",
        ));
    }
    check_name(p.name)?;
    let dir = abs(p.dir)?;
    let part = dir.join(format!("{PART_PREFIX}{}", p.id));
    if p.abort {
        let _ = std::fs::remove_file(&part);
        parts.done(&part);
        return Ok(json!({"ok": true, "aborted": true}));
    }
    if p.data.len() as u64 > PIECE_MAX {
        return Err(invalid("a piece is at most 4 MiB"));
    }
    let meta = std::fs::metadata(&dir).map_err(|e| io_err(&dir, e))?;
    if !meta.is_dir() {
        return Err(invalid(format!("{} is not a folder", p.dir)));
    }
    let have = std::fs::symlink_metadata(&part).map(|m| m.len()).ok();
    if p.offset > have.unwrap_or(0) {
        let have = have.unwrap_or(0);
        let mut v = refused(
            "gap",
            format!(
                "this station holds {have} bytes of that upload, and the piece begins at {}",
                p.offset
            ),
        );
        v["have"] = json!(have);
        return Ok(v);
    }
    if have.is_none() {
        parts.add(&part);
    }
    let end = p.offset + p.data.len() as u64;
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&part)?;
        f.seek(SeekFrom::Start(p.offset))?;
        f.write_all(p.data)?;
        f.set_len(end)?;
        if p.done {
            if let Some(ms) = p.mtime_ms {
                let _ = f.set_modified(UNIX_EPOCH + Duration::from_millis(ms));
            }
        }
        Ok(())
    };
    write().map_err(|e| io_err(&part, e))?;
    if !p.done {
        return Ok(json!({"ok": true, "received": end}));
    }
    set_mode(&part, p.mode);
    match place(&part, &dir, p.name, p.conflict)? {
        Some(to) => {
            parts.done(&part);
            let name = to.file_name().map(|n| n.to_string_lossy().into_owned());
            Ok(json!({"ok": true, "done": true, "path": shown(&to), "name": name, "size": end}))
        }
        None => {
            let mut v = refused(
                "exists",
                format!("{} already has something called {}", p.dir, p.name),
            );
            v["received"] = json!(end);
            Ok(v)
        }
    }
}

/// `files.mkdir`. `merge`: a folder that is already there is the answer.
pub fn mkdir(dir: &str, name: &str, conflict: Conflict, merge: bool) -> Result<Value> {
    check_name(name)?;
    let parent = abs(dir)?;
    let made = |to: PathBuf, was_there: bool| {
        let name = to.file_name().map(|n| n.to_string_lossy().into_owned());
        json!({"ok": true, "path": shown(&to), "name": name, "existed": was_there})
    };
    for n in 1..10_000u32 {
        let to = parent.join(match n {
            1 => name.to_string(),
            n => numbered(name, n),
        });
        match std::fs::create_dir(&to) {
            Ok(()) => return Ok(made(to, false)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if merge && std::fs::symlink_metadata(&to).is_ok_and(|m| m.is_dir()) {
                    return Ok(made(to, true));
                }
                if conflict != Conflict::KeepBoth {
                    break;
                }
            }
            Err(e) => return Err(io_err(&to, e)),
        }
    }
    Ok(refused(
        "exists",
        format!("{dir} already has something called {name}"),
    ))
}

/// `files.create`: an empty file, never in place of anything.
pub fn create(dir: &str, name: &str) -> Result<Value> {
    check_name(name)?;
    let to = abs(dir)?.join(name);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&to)
    {
        Ok(_) => Ok(json!({"ok": true, "path": shown(&to), "name": name})),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(refused(
            "exists",
            format!("{dir} already has something called {name}"),
        )),
        Err(e) => Err(io_err(&to, e)),
    }
}

/// Are these the one file, found under two spellings of its name?
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    // Where names are told apart only by more than their capitals.
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        true
    }
}

/// `files.rename`: another name in the same folder, never in place of
/// anything. (A name that differs only in its capitals, on a disk that
/// does not tell them apart, is the same file and is renamed.)
pub fn rename(path: &Path, name: &str) -> Result<Value> {
    check_name(name)?;
    let meta = std::fs::symlink_metadata(path).map_err(|e| io_err(path, e))?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid("the root of a disk has no name to change"))?;
    let to = parent.join(name);
    if to == path {
        return Ok(json!({"ok": true, "path": shown(&to), "name": name}));
    }
    let taken = match std::fs::symlink_metadata(&to) {
        Ok(other) => {
            let names_alike = path.file_name().map(|n| n.to_string_lossy().to_lowercase())
                == Some(name.to_lowercase());
            !(names_alike && same_file(&meta, &other))
        }
        Err(_) => false,
    };
    if taken {
        return Ok(refused(
            "exists",
            format!("{} already has something called {name}", shown(parent)),
        ));
    }
    std::fs::rename(path, &to).map_err(|e| io_err(path, e))?;
    Ok(json!({"ok": true, "path": shown(&to), "name": name}))
}

// ------------------------------------------------------------------ trash

fn item_failed(path: &str, code: &str, message: impl Into<String>) -> Value {
    json!({"path": path, "code": code, "message": message.into()})
}

/// `files.trash`: each path is moved into a folder of its own under
/// `trash`, with where it came from. What cannot be moved there (it is on
/// another disk: `other_disk`) is not deleted and is listed.
pub fn trash(
    root: &Path,
    paths: &[String],
    by: &str,
    home: &Path,
    state: &Path,
    busy: &[Busy],
) -> Result<Value> {
    let id = new_id("tr");
    let dir = root.join(&id);
    std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
    let _ = spandeck_terminal::sys::private_dir(root);
    let (mut items, mut trashed, mut failed) = (vec![], vec![], vec![]);
    for path in paths {
        let from = match abs(path) {
            Ok(p) => p,
            Err(e) => {
                failed.push(item_failed(path, "invalid", e.to_string()));
                continue;
            }
        };
        if let Some(why) = guarded(&from, home, state) {
            failed.push(item_failed(path, "refused", why));
            continue;
        }
        let who = in_use(&from, busy);
        if !who.is_empty() {
            failed.push(in_use_item(path, &who));
            continue;
        }
        let held = items.len().to_string();
        match std::fs::rename(&from, dir.join(&held)) {
            Ok(()) => {
                items.push(json!({"from": path, "held": held}));
                trashed.push(path.clone());
            }
            Err(e) => {
                use std::io::ErrorKind as K;
                let code = match e.kind() {
                    K::NotFound => "not_found",
                    K::CrossesDevices => "other_disk",
                    K::PermissionDenied => "denied",
                    _ => "failed",
                };
                failed.push(item_failed(path, code, e.to_string()));
            }
        }
    }
    if items.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        return Ok(json!({"trash": null, "trashed": trashed, "failed": failed}));
    }
    let manifest = json!({"id": id, "at_ms": now_ms(), "by": by, "items": items});
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).map_err(|e| io_err(&dir, e))?;
    Ok(json!({"trash": id, "trashed": trashed, "failed": failed}))
}

fn manifest(dir: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).ok()?).ok()
}

/// `files.restore`: everything of one trashing goes back where it was.
/// An item whose place is taken stays in the trash and is listed
/// (`exists`), and can be restored again later.
pub fn restore(root: &Path, id: &str) -> Result<Value> {
    if !valid_id(id) {
        return Err(invalid("that is not a trash id"));
    }
    let dir = root.join(id);
    let mut doc = manifest(&dir).ok_or_else(|| {
        Error::NotFound(format!(
            "trash {id} (it is kept 7 days, and is gone once restored)"
        ))
    })?;
    let (mut restored, mut failed, mut left) = (vec![], vec![], vec![]);
    for item in doc["items"].as_array().cloned().unwrap_or_default() {
        let (Some(from), Some(held)) = (item["from"].as_str(), item["held"].as_str()) else {
            continue;
        };
        let to = Path::new(from);
        if exists(to) {
            failed.push(item_failed(from, "exists", "something is there now"));
            left.push(item);
            continue;
        }
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::rename(dir.join(held), to) {
            Ok(()) => restored.push(from.to_string()),
            Err(e) => {
                failed.push(item_failed(from, "failed", e.to_string()));
                left.push(item);
            }
        }
    }
    if left.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
    } else {
        doc["items"] = json!(left);
        let _ = std::fs::write(dir.join("manifest.json"), doc.to_string());
    }
    Ok(json!({"restored": restored, "failed": failed, "left": left.len()}))
}

/// Remove what has been in the trash longer than `keep_ms`.
pub fn sweep_trash(root: &Path, now_ms: u64, keep_ms: u64) -> usize {
    let Ok(rd) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut gone = 0;
    for e in rd.flatten() {
        let dir = e.path();
        // No manifest: a trashing that was cut short. By the folder's time.
        let at = manifest(&dir)
            .and_then(|m| m["at_ms"].as_u64())
            .or_else(|| std::fs::symlink_metadata(&dir).ok().map(|m| mtime_ms(&m)))
            .unwrap_or(0);
        if now_ms.saturating_sub(at) > keep_ms && std::fs::remove_dir_all(&dir).is_ok() {
            gone += 1;
        }
    }
    gone
}

/// `files.delete`: for good.
pub fn delete(paths: &[String], home: &Path, state: &Path, busy: &[Busy]) -> Value {
    let (mut deleted, mut failed) = (vec![], vec![]);
    for path in paths {
        let who = abs(path).map(|p| in_use(&p, busy)).unwrap_or_default();
        if !who.is_empty() {
            failed.push(in_use_item(path, &who));
            continue;
        }
        let out = abs(path).and_then(|p| {
            if let Some(why) = guarded(&p, home, state) {
                return Err(Error::Denied(why.to_string()));
            }
            let meta = std::fs::symlink_metadata(&p).map_err(|e| io_err(&p, e))?;
            match meta.is_dir() {
                true => std::fs::remove_dir_all(&p),
                false => std::fs::remove_file(&p),
            }
            .map_err(|e| io_err(&p, e))
        });
        match out {
            Ok(()) => deleted.push(path.clone()),
            Err(e) => failed.push(item_failed(path, e.code(), e.to_string())),
        }
    }
    json!({"deleted": deleted, "failed": failed})
}

// ------------------------------------------------------------ walking a tree

enum Node<'a> {
    /// A folder, before what is in it.
    Dir(&'a std::fs::Metadata),
    /// A file, or a link to one (then the file's own details).
    File(&'a std::fs::Metadata),
    /// Left out, with a code and why.
    Skip(&'static str, String),
    /// The folder again, after what is in it.
    Leave(&'a std::fs::Metadata),
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Flow {
    On,
    /// Not into this folder.
    Prune,
    Stop,
}

/// Every file and folder at and under `path`, folders before and after
/// their contents, names in order. `rel` is the path below the top. A
/// link to a file is the file; a link to a folder is not followed, and
/// nothing that is not a file or a folder is taken.
fn walk(
    path: &Path,
    rel: &mut PathBuf,
    depth: u32,
    f: &mut dyn FnMut(&Path, &Path, Node) -> Flow,
) -> Flow {
    let stop = |flow: Flow| match flow {
        Flow::Stop => Flow::Stop,
        _ => Flow::On,
    };
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => return stop(f(path, rel, Node::Skip("not_found", e.to_string()))),
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        let node = match std::fs::metadata(path) {
            Ok(to) if to.is_file() => return stop(f(path, rel, Node::File(&to))),
            Ok(to) if to.is_dir() => {
                Node::Skip("link", "a link to a folder is not followed".into())
            }
            Ok(_) => Node::Skip("special", "it is not a file or a folder".into()),
            Err(_) => Node::Skip("link", "a link to nothing".into()),
        };
        return stop(f(path, rel, node));
    }
    if ft.is_file() {
        return stop(f(path, rel, Node::File(&meta)));
    }
    if !ft.is_dir() {
        let why = "it is not a file or a folder".to_string();
        return stop(f(path, rel, Node::Skip("special", why)));
    }
    if depth > 200 {
        let why = "folders more than 200 deep are not followed".to_string();
        return stop(f(path, rel, Node::Skip("deep", why)));
    }
    match f(path, rel, Node::Dir(&meta)) {
        Flow::On => {}
        Flow::Prune => return Flow::On,
        Flow::Stop => return Flow::Stop,
    }
    let mut names: Vec<OsString> = match std::fs::read_dir(path) {
        Ok(rd) => rd.flatten().map(|e| e.file_name()).collect(),
        Err(e) => return stop(f(path, rel, Node::Skip("unreadable", e.to_string()))),
    };
    names.sort();
    for name in names {
        if is_part(&name) {
            continue;
        }
        rel.push(&name);
        let flow = walk(&path.join(&name), rel, depth + 1, f);
        rel.pop();
        if flow == Flow::Stop {
            return Flow::Stop;
        }
    }
    stop(f(path, rel, Node::Leave(&meta)))
}

fn under(top: &Path, rel: &Path) -> PathBuf {
    match rel.as_os_str().is_empty() {
        true => top.to_path_buf(),
        false => top.join(rel),
    }
}

// ------------------------------------------------------------------- jobs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Copy,
    Move,
}

#[derive(Default, Clone)]
struct Progress {
    state: &'static str,
    counting: bool,
    files_done: u64,
    files_total: u64,
    bytes_done: u64,
    bytes_total: u64,
    current: Option<String>,
    errors: Vec<Value>,
    errors_more: u64,
    skipped: Vec<Value>,
    skipped_more: u64,
    created: Vec<String>,
    message: Option<String>,
    finished_ms: u64,
}

/// A copy or a move that is running, or ran.
pub struct Job {
    pub id: String,
    op: Op,
    to: Option<String>,
    dir: String,
    conflict: Conflict,
    started_ms: u64,
    cancel: AtomicBool,
    seq: AtomicU64,
    st: Mutex<Progress>,
}

/// Errors and skipped items listed for one job; the rest are counted.
const LISTED: usize = 200;

impl Job {
    fn new(op: Op, to: Option<String>, dir: &str, conflict: Conflict) -> Job {
        Job {
            id: new_id("fj"),
            op,
            to,
            dir: dir.to_string(),
            conflict,
            started_ms: now_ms(),
            cancel: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            st: Mutex::new(Progress {
                state: "running",
                counting: true,
                ..Default::default()
            }),
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn error(&self, path: &Path, code: &str, message: impl Into<String>) {
        let mut st = self.st.lock().unwrap();
        match st.errors.len() < LISTED {
            true => st
                .errors
                .push(item_failed(&shown(path), code, message.into())),
            false => st.errors_more += 1,
        }
    }

    /// An error that carries more than a sentence (`in_use`: the panes).
    fn error_item(&self, item: Value) {
        let mut st = self.st.lock().unwrap();
        match st.errors.len() < LISTED {
            true => st.errors.push(item),
            false => st.errors_more += 1,
        }
    }

    fn skip(&self, path: &Path, code: &str, message: impl Into<String>) {
        let mut st = self.st.lock().unwrap();
        match st.skipped.len() < LISTED {
            true => st
                .skipped
                .push(item_failed(&shown(path), code, message.into())),
            false => st.skipped_more += 1,
        }
    }

    fn problems(&self) -> u64 {
        let st = self.st.lock().unwrap();
        st.errors.len() as u64 + st.errors_more
    }

    fn current(&self, path: &Path) {
        self.st.lock().unwrap().current = Some(shown(path));
    }

    fn file_done(&self) {
        self.st.lock().unwrap().files_done += 1;
    }

    fn bytes(&self, n: u64) {
        self.st.lock().unwrap().bytes_done += n;
    }

    fn finish(&self, failed: Option<String>) {
        let mut st = self.st.lock().unwrap();
        st.state = match (&failed, self.cancelled()) {
            (Some(_), _) => "failed",
            (None, true) => "cancelled",
            (None, false) => "done",
        };
        st.message = failed;
        st.current = None;
        st.counting = false;
        st.finished_ms = now_ms();
    }

    pub fn view(&self) -> Value {
        let st = self.st.lock().unwrap();
        json!({
            "id": self.id,
            "op": match self.op { Op::Copy => "copy", Op::Move => "move" },
            "to": self.to,
            "dir": self.dir,
            "state": st.state,
            "counting": st.counting,
            "files_done": st.files_done,
            "files_total": st.files_total,
            "bytes_done": st.bytes_done,
            "bytes_total": st.bytes_total,
            "current": st.current,
            "errors": st.errors,
            "errors_more": st.errors_more,
            "skipped": st.skipped,
            "skipped_more": st.skipped_more,
            "created": st.created,
            "message": st.message,
            "started_ms": self.started_ms,
            "finished_ms": st.finished_ms,
        })
    }
}

/// How many files and bytes are at and under each path.
fn count(job: &Job, paths: &[PathBuf]) -> Vec<(u64, u64)> {
    let mut per = vec![];
    for path in paths {
        let (mut files, mut bytes) = (0u64, 0u64);
        walk(path, &mut PathBuf::new(), 0, &mut |_, _, node| {
            if let Node::File(meta) = node {
                files += 1;
                bytes += meta.len();
            }
            match job.cancelled() {
                true => Flow::Stop,
                false => Flow::On,
            }
        });
        let mut st = job.st.lock().unwrap();
        st.files_total += files;
        st.bytes_total += bytes;
        per.push((files, bytes));
    }
    job.st.lock().unwrap().counting = false;
    per
}

enum CopyErr {
    Cancelled,
    Failed(String),
}

/// Copy one file to `to`, through a part file beside it, with the
/// source's permission bits and time. Whatever has the name is replaced,
/// unless it is a folder.
fn copy_file(
    job: &Job,
    parts: &Parts,
    from: &Path,
    to: &Path,
    meta: &std::fs::Metadata,
) -> std::result::Result<(), CopyErr> {
    let fail = |e: std::io::Error| CopyErr::Failed(e.to_string());
    let parent = to.parent().unwrap_or(Path::new("."));
    let n = job.seq.fetch_add(1, Ordering::Relaxed);
    let part = parent.join(format!("{PART_PREFIX}{}-{n}", job.id));
    let mut src = std::fs::File::open(from).map_err(fail)?;
    parts.add(&part);
    let mut run = || -> std::result::Result<(), CopyErr> {
        let mut out = std::fs::File::create(&part).map_err(fail)?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if job.cancelled() {
                return Err(CopyErr::Cancelled);
            }
            let n = src.read(&mut buf).map_err(fail)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(fail)?;
            job.bytes(n as u64);
        }
        if let Ok(at) = meta.modified() {
            let _ = out.set_modified(at);
        }
        drop(out);
        set_mode(&part, mode_of(meta));
        if std::fs::symlink_metadata(to).is_ok_and(|m| m.is_dir()) {
            return Err(CopyErr::Failed(
                "a folder of that name is there, and a file does not replace a folder".into(),
            ));
        }
        std::fs::rename(&part, to).map_err(fail)
    };
    let out = run();
    if out.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    parts.done(&part);
    out
}

/// Copy the file or the tree at `from` to `to` on this station. A folder
/// that is already there is added to; a file that is already there is
/// replaced. A file that fails is listed and the rest carries on.
fn copy_tree(job: &Job, parts: &Parts, from: &Path, to: &Path) {
    walk(from, &mut PathBuf::new(), 0, &mut |src, rel, node| {
        if job.cancelled() {
            return Flow::Stop;
        }
        let dest = under(to, rel);
        match node {
            Node::Dir(_) => match std::fs::create_dir(&dest) {
                Ok(()) => Flow::On,
                Err(_) if std::fs::symlink_metadata(&dest).is_ok_and(|m| m.is_dir()) => Flow::On,
                Err(e) => {
                    job.error(src, "failed", e.to_string());
                    Flow::Prune
                }
            },
            Node::File(meta) => {
                job.current(src);
                match copy_file(job, parts, src, &dest, meta) {
                    Ok(()) => job.file_done(),
                    Err(CopyErr::Cancelled) => return Flow::Stop,
                    Err(CopyErr::Failed(why)) => job.error(src, "failed", why),
                }
                Flow::On
            }
            Node::Skip(code, why) => {
                job.skip(src, code, why);
                Flow::On
            }
            Node::Leave(meta) => {
                set_mode(&dest, mode_of(meta));
                set_mtime(&dest, meta.modified().ok());
                Flow::On
            }
        }
    });
}

/// Where a top-level item goes on this station.
enum Top {
    To(PathBuf),
    Skip,
    Taken(String),
}

fn top_place(dir: &Path, name: &str, from_dir: bool, conflict: Conflict) -> Top {
    let to = dir.join(name);
    let Ok(there) = std::fs::symlink_metadata(&to) else {
        return Top::To(to);
    };
    match conflict {
        Conflict::Fail => Top::Taken(format!(
            "{} already has something called {name}",
            shown(dir)
        )),
        Conflict::Skip => Top::Skip,
        Conflict::KeepBoth => match free_name(dir, name) {
            Some(free) => Top::To(dir.join(free)),
            None => Top::Taken("no free name was found".into()),
        },
        Conflict::Replace if from_dir == there.is_dir() => Top::To(to),
        Conflict::Replace if from_dir => {
            Top::Taken("a file of that name is there, and a folder does not replace a file".into())
        }
        Conflict::Replace => Top::Taken(
            "a folder of that name is there, and a file does not replace a folder".into(),
        ),
    }
}

fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A copy or a move on this station. `busy`: the panes the station lists
/// now, asked for each item moved (a job can run long).
fn transfer_local(
    job: &Job,
    parts: &Parts,
    paths: &[PathBuf],
    home: &Path,
    state: &Path,
    busy: &dyn Fn() -> Vec<Busy>,
) {
    let dir = PathBuf::from(&job.dir);
    let per = count(job, paths);
    for (from, (files, bytes)) in paths.iter().zip(per) {
        if job.cancelled() {
            return;
        }
        let Ok(meta) = std::fs::symlink_metadata(from) else {
            job.error(from, "not_found", "it is not there");
            continue;
        };
        let Some(name) = from.file_name().and_then(|n| n.to_str()) else {
            job.error(from, "name", "its name cannot be said in a request");
            continue;
        };
        let from_dir = meta.is_dir();
        if from_dir && real(&dir).starts_with(real(from)) {
            job.error(from, "into_itself", "a folder cannot go into itself");
            continue;
        }
        let same_folder = from.parent().is_some_and(|p| real(p) == real(&dir));
        if job.op == Op::Move {
            if same_folder {
                job.skip(from, "same", "it is already in that folder");
                continue;
            }
            if let Some(why) = guarded(from, home, state) {
                job.error(from, "refused", why);
                continue;
            }
            let panes = busy();
            let who = in_use(from, &panes);
            if !who.is_empty() {
                job.error_item(in_use_item(&shown(from), &who));
                continue;
            }
        }
        // A copy into the folder it is in is a second copy beside it.
        let conflict = match same_folder {
            true => Conflict::KeepBoth,
            false => job.conflict,
        };
        let to = match top_place(&dir, name, from_dir, conflict) {
            Top::To(to) => to,
            Top::Skip => {
                job.skip(from, "exists", "something of that name is there");
                continue;
            }
            Top::Taken(why) => {
                job.error(from, "exists", why);
                continue;
            }
        };
        let merging = exists(&to) && from_dir;
        if job.op == Op::Move && !merging && std::fs::rename(from, &to).is_ok() {
            let mut st = job.st.lock().unwrap();
            st.files_done += files;
            st.bytes_done += bytes;
            st.created.push(shown(&to));
            continue;
        }
        // A copy; or a move to another disk or into a folder that is
        // there: copied, and the source removed only when all of it was.
        let before = job.problems();
        copy_tree(job, parts, from, &to);
        job.st.lock().unwrap().created.push(shown(&to));
        if job.op == Op::Move && !job.cancelled() {
            if job.problems() > before {
                job.error(
                    from,
                    "kept",
                    "not all of it could be copied, so it was left where it is",
                );
                continue;
            }
            let gone = match from_dir {
                true => std::fs::remove_dir_all(from),
                false => std::fs::remove_file(from),
            };
            if let Err(e) = gone {
                job.error(from, "kept", format!("copied, but not removed: {e}"));
            }
        }
    }
}

// ---------------------------------------------------- to another station

enum Far {
    /// The station answered with an error.
    Said(String),
    /// No answer, after three tries.
    Away(String),
    /// It has no `files.` methods.
    Old,
}

/// The other station of a copy: this station carries out the owner's
/// request there by forwarding the owner's `files.mkdir` and `files.put`.
struct Other {
    d: Daemon,
    rt: tokio::runtime::Handle,
    station: String,
    sep: String,
}

impl Other {
    fn call(&self, method: &str, params: Value) -> std::result::Result<Value, Far> {
        let mut last = String::new();
        for attempt in 0..3u64 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(400 * attempt));
            }
            let request = json!({"method": method, "params": params});
            let out = self.rt.block_on(self.d.fleet.call_bulk(
                &self.station,
                "rpc.forward",
                request,
                BULK_LIMIT,
            ));
            match out {
                Ok(v) => return Ok(v),
                Err(e) if e.contains("method files.") && e.contains("not found") => {
                    return Err(Far::Old)
                }
                Err(e) if crate::fleet::NoAnswer::said(&e) || e.starts_with("bad reply") => {
                    last = e
                }
                Err(e) => return Err(Far::Said(e)),
            }
        }
        Err(Far::Away(last))
    }

    fn join(&self, dir: &str, name: &str) -> String {
        match dir.ends_with(&self.sep) {
            true => format!("{dir}{name}"),
            false => format!("{dir}{}{name}", self.sep),
        }
    }

    /// Send one file in pieces. Ok(None): the name is taken there.
    fn send(
        &self,
        job: &Job,
        from: &Path,
        dir: &str,
        name: &str,
        meta: &std::fs::Metadata,
        conflict: Conflict,
    ) -> std::result::Result<Option<String>, Far> {
        let said = |e: std::io::Error| Far::Said(e.to_string());
        let id = format!("{}-{}", job.id, job.seq.fetch_add(1, Ordering::Relaxed));
        let mut src = std::fs::File::open(from).map_err(said)?;
        let (mut at, mut piece, mut gaps) = (0u64, SEND_START, 0);
        let mut buf = vec![0u8; SEND_MAX];
        loop {
            if job.cancelled() {
                let _ = self.call(
                    "files.put",
                    json!({"id": id, "dir": dir, "name": name, "abort": true}),
                );
                return Ok(None);
            }
            let mut got = 0;
            while got < piece {
                match src.read(&mut buf[got..piece]).map_err(said)? {
                    0 => break,
                    n => got += n,
                }
            }
            let done = got < piece;
            let began = std::time::Instant::now();
            let answer = self.call(
                "files.put",
                json!({
                    "id": id, "dir": dir, "name": name, "offset": at,
                    "data_b64": base64::engine::general_purpose::STANDARD.encode(&buf[..got]),
                    "done": done, "conflict": conflict.word(),
                    "mode": mode_of(meta), "mtime_ms": mtime_ms(meta),
                }),
            )?;
            match (answer["ok"].as_bool(), answer["code"].as_str()) {
                (Some(true), _) => {}
                (_, Some("exists")) => {
                    let _ = self.call(
                        "files.put",
                        json!({"id": id, "dir": dir, "name": name, "abort": true}),
                    );
                    return Ok(None);
                }
                // That station holds less than was sent (it was started
                // again, say): from what it holds.
                (_, Some("gap")) if gaps < 5 => {
                    gaps += 1;
                    let have = answer["have"].as_u64().unwrap_or(0);
                    job.st.lock().unwrap().bytes_done -= at.saturating_sub(have).min(at);
                    at = have;
                    src.seek(SeekFrom::Start(at)).map_err(said)?;
                    continue;
                }
                _ => {
                    return Err(Far::Said(
                        answer["message"]
                            .as_str()
                            .unwrap_or("it was not taken")
                            .to_string(),
                    ))
                }
            }
            at += got as u64;
            job.bytes(got as u64);
            if done {
                return Ok(Some(
                    answer["path"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| self.join(dir, name)),
                ));
            }
            // A piece that took long is followed by a smaller one, and the
            // link is left a quarter of the time for everything else.
            let took = began.elapsed();
            if took > Duration::from_secs(2) {
                piece = (piece / 2).max(SEND_MIN);
            } else if took < Duration::from_millis(500) {
                piece = (piece * 2).min(SEND_MAX);
            }
            std::thread::sleep((took / 4).min(Duration::from_millis(250)));
        }
    }
}

/// Why a copy to another station could not go on.
fn far_words(station: &str, far: Far) -> String {
    match far {
        Far::Old => format!(
            "{station} does not have the file explorer yet: it needs updating to this version of spandeck before files can be copied to it"
        ),
        Far::Away(why) => format!("{station} {why}"),
        Far::Said(why) => format!("{station}: {why}"),
    }
}

/// Copy to another station. Ok: the job ran (what failed is listed);
/// Err: why it could not go on.
fn transfer_far(job: &Job, other: &Other, paths: &[PathBuf]) -> std::result::Result<(), String> {
    count(job, paths);
    // The top of each item, and under it the folder each folder became.
    for from in paths {
        if job.cancelled() {
            return Ok(());
        }
        let Some(name) = from.file_name().and_then(|n| n.to_str()) else {
            job.error(from, "name", "its name cannot be said in a request");
            continue;
        };
        let top_conflict = job.conflict;
        // Source folder -> the folder it is over there.
        let mut dirs: HashMap<PathBuf, String> = HashMap::new();
        let mut stopped: Option<Far> = None;
        walk(from, &mut PathBuf::new(), 0, &mut |src, rel, node| {
            if job.cancelled() {
                return Flow::Stop;
            }
            let top = rel.as_os_str().is_empty();
            let parent = match top {
                true => Some(job.dir.clone()),
                false => rel.parent().and_then(|p| dirs.get(p)).cloned(),
            };
            let (Some(parent), Some(own)) = (
                parent,
                match top {
                    true => Some(name),
                    false => rel.file_name().and_then(|n| n.to_str()),
                },
            ) else {
                job.skip(src, "name", "its name cannot be said in a request");
                return Flow::Prune;
            };
            let mut far = |out: std::result::Result<Flow, Far>| match out {
                Ok(flow) => flow,
                Err(Far::Said(why)) => {
                    job.error(src, "failed", why);
                    Flow::Prune
                }
                Err(far) => {
                    stopped = Some(far);
                    Flow::Stop
                }
            };
            match node {
                Node::Dir(_) => {
                    let conflict = match (top, top_conflict) {
                        (true, Conflict::KeepBoth) => "keep_both",
                        _ => "fail",
                    };
                    let merge = !top || top_conflict == Conflict::Replace;
                    let made = other.call(
                        "files.mkdir",
                        json!({"dir": parent, "name": own, "conflict": conflict, "merge": merge}),
                    );
                    far(made.map(|v| match v["path"].as_str() {
                        Some(path) if v["ok"] == json!(true) => {
                            if top {
                                job.st.lock().unwrap().created.push(path.to_string());
                            }
                            dirs.insert(rel.to_path_buf(), path.to_string());
                            Flow::On
                        }
                        _ if top && top_conflict == Conflict::Skip => {
                            job.skip(src, "exists", "something of that name is there");
                            Flow::Prune
                        }
                        _ => {
                            let why = v["message"].as_str().unwrap_or("it could not be made");
                            job.error(src, v["code"].as_str().unwrap_or("failed"), why);
                            Flow::Prune
                        }
                    }))
                }
                Node::File(meta) => {
                    job.current(src);
                    let conflict = match top {
                        true => top_conflict,
                        false => Conflict::Replace,
                    };
                    let sent = other.send(job, src, &parent, own, meta, conflict);
                    far(sent.map(|path| {
                        match path {
                            Some(path) => {
                                job.file_done();
                                if top {
                                    job.st.lock().unwrap().created.push(path);
                                }
                            }
                            None if job.cancelled() => return Flow::Stop,
                            None if top && top_conflict == Conflict::Skip => {
                                job.skip(src, "exists", "something of that name is there")
                            }
                            None => job.error(src, "exists", "something of that name is there"),
                        }
                        Flow::On
                    }))
                }
                Node::Skip(code, why) => {
                    job.skip(src, code, why);
                    Flow::On
                }
                Node::Leave(_) => Flow::On,
            }
        });
        if let Some(far) = stopped {
            return Err(far_words(&other.station, far));
        }
    }
    Ok(())
}

// -------------------------------------------------------------------- zip

#[derive(Default, Clone)]
struct ZipProgress {
    state: &'static str,
    files_done: u64,
    files_total: u64,
    bytes_done: u64,
    bytes_total: u64,
    size: u64,
    code: Option<&'static str>,
    message: Option<String>,
    skipped: Vec<Value>,
    used_ms: u64,
}

/// One zip being built, or built and waiting to be read.
pub struct Zip {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    cancel: AtomicBool,
    st: Mutex<ZipProgress>,
}

impl Zip {
    pub fn view(&self) -> Value {
        let st = self.st.lock().unwrap();
        json!({
            "id": self.id,
            "name": self.name,
            "state": st.state,
            "files_done": st.files_done,
            "files_total": st.files_total,
            "bytes_done": st.bytes_done,
            "bytes_total": st.bytes_total,
            "size": st.size,
            "code": st.code,
            "message": st.message,
            "skipped": st.skipped,
        })
    }

    fn ready(&self) -> bool {
        self.st.lock().unwrap().state == "ready"
    }
}

/// The name a zip of these paths is given.
fn zip_name(paths: &[PathBuf]) -> String {
    match paths {
        [one] => format!(
            "{}.zip",
            one.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "files".into())
        ),
        _ => "files.zip".to_string(),
    }
}

/// A time as a zip's header says one: two seconds fine, from 1980, in UTC.
fn dos_time(secs: u64) -> (u16, u16) {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Days since 1970 to a date (H. Hinnant's civil_from_days).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    if !(1980..=2107).contains(&year) {
        return (0, (1 << 5) | 1);
    }
    let time = ((rest / 3600) << 11) | ((rest % 3600 / 60) << 5) | (rest % 60 / 2);
    let date = ((year - 1980) << 9) | (month << 5) | day;
    (time as u16, date as u16)
}

struct ZipEntry {
    name: Vec<u8>,
    crc: u32,
    size: u32,
    at: u32,
    mode: u32,
    secs: u64,
}

fn zip_header(e: &ZipEntry, central: bool) -> Vec<u8> {
    let (time, date) = dos_time(e.secs);
    let mut h = vec![];
    let extra_len: u16 = 9;
    match central {
        false => h.extend(0x0403_4b50u32.to_le_bytes()),
        true => {
            h.extend(0x0201_4b50u32.to_le_bytes());
            // Made on Unix (3), by version 2.0: the mode below is read.
            h.extend(((3u16 << 8) | 20).to_le_bytes());
        }
    }
    h.extend(20u16.to_le_bytes());
    // Names are UTF-8.
    h.extend(0x0800u16.to_le_bytes());
    // Stored, not compressed.
    h.extend(0u16.to_le_bytes());
    h.extend(time.to_le_bytes());
    h.extend(date.to_le_bytes());
    h.extend(e.crc.to_le_bytes());
    h.extend(e.size.to_le_bytes());
    h.extend(e.size.to_le_bytes());
    h.extend((e.name.len() as u16).to_le_bytes());
    h.extend(extra_len.to_le_bytes());
    if central {
        h.extend(0u16.to_le_bytes()); // comment
        h.extend(0u16.to_le_bytes()); // disk
        h.extend(0u16.to_le_bytes()); // internal attributes
        let dir = if e.name.ends_with(b"/") { 0x10 } else { 0 };
        h.extend(((e.mode << 16) | dir).to_le_bytes());
        h.extend(e.at.to_le_bytes());
    }
    h.extend(&e.name);
    // The time again, exactly and in UTC ("UT", the extended timestamp).
    h.extend(0x5455u16.to_le_bytes());
    h.extend(5u16.to_le_bytes());
    h.push(1);
    h.extend((e.secs.min(u32::MAX as u64) as u32).to_le_bytes());
    h
}

/// Write one zip of the files and trees at `paths` to `zip.path`, stored
/// and not compressed. Err: (code, why).
fn zip_build(zip: &Zip, paths: &[PathBuf]) -> std::result::Result<(), (&'static str, String)> {
    let failed = |e: std::io::Error| ("failed", e.to_string());
    // What it would be, before anything is written.
    let (mut files, mut entries, mut bytes) = (0u64, 0u64, 0u64);
    for path in paths {
        let flow = walk(path, &mut PathBuf::new(), 0, &mut |_, _, node| {
            match node {
                Node::File(meta) => {
                    files += 1;
                    entries += 1;
                    bytes += meta.len();
                }
                Node::Dir(_) => entries += 1,
                _ => {}
            }
            match zip.cancel.load(Ordering::Relaxed) || entries > ZIP_MAX_ENTRIES {
                true => Flow::Stop,
                false => Flow::On,
            }
        });
        if flow == Flow::Stop {
            break;
        }
    }
    if entries > ZIP_MAX_ENTRIES || bytes > ZIP_MAX_BYTES {
        let what = match entries > ZIP_MAX_ENTRIES {
            true => format!("more than {ZIP_MAX_ENTRIES} files and folders"),
            false => format!("{files} files, {} MB", bytes >> 20),
        };
        return Err((
            "too_big",
            format!(
                "one zip holds at most {ZIP_MAX_ENTRIES} files and folders and {} MB; this would be {what}. Download it in parts, or copy it to a station",
                ZIP_MAX_BYTES >> 20
            ),
        ));
    }
    {
        let mut st = zip.st.lock().unwrap();
        st.files_total = files;
        st.bytes_total = bytes;
    }
    if let Some(dir) = zip.path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
        let _ = spandeck_terminal::sys::private_dir(dir);
    }
    let mut out = std::fs::File::create(&zip.path).map_err(failed)?;
    let mut list: Vec<ZipEntry> = vec![];
    let mut at = 0u64;
    let mut broke: Option<(&'static str, String)> = None;
    let mut buf = vec![0u8; 1 << 20];
    for path in paths {
        let Some(top) = path.file_name().map(PathBuf::from) else {
            continue;
        };
        let flow = walk(path, &mut PathBuf::new(), 0, &mut |src, rel, node| {
            if zip.cancel.load(Ordering::Relaxed) {
                broke = Some(("cancelled", "it was cancelled".into()));
                return Flow::Stop;
            }
            let skip = |code: &str, why: String| {
                let mut st = zip.st.lock().unwrap();
                if st.skipped.len() < LISTED {
                    st.skipped.push(item_failed(&shown(src), code, why));
                }
            };
            let inside = under(&top, rel);
            // Names in a zip are relative, with `/` whatever the system.
            let Some(name) = inside
                .iter()
                .map(|part| part.to_str())
                .collect::<Option<Vec<&str>>>()
                .map(|parts| parts.join("/"))
            else {
                skip("name", "its name is not UTF-8".into());
                return Flow::Prune;
            };
            let (meta, is_dir) = match node {
                Node::Dir(meta) => (meta, true),
                Node::File(meta) => (meta, false),
                Node::Skip(code, why) => {
                    skip(code, why);
                    return Flow::On;
                }
                Node::Leave(_) => return Flow::On,
            };
            let mut e = ZipEntry {
                name: match is_dir {
                    true => format!("{name}/").into_bytes(),
                    false => name.into_bytes(),
                },
                crc: 0,
                size: 0,
                at: at as u32,
                mode: match is_dir {
                    true => 0o040000 | mode_of(meta).map_or(0o755, |m| m & 0o777),
                    false => 0o100000 | mode_of(meta).map_or(0o644, |m| m & 0o777),
                },
                secs: mtime_ms(meta) / 1000,
            };
            if e.name.len() > u16::MAX as usize {
                skip("name", "its path is too long for a zip".into());
                return Flow::Prune;
            }
            let file = match is_dir {
                true => None,
                false => match std::fs::File::open(src) {
                    Ok(f) => Some(f),
                    Err(err) => {
                        skip("unreadable", err.to_string());
                        return Flow::On;
                    }
                },
            };
            let write = || -> std::io::Result<bool> {
                let head = zip_header(&e, false);
                out.write_all(&head)?;
                let mut sum = crc32fast::Hasher::new();
                let mut size = 0u64;
                if let Some(mut file) = file {
                    loop {
                        if zip.cancel.load(Ordering::Relaxed) {
                            return Ok(false);
                        }
                        let n = file.read(&mut buf)?;
                        if n == 0 {
                            break;
                        }
                        // A file that grew since it was counted.
                        if at + head.len() as u64 + size + n as u64 > u32::MAX as u64 - (64 << 20) {
                            return Err(std::io::Error::other("the zip would pass 4 GiB"));
                        }
                        out.write_all(&buf[..n])?;
                        sum.update(&buf[..n]);
                        size += n as u64;
                        zip.st.lock().unwrap().bytes_done += n as u64;
                    }
                    // The sum and the sizes, into the header written above.
                    e.crc = sum.finalize();
                    e.size = size as u32;
                    out.seek(SeekFrom::Start(at + 14))?;
                    out.write_all(&e.crc.to_le_bytes())?;
                    out.write_all(&e.size.to_le_bytes())?;
                    out.write_all(&e.size.to_le_bytes())?;
                    out.seek(SeekFrom::End(0))?;
                    zip.st.lock().unwrap().files_done += 1;
                }
                at += head.len() as u64 + size;
                Ok(true)
            };
            match write() {
                Ok(true) => {
                    list.push(e);
                    Flow::On
                }
                Ok(false) => {
                    broke = Some(("cancelled", "it was cancelled".into()));
                    Flow::Stop
                }
                Err(err) => {
                    broke = Some(("failed", err.to_string()));
                    Flow::Stop
                }
            }
        });
        if flow == Flow::Stop {
            break;
        }
    }
    if let Some(why) = broke {
        return Err(why);
    }
    let start = at;
    for e in &list {
        let head = zip_header(e, true);
        out.write_all(&head).map_err(failed)?;
        at += head.len() as u64;
    }
    let mut end = vec![];
    end.extend(0x0605_4b50u32.to_le_bytes());
    end.extend(0u16.to_le_bytes());
    end.extend(0u16.to_le_bytes());
    end.extend((list.len() as u16).to_le_bytes());
    end.extend((list.len() as u16).to_le_bytes());
    end.extend(((at - start) as u32).to_le_bytes());
    end.extend((start as u32).to_le_bytes());
    end.extend(0u16.to_le_bytes());
    out.write_all(&end).map_err(failed)?;
    out.flush().map_err(failed)?;
    zip.st.lock().unwrap().size = at + end.len() as u64;
    Ok(())
}

// ------------------------------------------------------------ the station

/// What this station holds for the explorer between calls.
pub struct FilesState {
    pub parts: Parts,
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
    zips: Mutex<BTreeMap<String, Arc<Zip>>>,
    /// Finished uploads: id -> (when, folder and name, the answer).
    finished: Mutex<HashMap<String, (u64, String, Value)>>,
    /// One piece of an upload at a time.
    putting: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Downloads a signed-in browser may start by a link.
    tickets: Mutex<HashMap<String, (u64, Value)>>,
}

impl FilesState {
    pub fn new(dir: &Path) -> FilesState {
        // A zip is named by an id only this process knew.
        let _ = std::fs::remove_dir_all(dir.join("zips"));
        FilesState {
            parts: Parts::at(dir.join("parts.log")),
            jobs: Default::default(),
            zips: Default::default(),
            finished: Default::default(),
            putting: Default::default(),
            tickets: Default::default(),
        }
    }
}

fn paths_of(params: &Value) -> Result<Vec<String>> {
    let list: Vec<String> = params
        .get("paths")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .ok_or_else(|| invalid("missing parameter: paths (a list of absolute paths)"))?;
    if list.is_empty() || list.len() > PATHS_MAX {
        return Err(invalid(format!("paths: 1 to {PATHS_MAX} of them")));
    }
    for p in &list {
        abs(p)?;
    }
    Ok(list)
}

fn text<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid(format!("missing parameter: {key}")))
}

impl Daemon {
    fn files_dir(&self) -> PathBuf {
        self.paths.state_dir.join("files")
    }

    fn trash_dir(&self) -> PathBuf {
        self.files_dir().join("trash")
    }

    /// Every pane this station lists, with its folder: what is in use.
    fn files_busy(&self) -> Vec<Busy> {
        self.list_panes()
            .iter()
            .map(|p| {
                let m = p.meta.lock().unwrap();
                Busy::new(
                    &m.id,
                    &m.name,
                    m.project.as_deref(),
                    m.exit.is_some(),
                    &m.cwd,
                )
            })
            .collect()
    }

    /// The ship's log says what was changed, where, by whom and how much:
    /// never what a file holds.
    fn files_log(&self, kind: &str, by: &str, data: Value) {
        self.record(kind, None, None, None, Some(by), data);
    }

    /// Off the threads that answer everything else: a disk can be slow.
    async fn files_off<T: Send + 'static>(
        &self,
        work: impl FnOnce(Daemon) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let d = self.clone();
        tokio::task::spawn_blocking(move || work(d))
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("the file work ended early: {e}")))?
    }

    /// Every `files.` method. The caller is the owner: rpc.rs has asked.
    pub(crate) async fn files_call(&self, method: &str, params: Value, by: &str) -> Result<Value> {
        let by = by.to_string();
        match method {
            "files.home" => {
                let cwd = match params.get("pane").and_then(|p| p.as_str()) {
                    Some(pane) => Some(self.pane(pane)?.meta().cwd),
                    None => None,
                };
                Ok(json!({
                    "station": self.station,
                    "home": shown(&self.home),
                    "cwd": cwd,
                    "roots": roots(),
                    "sep": std::path::MAIN_SEPARATOR_STR,
                    "os": std::env::consts::OS,
                    "piece_max": PIECE_MAX,
                }))
            }
            "files.list" => {
                self.files_off(move |_| list(text(&params, "path")?, LIST_MAX))
                    .await
            }
            "files.stat" => self.files_off(move |_| stat(text(&params, "path")?)).await,
            "files.read" => {
                self.files_off(move |d| {
                    let offset = params["offset"].as_u64().unwrap_or(0);
                    let len = params["len"].as_u64().unwrap_or(1 << 20);
                    match params.get("zip").and_then(|z| z.as_str()) {
                        Some(id) => {
                            let zip = d.files_zip(id)?;
                            if !zip.ready() {
                                return Err(invalid("that zip is not ready yet"));
                            }
                            zip.st.lock().unwrap().used_ms = now_ms();
                            read(&zip.path, offset, len)
                        }
                        None => read(&abs(text(&params, "path")?)?, offset, len),
                    }
                })
                .await
            }
            "files.put" => self.files_off(move |d| d.files_put(&params, &by)).await,
            "files.mkdir" => {
                self.files_off(move |d| {
                    let (dir, name) = (text(&params, "dir")?, text(&params, "name")?);
                    let conflict =
                        Conflict::parse(params.get("conflict").and_then(|c| c.as_str()), false)?;
                    let merge = params["merge"] == json!(true);
                    let out = mkdir(dir, name, conflict, merge)?;
                    if out["ok"] == json!(true) && out["existed"] != json!(true) {
                        d.files_log("files.folder_made", &by, json!({"path": out["path"]}));
                    }
                    Ok(out)
                })
                .await
            }
            "files.create" => {
                self.files_off(move |d| {
                    let out = create(text(&params, "dir")?, text(&params, "name")?)?;
                    if out["ok"] == json!(true) {
                        d.files_log("files.created", &by, json!({"path": out["path"]}));
                    }
                    Ok(out)
                })
                .await
            }
            "files.rename" => {
                self.files_off(move |d| {
                    let from = text(&params, "path")?;
                    let path = abs(from)?;
                    if let Some(why) = guarded(&path, &d.home, &d.paths.state_dir) {
                        return Err(Error::Denied(format!("{from} is not renamed here: {why}")));
                    }
                    if let Some(refusal) = rename_in_use(&path, &d.files_busy()) {
                        return Ok(refusal);
                    }
                    let out = rename(&path, text(&params, "name")?)?;
                    if out["ok"] == json!(true) {
                        d.files_log(
                            "files.renamed",
                            &by,
                            json!({"from": from, "path": out["path"]}),
                        );
                    }
                    Ok(out)
                })
                .await
            }
            "files.trash" => {
                self.files_off(move |d| {
                    let paths = paths_of(&params)?;
                    let busy = d.files_busy();
                    let out = trash(
                        &d.trash_dir(),
                        &paths,
                        &by,
                        &d.home,
                        &d.paths.state_dir,
                        &busy,
                    )?;
                    if !out["trash"].is_null() {
                        d.files_log(
                            "files.trashed",
                            &by,
                            json!({"trash": out["trash"], "paths": out["trashed"],
                                   "count": out["trashed"].as_array().map_or(0, |a| a.len())}),
                        );
                    }
                    Ok(out)
                })
                .await
            }
            "files.restore" => {
                self.files_off(move |d| {
                    let id = text(&params, "trash")?;
                    let out = restore(&d.trash_dir(), id)?;
                    d.files_log(
                        "files.restored",
                        &by,
                        json!({"trash": id, "paths": out["restored"],
                               "count": out["restored"].as_array().map_or(0, |a| a.len())}),
                    );
                    Ok(out)
                })
                .await
            }
            "files.delete" => {
                self.files_off(move |d| {
                    let busy = d.files_busy();
                    let out = delete(&paths_of(&params)?, &d.home, &d.paths.state_dir, &busy);
                    d.files_log(
                        "files.deleted",
                        &by,
                        json!({"paths": out["deleted"],
                               "count": out["deleted"].as_array().map_or(0, |a| a.len())}),
                    );
                    Ok(out)
                })
                .await
            }
            "files.transfer" => self.files_transfer(&params, &by),
            "files.jobs" => {
                let jobs = self.files.jobs.lock().unwrap();
                Ok(json!({"jobs": jobs.values().map(|j| j.view()).collect::<Vec<_>>()}))
            }
            "files.job.cancel" => {
                let id = text(&params, "id")?;
                if let Some(job) = self.files.jobs.lock().unwrap().get(id) {
                    job.cancel.store(true, Ordering::Relaxed);
                    return Ok(json!({"ok": true}));
                }
                let zip = self.files_zip(id)?;
                zip.cancel.store(true, Ordering::Relaxed);
                Ok(json!({"ok": true}))
            }
            "files.zip" => self.files_zip_start(&params),
            "files.zip.status" => Ok(self.files_zip(text(&params, "id")?)?.view()),
            "files.ticket" => {
                let station = params.get("station").and_then(|s| s.as_str());
                let (path, zip) = (
                    params.get("path").and_then(|s| s.as_str()),
                    params.get("zip").and_then(|s| s.as_str()),
                );
                if path.is_none() == zip.is_none() {
                    return Err(invalid("a ticket is for one path or one zip"));
                }
                let ticket = random_token();
                let expires = now_ms() + TICKET_MS;
                let mut tickets = self.files.tickets.lock().unwrap();
                tickets.retain(|_, (until, _)| *until > now_ms());
                if tickets.len() >= 1000 {
                    return Err(invalid("too many downloads were asked for at once"));
                }
                tickets.insert(
                    ticket.clone(),
                    (
                        expires,
                        json!({"station": station, "path": path, "zip": zip,
                               "inline": params["inline"] == json!(true)}),
                    ),
                );
                Ok(json!({
                    "ticket": ticket,
                    "url": format!("/api/files/get?ticket={ticket}"),
                    "expires_ms": expires,
                }))
            }
            _ => Err(Error::NotFound(format!("method {method}"))),
        }
    }

    /// What a download's ticket is for, while it is good.
    pub fn files_ticket(&self, ticket: &str) -> Option<Value> {
        let tickets = self.files.tickets.lock().unwrap();
        tickets
            .get(ticket)
            .filter(|(until, _)| *until > now_ms())
            .map(|(_, what)| what.clone())
    }

    fn files_zip(&self, id: &str) -> Result<Arc<Zip>> {
        self.files
            .zips
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "zip {id} (one is kept until nothing has read it for 10 minutes)"
                ))
            })
    }

    fn files_put(&self, params: &Value, by: &str) -> Result<Value> {
        let (id, dir, name) = (
            text(params, "id")?,
            text(params, "dir")?,
            text(params, "name")?,
        );
        let place = format!("{dir}\0{name}");
        // The last piece sent a second time: the same answer.
        {
            let mut finished = self.files.finished.lock().unwrap();
            finished.retain(|_, (at, ..)| now_ms().saturating_sub(*at) < DONE_KEEP_MS);
            if let Some((_, was, answer)) = finished.get(id) {
                return match *was == place {
                    true => Ok(answer.clone()),
                    false => Err(invalid("that upload's id was used for another file")),
                };
            }
        }
        let data = match params.get("data_b64").and_then(|d| d.as_str()) {
            Some(b64) => base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|_| invalid("data_b64 is not base64"))?,
            None => vec![],
        };
        let p = Put {
            id,
            dir,
            name,
            offset: params["offset"].as_u64().unwrap_or(0),
            data: &data,
            done: params["done"] == json!(true),
            abort: params["abort"] == json!(true),
            conflict: Conflict::parse(params.get("conflict").and_then(|c| c.as_str()), false)?,
            mode: params["mode"].as_u64().map(|m| m as u32),
            mtime_ms: params["mtime_ms"].as_u64(),
        };
        let lock = {
            let mut putting = self.files.putting.lock().unwrap();
            if putting.len() > 1000 {
                putting.retain(|_, l| Arc::strong_count(l) > 1);
            }
            putting.entry(id.to_string()).or_default().clone()
        };
        let out = {
            let _one = lock.lock().unwrap();
            put(&self.files.parts, &p)
        };
        let out = out?;
        if out["done"] == json!(true) || p.abort {
            self.files.putting.lock().unwrap().remove(id);
        }
        if out["done"] == json!(true) {
            self.files
                .finished
                .lock()
                .unwrap()
                .insert(id.to_string(), (now_ms(), place, out.clone()));
            self.files_log(
                "files.uploaded",
                by,
                json!({"path": out["path"], "bytes": out["size"]}),
            );
        }
        Ok(out)
    }

    fn files_running(&self) -> usize {
        let jobs = self.files.jobs.lock().unwrap();
        let zips = self.files.zips.lock().unwrap();
        jobs.values()
            .filter(|j| j.st.lock().unwrap().finished_ms == 0)
            .count()
            + zips
                .values()
                .filter(|z| z.st.lock().unwrap().state == "building")
                .count()
    }

    fn files_room(&self) -> Result<()> {
        match self.files_running() >= JOBS_MAX {
            true => Err(invalid(format!(
                "{JOBS_MAX} copies, moves or zips are already running on {}: wait for one to finish, or cancel one",
                self.station
            ))),
            false => Ok(()),
        }
    }

    /// `files.transfer`: checked, started, and answered at once.
    fn files_transfer(&self, params: &Value, by: &str) -> Result<Value> {
        let op = match params.get("op").and_then(|o| o.as_str()) {
            Some("copy") => Op::Copy,
            Some("move") => Op::Move,
            _ => return Err(invalid("op is copy or move")),
        };
        let paths: Vec<PathBuf> = paths_of(params)?.iter().map(PathBuf::from).collect();
        let dir = text(params, "dir")?;
        let conflict = Conflict::parse(params.get("conflict").and_then(|c| c.as_str()), true)?;
        let to = params
            .get("to")
            .and_then(|t| t.as_str())
            .filter(|t| *t != self.station)
            .map(String::from);
        match &to {
            None => {
                let d = abs(dir)?;
                if !d.is_dir() {
                    return Err(invalid(format!("{dir} is not a folder")));
                }
            }
            Some(station) => {
                if op == Op::Move {
                    return Err(invalid(
                        "between stations files are copied, never moved: copy them, then delete them here",
                    ));
                }
                self.require_peer(station)?;
            }
        }
        self.files_room()?;
        let job = Arc::new(Job::new(op, to.clone(), dir, conflict));
        self.files
            .jobs
            .lock()
            .unwrap()
            .insert(job.id.clone(), job.clone());
        let (d, by, running) = (self.clone(), by.to_string(), job.clone());
        let rt = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let job = running;
            let failed = match &job.to {
                None => {
                    let busy = || d.files_busy();
                    transfer_local(
                        &job,
                        &d.files.parts,
                        &paths,
                        &d.home,
                        &d.paths.state_dir,
                        &busy,
                    );
                    None
                }
                Some(station) => {
                    let mut other = Other {
                        d: d.clone(),
                        rt,
                        station: station.clone(),
                        sep: "/".into(),
                    };
                    // Which way its paths are written; and whether it has
                    // these methods at all.
                    match other.call("files.home", json!({})) {
                        Ok(home) => {
                            other.sep = home["sep"].as_str().unwrap_or("/").to_string();
                            transfer_far(&job, &other, &paths).err()
                        }
                        Err(far) => Some(far_words(station, far)),
                    }
                }
            };
            job.finish(failed);
            let v = job.view();
            d.files_log(
                match job.op {
                    Op::Copy => "files.copied",
                    Op::Move => "files.moved",
                },
                &by,
                json!({
                    "job": job.id, "to": job.to, "dir": job.dir, "state": v["state"],
                    "from": paths.iter().take(20).map(|p| shown(p)).collect::<Vec<_>>(),
                    "count": paths.len(), "files": v["files_done"], "bytes": v["bytes_done"],
                    "errors": job.problems(), "message": v["message"],
                }),
            );
        });
        Ok(json!({"job": job.id}))
    }

    /// `files.zip`: started, and answered at once with its id.
    fn files_zip_start(&self, params: &Value) -> Result<Value> {
        let paths: Vec<PathBuf> = paths_of(params)?.iter().map(PathBuf::from).collect();
        self.files_room()?;
        // Only who is given the id reads the zip: not guessable.
        let id = format!("z{}", &random_token()[..32]);
        let zip = Arc::new(Zip {
            id: id.clone(),
            name: zip_name(&paths),
            path: self.files_dir().join("zips").join(format!("{id}.zip")),
            cancel: AtomicBool::new(false),
            st: Mutex::new(ZipProgress {
                state: "building",
                used_ms: now_ms(),
                ..Default::default()
            }),
        });
        self.files
            .zips
            .lock()
            .unwrap()
            .insert(id.clone(), zip.clone());
        let name = zip.name.clone();
        tokio::task::spawn_blocking(move || {
            let out = zip_build(&zip, &paths);
            let mut st = zip.st.lock().unwrap();
            st.used_ms = now_ms();
            match out {
                Ok(()) => st.state = "ready",
                Err((code, why)) => {
                    st.state = "failed";
                    st.code = Some(code);
                    st.message = Some(why);
                    let _ = std::fs::remove_file(&zip.path);
                }
            }
        });
        Ok(json!({"zip": id, "name": name}))
    }

    /// Every ten minutes and at the start: old trash, part files nothing
    /// writes to, zips nobody reads, and jobs that finished a while ago.
    pub(crate) fn files_sweep(&self, thorough: bool) {
        let now = now_ms();
        self.files.jobs.lock().unwrap().retain(|_, j| {
            let done = j.st.lock().unwrap().finished_ms;
            done == 0 || now.saturating_sub(done) < DONE_KEEP_MS
        });
        self.files.zips.lock().unwrap().retain(|_, z| {
            let st = z.st.lock().unwrap();
            let keep = st.state == "building" || now.saturating_sub(st.used_ms) < ZIP_IDLE_MS;
            if !keep {
                let _ = std::fs::remove_file(&z.path);
            }
            keep
        });
        if thorough {
            sweep_trash(&self.trash_dir(), now, TRASH_KEEP_MS);
            self.files.parts.sweep(now, PART_IDLE_MS);
        }
    }
}

#[cfg(test)]
mod tests;
