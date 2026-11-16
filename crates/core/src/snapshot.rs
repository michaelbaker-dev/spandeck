//! `deck.snapshot`: everything a Control Deck shows of one station, in one
//! answer, with what the deck already holds left out.
//!
//! A deck's refresh used to be seven questions to every station (panes,
//! projects, tasks, messages, approvals, notices, stops) and five more to
//! the station serving it: 34 calls for four stations, every 2.5 seconds
//! and after almost any event (measured, 15 Nov 2026). It is now one
//! question per station. Each section comes with a version; the deck sends
//! back the versions it holds, and a section whose version is unchanged is
//! answered `null`.
//!
//! A version is worked out from the section's content (a hash), not counted
//! at the places that change it. So it cannot miss a change that raises no
//! event (a pane's title set by its program, a task's state written by a
//! hook), it needs nothing kept between calls, and it means the same after
//! a restart. Two fields are left out of it because they change with the
//! clock and nothing else: a pane's `last_output_ms`, and a stop's `age_ms`
//! (the deck counts a stop's age on from when it was told).

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::daemon::Daemon;

/// Announced in `caps`: a deck asks `deck.snapshot` only of a station that
/// says this, and asks the seven lists of one that does not (0.4.5 and
/// earlier). Also the name of the switch (`[flags] deck_snapshot = false`,
/// or `station.flags`): off, the station neither says it nor answers the
/// call, exactly as a station from before.
pub const CAP: &str = "deck_snapshot";

/// The sections every snapshot has, in the order of the seven calls they
/// replace.
pub const SECTIONS: [&str; 7] = [
    "panes",
    "projects",
    "tasks",
    "msgs",
    "approvals",
    "notes",
    "stops",
];

/// What the station serving a deck is also asked (`docs: true`): the
/// documents and lists a deck reads from that station alone.
pub const DOCS: [&str; 4] = ["trust", "deck", "look", "prefs"];

/// A field of a section's rows that changes with the clock alone.
fn passed_over(section: &str) -> Option<&'static str> {
    match section {
        "panes" => Some("last_output_ms"),
        "stops" => Some("age_ms"),
        _ => None,
    }
}

/// A section's version: the same text for the same content, and nothing
/// else is to be read into it.
pub fn version(section: &str, value: &Value) -> String {
    let mut h = Sha256::new();
    match (passed_over(section), value.as_array()) {
        (Some(skip), Some(rows)) => {
            for row in rows {
                match row.as_object() {
                    Some(fields) => {
                        for (k, v) in fields.iter().filter(|(k, _)| k.as_str() != skip) {
                            h.update(k.as_bytes());
                            h.update([0]);
                            h.update(v.to_string().as_bytes());
                            h.update([0]);
                        }
                    }
                    None => h.update(row.to_string().as_bytes()),
                }
                h.update([1]);
            }
        }
        _ => h.update(value.to_string().as_bytes()),
    }
    h.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The answer: every section's version, and each section itself unless the
/// caller holds that version (`have`: section name to version; anything
/// else there is taken as holding nothing).
pub fn answer(sections: Vec<(&'static str, Value)>, have: &Value) -> Value {
    let mut out = Map::new();
    let mut versions = Map::new();
    for (name, value) in sections {
        let v = version(name, &value);
        let held = have.get(name).and_then(|h| h.as_str()) == Some(v.as_str());
        versions.insert(name.to_string(), json!(v));
        out.insert(name.to_string(), if held { Value::Null } else { value });
    }
    out.insert("v".to_string(), Value::Object(versions));
    Value::Object(out)
}

impl Daemon {
    /// `deck.snapshot {have, docs}`. Each section is built by the function
    /// its own call uses (`pane.list`, `project.list`, `task.list {active,
    /// limit 100}`, `msg.recent {limit 60}`, `approval.list`, `notify.list
    /// {limit 60}`, `stop.list`; and `trust.status`, `deck.get`, `look.get`,
    /// `prefs.get`), so it says exactly what they say. All of them are on
    /// the read-only list.
    pub fn deck_snapshot(&self, params: &Value) -> Value {
        let mut sections = vec![
            ("panes", json!(self.pane_views())),
            ("projects", json!(self.projects())),
            ("tasks", json!(self.tasks(None, true, 100))),
            ("msgs", json!(self.mail().recent(None, 60))),
            ("approvals", json!(self.approvals(true))),
            ("notes", json!(self.notifications(false, 60))),
            ("stops", json!(self.stops(50))),
        ];
        if params.get("docs") == Some(&json!(true)) {
            sections.extend([
                ("trust", self.trust_view()),
                ("deck", json!(self.deck())),
                ("look", json!(self.look())),
                ("prefs", json!(self.prefs())),
            ]);
        }
        let mut out = answer(sections, params.get("have").unwrap_or(&Value::Null));
        out["station"] = json!(self.station);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit 06 of the performance plan. The reason for the test: a section
    /// answered `null` is one the deck goes on showing from before, so
    /// "unchanged" must never be said of something that changed, and a
    /// change to one section must not cost the others.
    #[test]
    fn a_section_is_left_out_only_when_the_deck_holds_exactly_it() {
        let pane =
            |state: &str, out: u64| json!({"id": "p1", "state": state, "last_output_ms": out});
        let sections = |panes: Value, stops: Value| {
            vec![
                ("panes", panes),
                ("tasks", json!([{"id": "t1", "state": "running"}])),
                ("stops", stops),
            ]
        };
        let stop = |age: u64, status: &str| json!([{"id": "s1", "age_ms": age, "status": status}]);
        // Nothing held, or something that is not a list of versions:
        // everything.
        let first = answer(
            sections(json!([pane("idle", 5)]), stop(10, "requested")),
            &Value::Null,
        );
        for held in [json!("x"), json!({"panes": 7}), json!({"panes": "0000"})] {
            let all = answer(
                sections(json!([pane("idle", 5)]), stop(10, "requested")),
                &held,
            );
            assert!(
                SECTIONS
                    .iter()
                    .all(|s| all.get(s).is_none_or(|v| !v.is_null())),
                "{all}"
            );
        }
        assert_eq!(first["panes"][0]["state"], json!("idle"));
        let have = first["v"].clone();
        // Nothing changed: nothing is sent again. A pane that only printed,
        // and a stop that only grew older, have not changed.
        let same = answer(
            sections(json!([pane("idle", 99)]), stop(2_500, "requested")),
            &have,
        );
        assert_eq!(
            (&same["panes"], &same["tasks"], &same["stops"]),
            (&Value::Null, &Value::Null, &Value::Null)
        );
        assert_eq!(same["v"], have);
        // A pane changed: panes are sent, with a new version, and only they.
        let changed = answer(
            sections(json!([pane("working", 99)]), stop(2_500, "requested")),
            &have,
        );
        assert_eq!(changed["panes"][0]["state"], json!("working"));
        assert_ne!(changed["v"]["panes"], have["panes"]);
        assert_eq!(
            (&changed["tasks"], &changed["stops"]),
            (&Value::Null, &Value::Null)
        );
        // A stop confirmed: its age is sent with it.
        let confirmed = answer(
            sections(json!([pane("idle", 99)]), stop(2_500, "confirmed")),
            &have,
        );
        assert_eq!(confirmed["stops"][0]["age_ms"], json!(2_500));
        assert_eq!(confirmed["panes"], Value::Null);
        // A pane closed, and one in its place: both change the version.
        let none = answer(sections(json!([]), stop(1, "requested")), &have);
        assert_eq!(none["panes"], json!([]));
        let other = json!([{"id": "p2", "state": "idle", "last_output_ms": 5}]);
        assert!(!answer(sections(other, stop(1, "requested")), &have)["panes"].is_null());
    }
}
