//! While spandeckd is away: what an agent's hook does (DECISIONS.md,
//! "While spandeck is away").
//!
//! A hook used to pass in silence whenever the daemon did not answer
//! (measured: 596 of 596 during a minute down, each in 3 ms), so an
//! enforced rule was simply not applied. Now a hook that can block waits
//! for the daemon, and when it stays away:
//!
//! - a tool call (`PreToolUse`) is refused where a rule is enforced for
//!   that pane and its agent can be blocked, and allowed otherwise;
//! - a permission request (`PermissionRequest`) says nothing, so the
//!   agent's own prompt takes over, as it does when nobody answers.
//!
//! The hook cannot ask a daemon that is away whether a rule is enforced,
//! so the daemon writes it down while it runs: a small file per pane, the
//! pane's guard. It is rewritten when what it says changes, and touched
//! every ten seconds, so its age also says for how long the daemon has
//! been away.
//!
//! The agent cancels a hook that reaches its timeout and then runs the
//! tool (Claude Code's and Codex's documentation both say so). So the
//! hook is told its timeout (`SPANDECK_HOOK_DEADLINE`) and answers two
//! seconds before it. A session started before 16 Nov 2026 gives a
//! tool-call hook 10 seconds and tells it nothing: such a hook decides by
//! 8 seconds.

use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spandeck_agents::{adapter, AgentKind, HookDecision, HookReply};

use crate::client::{Client, HOOK_WAIT};
use crate::daemon::{Daemon, PaneEntry, PaneMeta};
use crate::orders::OrderKind;
use crate::paths::Paths;

/// The switch (`[flags] hooks_when_down = "allow"`, or `station.flags`):
/// `"deny_enforced"` (the default) or `"allow"`, which is how it was
/// before: a hook that gets no answer says nothing.
pub const SWITCH: &str = "hooks_when_down";
pub const DENY_ENFORCED: &str = "deny_enforced";
pub const ALLOW: &str = "allow";

/// How long a hook that can block waits for a daemon that is away.
pub const HOLD: Duration = Duration::from_secs(60);
/// A hook answers this long before its agent would cancel it.
pub const MARGIN: Duration = Duration::from_secs(2);
/// What a tool-call hook is given by a session started before it was told
/// (entries of version 1), and a permission hook by any session.
pub const OLD_TOOL_SECS: u64 = 10;
pub const PERMISSION_SECS: u64 = 330;

/// What the agent is told when a tool call is refused because the daemon
/// is away. It must not invite another try: each one is paid for.
pub const DOWN_TEXT: &str = "Blocked by spandeck: spandeck is not running on this station, so the rules enforced for this session cannot be checked. Do not retry this and do not try another way: stop and wait for the owner.";

/// A pane's guard, as the daemon writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guard {
    /// A rule is enforced for this pane, and its agent's integration can
    /// block: the same answer the Control Deck shows (orders.rs).
    pub enforced: bool,
    /// The switch, as it stood.
    #[serde(default)]
    pub when_down: String,
    /// Which rules, by id: for whoever reads the file.
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub agent: String,
}

/// What a hook knows of its pane when the daemon does not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Known {
    /// The guard says a rule is enforced.
    Enforced,
    /// No guard, or one that cannot be read: taken as enforced.
    Unknown,
    /// The guard says nothing is enforced. `away`: how long ago the daemon
    /// last touched it.
    NotEnforced { away: Duration },
    /// The switch says a hook with no answer says nothing.
    Allow,
}

/// What kind of hook this is, for an agent that can be blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Before a tool call: where a rule is enforced.
    Tool,
    /// The agent asks permission: answered from the Control Deck.
    Permission,
    /// Everything else, and every hook of an agent that cannot be blocked.
    Other,
}

pub fn kind_of(agent: AgentKind, event: &str) -> Kind {
    let Some(caps) = adapter(agent).map(|a| a.capabilities()) else {
        return Kind::Other;
    };
    match event {
        "PreToolUse" if caps.block_tools => Kind::Tool,
        "PermissionRequest" if caps.remote_approval => Kind::Permission,
        _ => Kind::Other,
    }
}

/// How long a hook goes on, in two limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// How long it tries to reach a daemon that is away, each time it
    /// finds it away.
    pub reach: Duration,
    /// When it must have answered, counted from its start. None: it has
    /// no limit of its own (a hook that cannot block).
    pub decide_by: Option<Duration>,
}

/// `deadline`: the agent's timeout for this hook in seconds, when the
/// hook was told it.
pub fn plan(kind: Kind, deadline: Option<u64>, known: Known) -> Plan {
    let by = |default: u64| {
        Duration::from_secs(deadline.unwrap_or(default).clamp(3, 3600)).saturating_sub(MARGIN)
    };
    match kind {
        // As before: three seconds for a daemon that is starting.
        Kind::Other => Plan {
            reach: HOOK_WAIT,
            decide_by: None,
        },
        Kind::Permission => {
            let by = by(PERMISSION_SECS);
            Plan {
                reach: HOLD.min(by),
                decide_by: Some(by),
            }
        }
        Kind::Tool => {
            let by = by(OLD_TOOL_SECS);
            let reach = match known {
                // Held every time: a call that was refused and is made
                // again waits again, so a loop of tries is a slow one.
                Known::Enforced | Known::Unknown => HOLD,
                // Held until the daemon has been away for a minute in
                // all, not a minute for every call.
                Known::NotEnforced { away } => HOLD.saturating_sub(away).max(HOOK_WAIT),
                Known::Allow => HOOK_WAIT,
            };
            Plan {
                reach: reach.min(by),
                decide_by: Some(by),
            }
        }
    }
}

/// What a hook says when its time ended with no answer from the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing: the tool call goes ahead, or the agent's own prompt
    /// takes over.
    Silent,
    /// Refuse the tool call, with `DOWN_TEXT`.
    Deny,
}

pub fn unanswered(kind: Kind, known: Known) -> Verdict {
    match (kind, known) {
        (Kind::Tool, Known::Enforced | Known::Unknown) => Verdict::Deny,
        _ => Verdict::Silent,
    }
}

/// Read a pane's guard. `now`: for the guard's age.
pub fn read(paths: &Paths, pane: &str, now: SystemTime) -> Known {
    let path = paths.guard_file(pane);
    let guard = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Guard>(&text).ok());
    let Some(guard) = guard else {
        return Known::Unknown;
    };
    if guard.when_down == ALLOW {
        return Known::Allow;
    }
    if guard.enforced {
        return Known::Enforced;
    }
    let away = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| now.duration_since(at).ok())
        .unwrap_or_default();
    Known::NotEnforced { away }
}

/// What a hook with nothing to say prints. Antigravity expects a JSON
/// object from every hook; the others expect silence.
fn nothing(agent: &str) -> HookReply {
    HookReply {
        stdout: if matches!(agent, "antigravity" | "agy") {
            "{}".into()
        } else {
            String::new()
        },
        exit_code: 0,
    }
}

/// How a hook's question to the daemon ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The daemon answered: the reply is its own.
    Answered,
    /// It did not (why): the reply is what this file decided.
    Unanswered(String),
}

/// `spandeck hook <agent>`: one hook payload to the daemon, and what to
/// print and exit with. `deadline`: the agent's timeout for this hook, in
/// seconds, when the hook was told it. It never breaks the agent: a
/// failure is a silent pass, except the one case this file is for.
pub fn run(paths: &Paths, agent: &str, deadline: Option<u64>, input: &str) -> HookReply {
    run_told(paths, agent, deadline, input).0
}

/// `run`, also saying whether the daemon answered (the stand-in agent of
/// the tests shows it).
pub fn run_told(
    paths: &Paths,
    agent: &str,
    deadline: Option<u64>,
    input: &str,
) -> (HookReply, Outcome) {
    let silent = |why: &str| (nothing(agent), Outcome::Unanswered(why.to_string()));
    // Outside a spandeck pane this is a no-op, so a global install is harmless.
    let Some(pane) = std::env::var("SPANDECK_PANE")
        .ok()
        .filter(|p| !p.is_empty())
    else {
        return silent("not in a spandeck pane");
    };
    if std::env::var_os("SPANDECK_TOKEN").is_none() {
        return silent("not in a spandeck pane");
    }
    let Ok(payload) = serde_json::from_str::<Value>(input) else {
        return silent("the hook's input is not JSON");
    };
    let event = payload
        .get("hook_event_name")
        .or_else(|| payload.get("hookEventName"))
        .and_then(|e| e.as_str())
        .unwrap_or("")
        .to_string();
    let kind = AgentKind::parse(agent).map_or(Kind::Other, |a| kind_of(a, &event));
    let known = match kind {
        Kind::Tool => read(paths, &pane, SystemTime::now()),
        _ => Known::Unknown,
    };
    let plan = plan(kind, deadline, known);
    let began = Instant::now();
    // Made once, and sent with every try: what this hook asks the owner
    // is found again by it (approvals.rs).
    let hook_id = crate::rids::new_rid();
    let params = json!({"agent": agent, "payload": payload, "hook_id": hook_id});
    let answered = |reply: Value| {
        let exit_code = reply["exit_code"].as_i64().unwrap_or(0) as i32;
        let reply = match reply["stdout"].as_str().filter(|s| !s.is_empty()) {
            Some(out) => HookReply {
                stdout: out.to_string(),
                exit_code,
            },
            None => HookReply {
                exit_code: exit_code.max(0),
                ..nothing(agent)
            },
        };
        (reply, Outcome::Answered)
    };
    let Some(decide_by) = plan.decide_by else {
        // A hook that cannot block: one try, as it always was.
        let mut client = match Client::from_env_waiting(paths, plan.reach) {
            Ok(client) => client,
            Err(e) => return silent(&e.message),
        };
        client.set_timeout(Some(Duration::from_secs(400)));
        return match client.call("hook.ingest", params) {
            Ok(reply) => answered(reply),
            Err(e) => silent(&e.message),
        };
    };
    // When the daemon was last found away: its time to come back counts
    // from then.
    let mut away_since = began;
    let mut why = String::from("spandeckd did not answer in time");
    loop {
        let left = decide_by.saturating_sub(began.elapsed());
        if left.is_zero() {
            break;
        }
        let reach = plan.reach.saturating_sub(away_since.elapsed()).min(left);
        let mut client = match Client::from_env_waiting(paths, reach) {
            Ok(client) => client,
            Err(e) => {
                why = e.message;
                break;
            }
        };
        client.set_timeout(Some(left));
        match client.call("hook.ingest", params.clone()) {
            Ok(reply) => return answered(reply),
            // The connection ended before the answer (the daemon is being
            // upgraded, started again, or was ended), or this hook's own
            // time ran out: asked again, with the same id, while there is
            // time.
            Err(e) if matches!(e.code.as_str(), "closed" | "io") => {
                away_since = Instant::now();
                std::thread::sleep(Duration::from_millis(25).min(left));
            }
            // The daemon answered, with an error (it does not know this
            // pane any more): nothing is said, as before.
            Err(e) => return silent(&e.message),
        }
    }
    let reply = match (
        unanswered(kind, known),
        AgentKind::parse(agent).and_then(adapter),
    ) {
        (Verdict::Deny, Some(a)) => a.reply(&event, &HookDecision::deny(DOWN_TEXT)),
        _ => nothing(agent),
    };
    (reply, Outcome::Unanswered(why))
}

impl Daemon {
    /// Can a rule be enforced on this pane's agent? Only where its
    /// integration blocks a tool call and that was seen with the real
    /// agent (Guarantees, Enforcement). It is what the Control Deck shows
    /// a rule as, and what a hook does when the daemon is away.
    pub(crate) fn can_block(meta: &PaneMeta) -> bool {
        adapter(meta.agent)
            .map(|a| a.capabilities())
            .is_some_and(|c| c.block_tools && c.verified && meta.agent.is_agent())
    }

    fn guard_of(&self, meta: &PaneMeta) -> Guard {
        let (orders, _) = self.orders_for(meta);
        // A rule with no matcher at all is ignored where rules are checked.
        let rules: Vec<String> = orders
            .iter()
            .filter(|o| o.kind == OrderKind::Rule && (o.tool.is_some() || o.pattern.is_some()))
            .map(|o| o.id.clone())
            .collect();
        let allow = self.flag(SWITCH).as_ref().and_then(|v| v.as_str()) == Some(ALLOW);
        Guard {
            enforced: Self::can_block(meta) && !rules.is_empty(),
            when_down: if allow { ALLOW } else { DENY_ENFORCED }.to_string(),
            rules,
            agent: meta.agent.as_str().to_string(),
        }
    }

    /// Write this pane's guard, or touch it when it says the same.
    pub(crate) fn guard_write(&self, p: &PaneEntry) {
        let meta = p.meta();
        let path = self.paths.guard_file(&p.id);
        if meta.exit.is_some() || !meta.agent.is_agent() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        let text = serde_json::to_string(&self.guard_of(&meta)).unwrap_or_default();
        if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
            let touched = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .and_then(|f| f.set_modified(SystemTime::now()));
            if touched.is_ok() {
                return;
            }
        }
        let written = (|| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // Whole or not at all: a hook never reads half a guard.
            let tmp = path.with_extension("guard.new");
            spandeck_terminal::sys::write_private(&tmp, text.as_bytes())?;
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = written {
            tracing::warn!(pane = %p.id, error = %e, "could not write a pane's guard");
        }
    }

    /// Every agent pane's guard as it stands now, and none for a pane
    /// that is gone. At the start, every ten seconds (the orders are
    /// files, read when they are needed: a change is in the guards within
    /// ten seconds), and when the switch is set.
    pub(crate) fn guards_refresh(&self) {
        let panes = self.list_panes();
        for p in &panes {
            self.guard_write(p);
        }
        let Ok(dir) = std::fs::read_dir(self.paths.guard_dir()) else {
            return;
        };
        for entry in dir.flatten() {
            let path = entry.path();
            let known = path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|stem| panes.iter().any(|p| p.id == stem));
            if !known {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    /// The audit's gap 7 and REVIEW2 blocker 8, as a table: what a hook
    /// does with no answer from the daemon, for every kind of hook, both
    /// timeouts, every state of the guard and the switch. Decided by
    /// default on 15 Nov 2026: wait about a minute (inside the old 10
    /// seconds for a session from before), then refuse where a rule is
    /// enforced and the agent can be blocked, allow otherwise. A hook
    /// always answers two seconds before its agent would cancel it, since
    /// a cancelled hook lets the tool run.
    #[test]
    fn what_a_hook_does_when_the_daemon_does_not_answer() {
        let fresh = Known::NotEnforced { away: S(0) };
        let long_away = Known::NotEnforced { away: S(600) };
        // (kind, the timeout it was told, guard) -> (reach, decide by, verdict)
        let table = [
            // A session started after the update: 75 seconds.
            (
                Kind::Tool,
                Some(75),
                Known::Enforced,
                S(60),
                Some(S(73)),
                Verdict::Deny,
            ),
            (
                Kind::Tool,
                Some(75),
                Known::Unknown,
                S(60),
                Some(S(73)),
                Verdict::Deny,
            ),
            (
                Kind::Tool,
                Some(75),
                fresh,
                S(60),
                Some(S(73)),
                Verdict::Silent,
            ),
            // Nothing enforced, and the daemon away for a while already:
            // three seconds, as before, not a minute for every call.
            (
                Kind::Tool,
                Some(75),
                long_away,
                S(3),
                Some(S(73)),
                Verdict::Silent,
            ),
            (
                Kind::Tool,
                Some(75),
                Known::NotEnforced { away: S(20) },
                S(40),
                Some(S(73)),
                Verdict::Silent,
            ),
            (
                Kind::Tool,
                Some(75),
                Known::Allow,
                S(3),
                Some(S(73)),
                Verdict::Silent,
            ),
            // A session from before: 10 seconds, and the hook is not told.
            (
                Kind::Tool,
                None,
                Known::Enforced,
                S(8),
                Some(S(8)),
                Verdict::Deny,
            ),
            (
                Kind::Tool,
                None,
                Known::Unknown,
                S(8),
                Some(S(8)),
                Verdict::Deny,
            ),
            (Kind::Tool, None, fresh, S(8), Some(S(8)), Verdict::Silent),
            (
                Kind::Tool,
                None,
                long_away,
                S(3),
                Some(S(8)),
                Verdict::Silent,
            ),
            (
                Kind::Tool,
                None,
                Known::Allow,
                S(3),
                Some(S(8)),
                Verdict::Silent,
            ),
            // A permission request falls through to the agent's own prompt.
            (
                Kind::Permission,
                Some(330),
                Known::Unknown,
                S(60),
                Some(S(328)),
                Verdict::Silent,
            ),
            (
                Kind::Permission,
                None,
                Known::Enforced,
                S(60),
                Some(S(328)),
                Verdict::Silent,
            ),
            // Any other hook: three seconds for a daemon that is starting.
            (
                Kind::Other,
                None,
                Known::Enforced,
                S(3),
                None,
                Verdict::Silent,
            ),
            (
                Kind::Other,
                Some(75),
                Known::Unknown,
                S(3),
                None,
                Verdict::Silent,
            ),
        ];
        for (kind, deadline, known, reach, decide_by, verdict) in table {
            let what = format!("{kind:?} {deadline:?} {known:?}");
            assert_eq!(
                plan(kind, deadline, known),
                Plan { reach, decide_by },
                "{what}"
            );
            assert_eq!(unanswered(kind, known), verdict, "{what}");
        }
        // A timeout nobody would set still leaves the hook a second.
        assert_eq!(
            plan(Kind::Tool, Some(1), Known::Enforced).decide_by,
            Some(S(1))
        );
    }

    /// Guarantees, Enforcement: "A rule is shown as enforced for an agent
    /// only where that agent's integration can block it." So only such an
    /// agent's tool-call hook is ever held or refused: OpenCode has no
    /// hooks and Antigravity's cannot block. And the refusal is in the
    /// agent's own dialect, with the exact words, which tell it not to
    /// try again.
    #[test]
    fn only_an_agent_that_can_be_blocked_is_held_and_the_refusal_is_in_its_dialect() {
        for agent in [AgentKind::Claude, AgentKind::Codex, AgentKind::Fake] {
            assert_eq!(kind_of(agent, "PreToolUse"), Kind::Tool);
            assert_eq!(kind_of(agent, "PermissionRequest"), Kind::Permission);
            assert_eq!(kind_of(agent, "PostToolUse"), Kind::Other);
            assert_eq!(kind_of(agent, "Stop"), Kind::Other);
        }
        for agent in [
            AgentKind::OpenCode,
            AgentKind::Antigravity,
            AgentKind::Shell,
        ] {
            for event in ["PreToolUse", "PermissionRequest", "Stop", "PreInvocation"] {
                assert_eq!(kind_of(agent, event), Kind::Other, "{agent:?} {event}");
            }
        }
        let reply = adapter(AgentKind::Claude)
            .unwrap()
            .reply("PreToolUse", &HookDecision::deny(DOWN_TEXT));
        assert_eq!(reply.exit_code, 0);
        let out: Value = serde_json::from_str(&reply.stdout).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            out["hookSpecificOutput"]["permissionDecisionReason"],
            "Blocked by spandeck: spandeck is not running on this station, so the rules enforced for this session cannot be checked. Do not retry this and do not try another way: stop and wait for the owner."
        );
    }

    /// The guard as a hook reads it: enforced, not enforced with its age,
    /// the switch, and anything that is not a guard taken as enforced (a
    /// missing or damaged file must not open the gate).
    #[test]
    fn a_guard_that_is_missing_or_unreadable_is_taken_as_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        let now = SystemTime::now() + S(30);
        assert_eq!(read(&paths, "p1", now), Known::Unknown);
        std::fs::create_dir_all(paths.guard_dir()).unwrap();
        let write = |pane: &str, text: &str| std::fs::write(paths.guard_file(pane), text).unwrap();
        write("p1", "{ not json");
        assert_eq!(read(&paths, "p1", now), Known::Unknown);
        write(
            "p1",
            r#"{"enforced":true,"when_down":"deny_enforced","rules":["no-force-push"],"agent":"claude"}"#,
        );
        assert_eq!(read(&paths, "p1", now), Known::Enforced);
        write("p1", r#"{"enforced":true,"when_down":"allow"}"#);
        assert_eq!(read(&paths, "p1", now), Known::Allow);
        write("p1", r#"{"enforced":false,"when_down":"deny_enforced"}"#);
        match read(&paths, "p1", now) {
            Known::NotEnforced { away } => assert!(away >= S(29) && away < S(90), "{away:?}"),
            other => panic!("{other:?}"),
        }
        // A switch value nobody knows is the default.
        write("p1", r#"{"enforced":true,"when_down":"whatever"}"#);
        assert_eq!(read(&paths, "p1", now), Known::Enforced);
        // A pane's name is never a path: what is not a letter, a digit,
        // `-` or `_` is dropped.
        assert_eq!(
            paths.guard_file("../../etc/p1"),
            paths.guard_dir().join("etcp1.guard")
        );
    }
}
