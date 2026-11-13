use std::fs::OpenOptions;
use std::io::Write;

use spandeck_archive::{Archive, Scope};

fn user(text: &str) -> String {
    serde_json::json!({"type":"user","timestamp":"2026-11-13T05:00:00Z","message":{"role":"user","content":text}}).to_string()
}
fn assistant(text: &str) -> String {
    serde_json::json!({"type":"assistant","timestamp":"2026-11-13T05:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":text}]}}).to_string()
}
fn tool_use(cmd: &str) -> String {
    serde_json::json!({"type":"assistant","timestamp":"2026-11-13T05:00:02Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":cmd}}]}}).to_string()
}
const BOUNDARY: &str = r#"{"type":"system","subtype":"compact_boundary","timestamp":"2026-11-13T05:10:00Z","compactMetadata":{"trigger":"auto","preTokens":403586}}"#;
const SUMMARY: &str = r#"{"type":"user","isCompactSummary":true,"message":{"role":"user","content":"Summary: we chose the zebra migration plan."}}"#;

fn append(path: &std::path::Path, text: &str) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

/// Phase 3 done-when: a session is archived line for line.
#[test]
fn a_session_is_archived_line_for_line_as_it_grows() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("s1.jsonl");
    let a = Archive::memory().unwrap();
    a.follow(
        "s1",
        "claude",
        file.to_str().unwrap(),
        Some("proj"),
        Some("p-1"),
    );
    assert_eq!(a.poll(), 0, "a missing file is not an error");

    let l1 = user("please add the quokka parser");
    let l2 = assistant("Adding the quokka parser now.");
    append(&file, &format!("{l1}\n{l2}\n"));
    assert_eq!(a.poll(), 2);

    // A half-written line is left for the next poll, never stored broken.
    let l3 = tool_use("cargo test quokka");
    let (head, tail) = l3.split_at(20);
    append(&file, head);
    assert_eq!(a.poll(), 0);
    append(&file, &format!("{tail}\n"));
    assert_eq!(a.poll(), 1);
    assert_eq!(a.poll(), 0);

    assert_eq!(a.raw_lines("s1"), vec![l1, l2, l3]);
    assert_eq!(a.sessions()[0].lines, 3);
}

/// Phase 0 spike, kept as a test: turns from before a compaction stay searchable.
#[test]
fn turns_before_a_compaction_remain_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("s.jsonl");
    let a = Archive::memory().unwrap();
    a.follow("s", "claude", file.to_str().unwrap(), Some("proj"), None);
    append(
        &file,
        &format!(
            "{}\n{}\n",
            user("use the heron retry policy with three attempts"),
            assistant("ok")
        ),
    );
    a.poll();
    append(
        &file,
        &format!("{BOUNDARY}\n{SUMMARY}\n{}\n", user("continue")),
    );
    a.poll();

    let hits = a.search(
        "heron retry",
        &Scope {
            session: Some("s".into()),
            ..Default::default()
        },
        10,
    );
    assert_eq!(hits.len(), 1);
    assert!(hits[0].excerpt.contains("heron"));
    assert_eq!(hits[0].ts.as_deref(), Some("2026-11-13T05:00:00Z"));
    let comp = a.search(
        "compaction",
        &Scope {
            session: Some("s".into()),
            ..Default::default()
        },
        10,
    );
    assert!(comp.iter().any(|h| h.kind == "compaction"));
}

/// Phase 6 done-when: a planted secret does not appear in recall results.
#[test]
fn a_planted_secret_never_appears_in_search_results() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("s.jsonl");
    let a = Archive::memory().unwrap();
    a.follow("s", "claude", file.to_str().unwrap(), Some("proj"), None);
    let secret = "sk-ant-api03-PLANTEDsecretPLANTEDsecret0123456789";
    append(
        &file,
        &format!(
            "{}\n{}\n",
            user(&format!("deploy with the walrus key {secret} please")),
            tool_use(&format!("export ANTHROPIC_API_KEY={secret} && make walrus"))
        ),
    );
    a.poll();
    let all = Scope {
        all_projects: true,
        ..Default::default()
    };
    let hits = a.search("walrus", &all, 10);
    assert_eq!(hits.len(), 2);
    for h in &hits {
        assert!(
            !h.excerpt.contains("PLANTED"),
            "secret leaked: {}",
            h.excerpt
        );
    }
    for h in a.around("s", 1, 5) {
        assert!(!h.excerpt.contains("PLANTED"));
    }
    // The raw copy keeps it (it never leaves the station) so resume still works.
    assert!(a.raw_lines("s")[0].contains(secret));
}

#[test]
fn scope_limits_what_an_agent_can_see() {
    let dir = tempfile::tempdir().unwrap();
    let a = Archive::memory().unwrap();
    for (id, project) in [("mine", "alpha"), ("teammate", "alpha"), ("other", "beta")] {
        let file = dir.path().join(format!("{id}.jsonl"));
        a.follow(id, "claude", file.to_str().unwrap(), Some(project), None);
        append(
            &file,
            &format!("{}\n", user(&format!("the ocelot decision from {id}"))),
        );
    }
    a.poll();
    let own = a.search(
        "ocelot",
        &Scope {
            session: Some("mine".into()),
            ..Default::default()
        },
        10,
    );
    assert_eq!(
        own.iter().map(|h| h.session.as_str()).collect::<Vec<_>>(),
        vec!["mine"]
    );
    let project = a.search(
        "ocelot",
        &Scope {
            session: Some("mine".into()),
            project: Some("alpha".into()),
            ..Default::default()
        },
        10,
    );
    assert_eq!(project.len(), 2);
    assert!(project
        .iter()
        .all(|h| h.project.as_deref() == Some("alpha")));
    let all = a.search(
        "ocelot",
        &Scope {
            all_projects: true,
            ..Default::default()
        },
        10,
    );
    assert_eq!(all.len(), 3);
    assert!(
        a.search("ocelot", &Scope::default(), 10).is_empty(),
        "no scope, no results"
    );
}

#[test]
fn a_deleted_session_file_can_be_put_back() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("gone.jsonl");
    let a = Archive::memory().unwrap();
    a.follow("gone", "claude", file.to_str().unwrap(), None, None);
    let body = format!("{}\n{}\n", user("first"), assistant("second"));
    append(&file, &body);
    a.poll();
    assert!(a.restore("gone").is_err(), "never overwrites a live file");
    std::fs::remove_file(&file).unwrap();
    a.restore("gone").unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), body);
}

#[test]
fn codex_rollout_lines_normalize() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("rollout.jsonl");
    let a = Archive::memory().unwrap();
    a.follow("cx", "codex", file.to_str().unwrap(), Some("proj"), None);
    append(
        &file,
        concat!(
            r#"{"timestamp":"2026-11-13T05:00:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"review the pangolin diff"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-11-13T05:00:01Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"git\",\"diff\"]}"}}"#,
            "\n",
            r#"{"timestamp":"2026-11-13T05:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"The pangolin diff looks fine."}]}}"#,
            "\n"
        ),
    );
    assert_eq!(a.poll(), 3);
    let hits = a.search(
        "pangolin",
        &Scope {
            project: Some("proj".into()),
            ..Default::default()
        },
        10,
    );
    assert_eq!(hits.len(), 2);
}
