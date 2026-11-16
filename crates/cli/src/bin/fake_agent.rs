//! A scriptable stand-in for a coding agent, for scenario tests.
//!
//! It speaks Claude Code's hook dialect to the daemon over the local socket,
//! prints what happens on its terminal, and reads input like an agent prompt.
//!
//!   spandeck-fake-agent [--script file.json] [--resume <session>] [--prompt <text>]
//!
//! Script: {"steps": [...], "on_input": [...]}. Steps:
//!   {"say": "text"}                      print a line
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
//!   {"turn_end": "last message"}         Stop (may be continued with mail)
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
//! Beside the steps, `"hook_deadline"`: how long this agent gives a hook
//! that can block, in seconds, and tells it (default: what spandeck
//! installs now, 75 for a tool call); `"old"`: 10 seconds and the hook is
//! told nothing, as in a session started before 16 Nov 2026;
//! `"question_deadline"`: what the hook that may wait for the owner is
//! given, in seconds (default: what spandeck installs, an hour).
//! `"trust_screen"` beside the steps: the same, shown before the session
//! starts, where the real programs ask.
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
        let obj = extra.as_object_mut().expect("hook payload is an object");
        obj.insert("hook_event_name".into(), json!(event));
        obj.insert("session_id".into(), json!(self.session));
        obj.insert("transcript_path".into(), json!(self.transcript));
        obj.insert("cwd".into(), json!(self.cwd));
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

    fn append_transcript(&self, line: &Value) {
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
            self.hook(
                if stop { "SubagentStop" } else { "SubagentStart" },
                json!({"agent_type": sub.get("type").and_then(|t| t.as_str()).unwrap_or("Explore")}),
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
        } else if let Some(n) = step.get("repeat").and_then(|s| s.as_u64()) {
            let steps = step["steps"].as_array().cloned().unwrap_or_default();
            for _ in 0..n {
                self.run_steps(&steps);
            }
        } else if let Some(spec) = step.get("trust_screen") {
            self.trust_screen(spec);
        } else if let Some(code) = step.get("exit").and_then(|s| s.as_i64()) {
            self.hook("SessionEnd", json!({"reason": "other"}));
            restore_terminal();
            std::process::exit(code as i32);
        }
    }

    /// A program's folder-trust screen (see the top of this file).
    fn trust_screen(&mut self, spec: &Value) {
        use spandeck_agents::foldertrust::{drawn, Focus};
        let program = spec["as"].as_str().unwrap_or("claude");
        if let Some(ms) = spec["after_ms"].as_u64() {
            std::thread::sleep(Duration::from_millis(ms));
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
                "codex-hooks" => drawn::codex_hooks(),
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
                b'\r' | b'\n' if program == "codex-hooks" => {
                    clear();
                    out("[fake] hooks screen answered");
                    return;
                }
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
        hook_deadline: script["hook_deadline"].clone(),
        question_deadline: script["question_deadline"].as_u64(),
        paste: script["paste"].as_bool().unwrap_or(false),
        drop_enters: script["drop_enters"].as_u64().unwrap_or(0),
    };
    if agent.paste {
        let _ = std::io::stdout().write_all(b"\x1b[?2004h");
    }

    // Where the real programs ask about the folder: before anything else.
    if let Some(spec) = script.get("trust_screen") {
        agent.trust_screen(spec);
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
