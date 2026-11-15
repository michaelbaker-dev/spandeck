//! After a restart: nothing is left wrong, and the owner is told once.
//!
//! `Daemon::recover` (daemon.rs) starts agents again by session id. This
//! module is what goes with it (DECISIONS.md, "After a restart"): the count
//! of starts that followed no clean stop, which holds sessions back at the
//! third in ten minutes; panes that could not be started again, which stay
//! listed as ended for seven days; tasks failed with their resume
//! information, and `task.continue`; approvals nobody can answer any more;
//! what a pane's program left running; the one notice and the
//! `station.recovered` entry; and the station's switches (`station.flags`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spandeck_agents::AgentState;
use spandeck_terminal::{sys, ExitInfo, Pane};
use tokio::sync::broadcast;

use crate::daemon::{invalid, Daemon, PaneEntry, PaneMeta, Result};
use crate::paths::now_ms;

/// Starts that followed no clean stop are counted over this long.
pub const LOOP_WINDOW_MS: u64 = 10 * 60_000;
/// At this many of them, sessions are listed and not started.
pub const LOOP_STARTS: usize = 3;
/// One recovery notice in this long; later recoveries rewrite it.
pub const NOTICE_EVERY_MS: u64 = 10 * 60_000;
/// A pane that ended with a restart stays listed this long.
const ENDED_LISTED_MS: u64 = 7 * 86_400_000;
/// A program's start as `ps` gives it (whole seconds) against the time
/// kept when it was started.
const START_SLACK_MS: u64 = 5_000;
/// "Last running" is written every ten seconds; a leftover started no
/// later than this after it.
const ALIVE_SLACK_MS: u64 = 30_000;

const KV_CLEAN: &str = "restore.stopped_clean";
const KV_STARTS: &str = "restore.starts";
const KV_ALIVE: &str = "restore.alive";
const KV_NOTICE: &str = "restore.notice";
const KV_FLAGS: &str = "flags";

/// What a start found out before it restored anything.
#[derive(Debug, Clone)]
pub struct StartInfo {
    pub at_ms: u64,
    /// The station was stopped on purpose before this start.
    pub clean: bool,
    /// The third start in ten minutes that followed no clean stop: sessions
    /// are listed and none is started.
    pub hold: bool,
    /// Such starts in the last ten minutes, this one included.
    pub starts: usize,
    /// When the daemon before this one was last known to be running.
    pub last_alive_ms: Option<u64>,
    /// Not a start at all: panes lost in an upgrade's handoff are looked at.
    /// Sessions held back before it stay held back.
    pub handoff: bool,
}

impl StartInfo {
    /// For panes lost in an upgrade's handoff: the daemon before was this
    /// same process a moment ago, and this is not a start that is counted.
    pub fn handoff(now: u64) -> StartInfo {
        StartInfo {
            at_ms: now,
            clean: true,
            hold: false,
            starts: 0,
            last_alive_ms: Some(now),
            handoff: true,
        }
    }

    pub fn away_ms(&self) -> Option<u64> {
        self.last_alive_ms.map(|a| self.at_ms.saturating_sub(a))
    }
}

/// What `recover` did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Recovery {
    /// Panes that were open and not kept alive: each was started again,
    /// held back, or listed as ended.
    pub panes: usize,
    /// Kept alive through it (the pane host, or an upgrade's handoff).
    pub kept: usize,
    pub resumed: usize,
    pub closed_shells: usize,
    pub closed_agents: usize,
    pub held: usize,
    pub tasks_failed: usize,
    pub approvals_expired: usize,
    /// Programs still running from panes that were not started again:
    /// "name (pid 123)", ten at most. Listed, never touched.
    pub leftovers: Vec<String>,
    pub leftovers_more: usize,
    /// Programs ended because their pane was started again.
    pub leftovers_ended: usize,
    /// Sessions started again that take no mail until the owner releases
    /// them (holds.rs). None when that hold is switched off here.
    pub mail_held: usize,
}

impl Recovery {
    pub fn closed(&self) -> usize {
        self.closed_shells + self.closed_agents
    }

    pub(crate) fn note_left(&mut self, left: &[sys::Proc]) {
        for p in left {
            if self.leftovers.len() >= 10 {
                self.leftovers_more += 1;
                continue;
            }
            // A program's name is text from somewhere else.
            let name: String = p
                .name
                .chars()
                .filter(|c| !c.is_control())
                .take(40)
                .collect();
            self.leftovers.push(format!("{name} (pid {})", p.pid));
        }
    }
}

/// For the program that keeps a Windows station running (`spandeckd`): did
/// this station's daemon last end because it was asked to stop? Read from
/// the station's database without the daemon, which has ended. None when
/// that cannot be read, or the station has never said.
pub fn stopped_clean(paths: &crate::paths::Paths) -> Option<bool> {
    use rusqlite::OpenFlags;
    let c = rusqlite::Connection::open_with_flags(
        paths.db(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    c.busy_timeout(std::time::Duration::from_secs(2)).ok()?;
    let v: String = c
        .query_row("SELECT v FROM kv WHERE k = ?1", [KV_CLEAN], |r| r.get(0))
        .ok()?;
    Some(v == "1")
}

/// The count of starts that followed no clean stop. Returns the times to
/// keep and whether this start holds sessions back. A clean stop resets it.
pub fn count_start(earlier: &[u64], clean: bool, now: u64) -> (Vec<u64>, bool) {
    if clean {
        return (vec![], false);
    }
    let mut starts: Vec<u64> = earlier
        .iter()
        .copied()
        .filter(|t| *t <= now && now - *t < LOOP_WINDOW_MS)
        .collect();
    starts.push(now);
    let keep = starts.len().saturating_sub(8);
    let starts = starts.split_off(keep);
    let hold = starts.len() >= LOOP_STARTS;
    (starts, hold)
}

/// The recovery notice that stands, kept across restarts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NoticeState {
    pub id: String,
    /// The first restart it speaks of.
    pub first_ms: u64,
    /// How many restarts it speaks of.
    pub count: u32,
}

/// At most one recovery notice in ten minutes: a recovery inside that time
/// of the notice that stands rewrites it and counts one more; otherwise a
/// new notice starts. Returns the state to keep; its `id` is empty when a
/// new notice is to be raised.
pub fn plan_notice(standing: Option<&NoticeState>, now: u64) -> NoticeState {
    match standing {
        Some(s) if now >= s.first_ms && now - s.first_ms < NOTICE_EVERY_MS && !s.id.is_empty() => {
            NoticeState {
                id: s.id.clone(),
                first_ms: s.first_ms,
                count: s.count + 1,
            }
        }
        _ => NoticeState {
            id: String::new(),
            first_ms: now,
            count: 1,
        },
    }
}

/// "14:02 UTC": the same words whatever the station's time zone.
pub fn clock_utc(ms: u64) -> String {
    let minutes = ms / 60_000;
    format!("{:02}:{:02} UTC", (minutes / 60) % 24, minutes % 60)
}

fn length(ms: u64) -> String {
    match ms / 1000 {
        s if s < 120 => format!("{s} s"),
        s if s < 7200 => format!("{} min", s / 60),
        s if s < 172_800 => format!("{} h", s / 3600),
        s => format!("{} days", s / 86_400),
    }
}

fn many(n: usize, one: &str, more: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { more })
}

/// The words of the recovery notice.
pub fn notice_text(
    station: &str,
    start: &StartInfo,
    found: &Recovery,
    notice: &NoticeState,
) -> (String, String) {
    let title = if notice.count > 1 {
        format!(
            "{station} restarted {} times since {}",
            notice.count,
            clock_utc(notice.first_ms)
        )
    } else {
        let away = start
            .away_ms()
            .map(|a| format!(" (away {})", length(a)))
            .unwrap_or_default();
        format!("{station} restarted at {}{away}", clock_utc(start.at_ms))
    };
    let mut parts = vec![];
    if found.resumed > 0 {
        parts.push(format!(
            "{} started again",
            many(found.resumed, "session was", "sessions were")
        ));
    }
    if found.held > 0 {
        parts.push(format!(
            "{} not started again, because spandeck restarted {LOOP_STARTS} times in 10 minutes",
            many(found.held, "session was", "sessions were")
        ));
    }
    if found.closed_shells > 0 {
        parts.push(format!(
            "{} ended",
            many(found.closed_shells, "shell", "shells")
        ));
    }
    if found.closed_agents > 0 {
        parts.push(format!(
            "{} ended",
            many(
                found.closed_agents,
                "agent with no conversation to go back to",
                "agents with no conversation to go back to"
            )
        ));
    }
    if found.tasks_failed > 0 {
        parts.push(format!(
            "{} failed",
            many(found.tasks_failed, "task", "tasks")
        ));
    }
    if found.kept > 0 {
        parts.push(format!(
            "{} kept running",
            many(found.kept, "session", "sessions")
        ));
    }
    let mut body = format!("{}.", parts.join(", "));
    if found.mail_held > 0 {
        // The words name no button: an older Control Deck has none, and
        // typing into a pane releases it from any deck.
        body.push_str(
            " They wait for you, and take no mail from other agents until you release them: \
             type into one, or release them all (Release all on this station, or \
             `spandeck pane release --all`).",
        );
    } else if found.resumed + found.held > 0 {
        body.push_str(" They wait for you.");
    }
    if found.held + found.closed() > 0 {
        body.push_str(" One that was not started can be brought back from its pane.");
    }
    if found.leftovers_ended > 0 {
        body.push_str(&format!(
            " {} left running by the sessions started again {} ended.",
            many(found.leftovers_ended, "program", "programs"),
            if found.leftovers_ended == 1 {
                "was"
            } else {
                "were"
            }
        ));
    }
    if !found.leftovers.is_empty() {
        body.push_str(&format!(
            " Still running from sessions that were not started again, and not touched: {}{}.",
            found.leftovers.join(", "),
            if found.leftovers_more > 0 {
                format!(" and {} more", found.leftovers_more)
            } else {
                String::new()
            }
        ));
    }
    (title, body)
}

/// What a pane's program left running after the daemon that held the pane
/// ended: every process still in the session that program started.
///
/// `pid` and `started_ms` are the program's, as kept in the pane's record;
/// `procs` is what runs now. A pid is given to another process in time, so
/// nothing is returned unless everything agrees: the record has both the
/// pid and the start time; if a process has that pid now, it leads that
/// session and started when the record says (within `ps`'s whole seconds),
/// else the number was given to something else; and each process returned
/// started no earlier than the program and no later than the daemon was
/// last known to be running, so a session that reused the number while the
/// station was down is not taken for the pane's. This process is never
/// returned.
pub fn leftovers(
    procs: &[sys::Proc],
    pid: Option<i64>,
    started_ms: Option<u64>,
    last_alive_ms: Option<u64>,
    now: u64,
) -> Vec<sys::Proc> {
    let (Some(pid), Some(started), Some(alive)) = (pid, started_ms, last_alive_ms) else {
        return vec![];
    };
    let me = std::process::id() as i64;
    if pid <= 1 || pid == me {
        return vec![];
    }
    let start_of = |p: &sys::Proc| now.saturating_sub(p.age_secs.saturating_mul(1000));
    if let Some(leader) = procs.iter().find(|p| p.pid as i64 == pid) {
        if leader.sid as i64 != pid || start_of(leader).abs_diff(started) > START_SLACK_MS {
            return vec![];
        }
    }
    procs
        .iter()
        .filter(|p| p.sid as i64 == pid && p.pid as i64 != me)
        .filter(|p| {
            let at = start_of(p);
            at + START_SLACK_MS >= started && at <= alive + ALIVE_SLACK_MS
        })
        .cloned()
        .collect()
}

/// A flag's name: `holds.restored`, `deck_input.fresh_ms`.
fn flag_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.'))
}

/// A flag's value: true or false, a whole number, or a short text.
fn flag_value_ok(v: &Value) -> bool {
    match v {
        Value::Bool(_) => true,
        Value::Number(n) => n.is_i64() || n.is_u64(),
        Value::String(s) => s.len() <= 200 && !s.chars().any(|c| c.is_control()),
        _ => false,
    }
}

/// `[flags]` as the config has it: a table inside it (`[flags.holds]`, or
/// `holds.restored = false`) gives dotted names. What is not a flag's kind
/// of value, or has no flag's kind of name, is passed over.
pub fn flags_of_config(table: &BTreeMap<String, toml::Value>) -> BTreeMap<String, Value> {
    fn walk(prefix: &str, v: &toml::Value, out: &mut BTreeMap<String, Value>, depth: u8) {
        match v {
            toml::Value::Table(t) if depth < 4 => {
                for (k, v) in t {
                    walk(&format!("{prefix}.{k}"), v, out, depth + 1);
                }
            }
            other => {
                let value = match other {
                    toml::Value::Boolean(b) => json!(b),
                    toml::Value::Integer(i) => json!(i),
                    toml::Value::String(s) => json!(s),
                    _ => return,
                };
                if flag_name_ok(prefix) && flag_value_ok(&value) && out.len() < 64 {
                    out.insert(prefix.to_string(), value);
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    for (k, v) in table {
        walk(k, v, &mut out, 0);
    }
    out
}

#[derive(Default)]
pub struct RestoreState {
    /// The switches in force: the config's, with the owner's over them.
    flags: RwLock<BTreeMap<String, Value>>,
    /// Held while the owner's switches are read, changed and written.
    flags_lock: Mutex<()>,
}

impl Daemon {
    // ------------------------------------------------------------- switches

    fn flags_set_by_owner(&self) -> BTreeMap<String, Value> {
        self.db
            .kv_get(KV_FLAGS)
            .and_then(|s| serde_json::from_str::<BTreeMap<String, Value>>(&s).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|(k, v)| flag_name_ok(k) && flag_value_ok(v))
            .collect()
    }

    /// Work out the switches in force. At start, and after the owner
    /// changed one.
    pub(crate) fn flags_load(&self) {
        let mut flags = flags_of_config(&self.cfg.flags);
        flags.extend(self.flags_set_by_owner());
        *self.restore.flags.write().unwrap() = flags;
        // `db_writer = false`: the ship's log and pane records are written
        // by their callers again (db.rs says what that does not take back).
        self.db.set_queued(self.flag_on("db_writer", true));
        // `request_ids = false`: no id is kept and none is announced, so
        // no change is tried again by its id at this station (rids.rs).
        self.rids.set_on(self.flag_on(crate::rids::FLAG, true));
    }

    /// A switch as it stands now, if the config or the owner set it.
    pub fn flag(&self, name: &str) -> Option<Value> {
        self.restore.flags.read().unwrap().get(name).cloned()
    }

    /// A true/false switch; `default` when nobody set it, or set it to
    /// something that is not true or false.
    pub fn flag_on(&self, name: &str, default: bool) -> bool {
        self.flag(name).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    /// `station.flags` with nothing to set: what is in force, and where
    /// each came from.
    pub fn flags_view(&self) -> Value {
        json!({
            "station": self.station,
            "flags": *self.restore.flags.read().unwrap(),
            "config": flags_of_config(&self.cfg.flags),
            "set": self.flags_set_by_owner(),
        })
    }

    /// `station.flags {set: {name: value | null}}`: the owner sets a switch
    /// for this station, or (null) takes back what they set so that the
    /// config's value, or the default, stands again. Kept in the station's
    /// own state; the config file is not written.
    pub fn flags_set(&self, set: &Value, by: &str) -> Result<Value> {
        let changes = set
            .as_object()
            .ok_or_else(|| invalid("set: an object of flag names and values"))?;
        let _one = self.restore.flags_lock.lock().unwrap();
        let mut mine = self.flags_set_by_owner();
        for (name, value) in changes {
            if !flag_name_ok(name) {
                return Err(invalid(format!(
                    "{name:?} is not a flag's name: lower-case letters, digits, _ and ., 64 at most"
                )));
            }
            if value.is_null() {
                mine.remove(name);
            } else if flag_value_ok(value) {
                mine.insert(name.clone(), value.clone());
            } else {
                return Err(invalid(format!(
                    "{name}: a flag is true or false, a whole number, or a text of at most 200 characters"
                )));
            }
        }
        if mine.len() > 64 {
            return Err(invalid("at most 64 flags can be set"));
        }
        self.db.kv_set(KV_FLAGS, &json!(mine).to_string());
        self.flags_load();
        self.record(
            "station.flags",
            None,
            None,
            None,
            Some(by),
            json!({"set": changes}),
        );
        Ok(self.flags_view())
    }

    // ---------------------------------------------------------------- start

    /// Does the ship's log say the station was stopped on purpose? For the
    /// first start after an update from a version that kept no mark.
    fn log_says_clean(&self) -> bool {
        let last = self
            .db
            .log_query(None, Some("station."), 0, 200)
            .into_iter()
            .find(|e| {
                matches!(
                    e["kind"].as_str(),
                    Some("station.stopped" | "station.started" | "station.upgraded")
                )
            });
        matches!(
            last.as_ref().and_then(|e| e["kind"].as_str()),
            None | Some("station.stopped")
        )
    }

    /// Before anything is restored: did this start follow a clean stop, how
    /// many lately did not, and when was the station last running.
    pub(crate) fn restore_begin(&self) -> StartInfo {
        let now = now_ms();
        let clean = match self.db.kv_get(KV_CLEAN).as_deref() {
            Some("1") => true,
            Some(_) => false,
            None => self.log_says_clean(),
        };
        let earlier: Vec<u64> = self
            .db
            .kv_get(KV_STARTS)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let (starts, hold) = count_start(&earlier, clean, now);
        self.db.kv_put(KV_STARTS, &json!(starts).to_string());
        StartInfo {
            at_ms: now,
            clean,
            hold,
            starts: starts.len(),
            last_alive_ms: self.db.kv_get(KV_ALIVE).and_then(|s| s.parse().ok()),
            handoff: false,
        }
    }

    /// The station is in service. From here on, ending without a stop is
    /// what the next start finds.
    pub(crate) fn restore_started(&self, adopted: bool) {
        self.db.kv_put(KV_CLEAN, "0");
        self.restore_alive();
        self.load_ended();
        if adopted {
            // Panes from a version that did not keep when their program
            // started: read once, off the path of the upgrade.
            let d = self.clone();
            std::thread::spawn(move || d.backfill_proc_starts());
        }
    }

    /// Written every ten seconds: when this daemon was last running, which
    /// is what "away" is counted from at the next start.
    pub(crate) fn restore_alive(&self) {
        self.db.kv_put(KV_ALIVE, &now_ms().to_string());
    }

    /// The station was asked to stop: the next start follows a clean stop.
    pub(crate) fn restore_stopping(&self) {
        self.restore_alive();
        self.db.kv_put(KV_CLEAN, "1");
    }

    fn backfill_proc_starts(&self) {
        let missing: Vec<Arc<PaneEntry>> = self
            .list_panes()
            .into_iter()
            .filter(|p| {
                let m = p.meta.lock().unwrap();
                m.exit.is_none() && m.proc_started_ms.is_none()
            })
            .collect();
        if missing.is_empty() {
            return;
        }
        let procs = sys::processes();
        let now = now_ms();
        for p in missing {
            let pid = p.term.pid();
            let Some(found) = procs.iter().find(|x| x.pid == pid && x.sid == pid) else {
                continue;
            };
            p.meta.lock().unwrap().proc_started_ms =
                Some(now.saturating_sub(found.age_secs.saturating_mul(1000)));
            self.save_pane(&p);
        }
    }

    // ------------------------------------------------- panes that are listed

    /// List a pane that was not started again: `held` when it is held back
    /// after three restarts (its record stays open), otherwise as ended
    /// with the restart (kept for seven days). It has no program; "Bring
    /// back" is `pane.restart`.
    pub(crate) fn list_ended(&self, mut meta: PaneMeta, token: String, held: bool) {
        let now = now_ms();
        meta.exit = Some(ExitInfo::Unknown);
        meta.state = AgentState::Idle;
        meta.state_since_ms = now;
        meta.needs_reason = None;
        meta.tool = None;
        meta.subagents.clear();
        meta.controller = None;
        if held {
            meta.held_back = true;
        } else {
            meta.ended_by = Some("restart".into());
            meta.ended_ms = now;
        }
        if let Some(p) = self.insert_ended(meta, token) {
            self.save_pane(&p);
        }
    }

    fn insert_ended(&self, meta: PaneMeta, token: String) -> Option<Arc<PaneEntry>> {
        let note = if meta.held_back {
            format!(
                "Not started again: spandeck restarted {LOOP_STARTS} times in 10 minutes.\r\nBring it back from this pane when the station is steady."
            )
        } else {
            format!(
                "Ended when spandeck restarted at {}.\r\n{}",
                clock_utc(meta.ended_ms),
                if meta.agent.is_agent() {
                    "It can be brought back from this pane."
                } else {
                    "What was running here did not survive. A new shell can be started from this pane."
                }
            )
        };
        let term = match Pane::ended(meta.spec.cols.max(2), meta.spec.rows.max(1), &note) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(pane = %meta.id, error = %e, "an ended pane could not be listed");
                return None;
            }
        };
        let (tx, _) = broadcast::channel(16);
        let entry = Arc::new(PaneEntry {
            id: meta.id.clone(),
            token,
            term,
            tx,
            meta: Mutex::new(meta),
        });
        self.panes
            .write()
            .unwrap()
            .insert(entry.id.clone(), entry.clone());
        Some(entry)
    }

    /// Panes listed at an earlier start that are still to be shown: ended
    /// with a restart in the last seven days, or held back while this
    /// daemon took over from the one that held them.
    fn load_ended(&self) {
        let cutoff = now_ms().saturating_sub(ENDED_LISTED_MS) as i64;
        let rows: Vec<(String, String, String, Option<i64>)> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, meta, token, closed FROM panes WHERE closed IS NULL OR closed > ?1",
                )?;
                let rows = stmt.query_map([cutoff], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?;
                rows.collect()
            })
            .unwrap_or_default();
        for (id, meta, token, closed) in rows {
            if self.panes.read().unwrap().contains_key(&id) {
                continue;
            }
            let Ok(meta) = serde_json::from_str::<PaneMeta>(&meta) else {
                continue;
            };
            let listed = match closed {
                Some(_) => meta.ended_by.is_some(),
                None => meta.held_back,
            };
            if listed {
                self.insert_ended(meta, token);
            }
        }
    }

    /// Seven days after it ended with a restart, a pane leaves the list.
    pub(crate) fn prune_ended(&self) {
        let cutoff = now_ms().saturating_sub(ENDED_LISTED_MS);
        let old: Vec<String> = self
            .list_panes()
            .iter()
            .filter(|p| {
                let m = p.meta.lock().unwrap();
                m.ended_by.is_some() && m.ended_ms < cutoff
            })
            .map(|p| p.id.clone())
            .collect();
        for id in old {
            self.panes.write().unwrap().remove(&id);
            self.emit(
                "pane.closed",
                Some(&id),
                None,
                json!({"why": "listed for seven days"}),
            );
        }
    }

    /// The owner closed a pane that was only listed: its record no longer
    /// says so, and it is not listed at the next start.
    pub(crate) fn forget_ended(&self, meta: &PaneMeta) {
        if meta.ended_by.is_none() && !meta.held_back {
            return;
        }
        let mut meta = meta.clone();
        meta.ended_by = None;
        meta.held_back = false;
        self.db.put(crate::db::Op::PaneUnlisted {
            id: meta.id.clone(),
            meta: serde_json::to_string(&meta).unwrap_or_default(),
            at: now_ms() as i64,
        });
    }

    // ---------------------------------------------------------------- tasks

    /// A pane's program went with the daemon. If it was working on a task,
    /// the task did not complete: it failed, and keeps what is needed to
    /// resume (the Workspaces Guarantee). No notice of its own: the
    /// recovery notice counts it.
    pub(crate) fn fail_task_for_restart(&self, meta: &PaneMeta) -> bool {
        let Some(task) = &meta.task else { return false };
        let Ok(t) = self.task_get(task) else {
            return false;
        };
        if !matches!(
            t["state"].as_str(),
            Some("running" | "blocked" | "waiting_for_approval" | "queued")
        ) {
            return false;
        }
        self.task_set_state(
            task,
            "failed",
            json!({"resume": {
                "reason": "spandeck restarted",
                "at_ms": now_ms(),
                "session_id": meta.session_id, "pane": meta.id,
                "worktree": t["workspace"]["path"], "branch": t["workspace"]["branch"],
            }}),
            "spandeck",
        )
        .is_ok()
    }

    /// `task.continue`: the owner sends a failed task's agent back to it.
    /// The task is running again, and its pane is typed one line saying
    /// what happened. Only for a task that kept resume information, whose
    /// pane is running and is not paused: Pause means no new work.
    pub fn task_continue(&self, task: &str, by: &str) -> Result<Value> {
        let t = self.task_get(task)?;
        if t["state"] != json!("failed") {
            return Err(invalid(format!(
                "only a failed task can be continued; this one is {}",
                t["state"].as_str().unwrap_or("?").replace('_', " ")
            )));
        }
        let resume = &t["resume"];
        let pane_id = resume["pane"]
            .as_str()
            .or(t["pane"].as_str())
            .ok_or_else(|| {
                invalid("this task kept no resume information: there is no pane to continue it in")
            })?;
        let pane = self.pane(pane_id).map_err(|_| {
            invalid("the pane this task ran in is closed: the task cannot be continued there")
        })?;
        let meta = pane.meta();
        if meta.exit.is_some() {
            return Err(invalid(format!(
                "{} is not running: bring the pane back first, then continue the task",
                meta.name
            )));
        }
        if meta.task.as_deref() != Some(task) {
            return Err(invalid(format!(
                "{} is no longer working on this task",
                meta.name
            )));
        }
        if meta.paused {
            return Err(invalid(format!(
                "{} is paused, and a paused pane is given no new work: unpause it first",
                meta.name
            )));
        }
        let at = resume["at_ms"]
            .as_u64()
            .or(t["updated"].as_u64())
            .unwrap_or(0);
        let line = if resume["reason"] == json!("spandeck restarted") {
            format!(
                "spandeck: your task was interrupted by a restart at {}. Check your last step, then continue.",
                clock_utc(at)
            )
        } else {
            format!(
                "spandeck: your task was interrupted at {}. Check your last step, then continue.",
                clock_utc(at)
            )
        };
        let updated = self.task_set_state(
            task,
            "running",
            json!({"resume": {"continued_ms": now_ms(), "continued_by": by}}),
            by,
        )?;
        // The owner sends this into the pane: it is released, as by typing.
        self.release_by_typing(&pane.id, by);
        self.send_text(&pane.id, &line, true)?;
        self.record(
            "task.continued",
            Some(&pane.id),
            meta.project.as_deref(),
            Some(task),
            Some(by),
            json!({}),
        );
        Ok(updated)
    }

    // ------------------------------------------------------------ approvals

    /// After a restart: an approval that was waiting has nobody waiting for
    /// its answer when its agent's program went with the daemon. It is
    /// expired, so it leaves every list. One whose pane was kept alive
    /// (`kept`) is left as it is. Returns how many.
    pub(crate) fn expire_ghost_approvals(&self, kept: &[String]) -> usize {
        let waiting: Vec<(String, String)> = self
            .db
            .read(|c| {
                let mut stmt =
                    c.prepare("SELECT id, pane FROM approvals WHERE status='pending'")?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect()
            })
            .unwrap_or_default();
        let mut expired = 0;
        for (id, pane) in waiting {
            if kept.contains(&pane) {
                continue;
            }
            self.expire_approval(&id, &pane);
            expired += 1;
        }
        expired
    }

    fn expire_approval(&self, id: &str, pane: &str) {
        self.db.put(crate::db::Op::ApprovalStatus {
            id: id.to_string(),
            status: "expired".into(),
            at: now_ms() as i64,
            by: None,
            reason: None,
            only_pending: true,
        });
        self.emit(
            "approval.resolved",
            Some(pane),
            None,
            json!({"id": id, "status": "expired", "why": "restart"}),
        );
    }

    /// The owner answered an approval nobody is waiting on: its pane was
    /// kept through a restart, but the hook that asked was cut off, and the
    /// agent went on to ask in its own terminal. It cannot be answered, so
    /// it is expired and leaves the list instead of staying for ever. The
    /// pane's state is not changed: the agent may well still be asking.
    pub(crate) fn expire_unanswerable(&self, id: &str) {
        let pane: Option<String> = self
            .db
            .read(|c| {
                c.query_row(
                    "SELECT pane FROM approvals WHERE id=?1 AND status='pending'",
                    [id],
                    |r| r.get(0),
                )
            })
            .ok();
        if let Some(pane) = pane {
            self.expire_approval(id, &pane);
        }
    }

    // --------------------------------------------------------------- report

    /// One entry in the ship's log for a recovery, and one notice for the
    /// owner: at most one in ten minutes, a later recovery rewriting it.
    /// `why`: "crash" (the station ended without being stopped), "stop"
    /// (it was stopped and started), "upgrade" (panes lost in a handoff).
    pub(crate) fn restore_report(&self, start: &StartInfo, found: &Recovery, why: &str) {
        if why != "crash" && found.panes == 0 {
            return;
        }
        self.record(
            "station.recovered",
            None,
            None,
            None,
            None,
            json!({
                "why": why, "away_ms": start.away_ms(), "at_ms": start.at_ms,
                "panes": found.panes + found.kept, "kept": found.kept,
                "resumed": found.resumed, "closed": found.closed(),
                "held_back": found.held, "tasks_failed": found.tasks_failed,
                "approvals_expired": found.approvals_expired, "mail_held": found.mail_held,
                "leftovers": found.leftovers, "leftovers_ended": found.leftovers_ended,
                "unclean_starts": start.starts,
            }),
        );
        // No session was touched: there is nothing for the owner to do.
        if found.panes == 0 {
            return;
        }
        let standing: Option<NoticeState> = self
            .db
            .kv_get(KV_NOTICE)
            .and_then(|s| serde_json::from_str(&s).ok());
        let mut notice = plan_notice(standing.as_ref(), start.at_ms);
        let (title, body) = notice_text(&self.station, start, found, &notice);
        if notice.id.is_empty()
            || !self.notify_rewrite(&notice.id, "station.recovered", &title, &body)
        {
            notice.id = self.notify_station("station.recovered", &title, &body);
        }
        self.db.kv_set(
            KV_NOTICE,
            &serde_json::to_string(&notice).unwrap_or_default(),
        );
    }

    /// The last recovery, for the Performance page.
    pub(crate) fn last_recovered(&self) -> Value {
        let since = now_ms().saturating_sub(24 * 3_600_000);
        self.db
            .log_query(None, Some("station.recovered"), since, 1)
            .into_iter()
            .next()
            .map(|e| {
                let mut d = e["data"].clone();
                d["at_ms"] = e["ts"].clone();
                d
            })
            .unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;

    /// DECISIONS.md, "After a restart": three restarts in ten minutes hold
    /// sessions back; a clean stop starts the count again.
    #[test]
    fn three_unclean_starts_in_ten_minutes_hold_sessions_back() {
        let t0 = 1_800_000_000_000;
        let (s1, hold1) = count_start(&[], false, t0);
        let (s2, hold2) = count_start(&s1, false, t0 + MIN);
        let (s3, hold3) = count_start(&s2, false, t0 + 2 * MIN);
        assert_eq!((hold1, hold2, hold3), (false, false, true));
        // Still held at the fourth, while three lie within ten minutes.
        let (s4, hold4) = count_start(&s3, false, t0 + 3 * MIN);
        assert!(hold4);
        // A start that followed a clean stop is not counted and resets it.
        let (after_stop, hold) = count_start(&s4, true, t0 + 4 * MIN);
        assert!(!hold && after_stop.is_empty());
        let (_, hold) = count_start(&after_stop, false, t0 + 5 * MIN);
        assert!(!hold);
        // Starts more than ten minutes back no longer count.
        let (s, hold) = count_start(&s2, false, t0 + 12 * MIN);
        assert!(!hold);
        assert_eq!(s, vec![t0 + 12 * MIN]);
        // A clock that went backwards leaves no start in the future behind.
        let (s, hold) = count_start(&[t0 + 50 * MIN, t0 + 51 * MIN], false, t0);
        assert!(!hold);
        assert_eq!(s, vec![t0]);
        // The list is bounded however often the station restarts.
        let mut s = vec![];
        for i in 0..40 {
            s = count_start(&s, false, t0 + i * 1000).0;
        }
        assert!(s.len() <= 8);
    }

    /// DECISIONS.md, "After a restart": at most one recovery notice in ten
    /// minutes; a later recovery rewrites it and says how many.
    #[test]
    fn one_recovery_notice_in_ten_minutes() {
        let t0 = 1_800_000_000_000 + 14 * 3_600_000 + 2 * MIN;
        let start = |at| StartInfo {
            at_ms: at,
            clean: false,
            hold: false,
            starts: 1,
            last_alive_ms: Some(at - 12_000),
            handoff: false,
        };
        let found = Recovery {
            panes: 5,
            resumed: 4,
            closed_shells: 1,
            tasks_failed: 1,
            ..Default::default()
        };
        let mut first = plan_notice(None, t0);
        assert_eq!((first.id.as_str(), first.count), ("", 1));
        let (title, body) = notice_text("spark2", &start(t0), &found, &first);
        let at = clock_utc(t0);
        assert_eq!(title, format!("spark2 restarted at {at} (away 12 s)"));
        assert_eq!(
            body,
            "4 sessions were started again, 1 shell ended, 1 task failed. They wait for you. \
             One that was not started can be brought back from its pane."
        );
        // With the sessions held for the owner (the default): the notice
        // says so and how they are released, in words an older Control
        // Deck with no Release button can show too.
        let held = Recovery {
            mail_held: 4,
            ..found.clone()
        };
        let (_, body) = notice_text("spark2", &start(t0), &held, &first);
        assert_eq!(
            body,
            "4 sessions were started again, 1 shell ended, 1 task failed. They wait for you, \
             and take no mail from other agents until you release them: type into one, or \
             release them all (Release all on this station, or `spandeck pane release --all`). \
             One that was not started can be brought back from its pane."
        );
        first.id = "n-1".into();

        // Twice more inside ten minutes: the same notice, rewritten.
        let second = plan_notice(Some(&first), t0 + 3 * MIN);
        let third = plan_notice(Some(&second), t0 + 9 * MIN);
        assert_eq!(
            (third.id.as_str(), third.first_ms, third.count),
            ("n-1", t0, 3)
        );
        let (title, _) = notice_text("spark2", &start(t0 + 9 * MIN), &found, &third);
        assert_eq!(title, format!("spark2 restarted 3 times since {at}"));

        // Ten minutes after the first it spoke of: a new notice.
        let later = plan_notice(Some(&third), t0 + 10 * MIN);
        assert_eq!(
            (later.id.as_str(), later.first_ms, later.count),
            ("", t0 + 10 * MIN, 1)
        );
        // A standing notice from the future (the clock was set back) is not rewritten.
        assert!(plan_notice(Some(&first), t0 - MIN).id.is_empty());

        // Held back, and a job left running by a shell that was not started again.
        let mut held = Recovery {
            panes: 3,
            held: 2,
            closed_shells: 1,
            ..Default::default()
        };
        held.note_left(&[sys::Proc {
            pid: 4242,
            sid: 4000,
            age_secs: 90,
            name: "sleep\u{1b}[31m".into(),
        }]);
        let (_, body) = notice_text("spark2", &start(t0), &held, &plan_notice(None, t0));
        assert!(body.starts_with(
            "2 sessions were not started again, because spandeck restarted 3 times in 10 minutes, 1 shell ended."
        ));
        assert!(
            body.ends_with("and not touched: sleep[31m (pid 4242)."),
            "{body}"
        );
    }

    /// Design 9.2 (6): only what the pane's own program left is ended, and
    /// a pid that was given to something else since is never touched.
    #[test]
    fn leftovers_are_only_the_panes_own_session() {
        let now = 1_800_000_000_000u64;
        let started = now - 600_000; // the pane's program, ten minutes ago
        let alive = now - 120_000; // the daemon was last running two minutes ago
        let p = |pid, sid, age_secs, name: &str| sys::Proc {
            pid,
            sid,
            age_secs,
            name: name.into(),
        };
        let pids = |v: Vec<sys::Proc>| v.iter().map(|p| p.pid).collect::<Vec<_>>();

        // The program is gone; a job it started that ignored the hang-up is not.
        let procs = vec![p(700, 500, 300, "sleep"), p(900, 900, 300, "bash")];
        assert_eq!(
            pids(leftovers(
                &procs,
                Some(500),
                Some(started),
                Some(alive),
                now
            )),
            vec![700]
        );
        // The program itself survived: it goes with its jobs.
        let procs = vec![p(500, 500, 600, "agent"), p(700, 500, 300, "sleep")];
        assert_eq!(
            pids(leftovers(
                &procs,
                Some(500),
                Some(started),
                Some(alive),
                now
            )),
            vec![500, 700]
        );
        // The number now belongs to a process started at another time: nothing.
        let procs = vec![p(500, 500, 60, "other"), p(700, 500, 30, "child")];
        assert!(leftovers(&procs, Some(500), Some(started), Some(alive), now).is_empty());
        // Or to one that leads no session of that number.
        let procs = vec![p(500, 77, 600, "other"), p(700, 500, 300, "sleep")];
        assert!(leftovers(&procs, Some(500), Some(started), Some(alive), now).is_empty());
        // A session under that number that began after the daemon was last
        // running, or before the pane's program started, is not the pane's.
        let procs = vec![p(700, 500, 2, "new"), p(701, 500, 5000, "old")];
        assert!(leftovers(&procs, Some(500), Some(started), Some(alive), now).is_empty());
        // A record from before start times were kept, or with no "last
        // running" to go by: nothing is touched.
        let procs = vec![p(700, 500, 300, "sleep")];
        assert!(leftovers(&procs, Some(500), None, Some(alive), now).is_empty());
        assert!(leftovers(&procs, Some(500), Some(started), None, now).is_empty());
        assert!(leftovers(&procs, None, Some(started), Some(alive), now).is_empty());
        assert!(leftovers(&procs, Some(1), Some(started), Some(alive), now).is_empty());
    }
}
