//! Is an agent program the newest published version?
//!
//! The owner, 17 Nov 2026: "don't know if the version listed is the latest
//! or not". Each station asks, for each program it has, the place that
//! program's own installer (or the package manager it was installed with)
//! takes the newest version from: the catalogue says which, with where and
//! when that was established. Nothing is installed by asking.
//!
//! - It is asked when the owner asks (`agents.check`: the Agent programs
//!   dialog as it opens, `spandeck agents`), never by the station on its
//!   own and never when a pane starts. An answer is kept for three hours
//!   and reused; "Check again" asks anew.
//! - The request says nothing of the owner, the station or a project: it
//!   is a plain GET of a public address (or `npm view` / `brew info`), and
//!   what the release host learns is that some machine at this address
//!   asked for a public version number (SECURITY.md).
//! - `[agents] check_latest = false` turns it off: nothing is asked, and
//!   the dialog says checks are off.
//! - A station that cannot reach the host says so in a line ("no route to
//!   downloads.claude.ai"): that is "could not check", never "latest".
//!
//! The HTTP client is the one every outside call uses (`http.rs`), so the
//! system's proxy settings apply as they do to a release or the price list.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spandeck_agents::catalogue::{self, Latest};
use spandeck_agents::AgentKind;
use spandeck_terminal::sys;

use crate::agents::{Ended, Probe};
use crate::daemon::{shell_join, Daemon};
use crate::paths::now_ms;

/// How long an answer is reused before the dialog's opening asks again.
const KEEP_MS: u64 = 3 * 3600 * 1000;
/// A check that failed is tried again by the next look after this.
const RETRY_MS: u64 = 60 * 1000;
const CONNECT_LIMIT: Duration = Duration::from_secs(3);
const ANSWER_LIMIT: Duration = Duration::from_secs(8);
/// A package manager asked what it would install (it makes its own call).
const COMMAND_LIMIT: Duration = Duration::from_secs(20);
/// Codex's channel file is 58 KB; nothing here is near this.
const MAX_BODY: usize = 1024 * 1024;

/// What a release source last answered for one program.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Seen {
    pub checked_ms: u64,
    /// The way of installing whose source was asked.
    pub method: String,
    /// The newest published version, when it was read.
    pub version: Option<String>,
    /// Else why not, in one line.
    pub why: Option<String>,
    /// Who was asked: a host name, or the command.
    pub asked: String,
}

/// What is asked, for one program on this station.
enum Plan {
    Get {
        url: String,
        pointer: Option<String>,
    },
    Run {
        argv: Vec<String>,
        tool: Option<String>,
        pointer: Option<String>,
        shown: String,
    },
}

fn key(kind: AgentKind) -> String {
    format!("agents.latest.{}", kind.as_str())
}

/// The host of an address, for a line the owner reads.
fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_else(|| url.chars().take(60).collect())
}

/// How a copy at this path was installed, when the path says so: npm puts
/// a program under `node_modules`, Homebrew under its Cellar or Caskroom.
fn installed_with(path: &std::path::Path) -> Option<&'static str> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let real = real.to_string_lossy().replace('\\', "/");
    if real.contains("/node_modules/") {
        Some("npm")
    } else if real.contains("/Cellar/") || real.contains("/Caskroom/") {
        Some("brew")
    } else {
        None
    }
}

impl Daemon {
    /// `[agents] check_latest`: on unless the station's config says no.
    pub(crate) fn latest_on(&self) -> bool {
        self.cfg.agents.check_latest.unwrap_or(true)
    }

    /// The way this copy was installed, for choosing whom to ask: the way
    /// the owner chose here, else what its path shows, else the vendor's
    /// own script.
    pub(crate) fn latest_method(&self, kind: AgentKind, probe: &Probe) -> Option<String> {
        if let Some(chosen) = self.agent_method_chosen_id(kind) {
            return Some(chosen);
        }
        let methods = self.agent_methods(kind);
        let seen = probe.path.as_deref().and_then(installed_with);
        seen.filter(|id| methods.iter().any(|m| m.id == *id))
            .map(String::from)
            .or_else(|| methods.first().map(|m| m.id.clone()))
    }

    fn latest_plan(&self, kind: AgentKind, probe: &Probe) -> Result<(String, Plan), String> {
        let cfg = self.cfg.agents.get(kind.as_str());
        if let Some(argv) = cfg
            .and_then(|a| a.latest_cmd.clone())
            .filter(|c| !c.is_empty())
        {
            let shown = shell_join(&argv);
            return Ok((
                "config".into(),
                Plan::Run {
                    argv,
                    tool: None,
                    pointer: None,
                    shown,
                },
            ));
        }
        let mirror = cfg.and_then(|a| a.latest_url.clone());
        let method = self.latest_method(kind, probe);
        let source = catalogue::program(kind)
            .and_then(|p| p.methods.iter().find(|m| Some(m.id) == method.as_deref()))
            .and_then(|m| m.latest.as_ref());
        let method = method.unwrap_or_else(|| "config".into());
        let get = |url: &str, pointer: Option<&str>| Plan::Get {
            url: mirror
                .clone()
                .unwrap_or_else(|| url.replace("{platform}", &catalogue::platform())),
            pointer: pointer.map(String::from),
        };
        let plan = match (source.map(|s| &s.how), &mirror) {
            (Some(Latest::Text(url)), _) => get(url, None),
            (Some(Latest::Json(url, pointer)), _) => get(url, Some(pointer)),
            (Some(Latest::Command(line, pointer)), _) => Plan::Run {
                argv: sys::shell_line(line),
                tool: line.split_whitespace().next().map(String::from),
                pointer: pointer.map(String::from),
                shown: line.to_string(),
            },
            (None, Some(url)) => get(url, None),
            (None, None) => {
                return Err(format!(
                    "spandeck knows no place to ask for {}'s newest version{}",
                    kind.display(),
                    if method == "config" {
                        String::new()
                    } else {
                        format!(" when it is installed with {method}")
                    }
                ))
            }
        };
        Ok((method, plan))
    }

    /// Ask, now. Blocking: a few seconds at most.
    fn latest_ask(&self, kind: AgentKind, runtime: Option<&tokio::runtime::Handle>) -> Seen {
        let probe = self.agent_probe_last(kind);
        let mut seen = Seen {
            checked_ms: now_ms(),
            ..Default::default()
        };
        let plan = match probe
            .as_ref()
            .ok_or_else(|| "its version has not been read yet".to_string())
            .and_then(|p| self.latest_plan(kind, p))
        {
            Ok((method, plan)) => {
                seen.method = method;
                plan
            }
            Err(why) => {
                seen.why = Some(why);
                return seen;
            }
        };
        let (answer, pointer) = match plan {
            Plan::Get { url, pointer } => {
                seen.asked = host_of(&url);
                let Some(runtime) = runtime else {
                    seen.why = Some("this station cannot make the call now".into());
                    return seen;
                };
                (runtime.block_on(get(&url)), pointer)
            }
            Plan::Run {
                argv,
                tool,
                pointer,
                shown,
            } => {
                seen.asked = format!("`{shown}`");
                if let Some(tool) = tool.filter(|t| self.find_program(t).is_none()) {
                    seen.why = Some(format!("{tool} is not on this station"));
                    return seen;
                }
                let ran = self.run_captured(&argv, COMMAND_LIMIT);
                let answer = match ran.ended {
                    Ended::Exited { ok: true } => Ok(ran.text),
                    Ended::NoAnswer => Err(format!(
                        "`{shown}` did not answer within {} s",
                        COMMAND_LIMIT.as_secs()
                    )),
                    _ => Err(format!(
                        "`{shown}` failed: {}",
                        catalogue::error_line(&ran.text)
                            .unwrap_or_else(|| "it said nothing".into())
                    )),
                };
                (answer, pointer)
            }
        };
        let version = answer.and_then(|text| {
            let said = match &pointer {
                None => Some(text),
                Some(pointer) => serde_json::from_str::<Value>(&text).ok().and_then(|v| {
                    v.pointer(pointer)
                        .and_then(|v| v.as_str().map(String::from))
                }),
            };
            said.as_deref()
                .and_then(catalogue::published_version)
                .ok_or_else(|| format!("{} did not answer with a version", seen.asked))
        });
        match version {
            Ok(v) => seen.version = Some(v),
            Err(why) => seen.why = Some(why.chars().take(300).collect()),
        }
        seen
    }

    /// What was last answered for this program, from memory or the store.
    pub(crate) fn latest_seen(&self, kind: AgentKind) -> Option<Seen> {
        if let Some(seen) = self.agents.latest.lock().unwrap().get(&kind) {
            return Some(seen.clone());
        }
        let seen: Seen = serde_json::from_str(&self.db.kv_get(&key(kind))?).ok()?;
        self.agents
            .latest
            .lock()
            .unwrap()
            .insert(kind, seen.clone());
        Some(seen)
    }

    /// `agents.check`: ask about one program, or every installed one, where
    /// what is known is older than three hours (`again`: whatever is
    /// known). Returns at once with which were asked; `agents.status`
    /// carries the answers.
    pub fn agents_check(&self, agent: Option<AgentKind>, again: bool) -> Value {
        if !self.latest_on() {
            return json!({"station": self.station, "off": true, "asking": []});
        }
        let runtime = tokio::runtime::Handle::try_current().ok();
        let now = now_ms();
        let mut asking = vec![];
        for kind in self.agent_kinds() {
            if agent.is_some_and(|a| a != kind) {
                continue;
            }
            let Some(probe) = self.agent_probe_last(kind).filter(|p| p.path.is_some()) else {
                continue;
            };
            let method = self
                .latest_method(kind, &probe)
                .unwrap_or_else(|| "config".into());
            // "Check again" asks anew; one question is in flight at a time.
            let fresh = !again
                && self.latest_seen(kind).is_some_and(|s| {
                    // An answer is kept for hours; "could not check" only
                    // for a minute, so the next look tries again.
                    let keep = if s.version.is_some() {
                        KEEP_MS
                    } else {
                        RETRY_MS
                    };
                    now.saturating_sub(s.checked_ms) < keep && s.method == method
                });
            if fresh || !self.agents.checking.lock().unwrap().insert(kind) {
                continue;
            }
            asking.push(json!(kind));
            let (d, runtime) = (self.clone(), runtime.clone());
            std::thread::spawn(move || {
                let seen = d.latest_ask(kind, runtime.as_ref());
                if let Ok(text) = serde_json::to_string(&seen) {
                    d.db.kv_set(&key(kind), &text);
                }
                d.agents.latest.lock().unwrap().insert(kind, seen);
                d.agents.checking.lock().unwrap().remove(&kind);
                d.emit(
                    "agents.status",
                    None,
                    None,
                    json!({"agent": kind, "station": d.station}),
                );
            });
        }
        json!({"station": self.station, "off": false, "asking": asking})
    }

    /// The `latest` of a row in `agents.status`: `latest` (nothing newer is
    /// published), `available` (with the version), `unknown` (with why),
    /// `not_checked`, or `off`. Null for a program that is not installed.
    pub(crate) fn latest_view(&self, kind: AgentKind, probe: &Probe) -> Value {
        if probe.path.is_none() {
            return Value::Null;
        }
        if !self.latest_on() {
            return json!({"state": "off"});
        }
        let checking = self.agents.checking.lock().unwrap().contains(&kind);
        let Some(seen) = self.latest_seen(kind) else {
            return json!({"state": "not_checked", "checking": checking});
        };
        let (state, why) = match (&seen.version, &probe.version) {
            (Some(published), Some(installed)) => match catalogue::is_newer(published, installed) {
                Some(true) => ("available", None),
                Some(false) => ("latest", None),
                None if published == installed => ("latest", None),
                None => (
                    "unknown",
                    Some(format!(
                        "{installed} and the published {published} cannot be compared"
                    )),
                ),
            },
            (Some(_), None) => (
                "unknown",
                Some("this copy's own version could not be read".to_string()),
            ),
            (None, _) => ("unknown", seen.why.clone()),
        };
        json!({
            "state": state, "version": seen.version, "why": why,
            "checked_ms": seen.checked_ms, "asked": seen.asked, "method": seen.method,
            "checking": checking,
        })
    }
}

/// One GET, with short limits, and what went wrong in a line that names
/// the host.
async fn get(url: &str) -> Result<String, String> {
    let host = host_of(url);
    let http = crate::http::builder()
        .connect_timeout(CONNECT_LIMIT)
        .timeout(ANSWER_LIMIT)
        .user_agent("spandeck")
        .build()
        .map_err(|e| e.to_string())?;
    let failed = |e: reqwest::Error| {
        if e.is_timeout() && !e.is_connect() {
            format!("{host} did not answer within {} s", ANSWER_LIMIT.as_secs())
        } else if e.is_connect() || e.is_timeout() {
            format!("no route to {host}")
        } else {
            format!("{host}: {}", e.without_url())
        }
    };
    let res = http.get(url).send().await.map_err(failed)?;
    if !res.status().is_success() {
        return Err(format!("{host} answered {}", res.status().as_u16()));
    }
    let body = res.bytes().await.map_err(failed)?;
    if body.len() > MAX_BODY {
        return Err(format!("{host} answered with too much to be a version"));
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}
