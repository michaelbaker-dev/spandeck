//! Edge cases in pure logic: message dedupe and the task state machine.

use std::sync::Arc;

use proptest::prelude::*;
use serde_json::json;
use spandeck_core::db::Db;
use spandeck_core::relay::{Envelope, MessageStore};
use spandeck_core::tasks::{transition_allowed, STATES};

fn env(id: u8, seq: u64) -> Envelope {
    Envelope {
        id: format!("m-{id}"),
        thread: "t".into(),
        seq,
        ts: 1,
        sender: "a@alpha".into(),
        recipient: "b@bravo".into(),
        kind: "note".into(),
        body: format!("body {id}"),
        refs: json!([]),
        task: None,
        project: None,
        auth: None,
        hops: 0,
    }
}

proptest! {
    /// However deliveries are repeated or reordered by retries, each message
    /// id is stored exactly once.
    #[test]
    fn duplicate_deliveries_are_dropped_by_id(deliveries in proptest::collection::vec(0u8..8, 1..60)) {
        let store = MessageStore::new(Arc::new(Db::memory().unwrap()), "bravo");
        let mut first_seen = std::collections::BTreeSet::new();
        for id in &deliveries {
            let ack = store.accept(&env(*id, *id as u64), 5);
            prop_assert!(ack.received);
            prop_assert_eq!(ack.duplicate, !first_seen.insert(*id));
        }
        let inbox = store.inbox(&["b".to_string()], false, false, 9);
        prop_assert_eq!(inbox.len(), first_seen.len());
    }

    /// No sequence of moves leaves a final state or reaches an unknown one.
    #[test]
    fn completed_and_cancelled_are_final(moves in proptest::collection::vec(0usize..STATES.len(), 1..40)) {
        let mut state = "queued";
        for m in moves {
            let to = STATES[m];
            if transition_allowed(state, to) {
                prop_assert!(state != "completed" && state != "cancelled", "left final state {}", state);
                state = to;
            }
        }
        prop_assert!(STATES.contains(&state));
    }
}

#[test]
fn only_a_running_task_can_complete() {
    for from in STATES {
        assert_eq!(
            transition_allowed(from, "completed"),
            *from == "running",
            "{from} -> completed"
        );
    }
    // A failed task keeps its resume information and can run again.
    assert!(transition_allowed("failed", "running"));
    assert!(!transition_allowed("completed", "running"));
}
