//! Pictures and videos an agent made, shown in its conversation
//! (DECISIONS.md, "What an agent makes is shown", 17 Nov 2026).
//!
//! The owner: "Can I paste or add an image in ChatGPT or Gemini and have it
//! make a image or video for me, would that work in out chat?" With the
//! real programs that day: Codex's own tool saved a picture and its reply
//! said `Path: /home/…/exec-….png`; Antigravity's saved one and its reply
//! had the path in backticks and a link `(file:///home/…/name.jpg)`. The
//! owner saw a path as text.
//!
//! When the station builds a conversation answer, what an agent said (and
//! the one line of a tool call) is looked through for names of files on
//! this station: whole paths, `~/…`, `file://` addresses, and names
//! relative to the pane's folder, ending as a picture's or a video's
//! does. One that **is** a file here, and whose first bytes **are** a
//! picture or a video (never by its name), within the sizes below, is
//! given an id of the station's own and put on the item (`media`).
//!
//! **What this lets an agent's text do, and no more (rule 6).** Text from
//! an agent is data. Here it chooses which picture or video file on the
//! pane's station the **owner's** deck is shown: a file the agent's own
//! program could already read, and only one that is a picture or a video
//! by its bytes. Nothing else is ever served by this, and it is served
//! to the owner alone: `pane.media.get` and `pane.media.attach` are on no
//! agent's list, no station's and not the read-only one, and take an id
//! the station made for that pane, never a path.
//!
//! The files stay where the agent wrote them: nothing is copied, moved or
//! deleted. One that is gone later is said to be gone.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::daemon::{invalid, Daemon, Error, Result};

/// A picture is at most this large, and a video.
pub const PICTURE_MAX: u64 = 40 << 20;
pub const VIDEO_MAX: u64 = 300 << 20;
/// Shown on one item; the rest are counted.
pub const PER_ITEM: usize = 8;
/// Files known for one pane.
const KEPT_MAX: usize = 400;

const ENDINGS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".webp", ".gif", ".mp4", ".webm", ".mov", ".m4v",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Made {
    id: String,
    path: String,
    name: String,
    size: u64,
    /// image or video: by its first bytes.
    kind: String,
    #[serde(default)]
    w: Option<u32>,
    #[serde(default)]
    h: Option<u32>,
}

fn key(pane: &str) -> String {
    format!("made.{pane}")
}

/// The names in a text that may be a picture's or a video's file here:
/// as written, in order, each once. A name is one word (no blanks), taken
/// out of backticks, brackets, quotes and a `file://` address; an address
/// on the web is never one.
pub fn names(text: &str) -> Vec<String> {
    let low = text.to_lowercase();
    if !ENDINGS.iter().any(|e| low.contains(e)) {
        return vec![];
    }
    let mut out: Vec<String> = vec![];
    for word in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '`' | '(' | ')' | '[' | ']' | '"' | '\'' | '<' | '>' | '|' | '*'
            )
    }) {
        let word = word.trim_end_matches(['.', ',', ';', ':', '!', '?']);
        let word = word.strip_prefix("file://").unwrap_or(word);
        let lower = word.to_lowercase();
        if word.len() > 1024
            || !ENDINGS.iter().any(|e| {
                lower.ends_with(e)
                    && lower.len() > e.len()
                    && !lower[..lower.len() - e.len()].ends_with('/')
            })
            || word.contains("://")
            || word.contains('\0')
        {
            continue;
        }
        // (`%20` in an address is a blank in the name.)
        let name = word.replace("%20", " ");
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// What a file is by its first bytes: a picture or a video, or neither.
/// With a PNG's size, which its first bytes give.
pub fn sniff(head: &[u8]) -> Option<(&'static str, Option<(u32, u32)>)> {
    let at = |i: usize, s: &[u8]| head.len() >= i + s.len() && &head[i..i + s.len()] == s;
    if at(0, b"\x89PNG\r\n\x1a\n") {
        let n = |i: usize| {
            head.get(i..i + 4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        let size = match (at(12, b"IHDR"), n(16), n(20)) {
            (true, Some(w), Some(h)) if w > 0 && h > 0 && w < 100_000 && h < 100_000 => {
                Some((w, h))
            }
            _ => None,
        };
        return Some(("image", size));
    }
    if at(0, b"\xff\xd8\xff")
        || at(0, b"GIF87a")
        || at(0, b"GIF89a")
        || (at(0, b"RIFF") && at(8, b"WEBP"))
    {
        return Some(("image", None));
    }
    if at(4, b"ftyp") || at(0, b"\x1a\x45\xdf\xa3") {
        return Some(("video", None));
    }
    None
}

/// A name from a text as a file on this station: under the owner's home
/// for `~/`, under the pane's folder when it is not a whole path; then
/// the real file it leads to (links followed), which must be a plain
/// file, a picture or a video by its bytes, and within the sizes.
fn resolve(name: &str, home: &Path, cwd: &Path) -> Option<Made> {
    let path = match name.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if name.starts_with('/') => PathBuf::from(name),
        None => cwd.join(name),
    };
    let real = std::fs::canonicalize(&path).ok()?;
    let meta = std::fs::metadata(&real).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut head = [0u8; 32];
    let n = std::fs::File::open(&real).ok()?.read(&mut head).ok()?;
    let (kind, size) = sniff(&head[..n])?;
    let max = if kind == "image" {
        PICTURE_MAX
    } else {
        VIDEO_MAX
    };
    if meta.len() == 0 || meta.len() > max {
        return None;
    }
    let shown = real.display().to_string();
    let name: String = real
        .file_name()?
        .to_string_lossy()
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    Some(Made {
        id: String::new(),
        path: shown,
        name,
        size: meta.len(),
        kind: kind.to_string(),
        w: size.map(|s| s.0),
        h: size.map(|s| s.1),
    })
}

fn view(m: &Made) -> Value {
    let mut v = json!({"id": m.id, "name": m.name, "size": m.size, "kind": m.kind, "path": m.path});
    if let (Some(w), Some(h)) = (m.w, m.h) {
        v["w"] = json!(w);
        v["h"] = json!(h);
    }
    v
}

/// What was found for an item a moment ago: an answer is made five times
/// a second while a conversation is watched, and the files are not looked
/// for again each time.
type Seen = HashMap<String, (std::time::Instant, Vec<Made>, usize)>;
fn seen() -> &'static Mutex<Seen> {
    static SEEN: OnceLock<Mutex<Seen>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
}

impl Daemon {
    fn made_index(&self, pane: &str) -> Vec<Made> {
        self.db
            .kv_get(&key(pane))
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    /// The pictures and videos a text names, as files of this station,
    /// each with the station's id for it; and how many more it names
    /// than are shown.
    fn made_in(&self, pane: &str, cwd: &str, text: &str) -> (Vec<Made>, usize) {
        let mut found: Vec<Made> = vec![];
        for name in names(text) {
            if let Some(mut m) = resolve(&name, &self.home, Path::new(cwd)) {
                if found.iter().any(|f| f.path == m.path) {
                    continue;
                }
                // What the owner attached is the owner's, not something
                // made: an agent that reads it names its path. (The real
                // Antigravity, 17 Nov 2026.)
                let own = self.paths.drops_dir();
                let own = own.canonicalize().unwrap_or(own);
                if Path::new(&m.path).starts_with(&own) {
                    continue;
                }
                let hash = Sha256::digest(format!("{pane}\n{}", m.path).as_bytes());
                m.id = hash.iter().take(10).map(|b| format!("{b:02x}")).collect();
                found.push(m);
            }
        }
        let more = found.len().saturating_sub(PER_ITEM);
        found.truncate(PER_ITEM);
        (found, more)
    }

    /// Put what an agent made on the items of a conversation answer that
    /// name it: `media`, and `media_more` when there are more than shown.
    pub(crate) fn made_decorate(&self, pane: &str, cwd: &str, out: &mut Value) {
        let mut index: Option<Vec<Made>> = None;
        let mut changed = false;
        for list in ["items", "updated"] {
            let Some(items) = out.get_mut(list).and_then(|i| i.as_array_mut()) else {
                continue;
            };
            for item in items.iter_mut() {
                // What an agent said, what it printed, and a tool call's
                // one line. Never the owner's own message.
                let text = match item["kind"].as_str() {
                    Some("text") | Some("screen") => item["text"].as_str(),
                    Some("tool") => item["summary"].as_str(),
                    _ => None,
                };
                let Some(text) = text.filter(|t| !t.is_empty()) else {
                    continue;
                };
                let low = text.to_lowercase();
                if !ENDINGS.iter().any(|e| low.contains(e)) {
                    continue;
                }
                let at = format!(
                    "{}|{pane}|{}|{}",
                    self.home.display(),
                    item["id"].as_str().unwrap_or(""),
                    text.len()
                );
                let known = seen()
                    .lock()
                    .unwrap()
                    .get(&at)
                    .filter(|(when, _, _)| when.elapsed().as_secs() < 5)
                    .map(|(_, m, more)| (m.clone(), *more));
                let (found, more) = known.unwrap_or_else(|| {
                    let got = self.made_in(pane, cwd, text);
                    let mut s = seen().lock().unwrap();
                    if s.len() > 2000 {
                        s.clear();
                    }
                    s.insert(at, (std::time::Instant::now(), got.0.clone(), got.1));
                    got
                });
                if found.is_empty() {
                    continue;
                }
                let index = index.get_or_insert_with(|| self.made_index(pane));
                for m in &found {
                    match index.iter_mut().find(|k| k.id == m.id) {
                        Some(k) if k == m => {}
                        Some(k) => {
                            *k = m.clone();
                            changed = true;
                        }
                        None => {
                            index.push(m.clone());
                            changed = true;
                        }
                    }
                }
                item["media"] = json!(found.iter().map(view).collect::<Vec<_>>());
                if more > 0 {
                    item["media_more"] = json!(more);
                }
            }
        }
        if let (true, Some(mut index)) = (changed, index) {
            if index.len() > KEPT_MAX {
                index.drain(..index.len() - KEPT_MAX);
            }
            self.db.kv_put(
                &key(pane),
                &serde_json::to_string(&index).unwrap_or_default(),
            );
        }
    }

    /// The file behind an id the station made for this pane: still there,
    /// still a plain file, still a picture or a video.
    fn made_file(&self, pane: &str, id: &str) -> Result<(Made, PathBuf)> {
        let known = id.len() == 20 && id.bytes().all(|b| b.is_ascii_hexdigit());
        let m = self
            .made_index(pane)
            .into_iter()
            .find(|m| known && m.id == id)
            .ok_or_else(|| Error::NotFound(format!("media {id}")))?;
        let path = PathBuf::from(&m.path);
        let gone = || Error::NotFound("that file is not there any more".into());
        let meta = std::fs::symlink_metadata(&path).map_err(|_| gone())?;
        let mut head = [0u8; 32];
        let n = std::fs::File::open(&path)
            .and_then(|mut f| f.read(&mut head))
            .map_err(|_| gone())?;
        if !meta.is_file() || sniff(&head[..n]).map(|s| s.0) != Some(m.kind.as_str()) {
            return Err(gone());
        }
        Ok((m, path))
    }

    /// `pane.media.get {pane, id, offset?}`: a piece of a picture or a
    /// video an agent's reply named, by the station's id for it.
    pub fn made_get(&self, pane: &str, params: &Value) -> Result<Value> {
        let p = self.pane(pane)?;
        let id = params
            .get("id")
            .and_then(|i| i.as_str())
            .ok_or_else(|| invalid("missing parameter: id"))?;
        let (m, path) = self.made_file(&p.id, id)?;
        let offset = params.get("offset").and_then(|o| o.as_u64()).unwrap_or(0);
        let mut file = std::fs::File::open(&path)?;
        let size = file.metadata()?.len();
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![];
        file.take(crate::attach::PIECE_MAX as u64)
            .read_to_end(&mut buf)?;
        let next = offset + buf.len() as u64;
        Ok(json!({
            "id": m.id, "name": m.name, "size": size, "kind": m.kind,
            "data_b64": base64::engine::general_purpose::STANDARD.encode(&buf),
            "next": if next < size { json!(next) } else { Value::Null },
        }))
    }

    /// `pane.media.attach {pane, id}`: a picture or a video the agent made
    /// is made a file for the owner's next message to this pane, at the
    /// station: nothing travels to the owner's device and back.
    pub fn made_attach(&self, pane: &str, id: &str) -> Result<Value> {
        let p = self.pane(pane)?;
        let (m, path) = self.made_file(&p.id, id)?;
        self.attach_adopt(&p.id, &path, &m.name)
    }

    pub(crate) fn made_forget(&self, pane: &str) {
        self.db.put(crate::db::Op::KvDel { k: key(pane) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] =
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x06\x88\x00\x00\x03\xad\x08\x06\x00\x00\x00";

    /// The owner, 17 Nov 2026, after the real Codex and Antigravity each
    /// made a picture and Chat showed its path as text: the names in a
    /// reply, in the shapes the two programs wrote that day (the paths
    /// are made up).
    #[test]
    fn the_names_of_what_an_agent_made_are_found_in_its_reply() {
        assert_eq!(
            names("Made it.\n\nPath: /home/ada/.codex/generated_images/5b1e/exec-77aa.png\nTool: image_gen.imagegen"),
            ["/home/ada/.codex/generated_images/5b1e/exec-77aa.png"]
        );
        assert_eq!(
            names("Saved to `/home/ada/.gemini/antigravity-cli/brain/0aaa/heron_final_1794.jpg` ([heron_final_1794.jpg](file:///home/ada/.gemini/antigravity-cli/brain/0aaa/heron_final_1794.jpg))."),
            ["/home/ada/.gemini/antigravity-cli/brain/0aaa/heron_final_1794.jpg", "heron_final_1794.jpg"]
        );
        assert_eq!(names("Saved to: file:///home/ada/my%20marsh/a.PNG and ~/clips/tide.mp4, also out/reeds.webm."), ["/home/ada/my marsh/a.PNG", "~/clips/tide.mp4", "out/reeds.webm"]);
        // Not names of files here: an address on the web, other kinds of
        // file, a sentence about pictures.
        assert!(names("See https://example.org/heron.png and ![x](http://x.test/a.jpg); tally.py and notes.txt; a .png is a picture").is_empty());
        assert!(names("").is_empty());
    }

    /// The same request's guard: what is served is a picture or a video
    /// by its bytes, a plain file of this station, and nothing else,
    /// whatever its name says and wherever a link leads.
    #[test]
    fn only_a_real_picture_or_video_of_this_station_is_ever_named() {
        assert_eq!(sniff(PNG), Some(("image", Some((1672, 941)))));
        assert_eq!(
            sniff(b"\xff\xd8\xff\xe0\x00\x10JFIF").map(|s| s.0),
            Some("image")
        );
        assert_eq!(
            sniff(b"RIFF\x24\x00\x00\x00WEBPVP8 ").map(|s| s.0),
            Some("image")
        );
        assert_eq!(
            sniff(b"\x00\x00\x00\x20ftypisom\x00\x00\x02\x00").map(|s| s.0),
            Some("video")
        );
        assert_eq!(
            sniff(b"\x1a\x45\xdf\xa3\x9fB\x86\x81\x01").map(|s| s.0),
            Some("video")
        );
        assert_eq!(sniff(b"root:x:0:0:root:/root:/bin/bash"), None);
        assert_eq!(sniff(b"RIFF\x24\x00\x00\x00WAVEfmt "), None);
        assert_eq!(sniff(b""), None);

        let dir = tempfile::tempdir().unwrap();
        let (home, cwd) = (dir.path().join("home"), dir.path().join("home/marsh"));
        std::fs::create_dir_all(cwd.join("out")).unwrap();
        std::fs::write(cwd.join("out/heron.png"), PNG).unwrap();
        std::fs::write(home.join("made.jpg"), b"\xff\xd8\xff\xe0 a picture").unwrap();
        std::fs::write(
            cwd.join("secret.png"),
            b"the owner's notes, named as a picture",
        )
        .unwrap();
        std::fs::write(cwd.join("empty.png"), b"").unwrap();
        std::fs::create_dir_all(cwd.join("dir.png")).unwrap();
        let found = |name: &str| resolve(name, &home, &cwd);
        // Relative to the pane's folder, under the home, and whole.
        let m = found("out/heron.png").unwrap();
        assert_eq!(
            (m.kind.as_str(), m.name.as_str(), m.size, m.w, m.h),
            (
                "image",
                "heron.png",
                PNG.len() as u64,
                Some(1672),
                Some(941)
            )
        );
        assert_eq!(found("~/made.jpg").unwrap().kind, "image");
        assert_eq!(
            found(&cwd.join("out/heron.png").display().to_string())
                .unwrap()
                .path,
            m.path
        );
        // `..` leads where it leads, and what is there must still be a
        // picture: the real file is what is named and served.
        assert_eq!(found("out/../out/heron.png").unwrap().path, m.path);
        for not in [
            "secret.png",
            "empty.png",
            "dir.png",
            "absent.png",
            "../../../etc/passwd.png",
            "/etc/passwd",
        ] {
            assert!(found(not).is_none(), "{not}");
        }
        #[cfg(unix)]
        {
            // A link named as a picture that leads to something else is
            // not one; a link to a picture is that picture.
            std::os::unix::fs::symlink("/etc/passwd", cwd.join("link.png")).unwrap();
            std::os::unix::fs::symlink(cwd.join("out/heron.png"), cwd.join("alias.png")).unwrap();
            assert!(found("link.png").is_none());
            assert_eq!(found("alias.png").unwrap().path, m.path);
        }
    }
}
