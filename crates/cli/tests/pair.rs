//! The pair recipe (`spandeck project new --recipe pair`): a builder and a
//! critic set up in one folder, as the owner did by hand on 18 Nov 2026.
//! These are the acceptance criteria of that feature.
mod common;
use std::path::Path;
use std::process::Stdio;

use common::*;
use serde_json::{json, Value};

/// What the owner has sent, by whom it is for.
fn roles_sent(s: &Station, project: &str) -> Vec<(String, String)> {
    let mut sent: Vec<(String, String)> = s
        .call("msg.recent", json!({"limit": 200}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| {
            m["from"] == json!("you@alpha")
                && m["to"]
                    .as_str()
                    .is_some_and(|to| to.starts_with(&format!("{project}/")))
        })
        .map(|m| {
            (
                m["to"].as_str().unwrap().replace("@alpha", ""),
                m["body"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    sent.sort();
    sent
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"))
}

#[test]
fn the_pair_recipe_sets_up_a_builder_and_a_critic_once() {
    let s = Station::start("alpha");

    // The command says what recipes there are.
    let out = s
        .command(SPANDECK)
        .args(["project", "recipes"])
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    let list = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        list.starts_with("pair\n") && list.contains("--recipe pair") && list.contains("--critic"),
        "{list}"
    );

    // A folder that is not there, a brief in a file, by the command.
    let dir = s.root.join("shop");
    let brief = s.root.join("brief.txt");
    std::fs::write(&brief, "A shop for one kind of tea.\n").unwrap();
    let out = s
        .command(SPANDECK)
        .args(["--json", "project", "new", "--recipe", "pair", "--path"])
        .arg(&dir)
        .args(["--builder", "fake", "--critic", "fake", "--brief"])
        .arg(&brief)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["name"], json!("shop"));
    assert_eq!(
        v["recipe"]["written"],
        json!([".spandeck/project.toml", "PROCESS.md", "BRIEF.md"])
    );
    assert_eq!(v["recipe"]["roles"], json!("sent"), "{v}");

    // The files: the process names the project and both programs and
    // begins with stage 0; the brief is the owner's.
    let process = read(&dir, "PROCESS.md");
    for want in [
        "project `shop`",
        "**builder** (Fake agent)",
        "**critic** (Fake agent)",
        "0. **Tools**",
    ] {
        assert!(process.contains(want), "{want}: {process}");
    }
    assert!(!process.contains("{{"), "{process}");
    assert_eq!(read(&dir, "BRIEF.md"), "A shop for one kind of tea.\n");

    // A git repository with one commit: those files, by spandeck, since
    // nobody is configured here, and nothing was configured for it.
    assert_eq!(git(&dir, &["rev-list", "--count", "HEAD"]).trim(), "1");
    assert_eq!(
        git(&dir, &["log", "-1", "--format=%an <%ae>"]).trim(),
        "spandeck <spandeck@localhost>"
    );
    let mut committed: Vec<String> = git(&dir, &["show", "--name-only", "--format=", "HEAD"])
        .lines()
        .map(String::from)
        .collect();
    committed.sort();
    assert_eq!(
        committed,
        [".spandeck/project.toml", "BRIEF.md", "PROCESS.md"]
    );
    assert!(!s.root.join("home/.gitconfig").exists());
    assert!(!read(&dir, ".git/config").contains("spandeck@localhost"));

    // Two panes, builder and critic, in the same folder; the roles are
    // placed in the project's own tree.
    let project = s.call("project.get", json!({"name": "shop"}));
    let mut panes: Vec<(String, String, String)> = project["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_string(),
                p["role"].as_str().unwrap_or_default().to_string(),
                p["cwd"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    panes.sort();
    let at = project["path"].as_str().unwrap().to_string();
    assert_eq!(
        panes,
        [
            ("builder".to_string(), "builder".to_string(), at.clone()),
            ("critic".to_string(), "critic".to_string(), at.clone()),
        ]
    );
    for role in ["builder", "critic"] {
        assert_eq!(
            project["definition"]["roles"][role]["placement"],
            json!("same-tree"),
            "{project}"
        );
    }

    // Each was sent its role by the owner, once.
    let sent = roles_sent(&s, "shop");
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[0].0, "shop/builder");
    assert!(
        sent[0]
            .1
            .starts_with("You are the builder in this project. Read BRIEF.md and PROCESS.md")
            && sent[0].1.contains("the pane named critic"),
        "{}",
        sent[0].1
    );
    assert_eq!(sent[1].0, "shop/critic");
    assert!(
        sent[1].1.starts_with("You are the critic in this project.")
            && sent[1].1.contains("the pane named builder"),
        "{}",
        sent[1].1
    );

    // Run again on the same folder, with another brief: nothing is written
    // over, nothing is committed, and nobody is told twice.
    std::fs::write(
        dir.join("PROCESS.md"),
        format!("{process}\nOur own rule.\n"),
    )
    .unwrap();
    let again = s.call(
        "project.new",
        json!({"path": dir, "recipe": "pair", "builder": "fake", "critic": "fake",
               "brief": "Something else.", "open": true}),
    );
    assert_eq!(again["recipe"]["written"], json!([]), "{again}");
    assert_eq!(again["recipe"]["roles"], json!("sent before"));
    assert!(read(&dir, "PROCESS.md").ends_with("Our own rule.\n"));
    assert_eq!(read(&dir, "BRIEF.md"), "A shop for one kind of tea.\n");
    assert_eq!(git(&dir, &["rev-list", "--count", "HEAD"]).trim(), "1");
    assert_eq!(roles_sent(&s, "shop"), sent);
    assert_eq!(
        s.call("project.get", json!({"name": "shop"}))["panes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // A folder with the owner's own PROCESS.md and no brief: the file is
    // kept, BRIEF.md asks for a brief, the panes run, nobody is told to
    // start. Once a brief is written, opening the project sends the roles,
    // and opening it again does not.
    let own = s.root.join("own");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join("PROCESS.md"), "How we work here.\n").unwrap();
    let v = s.call(
        "project.new",
        json!({"path": own, "recipe": "pair", "builder": "fake", "critic": "fake", "open": true}),
    );
    assert_eq!(v["recipe"]["kept"], json!(["PROCESS.md"]), "{v}");
    assert_eq!(v["recipe"]["roles"], json!("waiting for a brief"));
    assert!(v["recipe"]["next"]
        .as_str()
        .unwrap()
        .contains("spandeck project open own"));
    assert_eq!(read(&own, "PROCESS.md"), "How we work here.\n");
    assert!(read(&own, "BRIEF.md").contains("Nothing is written here yet"));
    assert_eq!(v["panes"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(roles_sent(&s, "own"), []);
    // The first commit holds what the recipe wrote, not the owner's file.
    assert!(!git(&own, &["show", "--name-only", "--format=", "HEAD"]).contains("PROCESS.md"));

    std::fs::write(own.join("BRIEF.md"), "A page that tells the time.\n").unwrap();
    let opened = s.call("project.open", json!({"name": "own"}));
    assert_eq!(opened["recipe"]["roles"], json!("sent"), "{opened}");
    let told = roles_sent(&s, "own");
    assert_eq!(told.len(), 2, "{told:?}");
    let opened = s.call("project.open", json!({"name": "own"}));
    assert_eq!(opened["recipe"]["roles"], json!("sent before"));
    assert_eq!(roles_sent(&s, "own"), told);
    // A project that is no pair is opened as before.
    let plain = s.git_repo("plain");
    let v = s.call("project.new", json!({"path": plain, "open": true}));
    assert!(v["recipe"].is_null(), "{v}");

    // Rule 7: an agent cannot set a project up, with a recipe or without;
    // and what is no recipe, or no agent program, makes nothing.
    let builder = project["panes"][0]["id"].as_str().unwrap();
    let never = s.root.join("never");
    let refused = s
        .call_as_pane(
            builder,
            "project.new",
            json!({"path": never, "recipe": "pair", "builder": "fake", "critic": "fake"}),
        )
        .unwrap_err();
    assert_eq!(refused.code, "denied", "{}", refused.message);
    for params in [
        json!({"path": never, "recipe": "trio"}),
        json!({"path": never, "recipe": "pair", "builder": "shell"}),
        json!({"path": never, "recipe": "pair", "critic": "gpt"}),
    ] {
        let refused = s.try_call("project.new", params).unwrap_err();
        assert_eq!(refused.code, "invalid", "{}", refused.message);
    }
    assert!(!never.exists());
}
