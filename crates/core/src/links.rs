//! Project links: the owner joins two projects so their agents can work
//! together. A link is two-way, between exactly two projects, on one station
//! or on two stations of the fleet.
//!
//! What a link opens: messages between the two projects' agents, and each
//! project's posted findings to the other. What it does not: files, session
//! history (recall) and vault keys stay in each agent's own project.
//!
//! Only the owner makes or removes a link. For a link between stations, the
//! station where the owner asked tells the other with a signed peer call.

use std::time::Duration;

use serde_json::{json, Value};

use crate::daemon::{invalid, Daemon, Error, Result};
use crate::memory::Which;
use crate::paths::now_ms;

/// How long a search waits for another station's findings.
const PEER_WAIT: Duration = Duration::from_secs(4);

impl Daemon {
    /// The projects linked to a local project: (project, station).
    pub(crate) fn links_of(&self, project: &str) -> Vec<(String, String)> {
        self.db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT other, station FROM project_links WHERE project=?1 ORDER BY other, station",
                )?;
                let rows = stmt.query_map([project], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn linked(&self, project: &str, other: &str, station: &str) -> bool {
        self.db
            .read(|c| {
                c.query_row(
                    "SELECT 1 FROM project_links WHERE project=?1 AND other=?2 AND station=?3",
                    [project, other, station],
                    |_| Ok(()),
                )
            })
            .is_ok()
    }

    /// Record one direction of a link. Returns whether it was new.
    fn link_put(&self, project: &str, other: &str, station: &str) -> Result<bool> {
        let n = self.db.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO project_links(project,other,station,since) VALUES(?1,?2,?3,?4)",
                rusqlite::params![project, other, station, now_ms() as i64],
            )
        })?;
        Ok(n > 0)
    }

    fn link_drop(&self, project: &str, other: &str, station: &str) -> Result<bool> {
        let n = self.db.with(|c| {
            c.execute(
                "DELETE FROM project_links WHERE project=?1 AND other=?2 AND station=?3",
                [project, other, station],
            )
        })?;
        Ok(n > 0)
    }

    fn link_changed(&self, kind: &str, project: &str, other: &str, station: &str, by: &str) {
        self.record(
            kind,
            None,
            Some(project),
            None,
            Some(by),
            json!({"with": other, "station": station}),
        );
        self.emit("project.updated", None, Some(project), json!({}));
    }

    fn need_project(&self, name: &str) -> Result<()> {
        self.project_path(name)
            .map(|_| ())
            .ok_or_else(|| Error::NotFound(format!("project {name}")))
    }

    /// The owner links local `project` to `with` on `station`.
    pub async fn project_link(
        &self,
        project: &str,
        with: &str,
        station: Option<&str>,
        by: &str,
    ) -> Result<Value> {
        // By its name, or one it had before it was renamed.
        let named = station.map(|s| self.station_now(s));
        let station = named.as_deref().unwrap_or(&self.station);
        self.need_project(project)?;
        if station == self.station {
            if project == with {
                return Err(invalid("a project cannot be linked to itself"));
            }
            self.need_project(with)?;
            if self.link_put(project, with, station)? {
                self.link_changed("project.linked", project, with, station, by);
            }
            if self.link_put(with, project, station)? {
                self.link_changed("project.linked", with, project, station, by);
            }
        } else {
            self.require_peer(station)?;
            // The other station must know it too, or the link is not made.
            self.fleet
                .call(
                    station,
                    "project.link.record",
                    json!({"project": with, "with": project, "linked": true}),
                )
                .await
                .map_err(|e| invalid(format!("the link was not made: {station}: {e}")))?;
            if self.link_put(project, with, station)? {
                self.link_changed("project.linked", project, with, station, by);
            }
        }
        Ok(json!({"project": project, "with": with, "station": station}))
    }

    /// The owner removes a link. It is removed here even when the other
    /// station cannot be told; the reply then says so (`told: false`).
    pub async fn project_unlink(
        &self,
        project: &str,
        with: &str,
        station: Option<&str>,
        by: &str,
    ) -> Result<Value> {
        let named = station.map(|s| self.station_now(s));
        let station = named.as_deref().unwrap_or(&self.station);
        let mut out = json!({"project": project, "with": with, "station": station});
        if self.link_drop(project, with, station)? {
            self.link_changed("project.unlinked", project, with, station, by);
        }
        if station == self.station {
            if self.link_drop(with, project, station)? {
                self.link_changed("project.unlinked", with, project, station, by);
            }
        } else if let Err(e) = self
            .fleet
            .call(
                station,
                "project.link.record",
                json!({"project": with, "with": project, "linked": false}),
            )
            .await
        {
            out["told"] = json!(false);
            out["problem"] = json!(format!(
                "station {station} was not told ({e}); unlink again when it is reachable"
            ));
        }
        Ok(out)
    }

    pub fn project_links(&self, project: Option<&str>) -> Vec<Value> {
        self.db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT project, other, station, since FROM project_links
                     WHERE (?1 IS NULL OR project=?1) ORDER BY project, other, station",
                )?;
                let rows = stmt.query_map([project], |r| {
                    Ok(json!({
                        "project": r.get::<_, String>(0)?, "with": r.get::<_, String>(1)?,
                        "station": r.get::<_, String>(2)?, "since_ms": r.get::<_, i64>(3)?,
                    }))
                })?;
                rows.collect()
            })
            .unwrap_or_default()
    }

    /// A project that is removed takes its links on this station with it.
    pub(crate) fn links_forget(&self, project: &str) {
        let _ = self.db.with(|c| {
            c.execute(
                "DELETE FROM project_links WHERE project=?1 OR (other=?1 AND station=?2)",
                [project, self.station.as_str()],
            )
        });
    }

    /// Another station records a link the owner made (or removed) there.
    /// `project` is ours, `with` is the project on the calling station.
    pub(crate) fn link_record(&self, peer: &str, params: &Value) -> Result<Value> {
        let peer = &self.station_now(peer);
        let get = |k: &str| {
            params
                .get(k)
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid(format!("missing parameter: {k}")))
        };
        let (project, with) = (get("project")?, get("with")?);
        let linked = params.get("linked").and_then(|v| v.as_bool()) != Some(false);
        let by = format!("you (via {peer})");
        if linked {
            self.need_project(project)?;
            if self.link_put(project, with, peer)? {
                self.link_changed("project.linked", project, with, peer, &by);
            }
        } else if self.link_drop(project, with, peer)? {
            self.link_changed("project.unlinked", project, with, peer, &by);
        }
        Ok(json!({"project": project, "with": with, "station": peer, "linked": linked}))
    }

    /// Another station asks for the findings of our `project` on behalf of
    /// its project `for`. Answered only across a link this station knows.
    pub(crate) fn link_findings(&self, peer: &str, params: &Value) -> Result<Value> {
        let get = |k: &str| params.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        let (project, asker) = (get("project"), get("for"));
        if !self.linked(project, asker, peer) {
            return Err(Error::Denied(format!(
                "project {project} is not linked with {asker} on {peer}"
            )));
        }
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(100) as u32;
        // Posted notes only (of one kind when asked): a linked project is
        // never shown this project's activity, whatever it asks for.
        let which = match Which::parse(params.get("kind").and_then(|v| v.as_str()), None) {
            Ok(Which::Kind(k)) => Which::Kind(k),
            _ => Which::Notes,
        };
        Ok(json!(self.label(
            self.context_search(project, get("query"), limit, &which),
            project,
            &self.station,
        )))
    }

    fn label(&self, mut notes: Vec<Value>, project: &str, station: &str) -> Vec<Value> {
        for n in &mut notes {
            n["project"] = json!(project);
            n["station"] = json!(station);
        }
        notes
    }

    /// The findings board as a project sees it: its own memory, plus what
    /// linked projects posted. Each row says which project it is from. A
    /// station that cannot be reached is named in a row of kind `unreachable`.
    /// `which`: posted notes unless activity is asked for (memory.rs);
    /// activity is this project's own, never a linked project's.
    pub async fn board_search(
        &self,
        project: &str,
        query: &str,
        limit: u32,
        which: &Which,
    ) -> Vec<Value> {
        let mut rows = self.label(
            self.context_search(project, query, limit, which),
            project,
            &self.station,
        );
        let theirs = match which {
            Which::Activity => return rows,
            Which::Kind(k) => Which::Kind(k.clone()),
            Which::Notes | Which::All => Which::Notes,
        };
        let links = self.links_of(project);
        if links.is_empty() {
            return rows;
        }
        let kind = match &theirs {
            Which::Kind(k) => Some(k.as_str()),
            _ => None,
        };
        let mut missing = Vec::new();
        for (other, station) in links {
            if station == self.station {
                rows.extend(self.label(
                    self.context_search(&other, query, limit, &theirs),
                    &other,
                    &station,
                ));
                continue;
            }
            let call = self.fleet.call(
                &station,
                "project.link.findings",
                json!({"project": other, "for": project, "query": query, "limit": limit, "kind": kind}),
            );
            match tokio::time::timeout(PEER_WAIT, call).await {
                Ok(Ok(Value::Array(theirs))) => rows.extend(theirs),
                _ => missing.push(json!({
                    "id": null, "ts": now_ms(), "kind": "unreachable", "class": "note", "author": null, "task": null,
                    "project": other, "station": station, "unreachable": true,
                    "text": format!("Station {station} could not be reached, so the findings of linked project {other} are missing here."),
                })),
            }
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r["ts"].as_i64().unwrap_or(0)));
        rows.truncate(limit as usize);
        rows.extend(missing);
        rows
    }
}
