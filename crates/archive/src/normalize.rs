//! Turn each vendor's session-file line into one shape.
//!
//! Neither format is a stable contract. Raw lines are kept beside the
//! normalised copy so a fixed parser can re-read old sessions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// ISO timestamp from the line, when it has one.
    pub ts: Option<String>,
    /// user, assistant, tool, system.
    pub role: String,
    /// prompt, reply, tool_call, tool_result, compaction, summary.
    pub kind: String,
    pub text: String,
    pub tool: Option<String>,
    pub files: Vec<String>,
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|b| {
                b.get("text")
                    .or_else(|| b.get("content"))
                    .map(text_of)
                    .filter(|s| !s.is_empty())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => v
            .get("text")
            .or_else(|| v.get("content"))
            .map(text_of)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn files_of(input: &Value) -> Vec<String> {
    ["file_path", "path", "notebook_path"]
        .iter()
        .filter_map(|k| {
            input
                .get(*k)
                .and_then(|p| p.as_str())
                .map(|p| p.to_string())
        })
        .collect()
}

fn tool_text(name: &str, input: &Value) -> String {
    if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
        return format!("{name}: {cmd}");
    }
    let brief = input.to_string();
    format!("{name}: {brief}")
}

/// Claude Code `~/.claude/projects/<dir>/<session>.jsonl` (checked on 2.1.323).
pub fn claude_line(raw: &str) -> Vec<Entry> {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return vec![];
    };
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string());
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let mk =
        |role: &str, kind: &str, text: String, tool: Option<String>, files: Vec<String>| Entry {
            ts: ts.clone(),
            role: role.into(),
            kind: kind.into(),
            text,
            tool,
            files,
        };
    match ty {
        "system" if v.get("subtype").and_then(|s| s.as_str()) == Some("compact_boundary") => {
            let meta = v.get("compactMetadata").cloned().unwrap_or(Value::Null);
            vec![mk(
                "system",
                "compaction",
                format!("compaction {meta}"),
                None,
                vec![],
            )]
        }
        "user" => {
            let content = v
                .pointer("/message/content")
                .cloned()
                .unwrap_or(Value::Null);
            if v.get("isCompactSummary").and_then(|b| b.as_bool()) == Some(true) {
                return vec![mk("system", "summary", text_of(&content), None, vec![])];
            }
            match &content {
                Value::String(s) => vec![mk("user", "prompt", s.clone(), None, vec![])],
                Value::Array(blocks) => blocks
                    .iter()
                    .filter_map(|b| match b.get("type").and_then(|t| t.as_str()) {
                        Some("text") => Some(mk("user", "prompt", text_of(b), None, vec![])),
                        Some("tool_result") => Some(mk(
                            "tool",
                            "tool_result",
                            b.get("content").map(text_of).unwrap_or_default(),
                            None,
                            vec![],
                        )),
                        _ => None,
                    })
                    .filter(|e| !e.text.is_empty())
                    .collect(),
                _ => vec![],
            }
        }
        "assistant" => v
            .pointer("/message/content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| match b.get("type").and_then(|t| t.as_str()) {
                        Some("text") => Some(mk("assistant", "reply", text_of(b), None, vec![])),
                        Some("tool_use") => {
                            let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                            let input = b.get("input").cloned().unwrap_or(Value::Null);
                            Some(mk(
                                "assistant",
                                "tool_call",
                                tool_text(name, &input),
                                Some(name.to_string()),
                                files_of(&input),
                            ))
                        }
                        _ => None,
                    })
                    .filter(|e| !e.text.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        _ => vec![],
    }
}

/// Codex `~/.codex/sessions/.../rollout-*.jsonl`.
/// UNVERIFIED: written from the public rollout format, not from a real file.
pub fn codex_line(raw: &str) -> Vec<Entry> {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return vec![];
    };
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string());
    let payload = v.get("payload").unwrap_or(&v);
    let mk = |role: &str, kind: &str, text: String, tool: Option<String>| Entry {
        ts: ts.clone(),
        role: role.into(),
        kind: kind.into(),
        text,
        tool,
        files: vec![],
    };
    let out = match payload.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "message" => {
            let role = payload
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("assistant");
            // What Codex tells itself (seen in 0.169.0) is not conversation.
            if matches!(role, "developer" | "system") {
                return vec![];
            }
            let text = payload.get("content").map(text_of).unwrap_or_default();
            let kind = if role == "user" { "prompt" } else { "reply" };
            vec![mk(
                if role == "user" { "user" } else { "assistant" },
                kind,
                text,
                None,
            )]
        }
        "function_call" | "local_shell_call" | "custom_tool_call" => {
            let name = payload
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("shell");
            let args = payload
                .get("arguments")
                .or_else(|| payload.get("input"))
                .or_else(|| payload.get("action"))
                .map(|a| {
                    a.as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| a.to_string())
                })
                .unwrap_or_default();
            vec![mk(
                "assistant",
                "tool_call",
                format!("{name}: {args}"),
                Some(name.to_string()),
            )]
        }
        "function_call_output" | "custom_tool_call_output" => {
            let out = payload
                .get("output")
                .map(|o| {
                    o.as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| text_of(o))
                })
                .unwrap_or_default();
            vec![mk("tool", "tool_result", out, None)]
        }
        "compacted" => vec![mk(
            "system",
            "compaction",
            payload
                .get("message")
                .map(text_of)
                .unwrap_or_else(|| "compaction".into()),
            None,
        )],
        _ => vec![],
    };
    out.into_iter().filter(|e| !e.text.is_empty()).collect()
}

/// A row of OpenCode's database as the archive keeps it (stores.rs).
pub fn opencode_line(raw: &str) -> Vec<Entry> {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return vec![];
    };
    let data = &v["data"];
    let mk =
        |role: &str, kind: &str, text: String, tool: Option<String>, files: Vec<String>| Entry {
            ts: None,
            role: role.into(),
            kind: kind.into(),
            text,
            tool,
            files,
        };
    let out: Vec<Entry> = match v["type"].as_str().unwrap_or("") {
        "user" => vec![mk("user", "prompt", text_of(&data["text"]), None, vec![])],
        "assistant" => data["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| match p["type"].as_str() {
                        Some("text") => {
                            Some(mk("assistant", "reply", text_of(&p["text"]), None, vec![]))
                        }
                        Some("tool") => {
                            let name = p["name"].as_str().unwrap_or("tool");
                            let input = &p["state"]["input"];
                            Some(mk(
                                "assistant",
                                "tool_call",
                                tool_text(name, input),
                                Some(name.to_string()),
                                files_of(input),
                            ))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => vec![],
    };
    out.into_iter().filter(|e| !e.text.is_empty()).collect()
}

/// A step of the Antigravity CLI's transcript (`agy` 1.5.9).
pub fn antigravity_line(raw: &str) -> Vec<Entry> {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return vec![];
    };
    let ts = v["created_at"].as_str().map(String::from);
    let mk =
        |role: &str, kind: &str, text: String, tool: Option<String>, files: Vec<String>| Entry {
            ts: ts.clone(),
            role: role.into(),
            kind: kind.into(),
            text,
            tool,
            files,
        };
    let content = v["content"].as_str().unwrap_or("");
    let mut out = vec![];
    match v["type"].as_str().unwrap_or("") {
        "USER_INPUT" => {
            // The owner's words are between the program's own tags.
            let said = content
                .split_once("<USER_REQUEST>")
                .and_then(|(_, rest)| rest.split_once("</USER_REQUEST>"))
                .map_or(content, |(said, _)| said);
            out.push(mk("user", "prompt", said.trim().to_string(), None, vec![]));
        }
        "PLANNER_RESPONSE" => {
            out.push(mk(
                "assistant",
                "reply",
                content.trim().to_string(),
                None,
                vec![],
            ));
            for call in v["tool_calls"].as_array().into_iter().flatten() {
                let name = call["name"].as_str().unwrap_or("tool");
                let args = &call["args"];
                let text = match args["CommandLine"].as_str() {
                    Some(cmd) => format!("{name}: {cmd}"),
                    None => format!("{name}: {args}"),
                };
                let files = args["Path"]
                    .as_str()
                    .map(String::from)
                    .into_iter()
                    .collect();
                out.push(mk(
                    "assistant",
                    "tool_call",
                    text,
                    Some(name.to_string()),
                    files,
                ));
            }
        }
        "GENERIC" => out.push(mk("tool", "tool_result", content.to_string(), None, vec![])),
        _ => {}
    }
    out.into_iter().filter(|e| !e.text.is_empty()).collect()
}

pub fn normalize(agent: &str, raw: &str) -> Vec<Entry> {
    match agent {
        "codex" => codex_line(raw),
        "opencode" => opencode_line(raw),
        "antigravity" => antigravity_line(raw),
        // Claude Code and the fake agent share a format.
        _ => claude_line(raw),
    }
}
