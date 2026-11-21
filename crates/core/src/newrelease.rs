//! Is there a newer spandeck than the one the fleet runs?
//!
//! The owner, 19 Nov 2026: "how do I know if there is a spandeck update?"
//! The deck said so only when one station already ran a newer version than
//! another; nothing said a release had been published while every station
//! was on the same older one. (DECISIONS.md, "Is there a newer spandeck?")
//!
//! - Each station asks its release host for the newest release's name,
//!   about once a day, and when the owner asks (`update.check`: the deck's
//!   "Check for a newer spandeck now", `spandeck update --check`). The
//!   release host is the one `station.update` takes releases from: the
//!   one in the code, or `[update] url` in this station's config.
//! - The request says nothing of the owner, the station or a project: one
//!   plain GET of `<releases>/latest`, whose redirect names the release.
//!   The redirect is read, not followed: nothing else is asked of anyone.
//! - A good answer is kept a day. A check that fails is tried again after
//!   an hour, and is "could not check" with its reason, never "latest":
//!   what an earlier good answer said is not said again as if it were new.
//!   A release an earlier answer named is still named: it was published.
//! - `[update] check = false` in the config, or the switch `update_check`
//!   set to false (`[flags]`, `station.flags`), turns it off: nothing is
//!   asked, and the deck says checks are off.
//! - A station that cannot reach its release host has no answer of its
//!   own. Every station says what it learned in `station.info`, and the
//!   fleet's view is the newest release any of them learned: one station
//!   that reaches the host is enough, and no station is special.
//! - What comes back is data. A release's name is a version number or it
//!   is thrown away; another station's word is read the same way. The
//!   link to the release's page is made here, from this station's own
//!   release address and the checked version, never from the answer.
//! - Nothing is downloaded or installed by asking.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::{Daemon, VERSION};
use crate::manage::RELEASES;
use crate::paths::now_ms;
use crate::supply::{date_words, older};

/// The switch (`[flags] update_check = false`, `station.flags`).
pub const FLAG: &str = "update_check";
/// A good answer is not asked for again for a day.
pub(crate) const KEEP_MS: u64 = 24 * 3600 * 1000;
/// A check that failed is tried again after an hour.
pub(crate) const RETRY_MS: u64 = 3600 * 1000;
/// "latest" is said only of an answer of the last three days: a station
/// that was asleep for a week does not say what it knew before.
pub(crate) const SAID_MS: u64 = 3 * KEEP_MS;
const CONNECT_LIMIT: Duration = Duration::from_secs(3);
const ANSWER_LIMIT: Duration = Duration::from_secs(8);
const KEY: &str = "update.newest";

/// What this station's release host last answered.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Seen {
    /// When it was last asked.
    pub checked_ms: u64,
    /// When it last answered with a release.
    pub ok_ms: u64,
    /// The newest release it has named: "0.4.19".
    pub version: Option<String>,
    /// Why the last check gave nothing, in one line.
    pub why: Option<String>,
    /// Who was asked: a host name.
    pub asked: String,
}

#[derive(Default)]
pub(crate) struct State {
    seen: Mutex<Option<Seen>>,
    loaded: AtomicBool,
    asking: AtomicBool,
}

/// A release's version, from its name or from another station's word:
/// two to four numbers with dots, and at most a short pre-release part
/// ("0.4.19", "v0.5.0-beta.1"). Anything else is not a version.
pub fn plain_version(said: &str) -> Option<String> {
    let v = said.strip_prefix('v').unwrap_or(said);
    let (numbers, pre) = match v.split_once('-') {
        Some((n, p)) => (n, Some(p)),
        None => (v, None),
    };
    let parts: Vec<&str> = numbers.split('.').collect();
    let number = |p: &&str| (1..=5).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit());
    let pre_ok = pre.is_none_or(|p| {
        (1..=20).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
    });
    ((2..=4).contains(&parts.len()) && parts.iter().all(number) && pre_ok).then(|| v.to_string())
}

/// The page of a release at a release address: `<releases>/tag/v0.4.19`.
/// Only for an http(s) address and a checked version.
pub fn release_page(base: &str, version: &str) -> Option<String> {
    let version = plain_version(version)?;
    let base = base.trim_end_matches('/');
    let url = reqwest::Url::parse(base).ok()?;
    (matches!(url.scheme(), "https" | "http") && url.host_str().is_some())
        .then(|| format!("{base}/tag/v{version}"))
}

/// One GET of `<releases>/latest`. The answer is a redirect to the newest
/// release's page, whose last part is its name; it is read and not
/// followed. What went wrong is said in a line that names the host.
pub(crate) async fn newest_published(base: &str) -> Result<String, String> {
    let base = base.trim_end_matches('/');
    let host = crate::latest::host_of(base);
    let http = crate::http::builder()
        .connect_timeout(CONNECT_LIMIT)
        .timeout(ANSWER_LIMIT)
        .user_agent("spandeck")
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let res = http
        .get(format!("{base}/latest"))
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() && !e.is_connect() {
                format!("{host} did not answer within {} s", ANSWER_LIMIT.as_secs())
            } else if e.is_connect() || e.is_timeout() {
                format!("no route to {host}")
            } else {
                format!("{host}: {}", e.without_url())
            }
        })?;
    if !res.status().is_redirection() {
        return Err(if res.status().is_success() {
            format!("{host} did not name a release")
        } else {
            format!("{host} answered {}", res.status().as_u16())
        });
    }
    let to = res
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let path = to.split(['?', '#']).next().unwrap_or("");
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    name.strip_prefix('v')
        .and_then(plain_version)
        .ok_or_else(|| format!("{host} did not answer with a release's name"))
}

/// Another station's word about itself (`update` in its `station.info`),
/// kept only where it is what this station would say: a known state, a
/// version that is one, a reason cut to a line.
fn checked(said: &Value) -> Option<Value> {
    let state = said.get("state")?.as_str()?;
    if !["off", "not_checked", "available", "latest", "unknown"].contains(&state) {
        return None;
    }
    let version = said
        .get("version")
        .and_then(|v| v.as_str())
        .and_then(plain_version);
    let why: Option<String> = said
        .get("why")
        .and_then(|v| v.as_str())
        .map(|w| w.chars().filter(|c| !c.is_control()).take(200).collect());
    let ms = |k: &str| said.get(k).and_then(|v| v.as_u64());
    Some(json!({
        "state": state, "version": version, "why": why,
        "checked_ms": ms("checked_ms"), "ok_ms": ms("ok_ms"),
        "checking": said.get("checking").and_then(|v| v.as_bool()).unwrap_or(false),
    }))
}

/// The fleet's view, from what each station says (`stations`: the rows of
/// `fleet.view`, this station's own among them). Each row's `update` is
/// replaced by its checked form with `says`, the words for that station:
/// "0.4.19 available", "latest", "could not check: …", "checks are off".
/// The answer is the fleet's: the newest release any station learned, the
/// stations that are behind it, and the link to its page at `base`.
pub fn fleet_update(stations: &mut [Value], base: &str) -> Value {
    let members = |s: &&mut Value| s["state"] != json!("not_linked");
    // The newest release any station of the fleet has learned of, and
    // whether any of them has an answer that is good now.
    let (mut best, mut by, mut confirmed): (Option<String>, Value, bool) =
        (None, Value::Null, false);
    for s in stations.iter_mut().filter(members) {
        let update = s.get("update").and_then(checked);
        if let Some(u) = &update {
            let state = u["state"].as_str().unwrap_or("");
            if let Some(v) = u["version"].as_str().filter(|_| state != "off") {
                confirmed |= u["why"].is_null() && matches!(state, "latest" | "available");
                if best.as_deref().is_none_or(|b| older(b, v)) {
                    best = Some(v.to_string());
                    by = s["station"].clone();
                }
            }
        }
        s["update"] = update.unwrap_or(Value::Null);
    }
    // A release a station runs is published, whatever the last check said:
    // straight after an update no station has asked the host again, and
    // the fleet would offer the release before the one it runs.
    if let Some(known) = best.clone() {
        for s in stations.iter().filter(|s| s["state"] != json!("not_linked")) {
            if let Some(runs) = s["version"].as_str().and_then(plain_version) {
                if older(&known, &runs) && best.as_deref().is_none_or(|b| older(b, &runs)) {
                    best = Some(runs);
                }
            }
        }
    }
    let (mut behind, mut states, mut why) = (vec![], vec![], Value::Null);
    for s in stations.iter_mut().filter(members) {
        let runs = s["version"].as_str().and_then(plain_version);
        let state = s["update"]["state"].as_str().unwrap_or("").to_string();
        let says = match (&best, &runs) {
            (Some(best), Some(runs)) if older(runs, best) => {
                behind.push(s["station"].clone());
                format!("{best} available")
            }
            _ if state == "off" => "checks are off".to_string(),
            (Some(_), Some(_)) if confirmed => "latest".to_string(),
            _ if state == "unknown" => format!(
                "could not check: {}",
                s["update"]["why"].as_str().unwrap_or("no reason was given")
            ),
            _ if state == "not_checked" => "not checked yet".to_string(),
            _ => String::new(),
        };
        if state == "unknown" && (why.is_null() || s["self"] == json!(true)) {
            why = s["update"]["why"].clone();
        }
        if !state.is_empty() {
            states.push(state);
        }
        if !says.is_empty() {
            s["update"]["says"] = json!(says);
        }
    }
    let all = |want: &str| !states.is_empty() && states.iter().all(|s| s == want);
    let state = if !behind.is_empty() {
        "available"
    } else if best.is_some() && confirmed {
        "latest"
    } else if all("off") {
        "off"
    } else if states.iter().any(|s| s == "unknown") {
        "unknown"
    } else {
        "not_checked"
    };
    let says = match state {
        "available" => format!(
            "spandeck {} is available",
            best.as_deref().unwrap_or_default()
        ),
        "latest" => "latest".to_string(),
        "off" => "checks are off".to_string(),
        "unknown" => format!(
            "could not check: {}",
            why.as_str().unwrap_or("no reason was given")
        ),
        _ => "not checked yet".to_string(),
    };
    let url = match (state, &best) {
        ("available", Some(v)) => release_page(base, v),
        _ => None,
    };
    json!({
        "state": state, "says": says, "url": url, "by": by, "behind": behind,
        "version": best.filter(|_| matches!(state, "available" | "latest")),
        "why": if state == "unknown" { why } else { Value::Null },
    })
}

impl Daemon {
    /// On unless the config (`[update] check = false`) or the switch
    /// `update_check` says no.
    pub(crate) fn update_check_on(&self) -> bool {
        self.cfg.update.check.unwrap_or(true) && self.flag_on(FLAG, true)
    }

    fn update_base(&self) -> &str {
        self.cfg.update.url.as_deref().unwrap_or(RELEASES)
    }

    fn update_seen(&self) -> Option<Seen> {
        let mut seen = self.newrelease.seen.lock().unwrap();
        if !self.newrelease.loaded.swap(true, Ordering::SeqCst) {
            *seen = self
                .db
                .kv_get(KEY)
                .and_then(|v| serde_json::from_str(&v).ok());
        }
        seen.clone()
    }

    /// `update.check`: ask this station's release host for the newest
    /// release, if what is known is due again (`again`: whatever is
    /// known). Answers at once; `station.info` carries what is learned.
    pub fn update_check(&self, again: bool) -> Value {
        if !self.update_check_on() {
            return json!({"station": self.station, "off": true, "asking": false});
        }
        let now = now_ms();
        let due = again
            || self.update_seen().is_none_or(|s| {
                let keep = if s.why.is_none() { KEEP_MS } else { RETRY_MS };
                now.saturating_sub(s.checked_ms) >= keep
            });
        // One question at a time.
        if !due || self.newrelease.asking.swap(true, Ordering::SeqCst) {
            let asking = self.newrelease.asking.load(Ordering::SeqCst);
            return json!({"station": self.station, "off": false, "asking": asking});
        }
        let d = self.clone();
        tokio::spawn(async move {
            let base = d.update_base().to_string();
            let answer = newest_published(&base).await;
            let before = d.update_seen().unwrap_or_default();
            let now = now_ms();
            let seen = match answer {
                Ok(version) => Seen {
                    checked_ms: now,
                    ok_ms: now,
                    version: Some(version),
                    why: None,
                    asked: crate::latest::host_of(&base),
                },
                // What an earlier answer named stays named; that it was
                // the newest then is not said of now.
                Err(why) => Seen {
                    checked_ms: now,
                    why: Some(why.chars().take(200).collect()),
                    asked: crate::latest::host_of(&base),
                    ..before
                },
            };
            if let Ok(text) = serde_json::to_string(&seen) {
                d.db.kv_set(KEY, &text);
            }
            *d.newrelease.seen.lock().unwrap() = Some(seen);
            d.newrelease.asking.store(false, Ordering::SeqCst);
            d.emit("fleet.updated", None, None, json!({}));
        });
        json!({"station": self.station, "off": false, "asking": true})
    }

    /// `update` in `station.info`: what this station knows of the newest
    /// release. `off`; `not_checked`; `available` (a newer one than this
    /// station runs was named); `latest` (the host answered within three
    /// days and named nothing newer); `unknown`, with why.
    pub(crate) fn update_brief(&self) -> Value {
        if !self.update_check_on() {
            return json!({"state": "off"});
        }
        let checking = self.newrelease.asking.load(Ordering::SeqCst);
        let Some(seen) = self.update_seen() else {
            return json!({"state": "not_checked", "checking": checking});
        };
        let newer = seen.version.as_deref().is_some_and(|v| older(VERSION, v));
        let (state, why) = if newer {
            ("available", seen.why.clone())
        } else if seen.why.is_some() {
            ("unknown", seen.why.clone())
        } else if now_ms().saturating_sub(seen.ok_ms) > SAID_MS {
            (
                "unknown",
                Some(format!(
                    "{} last answered on {}",
                    seen.asked,
                    date_words(seen.ok_ms)
                )),
            )
        } else {
            ("latest", None)
        };
        json!({
            "state": state, "version": seen.version, "why": why,
            "checked_ms": seen.checked_ms, "ok_ms": seen.ok_ms, "asked": seen.asked,
            "checking": checking,
        })
    }

    /// The fleet's view for `fleet.view` (see `fleet_update`).
    pub(crate) fn fleet_update(&self, stations: &mut [Value]) -> Value {
        fleet_update(stations, self.update_base())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's request of 19 Nov 2026: what a release host or another
    /// station says is data. A name with markup, a path or anything that is
    /// not a version is thrown away, and a link is only ever this
    /// station's own release address with a checked version behind it.
    #[test]
    fn only_a_version_is_a_version_and_the_link_is_made_here() {
        for good in ["0.4.19", "v0.4.19", "1.0", "0.5.0-beta.1", "10.20.30.40"] {
            assert!(plain_version(good).is_some(), "{good}");
        }
        assert_eq!(plain_version("v0.4.19").as_deref(), Some("0.4.19"));
        for bad in [
            "",
            "v",
            "latest",
            "0",
            "0.4.19<script>",
            "<b>0.4.19</b>",
            "0.4.19 ",
            "0.4.19/../x",
            "0.4.19?x=1",
            "0.4.x",
            "1.2.3.4.5",
            "123456.1",
            "0.4.19-",
            "0.4.19-a_b",
            "0.4.19-aaaaaaaaaaaaaaaaaaaaaaaaa",
            "javascript:alert(1)",
            "https://evil.example/0.4.19",
        ] {
            assert!(plain_version(bad).is_none(), "{bad:?}");
            assert!(release_page(RELEASES, bad).is_none(), "{bad:?}");
        }
        assert_eq!(
            release_page(RELEASES, "0.4.19").as_deref(),
            Some("https://github.com/michaelbaker-dev/spandeck/releases/tag/v0.4.19")
        );
        assert!(release_page("javascript:alert(1)//", "0.4.19").is_none());
        assert!(release_page("file:///etc", "0.4.19").is_none());
    }

    fn row(station: &str, version: &str, update: Value) -> Value {
        json!({"station": station, "version": version, "state": "online", "update": update})
    }

    /// A bug that happened (21 Nov 2026): the fleet was updated to 0.4.20
    /// minutes after it was published, no station had asked the host
    /// since, and the deck offered "0.4.19" to a fleet that ran 0.4.20,
    /// because one station that was off still ran 0.4.18.
    #[test]
    fn a_release_a_station_runs_is_never_older_news_than_the_last_check() {
        let knew = json!({"state": "latest", "version": "0.4.19"});
        let mut rows = vec![
            row("alpha", "0.4.20", knew.clone()),
            row("beta", "0.4.20", knew.clone()),
            row("gamma", "0.4.18", Value::Null),
        ];
        let fleet = fleet_update(&mut rows, "https://example.test/releases");
        assert_eq!(fleet["version"], json!("0.4.20"), "{fleet}");
        assert_eq!(fleet["behind"], json!(["gamma"]), "{fleet}");
        assert_eq!(fleet["says"], json!("spandeck 0.4.20 is available"));
    }

    /// The same request: the fleet's view is the newest release any
    /// station learned, so one station that reaches the host is enough;
    /// "could not check" is never "latest"; and a hostile station's word
    /// (markup for a version, an address of its own) is not passed on.
    #[test]
    fn the_fleets_view_is_the_newest_any_station_learned() {
        let cannot = json!({"state": "unknown", "why": "no route to github.com"});
        // One station learned of 0.4.19; the other cannot reach the host.
        let mut rows = vec![
            row("a", "0.4.17", cannot.clone()),
            row(
                "b",
                "0.4.17",
                json!({"state": "available", "version": "0.4.19"}),
            ),
            row("old", "0.4.16", Value::Null),
        ];
        let fleet = fleet_update(&mut rows, RELEASES);
        assert_eq!(fleet["state"], "available");
        assert_eq!(fleet["version"], "0.4.19");
        assert_eq!(fleet["by"], "b");
        assert_eq!(fleet["behind"], json!(["a", "b", "old"]));
        assert!(fleet["url"]
            .as_str()
            .unwrap()
            .ends_with("/releases/tag/v0.4.19"));
        for r in &rows {
            assert_eq!(r["update"]["says"], "0.4.19 available", "{r}");
        }
        // Nobody could check: not "latest", whatever was known before.
        let before =
            json!({"state": "unknown", "version": "0.4.17", "why": "github.com answered 503"});
        let mut rows = vec![
            row("a", "0.4.17", cannot.clone()),
            row("b", "0.4.17", before),
        ];
        let fleet = fleet_update(&mut rows, RELEASES);
        assert_eq!(fleet["state"], "unknown");
        assert!(fleet["version"].is_null() && fleet["url"].is_null());
        assert_eq!(
            rows[0]["update"]["says"],
            "could not check: no route to github.com"
        );
        assert_eq!(
            rows[1]["update"]["says"],
            "could not check: github.com answered 503"
        );
        // One station has a good answer and nothing is newer: both latest.
        let mut rows = vec![
            row("a", "0.4.17", cannot),
            row(
                "b",
                "0.4.17",
                json!({"state": "latest", "version": "0.4.17"}),
            ),
            row("c", "0.4.17", json!({"state": "off"})),
        ];
        let fleet = fleet_update(&mut rows, RELEASES);
        assert_eq!(fleet["state"], "latest");
        assert_eq!(rows[0]["update"]["says"], "latest");
        assert_eq!(rows[2]["update"]["says"], "checks are off");
        // Every station has checks off.
        let mut rows = vec![row("a", "0.4.17", json!({"state": "off"}))];
        assert_eq!(fleet_update(&mut rows, RELEASES)["state"], "off");
        // A station that says markup for a version, and an address.
        let mut rows = vec![
            row(
                "a",
                "0.4.17",
                json!({"state": "latest", "version": "0.4.17"}),
            ),
            row(
                "b",
                "0.4.17",
                json!({"state": "available", "version": "9.9.9<img src=x onerror=alert(1)>",
                "url": "https://evil.example/", "says": "<b>run this</b>", "why": "x\u{1b}[31m"}),
            ),
        ];
        let fleet = fleet_update(&mut rows, RELEASES);
        assert_eq!(fleet["state"], "latest");
        assert!(fleet["url"].is_null());
        assert!(rows[1]["update"]["version"].is_null());
        assert!(rows[1]["update"].get("url").is_none());
        assert_eq!(rows[1]["update"]["says"], "latest");
        assert!(!rows[1].to_string().contains("evil") && !rows[1].to_string().contains("<b>"));
    }
}
