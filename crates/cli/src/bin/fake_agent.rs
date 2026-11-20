//! A scriptable stand-in for a coding agent, for scenario tests.
//!
//! It speaks Claude Code's hook dialect to the daemon over the local socket,
//! prints what happens on its terminal, and reads input like an agent prompt.
//!
//!   spandeck-fake-agent [--script file.json] [--resume <session>] [--prompt <text>]
//!
//! Script: {"steps": [...], "on_input": [...]}. Steps:
//!   {"say": "text"}                      print a line
//!   {"raw": "bytes"}                     write these to the terminal as they are
//!                                        (escape sequences: a program that
//!                                        draws its screen itself)
//!   {"sleep": ms}
//!   {"prompt": "text"}                   UserPromptSubmit
//!   {"tool": {"name":"Bash","command":"x","ok":true,"file":"p","run":false}}
//!   {"permission": {"name":"Write","file":"p"}}
//!   {"ask": {"questions":[…], "id":"toolu_1"}}   AskUserQuestion, as Claude Code asks
//!   {"plan": {"plan":"markdown", "id":"toolu_2"}} ExitPlanMode
//!       ("recorded": what the call's record in the session file holds as
//!       the plan, when it is not the plan the hook is given: Claude Code
//!       writes what its plan file held when the call was made)
//!       Both go through PreToolUse twice, as with the entries spandeck
//!       installs: the hook every tool call has, then the hook that may
//!       wait for the owner. Answered by that hook, the input it was
//!       given back is printed; not answered, the prompt is "in the
//!       terminal" and the next line typed answers it. The call and its
//!       result are written to the session file in Claude Code's shape.
//!   {"work": ms}                         a long tool call; ESC interrupts it
//!   {"subagent": {"type":"Explore","stop":false}}  the agent's own sub-agent starts (or stops)
//!       ("id": the program's own id for it, as the real programs' hooks
//!       carry one; "transcript": where its record is, at its stop;
//!       "type": "" is a helper of the program's own)
//!   {"turn_end": "last message"}         Stop (may be continued with mail)
//!   {"needs_input": "message"}           the program waits at a prompt of its own
//!                                        in its terminal and says so, as Claude
//!                                        Code does (Notification, `agent_needs_input`)
//!   {"rpc": {"method":"…","params":{}}}  call the daemon as this pane, print the reply
//!   {"task_result": {"summary":"…","findings_by_round":[["a"],[]]}}
//!   {"sh": "command"}                    run a shell command in the cwd
//!   {"transcript": {…}}                  append a line to the session file
//!   {"repeat": n, "steps": [...]}
//!   {"exit": code}
//!   {"trust_screen": {"as":"claude"|"antigravity"|"codex-hooks", "focus":"exit"|"trust",
//!                     "extra":["a line Claude Code adds"], "reword":"another question",
//!                     "status":"Antigravity's model row" | false,
//!                     "wait":true}}
//!       a program's folder-trust screen, drawn as the program draws it
//!       (agents/foldertrust.rs). Waiting, the arrow keys move its marker
//!       and Enter takes the choice: trusting goes on, leaving exits 1.
//!       `"wait": false` only prints it, as an agent quoting it would.
//!       `"after_ms"`: shown only after that long.
//!       `"as": "codex-hooks"` is Codex's review of hooks instead
//!       (agents/hookstrust.rs), with `"count"` hooks (12), its marker on
//!       "1. Review hooks"; `"above": ["a row"]` adds rows above it and
//!       `"reword"` replaces its title. The arrow keys move the marker and
//!       Enter takes the choice: "[fake] hooks: trusted all", "review"
//!       or "skipped".
//!   {"codex_reply": "text"}             the agent's answer, written to the session
//!                                        file as Codex writes one (see `"codex"`)
//!   {"codex_new": true}                  Codex's `/new`: the next message begins
//!                                        another session file
//!   {"wait_file": "name"}                wait until that file is in the folder
//!   {"hooks": true}                      this program's hooks run from here on
//!   {"model_screen": {"name": "Sonnet 5.5", "focus": "yes"|"no"}}
//!       Claude Code's "Switch model?", drawn as the real program drew it on
//!       17 Nov 2026 (agents/models.rs), under what is on the screen. It
//!       waits: Enter takes the marked choice ("[fake] model switched to
//!       Sonnet 5.5", and the line Claude Code prints for it in the session
//!       file; or "[fake] model kept"), Esc goes back, the arrows move the
//!       marker, and anything else typed is swallowed and said ("[fake]
//!       the question swallowed: …"), as the real one swallowed a message.
//!   {"picker": {"title": "Select model", "choices": ["Opus 5.5", ["Haiku 5.5", "Fastest"]],
//!               "marked": 0, "multi": false, "window": 10, "hint": "…",
//!               "change_after": 2, "raw": ["rows drawn as they are"]}}
//!       a list to choose from, drawn in the layout of Claude Code's own
//!       (agents/picker.rs). It waits: the arrows move its marker (and the
//!       window on a longer list), Space ticks where several may be
//!       chosen, Enter takes it ("[fake] picked: Haiku 5.5"), `s` takes
//!       it for the session when its key names say so ("[fake] picked for
//!       this session: …"), Esc closes it ("[fake] list closed"), and
//!       anything else typed is swallowed and said. `change_after`: after
//!       that many moves the list is another list (its rows turned
//!       round), as one that changes under a tap. `raw`: these rows are
//!       drawn instead, and only Esc and Enter end it.
//!   {"effort": "high"}                   from here on its hooks say this effort
//!                                        level, as Claude Code's do (also a word
//!                                        of the script itself, for the start)
//!   {"name_session": {"id": "…", "path": "…"}}  a hook names that session as this
//!                                        program's, from here on
//! Beside the steps, `"codex": true`: this program keeps its conversation as
//! Codex 0.169.0 does: no session and no file until the first message, then
//! `<CODEX_HOME or ~/.codex>/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`,
//! whose first line records the id, the folder and when it began, and a
//! line for each message. `"no_hooks": true`: none of its hooks run, as
//! for a Codex whose hooks the owner has not reviewed.
//! `"hook_deadline"`: how long this agent gives a hook
//! that can block, in seconds, and tells it (default: what spandeck
//! installs now, 75 for a tool call); `"old"`: 10 seconds and the hook is
//! told nothing, as in a session started before 16 Nov 2026;
//! `"question_deadline"`: what the hook that may wait for the owner is
//! given, in seconds (default: what spandeck installs, an hour).
//! `"trust_screen"` beside the steps: the same, shown before the session
//! starts, where the real programs ask.
//! A list of them there: one after another, as Codex asks about the folder
//! and then about hooks. `"update_notice": true` beside the steps: before
//! those, Codex 0.169.0's notice of a newer version (agents/foldertrust.rs,
//! `before_first`). It waits: Esc skips it, Enter takes "Update now"
//! ("[fake] updating", and the program ends), anything else is swallowed.
//! After the steps it idles at a prompt; each input line runs `on_input`.

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use spandeck_core::client::Client;
use spandeck_core::Paths;

struct Agent {
    paths: Paths,
    session: String,
    transcript: String,
    cwd: String,
    input: Receiver<u8>,
    round: usize,
    interrupted: bool,
    /// The effort level its hooks say, when the script gives one.
    effort: Option<String>,
    /// The script's `hook_deadline`.
    hook_deadline: Value,
    /// The script's `question_deadline`.
    question_deadline: Option<u64>,
    /// The script's `paste`: this program asks for pastes to be marked,
    /// as the real agents do, and says when a line came as one.
    paste: bool,
    /// The script's `drop_enters`: how many Enters at a line that has text
    /// are not taken, as a program that missed the key, or one that cannot
    /// take a turn (a sign-in screen, no quota left).
    drop_enters: u64,
    /// The script's `no_hooks`, and the step `hooks`.
    hooks: bool,
    /// The script's `codex`: where Codex's session files go.
    codex: Option<std::path::PathBuf>,
    /// Whether the session file in use has its first line.
    rollout: bool,
}

/// Milliseconds since 1970 as `2026-11-15T12:00:00.000Z`.
fn iso(ms: u64) -> String {
    let (secs, milli) = (ms / 1000, ms % 1000);
    let days = (secs / 86_400) as i64;
    let (h, mi, sec) = (secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    // Howard Hinnant's civil-date arithmetic.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{sec:02}.{milli:03}Z")
}

fn out(s: &str) {
    let mut o = std::io::stdout();
    let _ = o.write_all(s.replace('\n', "\r\n").as_bytes());
    let _ = o.write_all(b"\r\n");
    let _ = o.flush();
}

impl Agent {
    fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut c =
            Client::from_env_waiting(&self.paths, Duration::ZERO).map_err(|e| e.to_string())?;
        c.set_timeout(Some(Duration::from_secs(400)));
        c.call(method, params).map_err(|e| e.to_string())
    }

    /// Send one hook payload; returns the daemon's reply stdout as JSON.
    fn hook(&self, event: &str, mut extra: Value) -> Value {
        if !self.hooks {
            return Value::Null;
        }
        let obj = extra.as_object_mut().expect("hook payload is an object");
        obj.insert("hook_event_name".into(), json!(event));
        obj.insert("session_id".into(), json!(self.session));
        obj.insert("transcript_path".into(), json!(self.transcript));
        obj.insert("cwd".into(), json!(self.cwd));
        if let Some(level) = &self.effort {
            obj.insert("effort".into(), json!({"level": level}));
        }
        // The hook itself is `spandeck hook`'s own code (core/guard.rs):
        // what it does while the daemon is away is what a real agent's
        // hook does. What a real agent adds is its timeout, which this
        // one only tells the hook.
        use spandeck_agents::{install, AgentKind};
        let deadline = match &self.hook_deadline {
            Value::String(old) if old == "old" => None,
            Value::Number(secs) if event == "PreToolUse" => secs.as_u64(),
            _ => install::timeout_secs(AgentKind::Fake, event).filter(|_| install::blocking(event)),
        };
        let (reply, outcome) = spandeck_core::guard::run_told(
            &self.paths,
            "fake",
            deadline,
            false,
            &extra.to_string(),
        );
        if let spandeck_core::guard::Outcome::Unanswered(why) = outcome {
            out(&format!("[fake] hook error: {why}"));
        }
        serde_json::from_str(&reply.stdout).unwrap_or(Value::Null)
    }

    /// The second hook of a call that asks the owner something: the one
    /// spandeck installs for those two tools by name, with its own long
    /// timeout, and told that it is the one that may wait.
    fn answering_hook(&self, mut extra: Value) -> Value {
        if !self.hooks {
            return Value::Null;
        }
        let obj = extra.as_object_mut().expect("hook payload is an object");
        obj.insert("hook_event_name".into(), json!("PreToolUse"));
        obj.insert("session_id".into(), json!(self.session));
        obj.insert("transcript_path".into(), json!(self.transcript));
        obj.insert("cwd".into(), json!(self.cwd));
        let deadline = self
            .question_deadline
            .unwrap_or(spandeck_agents::install::QUESTION_HOOK_SECS);
        let (reply, _) = spandeck_core::guard::run_told(
            &self.paths,
            "fake",
            Some(deadline),
            true,
            &extra.to_string(),
        );
        serde_json::from_str(&reply.stdout).unwrap_or(Value::Null)
    }

    /// AskUserQuestion or ExitPlanMode, the way Claude Code runs one.
    fn asks(&mut self, name: &str, spec: &Value) {
        let id = spec["id"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| spandeck_core::paths::new_id("toolu"));
        let input = match name {
            "AskUserQuestion" => json!({"questions": spec["questions"]}),
            _ => json!({"plan": spec["plan"], "planFilePath": format!("{}/plan.md", self.cwd)}),
        };
        let what = if name == "AskUserQuestion" {
            "question"
        } else {
            "plan"
        };
        let mut recorded = input.clone();
        if let Some(old) = spec.get("recorded") {
            recorded["plan"] = old.clone();
        }
        self.append_transcript(
            &json!({"type": "assistant", "message": {"role": "assistant",
            "content": [{"type": "tool_use", "id": id, "name": name, "input": recorded}]}}),
        );
        let payload = json!({"tool_name": name, "tool_input": input, "tool_use_id": id});
        let denied = |reply: &Value| {
            (reply.pointer("/hookSpecificOutput/permissionDecision") == Some(&json!("deny"))).then(
                || {
                    reply
                        .pointer("/hookSpecificOutput/permissionDecisionReason")
                        .and_then(|r| r.as_str())
                        .unwrap_or("")
                        .to_string()
                },
            )
        };
        let result = |content: &str, error: bool, tur: Value| {
            json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": content, "is_error": error}]},
                "toolUseResult": tur})
        };
        // The hook every tool call has: a rule may refuse the call.
        let first = self.hook("PreToolUse", payload.clone());
        if let Some(reason) = denied(&first) {
            out(&format!("[fake] BLOCKED {name}: {reason}"));
            self.append_transcript(&result(&reason, true, json!({})));
            return;
        }
        out(&format!("[fake] {what}?"));
        let reply = self.answering_hook(payload.clone());
        let given = reply.pointer("/hookSpecificOutput/updatedInput").cloned();
        let allowed =
            reply.pointer("/hookSpecificOutput/permissionDecision") == Some(&json!("allow"));
        let line = if let Some(reason) = denied(&reply) {
            out(&format!("[fake] {what} sent back: {reason}"));
            result(&reason, true, json!({}))
        } else if let (true, Some(given)) = (allowed, given) {
            // What the hook gave the call as its input, whole.
            out(&format!("[fake] {what} answered by the hook: {given}"));
            match name {
                "AskUserQuestion" => result(
                    "User has answered your questions.",
                    false,
                    json!({"questions": given["questions"], "answers": given["answers"]}),
                ),
                _ => result(
                    "User has approved your plan.",
                    false,
                    json!({"plan": given["plan"], "isAgent": false, "filePath": given["planFilePath"]}),
                ),
            }
        } else {
            // No decision: the prompt is the agent's own, in its terminal.
            // As Claude Code does when it shows that prompt (seen with
            // 2.1.325): it asks permission for the same call through its
            // permission hook, and says that it needs the owner.
            let asked = self.hook(
                "PermissionRequest",
                json!({"tool_name": name, "tool_input": input}),
            );
            if !asked.is_null() {
                out(&format!(
                    "[fake] {what}: the permission hook answered {asked}"
                ));
            }
            self.hook(
                "Notification",
                json!({"notification_type": "permission_prompt", "message": "Claude needs your permission"}),
            );
            out(&format!(
                "[fake] {what} is in the terminal: type your answer"
            ));
            let typed = self.read_line().unwrap_or_default();
            out(&format!("[fake] {what} answered in the terminal: {typed}"));
            match name {
                "AskUserQuestion" => {
                    let asked = input["questions"][0]["question"].as_str().unwrap_or("");
                    result(
                        "User has answered your questions.",
                        false,
                        json!({"questions": input["questions"], "answers": {asked: typed}}),
                    )
                }
                _ if typed.starts_with('y') => result(
                    "User has approved your plan.",
                    false,
                    json!({"plan": input["plan"], "isAgent": false}),
                ),
                _ => result(
                    "The user doesn't want to proceed with this tool use.",
                    true,
                    json!({}),
                ),
            }
        };
        let refused = line["message"]["content"][0]["is_error"] == json!(true);
        self.append_transcript(&line);
        if !refused {
            self.hook(
                "PostToolUse",
                json!({"tool_name": name, "tool_input": input, "tool_use_id": id, "tool_response": {}}),
            );
        }
    }

    fn show_context(reply: &Value) {
        if let Some(ctx) = reply
            .pointer("/hookSpecificOutput/additionalContext")
            .and_then(|c| c.as_str())
        {
            out(&format!("[fake] context: {}", ctx.replace('\n', " | ")));
        }
    }

    /// A message, as Codex 0.169.0 writes one to its session file (the
    /// shapes of `crates/archive/fixtures/codex-0.169.0.jsonl`). The file
    /// is begun with the first message, as the real program does.
    fn codex_say(&mut self, role: &str, text: &str) {
        let Some(sessions) = self.codex.clone() else {
            return;
        };
        let now = spandeck_core::paths::now_ms();
        if !self.rollout {
            let id = spandeck_core::paths::new_id("019a")
                .replace('_', "-")
                .to_lowercase();
            let day = iso(now);
            let dir = sessions.join(&day[0..4]).join(&day[5..7]).join(&day[8..10]);
            let _ = std::fs::create_dir_all(&dir);
            let name = format!("rollout-{}-{id}.jsonl", day[0..19].replace(':', "-"));
            self.transcript = dir.join(name).display().to_string();
            self.session = id;
            self.rollout = true;
            self.write_line(
                &json!({"timestamp": iso(now), "type": "session_meta", "payload": {
                "id": self.session, "session_id": self.session, "timestamp": iso(now),
                "cwd": self.cwd, "originator": "codex_cli_rs", "cli_version": "0.169.0",
                "source": "cli", "model_provider": "openai"}}),
            );
        }
        let (kind, extra) = if role == "user" {
            ("input_text", json!({}))
        } else {
            (
                "output_text",
                json!({"phase": "final_answer", "status": "completed"}),
            )
        };
        let mut payload = json!({"type": "message", "role": role,
            "content": [{"type": kind, "text": text}]});
        if let (Some(p), Some(e)) = (payload.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        self.write_line(
            &json!({"timestamp": iso(now), "type": "response_item", "payload": payload}),
        );
    }

    fn append_transcript(&mut self, line: &Value) {
        // Codex's file has Codex's lines: what the owner said, and no other.
        if self.codex.is_some() {
            // (And a record of Codex's own, given whole: a turn's end.)
            if matches!(line["type"].as_str(), Some("event_msg" | "response_item")) {
                let mut line = line.clone();
                line["timestamp"] = json!(iso(spandeck_core::paths::now_ms()));
                self.write_line(&line);
                return;
            }
            if let (Some("user"), Some(text)) = (
                line["type"].as_str(),
                line.pointer("/message/content").and_then(|c| c.as_str()),
            ) {
                self.codex_say("user", text);
            }
            return;
        }
        self.write_line(line);
    }

    fn write_line(&self, line: &Value) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    /// Wait up to `ms`; true if ESC arrived (the turn is interrupted).
    fn wait_or_escape(&mut self, ms: u64) -> bool {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.input.recv_timeout(left) {
                Ok(0x1b) => return true,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return false,
                Err(RecvTimeoutError::Disconnected) => std::process::exit(0),
            }
        }
    }

    fn read_line(&mut self) -> Option<String> {
        let mut line = Vec::new();
        let mut pasted = false;
        loop {
            match self.input.recv() {
                Ok(b'\r') | Ok(b'\n') => {
                    if line.is_empty() {
                        continue;
                    }
                    if self.drop_enters > 0 {
                        self.drop_enters -= 1;
                        continue;
                    }
                    let _ = std::io::stdout().write_all(b"\r\n");
                    let _ = std::io::stdout().flush();
                    if pasted {
                        out("[fake] (that line was pasted)");
                    }
                    return Some(String::from_utf8_lossy(&line).to_string());
                }
                // A paste's markers, for a program that asked for them:
                // `ESC[200~` before and `ESC[201~` after.
                Ok(0x1b) if self.paste => {
                    let mut seq = Vec::new();
                    while seq.len() < 5 {
                        match self.input.recv_timeout(Duration::from_millis(50)) {
                            Ok(b) => seq.push(b),
                            Err(_) => break,
                        }
                    }
                    match seq.as_slice() {
                        b"[200~" => pasted = true,
                        b"[201~" => {}
                        _ => {
                            out("[fake] (escape at prompt)");
                            line.clear();
                        }
                    }
                }
                Ok(0x1b) => {
                    out("[fake] (escape at prompt)");
                    line.clear();
                }
                Ok(0x03) | Ok(0x04) => return None,
                Ok(b) => {
                    line.push(b);
                    let _ = std::io::stdout().write_all(&[b]);
                    let _ = std::io::stdout().flush();
                }
                Err(_) => return None,
            }
        }
    }

    fn run_steps(&mut self, steps: &[Value]) {
        for step in steps {
            if self.interrupted {
                return;
            }
            self.step(step);
        }
    }

    fn step(&mut self, step: &Value) {
        if let Some(text) = step.get("say").and_then(|s| s.as_str()) {
            out(text);
        } else if let Some(bytes) = step.get("raw").and_then(|s| s.as_str()) {
            let mut o = std::io::stdout();
            let _ = o.write_all(bytes.as_bytes());
            let _ = o.flush();
        } else if let Some(ms) = step.get("sleep").and_then(|s| s.as_u64()) {
            std::thread::sleep(Duration::from_millis(ms));
        } else if let Some(text) = step.get("prompt").and_then(|s| s.as_str()) {
            self.append_transcript(
                &json!({"type": "user", "message": {"role": "user", "content": text}}),
            );
            let reply = self.hook("UserPromptSubmit", json!({"prompt": text}));
            Self::show_context(&reply);
        } else if let Some(tool) = step.get("tool") {
            self.tool(tool);
        } else if let Some(perm) = step.get("permission") {
            let name = perm["name"].as_str().unwrap_or("Write");
            let input = json!({"file_path": perm["file"], "command": perm["command"]});
            out("[fake] permission?");
            let reply = self.hook(
                "PermissionRequest",
                json!({"tool_name": name, "tool_input": input}),
            );
            let behavior = reply
                .pointer("/hookSpecificOutput/decision/behavior")
                .and_then(|b| b.as_str())
                .unwrap_or("none");
            out(&format!("[fake] permission: {behavior}"));
        } else if let Some(ask) = step.get("ask") {
            self.asks("AskUserQuestion", ask);
        } else if let Some(plan) = step.get("plan") {
            self.asks("ExitPlanMode", plan);
        } else if let Some(sub) = step.get("subagent") {
            let stop = sub.get("stop").and_then(|s| s.as_bool()).unwrap_or(false);
            let mut payload = json!({"agent_type": sub.get("type").and_then(|t| t.as_str()).unwrap_or("Explore")});
            // As Claude Code 2.1.329 and codex-cli 0.170.0 send them:
            // the sub-agent's own id, and at its stop where its record is.
            if let Some(id) = sub.get("id").and_then(|i| i.as_str()) {
                payload["agent_id"] = json!(id);
            }
            if let Some(path) = sub.get("transcript").and_then(|i| i.as_str()) {
                payload["agent_transcript_path"] = json!(path);
            }
            self.hook(
                if stop {
                    "SubagentStop"
                } else {
                    "SubagentStart"
                },
                payload,
            );
        } else if let Some(ms) = step.get("work").and_then(|s| s.as_u64()) {
            self.hook("PreToolUse", json!({"tool_name": "Bash", "tool_input": {"command": format!("sleep {ms}ms")}, "tool_use_id": "work"}));
            out("[fake] working (esc to interrupt)");
            if self.wait_or_escape(ms) {
                out("[fake] interrupted");
                self.hook(
                    "Stop",
                    json!({"last_assistant_message": "interrupted", "stop_hook_active": false}),
                );
                self.interrupted = true;
            } else {
                let post = self.hook("PostToolUse", json!({"tool_name": "Bash", "tool_input": {"command": format!("sleep {ms}ms")}, "tool_use_id": "work", "tool_response": {}}));
                out("[fake] work finished");
                Self::show_context(&post);
            }
        } else if let Some(msg) = step.get("turn_end").and_then(|s| s.as_str()) {
            self.append_transcript(&json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": msg}]}}));
            let reply = self.hook(
                "Stop",
                json!({"last_assistant_message": msg, "stop_hook_active": false}),
            );
            if reply["decision"] == json!("block") {
                out(&format!(
                    "[fake] CONTINUE: {}",
                    reply["reason"].as_str().unwrap_or("")
                ));
            } else {
                out("[fake] turn done");
            }
        } else if let Some(msg) = step.get("needs_input").and_then(|s| s.as_str()) {
            self.hook(
                "Notification",
                json!({"notification_type": "agent_needs_input", "message": msg}),
            );
        } else if let Some(rpc) = step.get("rpc") {
            let method = rpc["method"].as_str().unwrap_or("");
            match self.rpc(method, rpc.get("params").cloned().unwrap_or(json!({}))) {
                Ok(v) => out(&format!("[fake] {method} -> {v}")),
                Err(e) => out(&format!("[fake] {method} ERROR: {e}")),
            }
        } else if let Some(tr) = step.get("task_result") {
            let mut params = tr.clone();
            if let Some(by_round) = tr.get("findings_by_round").and_then(|f| f.as_array()) {
                let idx = self
                    .round
                    .saturating_sub(1)
                    .min(by_round.len().saturating_sub(1));
                params["findings"] = by_round.get(idx).cloned().unwrap_or(json!([]));
            }
            match self.rpc("task.result", params) {
                Ok(v) => out(&format!("[fake] task.result -> {}", v["state"])),
                Err(e) => out(&format!("[fake] task.result ERROR: {e}")),
            }
        } else if let Some(cmd) = step.get("sh").and_then(|s| s.as_str()) {
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&self.cwd)
                .stdin(std::process::Stdio::null())
                .output();
            match status {
                Ok(o) => out(&format!("[fake] sh exit {}", o.status.code().unwrap_or(-1))),
                Err(e) => out(&format!("[fake] sh error {e}")),
            }
        } else if let Some(line) = step.get("transcript") {
            self.append_transcript(line);
        } else if let Some(text) = step.get("codex_reply").and_then(|s| s.as_str()) {
            out(&format!("[fake] {text}"));
            self.codex_say("assistant", text);
        } else if let Some(name) = step.get("wait_file").and_then(|s| s.as_str()) {
            let path = std::path::Path::new(&self.cwd).join(name);
            while !path.exists() {
                std::thread::sleep(Duration::from_millis(30));
            }
        } else if step.get("codex_new").is_some() {
            self.rollout = false;
            out("[fake] new conversation");
        } else if let Some(level) = step.get("effort").and_then(|s| s.as_str()) {
            self.effort = Some(level.to_string());
        } else if let Some(on) = step.get("hooks").and_then(|s| s.as_bool()) {
            self.hooks = on;
        } else if let Some(named) = step.get("name_session") {
            self.hooks = true;
            self.session = named["id"].as_str().unwrap_or("").to_string();
            self.transcript = named["path"].as_str().unwrap_or("").to_string();
            self.rollout = true;
            self.hook("SessionStart", json!({"source": "resume"}));
        } else if let Some(n) = step.get("repeat").and_then(|s| s.as_u64()) {
            let steps = step["steps"].as_array().cloned().unwrap_or_default();
            for _ in 0..n {
                self.run_steps(&steps);
            }
        } else if let Some(spec) = step.get("trust_screen") {
            self.trust_screen(spec);
        } else if let Some(spec) = step.get("model_screen") {
            self.model_screen(spec);
        } else if let Some(spec) = step.get("picker") {
            self.picker(spec);
        } else if let Some(code) = step.get("exit").and_then(|s| s.as_i64()) {
            self.hook("SessionEnd", json!({"reason": "other"}));
            restore_terminal();
            std::process::exit(code as i32);
        }
    }

    /// Codex's review of hooks (see the top of this file).
    fn hooks_screen(&mut self, spec: &Value) {
        use spandeck_agents::hookstrust::{drawn, Mark};
        let count = spec["count"].as_u64().unwrap_or(12) as u32;
        let order = [Mark::Review, Mark::TrustAll, Mark::Skip];
        let mut at = 0usize;
        let draw = |at: usize| {
            let mut lines = drawn::codex(count, order[at]);
            if let Some(words) = spec["reword"].as_str() {
                lines[1] = format!("  {words}");
            }
            let above: Vec<String> = spec["above"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|l| l.as_str())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            lines.splice(0..0, above);
            let mut o = std::io::stdout();
            let _ = o.write_all(b"\x1b[2J\x1b[H");
            let _ = o.write_all(lines.join("\r\n").as_bytes());
            let _ = o.flush();
        };
        draw(at);
        if spec["wait"] == json!(false) {
            out("");
            return;
        }
        loop {
            let key = match self.input.recv() {
                Ok(b) => b,
                Err(_) => std::process::exit(0),
            };
            match key {
                b'\r' | b'\n' => {
                    let _ = std::io::stdout().write_all(b"\x1b[2J\x1b[H");
                    out(match order[at] {
                        Mark::Review => "[fake] hooks: review",
                        Mark::TrustAll => "[fake] hooks: trusted all",
                        Mark::Skip => "[fake] hooks: skipped",
                    });
                    return;
                }
                0x1b => {
                    let wait = Duration::from_millis(50);
                    if self.input.recv_timeout(wait) != Ok(b'[') {
                        continue;
                    }
                    match self.input.recv_timeout(wait) {
                        Ok(b'B') => at = (at + 1).min(2),
                        Ok(b'A') => at = at.saturating_sub(1),
                        _ => continue,
                    }
                    draw(at);
                }
                _ => {}
            }
        }
    }

    /// A program's folder-trust screen (see the top of this file).
    fn picker(&mut self, spec: &Value) {
        let title = spec["title"].as_str().unwrap_or("Choose").to_string();
        let multi = spec["multi"].as_bool().unwrap_or(false);
        let mut choices: Vec<(String, String, Option<bool>)> = spec["choices"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| match c {
                        Value::String(s) => (s.clone(), String::new(), multi.then_some(false)),
                        other => (
                            other[0].as_str().unwrap_or("").to_string(),
                            other[1].as_str().unwrap_or("").to_string(),
                            multi.then_some(false),
                        ),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let hint = spec["hint"]
            .as_str()
            .unwrap_or("Enter to confirm · Esc to cancel")
            .to_string();
        let window = spec["window"].as_u64().unwrap_or(20) as usize;
        let change_after = spec["change_after"].as_u64();
        let raw: Option<Vec<String>> = spec["raw"].as_array().map(|a| {
            a.iter()
                .filter_map(|l| l.as_str().map(String::from))
                .collect()
        });
        let mut marked = spec["marked"].as_u64().unwrap_or(0) as usize;
        let (mut top, mut moves, mut swallowed) = (0usize, 0u64, String::new());
        loop {
            if marked < top {
                top = marked;
            } else if marked >= top + window {
                top = marked + 1 - window;
            }
            let lines = match &raw {
                Some(rows) => rows.clone(),
                None => {
                    spandeck_agents::picker::drawn(&title, &choices, marked, top, window, &hint)
                }
            };
            let mut o = std::io::stdout();
            let _ = o.write_all(b"\r\n");
            let _ = o.write_all(lines.join("\r\n").as_bytes());
            let _ = o.flush();
            let key = match self.input.recv() {
                Ok(b) => b,
                Err(_) => std::process::exit(0),
            };
            let said = |agent: &mut Agent, what: String, swallowed: &str| {
                let _ = agent;
                out("");
                if !swallowed.is_empty() {
                    out(&format!("[fake] the list swallowed: {swallowed}"));
                }
                out(&what);
            };
            let picked = |choices: &[(String, String, Option<bool>)], marked: usize| match multi {
                true => choices
                    .iter()
                    .filter(|c| c.2 == Some(true))
                    .map(|c| c.0.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
                false => choices.get(marked).map(|c| c.0.clone()).unwrap_or_default(),
            };
            match key {
                b'\r' | b'\n' => {
                    return said(
                        self,
                        format!("[fake] picked: {}", picked(&choices, marked)),
                        &swallowed,
                    )
                }
                b's' if hint.contains("s to use this session only") && raw.is_none() => {
                    return said(
                        self,
                        format!(
                            "[fake] picked for this session: {}",
                            picked(&choices, marked)
                        ),
                        &swallowed,
                    )
                }
                b' ' if multi => {
                    if let Some(c) = choices.get_mut(marked) {
                        c.2 = Some(c.2 != Some(true));
                    }
                }
                0x1b => {
                    let wait = Duration::from_millis(50);
                    match self.input.recv_timeout(wait) {
                        Ok(b'[') => {
                            let mut seq = vec![];
                            while let Ok(b) = self.input.recv_timeout(wait) {
                                seq.push(b);
                                if b.is_ascii_alphabetic() || b == b'~' {
                                    break;
                                }
                            }
                            if raw.is_none() && (seq == b"A" || seq == b"B") {
                                moves += 1;
                                marked = if seq == b"A" {
                                    marked.saturating_sub(1)
                                } else {
                                    (marked + 1).min(choices.len().saturating_sub(1))
                                };
                                if change_after == Some(moves) {
                                    choices.rotate_left(1);
                                }
                            }
                        }
                        _ => return said(self, "[fake] list closed".to_string(), &swallowed),
                    }
                }
                b if b >= 0x20 => swallowed.push(b as char),
                _ => {}
            }
        }
    }

    fn model_screen(&mut self, spec: &Value) {
        let name = spec["name"].as_str().unwrap_or("Sonnet 5.5").to_string();
        let mut on_yes = spec["focus"].as_str() != Some("no");
        let draw = |on_yes: bool| {
            let lines = spandeck_agents::models::switch_drawn(&name, on_yes);
            let mut o = std::io::stdout();
            // (Under what is on the screen: nothing is wiped.)
            let _ = o.write_all(b"\r\n");
            let _ = o.write_all(lines.join("\r\n").as_bytes());
            let _ = o.flush();
        };
        draw(on_yes);
        let mut swallowed = String::new();
        let done = |agent: &mut Agent, switched: bool, swallowed: &str| {
            out("");
            if !swallowed.is_empty() {
                out(&format!("[fake] the question swallowed: {swallowed}"));
            }
            if switched {
                agent.append_transcript(&json!({"type": "user", "message": {"role": "user",
                    "content": format!("<local-command-stdout>Set model to `{name}` and saved as your default for new sessions</local-command-stdout>")}}));
                out(&format!("[fake] model switched to {name}"));
            } else {
                out("[fake] model kept");
            }
        };
        loop {
            let key = match self.input.recv() {
                Ok(b) => b,
                Err(_) => std::process::exit(0),
            };
            match key {
                b'\r' | b'\n' => return done(self, on_yes, &swallowed),
                0x1b => {
                    // An arrow, the marks around a paste, or Esc alone.
                    let wait = Duration::from_millis(50);
                    match self.input.recv_timeout(wait) {
                        Ok(b'[') => {
                            let mut seq = vec![];
                            while let Ok(b) = self.input.recv_timeout(wait) {
                                seq.push(b);
                                if b.is_ascii_alphabetic() || b == b'~' {
                                    break;
                                }
                            }
                            if seq == b"A" || seq == b"B" {
                                on_yes = !on_yes;
                                draw(on_yes);
                            }
                        }
                        _ => return done(self, false, &swallowed),
                    }
                }
                b if b >= 0x20 => swallowed.push(b as char),
                _ => {}
            }
        }
    }

    /// Codex's notice of a newer version (see the top of this file).
    fn update_notice(&mut self) {
        let rows = [
            "  Update available · 0.169.0 → 0.170.0",
            "",
            "› 1. Update now (runs `npm install -g @openai/codex`)",
            "  2. Skip",
            "  3. Skip until next version",
            "",
            "  enter continue · esc skip",
        ];
        let mut o = std::io::stdout();
        let _ = o.write_all(b"\x1b[2J\x1b[H");
        let _ = o.write_all(rows.join("\r\n").as_bytes());
        let _ = o.flush();
        loop {
            let key = match self.input.recv() {
                Ok(b) => b,
                Err(_) => std::process::exit(0),
            };
            match key {
                b'\r' | b'\n' => {
                    let _ = o.write_all(b"\x1b[2J\x1b[H");
                    out("[fake] updating");
                    restore_terminal();
                    std::process::exit(0);
                }
                0x1b => {
                    // Esc alone; an arrow or a paste's marks are not it.
                    let wait = Duration::from_millis(50);
                    if self.input.recv_timeout(wait).is_err() {
                        let _ = o.write_all(b"\x1b[2J\x1b[H");
                        let _ = o.flush();
                        return;
                    }
                    while let Ok(b) = self.input.recv_timeout(wait) {
                        if b.is_ascii_alphabetic() || b == b'~' {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn trust_screen(&mut self, spec: &Value) {
        use spandeck_agents::foldertrust::{drawn, Focus};
        let program = spec["as"].as_str().unwrap_or("claude");
        if let Some(ms) = spec["after_ms"].as_u64() {
            std::thread::sleep(Duration::from_millis(ms));
        }
        if program == "codex-hooks" {
            return self.hooks_screen(spec);
        }
        // Each program's own choice when it first draws the screen.
        let mut focus = match (spec["focus"].as_str(), program) {
            (Some("trust"), _) => Focus::Trust,
            (Some("exit"), _) => Focus::Exit,
            (_, "claude") => Focus::Exit,
            _ => Focus::Trust,
        };
        let extra: Vec<&str> = spec["extra"]
            .as_array()
            .map(|a| a.iter().filter_map(|l| l.as_str()).collect())
            .unwrap_or_default();
        let clear = || {
            let _ = std::io::stdout().write_all(b"\x1b[2J\x1b[H");
        };
        let draw = |focus: Focus| {
            let mut lines = match program {
                // As 1.5.9 draws it, with its model under the hint;
                // `"status": false` leaves that row out, another string
                // puts another model there.
                "antigravity" => match &spec["status"] {
                    Value::Bool(false) => drawn::antigravity_with(&self.cwd, focus, None),
                    Value::String(s) => drawn::antigravity_with(&self.cwd, focus, Some(s)),
                    _ => drawn::antigravity(&self.cwd, focus),
                },
                "codex" => drawn::codex(&self.cwd, focus, 110),
                _ => drawn::claude(&self.cwd, focus, 110, &extra),
            };
            if let Some(words) = spec["reword"].as_str() {
                let at = if program == "antigravity" { 4 } else { 6 };
                lines[at] = format!("{}{words}", if program == "antigravity" { "" } else { " " });
            }
            clear();
            let mut o = std::io::stdout();
            let _ = o.write_all(lines.join("\r\n").as_bytes());
            let _ = o.flush();
        };
        draw(focus);
        if spec["wait"] == json!(false) {
            out("");
            return;
        }
        // The key that moves the marker towards the lower choice.
        let trust_is_lower = program != "antigravity";
        let drawn_at = Instant::now();
        loop {
            let key = match self.input.recv() {
                Ok(b) => b,
                Err(_) => std::process::exit(0),
            };
            // Claude Code takes no key in the first moment its prompt is
            // up, so that one typed for something else is not an answer.
            if program == "claude" && drawn_at.elapsed() < Duration::from_millis(250) {
                continue;
            }
            match key {
                b'\r' | b'\n' if focus == Focus::Trust => {
                    clear();
                    out("[fake] folder trusted");
                    return;
                }
                b'\r' | b'\n' => {
                    clear();
                    out("[fake] left: the folder was not trusted");
                    restore_terminal();
                    std::process::exit(1);
                }
                0x1b => {
                    let wait = Duration::from_millis(50);
                    if self.input.recv_timeout(wait) != Ok(b'[') {
                        continue;
                    }
                    let down = match self.input.recv_timeout(wait) {
                        Ok(b'B') => true,
                        Ok(b'A') => false,
                        _ => continue,
                    };
                    focus = if down == trust_is_lower {
                        Focus::Trust
                    } else {
                        Focus::Exit
                    };
                    draw(focus);
                }
                _ => {}
            }
        }
    }

    fn tool(&mut self, tool: &Value) {
        let name = tool["name"].as_str().unwrap_or("Bash");
        let mut input = json!({});
        if let Some(c) = tool.get("command") {
            input["command"] = c.clone();
        }
        if let Some(f) = tool.get("file") {
            input["file_path"] = f.clone();
        }
        let id = spandeck_core::paths::new_id("toolu");
        let pre = self.hook(
            "PreToolUse",
            json!({"tool_name": name, "tool_input": input, "tool_use_id": id}),
        );
        if pre.pointer("/hookSpecificOutput/permissionDecision") == Some(&json!("deny")) {
            out(&format!(
                "[fake] BLOCKED {name}: {}",
                pre.pointer("/hookSpecificOutput/permissionDecisionReason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
            ));
            return;
        }
        let mut ok = tool.get("ok").and_then(|o| o.as_bool()).unwrap_or(true);
        if tool.get("run").and_then(|r| r.as_bool()).unwrap_or(false) {
            if let Some(cmd) = tool.get("command").and_then(|c| c.as_str()) {
                ok = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(&self.cwd)
                    .stdin(std::process::Stdio::null())
                    .output()
                    .is_ok_and(|o| o.status.success());
            }
        }
        if let (Some(file), Some(content)) = (
            tool.get("file").and_then(|f| f.as_str()),
            tool.get("content").and_then(|c| c.as_str()),
        ) {
            let path = if file.starts_with('/') {
                file.to_string()
            } else {
                format!("{}/{file}", self.cwd)
            };
            let _ = std::fs::write(path, content);
        }
        out(&format!(
            "[fake] ran {name} ({})",
            if ok { "ok" } else { "failed" }
        ));
        let post = if ok {
            self.hook("PostToolUse", json!({"tool_name": name, "tool_input": input, "tool_use_id": id, "tool_response": {}, "duration_ms": 5}))
        } else {
            self.hook("PostToolUseFailure", json!({"tool_name": name, "tool_input": input, "tool_use_id": id, "error": "Exit code 1", "duration_ms": 5}))
        };
        Self::show_context(&post);
    }
}

static SAVED: std::sync::Mutex<Option<spandeck_terminal::sys::RawInput>> =
    std::sync::Mutex::new(None);

/// Keys one at a time, no echo, Ctrl-C as a byte: how an agent's TUI reads.
fn raw_terminal() {
    *SAVED.lock().unwrap() = Some(spandeck_terminal::sys::stdin_raw());
}

fn restore_terminal() {
    if let Some(saved) = SAVED.lock().unwrap().as_mut() {
        saved.restore();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let script: Value = arg("--script")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({"steps": []}));
    let resume = arg("--resume");
    let prompt = arg("--prompt");
    // Started on a model or an effort of the pane's own, as Claude Code is
    // with these flags: said, so a test can see it.
    if let Some(model) = arg("--model") {
        out(&format!(
            "[fake] started on the model {model}{}",
            if resume.is_some() { ", resuming" } else { "" }
        ));
    }
    if let Some(level) = arg("--effort") {
        out(&format!("[fake] started at the effort {level}"));
    }
    let session = resume
        .clone()
        .unwrap_or_else(|| spandeck_core::paths::new_id("fake"));
    let dir = std::env::var("SPANDECK_FAKE_DIR")
        .unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    raw_terminal();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut b = [0u8; 1];
        while let Ok(1) = stdin.read(&mut b) {
            if tx.send(b[0]).is_err() {
                break;
            }
        }
    });

    // "Review round 2 of 3" in a delegated brief selects that round's script.
    let round = prompt
        .as_deref()
        .and_then(|p| p.split("round ").nth(1))
        .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    let mut agent = Agent {
        paths: Paths::from_env(),
        transcript: format!("{dir}/{session}.jsonl"),
        session,
        cwd,
        input: rx,
        round,
        interrupted: false,
        effort: script["effort"].as_str().map(String::from),
        hook_deadline: script["hook_deadline"].clone(),
        question_deadline: script["question_deadline"].as_u64(),
        paste: script["paste"].as_bool().unwrap_or(false),
        drop_enters: script["drop_enters"].as_u64().unwrap_or(0),
        hooks: script["no_hooks"] != json!(true),
        codex: (script["codex"] == json!(true)).then(|| {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            spandeck_agents::sessions::codex_sessions_dir(&home)
        }),
        rollout: false,
    };
    if agent.paste {
        let _ = std::io::stdout().write_all(b"\x1b[?2004h");
    }

    // Where the real programs ask about the folder: before anything else.
    if script["update_notice"] == json!(true) {
        agent.update_notice();
    }
    match script.get("trust_screen") {
        Some(Value::Array(specs)) => specs.iter().for_each(|spec| agent.trust_screen(spec)),
        Some(spec) => agent.trust_screen(spec),
        None => {}
    }
    match &resume {
        Some(id) => out(&format!("[fake] RESUMED {id}")),
        None => out(&format!("[fake] session {}", agent.session)),
    }
    let start = agent.hook(
        "SessionStart",
        json!({"source": if resume.is_some() { "resume" } else { "startup" }}),
    );
    Agent::show_context(&start);
    if let Some(p) = &prompt {
        out(&format!("[fake] prompt: {}", p.replace('\n', " | ")));
        agent
            .append_transcript(&json!({"type": "user", "message": {"role": "user", "content": p}}));
        agent.hook("UserPromptSubmit", json!({"prompt": p}));
    }
    let steps = script["steps"].as_array().cloned().unwrap_or_default();
    agent.run_steps(&steps);

    let on_input = script["on_input"].as_array().cloned().unwrap_or_default();
    loop {
        agent.interrupted = false;
        let _ = std::io::stdout().write_all(b"> ");
        let _ = std::io::stdout().flush();
        let Some(line) = agent.read_line() else { break };
        out(&format!("[fake] got: {line}"));
        agent.append_transcript(
            &json!({"type": "user", "message": {"role": "user", "content": line}}),
        );
        let reply = agent.hook("UserPromptSubmit", json!({"prompt": line}));
        Agent::show_context(&reply);
        if on_input.is_empty() {
            agent.step(&json!({"turn_end": "ok"}));
        } else {
            agent.run_steps(&on_input);
        }
    }
    agent.hook("SessionEnd", json!({"reason": "other"}));
    restore_terminal();
}
