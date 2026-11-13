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
//!   {"work": ms}                         a long tool call; ESC interrupts it
//!   {"turn_end": "last message"}         Stop (may be continued with mail)
//!   {"rpc": {"method":"…","params":{}}}  call the daemon as this pane, print the reply
//!   {"task_result": {"summary":"…","findings_by_round":[["a"],[]]}}
//!   {"sh": "command"}                    run a shell command in the cwd
//!   {"transcript": {…}}                  append a line to the session file
//!   {"repeat": n, "steps": [...]}
//!   {"exit": code}
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
}

fn out(s: &str) {
    let mut o = std::io::stdout();
    let _ = o.write_all(s.replace('\n', "\r\n").as_bytes());
    let _ = o.write_all(b"\r\n");
    let _ = o.flush();
}

impl Agent {
    fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut c = Client::from_env(&self.paths).map_err(|e| e.to_string())?;
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
        match self.rpc("hook.ingest", json!({"agent": "fake", "payload": extra})) {
            Ok(reply) => reply["stdout"].as_str().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null),
            Err(e) => {
                out(&format!("[fake] hook error: {e}"));
                Value::Null
            }
        }
    }

    fn show_context(reply: &Value) {
        if let Some(ctx) = reply.pointer("/hookSpecificOutput/additionalContext").and_then(|c| c.as_str()) {
            out(&format!("[fake] context: {}", ctx.replace('\n', " | ")));
        }
    }

    fn append_transcript(&self, line: &Value) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.transcript) {
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
        loop {
            match self.input.recv() {
                Ok(b'\r') | Ok(b'\n') => {
                    if line.is_empty() {
                        continue;
                    }
                    let _ = std::io::stdout().write_all(b"\r\n");
                    let _ = std::io::stdout().flush();
                    return Some(String::from_utf8_lossy(&line).to_string());
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
            self.append_transcript(&json!({"type": "user", "message": {"role": "user", "content": text}}));
            let reply = self.hook("UserPromptSubmit", json!({"prompt": text}));
            Self::show_context(&reply);
        } else if let Some(tool) = step.get("tool") {
            self.tool(tool);
        } else if let Some(perm) = step.get("permission") {
            let name = perm["name"].as_str().unwrap_or("Write");
            let input = json!({"file_path": perm["file"], "command": perm["command"]});
            out("[fake] permission?");
            let reply = self.hook("PermissionRequest", json!({"tool_name": name, "tool_input": input}));
            let behavior = reply.pointer("/hookSpecificOutput/decision/behavior").and_then(|b| b.as_str()).unwrap_or("none");
            out(&format!("[fake] permission: {behavior}"));
        } else if let Some(ms) = step.get("work").and_then(|s| s.as_u64()) {
            self.hook("PreToolUse", json!({"tool_name": "Bash", "tool_input": {"command": format!("sleep {ms}ms")}, "tool_use_id": "work"}));
            out("[fake] working (esc to interrupt)");
            if self.wait_or_escape(ms) {
                out("[fake] interrupted");
                self.hook("Stop", json!({"last_assistant_message": "interrupted", "stop_hook_active": false}));
                self.interrupted = true;
            } else {
                let post = self.hook("PostToolUse", json!({"tool_name": "Bash", "tool_input": {"command": format!("sleep {ms}ms")}, "tool_use_id": "work", "tool_response": {}}));
                out("[fake] work finished");
                Self::show_context(&post);
            }
        } else if let Some(msg) = step.get("turn_end").and_then(|s| s.as_str()) {
            self.append_transcript(&json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": msg}]}}));
            let reply = self.hook("Stop", json!({"last_assistant_message": msg, "stop_hook_active": false}));
            if reply["decision"] == json!("block") {
                out(&format!("[fake] CONTINUE: {}", reply["reason"].as_str().unwrap_or("")));
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
                let idx = self.round.saturating_sub(1).min(by_round.len().saturating_sub(1));
                params["findings"] = by_round.get(idx).cloned().unwrap_or(json!([]));
            }
            match self.rpc("task.result", params) {
                Ok(v) => out(&format!("[fake] task.result -> {}", v["state"])),
                Err(e) => out(&format!("[fake] task.result ERROR: {e}")),
            }
        } else if let Some(cmd) = step.get("sh").and_then(|s| s.as_str()) {
            let status = std::process::Command::new("sh").arg("-c").arg(cmd).current_dir(&self.cwd).stdin(std::process::Stdio::null()).output();
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
        } else if let Some(code) = step.get("exit").and_then(|s| s.as_i64()) {
            self.hook("SessionEnd", json!({"reason": "other"}));
            restore_terminal();
            std::process::exit(code as i32);
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
        let pre = self.hook("PreToolUse", json!({"tool_name": name, "tool_input": input, "tool_use_id": id}));
        if pre.pointer("/hookSpecificOutput/permissionDecision") == Some(&json!("deny")) {
            out(&format!(
                "[fake] BLOCKED {name}: {}",
                pre.pointer("/hookSpecificOutput/permissionDecisionReason").and_then(|r| r.as_str()).unwrap_or("")
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
        if let (Some(file), Some(content)) = (tool.get("file").and_then(|f| f.as_str()), tool.get("content").and_then(|c| c.as_str())) {
            let path = if file.starts_with('/') { file.to_string() } else { format!("{}/{file}", self.cwd) };
            let _ = std::fs::write(path, content);
        }
        out(&format!("[fake] ran {name} ({})", if ok { "ok" } else { "failed" }));
        let post = if ok {
            self.hook("PostToolUse", json!({"tool_name": name, "tool_input": input, "tool_use_id": id, "tool_response": {}, "duration_ms": 5}))
        } else {
            self.hook("PostToolUseFailure", json!({"tool_name": name, "tool_input": input, "tool_use_id": id, "error": "Exit code 1", "duration_ms": 5}))
        };
        Self::show_context(&post);
    }
}

static SAVED: std::sync::Mutex<Option<libc::termios>> = std::sync::Mutex::new(None);

fn raw_terminal() {
    // SAFETY: plain termios calls on stdin with a zeroed, then filled, struct.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) == 0 {
            *SAVED.lock().unwrap() = Some(t);
            t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
            libc::tcsetattr(0, libc::TCSANOW, &t);
        }
    }
}

fn restore_terminal() {
    if let Some(t) = *SAVED.lock().unwrap() {
        // SAFETY: restores the attributes raw_terminal saved.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let script: Value = arg("--script")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({"steps": []}));
    let resume = arg("--resume");
    let prompt = arg("--prompt");
    let session = resume.clone().unwrap_or_else(|| spandeck_core::paths::new_id("fake"));
    let dir = std::env::var("SPANDECK_FAKE_DIR").unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();

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
    };

    match &resume {
        Some(id) => out(&format!("[fake] RESUMED {id}")),
        None => out(&format!("[fake] session {}", agent.session)),
    }
    let start = agent.hook("SessionStart", json!({"source": if resume.is_some() { "resume" } else { "startup" }}));
    Agent::show_context(&start);
    if let Some(p) = &prompt {
        out(&format!("[fake] prompt: {}", p.replace('\n', " | ")));
        agent.append_transcript(&json!({"type": "user", "message": {"role": "user", "content": p}}));
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
        agent.append_transcript(&json!({"type": "user", "message": {"role": "user", "content": line}}));
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
