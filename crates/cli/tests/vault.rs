//! AgentVault integration (docs/specs/spandeck-agentvault.md), against a fake
//! vault that follows the contract. Phases 1 to 3 of the spandeck side.
mod common;
use std::time::Duration;

use common::fake_vault::FakeVault;
use common::*;
use serde_json::{json, Value};

const KEY: &str = "sk-ant-api03-PLANTEDplantedPLANTEDplanted0123456789";

fn station(vault: &FakeVault) -> Station {
    let s = Station::start_with("alpha", &vault.config(), &[]);
    wait_for("the vault to be found", 10, || {
        (s.call("vault.refresh", json!({}))["found"] == json!(true)).then_some(())
    });
    s
}

fn connect(s: &Station, vault: &FakeVault) -> String {
    s.call("vault.connect", json!({}));
    wait_for("the request to reach the vault", 10, || {
        (vault.decide(true) > 0).then_some(())
    });
    wait_for("the station to collect its token", 10, || {
        (s.call("vault.status", json!({}))["state"] == json!("connected")).then_some(())
    });
    std::fs::read_to_string(s.root.join("state/vault.token")).unwrap()
}

fn project(s: &Station, keys: &str) -> std::path::PathBuf {
    let repo = s.git_repo("demo");
    std::fs::create_dir_all(repo.join(".spandeck")).unwrap();
    std::fs::write(
        repo.join(".spandeck/project.toml"),
        format!("name = \"demo\"\n\n[vault]\nkeys = {keys}\n"),
    )
    .unwrap();
    s.call("project.new", json!({"path": repo}));
    repo
}

fn everything_the_api_says(s: &Station) -> String {
    let mut all = String::new();
    for (m, p) in [
        ("station.info", json!({})),
        ("vault.status", json!({})),
        ("log.query", json!({"limit": 2000})),
        ("pane.list", json!({})),
        ("fleet.view", json!({})),
        ("notify.list", json!({})),
    ] {
        all.push_str(&s.call(m, p).to_string());
    }
    all
}

#[test]
fn a_station_connects_only_when_the_owner_approves_on_the_vault() {
    let vault = FakeVault::start();
    let s = station(&vault);
    assert_eq!(
        s.call("vault.status", json!({}))["state"],
        json!("not_connected")
    );

    // An agent cannot start a connect, change rotation or disconnect.
    let agent = s.fake(json!({"steps": []}), json!({}));
    s.wait_text(&agent, "[fake] session");
    for (m, p) in [
        ("vault.connect", json!({})),
        ("vault.rotation", json!({"every_hours": 1})),
        ("vault.disconnect", json!({})),
        ("vault.check", json!({})),
        ("vault.prompt", json!({"dismissed": true})),
    ] {
        assert_eq!(
            s.call_as_pane(&agent, m, p).unwrap_err().code,
            "denied",
            "{m}"
        );
    }

    let out = s.call("vault.connect", json!({}));
    // The owner is sent to the vault's own page; nothing is issued yet.
    assert!(
        out["approve_url"]
            .as_str()
            .unwrap()
            .starts_with(&vault.url()),
        "{out}"
    );
    assert!(s.call("vault.status", json!({}))["pending"]["approve_url"].is_string());
    std::thread::sleep(Duration::from_millis(1200));
    assert!(
        vault.issued().is_empty() && !s.root.join("state/vault.token").exists(),
        "no approval, no token"
    );

    assert_eq!(vault.decide(true), 1);
    wait_for("the token to be collected", 10, || {
        (s.call("vault.status", json!({}))["state"] == json!("connected")).then_some(())
    });
    let token_file = s.root.join("state/vault.token");
    let token = std::fs::read_to_string(&token_file).unwrap();
    assert_eq!(vault.issued(), vec![token.clone()]);
    // Unix: the file's own bits. Windows has none: the state folder's
    // access list is what keeps the file to this user (`sys::private_dir`).
    assert!(spandeck_terminal::sys::file_mode(&token_file).is_none_or(|m| m == 0o600));

    // The token is in no API reply, no log entry and no pane's environment.
    let pane = s.call(
        "pane.spawn",
        json!({"program": "/bin/sh", "cwd": s.root.join("home")}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.call(
        "pane.send_text",
        json!({"pane": pane, "text": "env | grep -ci avt_ ; echo env-done"}),
    );
    let screen = s.wait_text(&pane, "env-done");
    assert!(!screen.contains("avt_station"), "{screen}");
    assert!(!everything_the_api_says(&s).contains(&token));

    // Disconnect forgets the token and says where to revoke it.
    let out = s.call("vault.disconnect", json!({}));
    assert!(out["next"].as_str().unwrap().contains("spandeck:alpha"));
    assert!(!token_file.exists());
    assert_eq!(
        s.call("vault.status", json!({}))["state"],
        json!("not_connected")
    );
}

#[test]
fn a_denied_request_yields_nothing() {
    let vault = FakeVault::start();
    let s = station(&vault);
    s.call("vault.connect", json!({}));
    wait_for("the request", 10, || {
        (vault.decide(false) > 0).then_some(())
    });
    let v = wait_for("the denial to be seen", 10, || {
        let v = s.call("vault.status", json!({}));
        v["pending"].is_null().then_some(v)
    });
    assert_eq!(v["state"], json!("not_connected"));
    assert!(v["problem"].as_str().unwrap().contains("denied"));
    assert!(!s.root.join("state/vault.token").exists());
}

#[test]
fn agents_use_keys_through_the_vault_within_their_projects_list() {
    let vault = FakeVault::start();
    let s = station(&vault);
    let token = connect(&s, &vault);
    let repo = project(&s, "[\"OPENAI_*\"]");
    let dev = s.fake(
        json!({"steps": []}),
        json!({"name": "dev", "project": "demo", "cwd": repo}),
    );
    let loose = s.fake(json!({"steps": []}), json!({"name": "loose"}));
    s.wait_text(&dev, "[fake] session");
    s.wait_text(&loose, "[fake] session");

    // Only the keys the project lists are even visible.
    let list = s.call_as_pane(&dev, "vault.list", json!({})).unwrap();
    let names: Vec<&str> = list["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["OPENAI_API_KEY", "OPENAI_ORG"]);

    // A call goes through the vault, with the station token and the pane named.
    let out = s
        .call_as_pane(
            &dev,
            "vault.call",
            json!({"name": "OPENAI_API_KEY", "path": "/v1/models?limit=5"}),
        )
        .unwrap();
    assert_eq!(
        (
            out["status"].as_u64(),
            out["json"]["provider_says"].as_str()
        ),
        (Some(200), Some("ok"))
    );
    let calls = vault.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        (
            calls[0]["name"].as_str(),
            calls[0]["actor"].as_str(),
            calls[0]["token"].as_str()
        ),
        (
            Some("OPENAI_API_KEY"),
            Some("demo/dev@alpha"),
            Some(token.as_str())
        )
    );

    // Refused before the vault is ever called: a key not on the list, a pane
    // outside any project, and a path that tries to climb out.
    assert_eq!(
        s.call_as_pane(
            &dev,
            "vault.call",
            json!({"name": "GITHUB_TOKEN", "path": "/user"})
        )
        .unwrap_err()
        .code,
        "denied"
    );
    assert_eq!(
        s.call_as_pane(&dev, "vault.describe", json!({"name": "GITHUB_TOKEN"}))
            .unwrap_err()
            .code,
        "denied"
    );
    // A pane outside any project, with nothing set up, may use what the
    // owner allowed this station at the vault: connecting is the one step.
    let all = s.call_as_pane(&loose, "vault.list", json!({})).unwrap();
    assert_eq!(all["keys"].as_array().unwrap().len(), 3, "{all}");
    assert!(s
        .call_as_pane(
            &dev,
            "vault.call",
            json!({"name": "OPENAI_API_KEY", "path": "/../GITHUB_TOKEN/user"})
        )
        .is_err());
    // Found in review before release: an encoded "../" climbed out of the
    // key's proxy to the vault's own API, which hands out the station's token.
    for path in [
        "%2e%2e/%2e%2e/tokens/self/rotate",
        "%2E%2E/GITHUB_TOKEN/user",
        "a%2f..%2f..%2ftokens/self",
        "%252e%252e/tokens/self",
    ] {
        let err = s
            .call_as_pane(
                &dev,
                "vault.call",
                json!({"name": "OPENAI_API_KEY", "path": path, "method": "POST"}),
            )
            .unwrap_err();
        assert!(
            err.message.contains("not allowed"),
            "{path}: {}",
            err.message
        );
    }
    assert_eq!(vault.calls().len(), 1);

    // The ship's log says who used which key, and nothing about what was sent.
    s.call_as_pane(&dev, "vault.call", json!({"name": "OPENAI_API_KEY", "path": "/v1/chat/completions", "method": "POST", "body": {"prompt": "confidential-question"}})).unwrap();
    let uses = s.log("vault.use");
    assert_eq!(uses.len(), 2);
    assert_eq!(
        uses[0]["data"],
        json!({"key": "OPENAI_API_KEY", "method": "POST", "path": "v1/chat", "status": 200, "actor": "demo/dev@alpha"})
    );
    assert!(!everything_the_api_says(&s).contains("confidential-question"));
}

#[test]
fn a_locked_vault_shows_as_locked_not_as_an_agent_failure() {
    let vault = FakeVault::start();
    let s = station(&vault);
    connect(&s, &vault);
    vault.set_unlocked(false);
    assert_eq!(s.call("vault.refresh", json!({}))["state"], json!("locked"));
    assert_eq!(
        s.call("station.info", json!({}))["vault"]["state"],
        json!("locked")
    );
    let err = s
        .try_call(
            "vault.call",
            json!({"name": "OPENAI_API_KEY", "path": "/v1/models"}),
        )
        .unwrap_err();
    assert!(
        err.message.contains("the vault is locked"),
        "{}",
        err.message
    );
    vault.set_unlocked(true);
    assert_eq!(
        s.call("vault.refresh", json!({}))["state"],
        json!("connected")
    );
}

#[test]
fn something_that_is_not_agentvault_is_not_taken_for_a_vault() {
    let vault = FakeVault::start();
    vault.inner.lock().unwrap().service = false;
    let s = Station::start_with("alpha", &vault.config(), &[]);
    let v = s.call("vault.refresh", json!({}));
    assert_eq!(v["state"], json!("unreachable"));
    assert!(s.try_call("vault.connect", json!({})).is_err() || vault.issued().is_empty());
}

/// With a vault connected, a raw key in a command or file is blocked, and the
/// vault is told the key is exposed (phase 3). The key never reaches the log.
#[test]
fn a_raw_key_in_a_command_is_blocked_and_reported() {
    let vault = FakeVault::start();
    let s = station(&vault);
    let leaky = json!({"steps": [
        {"prompt": "deploy"},
        {"tool": {"name": "Bash", "command": format!("curl -H 'x-api-key: {KEY}' https://api.example.com")}},
        {"tool": {"name": "Bash", "command": "echo fine"}},
        {"turn_end": "tried"}
    ]});

    // No vault connected: spandeck does not block (there is nothing to offer instead).
    let before = s.fake(leaky.clone(), json!({"name": "before"}));
    s.wait_text(&before, "[fake] turn done");
    assert_eq!(s.text(&before).matches("[fake] ran Bash (ok)").count(), 2);

    connect(&s, &vault);
    let after = s.fake(leaky, json!({"name": "after"}));
    let screen = s.wait_text(&after, "[fake] turn done").replace('\n', "");
    assert!(screen.contains("[fake] BLOCKED Bash: Blocked by spandeck: this contains what looks like a Anthropic key"), "{screen}");
    assert!(screen.contains("vault_call"));
    assert_eq!(
        screen.matches("[fake] ran Bash (ok)").count(),
        1,
        "ordinary commands still run"
    );
    assert_eq!(
        s.log("order.blocked")[0]["data"]["order"],
        json!("no-raw-keys")
    );
    assert_eq!(
        vault.reports(),
        vec![KEY.to_string()],
        "the vault was told this key is exposed"
    );
    assert!(
        !everything_the_api_says(&s).contains("PLANTED"),
        "the key itself is in no log or reply"
    );

    // A key pasted into a prompt cannot be blocked, so the owner is warned.
    let pasted = s.fake(
        json!({"steps": [{"prompt": format!("use this key {KEY} please")}, {"turn_end": "ok"}]}),
        json!({"name": "pasted"}),
    );
    s.wait_text(&pasted, "[fake] turn done");
    let notes = s.call("notify.list", json!({}));
    assert!(
        notes.as_array().unwrap().iter().any(|n| n["title"]
            .as_str()
            .unwrap()
            .contains("key was pasted into pasted")),
        "{notes}"
    );
}

/// Phase 3: a key that turns up in a session transcript is reported.
#[test]
fn a_key_found_in_a_transcript_is_reported_to_the_vault() {
    let vault = FakeVault::start();
    let s = station(&vault);
    connect(&s, &vault);
    let other = "ghp_PLANTEDplantedPLANTEDplanted0123456789";
    let pane = s.fake(
        json!({"steps": [{"transcript": {"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": format!("the token is {other}")}]}}}, {"turn_end": "ok"}]}),
        json!({}),
    );
    s.wait_state(&pane, "done");
    wait_for("the report", 20, || {
        s.call("vault.refresh", json!({}));
        vault.reports().contains(&other.to_string()).then_some(())
    });
    assert!(s
        .log("vault.reported")
        .iter()
        .all(|e| !e.to_string().contains("PLANTED")));
}

/// Phase 2: rotation is set in the Control Deck and done by spandeckd.
#[test]
fn rotation_is_set_by_the_owner_and_the_station_swaps_its_own_token() {
    let vault = FakeVault::start();
    let s = station(&vault);
    let first = connect(&s, &vault);
    assert!(s
        .try_call("vault.rotation", json!({"every_hours": 0}))
        .unwrap_err()
        .message
        .contains("1 hour"));
    let v = s.call("vault.rotation", json!({"every_hours": 6}));
    assert_eq!(
        (
            v["rotation_hours"].as_u64(),
            v["vault_rotation_hours"].as_u64()
        ),
        (Some(6), Some(6))
    );
    assert_eq!(vault.inner.lock().unwrap().tokens[&first].rotation, Some(6));

    // The token is overdue (say the laptop slept): it can still rotate itself,
    // with nothing for the owner to do.
    vault.with_tokens("alpha", |t| t.overdue = true);
    s.call("vault.refresh", json!({}));
    let second = std::fs::read_to_string(s.root.join("state/vault.token")).unwrap();
    assert_ne!(second, first, "the new token was saved");
    // The old token works until the new one is first used...
    assert_eq!(vault.live_tokens("alpha").len(), 2);
    s.call(
        "vault.call",
        json!({"name": "OPENAI_API_KEY", "path": "/v1/models"}),
    );
    // ...and then it is retired.
    assert_eq!(vault.live_tokens("alpha"), vec![second.clone()]);
    assert_eq!(s.log("vault.rotated").len(), 1);
    assert!(!everything_the_api_says(&s).contains(&second));
}

/// A crash between receiving a new token and saving it must not lock the
/// station out: the old token is still good, and the next swap starts clean.
#[test]
fn a_swap_that_was_never_saved_leaves_the_station_working() {
    let vault = FakeVault::start();
    let s = station(&vault);
    let token = connect(&s, &vault);
    // spandeckd "crashed" after the vault issued a new token and before saving it.
    let lost = reqwest::blocking_post(&format!("{}/v1/tokens/self/rotate", vault.url()), &token);
    assert!(lost.contains("avt_station"));
    assert_eq!(
        std::fs::read_to_string(s.root.join("state/vault.token")).unwrap(),
        token
    );
    s.call(
        "vault.call",
        json!({"name": "OPENAI_API_KEY", "path": "/v1/models"}),
    );
    // The station rotates again with the old token: the unused one is discarded.
    s.call("vault.rotation", json!({"every_hours": 1}));
    vault.with_tokens("alpha", |t| t.overdue = t.unused_new.is_some());
    s.call("vault.refresh", json!({}));
    let now = std::fs::read_to_string(s.root.join("state/vault.token")).unwrap();
    assert_ne!(now, token);
    s.call(
        "vault.call",
        json!({"name": "OPENAI_API_KEY", "path": "/v1/models"}),
    );
    assert_eq!(vault.live_tokens("alpha"), vec![now]);
}

#[test]
fn the_owner_is_told_when_the_setting_or_the_token_changes_outside_the_control_deck() {
    let vault = FakeVault::start();
    let s = station(&vault);
    connect(&s, &vault);
    s.call("vault.rotation", json!({"every_hours": 24}));
    let titles = |s: &Station| -> Vec<String> {
        s.call("notify.list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["title"].as_str().unwrap().to_string())
            .collect()
    };

    // Something other than the Control Deck turns rotation off at the vault.
    vault.with_tokens("alpha", |t| t.rotation = None);
    s.call("vault.refresh", json!({}));
    assert!(
        titles(&s)
            .iter()
            .any(|t| t.contains("changed outside the Control Deck")),
        "{:?}",
        titles(&s)
    );
    s.call("vault.refresh", json!({}));
    assert_eq!(
        titles(&s)
            .iter()
            .filter(|t| t.contains("changed outside"))
            .count(),
        1,
        "told once, not on every check"
    );

    // Someone else swapped (or the owner revoked) the station's token.
    vault.with_tokens("alpha", |t| t.retired = true);
    let v = s.call("vault.refresh", json!({}));
    assert_eq!(v["state"], json!("token_rejected"));
    assert!(titles(&s)
        .iter()
        .any(|t| t.contains("vault token was refused")));
}

#[test]
fn the_control_deck_connects_a_station_through_a_pop_up_on_the_vault() {
    use common::browser::Page;
    let vault = FakeVault::start();
    let s = station(&vault);
    let page = Page::open(&s, 1400, 900);
    page.wait("the vault state on the station tile", "[...document.querySelectorAll('.tile.station .vault-chip')].some(c => c.innerText.includes('not connected'))");

    // Record what the Control Deck opens, as a browser would show it.
    page.js("(() => { window.__opened = []; const fake = {closed: false, close() { this.closed = true; }, location: {set href(u) { window.__opened.push(u); }}}; window.open = (u, n, f) => { window.__opened.push('open:' + u + ':' + f); return fake; }; return true; })()");
    page.js("[...document.querySelectorAll('.topbar button')].find(b => b.innerText.startsWith('Vault')).click()");
    page.wait(
        "the vault dialog",
        "[...document.querySelectorAll('.dialog button')].some(b => b.innerText === 'Connect')",
    );
    page.js("[...document.querySelectorAll('.dialog button')].find(b => b.innerText === 'Connect').click()");
    let opened = page.wait(
        "the pop-up to be pointed at the vault",
        "window.__opened.length >= 2 && window.__opened",
    );
    assert!(
        opened[0].as_str().unwrap().starts_with("open::")
            && opened[0].as_str().unwrap().contains("popup"),
        "opened at the click, as a pop-up: {opened}"
    );
    assert!(
        opened[1]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}/#/approve/", vault.url())),
        "{opened}"
    );
    assert!(
        page.js("!document.querySelector('.dialog input[type=password]')")
            .as_bool()
            .unwrap(),
        "the Control Deck never asks for the passphrase"
    );

    assert_eq!(vault.decide(true), 1);
    page.wait("the tile to show connected", "[...document.querySelectorAll('.tile.station .vault-chip')].some(c => c.innerText.includes('connected') && !c.innerText.includes('not'))");
    assert_eq!(
        s.call("vault.status", json!({}))["state"],
        json!("connected")
    );

    // Rotation is one setting in the same dialog.
    page.wait(
        "the rotation setting",
        "!!document.querySelector('.dialog select[aria-label=\"Token rotation for alpha\"]')",
    );
    page.js("(() => { const sel = document.querySelector('.dialog select[aria-label=\"Token rotation for alpha\"]'); sel.value = '24'; sel.dispatchEvent(new Event('change', {bubbles: true})); return true; })()");
    wait_for("the setting to reach the vault", 10, || {
        (s.call("vault.status", json!({}))["vault_rotation_hours"] == json!(24)).then_some(())
    });
}

fn check<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == json!(id))
        .unwrap_or_else(|| panic!("no {id} check in {v}"))
}

fn alerts(s: &Station, title: &str) -> usize {
    s.call("notify.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["title"].as_str().unwrap().contains(title))
        .count()
}

/// Check and repair on launch: the station checks the chain from an agent to
/// a key by itself, repairs what needs no approval, and says each thing only
/// the owner can fix in one sentence with the one action that fixes it.
#[test]
fn the_station_checks_the_chain_repairs_what_it_can_and_says_what_is_left() {
    let vault = FakeVault::start();
    let spare = free_port();
    // This station looks for its vault, as one on a tailnet does.
    let s = Station::start_with("alpha", &FakeVault::search(&[vault.port, spare]), &[]);

    // Nobody asked: the check ran because the daemon started.
    let v = wait_for("the check at launch", 10, || {
        let v = s.call("vault.status", json!({}));
        (v["health"] == json!("not_connected")).then_some(v)
    });
    assert_eq!(
        (&v["prompt"], &check(&v, "token")["action"]["kind"]),
        (&json!(true), &json!("connect")),
        "a vault was found and the station has no token: {v}"
    );
    // "Not now" is kept by the station, and can be undone.
    assert_eq!(
        s.call("vault.prompt", json!({"dismissed": true}))["prompt"],
        json!(false)
    );
    assert_eq!(
        s.call("vault.status", json!({}))["prompt_dismissed"],
        json!(true)
    );
    assert_eq!(
        s.call("vault.prompt", json!({"dismissed": false}))["prompt"],
        json!(true)
    );

    // Healthy: every link is checked and nothing is said to the owner.
    let token = connect(&s, &vault);
    let v = s.call("vault.check", json!({}));
    let ids: Vec<&str> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["address", "lock", "token", "keys", "panes"], "{v}");
    assert!(
        v["health"] == json!("ok")
            && v["checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["ok"] == json!(true)),
        "{v}"
    );
    assert!(check(&v, "keys")["detail"]
        .as_str()
        .unwrap()
        .starts_with("3 keys"));
    assert_eq!(v["prompt"], json!(false));
    assert_eq!(s.log("vault.alert").len(), 0);

    // Locked: only the owner can unlock it, on the vault's page.
    vault.set_unlocked(false);
    let v = s.call("vault.check", json!({}));
    let lock = check(&v, "lock");
    assert_eq!(v["health"], json!("problem"));
    assert_eq!(
        (
            &lock["ok"],
            &lock["action"]["kind"],
            lock["action"]["url"].as_str()
        ),
        (&json!(false), &json!("open"), Some(vault.url().as_str())),
        "{v}"
    );
    s.call("vault.check", json!({}));
    assert_eq!(
        alerts(&s, "The vault is locked"),
        1,
        "told once, not on every check"
    );
    let doctor = s.call("station.doctor", json!({}));
    let line = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == json!("vault"))
        .unwrap();
    assert!(
        line["ok"] == json!(false) && line["detail"].as_str().unwrap().contains("locked"),
        "{line}"
    );
    vault.set_unlocked(true);
    assert_eq!(s.call("vault.check", json!({}))["health"], json!("ok"));

    // The vault moved. The station sees an AgentVault at another address and
    // does not show it the token: the owner is told, and connects again.
    let shown = vault.inner.lock().unwrap().presented.len();
    let vault = vault.moved(spare);
    let v = s.call("vault.check", json!({}));
    let address = check(&v, "address");
    assert_eq!(
        (&v["health"], &address["action"]["label"]),
        (&json!("problem"), &json!("Reconnect")),
        "{v}"
    );
    assert!(
        address["detail"]
            .as_str()
            .unwrap()
            .starts_with(&format!("The vault seems to have moved to {}", vault.url())),
        "{v}"
    );
    s.call("vault.check", json!({}));
    s.call("vault.refresh", json!({}));
    assert_eq!(
        vault.inner.lock().unwrap().presented.len(),
        shown,
        "the token was never shown to the new address"
    );
    assert_eq!(vault.issued(), vec![token.clone()]);
    // Reconnect is the ordinary approval, at the vault that answers now.
    let token = connect(&s, &vault);
    let v = s.call("vault.check", json!({}));
    assert_eq!(
        (&v["health"], v["url"].as_str()),
        (&json!("ok"), Some(vault.url().as_str())),
        "{v}"
    );
    assert_eq!(vault.issued().len(), 2);

    // The vault is not answering anywhere: nothing to repair, said plainly.
    vault.answer(false);
    let v = s.call("vault.check", json!({}));
    let address = check(&v, "address");
    assert!(
        address["ok"] == json!(false)
            && address["detail"]
                .as_str()
                .unwrap()
                .contains("is not answering"),
        "{v}"
    );
    vault.answer(true);

    // The owner untied the token from this machine at the vault.
    vault.with_tokens("alpha", |t| t.unpinned = true);
    let v = s.call("vault.check", json!({}));
    assert_eq!(
        check(&v, "token")["action"]["label"],
        json!("Reconnect"),
        "{v}"
    );
    vault.with_tokens("alpha", |t| t.unpinned = false);

    // The token was revoked. A new one needs the owner's approval at the
    // vault: the station offers that and takes no other way.
    vault.with_tokens("alpha", |t| t.retired = true);
    let v = s.call("vault.check", json!({}));
    let tok = check(&v, "token");
    assert_eq!(
        (
            &v["health"],
            &tok["ok"],
            &tok["action"]["kind"],
            &v["prompt"]
        ),
        (
            &json!("problem"),
            &json!(false),
            &json!("connect"),
            &json!(true)
        ),
        "{v}"
    );
    s.call("vault.check", json!({}));
    s.call("vault.refresh", json!({}));
    assert_eq!(alerts(&s, "vault token was refused"), 1);
    assert_eq!(vault.issued().len(), 2);
    assert_eq!(
        std::fs::read_to_string(s.root.join("state/vault.token")).unwrap(),
        token
    );
}

/// `[vault] follow_moves = true`, the owner's choice: a station follows a
/// vault that moved, with the token it has.
#[test]
fn a_station_told_to_follow_moves_finds_its_vault_again_with_the_same_token() {
    let vault = FakeVault::start();
    let spare = free_port();
    let s = Station::start_with(
        "alpha",
        &format!(
            "{}follow_moves = true\n",
            FakeVault::search(&[vault.port, spare])
        ),
        &[],
    );
    wait_for("the vault to be found", 10, || {
        (s.call("vault.refresh", json!({}))["found"] == json!(true)).then_some(())
    });
    let token = connect(&s, &vault);
    let vault = vault.moved(spare);
    let v = s.call("vault.check", json!({}));
    assert_eq!(
        (&v["health"], v["url"].as_str()),
        (&json!("ok"), Some(vault.url().as_str())),
        "{v}"
    );
    assert!(check(&v, "address")["detail"]
        .as_str()
        .unwrap()
        .contains("spandeck followed it"));
    s.call(
        "vault.call",
        json!({"name": "OPENAI_API_KEY", "path": "/v1/models"}),
    );
    assert_eq!(vault.calls()[0]["token"], json!(token.clone()));
    assert_eq!(vault.issued(), vec![token]);
}

/// With `[vault]` turned off the station looks for no vault and says nothing
/// of one: spandeck works without it.
#[test]
fn a_station_with_the_vault_turned_off_asks_nothing_and_offers_nothing() {
    let vault = FakeVault::start();
    let s = Station::start_with(
        "alpha",
        &format!("{}enabled = false\n", vault.config()),
        &[],
    );
    let v = s.call("vault.check", json!({}));
    assert_eq!(
        (&v["health"], &v["found"], &v["prompt"]),
        (&json!("off"), &json!(false), &json!(false)),
        "{v}"
    );
    assert!(s.try_call("vault.connect", json!({})).is_err());
}

/// Connect prompt at start-up, and the one place to see the checks.
#[test]
fn the_control_deck_offers_to_connect_once_and_shows_what_each_check_found() {
    use common::browser::Page;
    let vault = FakeVault::start();
    // What the vault says of itself is data, never markup.
    vault.inner.lock().unwrap().version = Some("<b id=planted>9</b>".into());
    let s = station(&vault);
    let page = Page::open(&s, 1400, 900);
    let settled = "document.body.dataset.vaultAsked === '1'";
    let reload = |page: &Page| {
        page.js("(() => { window.__before = true; location.reload(); return true; })()");
        page.wait(
            "the deck to open again",
            &format!("!window.__before && {settled}"),
        );
    };
    let click = "(b => b ? (b.click(), true) : false)";
    let prompt_button = |label: &str| {
        format!("{click}([...document.querySelectorAll('.vault-prompt button')].find(b => b.innerText === '{label}'))")
    };

    // Said once, in a bar that leaves the deck usable.
    page.wait("the offer to connect", "(p => p && p.innerText.includes('alpha is not connected') && !document.querySelector('.backdrop'))(document.querySelector('.vault-prompt'))");
    page.wait("Not now", &prompt_button("Not now"));
    wait_for("the station to remember", 10, || {
        (s.call("vault.status", json!({}))["prompt_dismissed"] == json!(true)).then_some(())
    });
    reload(&page);
    assert_eq!(
        page.js("!document.querySelector('.vault-prompt')"),
        json!(true)
    );

    // Undone from the vault screen.
    page.js("[...document.querySelectorAll('.topbar button')].find(b => b.innerText.startsWith('Vault')).click()");
    page.wait("Remind me again", &format!("{click}([...document.querySelectorAll('.dialog button')].find(b => b.innerText === 'Remind me again'))"));
    wait_for("the reminder to be back on", 10, || {
        (s.call("vault.status", json!({}))["prompt"] == json!(true)).then_some(())
    });
    reload(&page);

    // Its one button starts the connect flow: a pop-up on the vault.
    page.js("(() => { window.__opened = []; window.open = (u, n, f) => ({closed: false, close() {}, location: {set href(u) { window.__opened.push(u); }}}); return true; })()");
    page.wait("Connect", &prompt_button("Connect"));
    let opened = page.wait(
        "the pop-up to be pointed at the vault",
        "window.__opened.length && window.__opened[0]",
    );
    assert!(
        opened
            .as_str()
            .unwrap()
            .starts_with(&format!("{}/#/approve/", vault.url())),
        "{opened}"
    );
    assert_eq!(vault.decide(true), 1);

    // The vault screen lists what each check found.
    page.js("[...document.querySelectorAll('.topbar button')].find(b => b.innerText.startsWith('Vault')).click()");
    let rows = "[...document.querySelectorAll('.dialog .vault-checks tr')].map(r => r.innerText)";
    page.wait(
        "the checks of a connected station",
        &format!("{rows}.some(r => r.includes('Keys for agents') && r.includes('3 keys'))"),
    );
    assert_eq!(
        page.js(&format!(
            "!document.querySelector('#planted') && {rows}.some(r => r.includes('<b id=planted>'))"
        )),
        json!(true),
        "text from the vault is shown as text"
    );
    assert_eq!(
        page.js("!document.querySelector('.vault-prompt')"),
        json!(true)
    );

    // A problem shows its sentence and the one action; Check again asks now.
    vault.set_unlocked(false);
    page.wait("Check again", &format!("{click}([...document.querySelectorAll('.dialog button')].find(b => b.innerText === 'Check again' && !b.disabled))"));
    page.wait("the locked vault and its action", "[...document.querySelectorAll('.dialog .vault-checks tr.broken')].some(r => r.innerText.includes('The vault is locked') && [...r.querySelectorAll('button')].some(b => b.innerText === 'Open the vault'))");
}

mod reqwest {
    /// A one-off POST with a bearer token, without pulling async into this test.
    pub fn blocking_post(url: &str, token: &str) -> String {
        let out = std::process::Command::new("curl")
            .args([
                "-s",
                "-X",
                "POST",
                "-H",
                &format!("Authorization: Bearer {token}"),
                url,
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

#[allow(dead_code)]
fn unused(_: Value) {}
