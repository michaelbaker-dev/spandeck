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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    /// The vault's address, for a vault that discovery cannot find (another
    /// port). Normally left unset: spandeck finds the vault each time.
    pub url: Option<String>,
}

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
    alerted_rejected: bool,
    alerted_mismatch: bool,
    last_problem: Option<String>,
}

#[derive(Default)]
pub struct VaultState {
    seen: Mutex<Seen>,
    pending: Mutex<Option<Pending>>,
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
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = self.vault_token_file().with_extension("token.new");
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(token.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, self.vault_token_file())
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
    }

    fn rotation_hours(&self) -> Option<u64> {
        self.db
            .kv_get("vault.rotation_hours")
            .and_then(|v| v.parse().ok())
    }

    /// What the Control Deck shows for this station. No network call.
    pub fn vault_view(&self) -> Value {
        let seen = self.vault.seen.lock().unwrap().clone();
        let base = self.vault_base();
        let connected = self.vault_connected();
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
        json!({
            "found": base.is_some(), "url": base, "state": state, "has_token": connected,
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
        let mut candidates: Vec<String> = vec![];
        if let Some(url) = &self.cfg.vault.url {
            candidates.push(clean_base(url));
        } else if self.cfg.discovery {
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
                    // The vault machine may be tagged: it then belongs to nobody.
                    if (mine || tagged) && online {
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
        // Ask the one we already know first.
        if let Some(known) = self.vault.seen.lock().unwrap().base.clone() {
            candidates.retain(|c| *c != known);
            candidates.insert(0, known);
        }
        let mut found: Option<(String, Value)> = None;
        for base in candidates {
            let probe = tokio::time::timeout(
                Duration::from_millis(2000),
                self.vault_get(&base, "/v1/status", None),
            )
            .await;
            if let Ok(Ok((200, body))) = probe {
                if body.get("service").and_then(|s| s.as_str()) == Some("agentvault") {
                    found = Some((base, body));
                    break;
                }
            }
        }
        let changed = {
            let mut seen = self.vault.seen.lock().unwrap();
            let before = (seen.base.clone(), seen.reachable, seen.unlocked);
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
            before != (seen.base.clone(), seen.reachable, seen.unlocked)
        };
        if changed {
            self.emit("vault.updated", None, None, self.vault_view());
        }
    }

    /// Keep the token healthy: check it, rotate it when due, and tell the
    /// owner about anything that changed outside the Control Deck.
    pub(crate) async fn vault_maintain(&self) {
        let (Some(base), Some(token)) = (self.vault_base(), self.vault_token()) else {
            return;
        };
        let Ok((status, me)) = self.vault_get(&base, "/v1/tokens/self", Some(&token)).await else {
            return;
        };
        let overdue = me.get("overdue").and_then(|o| o.as_bool()).unwrap_or(false);
        let mut notes: Vec<(&str, String)> = vec![];
        {
            let mut seen = self.vault.seen.lock().unwrap();
            if status == 200 {
                seen.token_ok = Some(true);
                seen.alerted_rejected = false;
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
                seen.token_ok = Some(false);
                if !seen.alerted_rejected {
                    seen.alerted_rejected = true;
                    notes.push((
                        "This station's vault token was refused",
                        "It was revoked, or swapped by something other than spandeck. Connect the station again from the Control Deck.".to_string(),
                    ));
                }
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
        // Rotate on schedule. An overdue token can still do this one thing.
        if let Some(hours) = self.rotation_hours() {
            let last = self
                .db
                .kv_get("vault.rotated_ms")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            if overdue || now_ms().saturating_sub(last) >= hours * 3_600_000 {
                let _ = self.vault_rotate().await;
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
        self.vault_discover().await;
        let base = self
            .vault_base()
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
        if !approve_url.starts_with(&base) {
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
                .header("x-agentvault-collect-code", &code)
                .send()
                .await;
            let Ok(resp) = resp else { continue };
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            if let Some(token) = body.get("token").and_then(|t| t.as_str()) {
                if self.vault_save_token(token).is_err() {
                    break "could not save the token";
                }
                self.db.kv_set("vault.rotated_ms", &now_ms().to_string());
                {
                    let mut seen = self.vault.seen.lock().unwrap();
                    seen.token_ok = Some(true);
                    seen.alerted_rejected = false;
                }
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
        *self.vault.pending.lock().unwrap() = None;
        self.vault.seen.lock().unwrap().token_ok = None;
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
        let meta: PaneMeta = self.pane(pane)?.meta();
        let project = meta.project.ok_or_else(|| {
            Error::Denied("this pane is not in a project, so it has no keys".into())
        })?;
        let (_, file) = self.project_file(&project)?;
        if file.vault.keys.is_empty() {
            return Err(Error::Denied(format!(
                "project {project} lists no keys. The owner adds them under [vault] keys in .spandeck/project.toml"
            )));
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
        if path.contains("..")
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
        let mut req = self
            .fleet
            .http
            .request(m, format!("{base}/v1/proxy/{name}/{path}"))
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
        if status == 503
            && String::from_utf8_lossy(&bytes[..bytes.len().min(400)]).contains("locked")
        {
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
            .json(&json!({"value": value, "where": format!("{seen_in} on {}", self.station), "found_ms": now_ms()}))
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
