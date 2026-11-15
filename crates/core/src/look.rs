//! The look document: the owner's themes, which one the Control Deck uses,
//! and what a layout or a pane changes about it. One document, kept identical
//! on every station, like the deck document and by the same rules: only the
//! owner changes it, the station where they did pushes it to the others, and
//! a station keeps whichever copy is newer.
//!
//! It is a document of its own (`look.json`, `look.push`) and not part of
//! the deck document, because a station from before themes reads the deck
//! document into the fields it knows and writes only those back: it would
//! drop the themes and, at its next change, push the document without them
//! to everyone. Such a station never holds this one. The same could happen
//! between two versions of this document, so it carries a format number: a
//! station takes no copy in a format newer than it knows, and none in a
//! format older than the copy it holds.
//!
//! Format 2 added the list of uploaded fonts (fonts.rs). A document is in
//! format 1 until a font is uploaded, so a fleet with a station from before
//! uploaded fonts keeps sharing its themes until then; from then on that
//! station keeps the copy it has until it is updated.
//!
//! Everything in it arrives from a file the owner imported or from another
//! station, so it is data and is checked here: a colour is `#` and hex
//! digits, a number is within its range, a font is a short name, and a
//! setting this version does not know is dropped. The built-in themes are in
//! the Control Deck's code, not here: this station knows a theme by its id.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::{invalid, write_private, Daemon, Error, Result};
use crate::paths::new_id;
use crate::project::valid_name;

/// The newest shape of the document this version reads and writes.
pub const FORMAT: u32 = 2;

const MAX_THEMES: usize = 64;
const MAX_LAYOUTS: usize = 256;
const MAX_PANES: usize = 512;
const MAX_NAME: usize = 48;
const MAX_FONTS: usize = 8;
const MAX_FONT_NAME: usize = 64;

/// What a setting's value may be.
enum Kind {
    Colour,
    Number(f64, f64),
    /// 100 to 900 in hundreds.
    Weight,
    Switch,
    OneOf(&'static [&'static str]),
    /// A list of font names, first choice first.
    Fonts,
}

const TERM_COLOURS: &[&str] = &[
    "background",
    "foreground",
    "cursor",
    "cursorAccent",
    "selection",
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "brightBlack",
    "brightRed",
    "brightGreen",
    "brightYellow",
    "brightBlue",
    "brightMagenta",
    "brightCyan",
    "brightWhite",
];
const DECK_COLOURS: &[&str] = &[
    "bg", "panel", "panel2", "line", "text", "muted", "blue", "green", "amber", "red", "violet",
    "focus",
];

/// Every setting there is. The Control Deck has the same list (look.js).
fn kind_of(key: &str) -> Option<Kind> {
    if let Some(name) = key.strip_prefix("term.") {
        if TERM_COLOURS.contains(&name) {
            return Some(Kind::Colour);
        }
    }
    if let Some(name) = key.strip_prefix("deck.") {
        if DECK_COLOURS.contains(&name) {
            return Some(Kind::Colour);
        }
    }
    Some(match key {
        "term.contrast" => Kind::Number(1.0, 21.0),
        "deck.scheme" => Kind::OneOf(&["dark", "light"]),
        "deck.focusWidth" => Kind::Number(0.0, 4.0),
        "font.family" | "ui.family" => Kind::Fonts,
        "font.size" => Kind::Number(6.0, 40.0),
        "font.lineHeight" => Kind::Number(1.0, 2.0),
        "font.letterSpacing" => Kind::Number(-2.0, 8.0),
        "font.weight" | "font.weightBold" => Kind::Weight,
        "font.ligatures" | "cursor.blink" => Kind::Switch,
        "ui.size" => Kind::Number(10.0, 22.0),
        "cursor.style" => Kind::OneOf(&["block", "bar", "underline"]),
        _ => return None,
    })
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`, and nothing else: no names, no
/// functions, nothing a style sheet could read as more than a colour.
fn is_colour(v: &str) -> bool {
    let Some(hex) = v.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 3 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A font's name: letters, digits, space, `-`, `_` and `.`. No quote, comma,
/// semicolon, bracket or slash, so a name cannot end the value it is put in.
pub(crate) fn is_font_name(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= MAX_FONT_NAME
        && v.trim() == v
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'-' | b'_' | b'.'))
}

fn check_value(key: &str, kind: &Kind, v: &Value) -> std::result::Result<(), String> {
    let ok = match kind {
        Kind::Colour => v.as_str().is_some_and(is_colour),
        Kind::Number(min, max) => v
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= *min && n <= *max),
        Kind::Weight => v
            .as_u64()
            .is_some_and(|n| (100..=900).contains(&n) && n % 100 == 0),
        Kind::Switch => v.is_boolean(),
        Kind::OneOf(list) => v.as_str().is_some_and(|s| list.contains(&s)),
        Kind::Fonts => v.as_array().is_some_and(|list| {
            !list.is_empty()
                && list.len() <= MAX_FONTS
                && list.iter().all(|f| f.as_str().is_some_and(is_font_name))
        }),
    };
    if ok {
        return Ok(());
    }
    Err(match kind {
        Kind::Colour => format!("{key} must be a colour written #rgb, #rrggbb or #rrggbbaa"),
        Kind::Number(min, max) => format!("{key} must be a number from {min} to {max}"),
        Kind::Weight => format!("{key} must be 100 to 900, in hundreds"),
        Kind::Switch => format!("{key} must be true or false"),
        Kind::OneOf(list) => format!("{key} must be one of: {}", list.join(", ")),
        Kind::Fonts => format!(
            "{key} must be a list of 1 to {MAX_FONTS} font names; a name uses letters, digits, spaces, - _ . only"
        ),
    })
}

/// Settings by name (`term.background`, `font.size`). Any may be missing.
pub type Settings = BTreeMap<String, Value>;

/// Keep the settings this version knows, refuse a value that is not what its
/// setting takes.
fn clean_settings(settings: Settings) -> std::result::Result<Settings, String> {
    let mut out = Settings::new();
    for (key, value) in settings {
        let Some(kind) = kind_of(&key) else { continue };
        check_value(&key, &kind, &value)?;
        out.insert(key, value);
    }
    Ok(out)
}

fn is_theme_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A theme the owner saved. Built-in themes are not stored: they have ids
/// without the `u-` every saved one starts with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub settings: Settings,
}

impl Theme {
    fn clean(mut self) -> std::result::Result<Theme, String> {
        if !is_theme_id(&self.id) || !self.id.starts_with("u-") {
            return Err("a saved theme's id starts with u- and uses a-z, 0-9 and - only".into());
        }
        self.name = self.name.trim().to_string();
        if self.name.is_empty()
            || self.name.chars().count() > MAX_NAME
            || self.name.chars().any(char::is_control)
        {
            return Err(format!(
                "a theme's name is 1 to {MAX_NAME} characters on one line"
            ));
        }
        self.settings = clean_settings(self.settings)?;
        Ok(self)
    }
}

/// What one level (the deck, a layout, a pane) asks for: a theme by id,
/// single settings, or both. Settings win over the theme.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Choice {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub set: Settings,
}

impl Choice {
    fn clean(mut self) -> std::result::Result<Choice, String> {
        if self.theme.as_deref().is_some_and(|id| !is_theme_id(id)) {
            return Err("a theme is named by its id".into());
        }
        self.set = clean_settings(self.set)?;
        Ok(self)
    }

    fn is_empty(&self) -> bool {
        self.theme.is_none() && self.set.is_empty()
    }
}

/// `global:<name>` for a layout the whole fleet keeps, or
/// `<station>/<project>/<name>` for a project's.
fn is_layout_key(key: &str) -> bool {
    match key.strip_prefix("global:") {
        Some(name) => valid_name(name),
        None => {
            let parts: Vec<&str> = key.split('/').collect();
            parts.len() == 3 && parts.iter().all(|p| valid_name(p))
        }
    }
}

/// `<station>/<pane id>`.
fn is_pane_key(key: &str) -> bool {
    let parts: Vec<&str> = key.split('/').collect();
    parts.len() == 2 && parts.iter().all(|p| valid_name(p))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Look {
    pub format: u32,
    pub updated_ms: u64,
    pub themes: Vec<Theme>,
    /// The whole Control Deck.
    pub deck: Choice,
    pub layouts: BTreeMap<String, Choice>,
    pub panes: BTreeMap<String, Choice>,
    /// Font files the owner uploaded (format 2). The files themselves are
    /// kept beside the document, by id: see fonts.rs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fonts: Vec<crate::fonts::Font>,
    /// Which stations' changes this copy holds (sync.rs).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub changes: crate::sync::Changes,
}

impl Look {
    /// The document with everything in it checked, or why it is refused.
    pub fn clean(self) -> std::result::Result<Look, String> {
        if self.themes.len() > MAX_THEMES {
            return Err(format!("there are at most {MAX_THEMES} saved themes"));
        }
        if self.layouts.len() > MAX_LAYOUTS || self.panes.len() > MAX_PANES {
            return Err(format!(
                "at most {MAX_LAYOUTS} layouts and {MAX_PANES} panes can have a look of their own"
            ));
        }
        let themes = self
            .themes
            .into_iter()
            .map(Theme::clean)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (i, t) in themes.iter().enumerate() {
            if themes[..i].iter().any(|o| o.id == t.id || o.name == t.name) {
                return Err(format!("two saved themes are called {:?}", t.name));
            }
        }
        let level = |map: BTreeMap<String, Choice>, key_ok: fn(&str) -> bool, what: &str| {
            map.into_iter()
                .map(|(k, c)| {
                    if !key_ok(&k) {
                        return Err(format!("{k:?} does not name a {what}"));
                    }
                    Ok((k, c.clean()?))
                })
                .filter(|r| !matches!(r, Ok((_, c)) if c.is_empty()))
                .collect::<std::result::Result<BTreeMap<_, _>, String>>()
        };
        Ok(Look {
            format: self.format,
            updated_ms: self.updated_ms,
            themes,
            deck: self.deck.clean()?,
            layouts: level(self.layouts, is_layout_key, "layout")?,
            panes: level(self.panes, is_pane_key, "pane")?,
            fonts: crate::fonts::clean_list(self.fonts)?,
            // A record that is not one is dropped, not a reason to refuse.
            changes: Some(self.changes)
                .filter(crate::sync::changes_ok)
                .unwrap_or_default(),
        })
    }

    /// Stop naming a theme that is gone, and drop what that leaves empty.
    fn forget_theme(&mut self, id: &str) {
        let unname = |c: &mut Choice| {
            if c.theme.as_deref() == Some(id) {
                c.theme = None;
            }
        };
        unname(&mut self.deck);
        for map in [&mut self.layouts, &mut self.panes] {
            map.values_mut().for_each(unname);
            map.retain(|_, c| !c.is_empty());
        }
    }
}

impl Daemon {
    fn look_file(&self) -> PathBuf {
        self.paths.config_dir.join("look.json")
    }

    pub fn look(&self) -> Look {
        std::fs::read(self.look_file())
            .ok()
            .and_then(|b| serde_json::from_slice::<Look>(&b).ok())
            .and_then(|l| l.clean().ok())
            .unwrap_or_default()
    }

    fn look_write(&self, look: &Look) -> Result<()> {
        std::fs::create_dir_all(&self.paths.config_dir)?;
        let tmp = self.look_file().with_extension("json.tmp");
        write_private(
            &tmp,
            &serde_json::to_string_pretty(look).map_err(|e| invalid(e.to_string()))?,
        )?;
        std::fs::rename(&tmp, self.look_file())?;
        Ok(())
    }

    /// The owner's change path, as `deck_change` is for the deck document.
    /// `change` returns what to put in the ship's log, or nothing when the
    /// document is already as asked.
    pub(crate) fn look_change(
        &self,
        by: &str,
        change: impl FnOnce(&mut Look) -> Result<Option<Value>>,
    ) -> Result<Look> {
        let look = {
            let _held = self.look_lock.lock().unwrap();
            let mut look = self.look();
            if look.format > FORMAT {
                return Err(invalid(
                    "the looks saved here are from a newer spandeck than this station runs; update this station to change them",
                ));
            }
            let Some(what) = change(&mut look)? else {
                return Ok(look);
            };
            // A pane that is gone from this station has no look to keep.
            let mine = format!("{}/", self.station);
            let here = self.panes.read().unwrap();
            look.panes.retain(|k, _| match k.strip_prefix(&mine) {
                Some(id) => here.contains_key(id),
                None => true,
            });
            drop(here);
            let mut look = look.clean().map_err(invalid)?;
            // Never back to an older format: a station holding this copy
            // would then refuse the next one.
            look.format = look.format.max(if look.fonts.is_empty() { 1 } else { 2 });
            let base = look.updated_ms;
            look.updated_ms = self.sync_stamp("look", base);
            self.sync_mark(&mut look.changes, base, look.updated_ms);
            self.look_write(&look)?;
            self.record("look.changed", None, None, None, Some(by), what);
            look
        };
        self.emit(
            "look.updated",
            None,
            None,
            json!({"updated_ms": look.updated_ms}),
        );
        // A pane can be closed from a thread that is not the runtime's.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let (d, pushed) = (self.clone(), look.clone());
            rt.spawn(async move { d.look_send(&pushed, 0).await });
        }
        Ok(look)
    }

    /// Offer a copy to every online peer that keeps this document, or (when
    /// `only_if_older_than` is set) to those whose copy is older than that.
    /// A station that does not say it keeps one is from before themes and is
    /// not asked.
    async fn look_send(&self, look: &Look, only_if_older_than: u64) {
        for peer in self.fleet.peers() {
            let Some(theirs) = peer.info.pointer("/look/updated_ms") else {
                continue;
            };
            let theirs = theirs.as_u64().unwrap_or(0);
            // A station that reads only an older format would refuse this copy.
            let reads = peer.info.pointer("/look/format").and_then(|f| f.as_u64());
            if peer.state != "online"
                || reads < Some(look.format as u64)
                || (only_if_older_than > 0 && theirs >= only_if_older_than)
            {
                continue;
            }
            let r = self
                .fleet
                .call(&peer.station, "look.push", json!({"look": look}))
                .await;
            self.sync_sent("look", look.updated_ms, &peer.station, r)
                .await;
        }
    }

    /// Save a theme: a new one (no id) or a change to one already saved.
    pub fn look_theme_save(&self, mut theme: Theme, by: &str) -> Result<Value> {
        let new = theme.id.is_empty();
        if new {
            theme.id = new_id("u");
        }
        let theme = theme.clean().map_err(invalid)?;
        let id = theme.id.clone();
        self.look_change(by, |look| {
            if look
                .themes
                .iter()
                .any(|t| t.id != theme.id && t.name == theme.name)
            {
                return Err(invalid(format!(
                    "there is already a theme called {:?}; choose another name",
                    theme.name
                )));
            }
            match look.themes.iter().position(|t| t.id == theme.id) {
                Some(at) if look.themes[at] == theme => return Ok(None),
                Some(at) => look.themes[at] = theme.clone(),
                None if new && look.themes.len() < MAX_THEMES => look.themes.push(theme.clone()),
                None if new => {
                    return Err(invalid(format!(
                        "there are at most {MAX_THEMES} saved themes; delete one first"
                    )))
                }
                None => return Err(Error::NotFound(format!("theme {}", theme.id))),
            }
            Ok(Some(
                json!({"theme": theme.id, "name": theme.name, "saved": true}),
            ))
        })?;
        Ok(json!({"id": id}))
    }

    pub fn look_theme_remove(&self, id: &str, by: &str) -> Result<Value> {
        self.look_change(by, |look| {
            let before = look.themes.len();
            look.themes.retain(|t| t.id != id);
            if look.themes.len() == before {
                return Err(Error::NotFound(format!("theme {id}")));
            }
            look.forget_theme(id);
            Ok(Some(json!({"theme": id, "removed": true})))
        })?;
        Ok(json!({"removed": id}))
    }

    /// Set what the deck, a layout or a pane asks for. No choice (or an empty
    /// one) at a layout or a pane takes its own look away.
    pub fn look_set(
        &self,
        level: &str,
        key: Option<&str>,
        choice: Option<Choice>,
        by: &str,
    ) -> Result<Value> {
        let choice = choice
            .map(Choice::clean)
            .transpose()
            .map_err(invalid)?
            .unwrap_or_default();
        let ok = match (level, key) {
            ("deck", None) => true,
            ("layout", Some(k)) => is_layout_key(k),
            ("pane", Some(k)) => is_pane_key(k),
            _ => false,
        };
        if !ok {
            return Err(invalid(
                "a look is set for the deck, for a layout (global:<name> or <station>/<project>/<name>) or for a pane (<station>/<pane>)",
            ));
        }
        self.look_change(by, |look| {
            let (map, max) = match level {
                "deck" => {
                    if look.deck == choice {
                        return Ok(None);
                    }
                    look.deck = choice.clone();
                    return Ok(Some(json!({"level": "deck", "theme": choice.theme})));
                }
                "layout" => (&mut look.layouts, MAX_LAYOUTS),
                _ => (&mut look.panes, MAX_PANES),
            };
            let key = key.unwrap_or_default();
            if choice.is_empty() {
                if map.remove(key).is_none() {
                    return Ok(None);
                }
                return Ok(Some(json!({"level": level, "key": key, "cleared": true})));
            }
            if map.get(key) == Some(&choice) {
                return Ok(None);
            }
            if !map.contains_key(key) && map.len() >= max {
                return Err(invalid(format!(
                    "at most {max} {level}s can have a look of their own; clear one first"
                )));
            }
            map.insert(key.to_string(), choice.clone());
            Ok(Some(
                json!({"level": level, "key": key, "theme": choice.theme}),
            ))
        })?;
        Ok(json!({"ok": true}))
    }

    /// A pane was closed here: its own look goes with it, in this station's
    /// copy. Nobody asked for this change, so it is not one the fleet is
    /// told to take: the copy keeps its stamp and is not offered. Stamping
    /// it would make this copy the newest in the fleet whatever it had
    /// missed, and a station that had just come back would wipe out themes
    /// saved while it was away. The other stations drop the entry with the
    /// owner's next change made here (`look_change` prunes), or when the
    /// Control Deck that closed the pane asks, which it does.
    pub(crate) fn look_forget_pane(&self, id: &str, by: &str) {
        let key = format!("{}/{id}", self.station);
        {
            let _held = self.look_lock.lock().unwrap();
            let mut look = self.look();
            if look.format > FORMAT || look.panes.remove(&key).is_none() {
                return;
            }
            if self.look_write(&look).is_err() {
                return;
            }
        }
        self.record(
            "look.changed",
            None,
            None,
            None,
            Some(by),
            json!({"level": "pane", "key": key, "cleared": true, "here_only": true}),
        );
        self.emit("look.updated", None, None, json!({}));
    }

    /// A layout was deleted: its own look goes with it.
    pub(crate) fn look_forget_layout(&self, key: &str, by: &str) {
        let _ = self.look_change(by, |look| {
            Ok(look
                .layouts
                .remove(key)
                .map(|_| json!({"level": "layout", "key": key, "cleared": true})))
        });
    }

    /// A station was removed from the fleet: its panes' looks and its
    /// projects' layouts' looks are forgotten.
    pub(crate) fn look_forget_station(&self, station: &str, by: &str) -> Result<Value> {
        let prefix = format!("{station}/");
        let mut count = 0;
        self.look_change(by, |look| {
            let before = look.layouts.len() + look.panes.len();
            look.layouts.retain(|k, _| !k.starts_with(&prefix));
            look.panes.retain(|k, _| !k.starts_with(&prefix));
            count = before - look.layouts.len() - look.panes.len();
            Ok((count > 0).then(|| json!({"forgot_station": station, "looks": count})))
        })?;
        Ok(json!(count))
    }

    /// Another station offering its copy. Kept only if it is newer, and only
    /// in a format this station can hold without losing any of it.
    pub fn look_push(&self, look: Look, from: &str) -> Result<Value> {
        if look.format > FORMAT {
            return Ok(
                json!({"station": self.station, "ok": false, "kept": "that copy is in a newer format than this station knows"}),
            );
        }
        let look = look.clean().map_err(invalid)?;
        self.sync_saw("look", look.updated_ms);
        {
            let _held = self.look_lock.lock().unwrap();
            let mine = self.look();
            if look.format < mine.format {
                return Ok(
                    json!({"station": self.station, "ok": false, "kept": "this station's copy is in a newer format"}),
                );
            }
            if look.updated_ms <= mine.updated_ms {
                return Ok(
                    json!({"station": self.station, "ok": true, "kept": "this station's copy is as new or newer", "updated_ms": mine.updated_ms}),
                );
            }
            self.sync_replaced("look", &mine.changes, &look.changes);
            self.look_write(&look)?;
        }
        self.record(
            "look.received",
            None,
            None,
            None,
            Some(from),
            json!({"updated_ms": look.updated_ms, "themes": look.themes.len()}),
        );
        self.emit(
            "look.updated",
            None,
            None,
            json!({"updated_ms": look.updated_ms}),
        );
        // It may name fonts this station does not hold: ask the sender.
        if !look.fonts.is_empty() {
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                let (d, from) = (self.clone(), from.to_string());
                rt.spawn(async move { d.look_fonts_sync(Some(&from)).await });
            }
        }
        Ok(json!({"station": self.station, "ok": true, "updated_ms": look.updated_ms}))
    }

    /// Called after discovery: bring peers with an older copy up to date.
    pub(crate) async fn look_sync(&self) {
        let mine = self.look();
        if mine.updated_ms > 0 {
            self.look_send(&mine, mine.updated_ms).await;
        }
        self.look_fonts_sync(None).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(v: Value) -> Settings {
        serde_json::from_value(v).unwrap()
    }

    // The owner's request, section "safety of values": what a theme file or
    // another station sends is data, and only plain values get through.
    #[test]
    fn a_value_that_is_more_than_a_colour_a_number_or_a_font_name_is_refused() {
        let good = settings(json!({
            "term.background": "#0f1318", "term.red": "#f00", "term.selection": "#2b4a6b80",
            "deck.bg": "#ffffff", "deck.scheme": "light", "deck.focusWidth": 1,
            "font.family": ["Fira Code", "ui-monospace", "monospace"], "font.size": 13.5,
            "font.lineHeight": 1.2, "font.letterSpacing": 0, "font.weight": 400, "font.weightBold": 700,
            "font.ligatures": false, "ui.family": ["system-ui"], "ui.size": 13,
            "cursor.style": "bar", "cursor.blink": true, "term.contrast": 4.5,
        }));
        assert_eq!(clean_settings(good.clone()).unwrap(), good);

        for (key, bad) in [
            // Style sheet and markup smuggled in a colour.
            (
                "term.background",
                json!("red;background:url(//evil.example/x)"),
            ),
            ("deck.bg", json!("#fff;}</style><script>alert(1)</script>")),
            ("deck.text", json!("url(javascript:alert(1))")),
            ("deck.blue", json!("var(--bg)")),
            ("term.cursor", json!("rgb(1,2,3)")),
            ("term.cursor", json!("#12345")),
            ("term.cursor", json!("#gggggg")),
            ("term.cursor", json!(7)),
            // And in a font's name.
            ("font.family", json!(["x; } body { display: none"])),
            ("font.family", json!(["a\", url(//evil.example/f.woff)"])),
            ("ui.family", json!(["<img src=x onerror=alert(1)>"])),
            ("font.family", json!("monospace")),
            ("font.family", json!([])),
            ("font.family", json!(["a".repeat(65)])),
            (
                "font.family",
                json!(["a", "b", "c", "d", "e", "f", "g", "h", "i"]),
            ),
            // Numbers out of range, or not numbers.
            ("font.size", json!(400)),
            ("font.size", json!("13px")),
            ("font.lineHeight", json!(0)),
            ("font.weight", json!(450)),
            ("font.weight", json!("bold")),
            ("term.contrast", json!(22)),
            ("ui.size", json!(-1)),
            ("cursor.style", json!("beam")),
            ("cursor.blink", json!("yes")),
            ("deck.scheme", json!("dark light")),
        ] {
            let got = clean_settings(settings(json!({ key: bad.clone() })));
            assert!(got.is_err(), "{key} = {bad} was taken");
        }

        // A setting this version does not know is dropped, not kept.
        let mixed = settings(json!({"font.size": 14, "term.wallpaper": "x", "onload": "alert(1)"}));
        assert_eq!(
            clean_settings(mixed).unwrap(),
            settings(json!({"font.size": 14}))
        );
    }

    #[test]
    fn a_document_is_bounded_and_its_names_and_keys_are_checked() {
        let doc = |v: Value| serde_json::from_value::<Look>(v).unwrap().clean();
        let ok = doc(json!({
            "format": 1, "updated_ms": 5, "later_field": {"kept": false},
            "themes": [{"id": "u-1", "name": " Mine ", "settings": {"font.size": 15}, "extra": 1}],
            "deck": {"theme": "solarized-light"},
            "layouts": {"global:wall": {"theme": "u-1"}, "alpha/web/main": {"set": {"font.size": 12}}},
            "panes": {"alpha/p-01": {"set": {"term.background": "#000"}}, "alpha/p-02": {}},
        }))
        .unwrap();
        assert_eq!(ok.themes[0].name, "Mine");
        // An empty choice is no choice.
        assert_eq!(ok.panes.keys().collect::<Vec<_>>(), ["alpha/p-01"]);
        assert!(!serde_json::to_string(&ok).unwrap().contains("later_field"));

        for bad in [
            json!({"themes": [{"id": "dracula", "name": "x"}]}),
            json!({"themes": [{"id": "u-1", "name": ""}]}),
            json!({"themes": [{"id": "u-1", "name": "a\nb"}]}),
            json!({"themes": [{"id": "u-1", "name": "x".repeat(49)}]}),
            json!({"themes": [{"id": "u-1", "name": "x"}, {"id": "u-2", "name": "x"}]}),
            json!({"themes": [{"id": "u-1", "name": "x", "settings": {"deck.bg": "white"}}]}),
            json!({"deck": {"theme": "Not An Id"}}),
            json!({"layouts": {"../../etc": {"theme": "u-1"}}}),
            json!({"panes": {"alpha": {"theme": "u-1"}}}),
            json!({"panes": {"alpha/p 1": {"theme": "u-1"}}}),
            json!({"themes": (0..65).map(|i| json!({"id": format!("u-{i}"), "name": format!("t{i}")})).collect::<Vec<_>>()}),
        ] {
            assert!(doc(bad.clone()).is_err(), "{bad} was taken");
        }
    }
}
