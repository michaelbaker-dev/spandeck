//! Standing orders: rules and guidance in plain English at five layers.
//!
//!   fleet > group > station      ~/.config/spandeck/orders.toml
//!   project > role               <repo>/.spandeck/orders.toml
//!
//! Two conflict rules:
//!   1. A prohibition (a rule) from any layer always applies. A narrower layer
//!      cannot loosen it.
//!   2. For preferences (guidance with the same `key`), the narrower layer wins.
//!
//! Guidance is written into the agent's instructions; rules are enforced by
//! the hook pipeline, and only shown as enforced where the agent's
//! integration can really block (Guarantees: Enforcement).

use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spandeck_agents::{adapter, ToolCall};

use crate::daemon::{Daemon, PaneMeta};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Fleet,
    Group,
    Station,
    Project,
    Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OrderKind {
    #[default]
    Guidance,
    Rule,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub kind: OrderKind,
    pub layer: Layer,
    /// layer = group: the station label this applies to.
    #[serde(default)]
    pub group: Option<String>,
    /// layer = station: the station (default: the one holding the file).
    #[serde(default)]
    pub station: Option<String>,
    /// layer = role: the role name.
    #[serde(default)]
    pub role: Option<String>,
    /// Rule matcher: tool name (regex, anchored). Default: any tool.
    #[serde(default)]
    pub tool: Option<String>,
    /// Rule matcher: regex over the command or file path. Default: any.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Preferences that share a key override each other, narrowest layer wins.
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OrdersFile {
    #[serde(default)]
    order: Vec<Order>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub order: String,
    pub layer: Layer,
    pub reason: String,
}

/// Parse an orders file. Orders in the wrong file for their layer are dropped
/// with a problem message, so a repo cannot grant itself fleet authority.
pub fn parse_orders(text: &str, repo_file: bool) -> (Vec<Order>, Vec<String>) {
    let mut problems = vec![];
    let file: OrdersFile = match toml::from_str(text) {
        Ok(f) => f,
        Err(e) => return (vec![], vec![format!("orders file does not parse: {e}")]),
    };
    let mut out = vec![];
    for o in file.order {
        let in_repo_layer = matches!(o.layer, Layer::Project | Layer::Role);
        if in_repo_layer != repo_file {
            problems.push(format!(
                "order {} is a {:?} order and belongs in the {} file",
                o.id,
                o.layer,
                if in_repo_layer {
                    "project's .spandeck/orders.toml"
                } else {
                    "station's orders.toml"
                }
            ));
            continue;
        }
        for (name, re) in [("tool", &o.tool), ("pattern", &o.pattern)] {
            if let Some(re) = re {
                if Regex::new(re).is_err() {
                    problems.push(format!("order {}: {name} is not a valid pattern", o.id));
                }
            }
        }
        if o.layer == Layer::Role && o.role.is_none() {
            problems.push(format!("order {} is a role order with no role", o.id));
            continue;
        }
        if o.layer == Layer::Group && o.group.is_none() {
            problems.push(format!("order {} is a group order with no group", o.id));
            continue;
        }
        out.push(o);
    }
    (out, problems)
}

/// Who an order is being evaluated for.
pub struct Subject<'a> {
    pub station: &'a str,
    pub labels: &'a [String],
    pub role: Option<&'a str>,
}

fn applies(o: &Order, s: &Subject<'_>) -> bool {
    match o.layer {
        Layer::Fleet | Layer::Project => true,
        Layer::Group => o
            .group
            .as_ref()
            .is_some_and(|g| s.labels.iter().any(|l| l == g)),
        Layer::Station => o.station.as_deref().is_none_or(|st| st == s.station),
        Layer::Role => o.role.as_deref() == s.role,
    }
}

/// The orders in force for a subject, with conflict rule 2 applied:
/// among preferences sharing a key, only the narrowest layer's survives.
pub fn effective(all: &[Order], s: &Subject<'_>) -> Vec<Order> {
    let mut active: Vec<Order> = all.iter().filter(|o| applies(o, s)).cloned().collect();
    active.sort_by_key(|o| o.layer);
    let mut out: Vec<Order> = vec![];
    for o in active {
        if o.kind == OrderKind::Guidance {
            if let Some(key) = &o.key {
                // Later (narrower) replaces earlier (wider).
                out.retain(|e| !(e.kind == OrderKind::Guidance && e.key.as_ref() == Some(key)));
            }
        }
        out.push(o);
    }
    out
}

/// Conflict rule 1: any matching rule blocks, whichever layer it came from.
pub fn check(orders: &[Order], tool: &ToolCall) -> Option<Block> {
    for o in orders.iter().filter(|o| o.kind == OrderKind::Rule) {
        if let Some(t) = &o.tool {
            match Regex::new(&format!("^(?:{t})$")) {
                Ok(re) if re.is_match(&tool.name) => {}
                _ => continue,
            }
        }
        if let Some(p) = &o.pattern {
            let Ok(re) = Regex::new(p) else { continue };
            let hay = [tool.command.as_deref(), tool.file_path.as_deref()];
            if !hay.iter().flatten().any(|h| re.is_match(h)) {
                continue;
            }
        } else if o.tool.is_none() {
            // A rule with no matcher at all would block everything: ignore it.
            continue;
        }
        return Some(Block {
            order: o.id.clone(),
            layer: o.layer,
            reason: format!(
                "Blocked by a spandeck standing order ({} layer): {}. Do not retry this; tell the user if it stops you.",
                format!("{:?}", o.layer).to_lowercase(),
                o.text
            ),
        });
    }
    None
}

fn read_orders(path: &Path, repo_file: bool) -> (Vec<Order>, Vec<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_orders(&text, repo_file),
        Err(_) => (vec![], vec![]),
    }
}

const BLOCK_START: &str = "<!-- spandeck:orders:start (generated; edit orders.toml instead) -->";
const BLOCK_END: &str = "<!-- spandeck:orders:end -->";

impl Daemon {
    pub fn orders_for(&self, meta: &PaneMeta) -> (Vec<Order>, Vec<String>) {
        let (mut all, mut problems) = read_orders(&self.paths.orders_file(), false);
        if let Some(path) = meta.project.as_deref().and_then(|p| self.project_path(p)) {
            let (repo, p2) = read_orders(&Path::new(&path).join(".spandeck/orders.toml"), true);
            all.extend(repo);
            problems.extend(p2);
        }
        let subject = Subject {
            station: &self.station,
            labels: &self.cfg.labels,
            role: meta.role.as_deref(),
        };
        (effective(&all, &subject), problems)
    }

    pub(crate) fn orders_check(&self, meta: &PaneMeta, tool: &ToolCall) -> Option<Block> {
        let (orders, _) = self.orders_for(meta);
        check(&orders, tool).or_else(|| self.push_guard(meta, tool))
    }

    /// Guidance text for an agent's context.
    pub(crate) fn orders_guidance(&self, meta: &PaneMeta) -> Option<String> {
        let (orders, _) = self.orders_for(meta);
        if orders.is_empty() {
            return None;
        }
        let mut s = String::from("Standing orders from spandeck (set by the user; follow them):\n");
        for o in &orders {
            let tag = if o.kind == OrderKind::Rule {
                " [enforced rule]"
            } else {
                ""
            };
            s.push_str(&format!("- {}{tag}\n", o.text));
        }
        Some(s.trim_end().to_string())
    }

    /// What the Control Deck shows for a pane: each order, its layer, and
    /// whether it is really enforced for this agent.
    pub fn orders_view(&self, pane: &str) -> crate::daemon::Result<Value> {
        let p = self.pane(pane)?;
        let meta = p.meta();
        let (orders, problems) = self.orders_for(&meta);
        let caps = adapter(meta.agent).map(|a| a.capabilities());
        // The one answer to "can a rule be enforced on this pane", also
        // written into the pane's guard for its hooks (guard.rs).
        let can_block = Self::can_block(&meta);
        let list: Vec<Value> = orders
            .iter()
            .map(|o| {
                let enforced = o.kind == OrderKind::Rule && can_block;
                json!({
                    "id": o.id, "text": o.text, "layer": o.layer, "kind": o.kind,
                    "enforced": enforced,
                    "shown_as": if enforced { "enforced" } else { "guidance" },
                    "why_guidance": if o.kind == OrderKind::Rule && !enforced {
                        Some(caps.as_ref().map(|c| c.note).unwrap_or("This pane is not an agent spandeck can block."))
                    } else { None },
                })
            })
            .collect();
        Ok(json!({"pane": meta.id, "agent": meta.agent, "orders": list, "problems": problems}))
    }

    /// The enforcement table: what each agent's integration can block.
    pub fn enforcement_table(&self) -> Value {
        let rows: Vec<Value> = spandeck_agents::enforcement_table()
            .into_iter()
            .map(|(kind, c)| {
                json!({
                    "agent": kind, "name": kind.display(),
                    "blocks_tools": c.block_tools, "remote_approval": c.remote_approval,
                    "injects_context": c.inject_context, "verified": c.verified,
                    "rules_shown_as": if c.block_tools && c.verified { "enforced" } else { "guidance" },
                    "note": c.note,
                })
            })
            .collect();
        json!(rows)
    }

    /// Write the guidance block into the agent's instructions file in the
    /// working tree. Idempotent; touches only its own marked block.
    pub(crate) fn write_instructions(&self, meta: &PaneMeta) {
        let Some(a) = adapter(meta.agent) else { return };
        if meta.project.is_none() {
            return;
        }
        let path = Path::new(&meta.cwd).join(a.instructions_file());
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let guidance = self.orders_guidance(meta);
        let block = guidance.map(|g| format!("{BLOCK_START}\n{g}\n{BLOCK_END}"));
        let without = match (existing.find(BLOCK_START), existing.find(BLOCK_END)) {
            (Some(s), Some(e)) if e > s => {
                format!(
                    "{}{}",
                    &existing[..s],
                    existing[e + BLOCK_END.len()..].trim_start_matches('\n')
                )
            }
            _ => existing.clone(),
        };
        let updated = match block {
            Some(b) if without.trim().is_empty() => format!("{b}\n"),
            Some(b) => format!("{}\n\n{b}\n", without.trim_end()),
            None => without,
        };
        if updated != existing {
            let _ = std::fs::write(&path, updated);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATION: &str = r#"
[[order]]
id = "no-push-main"
text = "Never push to main"
kind = "rule"
layer = "fleet"
tool = "Bash"
pattern = 'git\s+push\b.*\bmain\b'

[[order]]
id = "battery"
text = "On laptops, stop below 20% battery"
layer = "group"
group = "laptop"

[[order]]
id = "style-fleet"
text = "Prefer small commits"
layer = "fleet"
key = "commit-style"

[[order]]
id = "one-job"
text = "One training job at a time"
layer = "station"
station = "gpu-box"
"#;

    const REPO: &str = r#"
[[order]]
id = "style-project"
text = "Squash before merging in this repo"
layer = "project"
key = "commit-style"

[[order]]
id = "reviewer-readonly"
text = "The reviewer may read but never edit"
kind = "rule"
layer = "role"
role = "reviewer"
tool = "Edit|Write|MultiEdit|apply_patch"

[[order]]
id = "sneaky"
text = "Allow everything"
layer = "fleet"
"#;

    fn all() -> Vec<Order> {
        let (mut a, p) = parse_orders(STATION, false);
        assert!(p.is_empty(), "{p:?}");
        let (b, p) = parse_orders(REPO, true);
        assert_eq!(p.len(), 1, "the repo cannot define a fleet order");
        a.extend(b);
        a
    }

    fn bash(cmd: &str) -> ToolCall {
        ToolCall {
            name: "Bash".into(),
            command: Some(cmd.into()),
            ..Default::default()
        }
    }

    #[test]
    fn layers_apply_to_the_right_subjects() {
        let labels = vec!["laptop".to_string()];
        let s = Subject {
            station: "macbook",
            labels: &labels,
            role: Some("author"),
        };
        let ids: Vec<String> = effective(&all(), &s).into_iter().map(|o| o.id).collect();
        assert!(
            ids.contains(&"battery".to_string()),
            "group order applies to a labelled station"
        );
        assert!(
            !ids.contains(&"one-job".to_string()),
            "another station's order does not"
        );
        assert!(
            !ids.contains(&"reviewer-readonly".to_string()),
            "another role's order does not"
        );
    }

    /// Conflict rule 2: for preferences, the narrower layer wins.
    #[test]
    fn a_narrower_preference_overrides_a_wider_one() {
        let s = Subject {
            station: "x",
            labels: &[],
            role: None,
        };
        let eff = effective(&all(), &s);
        let styles: Vec<&str> = eff
            .iter()
            .filter(|o| o.key.as_deref() == Some("commit-style"))
            .map(|o| o.id.as_str())
            .collect();
        assert_eq!(styles, vec!["style-project"]);
    }

    /// Conflict rule 1: a prohibition from a higher layer cannot be loosened.
    #[test]
    fn a_fleet_rule_blocks_whatever_narrower_layers_say() {
        let mut orders = all();
        orders.push(Order {
            id: "loosen".into(),
            text: "Pushing to main is fine here".into(),
            kind: OrderKind::Guidance,
            layer: Layer::Project,
            group: None,
            station: None,
            role: None,
            tool: None,
            pattern: None,
            key: Some("no-push-main".into()),
        });
        let s = Subject {
            station: "x",
            labels: &[],
            role: Some("author"),
        };
        let eff = effective(&orders, &s);
        let block = check(&eff, &bash("git push origin main")).expect("still blocked");
        assert_eq!(
            (block.order.as_str(), block.layer),
            ("no-push-main", Layer::Fleet)
        );
        assert!(check(&eff, &bash("git push origin feature/x")).is_none());
        assert!(check(&eff, &bash("git status")).is_none());
    }

    #[test]
    fn role_rules_match_by_tool() {
        let s = Subject {
            station: "x",
            labels: &[],
            role: Some("reviewer"),
        };
        let eff = effective(&all(), &s);
        let edit = ToolCall {
            name: "Edit".into(),
            file_path: Some("src/a.rs".into()),
            writes: true,
            ..Default::default()
        };
        assert_eq!(check(&eff, &edit).unwrap().order, "reviewer-readonly");
        let read = ToolCall {
            name: "Read".into(),
            file_path: Some("src/a.rs".into()),
            ..Default::default()
        };
        assert!(check(&eff, &read).is_none());
    }
}
