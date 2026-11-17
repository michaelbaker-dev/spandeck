//! A photo or a document sent to an agent with a message (DECISIONS.md,
//! "A photo or a document goes with the message", 17 Nov 2026).
//!
//! The owner: "In the chat I want to add a photo or document to the agent
//! just like Claude or open ai". Until this, the "+" of the message box
//! saved the file and at once typed its path into the program.
//!
//! - **A file waits with the message.** The deck uploads it to the pane's
//!   own station in pieces (`pane.attach.put`), where it is kept under the
//!   station's state, in a folder of the pane's own, under a name made
//!   safe, never over another, readable by the owner's account alone.
//!   Nothing is typed.
//! - **It goes with the message.** `pane.paste` with `files` sends one
//!   message: the owner's words, then a line for each file with where it
//!   is. No program's own way of taking a file is known for certain (see
//!   `mention`), so the line is the same for all: a path a program can
//!   open.
//! - **The conversation shows what was attached.** The station keeps which
//!   files went with which message (in its key-value table, for each
//!   pane), takes the lines it added out of the message as the
//!   conversation gives it, and puts `attachments` on the item instead.
//!   A file is read back only by its pane and its id (`pane.attach.get`):
//!   no path comes from the page.
//! - All three calls are the owner's alone. The files go when the pane is
//!   closed.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::{invalid, Daemon, Error, Result};
use crate::paths::now_ms;

/// A file is at most this large.
pub const FILE_MAX: u64 = 64 << 20;
/// A piece of an upload, and of a read, is at most this many bytes.
pub const PIECE_MAX: usize = 256 << 10;
/// Files kept for a pane: the oldest that were sent go first.
const KEPT_MAX: usize = 200;
/// Files with one message.
pub const WITH_MESSAGE_MAX: usize = 10;
/// The words before a file's path in the message the program is given.
pub const LEAD: &str = "Attached file: ";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct File {
    id: String,
    /// As the owner's device named it, cleaned: shown, never opened.
    name: String,
    /// The name it has on disk, in the pane's folder.
    on_disk: String,
    size: u64,
    /// Its name ends as a picture's does.
    image: bool,
    ts: u64,
    /// Whole: its last piece has arrived.
    #[serde(default)]
    whole: bool,
    /// It went with a message.
    #[serde(default)]
    sent: bool,
}

/// A message that went with files: to find it again in a conversation
/// whose program rewrote the lines (Claude Code makes a picture's path
/// into "[Image #1]").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Sent {
    ts: u64,
    /// The beginning of the owner's own words.
    head: String,
    files: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    files: Vec<File>,
    #[serde(default)]
    sent: Vec<Sent>,
}

fn key(pane: &str) -> String {
    format!("attach.{pane}")
}

/// A name that is safe as a file's: letters, digits, `.`, `-`, `_`; no
/// leading dot; never empty; 80 characters.
fn safe(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let clean = clean.trim_start_matches('.').to_string();
    if clean.is_empty() {
        "file".into()
    } else {
        clean
    }
}

/// The name as it is shown: no control characters, 120 characters.
fn shown(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .take(120)
        .collect();
    if s.trim().is_empty() {
        "file".into()
    } else {
        s.trim().to_string()
    }
}

fn is_image(name: &str) -> bool {
    let low = name.to_lowercase();
    [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic", ".heif", ".bmp",
    ]
    .iter()
    .any(|e| low.ends_with(e))
}

fn id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn view(f: &File) -> Value {
    json!({"id": f.id, "name": f.name, "size": f.size, "image": f.image})
}

/// How a file is named to a program in the message. **No program's own
/// way is established** (Claude Code is said to read a picture whose path
/// is in the prompt; nothing was read for Codex, Antigravity or
/// OpenCode), so every program is given the same: the words `Attached
/// file: ` and the file's whole path, on a line of its own.
pub fn mention(path: &str) -> String {
    format!("{LEAD}{path}")
}

impl Daemon {
    fn attach_dir(&self, pane: &str) -> PathBuf {
        self.paths.drops_dir().join(pane).join("attached")
    }

    fn attach_index(&self, pane: &str) -> Index {
        self.db
            .kv_get(&key(pane))
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    fn attach_save(&self, pane: &str, index: &Index) {
        self.db.kv_put(
            &key(pane),
            &serde_json::to_string(index).unwrap_or_default(),
        );
    }

    /// `pane.attach.put {pane, name, data_b64, id?, last?}`: a piece of a
    /// file for a message to this pane. The first piece names it and is
    /// answered with its `id`; the others carry the id; `last` closes it.
    pub fn attach_put(&self, pane: &str, params: &Value) -> Result<Value> {
        let p = self.pane(pane)?;
        let data = base64::engine::general_purpose::STANDARD
            .decode(
                params
                    .get("data_b64")
                    .and_then(|d| d.as_str())
                    .unwrap_or(""),
            )
            .map_err(|_| invalid("data_b64 is not base64"))?;
        if data.len() > PIECE_MAX {
            return Err(invalid("a piece is at most 256 KB"));
        }
        let last = params.get("last").and_then(|l| l.as_bool()) == Some(true);
        let dir = self.attach_dir(&p.id);
        let mut index = self.attach_index(&p.id);
        let at = match params.get("id").and_then(|i| i.as_str()) {
            Some(id) => index
                .files
                .iter()
                .position(|f| f.id == id && !f.whole)
                .ok_or_else(|| Error::NotFound(format!("upload {id}")))?,
            None => {
                let name = params
                    .get("name")
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| invalid("missing parameter: name"))?;
                std::fs::create_dir_all(&dir)?;
                let id =
                    crate::paths::new_id("a").replace(|c: char| !c.is_ascii_alphanumeric(), "");
                let on_disk = format!("{id}-{}", safe(name));
                // Never over another: the id is new, and the file must be.
                let mut open = std::fs::OpenOptions::new();
                open.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    open.mode(0o600);
                }
                open.open(dir.join(&on_disk))?;
                // What was never sent and is old, and the oldest beyond
                // what is kept, go.
                index.files.push(File {
                    id,
                    name: shown(name),
                    on_disk,
                    size: 0,
                    image: is_image(name),
                    ts: now_ms(),
                    whole: false,
                    sent: false,
                });
                while index.files.len() > KEPT_MAX {
                    let gone = index.files.remove(0);
                    let _ = std::fs::remove_file(dir.join(&gone.on_disk));
                }
                index.files.len() - 1
            }
        };
        let f = &mut index.files[at];
        if f.size + data.len() as u64 > FILE_MAX {
            let _ = std::fs::remove_file(dir.join(&f.on_disk));
            index.files.remove(at);
            self.attach_save(&p.id, &index);
            return Err(invalid("a file is at most 64 MB"));
        }
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(&f.on_disk))?
            .write_all(&data)?;
        f.size += data.len() as u64;
        f.whole = last;
        let out =
            json!({"id": f.id, "name": f.name, "size": f.size, "image": f.image, "whole": f.whole});
        self.attach_save(&p.id, &index);
        Ok(out)
    }

    /// `pane.attach.remove {pane, id}`: a file that was not sent is
    /// taken away.
    pub fn attach_remove(&self, pane: &str, id: &str) -> Result<Value> {
        let p = self.pane(pane)?;
        let mut index = self.attach_index(&p.id);
        if let Some(at) = index.files.iter().position(|f| f.id == id && !f.sent) {
            let gone = index.files.remove(at);
            let _ = std::fs::remove_file(self.attach_dir(&p.id).join(&gone.on_disk));
            self.attach_save(&p.id, &index);
        }
        Ok(json!({"ok": true}))
    }

    /// `pane.attach.get {pane, id, offset?}`: a piece of a file that was
    /// attached for this pane, by its id. Only such a file: the name on
    /// disk is the station's own, from its own record.
    pub fn attach_get(&self, pane: &str, params: &Value) -> Result<Value> {
        let p = self.pane(pane)?;
        let id = params
            .get("id")
            .and_then(|i| i.as_str())
            .filter(|i| id_ok(i))
            .ok_or_else(|| invalid("missing parameter: id"))?;
        let index = self.attach_index(&p.id);
        let f = index
            .files
            .iter()
            .find(|f| f.id == id && f.whole)
            .ok_or_else(|| Error::NotFound(format!("file {id}")))?;
        let offset = params.get("offset").and_then(|o| o.as_u64()).unwrap_or(0);
        let mut file = std::fs::File::open(self.attach_dir(&p.id).join(&f.on_disk))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![];
        file.take(PIECE_MAX as u64).read_to_end(&mut buf)?;
        let next = offset + buf.len() as u64;
        Ok(json!({
            "id": f.id, "name": f.name, "size": f.size, "image": f.image,
            "data_b64": base64::engine::general_purpose::STANDARD.encode(&buf),
            "next": if next < f.size { json!(next) } else { Value::Null },
        }))
    }

    /// The message a program is given for the owner's words and these
    /// files: the words, then a line for each. The files are marked as
    /// sent, with the message they went with. Refused whole when one of
    /// them is not there to send: a path is never typed for a file that
    /// was removed or did not arrive.
    pub fn attach_message(&self, pane: &str, text: &str, ids: &[String]) -> Result<String> {
        let p = self.pane(pane)?;
        if ids.len() > WITH_MESSAGE_MAX {
            return Err(invalid("at most 10 files go with one message"));
        }
        let mut index = self.attach_index(&p.id);
        let dir = self.attach_dir(&p.id);
        let mut lines = vec![];
        for id in ids {
            let f = index.files.iter_mut().find(|f| &f.id == id && f.whole).ok_or_else(|| invalid("a file of this message is not on the station (it was removed, or did not arrive): take it off and send again"))?;
            f.sent = true;
            lines.push(mention(&dir.join(&f.on_disk).display().to_string()));
        }
        index.sent.push(Sent {
            ts: now_ms(),
            head: text.trim().chars().take(60).collect(),
            files: ids.to_vec(),
        });
        if index.sent.len() > KEPT_MAX {
            index.sent.remove(0);
        }
        self.attach_save(&p.id, &index);
        let words = text.trim();
        Ok(match words.is_empty() {
            true => lines.join("\n"),
            false => format!("{words}\n\n{}", lines.join("\n")),
        })
    }

    /// Put what was attached on the owner's messages of a conversation
    /// answer, and take the lines the station added out of their words.
    /// By the path where the message still has it; else by the message it
    /// went with (its time, and how its words begin).
    pub(crate) fn attach_decorate(&self, pane: &str, out: &mut Value) {
        let index = self.attach_index(pane);
        if index.files.is_empty() {
            return;
        }
        let dir = self.attach_dir(pane);
        let mut used: Vec<String> = vec![];
        for list in ["items", "updated"] {
            let Some(items) = out.get_mut(list).and_then(|i| i.as_array_mut()) else {
                continue;
            };
            for item in items.iter_mut().filter(|i| i["kind"] == json!("user")) {
                let Some(text) = item["text"].as_str().map(String::from) else {
                    continue;
                };
                if !text.contains(LEAD) {
                    continue;
                }
                let mut words = text.clone();
                let mut files = vec![];
                for f in index.files.iter().filter(|f| f.sent) {
                    let line = mention(&dir.join(&f.on_disk).display().to_string());
                    if words.contains(&line) {
                        words = words.replace(&line, "");
                        files.push(f);
                    }
                }
                // The program rewrote the path (a picture taken in as
                // "[Image #1]"): the message these words began, sent a
                // moment before the record was written.
                if files.is_empty() {
                    let ts = item["ts_ms"].as_u64().unwrap_or(0);
                    let head: String = text.trim().chars().take(60).collect();
                    let sent = index.sent.iter().rev().find(|s| {
                        !used.contains(&format!("{}", s.ts))
                            && (s.head.is_empty() || head.starts_with(&s.head))
                            && (ts == 0 || (s.ts <= ts + 2_000 && ts <= s.ts + 30_000))
                    });
                    if let Some(sent) = sent {
                        used.push(format!("{}", sent.ts));
                        files = index
                            .files
                            .iter()
                            .filter(|f| sent.files.contains(&f.id))
                            .collect();
                        // What is left of the station's own lines.
                        words = words
                            .lines()
                            .filter(|l| !l.trim_start().starts_with(LEAD))
                            .collect::<Vec<_>>()
                            .join("\n");
                        if let Some(at) = words.find(LEAD) {
                            words.truncate(at);
                        }
                    }
                }
                if files.is_empty() {
                    continue;
                }
                item["text"] = json!(words.trim());
                item["attachments"] = json!(files.iter().map(|f| view(f)).collect::<Vec<_>>());
            }
        }
    }

    /// A closed pane's files go with it.
    pub(crate) fn attach_forget(&self, pane: &str) {
        let _ = std::fs::remove_dir_all(self.attach_dir(pane));
        self.db.put(crate::db::Op::KvDel { k: key(pane) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner, 17 Nov 2026: "add a photo or document to the agent just
    /// like Claude or open ai". A name from a device is never a path, and
    /// what the program is given is one line a file.
    #[test]
    fn a_files_name_is_made_safe_and_named_on_a_line_of_its_own() {
        assert_eq!(safe("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe(".env"), "env");
        assert_eq!(safe("heron count (1).png"), "heron_count__1_.png");
        assert_eq!(safe("///"), "___");
        assert_eq!(safe(""), "file");
        assert_eq!(shown("a/b\\c\u{7}.txt"), "abc.txt");
        assert!(
            is_image("IMG_0001.HEIC")
                && is_image("a.jpeg")
                && !is_image("notes.txt")
                && !is_image("png")
        );
        assert!(id_ok("a01jx7") && !id_ok("../x") && !id_ok("") && !id_ok("a b"));
        assert_eq!(
            mention("/s/drops/p1/attached/a1-x.png"),
            "Attached file: /s/drops/p1/attached/a1-x.png"
        );
    }
}
