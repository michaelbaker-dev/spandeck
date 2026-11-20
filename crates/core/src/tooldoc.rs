//! The tools document: which tools the fleet's agents have, where each is
//! installed, and which version each station runs.
//!
//! The owner, 18 Nov 2026: "I wonder if we should have a mcp or tool
//! section for spandeck. One where we add a mcp or tool and it
//! automatically do the setup on every machine and for every agent… it
//! then would ask for this machine or the fleet? … I can come back later
//! and decide to roll out across the fleet … It also lets me upgrade
//! across the fleet".
//!
//! One document, `tools.json`, kept the same on every station by the rules
//! of the other synced documents (sync.rs). It is a document of its own,
//! not a field of the agent setup, for the reason the look and preferences
//! documents are: a station from before it (0.4.15) reads the agent setup
//! into the fields it knows and writes only those back, so a tool kept
//! there would be dropped at that station, and from every station that
//! then took that station's copy. Such a station never holds this
//! document: it is offered only to a station that says it keeps it, in a
//! format that station reads.
//!
//! The document says what is wanted. It installs nothing: each station
//! reads it and acts for itself (tools.rs). What is in it is data, checked
//! here. A command is in it only for a custom tool, the owner's own words,
//! exactly as an MCP server of the agent setup is; for a catalogue tool it
//! names an entry of the catalogue and a version, never a command or an
//! address.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::{invalid, write_private, Daemon, Result};
use crate::setup::McpServer;
use crate::toolcat;

pub const FORMAT: u32 = 1;
const MAX_TOOLS: usize = 64;
const MAX_STATIONS: usize = 256;

/// One tool. `kind` says which of the fields below it are read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tool {
    /// `catalogue` (spandeck installs it) or `custom` (a command or an
    /// address of the owner's, as an MCP server of the agent setup is).
    pub kind: String,
    /// A catalogue tool: the entry's id.
    pub catalogue: Option<String>,
    /// A custom tool: the program and its arguments, or an address.
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: Option<String>,
    /// Which agents get it. Empty: all of them.
    pub agents: Vec<String>,
    /// Every station of the fleet, including one that joins later.
    pub fleet: bool,
    /// When not the whole fleet: these stations, by name.
    pub stations: Vec<String>,
    /// A catalogue tool: the version every station in its scope runs.
    pub version: Option<String>,
    /// And the stations that run another one (upgraded first, or held
    /// back). A rollout empties this.
    pub station_versions: BTreeMap<String, String>,
    /// Agents may use this tool without asking the owner about each call,
    /// where the agent's program has a rule for one server's tools.
    pub allow_without_asking: bool,
    /// After the last change of version: are running panes started again
    /// on it (an idle one now, another when its turn is over)?
    pub restart: bool,
}

impl Default for Tool {
    fn default() -> Tool {
        Tool {
            kind: "custom".into(),
            catalogue: None,
            command: None,
            args: vec![],
            env: BTreeMap::new(),
            url: None,
            agents: vec![],
            fleet: false,
            stations: vec![],
            version: None,
            station_versions: BTreeMap::new(),
            allow_without_asking: false,
            restart: true,
        }
    }
}

impl Tool {
    pub fn is_catalogue(&self) -> bool {
        self.kind == "catalogue"
    }

    /// Is this station one the tool is for?
    pub fn in_scope(&self, station: &str) -> bool {
        self.fleet || self.stations.iter().any(|s| s == station)
    }

    /// The version this station is to run.
    pub fn pinned(&self, station: &str) -> Option<&str> {
        self.station_versions
            .get(station)
            .or(self.version.as_ref())
            .map(String::as_str)
    }

    /// A custom tool, as the agent setup holds an MCP server.
    pub fn as_server(&self) -> McpServer {
        McpServer {
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            url: self.url.clone(),
            agents: self.agents.clone(),
        }
    }

    /// An MCP server of the agent setup, as a tool: custom, for the whole
    /// fleet, which is what it is today.
    pub fn from_server(s: &McpServer) -> Tool {
        Tool {
            command: s.command.clone(),
            args: s.args.clone(),
            env: s.env.clone(),
            url: s.url.clone(),
            agents: s.agents.clone(),
            fleet: true,
            ..Default::default()
        }
    }

    fn validate(&self, name: &str) -> std::result::Result<(), String> {
        let station_ok = |s: &String| !s.is_empty() && s.len() <= 64 && !s.contains('\n');
        if self.stations.len() > MAX_STATIONS
            || self.station_versions.len() > MAX_STATIONS
            || !self.stations.iter().all(station_ok)
            || !self.station_versions.keys().all(station_ok)
        {
            return Err(format!("tool {name}: its list of stations is not one"));
        }
        if self.agents.len() > 16
            || !self.agents.iter().all(|a| {
                !a.is_empty() && a.len() <= 32 && a.chars().all(|c| c.is_ascii_lowercase())
            })
        {
            return Err(format!("tool {name}: its list of agents is not one"));
        }
        match self.kind.as_str() {
            "catalogue" => {
                // An entry this version does not know is kept: a newer
                // station may know it, and this one says "needs a newer
                // spandeck" where it is asked to install it.
                let id_ok = self.catalogue.as_ref().is_some_and(|id| {
                    !id.is_empty()
                        && id.len() <= 64
                        && id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                });
                if !id_ok {
                    return Err(format!("tool {name}: it names no catalogue entry"));
                }
                if self.command.is_some() || self.url.is_some() || !self.args.is_empty() {
                    return Err(format!(
                        "tool {name}: a catalogue tool takes no command or address"
                    ));
                }
                if !self.version.as_deref().is_some_and(toolcat::version_ok)
                    || !self
                        .station_versions
                        .values()
                        .all(|v| toolcat::version_ok(v))
                {
                    return Err(format!("tool {name}: it needs a version, such as 0.0.85"));
                }
            }
            "custom" => {
                match (&self.command, &self.url) {
                    (Some(c), None) if !c.trim().is_empty() => {}
                    (None, Some(u)) if u.starts_with("https://") || u.starts_with("http://") => {}
                    _ => {
                        return Err(format!(
                            "tool {name}: give either a command or an http(s) URL"
                        ))
                    }
                }
                if self.version.is_some() || !self.station_versions.is_empty() {
                    return Err(format!(
                        "tool {name}: spandeck does not install a custom tool, so it has no version to pin"
                    ));
                }
            }
            other => return Err(format!("tool {name}: {other:?} is not a kind of tool")),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsDoc {
    pub format: u32,
    pub updated_ms: u64,
    pub updated_by: String,
    pub tools: BTreeMap<String, Tool>,
    /// Which stations' changes this copy holds (sync.rs).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub changes: crate::sync::Changes,
}

/// A tool's name: what an agent sees its server called. `runtime` is the
/// folder spandeck keeps its Node in.
pub fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !matches!(name, "spandeck" | "runtime")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

impl ToolsDoc {
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !crate::sync::changes_ok(&self.changes) {
            return Err("the record of changes is not one".into());
        }
        if self.tools.len() > MAX_TOOLS {
            return Err(format!("more than {MAX_TOOLS} tools"));
        }
        for (name, tool) in &self.tools {
            if !name_ok(name) {
                return Err(format!(
                    "tool name {name:?}: use letters, digits, - and _ (and not \"spandeck\" or \"runtime\")"
                ));
            }
            tool.validate(name)?;
        }
        Ok(())
    }
}

/// What `tools.add` is given.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Add {
    pub name: Option<String>,
    pub catalogue: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: Option<String>,
    pub agents: Vec<String>,
    pub fleet: bool,
    pub stations: Vec<String>,
    pub version: Option<String>,
    pub allow_without_asking: bool,
}

impl Daemon {
    fn tools_file(&self) -> PathBuf {
        self.paths.config_dir.join("tools.json")
    }

    /// The tools document, each station in a tool's scope named as it is
    /// called now: a tool given to a station is still given to it after
    /// the station is renamed.
    pub fn tools_doc(&self) -> ToolsDoc {
        let mut doc = self.tools_doc_unrenamed();
        let names = self.fleet.names();
        if !names.list().is_empty() {
            crate::rename::tools_renamed(&mut doc, &|s| names.now_called(s));
        }
        doc
    }

    /// The document as it is written.
    fn tools_doc_unrenamed(&self) -> ToolsDoc {
        std::fs::read(self.tools_file())
            .ok()
            .and_then(|b| serde_json::from_slice::<ToolsDoc>(&b).ok())
            .filter(|d| d.validate().is_ok())
            .unwrap_or_default()
    }

    fn tools_write(&self, doc: &ToolsDoc) -> Result<()> {
        std::fs::create_dir_all(&self.paths.config_dir)?;
        let tmp = self.tools_file().with_extension("json.tmp");
        // A custom tool's environment can carry credentials, as an MCP
        // server's can: readable by the user only.
        write_private(
            &tmp,
            &serde_json::to_string_pretty(doc).map_err(|e| invalid(e.to_string()))?,
        )?;
        std::fs::rename(&tmp, self.tools_file())?;
        Ok(())
    }

    /// A station name the owner gave: one this station knows of. A name
    /// typed wrong would otherwise be a tool that installs nowhere.
    fn tools_station_known(&self, station: &str) -> Result<()> {
        let known = station == self.station
            || self.fleet.peers().iter().any(|p| p.station == station)
            || self.known_stations().iter().any(|p| p.station == station);
        if known {
            Ok(())
        } else {
            Err(invalid(format!(
                "there is no station called {station} in this fleet"
            )))
        }
    }

    /// The owner's change to the document, made here: checked, stamped,
    /// written, offered to every station that keeps the document, and
    /// acted on by this station. Returns the document as it now is.
    pub(crate) fn tools_edit(
        &self,
        by: &str,
        what: &str,
        change: impl FnOnce(&mut ToolsDoc) -> Result<()>,
    ) -> Result<ToolsDoc> {
        let doc = {
            let _held = self.tools.doc_lock.lock().unwrap();
            let mut doc = self.tools_doc();
            if doc.format > FORMAT {
                return Err(invalid(
                    "the tools saved here are from a newer spandeck than this station runs; update this station to change them",
                ));
            }
            // What is written: a scope still under a name a station had
            // is saved under the name it has, as a change.
            let before = self.tools_doc_unrenamed().tools;
            change(&mut doc)?;
            doc.validate().map_err(invalid)?;
            if doc.tools == before {
                return Ok(doc);
            }
            doc.format = FORMAT;
            let base = doc.updated_ms;
            doc.updated_ms = self.sync_stamp("tools", base);
            self.sync_mark(&mut doc.changes, base, doc.updated_ms);
            doc.updated_by = format!("{by} at {}", self.station);
            self.tools_write(&doc)?;
            self.record(
                "tools.changed",
                None,
                None,
                None,
                Some(by),
                json!({"what": what, "tools": doc.tools.len()}),
            );
            doc
        };
        self.emit(
            "tools.updated",
            None,
            None,
            json!({"updated_ms": doc.updated_ms}),
        );
        let (d, pushed) = (self.clone(), doc.clone());
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { d.tools_send(&pushed).await });
        }
        self.tools_changed();
        Ok(doc)
    }

    /// Offer a copy to the online stations that keep this document, each
    /// in a call of its own and all at the same time.
    async fn tools_send(&self, doc: &ToolsDoc) {
        let peers = self.fleet.peers();
        crate::fleet::futures_join_all(peers.iter().map(|p| self.tools_offer(p, doc, false))).await;
    }

    /// Offer a copy to one online station that keeps this document.
    /// `if_older` (its round, at every look): only when it last said its
    /// copy was older. A station from before the document is never
    /// offered it: it says nothing of it in `station.info`.
    pub(crate) async fn tools_offer(
        &self,
        peer: &crate::fleet::Peer,
        doc: &ToolsDoc,
        if_older: bool,
    ) {
        let Some(theirs) = peer.info.pointer("/tools/updated_ms") else {
            return;
        };
        let theirs = theirs.as_u64().unwrap_or(0);
        let reads = peer.info.pointer("/tools/format").and_then(|f| f.as_u64());
        if doc.updated_ms == 0
            || peer.state != "online"
            || reads < Some(doc.format as u64)
            || (if_older && theirs >= doc.updated_ms)
        {
            return;
        }
        let r = self
            .fleet
            .call(&peer.station, "tools.push", json!({"tools": doc}))
            .await;
        self.sync_sent("tools", doc.updated_ms, &peer.station, r)
            .await;
    }

    /// Another station offering its copy. Kept only if it is newer, and in
    /// a format this station can hold without losing any of it. Then this
    /// station acts on it, for itself.
    pub fn tools_push(&self, doc: ToolsDoc, _from: &str) -> Result<Value> {
        if doc.format > FORMAT {
            return Ok(
                json!({"station": self.station, "ok": false, "kept": "that copy is in a newer format than this station knows"}),
            );
        }
        doc.validate().map_err(invalid)?;
        self.sync_saw("tools", doc.updated_ms);
        {
            let _held = self.tools.doc_lock.lock().unwrap();
            let mine = self.tools_doc();
            if doc.format < mine.format || doc.updated_ms <= mine.updated_ms {
                return Ok(
                    json!({"station": self.station, "ok": true, "kept": "this station's copy is as new or newer", "updated_ms": mine.updated_ms}),
                );
            }
            self.sync_replaced("tools", &mine.changes, &doc.changes);
            self.tools_write(&doc)?;
        }
        self.emit(
            "tools.updated",
            None,
            None,
            json!({"updated_ms": doc.updated_ms}),
        );
        self.tools_changed();
        Ok(json!({"station": self.station, "ok": true, "updated_ms": doc.updated_ms}))
    }

    /// `tools.list`: the document's tools with scope and pins, and after
    /// them the agent setup's MCP servers, which are custom tools for the
    /// whole fleet (`from_setup`).
    pub fn tools_list(&self) -> Value {
        let doc = self.tools_doc();
        let setup = self.setup();
        let mut tools: Vec<Value> = doc
            .tools
            .iter()
            .map(|(name, t)| tool_json(name, t, false))
            .collect();
        for (name, s) in &setup.mcp_servers {
            if !doc.tools.contains_key(name) {
                tools.push(tool_json(name, &Tool::from_server(s), true));
            }
        }
        json!({
            "station": self.station,
            "updated_ms": doc.updated_ms, "updated_by": doc.updated_by, "format": doc.format,
            "tools": tools,
        })
    }

    /// An MCP server of the agent setup becomes a tool of this document
    /// the first time the owner changes something an MCP server does not
    /// have (its scope, the "without asking" tick). It is written here
    /// first and then taken out of the agent setup; between the two the
    /// document's entry is the one that counts. A station from before this
    /// document stops giving it to its agents: it knows only the agent
    /// setup. Returns whether it was one.
    async fn tools_adopt(&self, name: &str, by: &str) -> Result<bool> {
        if self.tools_doc().tools.contains_key(name) {
            return Ok(false);
        }
        let mut setup = self.setup();
        let Some(server) = setup.mcp_servers.remove(name) else {
            return Err(invalid(format!("there is no tool called {name}")));
        };
        self.tools_edit(by, "adopt", |doc| {
            doc.tools
                .insert(name.to_string(), Tool::from_server(&server));
            Ok(())
        })?;
        self.setup_set(setup, by, true).await?;
        Ok(true)
    }

    fn tools_answer(&self, doc: &ToolsDoc, name: &str) -> Value {
        json!({
            "station": self.station, "updated_ms": doc.updated_ms,
            "tool": doc.tools.get(name).map(|t| tool_json(name, t, false)),
        })
    }

    /// `tools.add`: a catalogue tool (its version: the one named, else the
    /// newest the registry publishes, else the one spandeck was run with)
    /// or a custom one, for the whole fleet or for the stations named.
    pub async fn tools_add(&self, add: Add, by: &str) -> Result<Value> {
        let name = add
            .name
            .clone()
            .or_else(|| add.catalogue.clone())
            .ok_or_else(|| invalid("missing parameter: name"))?;
        if !name_ok(&name) {
            return Err(invalid(format!(
                "tool name {name:?}: use letters, digits, - and _ (and not \"spandeck\" or \"runtime\")"
            )));
        }
        if add.fleet == !add.stations.is_empty() {
            return Err(invalid(
                "say where: `fleet: true` for every station, or `stations` with their names",
            ));
        }
        for s in &add.stations {
            self.tools_station_known(s)?;
        }
        if self.setup().mcp_servers.contains_key(&name) {
            return Err(invalid(format!(
                "there is already an MCP server called {name} in Agent setup"
            )));
        }
        let mut tool = Tool {
            agents: add.agents.clone(),
            fleet: add.fleet,
            stations: add.stations.clone(),
            allow_without_asking: add.allow_without_asking,
            ..Default::default()
        };
        let mut version_from = None;
        if let Some(id) = &add.catalogue {
            let entry = toolcat::entry(id).ok_or_else(|| {
                invalid(format!(
                    "spandeck's catalogue has no tool called {id} (it has: {})",
                    toolcat::all()
                        .iter()
                        .map(|e| e.id)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
            let (version, from) = match &add.version {
                Some(v) if toolcat::version_ok(v) => (v.clone(), "caller"),
                Some(v) => return Err(invalid(format!("{v:?} is not a version"))),
                None => match self.tool_latest_now(entry).await {
                    Some(v) => (v, "registry"),
                    None => (entry.tested.to_string(), "catalogue"),
                },
            };
            tool.kind = "catalogue".into();
            tool.catalogue = Some(id.clone());
            tool.version = Some(version);
            version_from = Some(from);
        } else {
            tool.command = add.command.clone();
            tool.args = add.args.clone();
            tool.env = add.env.clone();
            tool.url = add.url.clone();
        }
        let doc = self.tools_edit(by, "add", |doc| {
            if doc.tools.contains_key(&name) {
                return Err(invalid(format!("there is already a tool called {name}")));
            }
            doc.tools.insert(name.clone(), tool);
            Ok(())
        })?;
        let mut out = self.tools_answer(&doc, &name);
        out["version_from"] = json!(version_from);
        Ok(out)
    }

    /// `tools.remove`: no station gives it to an agent any more. What a
    /// station installed stays on its disk (tools.rs).
    pub async fn tools_remove(&self, name: &str, by: &str) -> Result<Value> {
        if !self.tools_doc().tools.contains_key(name) {
            let mut setup = self.setup();
            if setup.mcp_servers.remove(name).is_none() {
                return Err(invalid(format!("there is no tool called {name}")));
            }
            self.setup_set(setup, by, true).await?;
            return Ok(json!({"station": self.station, "removed": name, "from_setup": true}));
        }
        let doc = self.tools_edit(by, "remove", |doc| {
            doc.tools.remove(name);
            Ok(())
        })?;
        Ok(json!({"station": self.station, "removed": name, "updated_ms": doc.updated_ms}))
    }

    /// `tools.scope`: the whole fleet, or the stations named. A pin of a
    /// station that is no longer in the scope goes with it.
    pub async fn tools_scope(
        &self,
        name: &str,
        fleet: bool,
        stations: Vec<String>,
        by: &str,
    ) -> Result<Value> {
        if fleet == !stations.is_empty() {
            return Err(invalid(
                "say where: `fleet: true` for every station, or `stations` with their names",
            ));
        }
        for s in &stations {
            self.tools_station_known(s)?;
        }
        let adopted = self.tools_adopt(name, by).await?;
        let doc = self.tools_edit(by, "scope", |doc| {
            let tool = tool_mut(doc, name)?;
            tool.fleet = fleet;
            tool.stations = stations.clone();
            if !fleet {
                tool.station_versions.retain(|s, _| stations.contains(s));
            }
            Ok(())
        })?;
        let mut out = self.tools_answer(&doc, name);
        out["moved_from_setup"] = json!(adopted);
        Ok(out)
    }

    /// `tools.set`: which agents get it, and the "without asking" tick.
    pub async fn tools_set(
        &self,
        name: &str,
        agents: Option<Vec<String>>,
        allow: Option<bool>,
        by: &str,
    ) -> Result<Value> {
        let adopted = self.tools_adopt(name, by).await?;
        let doc = self.tools_edit(by, "set", |doc| {
            let tool = tool_mut(doc, name)?;
            if let Some(agents) = agents {
                tool.agents = agents;
            }
            if let Some(allow) = allow {
                tool.allow_without_asking = allow;
            }
            Ok(())
        })?;
        let mut out = self.tools_answer(&doc, name);
        out["moved_from_setup"] = json!(adopted);
        Ok(out)
    }

    /// `tools.upgrade`: one station's version (this one's, unless another
    /// is named). The version: the one named, which may be an earlier one
    /// (a rollback), else the newest the registry was last heard to
    /// publish.
    pub async fn tools_upgrade(
        &self,
        name: &str,
        station: Option<&str>,
        to: Option<&str>,
        restart: bool,
        by: &str,
    ) -> Result<Value> {
        let station = station.unwrap_or(&self.station).to_string();
        self.tools_station_known(&station)?;
        let to = self.tools_version_for(name, to).await?;
        let doc = self.tools_edit(by, "upgrade", |doc| {
            let tool = tool_mut(doc, name)?;
            if !tool.in_scope(&station) {
                return Err(invalid(format!(
                    "{name} is not chosen for {station}: widen its scope first"
                )));
            }
            if tool.version.as_deref() == Some(to.as_str()) {
                tool.station_versions.remove(&station);
            } else {
                tool.station_versions.insert(station.clone(), to.clone());
            }
            tool.restart = restart;
            Ok(())
        })?;
        let mut out = self.tools_answer(&doc, name);
        out["on"] = json!(station);
        out["to"] = json!(to);
        out["restart"] = json!(restart);
        Ok(out)
    }

    /// `tools.rollout`: every station in the tool's scope runs one version,
    /// and no station has a pin of its own. The version: the one named,
    /// else the one the stations with a pin of their own agree on.
    pub async fn tools_rollout(
        &self,
        name: &str,
        to: Option<&str>,
        restart: bool,
        by: &str,
    ) -> Result<Value> {
        let tool = self
            .tools_doc()
            .tools
            .get(name)
            .cloned()
            .ok_or_else(|| invalid(format!("there is no tool called {name}")))?;
        let to = match to {
            Some(_) => self.tools_version_for(name, to).await?,
            None => {
                let mut pins: Vec<&String> = tool.station_versions.values().collect();
                pins.sort();
                pins.dedup();
                match pins.as_slice() {
                    [one] => (*one).clone(),
                    [] => {
                        return Err(invalid(format!(
                        "no station runs another version of {name} than the fleet's ({}): upgrade one first, or say which version with `to`",
                        tool.version.as_deref().unwrap_or("none")
                    )))
                    }
                    _ => {
                        return Err(invalid(format!(
                        "stations run several versions of {name} ({}): say which with `to`",
                        pins.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ")
                    )))
                    }
                }
            }
        };
        let doc = self.tools_edit(by, "rollout", |doc| {
            let tool = tool_mut(doc, name)?;
            tool.version = Some(to.clone());
            tool.station_versions.clear();
            tool.restart = restart;
            Ok(())
        })?;
        let mut out = self.tools_answer(&doc, name);
        out["to"] = json!(to);
        out["restart"] = json!(restart);
        Ok(out)
    }

    /// The version a change of version is to: the one named, else the
    /// newest known here for the tool's catalogue entry.
    async fn tools_version_for(&self, name: &str, to: Option<&str>) -> Result<String> {
        let tool = self
            .tools_doc()
            .tools
            .get(name)
            .cloned()
            .ok_or_else(|| invalid(format!("there is no tool called {name}")))?;
        if !tool.is_catalogue() {
            return Err(invalid(format!(
                "{name} is a custom tool: spandeck does not install it, so it has no version to change"
            )));
        }
        match to {
            Some(v) if toolcat::version_ok(v) => Ok(v.to_string()),
            Some(v) => Err(invalid(format!("{v:?} is not a version"))),
            None => {
                let entry = tool.catalogue.as_deref().and_then(toolcat::entry);
                let known = match entry {
                    Some(entry) => self.tool_latest_now(entry).await,
                    None => None,
                };
                known.ok_or_else(|| {
                    invalid(format!(
                        "the newest version of {name} is not known here: say which with `to`"
                    ))
                })
            }
        }
    }
}

fn tool_mut<'a>(doc: &'a mut ToolsDoc, name: &str) -> Result<&'a mut Tool> {
    doc.tools
        .get_mut(name)
        .ok_or_else(|| invalid(format!("there is no tool called {name}")))
}

/// A tool as `tools.list` and the answers to a change give it.
pub(crate) fn tool_json(name: &str, t: &Tool, from_setup: bool) -> Value {
    let mut v = json!(t);
    v["name"] = json!(name);
    // An MCP server of the agent setup: every station gives it to its
    // agents, also one from before this document.
    v["from_setup"] = json!(from_setup);
    v["title"] = json!(t
        .catalogue
        .as_deref()
        .and_then(toolcat::entry)
        .map(|e| e.name)
        .unwrap_or(name));
    v
}
