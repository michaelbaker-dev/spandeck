//! A page that comes back replaces its own earlier connection, and no
//! other (DECISIONS.md, "A key can never reach another pane").
//!
//! The audit's gap 13: a page whose link went half-open gives its socket up
//! and makes a new one, but the station still holds the old one, and what
//! the page had sent on it arrives when the link returns, after whatever it
//! has typed since. So a connection that is the same page come back closes
//! the one before it, and what that one had waiting is thrown away.
//!
//! "The same page" is decided by what only that page holds:
//!
//! - **A page** makes an *instance* when it loads (16 random hex digits, in
//!   memory only) and sends it at sign-in with its client id. A new
//!   connection with the same client id and the same instance closes the
//!   older one. A duplicated tab copies the client id (it is in
//!   `sessionStorage`) but loads as a page of its own, with its own
//!   instance: the two never close each other. (The second review: fenced
//!   on the client id alone they would have evicted each other for ever.)
//! - **A gateway** (a station showing one of this station's panes to a page
//!   it serves) opens one connection per pane, every one of a tab's under
//!   the one name `gw-<station>-<tab>`. Those are told apart by the pane as
//!   well, and by the page's instance, which the gateway passes on: a tab's
//!   second pane does not close its first, and two tabs showing one pane do
//!   not close each other's.
//! - Only a connection that says so is fenced. A page or a gateway from
//!   before sends neither, and is never closed by this rule.
//!
//! Whose connection it is (`who`) is part of what is compared, so a
//! connection can only ever replace one let in as the same caller: another
//! station's gateway, or an agent's token, cannot name a page's.

use std::collections::HashMap;

use serde_json::Value;

/// What says two connections are the same page (or the same page's same
/// pane, through a gateway).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    who: String,
    client: String,
    instance: String,
    pane: Option<String>,
}

/// An instance as a page makes one: hex digits, 8 to 32 of them.
pub fn instance_of(v: Option<&Value>) -> Option<&str> {
    v.and_then(|v| v.as_str())
        .filter(|i| (8..=32).contains(&i.len()) && i.bytes().all(|b| b.is_ascii_hexdigit()))
}

impl Key {
    /// From a connection's sign-in message, or nothing when it does not
    /// ask to be fenced. `who`: the caller it was let in as. `client`: the
    /// client id the station took it for. `from_station`: it came signed
    /// by another station of the fleet (a gateway).
    ///
    /// A page: `{"auth": {"client", "instance"}}`.
    /// A gateway: `{"auth": {"client", "fence": {"pane", "instance"}}}`.
    pub fn from_auth(who: &str, client: &str, from_station: bool, auth: &Value) -> Option<Key> {
        let (instance, pane) = if from_station {
            let fence = auth.get("fence")?;
            let pane = fence
                .get("pane")
                .and_then(|p| p.as_str())
                .filter(|p| !p.is_empty() && p.len() <= 128)?;
            (instance_of(fence.get("instance"))?, Some(pane.to_string()))
        } else {
            (instance_of(auth.get("instance"))?, None)
        };
        Some(Key {
            who: who.to_string(),
            client: client.to_string(),
            instance: instance.to_string(),
            pane,
        })
    }
}

/// The connections that asked to be fenced, by key. `T`: how the one that
/// holds a key is closed.
pub struct Table<T> {
    live: HashMap<Key, (u64, T)>,
    next: u64,
}

impl<T> Default for Table<T> {
    fn default() -> Self {
        Table {
            live: HashMap::new(),
            next: 1,
        }
    }
}

impl<T> Table<T> {
    /// A connection has signed in. Its ticket, and the older connection
    /// with the same key, which the caller closes.
    pub fn enter(&mut self, key: Key, what: T) -> (u64, Option<T>) {
        let ticket = self.next;
        self.next += 1;
        let older = self.live.insert(key, (ticket, what)).map(|(_, t)| t);
        (ticket, older)
    }

    /// A connection has ended: its key is let go of, unless a newer
    /// connection has taken it since.
    pub fn leave(&mut self, key: &Key, ticket: u64) {
        if self.live.get(key).is_some_and(|(t, _)| *t == ticket) {
            self.live.remove(key);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.live.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const A: &str = "0123456789abcdef";
    const B: &str = "fedcba9876543210";

    fn page(client: &str, instance: &str) -> Option<Key> {
        Key::from_auth(
            "you",
            client,
            false,
            &json!({"client": client, "instance": instance}),
        )
    }

    fn gateway(client: &str, instance: &str, pane: &str) -> Option<Key> {
        Key::from_auth(
            "you (via alpha)",
            client,
            true,
            &json!({"client": client, "fence": {"pane": pane, "instance": instance}}),
        )
    }

    /// The second review's blocker 6, as a table. Fenced on the client id
    /// alone, a duplicated tab (which copies the id) and its original would
    /// close each other for ever, and a tab's second pane through a gateway
    /// would close its first.
    #[test]
    fn a_connection_replaces_only_the_same_page_come_back() {
        let mut t: Table<&str> = Table::default();
        // The same client and instance: the older one is handed back to
        // be closed.
        let (first, older) = t.enter(page("web-1-aa", A).unwrap(), "first");
        assert_eq!(older, None);
        let (second, older) = t.enter(page("web-1-aa", A).unwrap(), "second");
        assert_eq!(older, Some("first"));
        // The one that was closed ends a moment later: it does not take
        // the key away from the one that replaced it.
        t.leave(&page("web-1-aa", A).unwrap(), first);
        let (_, older) = t.enter(page("web-1-aa", A).unwrap(), "third");
        assert_eq!(older, Some("second"));
        t.leave(&page("web-1-aa", A).unwrap(), second);
        assert_eq!(t.len(), 1);

        // A duplicated tab: the same client id, an instance of its own.
        // Both stay.
        let (_, older) = t.enter(page("web-1-aa", B).unwrap(), "the duplicate");
        assert_eq!(older, None);
        // Another tab with the same instance (which cannot happen by
        // chance) is still another client.
        assert_eq!(t.enter(page("web-1-bb", A).unwrap(), "other tab").1, None);

        // A gateway: the same tab and pane closes the older connection;
        // the tab's other pane, and another page's connection for the same
        // pane, are left alone.
        let (_, older) = t.enter(gateway("gw-alpha-web-1-aa", A, "p1").unwrap(), "p1");
        assert_eq!(older, None);
        let (_, older) = t.enter(gateway("gw-alpha-web-1-aa", A, "p2").unwrap(), "p2");
        assert_eq!(older, None);
        let (_, older) = t.enter(gateway("gw-alpha-web-1-aa", B, "p1").unwrap(), "p1, dup");
        assert_eq!(older, None);
        let (_, older) = t.enter(gateway("gw-alpha-web-1-aa", A, "p1").unwrap(), "p1 again");
        assert_eq!(older, Some("p1"));
        // A page cannot name a gateway's connection, nor a gateway a
        // page's: who was let in is part of the key.
        assert_ne!(
            page("gw-alpha-web-1-aa", A),
            Key::from_auth(
                "you (via alpha)",
                "gw-alpha-web-1-aa",
                false,
                &json!({"instance": A})
            )
        );

        // No fence asked for: nothing is kept, so nothing is ever closed.
        // A page from before sends no instance; a gateway from before no
        // `fence`; and a gateway's `fence` needs the page's instance.
        assert_eq!(
            Key::from_auth("you", "web-1-aa", false, &json!({"client": "web-1-aa"})),
            None
        );
        assert_eq!(
            Key::from_auth(
                "you (via alpha)",
                "gw-alpha-web-1-aa",
                true,
                &json!({"instance": A})
            ),
            None
        );
        assert_eq!(gateway("gw-alpha-web-1-aa", "", "p1"), None);
        assert_eq!(gateway("gw-alpha-web-1-aa", A, ""), None);
        // An instance is hex digits, 8 to 32 of them: anything else is
        // not one.
        for bad in ["", "abc", "not-hex-digits!!", &"a".repeat(33)] {
            assert_eq!(page("web-1-aa", bad), None, "{bad:?}");
        }
    }
}
