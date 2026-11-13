//! Risk: a vendor changes its format. Real (Claude Code) and documented
//! (Codex) hook payloads, checked against the normalized output.

use std::fs;
use std::path::Path;

use serde_json::{json, Value};
use spandeck_agents::{adapter, AgentKind, AgentState, EventKind, HookDecision};

fn normalize_dir(kind: AgentKind, dir: &str) -> Vec<Value> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(dir);
    let mut names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    let a = adapter(kind).unwrap();
    names
        .iter()
        .map(|name| {
            let payload: Value = serde_json::from_str(&fs::read_to_string(dir.join(name)).unwrap()).unwrap();
            let ev = a.normalize(&payload).unwrap_or_else(|| panic!("{name} did not normalize"));
            json!({"fixture": name, "event": ev})
        })
        .collect()
}

#[test]
fn claude_code_2_1_323_fixtures_normalize() {
    insta::assert_json_snapshot!(normalize_dir(AgentKind::Claude, "claude-code/2.1.323"));
}

#[test]
fn codex_documented_fixtures_normalize() {
    insta::assert_json_snapshot!(normalize_dir(AgentKind::Codex, "codex/documented"));
}

#[test]
fn a_real_claude_turn_walks_the_expected_states() {
    let events = normalize_dir(AgentKind::Claude, "claude-code/2.1.323");
    let kinds: Vec<EventKind> = events.iter().map(|e| serde_json::from_value(e["event"]["kind"].clone()).unwrap()).collect();
    assert_eq!(kinds.first(), Some(&EventKind::SessionStart));
    assert!(kinds.contains(&EventKind::ToolStart) && kinds.contains(&EventKind::TurnEnd));
    // The failed `false` command is a ToolEnd with ok=false.
    let failed = events.iter().find(|e| e["fixture"] == "08-PostToolUseFailure.json").unwrap();
    assert_eq!(failed["event"]["tool"]["ok"], json!(false));
    assert_eq!(failed["event"]["tool"]["command"], json!("false"));
    // A permission request means the agent needs you.
    let perm = events.iter().find(|e| e["fixture"] == "12-PermissionRequest.json").unwrap();
    assert_eq!(perm["event"]["state"], json!(AgentState::NeedsYou));
    assert_eq!(perm["event"]["tool"]["writes"], json!(true));
}

/// These three reply shapes were accepted by the real Claude Code 2.1.323.
#[test]
fn replies_match_the_shapes_verified_live() {
    let a = adapter(AgentKind::Claude).unwrap();
    let deny = a.reply("PreToolUse", &HookDecision::deny("Blocked by standing order: never push to main"));
    assert_eq!(
        serde_json::from_str::<Value>(&deny.stdout).unwrap(),
        json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny",
               "permissionDecisionReason": "Blocked by standing order: never push to main"}})
    );
    let allow = a.reply("PermissionRequest", &HookDecision::allow());
    assert_eq!(
        serde_json::from_str::<Value>(&allow.stdout).unwrap(),
        json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}})
    );
    let ctx = a.reply("SessionStart", &HookDecision::context("digest"));
    assert_eq!(
        serde_json::from_str::<Value>(&ctx.stdout).unwrap(),
        json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "digest"}})
    );
    // No decision means no output at all: the agent behaves as if unhooked.
    assert_eq!(a.reply("PreToolUse", &HookDecision::none()).stdout, "");
    assert_eq!(a.reply("PermissionRequest", &HookDecision::none()).stdout, "");
}

#[test]
fn launch_and_resume_commands() {
    use spandeck_agents::LaunchOpts;
    let c = adapter(AgentKind::Claude).unwrap().launch(&LaunchOpts { resume: Some("abc".into()), ..Default::default() });
    assert_eq!((c.program.as_str(), c.args), ("claude", vec!["--resume".to_string(), "abc".to_string()]));
    let x = adapter(AgentKind::Codex).unwrap().launch(&LaunchOpts { resume: Some("abc".into()), ..Default::default() });
    assert_eq!((x.program.as_str(), x.args), ("codex", vec!["resume".to_string(), "abc".to_string()]));
}
