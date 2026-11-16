//! What a Control Deck types into a pane (DECISIONS.md, "A key can never
//! reach another pane" and "What is typed"): the switches and the words a
//! station announces, kept here because a station says them in `caps`.

/// The switch (`[flags] deck_names = false`, or `station.flags`; read when
/// a connection begins), and what a station says in `caps` when it is on:
/// it takes the name a page gives an attachment (`pane.attach {chan}`), and
/// a page or a gateway that comes back replaces its own earlier connection.
pub const NAMES_FLAG: &str = "deck_names";
pub const NAMES_CAP: &str = "names";
