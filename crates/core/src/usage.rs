//! Usage: what each pane's agent has spent, and the station's totals.
//!
//! The token counts come from the session archive, which reads them from the
//! agents' own session data. A cost is an estimate at list prices
//! (`prices.rs`); nothing is estimated for a model the list does not name.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use spandeck_agents::AgentKind;
use spandeck_archive::usage::{opencode_db, Tally, Usage};

use crate::daemon::{Daemon, PaneEntry, PaneMeta};
use crate::paths::now_ms;
use crate::prices::Prices;

/// Totals for one model, or several added up.
#[derive(Default, Clone)]
struct Sum {
    all: Tally,
    side: i64,
    turns: i64,
}

impl Sum {
    fn add(&mut self, tally: &Tally, side: bool, turns: i64) {
        self.all.add(tally);
        if side {
            self.side += tally.total();
        }
        self.turns += turns;
    }
    fn is_empty(&self) -> bool {
        self.all.total() == 0 && self.turns == 0
    }
}

/// Usage keyed by (provider, model).
#[derive(Default, Clone)]
struct ByModel(BTreeMap<(Option<String>, String), Sum>);

impl ByModel {
    fn add(
        &mut self,
        provider: &Option<String>,
        model: &str,
        tally: &Tally,
        side: bool,
        turns: i64,
    ) {
        self.0
            .entry((provider.clone(), model.to_string()))
            .or_default()
            .add(tally, side, turns);
    }

    /// `turns`: `None` for an agent that does not record them.
    fn json(&self, prices: &Prices, count_turns: bool) -> Value {
        let mut total = Sum::default();
        let mut cost: Option<f64> = None;
        let mut unpriced = vec![];
        let mut rows = vec![];
        for ((provider, model), s) in &self.0 {
            if s.is_empty() {
                continue;
            }
            total.all.add(&s.all);
            total.side += s.side;
            total.turns += s.turns;
            let c = prices.cost(provider.as_deref(), model, &s.all);
            match c {
                Some(c) => cost = Some(cost.unwrap_or(0.0) + c),
                None if s.all.total() > 0 => unpriced.push(model.clone()),
                None => {}
            }
            let mut row = tokens(&s.all);
            row["model"] = json!(model);
            row["provider"] = json!(provider);
            row["subagent_tokens"] = json!(s.side);
            row["cost_estimate"] = json!(c);
            row["turns"] = if count_turns {
                json!(s.turns)
            } else {
                Value::Null
            };
            rows.push(row);
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r["total_tokens"].as_i64().unwrap_or(0)));
        let mut v = tokens(&total.all);
        v["subagent_tokens"] = json!(total.side);
        v["turns"] = if count_turns {
            json!(total.turns)
        } else {
            Value::Null
        };
        v["cost_estimate"] = json!(cost);
        v["unpriced_models"] = json!(unpriced);
        v["by_model"] = json!(rows);
        v
    }
}

fn tokens(t: &Tally) -> Value {
    json!({
        "input_tokens": t.input, "output_tokens": t.output,
        "cache_read_tokens": t.cache_read, "cache_write_tokens": t.cache_write,
        "total_tokens": t.total(),
    })
}

fn source_of(agent: AgentKind) -> Option<&'static str> {
    match agent {
        AgentKind::Claude | AgentKind::Fake => Some("Claude Code session file"),
        AgentKind::Codex => Some("Codex session file"),
        AgentKind::OpenCode => Some("OpenCode session database"),
        _ => None,
    }
}

impl Daemon {
    /// A pane as callers see it: its view plus what its agent has used.
    pub fn pane_view(&self, p: &PaneEntry) -> Value {
        let mut v = p.view(&self.station);
        let meta = p.meta();
        v["usage"] = self.pane_usage(&meta);
        // Whether the agent has said something the owner has not read, as
        // this station keeps it: the same answer from every station's deck.
        v["unread"] = json!(self.conversation_unread(&meta));
        // Stopped and staying stopped, or waiting for the owner after a
        // restart (holds.rs); null when neither.
        v["held"] = self.held_view(&meta);
        // A question or a plan its hook holds for the owner: which, and
        // until when; null when none. Never what it says (questions.rs).
        v["asks"] = self.asks_view(&meta.id);
        // Its program's folder-trust question: on the screen now, or
        // answered, and by whom (foldertrust.rs); null when neither.
        v["trust"] = self.folder_trust_view(&meta);
        // A deck from before holds shows only this: said where the pane
        // has no reason of its own to show.
        if meta.needs_reason.is_none() && meta.exit.is_none() {
            if let Some(words) = self.held_words(&meta) {
                v["needs_reason"] = json!(words);
            }
        }
        v
    }

    fn usage_of(&self, meta: &PaneMeta) -> Option<Usage> {
        let sid = meta.session_id.as_deref()?;
        if let Some(u) = self.archive.usage(sid) {
            return Some(u);
        }
        // Not counted yet: a conversation from before this version, or an
        // agent whose totals are in its own database.
        let path = match meta.agent {
            AgentKind::OpenCode => Some(opencode_db(&self.home).display().to_string()),
            _ => meta.transcript_path.clone(),
        }?;
        self.archive.track_usage(
            sid,
            meta.agent.as_str(),
            &path,
            meta.project.as_deref(),
            Some(&meta.id),
        );
        self.archive.usage(sid)
    }

    /// `null` for a shell. For an agent, every field is null until its
    /// session data says otherwise.
    pub fn pane_usage(&self, meta: &PaneMeta) -> Value {
        if !meta.agent.is_agent() {
            return Value::Null;
        }
        let prices = self.prices();
        let source = source_of(meta.agent);
        let usage = source.and_then(|_| self.usage_of(meta));
        let reported = usage.as_ref().is_some_and(|u| !u.by_model.is_empty());
        let u = usage.unwrap_or_default();
        let mut by = ByModel::default();
        for (model, m) in &u.by_model {
            by.add(&u.provider, model, &m.main, false, m.turns as i64);
            by.add(&u.provider, model, &m.side, true, 0);
        }
        let mut v = by.json(&prices, u.counts_turns);
        let listed = u
            .model
            .as_deref()
            .and_then(|m| prices.find(u.provider.as_deref(), m));
        let (window, window_source) = match (u.context_window, listed.and_then(|p| p.context)) {
            (Some(w), _) => (Some(w), Some("session file")),
            (None, Some(w)) => (Some(w), Some("price list")),
            _ => (None, None),
        };
        let pct = match (u.context_tokens, window) {
            (Some(t), Some(w)) if w > 0 => Some((t as f64 * 100.0 / w as f64).round().min(100.0)),
            _ => None,
        };
        v["model"] = json!(u.model);
        v["provider"] = json!(u.provider);
        v["context_tokens"] = json!(u.context_tokens);
        v["context_window"] = json!(window);
        v["context_window_source"] = json!(window_source);
        v["context_pct"] = json!(pct);
        v["turns"] = json!(u.turns());
        v["updated_ms"] = json!(u.updated_ms);
        v["source"] = json!(source.filter(|_| reported));
        v["reported_cost_usd"] = json!(u.reported_cost_usd);
        v["price_source"] = json!(prices.source());
        v["price_fetched_ms"] = json!(prices.fetched_ms());
        v["note"] = json!(if source.is_none() {
            Some("not reported by this agent")
        } else if !reported {
            Some("nothing reported yet")
        } else {
            None
        });
        if !reported {
            for k in [
                "input_tokens",
                "output_tokens",
                "cache_read_tokens",
                "cache_write_tokens",
                "total_tokens",
                "subagent_tokens",
            ] {
                v[k] = Value::Null;
            }
        }
        v
    }

    /// Tell the Control Deck which panes have new numbers.
    pub(crate) fn usage_changed(&self, sessions: &[String]) {
        if sessions.is_empty() {
            return;
        }
        for p in self.list_panes() {
            let m = p.meta.lock().unwrap();
            if m.session_id.as_ref().is_some_and(|s| sessions.contains(s)) {
                self.emit("pane.usage", Some(&p.id), m.project.as_deref(), json!({}));
            }
        }
    }

    fn pane_name(&self, id: &str) -> Option<String> {
        if let Ok(p) = self.pane(id) {
            return Some(p.meta.lock().unwrap().name.clone());
        }
        self.db
            .read(|c| {
                c.query_row(
                    "SELECT json_extract(meta, '$.name') FROM panes WHERE id=?1",
                    [id],
                    |r| r.get::<_, Option<String>>(0),
                )
            })
            .ok()
            .flatten()
    }

    /// Station totals from the same data: per pane, per project and per
    /// model, for today and for all time. "Today" starts at `today_from_ms`
    /// (the caller's local midnight; default: midnight UTC), rounded down to
    /// the hour.
    pub fn usage_summary(&self, today_from_ms: Option<u64>) -> Value {
        // Make sure every live pane's conversation is being counted.
        for p in self.list_panes() {
            let meta = p.meta();
            if meta.agent.is_agent() && source_of(meta.agent).is_some() {
                let _ = self.usage_of(&meta);
            }
        }
        let now = now_ms();
        let from = today_from_ms.unwrap_or(now - now % 86_400_000);
        let from = from - from % 3_600_000;
        let prices = self.prices();
        #[derive(Default)]
        struct Both {
            today: ByModel,
            all: ByModel,
        }
        impl Both {
            fn json(&self, prices: &Prices) -> Value {
                json!({"today": self.today.json(prices, true), "all": self.all.json(prices, true)})
            }
        }
        struct PaneRow {
            agent: String,
            project: Option<String>,
            sums: Both,
        }
        let mut station = Both::default();
        let mut projects: BTreeMap<Option<String>, Both> = BTreeMap::new();
        let mut panes: BTreeMap<String, PaneRow> = BTreeMap::new();
        for r in self.archive.usage_rows(from) {
            let key = r.pane.clone().unwrap_or_else(|| r.session.clone());
            let pane = panes.entry(key).or_insert_with(|| PaneRow {
                agent: r.agent.clone(),
                project: r.project.clone(),
                sums: Both::default(),
            });
            let turns = |t: i64| if r.side { 0 } else { t };
            for both in [
                &mut station,
                projects.entry(r.project.clone()).or_default(),
                &mut pane.sums,
            ] {
                both.today.add(
                    &r.provider,
                    &r.model,
                    &r.today,
                    r.side,
                    turns(r.today_turns),
                );
                both.all
                    .add(&r.provider, &r.model, &r.all, r.side, turns(r.all_turns));
            }
        }
        let live: BTreeMap<String, PaneMeta> = self
            .list_panes()
            .iter()
            .map(|p| (p.id.clone(), p.meta()))
            .collect();
        let mut pane_rows: Vec<Value> = panes
            .iter()
            .map(|(id, row)| {
                let mut v = row.sums.json(&prices);
                let meta = live.get(id);
                v["pane"] = json!(id);
                v["name"] = json!(self.pane_name(id));
                v["project"] = json!(row.project);
                v["agent"] = json!(row.agent);
                v["live"] = json!(meta.is_some_and(|m| m.exit.is_none()));
                v
            })
            .collect();
        pane_rows
            .sort_by_key(|r| std::cmp::Reverse(r["all"]["total_tokens"].as_i64().unwrap_or(0)));
        let project_rows: Vec<Value> = projects
            .iter()
            .map(|(name, both)| {
                let mut v = both.json(&prices);
                v["project"] = json!(name);
                v
            })
            .collect();
        let mut v = station.json(&prices);
        v["station"] = json!(self.station);
        v["today_from_ms"] = json!(from);
        v["price_source"] = json!(prices.source());
        v["price_fetched_ms"] = json!(prices.fetched_ms());
        v["projects"] = json!(project_rows);
        v["panes"] = json!(pane_rows);
        v
    }
}
