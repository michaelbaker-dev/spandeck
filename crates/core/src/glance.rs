//! What a pane's header says at a glance, beyond the pane's own view: its
//! folder's git branch and how many files are changed there, the processor
//! and memory its programs are using, its unread mail, and whether a
//! newer version of its agent program is published (DECISIONS.md, "A
//! pane's header says more at a glance", 19 Nov 2026).
//!
//! `pane.glance` answers at once from what is held and never works any of
//! it out itself. A thread of its own does that, every three seconds, and
//! only while somebody asks: twenty seconds after the last question it
//! stops, so a station nobody looks at samples nothing.
//!
//! - **Not in a pane's view.** The numbers change every few seconds, and a
//!   deck's snapshot sends the whole pane list again whenever any pane's
//!   view changes (snapshot.rs). And a pane's view is read by agents: this
//!   call is not on their list, so no agent learns another project's
//!   branch from it.
//! - **Processor and memory**: one pass over the system's processes for
//!   all panes (`sys::tree_usage`), on Linux and macOS; Windows says
//!   nothing. The share is of one core over the time since the last
//!   sample, so a pane using three cores says 300 %.
//! - **git**: one `git status` per folder, kept ten seconds, given two
//!   seconds and ended after that: a folder that was too slow is left
//!   alone for two minutes and says nothing. At most a second and a half
//!   of it in a round.
//! - **An update**: only what this station already holds (latest.rs): the
//!   version it last read of the program and the published one it was
//!   last told when the owner asked. Nothing is asked here, ever.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use spandeck_agents::catalogue;
use spandeck_agents::AgentKind;
use spandeck_terminal::sys;

use crate::daemon::Daemon;
use crate::git::{self, Asked, Brief};
use crate::paths::now_ms;

/// Announced in `caps`: a deck asks `pane.glance` only of a station that
/// says this.
pub const CAP: &str = "pane_glance";

const EVERY: Duration = Duration::from_secs(3);
/// With no question for this long, sampling stops.
const STOP_AFTER: Duration = Duration::from_secs(20);
const GIT_KEEP: Duration = Duration::from_secs(10);
const GIT_LIMIT: Duration = Duration::from_secs(2);
/// A folder whose `git status` was ended is not asked again for this long.
const GIT_SLOW_KEEP: Duration = Duration::from_secs(120);
/// How much of a round may go to git.
const GIT_BUDGET: Duration = Duration::from_millis(1500);

#[derive(Default)]
pub(crate) struct GlanceState {
    held: Mutex<Held>,
    /// The programs whose last published version was looked for in the
    /// store: once each, so an answer from before a restart is found and
    /// a program never asked about costs one look.
    looked: Mutex<HashSet<AgentKind>>,
}

#[derive(Default)]
struct Held {
    asked: Option<Instant>,
    running: bool,
    sampled_ms: u64,
    used: HashMap<String, Used>,
    git: HashMap<String, GitSeen>,
    mail: HashMap<String, usize>,
}

struct Used {
    pid: i32,
    at: Instant,
    cpu_ms: u64,
    /// Percent of one core since the sample before; none at the first.
    cpu_pct: Option<f64>,
    rss_bytes: u64,
    count: u32,
}

struct GitSeen {
    at: Instant,
    keep: Duration,
    found: Option<Brief>,
    slow: bool,
}

/// A pane, as a round reads it.
struct Live {
    id: String,
    pid: i32,
    cwd: String,
    agent: AgentKind,
}

impl Daemon {
    fn glance_live(&self) -> Vec<Live> {
        self.list_panes()
            .iter()
            .filter_map(|p| {
                let m = p.meta.lock().unwrap();
                m.exit.is_none().then(|| Live {
                    id: m.id.clone(),
                    pid: p.term.pid(),
                    cwd: m.cwd.clone(),
                    agent: m.agent,
                })
            })
            .collect()
    }

    /// `pane.glance`: what is held, at once. Asking is what keeps the
    /// sampling going.
    pub fn glance(&self) -> Value {
        let live = self.glance_live();
        let mut panes = Map::new();
        let sampled_ms = {
            let mut held = self.glance.held.lock().unwrap();
            held.asked = Some(Instant::now());
            if !held.running {
                held.running = true;
                let d = self.clone();
                let started = std::thread::Builder::new()
                    .name("glance".into())
                    .spawn(move || d.glance_loop());
                if started.is_err() {
                    held.running = false;
                }
            }
            for p in &live {
                let mut row = Map::new();
                if let Some(u) = held.used.get(&p.id).filter(|u| u.pid == p.pid) {
                    if let Some(pct) = u.cpu_pct {
                        row.insert("cpu_pct".into(), json!((pct * 10.0).round() / 10.0));
                    }
                    row.insert("mem_bytes".into(), json!(u.rss_bytes));
                    row.insert("procs".into(), json!(u.count));
                }
                if let Some(g) = held.git.get(&p.cwd) {
                    if let Some(b) = &g.found {
                        row.insert(
                            "git".into(),
                            json!({"branch": b.branch, "detached": b.detached, "changed": b.changed}),
                        );
                    } else if g.slow {
                        row.insert("git_slow".into(), json!(true));
                    }
                }
                if let Some(n) = held.mail.get(&p.id).filter(|n| **n > 0) {
                    row.insert("mail".into(), json!(n));
                }
                panes.insert(p.id.clone(), Value::Object(row));
            }
            held.sampled_ms
        };
        let mut programs = Map::new();
        for kind in live.iter().map(|p| p.agent).filter(|k| k.is_agent()) {
            if !programs.contains_key(kind.as_str()) {
                if let Some(v) = self.glance_program(kind) {
                    programs.insert(kind.as_str().to_string(), v);
                }
            }
        }
        json!({
            "station": self.station,
            "every_ms": EVERY.as_millis() as u64,
            "sampled_ms": sampled_ms,
            "load": sys::load_average(),
            "panes": panes,
            "programs": programs,
        })
    }

    /// An agent program's version as this station last read it, and the
    /// newer one published, if the station was told of one when the owner
    /// last asked. Only what is held: no program is run and no release
    /// host is asked.
    fn glance_program(&self, kind: AgentKind) -> Option<Value> {
        let installed = self.agent_probe_last(kind).and_then(|p| p.version)?;
        let mut v = json!({"name": kind.display(), "version": installed});
        if !self.latest_on() {
            return Some(v);
        }
        let in_memory = self.agents.latest.lock().unwrap().get(&kind).cloned();
        let seen = match in_memory {
            Some(seen) => Some(seen),
            // From before a restart: the store, looked in once.
            None if self.glance.looked.lock().unwrap().insert(kind) => self.latest_seen(kind),
            None => None,
        };
        if let Some(seen) = seen {
            if let Some(published) = &seen.version {
                if catalogue::is_newer(published, &installed) == Some(true) {
                    v["available"] = json!(published);
                    v["checked_ms"] = json!(seen.checked_ms);
                }
            }
        }
        Some(v)
    }

    fn glance_loop(&self) {
        loop {
            {
                let mut held = self.glance.held.lock().unwrap();
                if held.asked.is_none_or(|at| at.elapsed() > STOP_AFTER) {
                    held.running = false;
                    // Nothing held is true any more by the time it is asked again.
                    held.used.clear();
                    return;
                }
            }
            self.glance_round();
            std::thread::sleep(EVERY);
        }
    }

    /// One round: every pane's programs in one pass, each agent's unread
    /// mail, and the folders whose git answer has grown old.
    fn glance_round(&self) {
        let live = self.glance_live();
        let pids: Vec<i32> = live.iter().map(|p| p.pid).collect();
        let usage = sys::tree_usage(&pids);
        let now = Instant::now();
        let mail: HashMap<String, usize> = live
            .iter()
            .filter(|p| p.agent.is_agent())
            .map(|p| {
                let names = self.pane_names(&p.id);
                (
                    p.id.clone(),
                    self.mail().inbox(&names, true, false, now_ms()).len(),
                )
            })
            .collect();
        let stale: Vec<String> = {
            let mut held = self.glance.held.lock().unwrap();
            held.sampled_ms = now_ms();
            held.mail = mail;
            match usage {
                None => held.used.clear(),
                Some(usage) => {
                    let mut next = HashMap::new();
                    for (p, u) in live.iter().zip(usage) {
                        if u.count == 0 {
                            continue;
                        }
                        let cpu_pct = held
                            .used
                            .get(&p.id)
                            .filter(|was| was.pid == p.pid)
                            .and_then(|was| {
                                let ms = now.duration_since(was.at).as_millis() as f64;
                                // A program that ended took its time with
                                // it: never less than nothing.
                                (ms >= 250.0).then(|| {
                                    u.cpu_ms.saturating_sub(was.cpu_ms) as f64 * 100.0 / ms
                                })
                            });
                        next.insert(
                            p.id.clone(),
                            Used {
                                pid: p.pid,
                                at: now,
                                cpu_ms: u.cpu_ms,
                                cpu_pct,
                                rss_bytes: u.rss_bytes,
                                count: u.count,
                            },
                        );
                    }
                    held.used = next;
                }
            }
            let folders: HashSet<&str> = live.iter().map(|p| p.cwd.as_str()).collect();
            held.git
                .retain(|folder, _| folders.contains(folder.as_str()));
            let mut stale: Vec<String> = folders
                .into_iter()
                .filter(|f| held.git.get(*f).is_none_or(|g| g.at.elapsed() > g.keep))
                .map(String::from)
                .collect();
            // The one never asked first, then the oldest answer.
            stale.sort_by_key(|f| held.git.get(f).map(|g| g.at));
            stale
        };
        let began = Instant::now();
        for folder in stale {
            if began.elapsed() > GIT_BUDGET {
                break;
            }
            let seen = match git::brief(Path::new(&folder), GIT_LIMIT) {
                Asked::Repo(b) => GitSeen {
                    at: Instant::now(),
                    keep: GIT_KEEP,
                    found: Some(b),
                    slow: false,
                },
                Asked::NotRepo => GitSeen {
                    at: Instant::now(),
                    keep: GIT_KEEP,
                    found: None,
                    slow: false,
                },
                Asked::TooSlow => GitSeen {
                    at: Instant::now(),
                    keep: GIT_SLOW_KEEP,
                    found: None,
                    slow: true,
                },
            };
            self.glance.held.lock().unwrap().git.insert(folder, seen);
        }
    }
}
