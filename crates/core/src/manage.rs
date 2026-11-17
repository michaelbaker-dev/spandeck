//! Looking after stations from the Control Deck: restart one, update one to
//! a release, wake one that is asleep, and start one whose daemon is not
//! running. The last two reach a machine spandeck cannot talk to, so they
//! use what the machine offers without spandeck: its network card, and SSH.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use std::collections::BTreeMap;

use crate::daemon::{invalid, Daemon, Error, Result, VERSION};
use crate::fleet::Peer;
use crate::paths::now_ms;
use crate::project::valid_name;

/// Where releases are published. `[update] url` in the config replaces it.
pub const RELEASES: &str = "https://github.com/michaelbaker-dev/spandeck/releases";

/// What a stopped station is asked to run. The login service if there is
/// one, else the daemon itself, detached.
const START_SCRIPT: &str = "systemctl --user start spandeckd 2>/dev/null || launchctl kickstart gui/$(id -u)/dev.spandeck.spandeckd 2>/dev/null || (nohup \"$HOME/.local/bin/spandeck\" daemon >/dev/null 2>&1 &)";

/// The name of the release file for this machine.
pub fn release_target() -> Option<String> {
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        _ => return None,
    };
    match (std::env::consts::ARCH, os) {
        // The only Windows release so far.
        ("aarch64", "pc-windows-msvc") => None,
        (a @ ("aarch64" | "x86_64"), _) => Some(format!("{a}-{os}")),
        _ => None,
    }
}

/// A wake-on-LAN packet: six 0xFF bytes, then the address sixteen times.
pub fn magic_packet(mac: &str) -> Option<Vec<u8>> {
    let bytes: Vec<u8> = mac
        .split([':', '-'])
        .map(|p| u8::from_str_radix(p, 16).ok())
        .collect::<Option<_>>()?;
    if bytes.len() != 6 || bytes.iter().all(|b| *b == 0) {
        return None;
    }
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&bytes);
    }
    Some(packet)
}

/// The hardware addresses of this machine's real network cards.
pub fn mac_addresses() -> Vec<String> {
    static MACS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    MACS.get_or_init(|| {
        let mut out: Vec<String> = vec![];
        if cfg!(target_os = "macos") {
            if let Ok(o) = std::process::Command::new("networksetup")
                .arg("-listallhardwareports")
                .output()
            {
                for line in String::from_utf8_lossy(&o.stdout).lines() {
                    if let Some(mac) = line.strip_prefix("Ethernet Address: ") {
                        out.push(mac.trim().to_lowercase());
                    }
                }
            }
        } else if let Ok(dir) = std::fs::read_dir("/sys/class/net") {
            for e in dir.flatten() {
                // A real card has a device behind it; bridges and tunnels do not.
                if !e.path().join("device").exists() {
                    continue;
                }
                if let Ok(mac) = std::fs::read_to_string(e.path().join("address")) {
                    out.push(mac.trim().to_lowercase());
                }
            }
        }
        out.retain(|m| magic_packet(m).is_some());
        out.sort();
        out.dedup();
        out
    })
    .clone()
}

fn host_of(addr: &str) -> &str {
    addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr)
}

impl Daemon {
    /// Stations this one has been in a fleet with, kept across restarts so a
    /// station that is off is still listed, and can be woken or started.
    pub(crate) fn known_stations(&self) -> Vec<Peer> {
        self.db
            .kv_get("fleet.known")
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    pub(crate) fn remember_stations(&self, online: &[Peer]) {
        let mut known = self.known_stations();
        let before = serde_json::to_string(&known).unwrap_or_default();
        for p in online {
            known.retain(|k| k.station != p.station);
            let mut keep = p.clone();
            // Enough to find, wake and describe it; not its whole live state.
            keep.info = json!({
                "macs": p.info.get("macs"),
                "os": p.info.get("os"),
                "service": p.info.get("service"),
                // Where its deck is served over HTTPS: whose deck an
                // origin is must be known while that station is off.
                "https": crate::https::brief(&p.info),
            });
            known.push(keep);
        }
        known.sort_by(|a, b| a.station.cmp(&b.station));
        let after = serde_json::to_string(&known).unwrap_or_default();
        if after != before {
            self.db.kv_put("fleet.known", &after);
        }
    }

    /// Forget a station that is gone for good, on every station of the
    /// fleet: this one says so in its roster, the others read it at their
    /// next look (ten seconds at most) and drop it too. A station that was
    /// off hears of it when it is back. If the station itself comes back it
    /// is listed again, everywhere.
    pub fn forget_station(&self, station: &str) -> Result<Value> {
        self.roster_forget(&[station.to_string()]);
        self.fleet.forget(station);
        self.fleet.take_dropped();
        self.emit("fleet.updated", None, None, json!({}));
        Ok(json!({"forgotten": station}))
    }

    /// When the owner said to forget a station, by station. One that is
    /// seen after that time is a member again.
    fn forgotten_stations(&self) -> BTreeMap<String, u64> {
        self.db
            .kv_get("fleet.forgotten")
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    /// Take stations off the roster, as of now. "Now" is later than the
    /// last time anyone this station has heard from saw them, whatever the
    /// clocks say, so a copy of the roster that still has one does not
    /// bring it back.
    fn roster_forget(&self, stations: &[String]) {
        if stations.is_empty() {
            return;
        }
        let mut known = self.known_stations();
        let mut forgotten = self.forgotten_stations();
        for station in stations {
            let seen = known.iter().find(|k| &k.station == station);
            let at = now_ms().max(seen.map_or(0, |k| k.last_seen_ms + 1));
            forgotten.insert(station.clone(), at);
        }
        known.retain(|k| !stations.contains(&k.station));
        self.db.kv_set(
            "fleet.known",
            &serde_json::to_string(&known).unwrap_or_default(),
        );
        self.db.kv_set(
            "fleet.forgotten",
            &serde_json::to_string(&forgotten).unwrap_or_default(),
        );
    }

    /// What this station tells the others about the fleet (`station.info`).
    pub(crate) fn roster(&self) -> Value {
        let known: Vec<Value> = self
            .known_stations()
            .iter()
            .map(|k| json!({"station": k.station, "addr": k.addr, "version": k.version, "protocol": k.protocol, "last_seen_ms": k.last_seen_ms, "info": k.info}))
            .collect();
        json!({"known": known, "forgotten": self.forgotten_stations()})
    }

    /// After a look at the fleet: make this station's roster the union of
    /// its own and what each station of the fleet that answered says. A
    /// member another station saw more recently is taken from it; a member
    /// the owner said to forget, at any station, and nobody has seen since,
    /// goes. What a station says is data: names and sizes are checked, only
    /// the three things kept about a station are kept, and an address is
    /// taken only if it is one this station would look at by itself
    /// (`places`), so no station can have another send a call anywhere
    /// else. Says whether the list on screen changed.
    pub(crate) fn roster_merge(&self, online: &[Peer], places: &[String]) -> bool {
        // Dropped here since the last look: by a removal from the fleet.
        self.roster_forget(&self.fleet.take_dropped());
        let mut known = self.known_stations();
        let mut forgotten = self.forgotten_stations();
        let before = (
            serde_json::to_string(&known).unwrap_or_default(),
            serde_json::to_string(&forgotten).unwrap_or_default(),
        );
        for peer in online {
            let Some(roster) = peer.info.get("roster") else {
                continue;
            };
            let said = roster.get("forgotten").and_then(|f| f.as_object());
            for (station, at) in said.into_iter().flatten().take(256) {
                if let (true, Some(at)) = (valid_name(station), at.as_u64()) {
                    let mine = forgotten.entry(station.clone()).or_insert(0);
                    *mine = (*mine).max(at);
                }
            }
            let said = roster.get("known").and_then(|k| k.as_array());
            for k in said.into_iter().flatten().take(64) {
                let text = |key: &str| k.get(key).and_then(|v| v.as_str()).unwrap_or_default();
                let (station, addr) = (text("station"), text("addr"));
                let seen = k.get("last_seen_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let mine = known.iter().position(|m| m.station == station);
                let placed =
                    places.iter().any(|p| p == addr) || mine.is_some_and(|i| known[i].addr == addr);
                if !valid_name(station) || station == self.station || !placed {
                    continue;
                }
                if mine.is_some_and(|i| known[i].last_seen_ms >= seen) {
                    continue;
                }
                let learned = Peer {
                    station: station.to_string(),
                    addr: addr.to_string(),
                    version: text("version").chars().take(32).collect(),
                    protocol: k.get("protocol").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    state: "unreachable".into(),
                    last_seen_ms: seen,
                    info: json!({
                        "macs": k.pointer("/info/macs").filter(|m| m.to_string().len() <= 1024),
                        "os": k.pointer("/info/os").and_then(|v| v.as_str()).filter(|o| o.len() <= 32),
                        "service": k.pointer("/info/service").and_then(|v| v.as_bool()),
                        "https": k.get("info").map(crate::https::brief),
                    }),
                };
                match mine {
                    Some(i) => known[i] = learned,
                    None => known.push(learned),
                }
            }
        }
        // A station forgotten three months ago is not remembered as such.
        let now = now_ms();
        forgotten.retain(|_, at| now.saturating_sub(*at) < 90 * 24 * 3_600_000);
        let gone = |station: &str, seen: u64| forgotten.get(station).is_some_and(|at| *at >= seen);
        known.retain(|k| !gone(&k.station, k.last_seen_ms));
        known.sort_by(|a, b| a.station.cmp(&b.station));
        let after = (
            serde_json::to_string(&known).unwrap_or_default(),
            serde_json::to_string(&forgotten).unwrap_or_default(),
        );
        if after != before {
            self.db.kv_put("fleet.known", &after.0);
            self.db.kv_put("fleet.forgotten", &after.1);
        }
        self.fleet
            .follow_roster(&known, |p| gone(&p.station, p.last_seen_ms))
    }

    fn known(&self, station: &str) -> Result<Peer> {
        self.fleet
            .peer(station)
            .or_else(|| {
                self.known_stations()
                    .into_iter()
                    .find(|k| k.station == station)
            })
            .ok_or_else(|| Error::NotFound(format!("no station called {station} is known here")))
    }

    /// Restart this station's daemon in place. Every terminal is kept, and a
    /// daemon that does not come back healthy is undone, as for an upgrade.
    pub fn station_restart(&self, by: &str) -> Result<Value> {
        let exe = self.exe.clone();
        let mut out = self.upgrade(&exe, by, None)?;
        out["restart"] = json!(true);
        Ok(out)
    }

    /// Wake a sleeping station on the same network as this one.
    pub fn fleet_wake(&self, station: &str, by: &str) -> Result<Value> {
        let peer = self.known(station)?;
        let macs: Vec<String> = peer
            .info
            .get("macs")
            .and_then(|m| m.as_array())
            .into_iter()
            .flatten()
            .filter_map(|m| m.as_str().map(String::from))
            .collect();
        let packets: Vec<Vec<u8>> = macs.iter().filter_map(|m| magic_packet(m)).collect();
        if packets.is_empty() {
            return Err(invalid(format!(
                "{station}'s network address is not known yet; it is learned while the station is online"
            )));
        }
        let target = self
            .cfg
            .wake_broadcast
            .clone()
            .unwrap_or_else(|| "255.255.255.255".into());
        let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
        socket.set_broadcast(true)?;
        let mut sent = 0;
        for p in &packets {
            for port in [9u16, 7] {
                if socket.send_to(p, (target.as_str(), port)).is_ok() {
                    sent += 1;
                }
            }
        }
        self.record(
            "fleet.wake",
            None,
            None,
            None,
            Some(by),
            json!({"station": station, "packets": sent}),
        );
        if sent == 0 {
            return Err(invalid(
                "the wake packets could not be sent from this station",
            ));
        }
        Ok(json!({"station": station, "sent": sent,
            "note": "Wake packets only reach a machine on the same network as this station, and only if it is set to wake on them. Give it a minute."}))
    }

    /// Start a station whose daemon is not running, over SSH. Off unless
    /// `remote_start = true` here: it lets this station run a command there.
    pub async fn fleet_start(&self, station: &str, by: &str) -> Result<Value> {
        if !self.cfg.remote_start {
            return Err(Error::Denied(
                "starting another station is off. Set remote_start = true in this station's config.toml, and turn on Tailscale SSH on the other machine.".into(),
            ));
        }
        let peer = self.known(station)?;
        let host = host_of(&peer.addr).to_string();
        if host.is_empty() || host.starts_with('-') {
            return Err(invalid("that station has no usable address"));
        }
        let template: Vec<String> = self.cfg.remote_start_cmd.clone().unwrap_or_else(|| {
            ["tailscale", "ssh", "{host}", "sh", "-c", "{script}"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        });
        let argv: Vec<String> = template
            .iter()
            .map(|a| a.replace("{host}", &host).replace("{script}", START_SCRIPT))
            .collect();
        let Some((program, args)) = argv.split_first() else {
            return Err(invalid("remote_start_cmd is empty"));
        };
        self.record(
            "fleet.start",
            None,
            None,
            None,
            Some(by),
            json!({"station": station, "host": host}),
        );
        let run = tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(Duration::from_secs(25), run)
            .await
            .map_err(|_| invalid(format!("{station} did not answer over SSH in time")))?
            .map_err(|e| invalid(format!("could not run {program}: {e}")))?;
        if !out.status.success() {
            let said = String::from_utf8_lossy(&out.stderr);
            return Err(invalid(format!(
                "starting {station} failed: {}",
                said.lines().last().unwrap_or("no message").trim()
            )));
        }
        // Give it a moment to listen, then look.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        self.discover_peers().await;
        let state = self
            .fleet
            .peer(station)
            .map(|p| p.state)
            .unwrap_or_else(|| "unreachable".into());
        Ok(json!({"station": station, "state": state}))
    }

    /// Update this station to a published release: download it, check it
    /// against its published SHA-256, and upgrade in place with every
    /// terminal kept. An upgrade that fails its health check is undone.
    ///
    /// When the release cannot be had from the release host here (it does
    /// not answer, or the download breaks off), the stations of the fleet
    /// are asked for it (the owner's decision of 17 Nov 2026,
    /// supply_fleet.rs). That goes on after this call has answered: the
    /// answer says so, and a notice says how it went.
    pub async fn station_update(&self, version: Option<&str>, by: &str) -> Result<Value> {
        let target = release_target()
            .ok_or_else(|| invalid("there are no releases for this kind of machine"))?;
        // The release host and the check against the published SHA-256 are
        // the ones a release sent to another machine goes through (supply.rs).
        use crate::supply::{Fetch, ReleaseHost};
        if let Some(tag) = version.filter(|t| !crate::supply::valid_tag(t)) {
            return Err(invalid(format!("{tag:?} is not a release name")));
        }
        let url = self.cfg.update.url.as_deref();
        // Asked first with three seconds to answer, so that a station that
        // cannot reach it says so to whoever asked (another station waits
        // five seconds for this call) instead of going quiet.
        let probe = ReleaseHost::new(url, Duration::from_secs(3)).map_err(invalid)?;
        let host = ReleaseHost::new(url, Duration::from_secs(300)).map_err(invalid)?;
        let direct = match probe.latest().await {
            Ok(latest) => {
                let tag = version.map(String::from).unwrap_or(latest);
                if tag.trim_start_matches('v') == VERSION && version.is_none() {
                    return Ok(json!({"ok": true, "current": true, "version": VERSION,
                        "note": "This station already runs the latest release."}));
                }
                match host.verified(&tag, &target).await {
                    Ok((archive, _sum)) => Ok((tag, archive)),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        match direct {
            Ok((tag, archive)) => {
                let dir = self.release_dir(&tag)?;
                let file = dir.join(format!("release.{}", crate::supply::archive_ext(&target)));
                std::fs::write(&file, &archive)?;
                self.install_release(&tag, &target, &file, by).await
            }
            Err(Fetch::Checksum) => Err(invalid(
                "the download does not match its published checksum; nothing was changed",
            )),
            // The release host answers and has no such file: no station
            // of the fleet would find it there either.
            Err(Fetch::Http(url, 404)) => Err(invalid(format!("{url}: HTTP 404"))),
            Err(e) => {
                let why = match e {
                    Fetch::Unreachable(why) => why,
                    Fetch::Http(url, code) => format!("{url}: HTTP {code}"),
                    Fetch::Checksum => String::new(),
                };
                tracing::info!(%why, "the release could not be had from the release host; asking the fleet");
                self.update_through_fleet(version.map(String::from), target, probe.words(), by)
            }
        }
    }

    /// Where a release is unpacked: emptied first.
    fn release_dir(&self, tag: &str) -> Result<PathBuf> {
        let dir: PathBuf = self.paths.state_dir.join("releases").join(tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Unpack a release archive that has been checked and upgrade to it in
    /// place. The archive must hold `spandeck-<tag>-<target>/`: one that
    /// was made for another version or another kind of machine is refused
    /// here, wherever it came from.
    async fn install_release(
        &self,
        tag: &str,
        target: &str,
        archive: &Path,
        by: &str,
    ) -> Result<Value> {
        let name = format!("spandeck-{tag}-{target}");
        // A Windows release is a .zip holding spandeck.exe.
        let windows = spandeck_terminal::sys::WINDOWS;
        let program = if windows { "spandeck.exe" } else { "spandeck" };
        let dir = match archive.parent() {
            Some(dir) if dir.starts_with(self.paths.state_dir.join("releases")) => {
                dir.to_path_buf()
            }
            _ => self.release_dir(tag)?,
        };
        // The tar that Windows ships (bsdtar) reads .zip as well; it works
        // the kind out by itself, so it is not told gzip there.
        let unpack = tokio::process::Command::new("tar")
            .arg(if windows { "-xf" } else { "-xzf" })
            .arg(archive)
            .arg("-C")
            .arg(&dir)
            .output()
            .await
            .map_err(|e| invalid(format!("could not run tar: {e}")))?;
        let binary = dir.join(&name).join(program);
        if !unpack.status.success() || !binary.is_file() {
            return Err(invalid(
                "the release could not be unpacked; nothing was changed",
            ));
        }
        if archive.starts_with(&dir) {
            let _ = std::fs::remove_file(archive);
        }
        // After it proves healthy, the new binary also replaces the one this
        // station starts from, so a reboot does not bring the old one back.
        let install_to: Option<&Path> = self.cfg.update.in_place.then_some(self.exe.as_path());
        let mut out = self.upgrade(&binary, by, install_to)?;
        out["release"] = json!(tag);
        Ok(out)
    }

    /// The update goes on through the fleet, after this has answered.
    /// `host`: the release host in the owner's words ("GitHub").
    fn update_through_fleet(
        &self,
        version: Option<String>,
        target: String,
        host: String,
        by: &str,
    ) -> Result<Value> {
        use std::sync::atomic::Ordering;
        let station = self.station.clone();
        // Said before anything is begun: there is nobody to ask.
        if let Err(none) = self.supplier_names() {
            return Err(invalid(format!(
                "{station} cannot reach {host} to download the release, and {none}. Update it when it can reach {host}, or copy a release archive to it and run `spandeck upgrade <the spandeck program in it>`."
            )));
        }
        if self.held.updating.swap(true, Ordering::SeqCst) {
            return Err(invalid(format!(
                "{station} is already getting a release from the fleet: its notice says how far it is"
            )));
        }
        let begun = format!(
            "{station} cannot reach {host}: asking the stations of your fleet for the release…"
        );
        let notice = self.notify_quiet("station.update", &begun, "");
        self.record(
            "station.update",
            None,
            None,
            None,
            Some(by),
            json!({"outcome": "asking the fleet", "version": version}),
        );
        let (me, by) = (self.clone(), by.to_string());
        tokio::spawn(async move {
            let prefix = format!("{} cannot reach {host}: ", me.station);
            let said = |words: String| {
                let line = format!("{prefix}{words}");
                me.notify_rewrite(&notice, "station.update", &line, "");
                me.emit("station.update.progress", None, None, json!({"text": line}));
            };
            let got = me
                .release_from_fleet(version.as_deref(), &target, true, version.is_some(), &said)
                .await;
            let ended = match got {
                Ok(supplied) => {
                    let from = supplied.via.clone().unwrap_or_default();
                    let own = supplied.source == crate::supply::Source::FleetOwnCopy;
                    me.db.kv_set(
                        "release.supplied",
                        &json!({"by": from, "tag": supplied.tag, "at_ms": now_ms(), "own": own})
                            .to_string(),
                    );
                    me.record(
                        "station.update",
                        None,
                        None,
                        None,
                        Some(&by),
                        json!({"outcome": "got the release from the fleet", "from": from,
                            "release": supplied.tag, "platform": supplied.target,
                            "sha256": supplied.sha256, "bytes": supplied.size,
                            "what": if own { "that station's own program" } else { "the published release, checked by that station against its published SHA-256" }}),
                    );
                    let what = if own {
                        format!("{from}'s own copy, {}", supplied.tag)
                    } else {
                        supplied.tag.clone()
                    };
                    match me
                        .install_release(&supplied.tag, &target, &supplied.path, &by)
                        .await
                    {
                        Ok(_) => Ok(format!(
                            "{prefix}got {what} from {from} ({}) and is updating to it. Terminals stay open, and a version that does not start properly is undone.",
                            crate::supply::size_words(supplied.size)
                        )),
                        Err(e) => Err(format!(
                            "{prefix}got {what} from {from}, and it was not installed: {e}"
                        )),
                    }
                }
                Err(crate::supply_fleet::NoFleet::Current { tag }) => Ok(format!(
                    "{prefix}the newest release the stations of your fleet can get is {tag}, which {} already runs.",
                    me.station
                )),
                Err(none) => Err(format!(
                    "{prefix}it was not updated, because {}.",
                    none.words()
                )),
            };
            me.held.updating.store(false, Ordering::SeqCst);
            let (line, outcome) = match &ended {
                Ok(line) => (line, "done"),
                Err(line) => (line, "failed"),
            };
            me.notify_rewrite(&notice, "station.update", line, "");
            me.emit(
                "station.update.progress",
                None,
                None,
                json!({"text": line, "ended": outcome}),
            );
            me.record(
                "station.update",
                None,
                None,
                None,
                Some(&by),
                json!({"outcome": outcome, "text": line}),
            );
        });
        Ok(json!({"ok": true, "fleet": true, "from": VERSION, "note": begun}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A bug that would only show on a sleeping machine: the packet's shape.
    #[test]
    fn a_wake_packet_is_six_ff_bytes_then_the_address_sixteen_times() {
        let p = magic_packet("AA:bb:0c:0D:0e:0F").unwrap();
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xFF; 6]);
        for i in 0..16 {
            assert_eq!(
                &p[6 + i * 6..12 + i * 6],
                &[0xAA, 0xBB, 0x0C, 0x0D, 0x0E, 0x0F]
            );
        }
        for bad in [
            "",
            "aa:bb",
            "00:00:00:00:00:00",
            "zz:bb:0c:0d:0e:0f",
            "aa:bb:0c:0d:0e:0f:10",
        ] {
            assert!(magic_packet(bad).is_none(), "{bad}");
        }
    }
}
