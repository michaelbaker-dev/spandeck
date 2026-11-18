//! AgentVault: agents use API keys through the vault's proxy without ever
//! holding a key. The contract is in docs/specs/spandeck-agentvault.md.
//!
//! What this station holds is one *station token*: list + proxy only, pinned to
//! this machine by the vault, issued only when the owner approves on the
//! vault's own page. spandeckd keeps the token and makes every vault call
//! itself. The token is never put in a pane's environment, never returned by
//! any API call, and never written to the ship's log.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::{invalid, Caller, Daemon, Error, PaneMeta, Result};
use crate::paths::now_ms;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    /// `false` turns the integration off: no vault is looked for, nothing is
    /// checked and the Control Deck never mentions one.
    pub enabled: bool,
    /// The vault's address, for a vault that discovery cannot find (another
    /// port). Normally left unset: spandeck finds the vault each time.
    pub url: Option<String>,
    /// More addresses to ask when looking for the vault, beside the machines
    /// Tailscale lists: for a vault discovery cannot see, and tests.
    pub search: Vec<String>,
    /// Let a connected station follow a vault that moved, by trying its token
    /// at another AgentVault on the owner's machines once the address that
    /// issued it stops answering. Off unless the owner turns it on: without
    /// it a station shows its token to no address but the one that issued
    /// it, and a move is put right by the owner connecting again.
    pub follow_moves: bool,
}

impl Default for VaultConfig {
    fn default() -> Self {
        VaultConfig {
            enabled: true,
            url: None,
            search: vec![],
            follow_moves: false,
        }
    }
}

/// How long a check made because a Control Deck opened stays good.
const OPEN_CHECK_EVERY_MS: u64 = 60_000;

#[derive(Debug, Clone, Default)]
struct Pending {
    request_id: String,
    approve_url: String,
    expires_ms: u64,
}

#[derive(Debug, Clone, Default)]
struct Seen {
    /// Base URL of the vault, as last found.
    base: Option<String>,
    reachable: bool,
    unlocked: bool,
    version: Option<String>,
    /// What the vault says about our token: Some(false) means it refused it.
    token_ok: Option<bool>,
    /// The rotation setting the vault has recorded for this station's token.
    vault_rotation: Option<Option<u64>>,
    alerted_mismatch: bool,
    last_problem: Option<String>,
    /// What is wrong with a token the vault answered about: `refused`,
    /// `other_machine`, `unpinned` or `overdue`.
    token_trouble: Option<&'static str>,
    /// The machines the vault has tied the token to.
    token_nodes: Vec<String>,
    /// The address the vault answered at before it moved, when spandeck
    /// followed it since the daemon started.
    moved_from: Option<String>,
    /// An AgentVault that answers somewhere else while the one this station
    /// was connected to does not, and that does not know this station's token.
    elsewhere: Option<String>,
    /// The last full check's answer to "can agents list keys": how many, or why not.
    keys: Option<std::result::Result<usize, String>>,
    /// The last full check's answer to "do panes have their way to the vault".
    panes: Option<std::result::Result<String, String>>,
    /// When the chain was last checked at all, and last checked in full.
    checked_ms: u64,
    full_ms: u64,
}

#[derive(Default)]
pub struct VaultState {
    seen: Mutex<Seen>,
    pending: Mutex<Option<Pending>>,
    /// One check (and so one token swap) at a time.
    checking: tokio::sync::Mutex<()>,
}

/// One link of the chain between an agent and a key, as the owner reads it.
struct Check {
    id: &'static str,
    name: &'static str,
    /// Which failure this is, so the owner is told of each once.
    trouble: Option<&'static str>,
    detail: String,
    action: Option<Value>,
}

impl Check {
    fn ok(id: &'static str, name: &'static str, detail: String) -> Check {
        Check {
            id,
            name,
            trouble: None,
            detail,
            action: None,
        }
    }

    fn bad(
        id: &'static str,
        name: &'static str,
        trouble: &'static str,
        detail: String,
        action: Value,
    ) -> Check {
        Check {
            id,
            name,
            trouble: Some(trouble),
            detail,
            action: Some(action),
        }
    }

    fn json(&self) -> Value {
        json!({"id": self.id, "name": self.name, "ok": self.trouble.is_none(), "detail": self.detail, "action": self.action})
    }
}

fn act_check() -> Value {
    json!({"kind": "check", "label": "Check again"})
}

fn act_connect(label: &str) -> Value {
    json!({"kind": "connect", "label": label})
}

/// Text the vault sent, cut to a length that fits in a sentence. It is data:
/// the Control Deck inserts it as text.
fn said(body: &Value) -> String {
    let text = body
        .pointer("/error/message")
        .or_else(|| body.get("error"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    text.chars().take(160).collect()
}

pub fn glob_match(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => {
            name.len() >= head.len() + tail.len().min(name.len()) && name.starts_with(head) && {
                let rest = &name[head.len()..];
                if tail.contains('*') {
                    (0..=rest.len())
                        .any(|i| rest.is_char_boundary(i) && glob_match(tail, &rest[i..]))
                } else {
                    rest.ends_with(tail)
                }
            }
        }
    }
}

fn clean_base(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

impl Daemon {
    fn vault_token_file(&self) -> PathBuf {
        self.paths.state_dir.join("vault.token")
    }

    fn vault_token(&self) -> Option<String> {
        std::fs::read_to_string(self.vault_token_file())
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    }

    /// Save a token so that a crash at any point leaves a usable token on disk:
    /// written beside the old one, flushed, then swapped in.
    fn vault_save_token(&self, token: &str) -> std::io::Result<()> {
        let tmp = self.vault_token_file().with_extension("token.new");
        // Readable by the user only, and on disk before it is swapped in.
        spandeck_terminal::sys::write_private(&tmp, token.as_bytes())?;
        std::fs::rename(&tmp, self.vault_token_file())?;
        if let Some(base) = self.vault_base() {
            self.db.kv_set("vault.base", &base);
        }
        Ok(())
    }

    pub fn vault_connected(&self) -> bool {
        self.vault_token().is_some()
    }

    fn vault_base(&self) -> Option<String> {
        self.cfg
            .vault
            .url
            .as_deref()
            .map(clean_base)
            .or_else(|| self.vault.seen.lock().unwrap().base.clone())
            // A connected station knows where its vault was, even when the
            // vault has not answered since the daemon started.
            .or_else(|| self.vault_pinned())
    }

    /// The address of the vault that issued this station's token.
    fn vault_pinned(&self) -> Option<String> {
        self.vault_token()?;
        self.db.kv_get("vault.base").filter(|b| !b.is_empty())
    }

    fn vault_prompt_dismissed(&self) -> bool {
        self.db.kv_get("vault.prompt_dismissed").as_deref() == Some("1")
    }

    /// "Not now" on the Control Deck's connect prompt, and its undoing. Kept
    /// by the station, so it holds in every browser.
    pub fn vault_set_prompt(&self, dismissed: bool, by: &str) -> Value {
        self.db
            .kv_set("vault.prompt_dismissed", if dismissed { "1" } else { "" });
        self.record(
            "vault.prompt_set",
            None,
            None,
            None,
            Some(by),
            json!({"dismissed": dismissed}),
        );
        self.emit("vault.updated", None, None, self.vault_view());
        self.vault_view()
    }

    /// The chain from an agent to a key, link by link, from what the last
    /// check saw. A link that cannot be judged because an earlier one is
    /// broken is left out: one problem, one sentence.
    fn vault_checks(&self, seen: &Seen, base: &Option<String>, connected: bool) -> Vec<Check> {
        let Some(url) = base else {
            return vec![];
        };
        if seen.checked_ms == 0 {
            return vec![];
        }
        let mut out = vec![];
        const ADDRESS: &str = "Vault address";
        if seen.reachable {
            let version = seen
                .version
                .as_deref()
                .map(|v| format!(" {v}"))
                .unwrap_or_default();
            let moved = seen
                .moved_from
                .as_deref()
                .map(|old| format!(". It moved from {old}; spandeck followed it"))
                .unwrap_or_default();
            out.push(Check::ok(
                "address",
                ADDRESS,
                format!("AgentVault{version} answers at {url}{moved}"),
            ));
        } else if self.cfg.vault.url.is_some() {
            out.push(Check::bad("address", ADDRESS, "unreachable", format!("Nothing answers as AgentVault at {url}, the address set in config.toml: correct `[vault] url`, or remove it so spandeck finds the vault itself."), act_check()));
        } else if let Some(other) = seen.elsewhere.as_deref().filter(|_| connected) {
            out.push(Check::bad("address", ADDRESS, "moved", format!("The vault seems to have moved to {other}: the one this station was connected to, at {url}, is not answering. Reconnect to approve this station at {other}."), act_connect("Reconnect")));
        } else {
            out.push(Check::bad("address", ADDRESS, "unreachable", format!("The vault at {url} is not answering: check that AgentVault is running on that machine and that Tailscale is connected."), act_check()));
        }
        if seen.reachable {
            const LOCK: &str = "Vault unlocked";
            if seen.unlocked {
                out.push(Check::ok("lock", LOCK, "The vault is unlocked".into()));
            } else {
                out.push(Check::bad(
                    "lock",
                    LOCK,
                    "locked",
                    "The vault is locked: unlock it on the vault's own page.".into(),
                    json!({"kind": "open", "label": "Open the vault", "url": url}),
                ));
            }
            const TOKEN: &str = "Station token";
            let again = act_connect("Reconnect");
            match (connected, seen.token_ok, seen.token_trouble) {
                (false, ..) => out.push(Check::bad(
                    "token",
                    TOKEN,
                    "none",
                    "This station is not connected to the vault, so its agents cannot use your keys."
                        .into(),
                    act_connect("Connect"),
                )),
                (true, None, _) => {}
                (true, Some(false), Some("other_machine")) => out.push(Check::bad("token", TOKEN, "other_machine", "The vault refuses this station's token from this machine: it is tied to another one. Connect the station again.".into(), again)),
                (true, Some(false), Some("overdue")) => out.push(Check::bad("token", TOKEN, "overdue", "The token is overdue for rotation and the vault would not swap it. Connect the station again.".into(), again)),
                (true, Some(false), _) => out.push(Check::bad("token", TOKEN, "refused", "The vault no longer accepts this station's token: it was revoked or replaced. Connect the station again.".into(), again)),
                (true, Some(true), Some("unpinned")) => out.push(Check::bad("token", TOKEN, "unpinned", "The token works from any machine: the vault has not tied it to this one. Connect the station again for one that is.".into(), again)),
                (true, Some(true), _) => out.push(Check::ok(
                    "token",
                    TOKEN,
                    if seen.token_nodes.is_empty() {
                        "Accepted by the vault".to_string()
                    } else {
                        format!(
                            "Accepted by the vault and tied to this machine ({})",
                            seen.token_nodes.join(", ")
                        )
                    },
                )),
            }
            const KEYS: &str = "Keys for agents";
            if connected && seen.unlocked && seen.token_ok == Some(true) {
                match &seen.keys {
                    Some(Ok(0)) => out.push(Check::ok(
                        "keys",
                        KEYS,
                        "The vault lists no keys for this station yet".into(),
                    )),
                    Some(Ok(n)) => out.push(Check::ok(
                        "keys",
                        KEYS,
                        format!(
                            "{n} key{} listed for this station's agents",
                            if *n == 1 { "" } else { "s" }
                        ),
                    )),
                    Some(Err(why)) => out.push(Check::bad(
                        "keys",
                        KEYS,
                        "keys",
                        format!("The vault would not list its keys for this station ({why})."),
                        act_check(),
                    )),
                    None => {}
                }
            }
        }
        if connected {
            const PANES: &str = "Agent panes";
            match &seen.panes {
                Some(Ok(how)) => out.push(Check::ok("panes", PANES, how.clone())),
                Some(Err(why)) => out.push(Check::bad(
                    "panes",
                    PANES,
                    "panes",
                    why.clone(),
                    act_check(),
                )),
                None => {}
            }
        }
        out
    }

    fn rotation_hours(&self) -> Option<u64> {
        self.db
            .kv_get("vault.rotation_hours")
            .and_then(|v| v.parse().ok())
    }

    /// What the Control Deck shows for this station. No network call.
    pub fn vault_view(&self) -> Value {
        if !self.cfg.vault.enabled {
            return json!({"enabled": false, "found": false, "state": "off", "health": "off", "has_token": false, "checks": [], "prompt": false});
        }
        let seen = self.vault.seen.lock().unwrap().clone();
        let base = self.vault_base();
        let connected = self.vault_connected();
        let checks = self.vault_checks(&seen, &base, connected);
        let health = if base.is_none() {
            "none"
        } else if seen.checked_ms == 0 {
            "unchecked"
        } else if !connected {
            "not_connected"
        } else if checks.iter().all(|c| c.trouble.is_none()) {
            "ok"
        } else {
            "problem"
        };
        let state = match (
            &base,
            seen.reachable,
            seen.unlocked,
            connected,
            seen.token_ok,
        ) {
            (None, ..) => "none",
            (Some(_), false, ..) => "unreachable",
            (Some(_), true, false, ..) => "locked",
            (Some(_), true, true, false, _) => "not_connected",
            (Some(_), true, true, true, Some(false)) => "token_rejected",
            (Some(_), true, true, true, _) => "connected",
        };
        let pending = self
            .vault
            .pending
            .lock()
            .unwrap()
            .clone()
            .filter(|p| p.expires_ms > now_ms());
        let hours = self.rotation_hours();
        let last = self
            .db
            .kv_get("vault.rotated_ms")
            .and_then(|v| v.parse::<u64>().ok());
        // The Control Deck offers to connect a station that found a vault and
        // has no working token, until the owner says "not now".
        let dismissed = self.vault_prompt_dismissed();
        let prompt = seen.reachable
            && matches!(state, "not_connected" | "token_rejected")
            && pending.is_none()
            && !dismissed;
        json!({
            "enabled": true,
            "found": base.is_some(), "url": base, "state": state, "has_token": connected,
            "health": health,
            "checks": checks.iter().map(Check::json).collect::<Vec<_>>(),
            "checked_ms": (seen.checked_ms > 0).then_some(seen.checked_ms),
            "prompt": prompt, "prompt_dismissed": dismissed,
            "version": seen.version, "problem": seen.last_problem,
            "pending": pending.map(|p| json!({"approve_url": p.approve_url, "expires_ms": p.expires_ms})),
            "rotation_hours": hours,
            "rotated_ms": last,
            "next_rotation_ms": hours.zip(last).map(|(h, l)| l + h * 3_600_000),
            "vault_rotation_hours": seen.vault_rotation.flatten(),
        })
    }

    async fn vault_get(
        &self,
        base: &str,
        path: &str,
        token: Option<&str>,
    ) -> std::result::Result<(u16, Value), String> {
        let mut req = self.fleet.http.get(format!("{base}{path}"));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        Ok((status, resp.json().await.unwrap_or(Value::Null)))
    }

    /// Find the vault: every machine under the owner's login is asked, by its
    /// Tailscale DNS name on port 443, whether it is AgentVault. The address is
    /// looked up each time, because the vault may move.
    pub(crate) async fn vault_discover(&self) {
        if !self.cfg.vault.enabled {
            return;
        }
        let mut candidates = self.vault_candidates();
        // Ask the one we already know first.
        if let Some(known) = self.vault.seen.lock().unwrap().base.clone() {
            candidates.retain(|c| *c != known);
            candidates.insert(0, known);
        }
        let mut found: Option<(String, Value)> = None;
        let mut moved_from = None;
        let mut elsewhere = None;
        // A station that holds a token talks to the vault that issued it.
        if let (Some(pinned), Some(token)) = (self.vault_pinned(), self.vault_token()) {
            if self.cfg.vault.url.is_none() {
                found = self
                    .vault_probe(&pinned)
                    .await
                    .map(|body| (pinned.clone(), body));
                // It stopped answering there. Another AgentVault among the
                // owner's machines may be the same vault, moved. The token
                // is never shown to it: the owner is told, and connects the
                // station again. Only with `follow_moves` is the token tried
                // there, and the vault that still knows it followed.
                if found.is_none() {
                    for base in candidates.iter().filter(|c| **c != pinned) {
                        let Some(body) = self.vault_probe(base).await else {
                            continue;
                        };
                        if !self.cfg.vault.follow_moves {
                            elsewhere.get_or_insert(base.clone());
                            continue;
                        }
                        let knows = tokio::time::timeout(
                            Duration::from_millis(3000),
                            self.vault_get(base, "/v1/tokens/self", Some(&token)),
                        )
                        .await;
                        match knows {
                            Ok(Ok((200, _))) => {}
                            Ok(Ok((403, me)))
                                if me.get("code").and_then(|c| c.as_str())
                                    == Some("token_overdue") => {}
                            Ok(Ok(_)) => {
                                elsewhere.get_or_insert(base.clone());
                                continue;
                            }
                            _ => continue,
                        }
                        self.db.kv_set("vault.base", base);
                        moved_from = Some(pinned.clone());
                        found = Some((base.clone(), body));
                        break;
                    }
                }
                candidates.clear();
            }
        }
        for base in candidates {
            if let Some(body) = self.vault_probe(&base).await {
                found = Some((base, body));
                break;
            }
        }
        if let Some(old) = &moved_from {
            let new = found.as_ref().map(|f| f.0.clone()).unwrap_or_default();
            self.record(
                "vault.moved",
                None,
                None,
                None,
                None,
                json!({"from": old, "to": new}),
            );
            self.notify(
                "info",
                None,
                None,
                "The vault moved, and this station followed it",
                &format!("It answered at {old} and now answers at {new}. It still knows this station's token, so there is nothing to do."),
            );
        }
        let changed = {
            let mut seen = self.vault.seen.lock().unwrap();
            let before = (
                seen.base.clone(),
                seen.reachable,
                seen.unlocked,
                seen.elsewhere.clone(),
            );
            seen.elsewhere = elsewhere;
            if moved_from.is_some() {
                seen.moved_from = moved_from;
            }
            match found {
                Some((base, body)) => {
                    seen.base = Some(base);
                    seen.reachable = true;
                    // AgentVault 0.1.0 reports `sealed`; the contract says `unlocked`.
                    // Accept either so the lock state is right with both.
                    seen.unlocked = body
                        .get("unlocked")
                        .and_then(|u| u.as_bool())
                        .or_else(|| {
                            body.get("sealed")
                                .and_then(|s| s.as_bool())
                                .map(|sealed| !sealed)
                        })
                        .unwrap_or(false);
                    seen.version = body
                        .get("version")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
                None => seen.reachable = false,
            }
            before
                != (
                    seen.base.clone(),
                    seen.reachable,
                    seen.unlocked,
                    seen.elsewhere.clone(),
                )
        };
        if changed {
            self.emit("vault.updated", None, None, self.vault_view());
        }
    }

    /// Whether AgentVault answers at an address, and what it says of itself.
    async fn vault_probe(&self, base: &str) -> Option<Value> {
        let probe = tokio::time::timeout(
            Duration::from_millis(2000),
            self.vault_get(base, "/v1/status", None),
        )
        .await;
        match probe {
            Ok(Ok((200, body)))
                if body.get("service").and_then(|s| s.as_str()) == Some("agentvault") =>
            {
                Some(body)
            }
            _ => None,
        }
    }

    /// Where a vault may be: the address in the config and nowhere else, or
    /// every machine under the owner's login plus the addresses the config
    /// adds to the search.
    fn vault_candidates(&self) -> Vec<String> {
        let mut candidates: Vec<String> = vec![];
        if let Some(url) = &self.cfg.vault.url {
            candidates.push(clean_base(url));
            return candidates;
        }
        candidates.extend(self.cfg.vault.search.iter().map(|u| clean_base(u)));
        if self.cfg.discovery {
            if let Some(status) = tokio::task::block_in_place(crate::fleet::tailscale_status) {
                let me = status.pointer("/Self/UserID").and_then(|u| u.as_i64());
                let nodes = status.get("Self").into_iter().chain(
                    status
                        .get("Peer")
                        .and_then(|p| p.as_object())
                        .into_iter()
                        .flat_map(|m| m.values()),
                );
                for node in nodes {
                    let mine = node.get("UserID").and_then(|u| u.as_i64()) == me;
                    let tagged = node
                        .get("Tags")
                        .and_then(|t| t.as_array())
                        .is_some_and(|t| !t.is_empty());
                    let online = node.get("Online").and_then(|o| o.as_bool()).unwrap_or(true);
                    // Only the owner's own machines are asked. A tagged machine
                    // belongs to nobody, so a vault on one is named in the config.
                    let _ = tagged;
                    if mine && online {
                        if let Some(dns) = node
                            .get("DNSName")
                            .and_then(|d| d.as_str())
                            .map(|d| d.trim_end_matches('.'))
                            .filter(|d| !d.is_empty())
                        {
                            candidates.push(format!("https://{dns}"));
                        }
                    }
                }
            }
        }
        candidates
    }

    /// Check the whole chain from an agent to a key, repair what needs no
    /// approval, and tell the owner once of each thing that is left. `full`
    /// also asks the vault for its key list and looks at what panes need;
    /// the pass that runs every few seconds leaves those as last seen.
    pub(crate) async fn vault_check(&self, full: bool) {
        if !self.cfg.vault.enabled {
            return;
        }
        let _one = self.vault.checking.lock().await;
        let before = self.vault_view()["checks"].clone();
        self.vault_discover().await;
        self.vault_maintain().await;
        if full {
            let keys = self.vault_probe_keys().await;
            let d = self.clone();
            let panes = tokio::task::spawn_blocking(move || d.vault_probe_panes())
                .await
                .ok();
            let mut seen = self.vault.seen.lock().unwrap();
            seen.keys = keys;
            seen.panes = panes;
            seen.full_ms = now_ms();
        }
        let first = {
            let mut seen = self.vault.seen.lock().unwrap();
            let first = seen.checked_ms == 0;
            seen.checked_ms = now_ms();
            first
        };
        self.vault_alert(full);
        if first || before != self.vault_view()["checks"] {
            self.emit("vault.updated", None, None, self.vault_view());
        }
    }

    /// `vault.check`: the owner asked, or a Control Deck opened. An opening
    /// deck does not make the station ask again within a minute.
    pub async fn vault_check_now(&self, opened: bool) -> Value {
        let recent =
            now_ms().saturating_sub(self.vault.seen.lock().unwrap().full_ms) < OPEN_CHECK_EVERY_MS;
        if !(opened && recent) {
            self.vault_check(true).await;
        }
        self.vault_view()
    }

    /// What an agent's `vault_list` rests on: the station's token listing
    /// keys. None when an earlier link is broken and there is nothing to ask.
    async fn vault_probe_keys(&self) -> Option<std::result::Result<usize, String>> {
        let (base, token) = self.vault_ready().ok()?;
        {
            let seen = self.vault.seen.lock().unwrap();
            if !(seen.reachable && seen.unlocked && seen.token_ok == Some(true)) {
                return None;
            }
        }
        Some(
            match self.vault_get(&base, "/v1/secrets", Some(&token)).await {
                Ok((200, body)) => Ok(body
                    .as_array()
                    .or_else(|| body.get("secrets").and_then(|s| s.as_array()))
                    .map_or(0, |a| a.len())),
                Ok((status, body)) => {
                    Err(format!("HTTP {status} {}", said(&body)).trim().to_string())
                }
                Err(_) => Err("it did not answer".to_string()),
            },
        )
    }

    /// What a pane needs to reach the vault. No pane is ever given the token
    /// or a key: an agent calls spandeck's own tools (`spandeck mcp`), which
    /// ask this daemon over its socket. Claude Code, Codex and OpenCode are
    /// handed that server when their pane starts, so for them it is the
    /// program and the socket that must be there. Antigravity reads it from a
    /// plugin on disk, which is put right here if it is missing or stale.
    fn vault_probe_panes(&self) -> std::result::Result<String, String> {
        let runs = spandeck_terminal::sys::is_executable(&self.exe);
        if !runs {
            return Err(format!("spandeck's own program is missing at {}, so the vault tools cannot start in any pane: install spandeck again.", self.exe.display()));
        }
        let socket = self.paths.socket();
        if !socket.exists() {
            return Err(format!("spandeckd's socket is missing at {}, so the vault tools in panes cannot reach it: restart spandeckd.", socket.display()));
        }
        let mut how = "Claude Code, Codex and OpenCode panes are given spandeck's vault tools when they start".to_string();
        let antigravity = spandeck_agents::AgentKind::Antigravity;
        if !self.owns_agent_home() {
            // Its plugin is the station of the account's to keep.
            how.push_str("; Antigravity panes use the plugin the station of this account keeps, if there is one");
        } else if self
            .agent_probe(antigravity, Duration::from_secs(600))
            .path
            .is_some()
        {
            if let Some(problem) = self
                .antigravity_plugin(&self.with_tools(self.setup()))
                .into_iter()
                .next()
            {
                return Err(format!(
                    "Antigravity panes have no vault tools: {problem}. Move that folder away, then check again."
                ));
            }
            how.push_str("; Antigravity's spandeck plugin is in place");
        }
        Ok(how)
    }

    /// Tell the owner of each broken link once: not again while it stays
    /// broken, and not again after a restart. A token the vault refuses is
    /// told whenever it is seen; the rest when a full check finds them.
    fn vault_alert(&self, full: bool) {
        let seen = self.vault.seen.lock().unwrap().clone();
        let connected = self.vault_connected();
        let checks = self.vault_checks(&seen, &self.vault_base(), connected);
        let told: Vec<String> = self
            .db
            .kv_get("vault.alerted")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        let mut now_told: Vec<String> = vec![];
        for c in &checks {
            let Some(trouble) = c.trouble.filter(|_| connected) else {
                continue;
            };
            let key = format!("{}:{trouble}", c.id);
            if told.contains(&key) {
                now_told.push(key);
                continue;
            }
            if !(full || c.id == "token") {
                continue;
            }
            let title = match c.id {
                "address" if trouble == "moved" => "The vault seems to have moved",
                "address" => "The vault is not answering",
                "lock" => "The vault is locked",
                "token" if trouble == "unpinned" => {
                    "This station's vault token is not tied to this machine"
                }
                "token" => "This station's vault token was refused",
                "keys" => "The vault did not list its keys",
                _ => "Agents cannot reach the vault from their panes",
            };
            // The title has said what is wrong; the note goes on to what
            // to do about it, which these checks put after a colon.
            let body = match c.detail.split_once(": ") {
                Some((_, rest)) if matches!(c.id, "address" | "lock" | "token") => {
                    let mut chars = rest.chars();
                    chars
                        .next()
                        .map(|f| f.to_uppercase().chain(chars).collect::<String>())
                        .unwrap_or_default()
                }
                _ => c.detail.clone(),
            };
            self.notify("error", None, None, title, &body);
            self.record(
                "vault.alert",
                None,
                None,
                None,
                None,
                json!({"title": title, "check": c.id}),
            );
            now_told.push(key);
        }
        if now_told != told {
            self.db.kv_set("vault.alerted", &now_told.join(","));
        }
    }

    /// Keep the token healthy: check it, rotate it when due, and tell the
    /// owner about anything that changed outside the Control Deck.
    pub(crate) async fn vault_maintain(&self) {
        let (Some(base), Some(token)) = (self.vault_base(), self.vault_token()) else {
            return;
        };
        if !self.vault.seen.lock().unwrap().reachable {
            return;
        }
        let Ok((status, me)) = self.vault_get(&base, "/v1/tokens/self", Some(&token)).await else {
            return;
        };
        // The vault says why it refused in `code`; act on that, not on the text.
        let overdue =
            status == 403 && me.get("code").and_then(|c| c.as_str()) == Some("token_overdue");
        let mut notes: Vec<(&str, String)> = vec![];
        {
            let mut seen = self.vault.seen.lock().unwrap();
            if status == 200 {
                seen.token_ok = Some(true);
                // The vault itself refuses a tied token from another machine,
                // so one it accepts is tied here unless it is tied nowhere.
                let nodes: Option<Vec<String>> =
                    me.get("nodes").and_then(|n| n.as_array()).map(|n| {
                        n.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    });
                seen.token_trouble = nodes
                    .as_ref()
                    .is_some_and(|n| n.is_empty())
                    .then_some("unpinned");
                seen.token_nodes = nodes.unwrap_or_default();
                let theirs = me
                    .get("rotation")
                    .map(|r| r.get("every_hours").and_then(|h| h.as_u64()));
                seen.vault_rotation = theirs;
                let ours = self.rotation_hours();
                if let Some(theirs) = theirs {
                    if theirs != ours && !seen.alerted_mismatch {
                        seen.alerted_mismatch = true;
                        notes.push((
                            "The vault's token rotation setting was changed outside the Control Deck",
                            format!("The vault has {} for this station; the Control Deck has {}.", describe_hours(theirs), describe_hours(ours)),
                        ));
                    } else if theirs == ours {
                        seen.alerted_mismatch = false;
                    }
                }
            } else if (status == 401 || status == 403) && !overdue {
                // The owner is told by the check that called this.
                seen.token_ok = Some(false);
                seen.token_trouble = Some(if status == 403 {
                    "other_machine"
                } else {
                    "refused"
                });
            }
        }
        for (title, body) in notes {
            self.notify("error", None, None, title, &body);
            self.record(
                "vault.alert",
                None,
                None,
                None,
                None,
                json!({"title": title}),
            );
            self.emit("vault.updated", None, None, self.vault_view());
        }
        // Rotate on schedule. An overdue token can still do this one thing,
        // and does it whatever this station's own setting says: the vault's
        // word that it is overdue is enough.
        let due = self.rotation_hours().is_some_and(|hours| {
            let last = self
                .db
                .kv_get("vault.rotated_ms")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            now_ms().saturating_sub(last) >= hours * 3_600_000
        });
        if overdue || due {
            let swapped = self.vault_rotate().await.is_ok();
            if overdue {
                let mut seen = self.vault.seen.lock().unwrap();
                seen.token_ok = Some(swapped);
                seen.token_trouble = (!swapped).then_some("overdue");
            }
        }
    }

    /// Swap the station token for a fresh one. The new token is saved before it
    /// is ever used; the vault keeps the old one working until then.
    pub async fn vault_rotate(&self) -> Result<()> {
        let base = self.vault_base().ok_or_else(|| invalid("no vault found"))?;
        let token = self
            .vault_token()
            .ok_or_else(|| invalid("this station has no vault token"))?;
        let resp = self
            .fleet
            .http
            .post(format!("{base}/v1/tokens/self/rotate"))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| invalid(format!("vault unreachable: {e}")))?;
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let Some(new_token) = body
            .get("token")
            .and_then(|t| t.as_str())
            .filter(|_| status == 200)
        else {
            return Err(invalid(format!(
                "the vault did not rotate the token (HTTP {status})"
            )));
        };
        self.vault_save_token(new_token)?;
        self.db.kv_set("vault.rotated_ms", &now_ms().to_string());
        self.record("vault.rotated", None, None, None, None, json!({}));
        self.emit("vault.updated", None, None, self.vault_view());
        Ok(())
    }

    /// The owner's rotation setting for this station: None for never.
    pub async fn vault_set_rotation(&self, hours: Option<u64>, by: &str) -> Result<Value> {
        if hours.is_some_and(|h| h < 1) {
            return Err(invalid("the shortest rotation interval is 1 hour"));
        }
        // Not while a check is running: one that read the vault's setting
        // between this station's own being saved and the vault being told saw
        // the two differ, and told the owner it "was changed outside the
        // Control Deck" when the owner had just changed it here.
        let _one = self.vault.checking.lock().await;
        match hours {
            Some(h) => self.db.kv_set("vault.rotation_hours", &h.to_string()),
            None => self.db.kv_set("vault.rotation_hours", ""),
        }
        if hours.is_some()
            && self
                .db
                .kv_get("vault.rotated_ms")
                .is_none_or(|v| v.is_empty())
        {
            self.db.kv_set("vault.rotated_ms", &now_ms().to_string());
        }
        if let (Some(base), Some(token)) = (self.vault_base(), self.vault_token()) {
            let r = self
                .fleet
                .http
                .put(format!("{base}/v1/tokens/self/rotation"))
                .bearer_auth(&token)
                .json(&json!({"every_hours": hours}))
                .send()
                .await;
            if let Ok(resp) = r {
                if resp.status().is_success() {
                    let mut seen = self.vault.seen.lock().unwrap();
                    seen.vault_rotation = Some(hours);
                    seen.alerted_mismatch = false;
                }
            }
        }
        self.record(
            "vault.rotation_set",
            None,
            None,
            None,
            Some(by),
            json!({"every_hours": hours}),
        );
        self.emit("vault.updated", None, None, self.vault_view());
        Ok(self.vault_view())
    }

    /// The owner clicked Connect: ask the vault for a token. Nothing is issued
    /// until the owner approves on the vault's own page; this returns the
    /// address of that page and starts waiting to collect.
    pub async fn vault_connect(&self, by: &str) -> Result<Value> {
        if !self.cfg.vault.enabled {
            return Err(invalid("the vault is turned off in this station's config"));
        }
        self.vault_discover().await;
        // When the vault this station was connected to is gone and another
        // answers, that is the one the owner is asked to approve at.
        let replaced = {
            let seen = self.vault.seen.lock().unwrap();
            seen.elsewhere.clone().filter(|_| !seen.reachable)
        };
        let base = replaced
            .or_else(|| self.vault_base())
            .ok_or_else(|| invalid("no AgentVault was found on your tailnet"))?;
        let resp = self
            .fleet
            .http
            .post(format!("{base}/v1/token-requests"))
            .json(&json!({"station": self.station}))
            .send()
            .await
            .map_err(|e| invalid(format!("the vault did not answer: {e}")))?;
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let (Some(id), Some(url), Some(code)) = (
            body.get("request_id").and_then(|v| {
                v.as_str()
                    .map(String::from)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            }),
            body.get("approve_url").and_then(|v| v.as_str()),
            body.get("collect_code").and_then(|v| v.as_str()),
        ) else {
            let why = body
                .pointer("/error/message")
                .or_else(|| body.get("error"))
                .and_then(|m| m.as_str())
                .unwrap_or("it refused the request");
            return Err(invalid(format!(
                "the vault would not open a token request (HTTP {status}): {why}"
            )));
        };
        // The approval page must be on the vault itself.
        let approve_url = if url.starts_with('/') {
            format!("{base}{url}")
        } else {
            url.to_string()
        };
        if !same_origin(&approve_url, &base) {
            return Err(invalid(
                "the vault returned an approval address that is not on the vault",
            ));
        }
        let expires_ms = now_ms() + 10 * 60 * 1000;
        *self.vault.pending.lock().unwrap() = Some(Pending {
            request_id: id.clone(),
            approve_url: approve_url.clone(),
            expires_ms,
        });
        self.record(
            "vault.connect_requested",
            None,
            None,
            None,
            Some(by),
            json!({"request": id}),
        );
        self.emit("vault.updated", None, None, self.vault_view());

        let d = self.clone();
        let code = code.to_string();
        tokio::spawn(async move { d.vault_collect(base, id, code, expires_ms).await });
        Ok(json!({"approve_url": approve_url, "expires_ms": expires_ms}))
    }

    async fn vault_collect(&self, base: String, id: String, code: String, expires_ms: u64) {
        let outcome = loop {
            tokio::time::sleep(Duration::from_millis(700)).await;
            // A newer request replaced this one.
            if self
                .vault
                .pending
                .lock()
                .unwrap()
                .as_ref()
                .map(|p| p.request_id.as_str())
                != Some(id.as_str())
            {
                return;
            }
            if now_ms() > expires_ms {
                break "expired";
            }
            let resp = self
                .fleet
                .http
                .get(format!("{base}/v1/token-requests/{id}"))
                .header("x-collect-code", &code)
                .send()
                .await;
            let Ok(resp) = resp else { continue };
            // The vault forgets a request once it is collected, and all of them
            // when it restarts.
            if resp.status().as_u16() == 404 {
                break "forgotten by the vault (it may have restarted); press Connect again";
            }
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            if let Some(token) = body.get("token").and_then(|t| t.as_str()) {
                {
                    // The vault that issued the token is the station's vault.
                    let mut seen = self.vault.seen.lock().unwrap();
                    if seen.base.as_deref() != Some(base.as_str()) {
                        seen.base = Some(base.clone());
                        seen.reachable = true;
                        seen.elsewhere = None;
                        seen.moved_from = None;
                    }
                }
                if self.vault_save_token(token).is_err() {
                    break "could not save the token";
                }
                self.db.kv_set("vault.rotated_ms", &now_ms().to_string());
                // A later loss of the token is a new thing to offer to fix.
                self.db.kv_set("vault.prompt_dismissed", "");
                {
                    let mut seen = self.vault.seen.lock().unwrap();
                    seen.token_ok = Some(true);
                    seen.token_trouble = None;
                }
                let d = self.clone();
                tokio::spawn(async move { d.vault_check(true).await });
                break "connected";
            }
            match body.get("status").and_then(|s| s.as_str()) {
                Some("denied") => break "denied",
                Some("expired") => break "expired",
                _ => {}
            }
        };
        *self.vault.pending.lock().unwrap() = None;
        self.vault.seen.lock().unwrap().last_problem =
            (outcome != "connected").then(|| format!("The last connect request was {outcome}."));
        self.record(
            "vault.connect_result",
            None,
            None,
            None,
            None,
            json!({"request": id, "outcome": outcome}),
        );
        self.emit("vault.updated", None, None, self.vault_view());
    }

    pub fn vault_disconnect(&self, by: &str) -> Result<Value> {
        let _ = std::fs::remove_file(self.vault_token_file());
        self.db.kv_set("vault.base", "");
        *self.vault.pending.lock().unwrap() = None;
        {
            let mut seen = self.vault.seen.lock().unwrap();
            seen.token_ok = None;
            seen.token_trouble = None;
            seen.token_nodes.clear();
            seen.keys = None;
            seen.panes = None;
        }
        self.db.kv_set("vault.alerted", "");
        self.record("vault.disconnected", None, None, None, Some(by), json!({}));
        self.emit("vault.updated", None, None, self.vault_view());
        Ok(json!({
            "ok": true,
            "next": format!("This station forgot its token. Revoke `spandeck:{}` on the vault's Tokens page to end it at the vault too.", self.station),
        }))
    }

    // ------------------------------------------------------------- key use

    /// Which keys the caller may use. The owner may use any the token allows;
    /// a pane only those its project lists.
    fn vault_patterns(&self, caller: &Caller) -> Result<Option<Vec<String>>> {
        let Some(pane) = &caller.pane else {
            return Ok(None);
        };
        // With nothing set up, an agent may use every key the owner allowed
        // this station's token at the vault: connecting the station is the
        // one step. A project that lists keys narrows its agents to those.
        let meta: PaneMeta = self.pane(pane)?.meta();
        let Some(project) = meta.project else {
            return Ok(None);
        };
        let (_, file) = self.project_file(&project)?;
        if file.vault.keys.is_empty() {
            return Ok(None);
        }
        Ok(Some(file.vault.keys))
    }

    fn vault_ready(&self) -> Result<(String, String)> {
        let base = self
            .vault_base()
            .ok_or_else(|| invalid("no AgentVault is connected to this fleet"))?;
        let token = self.vault_token().ok_or_else(|| invalid("this station is not connected to the vault; the owner connects it from the Control Deck"))?;
        Ok((base, token))
    }

    fn vault_error(&self, status: u16, body: &Value) -> Error {
        let detail = body
            .pointer("/error/message")
            .or_else(|| body.get("error"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        match status {
            503 => invalid("the vault is locked; the owner unlocks it on the vault's page"),
            401 | 403 => Error::Denied(
                format!("the vault refused this station's token {detail}")
                    .trim()
                    .to_string(),
            ),
            404 => Error::NotFound("no such key in the vault".into()),
            s => invalid(
                format!("the vault answered HTTP {s} {detail}")
                    .trim()
                    .to_string(),
            ),
        }
    }

    pub async fn vault_list(&self, caller: &Caller) -> Result<Value> {
        let patterns = self.vault_patterns(caller)?;
        let (base, token) = self.vault_ready()?;
        let (status, body) = self
            .vault_get(&base, "/v1/secrets", Some(&token))
            .await
            .map_err(|e| invalid(format!("vault unreachable: {e}")))?;
        if status != 200 {
            return Err(self.vault_error(status, &body));
        }
        let items = body
            .as_array()
            .cloned()
            .or_else(|| body.get("secrets").and_then(|s| s.as_array()).cloned())
            .unwrap_or_default();
        let keys: Vec<Value> = items
            .into_iter()
            .filter(|k| {
                let name = k.get("name").and_then(|n| n.as_str()).unwrap_or("");
                patterns.as_ref().is_none_or(|ps| ps.iter().any(|p| glob_match(p, name)))
            })
            .map(|k| json!({"name": k["name"], "description": k.get("description"), "provider": k.get("provider"), "risk": k.get("risk")}))
            .collect();
        Ok(
            json!({"keys": keys, "how": "Call a provider with vault_call(name, path). The vault attaches the key; you never see it."}),
        )
    }

    fn vault_check_name(&self, caller: &Caller, name: &str) -> Result<()> {
        if name.is_empty()
            || name.contains("..")
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err(invalid("that is not a key name"));
        }
        if let Some(patterns) = self.vault_patterns(caller)? {
            if !patterns.iter().any(|p| glob_match(p, name)) {
                return Err(Error::Denied(format!(
                    "this project is not allowed the key {name}"
                )));
            }
        }
        Ok(())
    }

    pub async fn vault_describe(&self, caller: &Caller, name: &str) -> Result<Value> {
        self.vault_check_name(caller, name)?;
        let (base, token) = self.vault_ready()?;
        let (status, body) = self
            .vault_get(&base, &format!("/v1/secrets/{name}"), Some(&token))
            .await
            .map_err(|e| invalid(format!("vault unreachable: {e}")))?;
        if status != 200 {
            return Err(self.vault_error(status, &body));
        }
        Ok(body)
    }

    /// Call a provider through the vault with the key attached on the vault's
    /// side. The ship's log gets who used which key, never what was sent.
    pub async fn vault_call(
        &self,
        caller: &Caller,
        name: &str,
        path: &str,
        method: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        self.vault_check_name(caller, name)?;
        let (base, token) = self.vault_ready()?;
        let path = path.trim_start_matches('/');
        // An encoded dot or slash would let a path climb out of this key's
        // proxy to the vault's own API, which answers to the station's token.
        let lower = path.to_ascii_lowercase();
        if path.contains("..")
            || ["%2e", "%2f", "%5c", "%25"]
                .iter()
                .any(|e| lower.contains(e))
            || path.contains("://")
            || path.contains('\\')
            || path.chars().any(|c| c.is_control())
        {
            return Err(invalid("that path is not allowed"));
        }
        let method = method.to_uppercase();
        let m = match method.as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "PATCH" => reqwest::Method::PATCH,
            "DELETE" => reqwest::Method::DELETE,
            _ => return Err(invalid("method is one of GET, POST, PUT, PATCH, DELETE")),
        };
        let actor = match caller.pane.as_ref().and_then(|id| self.pane(id).ok()) {
            Some(p) => {
                let meta = p.meta();
                format!(
                    "{}/{}@{}",
                    meta.project.unwrap_or_default(),
                    meta.name,
                    self.station
                )
            }
            None => format!("owner@{}", self.station),
        };
        // Check where the request really goes after the address is parsed.
        let prefix = format!("/v1/proxy/{name}/");
        let url = reqwest::Url::parse(&format!("{base}{prefix}{path}"))
            .ok()
            .filter(|u| same_origin(u.as_str(), &base) && u.path().starts_with(&prefix))
            .ok_or_else(|| invalid("that path is not allowed"))?;
        let mut req = self
            .fleet
            .http
            .request(m, url)
            .timeout(Duration::from_secs(120))
            .bearer_auth(&token)
            .header("x-agentvault-actor", &actor);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| invalid(format!("vault unreachable: {e}")))?;
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = resp.bytes().await.map_err(|e| invalid(e.to_string()))?;
        // The vault's own "locked" answer is not the provider's: say so plainly.
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(400)]).to_lowercase();
        if status == 503 && (head.contains("sealed") || head.contains("locked")) {
            let mut seen = self.vault.seen.lock().unwrap();
            seen.unlocked = false;
            return Err(invalid(
                "the vault is locked; the owner unlocks it on the vault's page",
            ));
        }
        let prefix: String = path
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .split('/')
            .take(2)
            .collect::<Vec<_>>()
            .join("/");
        self.record(
            "vault.use",
            caller.pane.as_deref(),
            None,
            None,
            Some(&caller.who),
            json!({"key": name, "method": method, "path": prefix, "status": status, "actor": actor}),
        );
        const CAP: usize = 400_000;
        let truncated = bytes.len() > CAP;
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(CAP)]).to_string();
        let parsed = (!truncated && content_type.contains("json"))
            .then(|| serde_json::from_str::<Value>(&text).ok())
            .flatten();
        Ok(
            json!({"status": status, "content_type": content_type, "json": parsed, "text": if parsed.is_some() { None } else { Some(text) }, "truncated": truncated}),
        )
    }

    /// Tell the vault a key was seen where it should not be. The vault always
    /// gives the same reply, so nothing is learned from it; only the fact that
    /// a report was sent is logged, never the value.
    pub(crate) async fn vault_report(&self, value: &str, seen_in: &str) {
        let (Some(base), Some(token)) = (self.vault_base(), self.vault_token()) else {
            return;
        };
        let sent = self
            .fleet
            .http
            .post(format!("{base}/v1/attention/report"))
            .bearer_auth(&token)
            .json(&json!({"value": value, "place": seen_in.chars().take(40).collect::<String>()}))
            .send()
            .await
            .is_ok();
        self.record(
            "vault.reported",
            None,
            None,
            None,
            None,
            json!({"where": seen_in, "sent": sent}),
        );
    }

    /// Keys the archive found in session files go to the vault as exposed.
    pub(crate) async fn vault_report_archive(&self) {
        let found = self.archive.take_found_keys();
        if found.is_empty() || !self.vault_connected() {
            return;
        }
        for (_, value) in found {
            self.vault_report(&value, "a session transcript").await;
        }
    }
}

fn describe_hours(h: Option<u64>) -> String {
    match h {
        None => "never".into(),
        Some(1) => "every hour".into(),
        Some(n) => format!("every {n} hours"),
    }
}

/// Whether two addresses share a scheme, host and port.
fn same_origin(a: &str, b: &str) -> bool {
    match (reqwest::Url::parse(a), reqwest::Url::parse(b)) {
        (Ok(a), Ok(b)) => {
            a.scheme() == b.scheme()
                && a.host_str() == b.host_str()
                && a.port_or_known_default() == b.port_or_known_default()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn key_patterns_match_like_the_vault_does() {
        assert!(glob_match("OPENAI_*", "OPENAI_API_KEY"));
        assert!(glob_match("*", "ANYTHING"));
        assert!(glob_match("GITHUB_TOKEN", "GITHUB_TOKEN"));
        assert!(glob_match("*_KEY", "OPENAI_API_KEY"));
        assert!(glob_match("A*B*C", "AxxBxxC"));
        assert!(!glob_match("OPENAI_*", "ANTHROPIC_API_KEY"));
        assert!(!glob_match("GITHUB_TOKEN", "GITHUB_TOKEN_2"));
        assert!(!glob_match("A*B", "A"));
    }
}
