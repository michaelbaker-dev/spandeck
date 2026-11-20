//! An agent's own sub-agents, as rows of its pane's `live.tasks`
//! (DECISIONS.md, "Sub-agents are shown in a drawer of the agent's pane,
//! for every program"; `docs/conversation-api.md`, `tasks`).
//!
//! What is here: what the hooks say of one (its start and its stop, told
//! apart by the program's own id where its payload has one); what Claude
//! Code writes beside a session for each (`<session>/subagents/agent-<id>
//! .meta.json`, which names the call that began it before any result
//! does, and the sub-agent it was begun under); and the few words a row
//! says of what one is doing now, from the end of its own record.
//!
//! All of it only reads, and reads little: a folder is listed and a
//! store asked at most every two seconds, the end of a record is read
//! again only when the record has grown and at most once a second, and
//! only the first rows are given a "now". Text from a sub-agent is data
//! (rule 6): it leaves here as strings of a JSON answer, cut to a line.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use spandeck_agents::AgentEvent;
use spandeck_archive::conversation::{latest_step, ts_ms, Dialect};
use spandeck_archive::stores;

use crate::daemon::SubAgent;

/// How many sub-agents the hooks may have running for one pane, and how
/// many they said were over are kept.
const HOOKED_MAX: usize = 24;
/// How much of the end of a sub-agent's record is read for its "now".
const STEP_BYTES: u64 = 256 * 1024;
/// How many `meta.json` files of one session are read: the newest.
const SIDES_MAX: usize = 300;

/// A sub-agent the hooks said was over.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubOver {
    /// The program's own id for it.
    pub id: String,
    pub kind: String,
    /// When its start was reported; 0 when it was not.
    pub since_ms: u64,
    pub ended_ms: u64,
    /// Where the program said its record is. A claim: it is read only
    /// where `over_file` finds it among the program's own files.
    #[serde(default)]
    pub transcript: Option<String>,
    /// The session it was in.
    #[serde(default)]
    pub session: Option<String>,
}

fn kind_of(ev: &AgentEvent) -> String {
    ev.message.clone().unwrap_or_else(|| "sub-agent".into())
}

/// `SubagentStart`: one more runs. (The same id twice is one.)
pub(crate) fn started(running: &mut Vec<SubAgent>, ev: &AgentEvent, now: u64) {
    let again = ev.sub_id.is_some() && running.iter().any(|s| s.id == ev.sub_id);
    if !again && running.len() < HOOKED_MAX {
        running.push(SubAgent {
            kind: kind_of(ev),
            since_ms: now,
            id: ev.sub_id.clone(),
        });
    }
}

/// `SubagentStop`: the one it names is over. By its id where the payload
/// has one; else the first of its kind, as before ids were kept; else the
/// newest one that has no id. One with another id is never taken for it.
pub(crate) fn stopped(
    running: &mut Vec<SubAgent>,
    over: &mut Vec<SubOver>,
    ev: &AgentEvent,
    session: Option<String>,
    now: u64,
) {
    let kind = kind_of(ev);
    let at = ev
        .sub_id
        .as_ref()
        .and_then(|id| running.iter().position(|s| s.id.as_ref() == Some(id)))
        .or_else(|| {
            running
                .iter()
                .position(|s| s.kind == kind && (s.id.is_none() || ev.sub_id.is_none()))
        })
        .or_else(|| running.iter().rposition(|s| s.id.is_none()))
        .or_else(|| ev.sub_id.is_none().then(|| running.len().checked_sub(1))?);
    let was = at.map(|i| running.remove(i));
    if let Some(id) = &ev.sub_id {
        over.retain(|o| o.id != *id);
        over.push(SubOver {
            id: id.clone(),
            kind,
            since_ms: was.map_or(0, |s| s.since_ms),
            ended_ms: now,
            transcript: ev.sub_transcript.clone(),
            session,
        });
        if over.len() > HOOKED_MAX {
            let gone = over.len() - HOOKED_MAX;
            over.drain(..gone);
        }
    }
}

/// The record a hook named for a sub-agent that is over, when it is where
/// the pane's program keeps such records and nowhere else: under `root`
/// (beside the session's file for Claude Code, Codex's sessions folder),
/// a `.jsonl` file, with no step back up.
pub(crate) fn over_file(claimed: Option<&str>, root: Option<&Path>) -> Option<PathBuf> {
    let (file, root) = (Path::new(claimed?), root?);
    let inside = file.is_absolute()
        && file.starts_with(root)
        && !file
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        && file.extension().is_some_and(|e| e == "jsonl");
    (inside && file.is_file()).then(|| file.to_path_buf())
}

/// The four words a view shows for how a sub-agent is: from the status a
/// row has (the program's own word for one left in the background, a
/// result's for one waited for, the station's `lost`).
pub(crate) fn state_of(status: &str) -> &'static str {
    match status {
        "running" => "working",
        "error" | "failed" => "failed",
        "interrupted" | "denied" | "lost" | "stopped" | "killed" | "cancelled" | "canceled"
        | "aborted" => "stopped",
        _ => "done",
    }
}

fn modified_ms(path: &Path) -> u64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

/// What Claude Code wrote beside a session for one sub-agent
/// (`agent-<id>.meta.json`; fields as 2.1.329 writes them: `agentType`,
/// `description`, `toolUseId`, `spawnDepth`, `parentAgentId`, `finished`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Side {
    /// The program's id: the `<id>` of the file's name, a plain word.
    pub id: String,
    pub kind: String,
    pub title: String,
    /// The id of the call that began it: in the session's file for one
    /// the agent began, in the parent sub-agent's for one of its own.
    pub tool_use_id: String,
    /// 1 for the agent's own, 2 for one a sub-agent began, and so on.
    pub depth: u32,
    pub parent: Option<String>,
    /// The program wrote that it is over (the field is absent while it
    /// runs).
    pub finished: bool,
    pub modified_ms: u64,
}

fn side_of(path: &Path, id: &str) -> Option<Side> {
    if path.metadata().ok()?.len() > 64 * 1024 {
        return None;
    }
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let word = |key: &str, max: usize| {
        let w = v[key].as_str().unwrap_or("");
        spandeck_archive::conversation::summary_line(w)
            .chars()
            .take(max)
            .collect::<String>()
    };
    Some(Side {
        id: id.to_string(),
        kind: word("agentType", 80),
        title: word("description", 200),
        tool_use_id: v["toolUseId"].as_str().unwrap_or("").to_string(),
        depth: v["spawnDepth"].as_u64().unwrap_or(1).clamp(1, 9) as u32,
        parent: v["parentAgentId"]
            .as_str()
            .filter(|p| spandeck_agents::claude::plain_word(p))
            .map(String::from),
        finished: v["finished"] == Value::Bool(true),
        modified_ms: modified_ms(path),
    })
}

/// The sub-agents Claude Code has written a `meta.json` for beside the
/// session whose file is `<stem>.jsonl`, the newest `SIDES_MAX`. The
/// folder is listed at most every two seconds, and a file read again only
/// when it was written to since.
pub(crate) fn claude_sides(stem: &str) -> Vec<Side> {
    type Seen = HashMap<String, (Instant, Vec<Side>)>;
    static SEEN: OnceLock<Mutex<Seen>> = OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    let had = seen.lock().unwrap().get(stem).cloned();
    if let Some((at, sides)) = &had {
        if at.elapsed() < Duration::from_secs(2) {
            return sides.clone();
        }
    }
    let dir = Path::new(stem).join("subagents");
    let mut files: Vec<(u64, String, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id = name.strip_prefix("agent-")?.strip_suffix(".meta.json")?;
            spandeck_agents::claude::plain_word(id).then(|| {
                let path = e.path();
                (modified_ms(&path), id.to_string(), path)
            })
        })
        .collect();
    files.sort();
    if files.len() > SIDES_MAX {
        files.drain(..files.len() - SIDES_MAX);
    }
    let old = had.map(|(_, sides)| sides).unwrap_or_default();
    let sides: Vec<Side> = files
        .into_iter()
        .filter_map(|(modified, id, path)| {
            match old.iter().find(|s| s.id == id && s.modified_ms == modified) {
                Some(same) => Some(same.clone()),
                None => side_of(&path, &id),
            }
        })
        .collect();
    let mut seen = seen.lock().unwrap();
    if seen.len() > 64 {
        seen.clear();
    }
    seen.insert(stem.to_string(), (Instant::now(), sides.clone()));
    sides
}

/// When a record's first line says it was written (`timestamp`): when a
/// sub-agent of Claude Code's was begun. Read once for a file.
pub(crate) fn first_ms(path: &Path) -> Option<u64> {
    type Firsts = HashMap<PathBuf, u64>;
    static FIRSTS: OnceLock<Mutex<Firsts>> = OnceLock::new();
    let firsts = FIRSTS.get_or_init(Default::default);
    if let Some(known) = firsts.lock().unwrap().get(path) {
        return Some(*known);
    }
    use std::io::{BufRead, Read};
    let mut line = Vec::new();
    std::io::BufReader::new(std::fs::File::open(path).ok()?.take(4 << 20))
        .read_until(b'\n', &mut line)
        .ok()?;
    if line.last() != Some(&b'\n') {
        return None;
    }
    let v: Value = serde_json::from_slice(&line).ok()?;
    let at = v["timestamp"].as_str().and_then(ts_ms)?;
    let mut firsts = firsts.lock().unwrap();
    if firsts.len() > 2000 {
        firsts.clear();
    }
    firsts.insert(path.to_path_buf(), at);
    Some(at)
}

/// What a sub-agent is doing now and what it last said, each in a line,
/// from the end of its own record (`latest_step`). The end is read again
/// only when the file has grown, and at most once a second.
pub(crate) fn step_of(path: &Path, dialect: Dialect) -> (Option<String>, Option<String>) {
    type Step = (Option<String>, Option<String>);
    type Steps = HashMap<PathBuf, (u64, Instant, Step)>;
    static STEPS: OnceLock<Mutex<Steps>> = OnceLock::new();
    let steps = STEPS.get_or_init(Default::default);
    let len = path.metadata().map(|m| m.len()).unwrap_or(0);
    if let Some((was, at, step)) = steps.lock().unwrap().get(path) {
        if *was == len || at.elapsed() < Duration::from_secs(1) {
            return step.clone();
        }
    }
    let lines = stores::tail_lines(path, STEP_BYTES);
    let mut step = latest_step(dialect, lines.iter().map(|l| l.as_str()));
    let mut steps = steps.lock().unwrap();
    // (An end that is one long line still being written says nothing new:
    // what was known stays.)
    if let Some((_, _, had)) = steps.get(path) {
        step.0 = step.0.or_else(|| had.0.clone());
        step.1 = step.1.or_else(|| had.1.clone());
    }
    if steps.len() > 512 {
        steps.clear();
    }
    steps.insert(path.to_path_buf(), (len, Instant::now(), step.clone()));
    step
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use spandeck_agents::{Adapter, AgentKind};

    fn event(agent: AgentKind, payload: Value) -> Option<AgentEvent> {
        match agent {
            AgentKind::Codex => spandeck_agents::codex::Codex.normalize(&payload),
            _ => spandeck_agents::claude::ClaudeCode.normalize(&payload),
        }
    }

    fn hook(name: &str, kind: Option<&str>, id: Option<&str>) -> Value {
        let mut v = json!({"hook_event_name": name, "session_id": "s"});
        if let Some(kind) = kind {
            v["agent_type"] = json!(kind);
        }
        if let Some(id) = id {
            v["agent_id"] = json!(id);
            if name == "SubagentStop" {
                v["agent_transcript_path"] = json!(format!("/h/s/subagents/agent-{id}.jsonl"));
            }
        }
        v
    }

    struct Pane {
        running: Vec<SubAgent>,
        over: Vec<SubOver>,
        clock: u64,
    }

    impl Pane {
        fn new() -> Pane {
            Pane {
                running: vec![],
                over: vec![],
                clock: 1000,
            }
        }
        /// What the hook pipeline does with a payload (hooks.rs).
        fn takes(&mut self, agent: AgentKind, payload: Value) {
            self.clock += 10;
            let Some(ev) = event(agent, payload) else {
                return;
            };
            match ev.kind {
                spandeck_agents::EventKind::SubagentStart => {
                    started(&mut self.running, &ev, self.clock)
                }
                spandeck_agents::EventKind::SubagentStop => stopped(
                    &mut self.running,
                    &mut self.over,
                    &ev,
                    Some("s".into()),
                    self.clock,
                ),
                _ => {}
            }
        }
        fn ids(&self) -> Vec<&str> {
            self.running
                .iter()
                .map(|s| s.id.as_deref().unwrap_or("-"))
                .collect()
        }
    }

    /// The owner's request of 20 Nov 2026: each sub-agent is a row. Two
    /// of one kind ran, and the stop of the second took the first off the
    /// list (a stop removed "the first of its kind"): the row that stayed
    /// was the one that had ended. Payloads in the shapes Claude Code
    /// 2.1.329 and codex-cli 0.170.0 send.
    #[test]
    fn a_sub_agents_stop_ends_that_one_and_no_other() {
        let mut p = Pane::new();
        let c = AgentKind::Claude;
        p.takes(c, hook("SubagentStart", Some("Explore"), Some("a1")));
        p.takes(c, hook("SubagentStart", Some("Explore"), Some("a2")));
        p.takes(c, hook("SubagentStart", Some("review"), Some("a3")));
        // (A start told twice is one.)
        p.takes(c, hook("SubagentStart", Some("Explore"), Some("a2")));
        assert_eq!(p.ids(), vec!["a1", "a2", "a3"]);
        p.takes(c, hook("SubagentStop", Some("Explore"), Some("a2")));
        assert_eq!(p.ids(), vec!["a1", "a3"]);
        assert_eq!(
            (
                p.over[0].id.as_str(),
                p.over[0].kind.as_str(),
                p.over[0].since_ms,
                p.over[0].ended_ms,
                p.over[0].transcript.as_deref(),
                p.over[0].session.as_deref()
            ),
            (
                "a2",
                "Explore",
                1020,
                1050,
                Some("/h/s/subagents/agent-a2.jsonl"),
                Some("s")
            )
        );
        // One the list does not have (it began before the pane's turn was
        // cleared) ends nothing else, and is still known to be over.
        p.takes(c, hook("SubagentStop", Some("Explore"), Some("a9")));
        assert_eq!(p.ids(), vec!["a1", "a3"]);
        assert_eq!(p.over.len(), 2);
        // Codex's, with its thread's id.
        let mut p = Pane::new();
        let x = AgentKind::Codex;
        let id = "019ab3c4-0000-7000-8000-00000000c0de";
        p.takes(x, hook("SubagentStart", Some("worker"), Some(id)));
        assert_eq!(p.ids(), vec![id]);
        p.takes(x, hook("SubagentStop", Some("worker"), Some(id)));
        assert!(p.running.is_empty() && p.over[0].id == id);
    }

    /// A program whose payloads carry no id (Claude Code before it sent
    /// one; the stand-in agent of the tests) is counted as it was: a stop
    /// ends the first of its kind, else the newest. And the program's own
    /// helpers, which it reports with an empty kind, are never listed.
    #[test]
    fn a_stop_with_no_id_ends_one_of_its_kind_and_an_internal_one_is_not_listed() {
        let mut p = Pane::new();
        let c = AgentKind::Claude;
        p.takes(c, hook("SubagentStart", Some("Explore"), None));
        p.takes(c, hook("SubagentStart", Some("review"), None));
        p.takes(c, hook("SubagentStart", Some("Explore"), None));
        p.takes(c, hook("SubagentStop", Some("review"), None));
        assert_eq!(
            p.running
                .iter()
                .map(|s| s.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["Explore", "Explore"]
        );
        p.takes(c, hook("SubagentStop", Some("other"), None));
        p.takes(c, hook("SubagentStop", None, None));
        assert!(p.running.is_empty() && p.over.is_empty());
        // A start with an id, and a stop without one.
        p.takes(c, hook("SubagentStart", Some("Explore"), Some("a1")));
        p.takes(c, hook("SubagentStop", Some("Explore"), None));
        assert!(p.running.is_empty());
        // The program's own helper.
        p.takes(c, hook("SubagentStart", Some(""), Some("a7")));
        assert!(p.running.is_empty());
        p.takes(c, hook("SubagentStart", Some("Explore"), Some("a1")));
        p.takes(c, hook("SubagentStop", Some(""), Some("a7")));
        assert_eq!(p.ids(), vec!["a1"]);
        assert!(p.over.is_empty());
        // Never more than the list holds.
        for n in 0..40 {
            p.takes(
                c,
                hook("SubagentStart", Some("Explore"), Some(&format!("b{n}"))),
            );
        }
        assert_eq!(p.running.len(), HOOKED_MAX);
    }

    /// A hook's word for where a record is names a file to read: only one
    /// among the program's own records is.
    #[test]
    fn a_record_a_hook_names_is_read_only_among_the_programs_own() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        std::fs::create_dir_all(root.join("2026/11/20")).unwrap();
        let file = root.join("2026/11/20/rollout-x-c0de.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        std::fs::write(dir.path().join("other.jsonl"), "{}\n").unwrap();
        std::fs::write(root.join("notes.txt"), "x").unwrap();
        let ok = |claimed: &Path| over_file(claimed.to_str(), Some(&root));
        assert_eq!(ok(&file), Some(file.clone()));
        assert_eq!(ok(&dir.path().join("other.jsonl")), None);
        assert_eq!(ok(&root.join("2026/../../other.jsonl")), None);
        assert_eq!(ok(&root.join("notes.txt")), None);
        assert_eq!(ok(&root.join("2026/11/20/absent.jsonl")), None);
        assert_eq!(over_file(None, Some(&root)), None);
        assert_eq!(over_file(file.to_str(), None), None);
    }

    /// Claude Code 2.1.329 writes `agent-<id>.meta.json` when a sub-agent
    /// begins, and `finished` into it when it is over; one a sub-agent
    /// began names its parent. Fields as the program writes them; made-up
    /// words. (What it was asked is one line here: its first.)
    #[test]
    fn what_claude_code_writes_beside_a_session_for_each_sub_agent_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let stem = dir.path().join("0c1d");
        let subs = stem.join("subagents");
        std::fs::create_dir_all(&subs).unwrap();
        let meta = |id: &str, v: Value| {
            std::fs::write(subs.join(format!("agent-{id}.meta.json")), v.to_string()).unwrap()
        };
        meta(
            "a1b2c3d4e5f6a7b8c",
            json!({"agentType": "general-purpose", "isBuiltIn": true, "launchedWithDefaults": true,
                "description": "Count the nests", "toolUseId": "toolu_01", "spawnDepth": 1,
                "requestShape": "x", "requestNonInteractive": false, "pickedEffortModel": "x"}),
        );
        meta(
            "b1b2c3d4e5f6a7b8c",
            json!({"agentType": "Explore", "description": "Map the\nbank", "toolUseId": "toolu_02",
                "spawnDepth": 2, "parentAgentId": "a1b2c3d4e5f6a7b8c", "finished": true,
                "worktreePath": "/w/x", "worktreeBranch": "b"}),
        );
        std::fs::write(subs.join("agent-..meta.json"), "{}").unwrap();
        std::fs::write(subs.join("agent-c1.meta.json"), "not json").unwrap();
        std::fs::write(subs.join("agent-a1b2c3d4e5f6a7b8c.jsonl"),
            "{\"type\":\"user\",\"isSidechain\":true,\"agentId\":\"a1b2c3d4e5f6a7b8c\",\"timestamp\":\"2026-11-20T10:00:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"Count.\"}}\n").unwrap();
        let mut sides = claude_sides(stem.to_str().unwrap());
        sides.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(sides.len(), 2, "{sides:?}");
        assert_eq!(
            (
                sides[0].kind.as_str(),
                sides[0].title.as_str(),
                sides[0].tool_use_id.as_str(),
                sides[0].depth,
                sides[0].parent.as_deref(),
                sides[0].finished
            ),
            (
                "general-purpose",
                "Count the nests",
                "toolu_01",
                1,
                None,
                false
            )
        );
        assert_eq!(
            (
                sides[1].title.as_str(),
                sides[1].depth,
                sides[1].parent.as_deref(),
                sides[1].finished
            ),
            ("Map the", 2, Some("a1b2c3d4e5f6a7b8c"), true)
        );
        assert_eq!(
            first_ms(&subs.join("agent-a1b2c3d4e5f6a7b8c.jsonl")),
            ts_ms("2026-11-20T10:00:00.000Z")
        );
        assert!(claude_sides(dir.path().join("none").to_str().unwrap()).is_empty());
    }

    /// "Now: Bash · studio.layout --shot 7" (the owner's mock-up): a row's
    /// latest step is read from the end of the sub-agent's own record,
    /// and again only when the record has grown.
    #[test]
    fn a_rows_now_is_the_last_step_in_the_end_of_the_sub_agents_record() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agent-a1.jsonl");
        let say = |text: &str| json!({"type": "assistant", "isSidechain": true, "agentId": "a1", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}});
        let call = |id: &str, name: &str, input: Value| json!({"type": "assistant", "isSidechain": true, "agentId": "a1", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}});
        let write = |lines: &[Value]| {
            std::fs::write(
                &file,
                lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
            )
            .unwrap()
        };
        let mut lines = vec![
            json!({"type": "user", "isSidechain": true, "agentId": "a1", "message": {"role": "user", "content": "Redo the wide shots."}}),
            say("Shot 3 uses the old height.\nApplying the new values."),
            call(
                "toolu_1",
                "Bash",
                json!({"command": "python -m studio.layout --shot 7"}),
            ),
        ];
        write(&lines);
        assert_eq!(
            step_of(&file, Dialect::Claude),
            (
                Some("Bash · python -m studio.layout --shot 7".into()),
                Some("Shot 3 uses the old height.".into())
            )
        );
        // (Within a second of the last look the answer is the one kept.)
        lines.push(json!({"type": "user", "isSidechain": true, "agentId": "a1", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}]}}));
        lines.push(say("Two shots changed: 3 and 7."));
        write(&lines);
        assert_eq!(
            step_of(&file, Dialect::Claude).0.as_deref(),
            Some("Bash · python -m studio.layout --shot 7")
        );
        std::thread::sleep(Duration::from_millis(1050));
        assert_eq!(
            step_of(&file, Dialect::Claude),
            (
                Some("Two shots changed: 3 and 7.".into()),
                Some("Two shots changed: 3 and 7.".into())
            )
        );
        assert_eq!(
            step_of(&dir.path().join("absent.jsonl"), Dialect::Claude),
            (None, None)
        );
    }
}
