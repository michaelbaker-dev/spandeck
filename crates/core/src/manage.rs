//! Looking after stations from the Control Deck: restart one, update one to
//! a release, wake one that is asleep, and start one whose daemon is not
//! running. The last two reach a machine spandeck cannot talk to, so they
//! use what the machine offers without spandeck: its network card, and SSH.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::daemon::{invalid, Daemon, Error, Result, VERSION};
use crate::fleet::Peer;

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
            });
            known.push(keep);
        }
        known.sort_by(|a, b| a.station.cmp(&b.station));
        let after = serde_json::to_string(&known).unwrap_or_default();
        if after != before {
            self.db.kv_set("fleet.known", &after);
        }
    }

    /// Forget a station that is gone for good.
    pub fn forget_station(&self, station: &str) -> Result<Value> {
        let mut known = self.known_stations();
        known.retain(|k| k.station != station);
        self.db.kv_set(
            "fleet.known",
            &serde_json::to_string(&known).unwrap_or_default(),
        );
        self.fleet.forget(station);
        self.emit("fleet.updated", None, None, json!({}));
        Ok(json!({"forgotten": station}))
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
    pub async fn station_update(&self, version: Option<&str>, by: &str) -> Result<Value> {
        let target = release_target()
            .ok_or_else(|| invalid("there are no releases for this kind of machine"))?;
        let base = self
            .cfg
            .update
            .url
            .clone()
            .unwrap_or_else(|| RELEASES.to_string());
        let base = base.trim_end_matches('/');
        let http = crate::http::builder()
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| invalid(e.to_string()))?;
        let fetch = |url: String| {
            let http = http.clone();
            async move {
                let r = http
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| invalid(format!("could not reach {url}: {e}")))?;
                if !r.status().is_success() {
                    return Err(invalid(format!("{url}: HTTP {}", r.status().as_u16())));
                }
                Ok(r)
            }
        };
        let tag = match version {
            Some(v) => v.to_string(),
            None => {
                // The "latest" page redirects to the newest release's tag.
                let r = fetch(format!("{base}/latest")).await?;
                r.url().path().rsplit('/').next().unwrap_or("").to_string()
            }
        };
        if !tag.starts_with('v')
            || !tag
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        {
            return Err(invalid(format!("{tag:?} is not a release name")));
        }
        if tag.trim_start_matches('v') == VERSION && version.is_none() {
            return Ok(json!({"ok": true, "current": true, "version": VERSION,
                "note": "This station already runs the latest release."}));
        }
        let name = format!("spandeck-{tag}-{target}");
        // A Windows release is a .zip holding spandeck.exe.
        let windows = spandeck_terminal::sys::WINDOWS;
        let (ext, program) = if windows {
            ("zip", "spandeck.exe")
        } else {
            ("tar.gz", "spandeck")
        };
        let archive = fetch(format!("{base}/download/{tag}/{name}.{ext}"))
            .await?
            .bytes()
            .await
            .map_err(|e| invalid(format!("the download did not finish: {e}")))?;
        let published = fetch(format!("{base}/download/{tag}/{name}.{ext}.sha256"))
            .await?
            .text()
            .await
            .map_err(|e| invalid(e.to_string()))?;
        let want = published.split_whitespace().next().unwrap_or("");
        let got: String = Sha256::digest(&archive)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if want.len() != 64 || !crate::rpc::constant_eq(&got, &want.to_lowercase()) {
            return Err(invalid(
                "the download does not match its published checksum; nothing was changed",
            ));
        }
        let dir: PathBuf = self.paths.state_dir.join("releases").join(&tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!("release.{ext}"));
        std::fs::write(&file, &archive)?;
        // The tar that Windows ships (bsdtar) reads .zip as well; it works
        // the kind out by itself, so it is not told gzip there.
        let unpack = tokio::process::Command::new("tar")
            .arg(if windows { "-xf" } else { "-xzf" })
            .arg(&file)
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
        let _ = std::fs::remove_file(&file);
        // After it proves healthy, the new binary also replaces the one this
        // station starts from, so a reboot does not bring the old one back.
        let install_to: Option<&Path> = self.cfg.update.in_place.then_some(self.exe.as_path());
        let mut out = self.upgrade(&binary, by, install_to)?;
        out["release"] = json!(tag);
        Ok(out)
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
