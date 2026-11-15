//! Closing every pane on a station, and taking a station out of the fleet.
//!
//! Every station of a fleet holds the same key (`trust.rs`), so a station
//! cannot be shut out by a list: as long as it holds a key the others
//! accept, it can sign calls and make device tokens. Removing a station
//! therefore replaces the key on every station that stays:
//!
//! 1. The removed station, if it answers, closes its panes, makes a key of
//!    its own (a fleet of one) and forgets the others.
//! 2. The station the owner is using makes a new key and hands it to every
//!    other station that answers, in a call signed with the old one.
//! 3. A station that was away keeps the old key until it is seen again. Each
//!    station that changed keeps the old key for that one purpose (never to
//!    accept anything) and hands the new key to the stations it knew, at the
//!    addresses it knew them at, when they come back. After thirty days a
//!    station that is still away rejoins on the owner's approval instead.
//!
//! What follows from the key being new: every browser signed in from another
//! device signs in again, and connections made under the old key are closed.
//!
//! A station that joins another fleet uses the same machinery for the
//! stations of the fleet it was in (`fleet_joined`): it takes the key it was
//! given, keeps the old one for the one purpose in step 3, and hands the new
//! key to each of them when it next looks, which it does at once. So a join
//! moves a whole fleet, and never leaves part of it behind.

use std::time::Duration;

use serde_json::{json, Value};
use spandeck_agents::AgentState;

use crate::daemon::{invalid, Authority, Caller, Daemon, Error, Result};
use crate::deck::Pin;
use crate::fleet::{futures_join_all, Peer};
use crate::paths::{now_ms, random_token};
use crate::project::LayoutNode;
use crate::trust::{fleet_id_of, hex, unhex, Retired, Straggler};

/// How long a station that was told to stop keeps running, so its reply
/// (and the reply of the station that asked) still gets out.
const QUIT_AFTER: Duration = Duration::from_millis(1500);

/// A layout's tree without the panes of one station. A split that loses one
/// side becomes the other side; `None` when nothing is left.
fn without_station(node: LayoutNode, station: &str) -> Option<LayoutNode> {
    match node {
        LayoutNode::Pane {
            station: Some(ref s),
            ..
        }
        | LayoutNode::PaneId {
            station: Some(ref s),
            ..
        } if s == station => None,
        LayoutNode::Split { dir, ratio, a, b } => {
            match (without_station(*a, station), without_station(*b, station)) {
                (Some(a), Some(b)) => Some(LayoutNode::Split {
                    dir,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (one, other) => one.or(other),
            }
        }
        leaf => Some(leaf),
    }
}

impl Daemon {
    /// End every pane on this station and take each off the list, as
    /// `pane.close` does for one. Projects, layouts, history and the archive
    /// are not touched.
    pub fn close_all(&self, by: &str) -> Value {
        let (mut running, mut working, mut agents) = (0, 0, 0);
        let mut closed = vec![];
        for p in self.list_panes() {
            {
                let m = p.meta.lock().unwrap();
                if m.exit.is_none() {
                    running += 1;
                    if m.agent.is_agent() {
                        agents += 1;
                    }
                    if matches!(m.state, AgentState::Working | AgentState::NeedsYou) {
                        working += 1;
                    }
                }
            }
            if self.close(&p.id, by).is_ok() {
                closed.push(p.id.clone());
            }
        }
        self.record(
            "station.closed_all",
            None,
            None,
            None,
            Some(by),
            json!({"panes": closed.len(), "running": running, "agents": agents, "working": working}),
        );
        self.emit("pane.updated", None, None, json!({}));
        json!({"station": self.station, "closed": closed.len(), "running": running, "agents": agents, "working": working})
    }

    /// Every station this one counts as in its fleet, other than itself.
    pub(crate) fn fleet_members(&self) -> Vec<Peer> {
        let mut all: Vec<Peer> = self
            .fleet
            .peers()
            .into_iter()
            .filter(|p| p.state != "not_linked")
            .collect();
        for k in self.known_stations() {
            if self.fleet.peer(&k.station).is_none() {
                let mut k = k;
                k.state = "unreachable".into();
                all.push(k);
            }
        }
        all.retain(|p| p.station != self.station);
        all
    }

    /// Drop everything this station remembers about another one. The deck
    /// document is changed separately, once, by the station the owner used.
    fn forget_removed(&self, station: &str) {
        let mut known = self.known_stations();
        known.retain(|k| k.station != station);
        self.db.kv_set(
            "fleet.known",
            &serde_json::to_string(&known).unwrap_or_default(),
        );
        self.fleet.forget(station);
        let _ = self.db.with(|c| {
            c.execute("DELETE FROM project_links WHERE station=?1", [station])?;
            c.execute(
                "DELETE FROM stops WHERE station=?1 AND status!='confirmed'",
                [station],
            )?;
            c.execute("DELETE FROM routes WHERE station=?1", [station])?;
            c.execute(
                "DELETE FROM outbox WHERE dest=?1 AND state='pending'",
                [station],
            )
        });
        // It is not owed the new key either.
        let _ = self.trust.retired_change(|list| {
            for r in list {
                r.stations.retain(|s| s.station != station);
            }
        });
    }

    /// Take a station's panes out of the deck document: its pinned panes and
    /// layouts, and its panes in the global layouts. A global layout left
    /// with no pane goes too.
    fn deck_forget_station(&self, station: &str, by: &str) -> Result<Value> {
        let mut counts = json!({"pins": 0, "layouts": 0});
        self.deck_change(by, |deck| {
            let pins_before = deck.pins.len();
            deck.pins.retain(|p| match p {
                Pin::Pane { station: s, .. } => s != station,
                Pin::Layout { station: s, .. } => s.as_deref() != Some(station),
            });
            let mut touched = 0;
            let mut emptied = vec![];
            for l in &mut deck.layouts {
                let Some(tree) = l.tree.take() else { continue };
                let pruned = without_station(tree.clone(), station);
                if pruned.as_ref() != Some(&tree) {
                    touched += 1;
                }
                if pruned.is_none() {
                    emptied.push(l.name.clone());
                }
                l.tree = pruned;
            }
            deck.layouts.retain(|l| !emptied.contains(&l.name));
            deck.pins.retain(
                |p| !matches!(p, Pin::Layout { project: None, name, .. } if emptied.contains(name)),
            );
            let pins = pins_before - deck.pins.len();
            if pins == 0 && touched == 0 {
                return Ok(None);
            }
            counts = json!({"pins": pins, "layouts": touched});
            Ok(Some(
                json!({"forgot_station": station, "pins": pins, "layouts": touched}),
            ))
        })?;
        Ok(counts)
    }

    /// This station leaves its fleet: every pane is closed, it takes a key of
    /// its own and forgets the other stations. With `stop`, spandeck is then
    /// stopped here and no longer starts at login; the programs and the data
    /// stay on disk.
    pub fn station_leave(&self, stop: bool, by: &str) -> Result<Value> {
        let closed = self.close_all(by);
        let left = self.trust.fleet_id();
        let key = unhex(&random_token()).expect("a random token is hex");
        self.trust.replace_key(key).map_err(|e| {
            invalid(format!(
                "could not save a new key ({e}); nothing was changed"
            ))
        })?;
        let _ = self.trust.retired_change(Vec::clear);
        for p in self.fleet_members() {
            self.forget_removed(&p.station);
        }
        self.db.kv_set("fleet.known", "[]");
        self.fleet.forget_all();
        self.devices_forget(None);
        let service = if stop {
            self.service_remove()
        } else {
            json!(null)
        };
        self.record(
            "station.left",
            None,
            None,
            None,
            Some(by),
            json!({"left_fleet": left, "closed": closed["closed"], "working": closed["working"], "stop": stop, "service": service}),
        );
        self.notify(
            "station.left",
            None,
            None,
            "This station was removed from its fleet",
            "Its panes were closed. It no longer trusts the other stations, and they no longer trust it. Browsers on other devices must sign in again. To put it back, join a fleet and approve the request there.",
        );
        self.emit("fleet.updated", None, None, json!({}));
        self.emit("trust.updated", None, None, json!({}));
        let vault = self.vault_view();
        if stop {
            let d = self.clone();
            // Only a station that had the login service takes it out of the
            // system: the service's name is one per user, so a second daemon
            // under another home (a test, a trial) must leave it alone.
            let had_service = service["installed"] == json!(true);
            tokio::spawn(async move {
                tokio::time::sleep(QUIT_AFTER).await;
                if had_service {
                    d.service_remove_last();
                }
                d.quit.notify_one();
            });
        } else {
            // Look again: the others are now stations it is not linked with.
            let d = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(600)).await;
                d.discover_peers().await;
            });
        }
        Ok(json!({
            "station": self.station,
            "left": true,
            "closed": closed["closed"],
            "working": closed["working"],
            "stopping": stop,
            "service": service,
            "vault_token": vault["has_token"],
            "dirs": self.info()["dirs"],
        }))
    }

    /// Stop spandeck starting at login here. What stays is said in the reply.
    fn service_remove(&self) -> Value {
        if !crate::doctor::service_installed(&self.home) {
            return json!({"installed": false});
        }
        let removal = crate::doctor::service_removal(&self.home);
        let mut problems = vec![];
        for line in &removal.before {
            if let Err(e) = run_line(line) {
                problems.push(e);
            }
        }
        if let Err(e) = std::fs::remove_file(&removal.file) {
            problems.push(format!("{}: {e}", removal.file.display()));
        }
        for line in &removal.after {
            if let Err(e) = run_line(line) {
                problems.push(e);
            }
        }
        json!({"installed": true, "removed": removal.file.display().to_string(), "problems": problems})
    }

    /// The step that may end this process itself, so it is run last.
    fn service_remove_last(&self) {
        for line in crate::doctor::service_removal(&self.home).last {
            let argv = spandeck_terminal::sys::shell_line(&line);
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..]);
            let _ = spandeck_terminal::sys::spawn_detached(&mut cmd);
        }
    }

    /// Replace the fleet's key on every station that stays. Returns the
    /// stations that took the new key and those still to be told.
    async fn fleet_rotate(&self, removed: &str, by: &str) -> Result<(Vec<String>, Vec<String>)> {
        let old = self.trust.key();
        let new = unhex(&random_token()).expect("a random token is hex");
        let new_id = fleet_id_of(&new);
        let new_id = &new_id;
        let others: Vec<Peer> = self
            .fleet_members()
            .into_iter()
            .filter(|p| p.station != removed)
            .collect();
        let params = json!({"key": hex(&new), "removed": removed, "by": by});
        let calls = others.iter().map(|p| {
            let params = params.clone();
            async move {
                if p.state == "unreachable" {
                    return false;
                }
                if self
                    .fleet
                    .call_addr(&p.addr, "fleet.rekey", params)
                    .await
                    .is_ok()
                {
                    return true;
                }
                // Another station may have handed it the key a moment ago.
                self.fleet
                    .call_open(&p.addr, "station.hello", json!({}))
                    .await
                    .is_ok_and(|h| h["fleet"] == json!(new_id))
            }
        });
        let told = futures_join_all(calls).await;
        let (mut rekeyed, mut waiting, mut stragglers) = (vec![], vec![], vec![]);
        for (p, ok) in others.iter().zip(told) {
            if ok {
                rekeyed.push(p.station.clone());
            } else {
                waiting.push(p.station.clone());
                stragglers.push(Straggler {
                    station: p.station.clone(),
                    addr: p.addr.clone(),
                });
            }
        }
        self.take_key(new, old, (removed, "", ""), stragglers)?;
        Ok((rekeyed, waiting))
    }

    /// Start using `new`, and keep `old` only to hand `new` to `stragglers`.
    /// `why` is (the station removed, the station whose fleet was joined,
    /// who asked): one of the first two is empty.
    fn take_key(
        &self,
        new: Vec<u8>,
        old: Vec<u8>,
        why: (&str, &str, &str),
        stragglers: Vec<Straggler>,
    ) -> Result<()> {
        let retired = Retired {
            key: hex(&old),
            removed: why.0.to_string(),
            joined: why.1.to_string(),
            by: why.2.to_string(),
            since_ms: now_ms(),
            stations: stragglers,
        };
        self.trust
            .retired_change(|list| list.push(retired))
            .and_then(|_| self.trust.replace_key(new))
            .map_err(|e| invalid(format!("could not save the new fleet key: {e}")))
    }

    /// This station was let into `joined`'s fleet and given its key. It
    /// takes the key, and every station of the fleet it was in is owed it:
    /// each is handed it, in a call signed with the old key, the next time
    /// this station looks for stations (`rekey_stragglers`), whether that is
    /// in a moment or in three weeks. Returns those stations' names.
    ///
    /// The key is taken first and the mates told second, so that stopping
    /// halfway leaves a list of stations still to be told, not a fleet that
    /// moved without the station that asked.
    pub(crate) fn fleet_joined(&self, new: Vec<u8>, joined: &str, by: &str) -> Result<Vec<String>> {
        let old = self.trust.key();
        if new == old {
            return Ok(vec![]);
        }
        let mates: Vec<Straggler> = self
            .fleet_members()
            .into_iter()
            .filter(|p| p.station != joined)
            .map(|p| Straggler {
                station: p.station,
                addr: p.addr,
            })
            .collect();
        let names = mates.iter().map(|m| m.station.clone()).collect();
        self.take_key(new, old, ("", joined, by), mates)?;
        // Their tokens were made with the old key.
        self.devices_forget(None);
        Ok(names)
    }

    /// Another station of this fleet hands over the key that replaces the
    /// one both hold: a station was removed (`removed`), or the fleet joined
    /// another one (`joined`, with `removed` empty). Trusted for the same
    /// reason any call from it is: it is signed with the key held here.
    pub(crate) fn fleet_rekeyed(&self, caller: &Caller, params: &Value) -> Result<Value> {
        if caller.authority != Authority::Peer {
            return Err(Error::Denied(
                "fleet.rekey is for another station; use fleet.remove".into(),
            ));
        }
        let get = |k: &str| params.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        let (removed, by) = (get("removed"), get("by"));
        // A name from another station, shown in a notice: cleaned like one.
        let joined: String = get("joined")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
            .take(60)
            .collect();
        let joined = joined.as_str();
        let Some(new) = unhex(get("key")).filter(|k| k.len() == 32) else {
            return Err(invalid("the key is not usable"));
        };
        if removed == self.station {
            return Err(invalid("this station is the one being removed"));
        }
        let old = self.trust.key();
        if new == old {
            // Told twice: it has the key already.
            if !removed.is_empty() {
                self.forget_removed(removed);
            }
            return Ok(json!({"station": self.station, "ok": true}));
        }
        // Stations this one knows that may not have heard: they are handed
        // the key when they are next seen, if they still need it.
        let stragglers = self
            .fleet_members()
            .into_iter()
            .filter(|p| p.station != removed && p.station != caller.who)
            .map(|p| Straggler {
                station: p.station,
                addr: p.addr,
            })
            .collect();
        self.take_key(new, old, (removed, joined, by), stragglers)?;
        // Right for both: the browsers' tokens were made with the old key.
        self.devices_forget(None);
        if !removed.is_empty() {
            self.forget_removed(removed);
        }
        self.record(
            "fleet.rekeyed",
            None,
            None,
            None,
            Some(by),
            json!({"removed": removed, "joined": joined, "from": caller.who, "fleet": self.trust.fleet_id()}),
        );
        if removed.is_empty() {
            self.notify(
                "fleet.merged",
                None,
                None,
                &match joined {
                    "" => "This station's fleet joined another fleet".to_string(),
                    j => format!("This station's fleet joined {j}'s fleet"),
                },
                &format!("{} was let into that fleet and brought this station with it: the stations of both are now one fleet. Browsers signed in to this station from other devices must sign in again.", caller.who),
            );
        } else {
            self.notify(
                "fleet.removed",
                None,
                None,
                &format!("Station {removed} was removed from the fleet"),
                "The fleet has a new key, so the removed station can no longer act here. Browsers signed in from other devices must sign in again.",
            );
        }
        self.emit("fleet.updated", None, None, json!({}));
        self.emit("trust.updated", None, None, json!({}));
        let d = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            d.discover_peers().await;
        });
        Ok(json!({"station": self.station, "ok": true}))
    }

    /// After looking for stations: one that was away when the key was
    /// replaced, is back at the address it had, and still shows the old
    /// fleet, is handed the new key. Returns whether one was.
    pub(crate) async fn rekey_stragglers(&self) -> bool {
        let retired = self.trust.retired();
        if retired.is_empty() {
            return false;
        }
        // Stations that need the old key no longer: (key, station).
        let mut settled: Vec<(String, Straggler)> = vec![];
        let mut handed = false;
        for r in &retired {
            let Some(key) = unhex(&r.key) else { continue };
            let old_fleet = fleet_id_of(&key);
            for s in &r.stations {
                let seen = self
                    .fleet
                    .peer(&s.station)
                    .filter(|p| p.addr == s.addr && p.state != "unreachable");
                match seen {
                    // Already in the fleet again.
                    Some(p) if p.state != "not_linked" => settled.push((r.key.clone(), s.clone())),
                    Some(p) if p.info["fleet"] == json!(old_fleet) => {
                        let sent = self
                            .fleet
                            .call_with_key(
                                &s.addr,
                                "fleet.rekey",
                                json!({"key": hex(&self.trust.key()), "removed": r.removed, "joined": r.joined,
                                    "by": if r.by.is_empty() || r.joined.is_empty() { "the fleet" } else { r.by.as_str() }}),
                                &key,
                            )
                            .await;
                        if sent.is_ok() {
                            handed = true;
                            settled.push((r.key.clone(), s.clone()));
                            self.record(
                                "fleet.rekey_sent",
                                None,
                                None,
                                None,
                                None,
                                json!({"station": s.station, "removed": r.removed, "joined": r.joined}),
                            );
                        }
                    }
                    // Away, or it has joined some other fleet since.
                    _ => {}
                }
            }
        }
        // Also drops what is thirty days old, and keys nobody is owed.
        let _ = self.trust.retired_change(|list| {
            for r in list {
                r.stations
                    .retain(|s| !settled.iter().any(|(key, done)| *key == r.key && done == s));
            }
        });
        handed
    }

    /// The owner removes a station from the fleet. See the top of this file.
    pub async fn fleet_remove(&self, station: &str, stop: bool, caller: &Caller) -> Result<Value> {
        let by = caller.who.as_str();
        if station == self.station {
            return self.remove_self(stop, by).await;
        }
        let peer = self
            .fleet_members()
            .into_iter()
            .find(|p| p.station == station)
            .ok_or_else(|| match self.fleet.peer(station) {
                Some(_) => invalid(format!(
                    "{station} is not in this fleet, so there is nothing to remove"
                )),
                None => Error::NotFound(format!("no station called {station} is known here")),
            })?;
        // 1. The station itself, if it answers: panes closed, its own key.
        let left = if peer.state == "unreachable" {
            Err("it could not be reached".to_string())
        } else {
            self.fleet
                .call_addr(
                    &peer.addr,
                    "rpc.forward",
                    json!({"method": "station.leave", "params": {"stop": stop}}),
                )
                .await
        };
        // 2. A new key for every station that stays.
        let (rekeyed, waiting) = self.fleet_rotate(station, by).await?;
        // 3. Forgotten here and, through the deck document, everywhere.
        self.forget_removed(station);
        let device = self.device_token_again(by);
        self.devices_forget(device.as_ref().map(|(id, _)| id.as_str()));
        let deck = self
            .deck_forget_station(station, by)
            .unwrap_or_else(|e| json!({"problem": e.to_string()}));
        let looks = self
            .look_forget_station(station, by)
            .unwrap_or_else(|e| json!({"problem": e.to_string()}));
        let reached = left.is_ok();
        self.record(
            "fleet.removed",
            None,
            None,
            None,
            Some(by),
            json!({
                "station": station, "reached": reached, "stop": stop && reached,
                "closed": left.as_ref().ok().map(|v| v["closed"].clone()),
                "rekeyed": rekeyed, "waiting": waiting, "fleet": self.trust.fleet_id(),
            }),
        );
        self.emit("fleet.updated", None, None, json!({}));
        self.emit("trust.updated", None, None, json!({}));
        let d = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            d.discover_peers().await;
        });
        let mut out = json!({
            "station": station,
            "removed": true,
            "reached": reached,
            "rekeyed": rekeyed,
            "waiting": waiting,
            "deck": deck,
            "looks": looks,
            "alone": rekeyed.is_empty() && waiting.is_empty(),
        });
        match left {
            Ok(v) => out["there"] = v,
            Err(e) => out["problem"] = json!(e),
        }
        if let Some((_, token)) = device {
            // The browser that asked stays signed in: its new token.
            out["token"] = json!(token);
        }
        Ok(out)
    }

    /// The station the owner is using is the one to remove. Another station
    /// does the work, so the rest of the fleet gets its new key; with none
    /// in reach, this station leaves by itself and says who was not told.
    async fn remove_self(&self, stop: bool, by: &str) -> Result<Value> {
        let before = self.trust.fleet_id();
        // The browser that asks stays signed in to this station.
        let device = self.device_entry(by);
        let signed_in = |mut v: Value| {
            if let Some(o) = v.as_object_mut() {
                // A token for another station's fleet is not this browser's.
                o.remove("token");
            }
            if let (Some(entry), false) = (&device, stop) {
                self.devices_keep(entry.clone());
                if let Some((_, token)) = self.device_token_again(by) {
                    v["token"] = json!(token);
                }
            }
            v
        };
        let members = self.fleet_members();
        for p in members.iter().filter(|p| p.state == "online") {
            let asked = self
                .fleet
                .call_addr(
                    &p.addr,
                    "rpc.forward",
                    json!({"method": "fleet.remove", "params": {"station": self.station, "stop": stop}}),
                )
                .await;
            let gone = self.trust.fleet_id() != before;
            match asked {
                Ok(mut v) => {
                    v["self"] = json!(true);
                    v["through"] = json!(p.station);
                    return Ok(signed_in(v));
                }
                // It was done, and the reply was lost on the way back.
                Err(_) if gone => {
                    return Ok(signed_in(
                        json!({"station": self.station, "removed": true, "self": true, "reached": true, "through": p.station}),
                    ));
                }
                Err(_) => continue,
            }
        }
        if members.is_empty() {
            return Err(invalid(
                "this station is the only one in its fleet, so there is nothing to remove it from",
            ));
        }
        // Nobody to tell. Leaving still ends this station's part: it throws
        // its copy of the key away.
        let not_told: Vec<String> = members.into_iter().map(|p| p.station).collect();
        let there = self.station_leave(stop, by)?;
        Ok(signed_in(json!({
            "station": self.station,
            "removed": true,
            "self": true,
            "reached": true,
            "there": there,
            "not_told": not_told,
            "alone": not_told.is_empty(),
        })))
    }
}

fn run_line(line: &str) -> std::result::Result<(), String> {
    let argv = spandeck_terminal::sys::shell_line(line);
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).stdin(std::process::Stdio::null());
    spandeck_terminal::sys::no_window(&mut cmd);
    match cmd.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "{line}: {}",
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .last()
                .unwrap_or("failed")
                .trim()
        )),
        Err(e) => Err(format!("{line}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Acceptance (remove a station): a global layout loses only the removed
    // station's panes, and a split that loses a side keeps the other.
    #[test]
    fn a_layout_loses_only_the_removed_stations_panes() {
        let leaf = |id: &str, st: Option<&str>| LayoutNode::PaneId {
            id: id.into(),
            station: st.map(String::from),
        };
        let split = |a, b| LayoutNode::Split {
            dir: "row".into(),
            ratio: 500,
            a: Box::new(a),
            b: Box::new(b),
        };
        let tree = split(
            leaf("p-1", Some("gone")),
            split(leaf("p-2", Some("kept")), leaf("p-3", None)),
        );
        assert_eq!(
            without_station(tree, "gone"),
            Some(split(leaf("p-2", Some("kept")), leaf("p-3", None)))
        );
        assert_eq!(
            without_station(
                split(leaf("p-1", Some("gone")), leaf("p-2", Some("gone"))),
                "gone"
            ),
            None
        );
    }
}
