//! Fonts the owner uploaded, so a theme can name a font that is not installed
//! on the device showing the Control Deck (a phone, most of all).
//!
//! Which fonts there are is part of the look document (`Look::fonts`), so it
//! is the same on every station and travels by the document's rules. The
//! files do not travel in the document: each is kept in this station's
//! `fonts` folder under an id that is the start of the SHA-256 of its bytes,
//! and a station that reads of a font it does not hold asks another station
//! for it (`look.font.fetch`) and checks what arrives against that id.
//!
//! A font file comes from the owner's disk or from another station, and it is
//! data: only the four web font formats are taken, told by their first bytes;
//! the size and the count are bounded; the name it goes by follows the rule
//! for every font name; and the file is served by its id, never by a path a
//! caller chose. Nothing here reads into the font: the browser that draws
//! with it checks it before use, as it does for any web font.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::daemon::{invalid, Daemon, Error, Result};
use crate::look::is_font_name;

/// The largest font file taken.
pub const MAX_BYTES: usize = 5 << 20;
/// How many uploaded files the fleet keeps.
pub const MAX_FONTS: usize = 24;

const FACES: &[&str] = &[
    "regular",
    "bold",
    "italic",
    "bold italic",
    "variable",
    "variable italic",
];

/// Names a style sheet reads as a kind of font, not as a font: a file that
/// went by one of them could never be chosen. The last is the bundled
/// symbols font's.
const TAKEN: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "ui-rounded",
    "emoji",
    "math",
    "fangsong",
    "inherit",
    "initial",
    "unset",
    "revert",
    "default",
    "spandeck symbols",
];

/// Which of the four formats these bytes start as, if any.
pub fn format_of(bytes: &[u8]) -> Option<&'static str> {
    match bytes.get(..4)? {
        b"wOF2" => Some("woff2"),
        b"wOFF" => Some("woff"),
        b"OTTO" => Some("otf"),
        [0, 1, 0, 0] | b"true" => Some("ttf"),
        _ => None,
    }
}

fn mime_of(format: &str) -> &'static str {
    match format {
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "otf" => "font/otf",
        _ => "font/ttf",
    }
}

/// The name a file is kept and asked for under: from its bytes alone.
pub fn id_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Why these bytes are not kept as a font, or what format they are in.
pub fn check_file(bytes: &[u8]) -> std::result::Result<&'static str, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!(
            "a font file is at most {} MB",
            MAX_BYTES / (1 << 20)
        ));
    }
    format_of(bytes).ok_or_else(|| "that is not a font file: WOFF2, WOFF, TTF or OTF".to_string())
}

/// One uploaded file: a face of the family `name`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Font {
    pub id: String,
    /// The family a theme names it by.
    pub name: String,
    /// Which face of the family the file is.
    pub face: String,
    pub format: String,
    pub size: u64,
}

impl Font {
    pub(crate) fn clean(self) -> std::result::Result<Font, String> {
        if !is_id(&self.id) {
            return Err("a font's id is 32 hex digits".into());
        }
        if !is_font_name(&self.name) {
            return Err(
                "a font's name is 1 to 64 letters, digits, spaces, - _ . and nothing else".into(),
            );
        }
        if TAKEN.contains(&self.name.to_ascii_lowercase().as_str()) {
            return Err(format!(
                "{:?} already means something as a font name; choose another",
                self.name
            ));
        }
        if !FACES.contains(&self.face.as_str()) {
            return Err(format!("a font's face is one of: {}", FACES.join(", ")));
        }
        if !["woff2", "woff", "ttf", "otf"].contains(&self.format.as_str())
            || self.size == 0
            || self.size > MAX_BYTES as u64
        {
            return Err("a font is WOFF2, WOFF, TTF or OTF and at most 5 MB".into());
        }
        Ok(self)
    }
}

/// The list as the look document holds it: bounded, and no two files for the
/// same face of the same family.
pub(crate) fn clean_list(fonts: Vec<Font>) -> std::result::Result<Vec<Font>, String> {
    if fonts.len() > MAX_FONTS {
        return Err(format!("there are at most {MAX_FONTS} uploaded fonts"));
    }
    let fonts = fonts
        .into_iter()
        .map(Font::clean)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (i, f) in fonts.iter().enumerate() {
        if fonts[..i]
            .iter()
            .any(|o| o.id == f.id || (o.name.eq_ignore_ascii_case(&f.name) && o.face == f.face))
        {
            return Err(format!(
                "the {} face of {:?} is there twice",
                f.face, f.name
            ));
        }
    }
    Ok(fonts)
}

impl Daemon {
    fn fonts_dir(&self) -> PathBuf {
        self.paths.config_dir.join("fonts")
    }

    fn font_write(&self, id: &str, bytes: &[u8]) -> Result<()> {
        let dir = self.fonts_dir();
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("{id}.tmp"));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, dir.join(id))?;
        Ok(())
    }

    /// The owner uploads a font file under a family name.
    pub fn look_font_add(&self, name: &str, face: &str, bytes: &[u8], by: &str) -> Result<Value> {
        let format = check_file(bytes).map_err(invalid)?;
        let font = Font {
            id: id_of(bytes),
            name: name.trim().to_string(),
            face: face.to_string(),
            format: format.to_string(),
            size: bytes.len() as u64,
        }
        .clean()
        .map_err(invalid)?;
        // The file first: the document never names one this station lacks.
        self.font_write(&font.id, bytes)?;
        self.look_change(by, |look| {
            if look.fonts.contains(&font) {
                return Ok(None);
            }
            // The same file again, or another file for the same face, takes
            // the place of the one before.
            look.fonts.retain(|f| {
                f.id != font.id && !(f.name.eq_ignore_ascii_case(&font.name) && f.face == font.face)
            });
            if look.fonts.len() >= MAX_FONTS {
                return Err(invalid(format!(
                    "there are at most {MAX_FONTS} uploaded fonts; remove one first"
                )));
            }
            look.fonts.push(font.clone());
            Ok(Some(
                json!({"font": font.id, "name": font.name, "face": font.face, "added": true}),
            ))
        })?;
        Ok(json!(font))
    }

    /// Take a font off the list, on every station. Themes that name it keep
    /// the name and fall back to the next font in their list.
    pub fn look_font_remove(&self, id: &str, by: &str) -> Result<Value> {
        self.look_change(by, |look| {
            let before = look.fonts.len();
            look.fonts.retain(|f| f.id != id);
            if look.fonts.len() == before {
                return Err(Error::NotFound(format!("font {id}")));
            }
            Ok(Some(json!({"font": id, "removed": true})))
        })?;
        let _ = std::fs::remove_file(self.fonts_dir().join(id));
        Ok(json!({"removed": id}))
    }

    /// A font on the list, for the browser or for another station: its
    /// content type and its bytes. The id is looked up in the list, so no
    /// path is ever made from what a caller sent.
    pub fn look_font_file(&self, id: &str) -> Result<(&'static str, Vec<u8>)> {
        let look = self.look();
        let font = look
            .fonts
            .iter()
            .find(|f| f.id == id)
            .ok_or_else(|| Error::NotFound(format!("font {id}")))?;
        let bytes = std::fs::read(self.fonts_dir().join(&font.id))
            .map_err(|_| Error::NotFound(format!("font {id} has not reached this station yet")))?;
        Ok((mime_of(&font.format), bytes))
    }

    /// Another station asking for a file it read of in the look document.
    pub fn look_font_fetch(&self, id: &str) -> Result<Value> {
        let (_, bytes) = self.look_font_file(id)?;
        Ok(json!({"id": id, "data_b64": base64::engine::general_purpose::STANDARD.encode(bytes)}))
    }

    /// Get the files this station reads of and does not hold, from `first`
    /// (the station that sent the document) and then from any other that
    /// keeps fonts; and delete files no longer on the list. What arrives is
    /// kept only if it is the file the document means: its bytes give the id.
    pub(crate) async fn look_fonts_sync(&self, first: Option<&str>) {
        let look = self.look();
        let dir = self.fonts_dir();
        let mut got = 0;
        for font in &look.fonts {
            if dir.join(&font.id).is_file() {
                continue;
            }
            let mut from: Vec<String> = first.map(String::from).into_iter().collect();
            for peer in self.fleet.peers() {
                let keeps = peer.info.pointer("/look/format").and_then(|f| f.as_u64()) >= Some(2);
                if peer.state == "online" && keeps && !from.contains(&peer.station) {
                    from.push(peer.station);
                }
            }
            for station in from {
                let Ok(reply) = self
                    .fleet
                    .call(&station, "look.font.fetch", json!({"id": font.id}))
                    .await
                else {
                    continue;
                };
                let bytes = reply
                    .get("data_b64")
                    .and_then(|d| d.as_str())
                    .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok())
                    .unwrap_or_default();
                if check_file(&bytes).ok() != Some(font.format.as_str()) || id_of(&bytes) != font.id
                {
                    continue;
                }
                if self.font_write(&font.id, &bytes).is_ok() {
                    self.record(
                        "look.font.received",
                        None,
                        None,
                        None,
                        Some(&station),
                        json!({"font": font.id, "name": font.name, "bytes": bytes.len()}),
                    );
                    got += 1;
                    break;
                }
            }
        }
        if got > 0 {
            // Browsers open here read the document again and find the font.
            self.emit(
                "look.updated",
                None,
                None,
                json!({"updated_ms": look.updated_ms, "fonts": got}),
            );
        }
        // A file not on the list is one that was removed (here or elsewhere).
        // Not one just written, whose place on the list is being made.
        let Ok(files) = std::fs::read_dir(&dir) else {
            return;
        };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().to_string();
            let old = file
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age > Duration::from_secs(600));
            if old && !look.fonts.iter().any(|f| f.id == name) {
                let _ = std::fs::remove_file(file.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The owner's request (15 Nov 2026): fonts can be uploaded, and the file
    // is not trusted. Only the four font formats are taken, by their first
    // bytes; a file is bounded; its id comes from its bytes; and the name it
    // goes by cannot be more than a font's name.
    #[test]
    fn only_a_bounded_font_file_under_a_plain_name_is_taken() {
        let file = |magic: &[u8]| [magic, &[0u8; 60][..]].concat();
        for (magic, format) in [
            (&b"wOF2"[..], "woff2"),
            (b"wOFF", "woff"),
            (b"OTTO", "otf"),
            (&[0, 1, 0, 0], "ttf"),
            (b"true", "ttf"),
        ] {
            assert_eq!(check_file(&file(magic)), Ok(format));
        }
        for not_a_font in [
            &b"<svg xmlns='http://www.w3.org/2000/svg' onload='alert(1)'/>"[..],
            b"<!doctype html><script>alert(1)</script>",
            b"ttcf\0\x01\0\0",
            b"MZ\x90\0",
            b"wOF",
            b"",
        ] {
            assert!(check_file(not_a_font).is_err(), "{not_a_font:?} was taken");
        }
        let mut big = file(b"wOF2");
        big.resize(MAX_BYTES + 1, 0);
        assert!(check_file(&big).is_err());
        big.truncate(MAX_BYTES);
        assert!(check_file(&big).is_ok());

        // The id is the file's own and names nothing on a disk.
        let id = id_of(&file(b"wOF2"));
        assert!(is_id(&id), "{id}");
        assert_ne!(id, id_of(&file(b"wOFF")));

        let font = |name: &str, face: &str| Font {
            id: id.clone(),
            name: name.into(),
            face: face.into(),
            format: "woff2".into(),
            size: 64,
        };
        assert!(font("Fira Code", "regular").clean().is_ok());
        for (name, face) in [
            ("x\", url(//evil.example/f.woff)", "regular"),
            ("a; } body { display: none", "regular"),
            ("../../etc/passwd", "regular"),
            ("", "regular"),
            ("monospace", "regular"),
            ("Spandeck Symbols", "regular"),
            ("Fira Code", "900; src: url(x)"),
        ] {
            assert!(font(name, face).clean().is_err(), "{name:?} {face:?}");
        }
        for id in ["../look.json", "ABCDEF0123456789ABCDEF0123456789", "abc"] {
            let bad = Font {
                id: id.into(),
                ..font("Fira Code", "regular")
            };
            assert!(bad.clean().is_err(), "{id}");
        }
        // One file for a face of a family, and a bounded list.
        let other = Font {
            id: id_of(b"wOFFother"),
            ..font("fira code", "regular")
        };
        assert!(clean_list(vec![font("Fira Code", "regular"), other.clone()]).is_err());
        let bold = Font {
            face: "bold".into(),
            ..other
        };
        assert!(clean_list(vec![font("Fira Code", "regular"), bold]).is_ok());
        let many = (0..=MAX_FONTS)
            .map(|i| Font {
                id: id_of(&[i as u8]),
                ..font(&format!("F{i}"), "regular")
            })
            .collect();
        assert!(clean_list(many).is_err());
    }
}
