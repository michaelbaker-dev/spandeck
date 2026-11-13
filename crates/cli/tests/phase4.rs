//! Phase 4 acceptance: projects.
//! Done when: a project created, closed and reopened restores its panes and agents.
mod common;
use common::*;
use serde_json::json;

#[test]
fn a_project_created_closed_and_reopened_restores_its_panes_and_agents() {
    let s = Station::start("alpha");
    let repo = s.git_repo("demo");
    let script = s.script_file(&json!({"steps": []}));
    let v = s.call(
        "project.new",
        json!({"path": repo, "name": "demo", "open": true, "panes": [
            {"name": "dev", "agent": "fake", "role": "author", "args": ["--script", script]},
            {"name": "term", "agent": "shell"}
        ]}),
    );
    assert_eq!(v["address"], json!("demo@alpha"));
    assert_eq!(v["started"].as_array().unwrap().len(), 2);
    // The definition lives in the repo, so it travels through git.
    let toml = std::fs::read_to_string(repo.join(".spandeck/project.toml")).unwrap();
    assert!(toml.contains("name = \"demo\"") && toml.contains("agent = \"fake\""), "{toml}");

    let dev = s.pane("dev");
    let dev_id = dev["id"].as_str().unwrap().to_string();
    assert_eq!(dev["project"], json!("demo"));
    assert_eq!(dev["cwd"].as_str().unwrap(), repo.canonicalize().unwrap().to_str().unwrap());
    s.wait_text(&dev_id, "[fake] session");
    let session = wait_for("session id", 10, || s.pane(&dev_id)["session_id"].as_str().map(String::from));

    s.call("project.close", json!({"name": "demo"}));
    assert!(s.call("pane.list", json!({})).as_array().unwrap().is_empty());
    assert_eq!(s.call("project.get", json!({"name": "demo"}))["open"], json!(false));

    // Reopen: the agent resumes its conversation; the shell starts fresh.
    let v = s.call("project.open", json!({"name": "demo"}));
    let started = v["started"].as_array().unwrap();
    assert_eq!(started.len(), 2);
    let dev2 = s.pane("dev")["id"].as_str().unwrap().to_string();
    s.wait_text(&dev2, &format!("[fake] RESUMED {session}"));
    assert_eq!(s.pane("term")["agent"], json!("shell"));
    // Opening twice starts nothing twice.
    assert!(s.call("project.open", json!({"name": "demo"}))["started"].as_array().unwrap().is_empty());
}

#[test]
fn a_folder_that_already_has_a_definition_is_registered_as_it_is() {
    let s = Station::start("alpha");
    let repo = s.git_repo("kept");
    std::fs::create_dir_all(repo.join(".spandeck")).unwrap();
    std::fs::write(repo.join(".spandeck/project.toml"), "name = \"kept\"\n\n[[pane]]\nname = \"sh\"\nagent = \"shell\"\n\n[roles.reviewer]\nagent = \"fake\"\nplacement = \"snapshot\"\n").unwrap();
    let v = s.call("project.new", json!({"path": repo}));
    assert_eq!(v["name"], json!("kept"));
    assert_eq!(v["definition"]["roles"]["reviewer"]["placement"], json!("snapshot"));
    // A second project cannot take the same name from another folder.
    let other = s.git_repo("other");
    let err = s.try_call("project.new", json!({"path": other, "name": "kept"})).unwrap_err();
    assert!(err.message.contains("already exists"), "{}", err.message);
}

#[test]
fn the_command_line_creates_a_project_in_the_current_folder() {
    let s = Station::start("alpha");
    let repo = s.git_repo("cli-proj");
    let out = std::process::Command::new(SPANDECK)
        .args(["project", "new", "--pane", "sh:shell"])
        .current_dir(&repo)
        .env("SPANDECK_STATE_DIR", s.root.join("state"))
        .env("SPANDECK_CONFIG_DIR", s.root.join("config"))
        .env_remove("SPANDECK_TOKEN")
        .env_remove("SPANDECK_PANE")
        .env_remove("SPANDECK_SOCKET")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("cli-proj@alpha"));
    assert_eq!(s.pane("sh")["project"], json!("cli-proj"));
}
