//! A station is renamed (DECISIONS, "A station is renamed, and no two
//! stations have one name").
//!
//! A station has no identity but its name, and the name is in a great many
//! places: pane addresses, the roster, layouts, looks, messages, stops,
//! links between projects. So a rename is two things. What is stored under
//! the old name is moved to the new one, on every station, once. And the
//! old name goes on finding the station for a year (names.rs), for whatever
//! was not moved, was written by a station that had not heard, or is in
//! somebody's memory.
//!
//! How a station takes its new name: its config file's `station =` line is
//! written and the daemon is restarted in place, the live handoff an
//! upgrade uses, so every pane, agent and terminal stays open. Not changed
//! while running: the name is read at start into every part of the daemon
//! (the relay, the stop book, the approval book, the web server's hosts,
//! every pane's record), and a daemon half of which had the new name would
//! be worse than a second's reconnect. The image that starts sees that the
//! name is not the one it last ran under, and does the rest: that is also
//! what makes a rename done by hand in the config file a rename like any
//! other.
//!
//! How the others hear: the renamed station tells every member that
//! answers at once (`fleet.renamed`, a station's signed call only), and
//! every station says the renames it knows in its roster, so a member that
//! was off hears from whoever it sees first. A station that does not know
//! its own former name (renamed by hand on a version from before this) is
//! found by its address: see `renamed_at`.

use serde_json::{json, Value};

use crate::daemon::{invalid, Authority, Caller, Daemon, Error, Result};
use crate::fleet::Peer;
use crate::names::{self, Names, Renamed};
use crate::paths::now_ms;
use crate::project::LayoutNode;

/// The renames this station knows, as it keeps them.
const KV: &str = "fleet.renamed";
/// The name this station last ran under.
const KV_NAME: &str = "station.name";
/// Who asked for the rename the next start will find, and to what.
const KV_ASKED: &str = "station.renaming";
/// Renames of this station whose synced documents are still to be moved
/// and whose news is still to be told.
const KV_OWED: &str = "station.rename_owed";

/// A leaf's station, renamed. Says whether anything changed.
pub(crate) fn node_renamed(node: &mut LayoutNode, now: &dyn Fn(&str) -> String) -> bool {
    match node {
        LayoutNode::Pane { station, .. }
        | LayoutNode::PaneId { station, .. }
        | LayoutNode::View { station, .. } => match station {
            Some(s) => {
                let to = now(s);
                let changed = to != *s;
                *s = to;
                changed
            }
            None => false,
        },
        LayoutNode::Split { a, b, .. } => node_renamed(a, now) | node_renamed(b, now),
    }
}

/// A layout's leaves and hidden panes, renamed.
pub(crate) fn layout_renamed(
    layout: &mut crate::project::Layout,
    now: &dyn Fn(&str) -> String,
) -> bool {
    let mut changed = false;
    if let Some(tree) = &mut layout.tree {
        changed |= node_renamed(tree, now);
    }
    for leaf in &mut layout.hidden {
        changed |= node_renamed(leaf, now);
    }
    changed
}

/// The deck document's pins and global layouts, renamed.
pub(crate) fn deck_renamed(deck: &mut crate::deck::Deck, now: &dyn Fn(&str) -> String) -> bool {
    use crate::deck::Pin;
    let mut changed = false;
    for l in &mut deck.layouts {
        changed |= layout_renamed(l, now);
    }
    for p in &mut deck.pins {
        let station = match p {
            Pin::Pane { station, .. } => Some(station),
            Pin::Layout { station, .. } => station.as_mut(),
        };
        if let Some(s) = station {
            let to = now(s);
            changed |= to != *s;
            *s = to;
        }
    }
    // Two pins that now name one thing are one pin.
    let mut seen: Vec<Pin> = vec![];
    deck.pins.retain(|p| {
        let new = !seen.contains(p);
        if new {
            seen.push(p.clone());
        }
        new
    });
    changed
}

/// `<station>/…` keys of a map, renamed. An entry already under the new
/// name is kept: it is the newer.
fn keys_renamed<V>(
    map: &mut std::collections::BTreeMap<String, V>,
    now: &dyn Fn(&str) -> String,
) -> bool {
    let moved: Vec<(String, String)> = map
        .keys()
        .filter(|k| !k.starts_with("global:"))
        .filter_map(|k| {
            let (station, rest) = k.split_once('/')?;
            let to = now(station);
            (to != station).then(|| (k.clone(), format!("{to}/{rest}")))
        })
        .collect();
    for (old, new) in &moved {
        if let Some(v) = map.remove(old) {
            map.entry(new.clone()).or_insert(v);
        }
    }
    !moved.is_empty()
}

/// The look document's layouts and panes, renamed.
pub(crate) fn look_renamed(look: &mut crate::look::Look, now: &dyn Fn(&str) -> String) -> bool {
    keys_renamed(&mut look.layouts, now) | keys_renamed(&mut look.panes, now)
}

/// The tools document's scopes and held-back versions, renamed.
pub(crate) fn tools_renamed(
    doc: &mut crate::tooldoc::ToolsDoc,
    now: &dyn Fn(&str) -> String,
) -> bool {
    let mut changed = false;
    for tool in doc.tools.values_mut() {
        for s in &mut tool.stations {
            let to = now(s);
            changed |= to != *s;
            *s = to;
        }
        let mut seen: Vec<String> = vec![];
        tool.stations.retain(|s| {
            let new = !seen.contains(s);
            if new {
                seen.push(s.clone());
            }
            new
        });
        let moved: Vec<(String, String)> = tool
            .station_versions
            .keys()
            .filter_map(|k| {
                let to = now(k);
                (to != *k).then(|| (k.clone(), to))
            })
            .collect();
        for (old, new) in moved {
            if let Some(v) = tool.station_versions.remove(&old) {
                tool.station_versions.entry(new).or_insert(v);
            }
            changed = true;
        }
    }
    changed
}

impl Daemon {
    /// Does this station know of renames? `[flags] rename = false` makes it
    /// behave as a station from before, in both directions.
    pub fn rename_on(&self) -> bool {
        self.flag_on(names::CAP, true)
    }

    /// What the station once called `name` is called now.
    pub fn station_now(&self, name: &str) -> String {
        self.fleet.now_called(name)
    }

    /// Is `name` this station: its name, or a name it had?
    pub fn is_here(&self, name: &str) -> bool {
        name == self.station || self.fleet.now_called(name) == self.station
    }

    /// An address with its station named as it is called now.
    pub(crate) fn address_now(&self, addr: &str) -> String {
        match crate::relay::split_address(addr) {
            (name, Some(station)) => {
                let now = self.station_now(station);
                if now == station {
                    addr.to_string()
                } else {
                    format!("{name}@{now}")
                }
            }
            _ => addr.to_string(),
        }
    }

    fn names_save(&self, names: &Names) {
        self.db
            .kv_set(KV, &serde_json::to_string(names.list()).unwrap_or_default());
        self.fleet.names_set(names.clone());
    }

    /// At start, before anything is served: the renames kept, and whether
    /// this station's own name is the one it last ran under. If it is not
    /// (the owner renamed it, from the deck, the command line or by hand
    /// in the config file; or the machine's own name changed and the
    /// config names none) that is a rename, and what is stored here under
    /// the old name is moved now.
    pub(crate) fn rename_started(&self) {
        if !self.rename_on() {
            return;
        }
        let kept: Vec<Renamed> = self
            .db
            .kv_get(KV)
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        let mut names = Names::from_list(kept);
        names.prune(now_ms());
        self.fleet.names_set(names.clone());
        let last = self.db.kv_get(KV_NAME);
        let asked: Value = self
            .db
            .kv_get(KV_ASKED)
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or(Value::Null);
        self.db.kv_set(KV_ASKED, "null");
        match last {
            Some(old) if old != self.station && names::valid(&old) => {
                let by = match asked["to"].as_str() {
                    Some(to) if to == self.station => asked["by"].as_str().unwrap_or("you"),
                    _ => "its config file",
                };
                let pair = Renamed {
                    from: old.clone(),
                    to: self.station.clone(),
                    at_ms: names.stamp(now_ms()),
                };
                self.db.kv_set(KV_NAME, &self.station);
                self.record(
                    "station.renamed",
                    None,
                    None,
                    None,
                    Some(by),
                    json!({"from": old, "to": self.station}),
                );
                self.renames_take(vec![pair], by);
            }
            Some(old) if old == self.station => {}
            _ => self.db.kv_set(KV_NAME, &self.station),
        }
        // What an earlier image did not get to.
        if !self.rename_owed().is_empty() {
            self.rename_settle_soon();
        }
    }

    fn rename_owed(&self) -> Vec<Value> {
        self.db
            .kv_get(KV_OWED)
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    /// A value another station signs with its name, to show it holds this
    /// fleet's key under that name (`station.info` with `nonce`).
    pub(crate) fn name_proof(&self, nonce: &str) -> Option<String> {
        let plain = !nonce.is_empty()
            && nonce.len() <= 128
            && nonce.chars().all(|c| c.is_ascii_alphanumeric());
        (plain && self.rename_on()).then(|| {
            self.trust
                .sign("name", format!("{nonce}\n{}", self.station).as_bytes())
        })
    }

    /// Did the station that answered `info` sign `nonce` and the name it
    /// gives with this fleet's key?
    pub(crate) fn name_proved(&self, nonce: &str, info: &Value) -> bool {
        match (info["station"].as_str(), info["proof"].as_str()) {
            (Some(station), Some(proof)) => {
                self.trust
                    .verify("name", format!("{nonce}\n{station}").as_bytes(), proof)
            }
            _ => false,
        }
    }

    /// Renames a station of this fleet told: in its roster, or in
    /// `fleet.renamed`. Data from another machine: each name is checked,
    /// the list capped. Says whether anything was news.
    pub(crate) fn renames_told(&self, said: Option<&Value>, how: &str) -> bool {
        let list: Vec<Renamed> = said
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .take(names::MAX)
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        self.renames_take(list, how)
    }

    /// Take renames, and move what is stored here for each that is news.
    fn renames_take(&self, mut list: Vec<Renamed>, how: &str) -> bool {
        if list.is_empty() || !self.rename_on() {
            return false;
        }
        let _one = self.fleet.renaming.lock().unwrap();
        let now = now_ms();
        let mut names = self.fleet.names();
        list.sort_by_key(|r| r.at_ms);
        let mut news = vec![];
        for r in list {
            // Nothing from further ahead than any clock is wrong by, and
            // nothing this station would have forgotten already. And never
            // that this station, which knows its name, is called otherwise.
            if r.at_ms > now + names::KEPT_MS
                || now.saturating_sub(r.at_ms) >= names::KEPT_MS
                || (r.from == self.station && names.now_called(&r.from) == self.station)
            {
                continue;
            }
            if names.add(r.clone()) {
                news.push(r);
            }
        }
        if news.is_empty() {
            return false;
        }
        names.prune(now);
        self.names_save(&names);
        for r in &news {
            // Where the name leads now: a rename since undone moves nothing.
            let to = names.now_called(&r.from);
            if to != r.from {
                self.renamed_apply(&r.from, &to, how);
            }
        }
        self.emit("fleet.updated", None, None, json!({}));
        true
    }

    /// Move what this station stores under `from` to `to`.
    fn renamed_apply(&self, from: &str, to: &str, how: &str) {
        let own = to == self.station;
        // The list of stations, and the roster kept across restarts.
        if own {
            // This station under the name it had: not another station.
            self.fleet.forget_quietly(from);
        } else {
            self.fleet.renamed_entry(from, to);
        }
        let mut known = self.known_stations();
        if let Some(i) = known.iter().position(|k| k.station == from) {
            let mut old = known.remove(i);
            if !own && !known.iter().any(|k| k.station == to) {
                old.station = to.to_string();
                known.push(old);
            }
            known.sort_by(|a, b| a.station.cmp(&b.station));
            self.db.kv_set(
                "fleet.known",
                &serde_json::to_string(&known).unwrap_or_default(),
            );
        }
        // Messages (both ends of each), what waits to be sent, stops,
        // approved routes, links between projects, tasks.
        let (at_from, at_to) = (format!("@{from}"), format!("@{to}"));
        let moved = self.db.with_tx(|tx| {
            let mut n = 0;
            for column in ["sender", "recipient"] {
                n += tx.execute(
                    &format!(
                        "UPDATE messages SET {column} = substr({column}, 1, length({column}) - length(?1)) || ?2
                         WHERE length({column}) > length(?1) AND substr({column}, -length(?1)) = ?1"
                    ),
                    [&at_from, &at_to],
                )?;
            }
            n += tx.execute("UPDATE outbox SET dest=?2 WHERE dest=?1", [from, to])?;
            n += tx.execute("UPDATE stops SET station=?2 WHERE station=?1", [from, to])?;
            n += tx.execute("UPDATE stops SET scope=?2 WHERE scope=?1", [from, to])?;
            n += tx.execute("UPDATE routes SET station=?2 WHERE station=?1", [from, to])?;
            n += tx.execute(
                "UPDATE OR REPLACE project_links SET station=?2 WHERE station=?1",
                [from, to],
            )?;
            n += tx.execute("UPDATE tasks SET station=?2 WHERE station=?1", [from, to])?;
            Ok(n)
        });
        self.stops.book.rename_station(from, to);
        self.trust_renamed(from, to);
        // What each station said of its Tailscale key.
        if let Some(mut reports) = self
            .db
            .kv_get("tailnet.members")
            .and_then(|v| serde_json::from_str::<serde_json::Map<String, Value>>(&v).ok())
        {
            if let Some(r) = reports.remove(from) {
                reports.entry(to.to_string()).or_insert(r);
                self.db
                    .kv_set("tailnet.members", &Value::Object(reports).to_string());
            }
        }
        self.record(
            "fleet.renamed",
            None,
            None,
            None,
            None,
            json!({"from": from, "to": to, "how": how, "moved": moved.unwrap_or(0)}),
        );
        // What waited for the old name is sent to the new one by the tasks
        // that send (a message within half a second, a stop within two).
        if own {
            let mut owed = self.rename_owed();
            owed.retain(|o| o["from"] != json!(from));
            owed.push(json!({"from": from, "to": to, "by": how}));
            self.db.kv_set(KV_OWED, &Value::Array(owed).to_string());
            self.rename_settle_soon();
        }
        self.emit("pane.updated", None, None, json!({}));
    }

    fn rename_settle_soon(&self) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let d = self.clone();
        rt.spawn(async move { d.rename_settle().await });
    }

    /// The renamed station's own part, after it has looked at the fleet
    /// (so that it changes the newest copy of each document): the synced
    /// documents are saved under the new name, every member that answers
    /// is told, and the owner is told, fleet-wide.
    async fn rename_settle(&self) {
        // One at a time.
        let _one = self.fleet.settling.lock().await;
        let owed = self.rename_owed();
        if owed.is_empty() {
            return;
        }
        // As soon as this station answers again (a station that is told
        // looks at it at once, and must find it there): every member it
        // knows of is told, at the address it had, before this station
        // has looked at anyone.
        for _ in 0..60 {
            if !self.web_addrs.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        self.renames_tell(&self.fleet_members()).await;
        self.discover_peers().await;
        let by = owed
            .last()
            .and_then(|o| o["by"].as_str())
            .unwrap_or("you")
            .to_string();
        // Read as they are written, the documents already name the
        // station as it is called now (`deck()`, `look()`, `tools_doc()`).
        // Saved once as a change made here, every station takes them so,
        // one from before renames included.
        if self.deck_unrenamed() != self.deck() {
            let _ = self.deck_change(&by, |_| Ok(Some(json!({"renamed": owed}))));
        }
        if self.look_unrenamed() != self.look() {
            let _ = self.look_change(&by, |_| Ok(Some(json!({"renamed": owed}))));
        }
        let _ = self.tools_edit(&by, "a station was renamed", |_| Ok(()));
        // Every member that answers is told now; the rest read it in the
        // roster of whichever station they see first.
        let members: Vec<Peer> = self
            .fleet_members()
            .into_iter()
            .filter(|p| p.state != "unreachable")
            .collect();
        self.renames_tell(&members).await;
        self.discover_peers().await;
        for o in &owed {
            let (from, to) = (
                o["from"].as_str().unwrap_or_default(),
                o["to"].as_str().unwrap_or_default(),
            );
            if to != self.station {
                continue;
            }
            let behind = self.rename_behind();
            let mut body = format!(
                "This station was renamed ({}). Its panes, agents and terminals kept running. What named {from} now names {to} on every station that knows of renames, and {from} still finds it for a year.",
                match o["by"].as_str() {
                    Some("its config file") => "in its config file".to_string(),
                    Some("the roster") | Some("its address") =>
                        "the other stations found it under its new name".to_string(),
                    Some(by) => format!("by {by}"),
                    None => "by you".to_string(),
                }
            );
            if let Some(words) = behind_words(from, &behind) {
                body.push(' ');
                body.push_str(&words);
            }
            self.notify(
                "station.renamed",
                None,
                None,
                &format!("{from} is now {to}"),
                &body,
            );
        }
        self.db.kv_set(KV_OWED, "[]");
        self.emit("fleet.updated", None, None, json!({}));
    }

    /// Tell these stations the renames this station knows, each in a call
    /// of its own and all at the same time. One from before renames
    /// answers that it knows no such call, which costs nothing.
    async fn renames_tell(&self, members: &[Peer]) {
        let said = json!({"renamed": self.fleet.names().list()});
        let calls = members
            .iter()
            .map(|p| self.fleet.call_addr(&p.addr, "fleet.renamed", said.clone()));
        crate::fleet::futures_join_all(calls).await;
    }

    /// In a station's round: one that knows of renames and does not list
    /// one this station knows (it was off when it happened) is told now,
    /// by whichever station sees it first.
    pub(crate) async fn renames_offer(&self, peer: &Peer) {
        let knows = peer.info["caps"]
            .as_array()
            .is_some_and(|c| c.iter().any(|c| c == names::CAP));
        if !knows || !self.rename_on() {
            return;
        }
        let theirs = Names::from_list(
            peer.info
                .pointer("/roster/renamed")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default(),
        );
        let mine = self.fleet.names();
        let news = mine.list().iter().any(|r| {
            !theirs
                .list()
                .iter()
                .any(|t| t.from == r.from && t.at_ms >= r.at_ms)
        });
        if news {
            self.renames_tell(std::slice::from_ref(peer)).await;
        }
    }

    /// The members that will go on showing the old name: those that do not
    /// say they know of renames, as (name, why).
    fn rename_behind(&self) -> Vec<(String, &'static str)> {
        self.fleet_members()
            .into_iter()
            .filter_map(|p| {
                let knows = p.info["caps"]
                    .as_array()
                    .is_some_and(|c| c.iter().any(|c| c == names::CAP))
                    || p.info["rename"] == json!(true);
                match (knows, p.state.as_str()) {
                    (true, _) => None,
                    (false, "unreachable") => Some((p.station, "off")),
                    (false, _) => Some((p.station, "old")),
                }
            })
            .collect()
    }

    /// The owner renames this station. The name is checked, the config
    /// file's `station =` line written, and the daemon restarted in place
    /// with every terminal kept; the image that starts does the rest
    /// (`rename_started`).
    pub fn station_rename(&self, new: &str, by: &str) -> Result<Value> {
        if !self.rename_on() {
            return Err(invalid(
                "renaming is switched off on this station (`rename = false` under [flags])",
            ));
        }
        let new = new.trim();
        let mut others: Vec<String> = self.fleet.peers().into_iter().map(|p| p.station).collect();
        others.extend(self.known_stations().into_iter().map(|k| k.station));
        if let Some(no) = names::refusal(new, &self.station, &others, &self.fleet.names()) {
            return Err(invalid(no));
        }
        let file = self.paths.config_file();
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(invalid(format!(
                    "this station's config file could not be read ({e}); nothing was changed"
                )))
            }
        };
        let with = names::config_with_name(&text, new).map_err(|e| {
            invalid(format!(
                "the name could not be written into {} ({e}); nothing was changed",
                file.display()
            ))
        })?;
        let tmp = file.with_extension("toml.rename");
        std::fs::create_dir_all(&self.paths.config_dir)?;
        std::fs::write(&tmp, &with)
            .and_then(|_| {
                // The file keeps who may read it.
                if let Ok(meta) = std::fs::metadata(&file) {
                    let _ = std::fs::set_permissions(&tmp, meta.permissions());
                }
                std::fs::rename(&tmp, &file)
            })
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                invalid(format!(
                    "{} could not be written ({e}); nothing was changed",
                    file.display()
                ))
            })?;
        self.db
            .kv_set(KV_ASKED, &json!({"to": new, "by": by}).to_string());
        let behind = self.rename_behind();
        let restart = match self.station_restart(by) {
            Ok(v) => v,
            Err(e) => {
                // Not restarted: the file says what this station is called.
                let _ = std::fs::write(&file, &text);
                self.db.kv_set(KV_ASKED, "null");
                return Err(invalid(format!(
                    "this station could not be restarted to take the name ({e}); nothing was changed"
                )));
            }
        };
        let mut says = format!(
            "{} is now {new}. It is restarting in place to take the name: every pane, agent and terminal stays open.",
            self.station
        );
        if let Some(words) = behind_words(&self.station, &behind) {
            says.push(' ');
            says.push_str(&words);
        }
        Ok(json!({
            "station": self.station, "name": new, "renamed": true, "restart": restart,
            "behind": behind.iter().map(|(s, why)| json!({"station": s, "why": why})).collect::<Vec<_>>(),
            "says": says,
        }))
    }

    /// `fleet.renamed`: a station of this fleet says which stations were
    /// renamed (itself, as a rule). A station's signed call only.
    pub(crate) fn fleet_renamed(&self, caller: &Caller, params: &Value) -> Result<Value> {
        if caller.authority != Authority::Peer {
            return Err(Error::Denied(
                "fleet.renamed is for another station of this fleet; use station.rename".into(),
            ));
        }
        let news = self.renames_told(params.get("renamed"), "the roster");
        if news {
            // It is looked at now, under its new name.
            let d = self.clone();
            tokio::spawn(async move { d.discover_peers().await });
        }
        Ok(json!({"station": self.station, "ok": true, "news": news}))
    }

    /// The station that has just answered at `peer`'s address, and proved
    /// that it holds this fleet's key under its name, is listed here under
    /// another name as well: the same station, renamed where nobody was
    /// told (by hand in its config file, on a version from before
    /// renames, so that it does not know its former name itself). The old
    /// entry is replaced and its name becomes a former name.
    ///
    /// All three must hold: the same address (an entry's address is where
    /// that station last answered, so the old name answers nowhere else);
    /// the answer was to a signed call and carried the proof, which only a
    /// holder of the key can make for a value asked just then; and the
    /// entry is a member's, not a machine that never joined. Never on what
    /// an unsigned answer says.
    pub(crate) fn renamed_at(&self, peer: &Peer) -> bool {
        if !self.rename_on() || peer.info["proved"] != json!(true) {
            return false;
        }
        let names = self.fleet.names();
        let stale: Vec<Renamed> = self
            .fleet
            .others_at(&peer.addr, &peer.station)
            .into_iter()
            .filter(|old| old.last_seen_ms < peer.last_seen_ms)
            .map(|old| Renamed {
                from: old.station,
                to: peer.station.clone(),
                at_ms: names.stamp(now_ms()),
            })
            .collect();
        self.renames_take(stale, "its address")
    }

    /// Pairs of stations of this fleet whose names differ only by
    /// capitals: the deck offers to rename one of each.
    pub fn name_twins(&self) -> Vec<Value> {
        let mut all = vec![self.station.clone()];
        all.extend(self.fleet_members().into_iter().map(|p| p.station));
        all.sort();
        all.dedup();
        let mut out = vec![];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                if names::alike(a, b) {
                    out.push(json!([a, b]));
                }
            }
        }
        out
    }

    /// `fleet.remove` of a name that is only a former name of a station
    /// that is in this fleet: the name is taken off the list, and nothing
    /// else is done. `None` when it is not that.
    pub(crate) fn remove_former(&self, station: &str, live: Option<&str>) -> Option<Value> {
        let now = match live {
            Some(live) => live.to_string(),
            None => self.fleet.names().former(station)?,
        };
        if now == station {
            return None;
        }
        self.fleet.forget_quietly(station);
        let mut known = self.known_stations();
        known.retain(|k| k.station != station);
        self.db.kv_set(
            "fleet.known",
            &serde_json::to_string(&known).unwrap_or_default(),
        );
        self.record(
            "fleet.name_forgotten",
            None,
            None,
            None,
            None,
            json!({"name": station, "station": now}),
        );
        self.emit("fleet.updated", None, None, json!({}));
        Some(json!({
            "station": station, "removed": false, "former_name_of": now,
            "says": format!("{station} is a former name of {now}, which is in this fleet and was not touched: nothing was closed, no key was replaced. The old name was taken off the list."),
        }))
    }
}

/// "MacBookM5 will show RTX6000 as an unreachable station until it is
/// updated": the members that do not know of renames, said plainly.
pub(crate) fn behind_words(old: &str, behind: &[(String, &'static str)]) -> Option<String> {
    if behind.is_empty() {
        return None;
    }
    let list = |why: &str| -> Vec<&str> {
        behind
            .iter()
            .filter(|(_, w)| *w == why)
            .map(|(s, _)| s.as_str())
            .collect()
    };
    let (old_ones, off) = (list("old"), list("off"));
    let mut out = vec![];
    if !old_ones.is_empty() {
        out.push(format!(
            "{} run{} a spandeck from before stations could be renamed: {} will list {old} as well, as a station that does not answer, until {} updated.",
            old_ones.join(", "),
            if old_ones.len() == 1 { "s" } else { "" },
            if old_ones.len() == 1 { "it" } else { "they" },
            if old_ones.len() == 1 { "it is" } else { "they are" },
        ));
    }
    if !off.is_empty() {
        out.push(format!(
            "{} {} off, and did not say when last seen that {} of renames: if {} still on a spandeck from before this, {} will list {old} as well, as a station that does not answer, until updated; otherwise {} of the rename when {} back.",
            off.join(", "),
            if off.len() == 1 { "is" } else { "are" },
            if off.len() == 1 { "it knows" } else { "they know" },
            if off.len() == 1 { "it is" } else { "they are" },
            if off.len() == 1 { "it" } else { "they" },
            if off.len() == 1 { "it hears" } else { "they hear" },
            if off.len() == 1 { "it is" } else { "they are" },
        ));
    }
    Some(out.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The incident's fleet: MacBookM5 on 0.4.18 and off.
    #[test]
    fn a_rename_names_the_stations_that_will_go_on_showing_the_old_name() {
        assert_eq!(behind_words("RTX6000", &[]), None);
        let words = behind_words(
            "RTX6000",
            &[
                ("MacBookM5".to_string(), "off"),
                ("spark1".to_string(), "old"),
            ],
        )
        .unwrap();
        assert!(words.contains("spark1 runs a spandeck from before stations could be renamed"));
        assert!(words.contains("MacBookM5 is off"));
        assert!(words.contains("will list RTX6000 as well"));
    }

    // What names the station by its old name must not break: a layout's
    // leaves and hidden panes, pins, looks.
    #[test]
    fn a_layout_a_pin_and_a_look_follow_a_station_to_its_new_name() {
        let now = |s: &str| match s {
            "RTX6000" => "rtx6000-wsl".to_string(),
            other => other.to_string(),
        };
        let mut deck: crate::deck::Deck = serde_json::from_value(json!({
            "updated_ms": 5,
            "layouts": [{"name": "all", "tree": {"dir": "row", "ratio": 500,
                "a": {"id": "p-1", "station": "RTX6000"},
                "b": {"pane": "dev", "project": "web", "station": "rtx6000"}},
                "hidden": [{"id": "p-1", "station": "RTX6000"}]}],
            "pins": [{"kind": "pane", "station": "RTX6000", "id": "p-1"},
                {"kind": "pane", "station": "rtx6000-wsl", "id": "p-1"},
                {"kind": "layout", "station": "RTX6000", "project": "web", "name": "main"}],
        }))
        .unwrap();
        assert!(deck_renamed(&mut deck, &now));
        let v = serde_json::to_value(&deck).unwrap();
        assert_eq!(v["layouts"][0]["tree"]["a"]["station"], "rtx6000-wsl");
        // The other station, whose name differs only by capitals, is not it.
        assert_eq!(v["layouts"][0]["tree"]["b"]["station"], "rtx6000");
        assert_eq!(v["layouts"][0]["hidden"][0]["station"], "rtx6000-wsl");
        assert_eq!(v["pins"].as_array().unwrap().len(), 2, "{v}");
        assert_eq!(v["pins"][1]["station"], "rtx6000-wsl");
        assert!(!deck_renamed(&mut deck, &now), "moved once");

        let mut look = crate::look::Look::default();
        let choice = crate::look::Choice::default();
        look.panes.insert("RTX6000/p-1".into(), choice.clone());
        look.panes.insert("rtx6000/p-1".into(), choice.clone());
        look.layouts
            .insert("RTX6000/web/main".into(), choice.clone());
        look.layouts.insert("global:all".into(), choice);
        assert!(look_renamed(&mut look, &now));
        let keys: Vec<&String> = look.panes.keys().chain(look.layouts.keys()).collect();
        assert_eq!(
            keys,
            [
                "rtx6000-wsl/p-1",
                "rtx6000/p-1",
                "global:all",
                "rtx6000-wsl/web/main"
            ]
        );
    }

    /// A view follows its station's new name, and keeps the rest (20 Nov 2026).
    #[test]
    fn a_view_follows_its_stations_new_name() {
        let now = |s: &str| {
            if s == "old" {
                "new".to_string()
            } else {
                s.to_string()
            }
        };
        let mut leaf = LayoutNode::View {
            id: "view.files.a1".into(),
            station: Some("old".into()),
            folder: Some("/srv/out".into()),
            watch: None,
            hidden: true,
        };
        assert!(node_renamed(&mut leaf, &now));
        assert_eq!(
            leaf,
            LayoutNode::View {
                id: "view.files.a1".into(),
                station: Some("new".into()),
                folder: Some("/srv/out".into()),
                watch: None,
                hidden: true,
            }
        );
        assert!(!node_renamed(&mut leaf, &now));
    }
}
