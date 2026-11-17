//! Notifications: one inbox, and a push only when you are away.
//!
//! Rules: a muted pane never notifies; a pane that needs you or failed always
//! lands in the inbox; a finished turn lands there for watched panes. A push
//! (ntfy or webhook) is sent only if you are not at a keyboard.

use serde_json::{json, Value};
use spandeck_agents::AgentState;

use crate::daemon::{Daemon, PaneMeta};
use crate::paths::{new_id, now_ms};

impl Daemon {
    pub(crate) fn notify_state(&self, meta: &PaneMeta, _old: AgentState, new: AgentState) {
        if meta.muted || !meta.agent.is_agent() {
            return;
        }
        let configured = &self.cfg.notify.states;
        let wanted = |s: &str| configured.is_empty() || configured.iter().any(|c| c == s);
        let (kind, title) = match new {
            AgentState::NeedsYou if wanted("needs_you") => {
                ("needs_you", format!("{} needs you", meta.name))
            }
            AgentState::Error if wanted("error") => {
                ("error", format!("{} hit an error", meta.name))
            }
            AgentState::Done if meta.watch || configured.iter().any(|c| c == "done") => {
                ("done", format!("{} finished", meta.name))
            }
            _ => return,
        };
        let body = meta
            .needs_reason
            .clone()
            .or(meta.last_message.clone())
            .unwrap_or_default();
        self.notify(kind, Some(&meta.id), meta.project.as_deref(), &title, &body);
    }

    pub fn notify(
        &self,
        kind: &str,
        pane: Option<&str>,
        project: Option<&str>,
        title: &str,
        body: &str,
    ) {
        let _ = self.notify_as(kind, pane, project, title, body, true);
    }

    /// A notice every station of the fleet raises in the same words, which
    /// the Control Deck shows as one. `push`: whether this is the station
    /// that also sends it to the phone (by the usual rules: a sink is set,
    /// and the owner is not at a keyboard). The others mark theirs 3, "left
    /// to another station".
    pub(crate) fn notify_with(&self, kind: &str, title: &str, body: &str, push: bool) {
        let _ = self.notify_as(kind, None, None, title, body, push);
    }

    /// A notice about the station itself whose words may be replaced later
    /// (`notify_rewrite`): returns its id.
    pub(crate) fn notify_station(&self, kind: &str, title: &str, body: &str) -> String {
        self.notify_as(kind, None, None, title, body, true)
    }

    /// The same, for a notice that is progress: it is not sent to the phone.
    pub(crate) fn notify_quiet(&self, kind: &str, title: &str, body: &str) -> String {
        self.notify_as(kind, None, None, title, body, false)
    }

    /// Replace the words of a notice already raised, and show it as unread
    /// again. It is not pushed a second time. False when it is no longer
    /// in the inbox.
    pub(crate) fn notify_rewrite(&self, id: &str, kind: &str, title: &str, body: &str) -> bool {
        let changed = self
            .db
            .with(|c| {
                c.execute(
                    "UPDATE notifications SET title=?2, body=?3, read=0 WHERE id=?1",
                    rusqlite::params![id, title, body],
                )
            })
            .unwrap_or(0);
        if changed == 0 {
            return false;
        }
        self.emit(
            "notify.new",
            None,
            None,
            json!({"id": id, "kind": kind, "title": title, "body": body, "url": null, "station": self.station}),
        );
        self.notify_relay(json!({"id": id, "kind": kind, "title": title, "body": body}));
        true
    }

    fn notify_as(
        &self,
        kind: &str,
        pane: Option<&str>,
        project: Option<&str>,
        title: &str,
        body: &str,
        push: bool,
    ) -> String {
        let id = new_id("n");
        let url = pane.map(|p| format!("/#/s/{}/p/{}", self.station, p));
        let pushed = if push { 0 } else { 3 };
        // Raised from hooks and from the stop path: never waited for, kept
        // until it is written (db.rs). Decks are told at once, below.
        self.db.put(crate::db::Op::Notice {
            id: id.clone(),
            ts: now_ms() as i64,
            kind: kind.to_string(),
            pane: pane.map(String::from),
            project: project.map(String::from),
            title: title.to_string(),
            body: body.to_string(),
            url: url.clone(),
            pushed,
        });
        self.emit(
            "notify.new",
            pane,
            project,
            json!({"id": id, "kind": kind, "title": title, "body": body, "url": url, "station": self.station}),
        );
        // A Control Deck open on another station hears of it too.
        self.notify_relay(
            json!({"id": id, "kind": kind, "title": title, "body": body, "url": url}),
        );
        id
    }

    /// Tell the other stations of the fleet, so that a Control Deck served by
    /// any of them shows what this one raised (or that its inbox was read).
    /// Nothing is stored there: the notice stays in this station's inbox.
    /// A notice can be raised from a thread that is not the runtime's.
    fn notify_relay(&self, what: Value) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let d = self.clone();
        rt.spawn(async move {
            let online = d.fleet.peers().into_iter().filter(|p| p.state == "online");
            let calls = online.map(|p| {
                let (d, what) = (d.clone(), what.clone());
                async move { d.fleet.call(&p.station, "notify.relay", what).await }
            });
            // A station from before this refuses the call: a deck there shows
            // the notice at its next look at this station's inbox.
            let _ = crate::fleet::futures_join_all(calls).await;
        });
    }

    /// Another station of the fleet saying it raised a notice, or that its
    /// inbox was read. A station's signed call only. What it says is text
    /// from another machine: bounded, cleaned, shown to the decks connected
    /// here and kept nowhere.
    pub(crate) fn notify_relayed(&self, from: &str, params: &Value) -> Value {
        let text = |key: &str, max: usize| -> String {
            params
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(max)
                .collect()
        };
        if params.get("read").is_some() {
            self.emit("notify.read", None, None, json!({"station": from}));
            return json!({"ok": true});
        }
        // A link is only ever a place in the Control Deck.
        let url = Some(text("url", 300)).filter(|u| u.starts_with("/#/"));
        self.emit(
            "notify.new",
            None,
            None,
            json!({
                "id": text("id", 64), "kind": text("kind", 32), "title": text("title", 200),
                "body": text("body", 2000), "url": url, "station": from, "relayed": true,
            }),
        );
        json!({"ok": true})
    }

    pub fn notifications(&self, unread_only: bool, limit: u32) -> Vec<Value> {
        let mut list: Vec<Value> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT id,ts,kind,pane,project,title,body,url,read,pushed FROM notifications
                     WHERE (?1=0 OR read=0) ORDER BY ts DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(rusqlite::params![unread_only as i64, limit], |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?, "ts": r.get::<_, i64>(1)?, "kind": r.get::<_, String>(2)?,
                        "pane": r.get::<_, Option<String>>(3)?, "project": r.get::<_, Option<String>>(4)?,
                        "title": r.get::<_, String>(5)?, "body": r.get::<_, String>(6)?,
                        "url": r.get::<_, Option<String>>(7)?, "read": r.get::<_, i64>(8)? != 0,
                        // 0 waiting, 1 pushed, 2 held back because you were at a keyboard,
                        // 3 left to another station of the fleet to push
                        "pushed": r.get::<_, i64>(9)?, "station": self.station,
                    }))
                })?;
                rows.collect()
            })
            .unwrap_or_default();
        // A notice the database has not taken yet is listed all the same:
        // the one that says the database cannot be written is among them.
        for op in self.db.pending() {
            if let crate::db::Op::Notice {
                id,
                ts,
                kind,
                pane,
                project,
                title,
                body,
                url,
                pushed,
            } = op
            {
                if !list.iter().any(|n| n["id"] == id.as_str()) {
                    list.insert(
                        0,
                        json!({
                            "id": id, "ts": ts, "kind": kind, "pane": pane, "project": project,
                            "title": title, "body": body, "url": url, "read": false,
                            "pushed": pushed, "station": self.station,
                        }),
                    );
                }
            }
        }
        list.truncate(limit as usize);
        list
    }

    pub fn notifications_read(&self, id: Option<&str>) {
        let _ = self.db.with(|c| match id {
            Some(id) => c.execute("UPDATE notifications SET read=1 WHERE id=?1", [id]),
            None => c.execute("UPDATE notifications SET read=1", []),
        });
        self.emit("notify.read", None, None, json!({"id": id}));
        self.notify_relay(json!({"read": true}));
    }

    /// The owner is at a Control Deck served by this station: say so to the
    /// other stations, so that none of them pushes to the phone meanwhile.
    /// It is the owner's own `presence.touch`, passed on as a deck's calls to
    /// another station are, so a station of any version takes it.
    pub(crate) fn presence_relay(&self) {
        let d = self.clone();
        tokio::spawn(async move {
            let online = d.fleet.peers().into_iter().filter(|p| p.state == "online");
            let calls = online.map(|p| {
                let d = d.clone();
                async move {
                    let touch = json!({"method": "presence.touch", "params": {}});
                    d.fleet.call(&p.station, "rpc.forward", touch).await
                }
            });
            let _ = crate::fleet::futures_join_all(calls).await;
        });
    }

    /// Decide, for each new notification, whether it is pushed or held back.
    pub(crate) async fn pump_notifications(&self) {
        let pending: Vec<(String, String, String, String, Option<String>)> = self
            .db
            .read(|c| {
                let mut stmt = c.prepare("SELECT id,kind,title,body,url FROM notifications WHERE pushed=0 ORDER BY ts LIMIT 20")?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
                rows.collect()
            })
            .unwrap_or_default();
        if pending.is_empty() {
            return;
        }
        let at_keyboard = self.at_keyboard();
        for (id, kind, title, body, url) in pending {
            let mark = if at_keyboard {
                2
            } else {
                self.push(&kind, &title, &body, url.as_deref()).await;
                1
            };
            let _ = self.db.with(|c| {
                c.execute(
                    "UPDATE notifications SET pushed=?2 WHERE id=?1",
                    rusqlite::params![id, mark],
                )
            });
        }
    }

    /// Where a pushed notice leads: the pane's place in this station's
    /// Control Deck, at the station's own tailnet address, which is where a
    /// phone reaches it. None for a notice about no pane, and on a station
    /// with no tailnet address, from which a link would lead nowhere.
    async fn click_url(&self, url: Option<&str>) -> Option<String> {
        let path = url.filter(|u| u.starts_with("/#/"))?;
        let d = self.clone();
        let me = tokio::task::spawn_blocking(move || d.fleet.tailnet_self())
            .await
            .ok()??;
        let ip = me
            .ips
            .iter()
            .find(|i| i.parse::<std::net::Ipv4Addr>().is_ok())?;
        Some(format!("http://{ip}:{}{path}", self.cfg.web_port))
    }

    async fn push(&self, kind: &str, title: &str, body: &str, url: Option<&str>) {
        let client = &self.fleet.http;
        let title = format!("{} · {}", title, self.station);
        if let Some(ntfy) = &self.cfg.notify.ntfy_url {
            let mut req = client
                .post(ntfy)
                .header("Title", title.clone())
                .header("Tags", kind.to_string())
                .body(body.to_string());
            if matches!(kind, "needs_you" | "approval" | "error") {
                req = req.header("Priority", "high");
            }
            // Touching the notice opens the pane in the Control Deck, which
            // on a phone shows an agent's pane as its conversation.
            if let Some(click) = self.click_url(url).await {
                req = req.header("Click", click);
            }
            if let Err(e) = req.send().await {
                tracing::warn!(error = %e, "ntfy push failed");
            }
        }
        if let Some(hook) = &self.cfg.notify.webhook_url {
            let payload = json!({"kind": kind, "title": title, "body": body, "url": url, "station": self.station});
            if let Err(e) = client.post(hook).json(&payload).send().await {
                tracing::warn!(error = %e, "webhook push failed");
            }
        }
    }
}
