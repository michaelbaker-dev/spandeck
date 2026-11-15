//! The lanes of a Control Deck's connection.
//!
//! A page sends everything down one socket. Its requests used to be taken
//! one at a time, so a key typed into a pane on this station stood behind
//! every question the page had asked of every other station: up to 19.5 s
//! with one station cut off (measured on 0.4.2). They are now read in order
//! and run side by side, and this file is the rule for what may run beside
//! what:
//!
//! - **What is sent to one pane keeps its order.** Keys, a paste, a line of
//!   text, a resize, Stop, Pause, Release and Kill for one pane are applied
//!   in the order they were read, whether they name the pane or a channel
//!   and whether they are for this station or pass through it to another.
//!   "Stop every agent" and "Release all" stand in that order for every
//!   pane of their station.
//! - **Changes to one station keep their order.** Every other call that
//!   changes something is made in the order read, per station.
//! - **Reads keep none**, and only a few are in flight at once.
//!
//! Nothing here waits for anything: a request takes its place in a line
//! when it is read (`Lines::enter`), and the task that runs it waits for
//! its turn (`Place::turn`). A place is given up when it is dropped.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::Value;
pub use spandeck_core::perf::Lane;
use tokio::sync::watch;

/// The switch (`[flags] ws_lanes = false`, or `station.flags`). Off: a
/// connection's requests are taken one at a time, as before. Read when a
/// connection is made.
pub const FLAG: &str = "ws_lanes";

/// What a page says at sign-in (`auth.caps`) when it knows the word `busy`
/// and `input_refused` on a channel.
pub const CAP_BUSY: &str = "busy";

/// Requests of one connection that are waiting or running, over which the
/// next is refused at once.
pub const MAX_WAITING: usize = 256;
/// Reads in flight per connection for this station, and for each other
/// station. Each station has its own, so stations that do not answer hold
/// up no read of this station or of one that does.
pub const READS: usize = 16;
pub const READS_PER_STATION: usize = 4;
/// Attaches (and conversations being opened) in flight per connection.
pub const ATTACHES: usize = 8;

/// How many requests of one lane may be waiting or running for one other
/// station, per connection. Each lane has its own, so the reads a page
/// keeps asking of a station that does not answer cannot crowd out a Stop
/// for it.
pub fn station_limit(lane: Lane) -> usize {
    match lane {
        Lane::Read => READS_PER_STATION + 16,
        Lane::Write => 16,
        Lane::Urgent => 16,
        Lane::Input => 64,
        Lane::Attach | Lane::Held => MAX_WAITING,
    }
}

/// What a request in a line is addressed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// One pane, by the name the request gave (its id, where that could be
    /// found out without waiting).
    Pane(String),
    /// Every pane of the station: "stop every agent", "Release all".
    AllPanes,
    /// A change to the station that is not for a pane's input or its marks.
    Write,
}

impl Scope {
    /// Must one of two requests for the same station wait for the other?
    fn crosses(&self, other: &Scope) -> bool {
        match (self, other) {
            (Scope::Write, Scope::Write) => true,
            // A Stop never waits behind a change to the station, however
            // long that takes, nor a change behind what is typed.
            (Scope::Write, _) | (_, Scope::Write) => false,
            (Scope::AllPanes, _) | (_, Scope::AllPanes) => true,
            (Scope::Pane(a), Scope::Pane(b)) => a == b,
        }
    }
}

struct Entry {
    id: u64,
    scope: Scope,
    /// Something typed or sent, as against a stop, a mark or a change.
    input: bool,
}

#[derive(Default)]
struct State {
    next: u64,
    /// Per station ("" is this one), in the order read.
    lines: HashMap<String, Vec<Entry>>,
}

/// The lines of one connection: per station, the requests that were read
/// and are not yet done, in the order they were read.
#[derive(Clone)]
pub struct Lines {
    state: Arc<Mutex<State>>,
    moved: watch::Sender<u64>,
}

impl Default for Lines {
    fn default() -> Self {
        Lines {
            state: Arc::default(),
            moved: watch::channel(0).0,
        }
    }
}

impl Lines {
    /// Is nothing in the station's line that a request with this scope
    /// would have to wait for? Then what is typed goes straight to its
    /// pane, in the read loop itself.
    pub fn free(&self, station: &str, scope: &Scope) -> bool {
        let st = self.state.lock().unwrap();
        st.lines
            .get(station)
            .is_none_or(|line| !line.iter().any(|e| e.scope.crosses(scope)))
    }

    /// Is something typed for this pane still waiting in the station's
    /// line? A stop read now takes its place in that pane's input when its
    /// turn comes, not ahead of what was typed before it.
    pub fn input_waits(&self, station: &str, pane: &str) -> bool {
        let st = self.state.lock().unwrap();
        st.lines.get(station).is_some_and(|line| {
            line.iter()
                .any(|e| e.input && matches!(&e.scope, Scope::Pane(p) if p == pane))
        })
    }

    /// Take a place at the end of the station's line. Never waits.
    pub fn enter(&self, station: &str, scope: Scope, input: bool) -> Place {
        let mut st = self.state.lock().unwrap();
        st.next += 1;
        let id = st.next;
        st.lines
            .entry(station.to_string())
            .or_default()
            .push(Entry { id, scope, input });
        Place {
            lines: self.clone(),
            station: station.to_string(),
            id,
        }
    }

    fn is_turn(&self, station: &str, id: u64) -> bool {
        let st = self.state.lock().unwrap();
        let Some(line) = st.lines.get(station) else {
            return true;
        };
        let Some(at) = line.iter().position(|e| e.id == id) else {
            return true;
        };
        !line[..at].iter().any(|e| e.scope.crosses(&line[at].scope))
    }
}

/// A request's place in a station's line, held until it is dropped.
pub struct Place {
    lines: Lines,
    station: String,
    id: u64,
}

impl Place {
    /// Wait until nothing read before this request, and not yet done,
    /// is something it must follow.
    pub async fn turn(&self) {
        let mut moved = self.lines.moved.subscribe();
        while !self.lines.is_turn(&self.station, self.id) {
            if moved.changed().await.is_err() {
                return;
            }
        }
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut st = self.lines.state.lock().unwrap();
        if let Some(line) = st.lines.get_mut(&self.station) {
            line.retain(|e| e.id != self.id);
            if line.is_empty() {
                st.lines.remove(&self.station);
            }
        }
        drop(st);
        self.lines.moved.send_modify(|n| *n += 1);
    }
}

impl spandeck_core::rpc::StopLeg for Place {
    fn turn(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(Place::turn(self))
    }
}

/// How many requests of each lane a connection has waiting or running, in
/// all and per other station.
#[derive(Clone, Default)]
pub struct Counts {
    held: Arc<Mutex<Held>>,
}

#[derive(Default)]
struct Held {
    all: usize,
    /// By station and lane.
    by: HashMap<(String, u8), usize>,
}

/// One request counted; uncounted when dropped.
pub struct Counted {
    counts: Counts,
    key: Option<(String, u8)>,
}

impl Counts {
    /// Count one more request, or say how many are already waiting where
    /// it would wait: for the connection (`None`), or for that station.
    /// `station`: another station's name; `None` for this one, which has
    /// no limit of its own.
    pub fn admit(&self, station: Option<&str>, lane: Lane) -> Result<Counted, Busy> {
        let mut held = self.held.lock().unwrap();
        if held.all >= MAX_WAITING {
            return Err(Busy {
                station: None,
                waiting: held.all,
            });
        }
        let key = station.map(|s| (s.to_string(), lane as u8));
        if let Some(key) = &key {
            let n = held.by.get(key).copied().unwrap_or(0);
            if n >= station_limit(lane) {
                return Err(Busy {
                    station: Some(key.0.clone()),
                    waiting: n,
                });
            }
            *held.by.entry(key.clone()).or_default() += 1;
        }
        held.all += 1;
        Ok(Counted {
            counts: self.clone(),
            key,
        })
    }

    #[cfg(test)]
    fn waiting(&self) -> usize {
        self.held.lock().unwrap().all
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        let mut held = self.counts.held.lock().unwrap();
        held.all -= 1;
        if let Some(key) = &self.key {
            if let Some(n) = held.by.get_mut(key) {
                *n -= 1;
                if *n == 0 {
                    held.by.remove(key);
                }
            }
        }
    }
}

/// Too much is already waiting: the request was not taken.
#[derive(Debug, PartialEq, Eq)]
pub struct Busy {
    /// The station it was for, when it is that station's requests that
    /// are waiting.
    pub station: Option<String>,
    pub waiting: usize,
}

impl Busy {
    /// The answer. A page that said it knows `busy` gets that code; one
    /// from before gets the code any failed call had.
    pub fn error(&self, page_knows: bool) -> Value {
        let message = match &self.station {
            Some(s) => format!(
                "{s} is busy: {} requests from this page are still waiting for it, so this one was not sent. Nothing was changed",
                self.waiting
            ),
            None => format!(
                "this page has {} requests waiting, so this one was not taken. Nothing was changed",
                self.waiting
            ),
        };
        serde_json::json!({"code": if page_knows { CAP_BUSY } else { "internal" }, "message": message})
    }
}

/// Where a request goes: its lane, the station it is for, and what it must
/// keep its order with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub lane: Lane,
    /// Another station, reached through this one; `None` for this station.
    pub station: Option<String>,
    /// The method that acts: what a `peer.call` or `rpc.forward` carries.
    pub method: String,
    pub scope: Option<Scope>,
    /// `stop.all` only: the scope it names ("station", "fleet", a name).
    pub stop_scope: Option<String>,
    /// Ended with the connection: a read nobody will see answered, a wait.
    /// Everything else runs to its end once it was read.
    pub ends_with_connection: bool,
}

/// Which lane a request belongs in. `own`: this station's name.
///
/// Decided by the method that acts, so a key typed through `peer.call`
/// (a phone typing into a pane it watches) is typing, and a Stop passed on
/// to another station is a Stop.
pub fn route(own: &str, method: &str, params: &Value) -> Route {
    let (mut method, mut params) = (method, params);
    let mut station = None;
    // A call for this station wrapped as one for "a station" is the call.
    for _ in 0..4 {
        match method {
            "peer.call" => {
                let to = params.get("station").and_then(|s| s.as_str()).unwrap_or("");
                let inner = params.get("method").and_then(|m| m.as_str());
                let Some(inner) = inner else { break };
                if to != own {
                    station = Some(to.to_string());
                }
                method = inner;
                params = params.get("params").unwrap_or(&Value::Null);
                if station.is_some() {
                    break;
                }
            }
            "rpc.forward" => {
                let Some(inner) = params.get("method").and_then(|m| m.as_str()) else {
                    break;
                };
                method = inner;
                params = params.get("params").unwrap_or(&Value::Null);
            }
            _ => break,
        }
    }
    let pane = || {
        params
            .get("pane")
            .and_then(|p| p.as_str())
            .map(|p| Scope::Pane(p.to_string()))
    };
    let all = params.get("all").and_then(|a| a.as_bool()) == Some(true);
    let (lane, scope) = match method {
        "pane.input" | "pane.paste" | "pane.send_text" | "pane.resize" => (Lane::Input, pane()),
        "pane.stop" | "pane.kill" | "pane.pause" => (Lane::Urgent, pane()),
        "pane.release" if all => (Lane::Urgent, Some(Scope::AllPanes)),
        "pane.release" => (Lane::Urgent, pane()),
        "stop.all" => (Lane::Urgent, Some(Scope::AllPanes)),
        "approval.answer" => (Lane::Urgent, None),
        "pane.attach" | "pane.conversation.watch" => (Lane::Attach, None),
        // Calls that wait on purpose: for an agent, a task, the owner's
        // answer at another station. Nothing stands behind them.
        "wait" | "task.wait" | "hook.ingest" | "trust.approve" | "fleet.join" => (Lane::Held, None),
        m if spandeck_core::rpc::reads(m) => (Lane::Read, None),
        _ => (Lane::Write, Some(Scope::Write)),
    };
    Route {
        lane,
        station,
        method: method.to_string(),
        scope,
        stop_scope: (method == "stop.all").then(|| {
            params
                .get("scope")
                .and_then(|s| s.as_str())
                .unwrap_or("station")
                .to_string()
        }),
        ends_with_connection: matches!(lane, Lane::Read | Lane::Attach)
            || matches!(method, "wait" | "task.wait"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Semaphore;

    /// What the connection does with a request, without the station: the
    /// lane's rule and nothing else. `work` is the request's handler; one
    /// that never finishes is a station that does not answer.
    struct Deck {
        lines: Lines,
        counts: Counts,
        reads: Arc<Semaphore>,
        per_station: HashMap<String, Arc<Semaphore>>,
        log: Arc<Mutex<Vec<String>>>,
        ending: tokio::task::JoinSet<()>,
    }

    impl Deck {
        fn new() -> Deck {
            Deck {
                lines: Lines::default(),
                counts: Counts::default(),
                reads: Arc::new(Semaphore::new(READS)),
                per_station: HashMap::new(),
                log: Arc::default(),
                ending: tokio::task::JoinSet::new(),
            }
        }

        /// Read one request. `hangs`: its handler never finishes.
        fn read(&mut self, name: &str, method: &str, params: Value, hangs: bool) -> Option<Busy> {
            let r = route("here", method, &params);
            let station = r.station.clone().unwrap_or_default();
            let (log, name) = (self.log.clone(), name.to_string());
            let work = async move {
                if hangs {
                    std::future::pending::<()>().await;
                }
                log.lock().unwrap().push(name);
            };
            // What is typed for a pane nothing stands before goes at once.
            if r.lane == Lane::Input {
                if let Some(scope) = r.scope.as_ref().filter(|_| r.station.is_none()) {
                    if self.lines.free("", scope) {
                        assert!(!hangs);
                        futures_util::FutureExt::now_or_never(work).expect("inline");
                        return None;
                    }
                }
            }
            let counted = match self.counts.admit(r.station.as_deref(), r.lane) {
                Ok(c) => c,
                Err(busy) => return Some(busy),
            };
            let place = r
                .scope
                .clone()
                .map(|s| self.lines.enter(&station, s, r.lane == Lane::Input));
            let reads = (r.lane == Lane::Read).then(|| match &r.station {
                Some(s) => self
                    .per_station
                    .entry(s.clone())
                    .or_insert_with(|| Arc::new(Semaphore::new(READS_PER_STATION)))
                    .clone(),
                None => self.reads.clone(),
            });
            let task = async move {
                let _counted = counted;
                let _held = match reads {
                    Some(places) => Some(places.acquire_owned().await.unwrap()),
                    None => None,
                };
                if let Some(place) = &place {
                    place.turn().await;
                }
                work.await;
            };
            if r.ends_with_connection {
                self.ending.spawn(task);
            } else {
                tokio::spawn(task);
            }
            None
        }

        async fn settle(&self) -> Vec<String> {
            for _ in 0..50 {
                tokio::task::yield_now().await;
            }
            self.log.lock().unwrap().clone()
        }
    }

    fn peer(station: &str, method: &str, params: Value) -> Value {
        json!({"station": station, "method": method, "params": params})
    }

    /// The audit's gap 1, measured on 0.4.2: a key typed into a pane on
    /// this station took up to 19.5 s with one other station cut off,
    /// because it stood behind the page's questions to that station. Here
    /// a hundred questions to a station that never answers are waiting,
    /// and five more such stations hold every read they may, and a key, a
    /// Stop, a change and a read for anywhere else are done.
    #[tokio::test]
    async fn a_station_that_does_not_answer_holds_up_only_itself() {
        let mut deck = Deck::new();
        let mut refused = 0;
        for i in 0..100 {
            let ask = peer("gone", "pane.list", json!({}));
            refused += deck
                .read(&format!("gone read {i}"), "peer.call", ask, true)
                .is_some() as usize;
        }
        // Past what one station may have waiting, the page is told at once.
        assert_eq!(refused, 100 - station_limit(Lane::Read));
        // Five more stations that do not answer, each with every read it
        // may have in flight: more than this station's own reads have.
        for gone in ["g1", "g2", "g3", "g4", "g5"] {
            for i in 0..READS_PER_STATION {
                let ask = peer(gone, "pane.list", json!({}));
                let name = format!("{gone} read {i}");
                assert!(deck.read(&name, "peer.call", ask, true).is_none());
            }
        }
        let p = json!({"pane": "p1", "data": "x"});
        assert!(deck.read("key", "pane.input", p, false).is_none());
        assert_eq!(deck.settle().await, ["key"], "typed in the read loop");
        deck.read("stop", "pane.stop", json!({"pane": "p1"}), false);
        deck.read("read here", "pane.list", json!({}), false);
        deck.read("change here", "pane.rename", json!({"pane": "p1"}), false);
        let other = peer("far", "pane.list", json!({}));
        deck.read("read far", "peer.call", other, false);
        // A Stop and a change for the station that does not answer wait
        // for it and for nothing else.
        let stop = peer("gone", "pane.stop", json!({"pane": "g1"}));
        deck.read("stop gone", "peer.call", stop, true);
        let mut done = deck.settle().await;
        done.sort();
        assert_eq!(
            done,
            ["change here", "key", "read far", "read here", "stop"]
        );
        // The connection closes: the questions nobody will read the
        // answers to are ended, and their places are free again.
        deck.ending.shutdown().await;
        assert_eq!(deck.counts.waiting(), 1, "only the Stop for gone is left");
    }

    /// What is sent to one pane keeps the order it was read in, whatever
    /// lane each thing is in: Pause, then a paste, then Enter (REVIEW B5:
    /// "Paste then Enter ... Enter can overtake"), a phone typing through
    /// `peer.call`, and two changes to one station.
    #[tokio::test]
    async fn what_is_for_one_pane_keeps_its_order() {
        let mut deck = Deck::new();
        let gate = Arc::new(AtomicBool::new(false));
        // A Pause that takes its time (it waits for the database).
        let place = deck.lines.enter("", Scope::Pane("p1".into()), false);
        let (log, open) = (deck.log.clone(), gate.clone());
        tokio::spawn(async move {
            place.turn().await;
            while !open.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            log.lock().unwrap().push("pause p1".into());
        });
        let p1 = || json!({"pane": "p1"});
        deck.read("paste p1", "pane.paste", p1(), false);
        deck.read("enter p1", "pane.input", p1(), false);
        deck.read("stop p1", "pane.stop", p1(), false);
        deck.read("after stop p1", "pane.send_text", p1(), false);
        // Another pane is not held up by any of that.
        deck.read("key p2", "pane.input", json!({"pane": "p2"}), false);
        assert!(deck.lines.input_waits("", "p1"));
        assert!(!deck.lines.input_waits("", "p2"));
        assert_eq!(deck.settle().await, ["key p2"]);
        gate.store(true, Ordering::SeqCst);
        assert_eq!(
            deck.settle().await,
            [
                "key p2",
                "pause p1",
                "paste p1",
                "enter p1",
                "stop p1",
                "after stop p1"
            ]
        );
        assert!(deck.lines.free("", &Scope::Pane("p1".into())));

        // Through another station: each call is a round trip of its own,
        // and still one after the other for one pane, and for one station's
        // changes. "Stop every agent" there stands in every pane's order.
        let mut deck = Deck::new();
        for (name, method, params) in [
            ("far paste", "pane.paste", json!({"pane": "f1"})),
            ("far enter", "pane.input", json!({"pane": "f1"})),
            ("far other pane", "pane.input", json!({"pane": "f2"})),
            ("far stop all", "stop.all", json!({})),
            ("far release", "pane.release", json!({"pane": "f1"})),
            ("far rename", "pane.rename", json!({"pane": "f1"})),
            ("far close", "pane.close", json!({"pane": "f1"})),
        ] {
            deck.read(name, "peer.call", peer("far", method, params), false);
        }
        let done = deck.settle().await;
        let at = |name: &str| done.iter().position(|d| d == name).expect(name);
        assert!(at("far paste") < at("far enter"));
        assert!(at("far enter") < at("far stop all"));
        assert!(at("far other pane") < at("far stop all"));
        assert!(at("far stop all") < at("far release"));
        assert!(at("far rename") < at("far close"));
    }

    /// A page cannot make a station hold more than 256 of its requests:
    /// the next is refused at once, with a word a newer page shows and an
    /// older page takes for any failed call.
    #[tokio::test]
    async fn the_257th_waiting_request_is_refused() {
        let mut deck = Deck::new();
        for i in 0..MAX_WAITING {
            let busy = deck.read(&format!("{i}"), "pane.rename", json!({}), true);
            assert!(busy.is_none(), "{i}");
        }
        let busy = deck
            .read("one more", "pane.rename", json!({}), false)
            .expect("refused");
        assert_eq!(busy.station, None);
        assert_eq!(busy.error(true)["code"], "busy");
        assert_eq!(busy.error(false)["code"], "internal");
        // What is typed into a pane nothing stands before is still taken.
        let key = json!({"pane": "p1"});
        assert!(deck.read("key", "pane.input", key, false).is_none());
        assert_eq!(deck.settle().await, ["key"]);
    }

    /// Which lane: decided by the method that acts, wherever it is going.
    #[test]
    fn the_method_that_acts_decides_the_lane() {
        let lane = |method: &str, params: Value| {
            let r = route("here", method, &params);
            (r.lane, r.station, r.scope)
        };
        let pane = |p: &str| Some(Scope::Pane(p.to_string()));
        // A phone typing into a pane it only watches (REVIEW B5).
        let typed = peer("far", "pane.input", json!({"pane": "p9", "data": "y"}));
        assert_eq!(
            lane("peer.call", typed),
            (Lane::Input, Some("far".into()), pane("p9"))
        );
        // A call for this station, wrapped: the call itself.
        let wrapped = peer("here", "pane.pause", json!({"pane": "p1"}));
        assert_eq!(lane("peer.call", wrapped), (Lane::Urgent, None, pane("p1")));
        assert_eq!(
            lane("pane.release", json!({"all": true})),
            (Lane::Urgent, None, Some(Scope::AllPanes))
        );
        assert_eq!(
            lane("approval.answer", json!({})),
            (Lane::Urgent, None, None)
        );
        assert_eq!(lane("pane.list", json!({})), (Lane::Read, None, None));
        let update = peer("far", "station.update", json!({}));
        assert_eq!(
            lane("peer.call", update),
            (Lane::Write, Some("far".into()), Some(Scope::Write))
        );
        let r = route("here", "wait", &json!({}));
        assert!(r.lane == Lane::Held && r.ends_with_connection);
        assert!(!route("here", "pane.rename", &json!({})).ends_with_connection);
        assert!(!route("here", "pane.stop", &json!({})).ends_with_connection);
        let stop = route("here", "stop.all", &json!({"scope": "fleet"}));
        assert_eq!(stop.stop_scope.as_deref(), Some("fleet"));
    }
}
