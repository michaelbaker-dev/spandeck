//! What is typed, pasted or sent into a pane, on its way to the program.
//!
//! A program that is not reading (`sleep 600`, one stopped in a debugger)
//! lets the terminal fill up, and a write then waits for as long as that
//! lasts. That wait used to be taken by whoever sent the input, holding the
//! pane's lock: a 20 KB paste into such a pane stopped the whole station
//! answering, Kill included (found on 15 Nov 2026). So input is handed to a
//! thread of the pane's own, in order, and the sender never waits.
//!
//! What waits is whole items, not bytes (DECISIONS.md, "A stopped agent
//! stays stopped"): keys, a paste, sent text, a nudge. A stop marks its
//! place in the line (`mark`), and when it runs (`interrupt`):
//!
//! - an item sent before the mark and not yet started is thrown away
//!   whole, and its sender can be told;
//! - an item being written is cut at the next 4 KB, and a paste the program
//!   was told is a paste is closed (`ESC[201~`) first, so the interrupt key
//!   is not read as pasted text;
//! - the interrupt keys are written next, ahead of everything else;
//! - what was sent after the mark is kept, and written after the keys.
//!
//! A key cannot enter a terminal that is full either. The keys are given
//! two seconds; when the terminal has not taken them by then they are
//! taken back, never written late, and the stop says `delivered: false`:
//! the program is not reading, and was not stopped.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How much input may wait for a program that is not reading before more is
/// refused.
const WAITING_MAX: usize = 8 << 20;

/// An item is written this much at a time, and a stop is looked for between.
pub const PIECE: usize = 4096;

/// How long a stop's keys are given to enter the terminal.
pub const INTERRUPT_WAIT: Duration = Duration::from_secs(2);

/// The marker that ends a paste, for a program that asked for pastes to be
/// marked.
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Keys as they are typed, and anything else sent as it is.
    Keys,
    /// A paste. `bracketed`: it begins with `ESC[200~` and ends with
    /// `ESC[201~`, because the program asked for that.
    Paste { bracketed: bool },
    /// A line of text sent to the program (a message box, the command line).
    Text,
    /// The one typed line that wakes an idle agent that has mail.
    Nudge,
}

/// One thing sent into a pane.
#[derive(Debug, Clone)]
pub struct Item {
    pub kind: Kind,
    pub bytes: Vec<u8>,
    /// Press Enter this long after the bytes: agents that detect pastes
    /// need the Enter as a write of its own, a moment later.
    pub enter_after: Option<Duration>,
    /// Who sent it (a client's id), so that it can be told when the item
    /// is thrown away. Never looked into here.
    pub from: Option<String>,
}

impl Item {
    pub fn keys(bytes: &[u8]) -> Item {
        Item {
            kind: Kind::Keys,
            bytes: bytes.to_vec(),
            enter_after: None,
            from: None,
        }
    }
}

/// An item a stop threw away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discarded {
    pub id: u64,
    pub kind: Kind,
    pub from: Option<String>,
    /// It had been started: the program has the beginning of it.
    pub cut: bool,
}

/// What came of a stop's keys.
#[derive(Debug, Default)]
pub struct Interrupt {
    pub discarded: Vec<Discarded>,
    /// The terminal took the keys. False: it is full because the program is
    /// not reading (or the terminal is gone), the keys were not written and
    /// will not be, and the program was not stopped.
    pub delivered: bool,
}

struct Urgent {
    token: u64,
    keys: Vec<u8>,
}

#[derive(Default)]
struct State {
    items: VecDeque<(u64, Item)>,
    /// The id the next item sent is given.
    next: u64,
    /// The item being written: its id, kind and sender.
    writing: Option<(u64, Kind, Option<String>)>,
    /// How much of it is in pieces not yet begun, and whether its Enter
    /// is still to come: with neither, a stop can take nothing from it.
    writing_rest: usize,
    writing_enter: bool,
    /// The item being written is to stop at the next piece.
    cut: bool,
    /// Stops' places: items sent at or after the earliest one wait until
    /// that stop has run or been dropped.
    marks: Vec<u64>,
    urgent: VecDeque<Urgent>,
    next_token: u64,
    /// The token of the stop whose keys are being written now.
    urgent_writing: Option<u64>,
    /// Tokens of stops whose keys the terminal took.
    urgent_done: Vec<u64>,
    /// Bytes not yet written.
    waiting: usize,
    /// The terminal refuses writes: nothing more can be sent.
    closed: bool,
    /// The pane is gone. What it was sent before that is still written.
    dropped: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// A stop's place in a pane's input. Everything sent after it waits until
/// the stop has run (`Input::interrupt`) or the mark is dropped.
pub struct Mark {
    at: u64,
    shared: Arc<Shared>,
    live: bool,
}

impl Mark {
    fn take(&mut self, st: &mut State) {
        if self.live {
            self.live = false;
            if let Some(i) = st.marks.iter().position(|m| *m == self.at) {
                st.marks.swap_remove(i);
            }
        }
    }
}

impl Drop for Mark {
    fn drop(&mut self) {
        if self.live {
            let shared = self.shared.clone();
            let mut st = shared.state.lock().unwrap();
            self.take(&mut st);
            drop(st);
            shared.wake.notify_all();
        }
    }
}

pub(crate) struct Input {
    shared: Arc<Shared>,
}

enum Job {
    Urgent(Urgent),
    Item(Item),
}

impl Input {
    pub(crate) fn start(writer: Box<dyn Write + Send>) -> io::Result<Input> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let theirs = shared.clone();
        thread::Builder::new()
            .name("pane-input".into())
            .spawn(move || run(theirs, writer))?;
        Ok(Input { shared })
    }

    /// Put an item in the line. Never waits for the program. Refused when
    /// too much already waits.
    pub(crate) fn send(&self, item: Item) -> io::Result<u64> {
        self.send_if(item, || true).map(|id| id.unwrap_or(0))
    }

    /// Like `send`, when `ok` says so. `ok` is asked with the line locked,
    /// so that "look, then type" and `withdraw` cannot pass each other: an
    /// item is either in the line when `withdraw` runs, or `ok` is asked
    /// after whatever `withdraw`'s caller set. `Ok(None)`: `ok` said no.
    pub(crate) fn send_if(&self, item: Item, ok: impl FnOnce() -> bool) -> io::Result<Option<u64>> {
        let mut st = self.shared.state.lock().unwrap();
        if st.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "this pane's terminal is closed",
            ));
        }
        if st.waiting + item.bytes.len() > WAITING_MAX {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the program in this pane is not reading what is sent to it; nothing more was sent",
            ));
        }
        if !ok() {
            return Ok(None);
        }
        let id = st.next;
        st.next += 1;
        st.waiting += item.bytes.len();
        st.items.push_back((id, item));
        drop(st);
        self.shared.wake.notify_all();
        Ok(Some(id))
    }

    /// Take back every item of `kind` that has not been started. How many.
    pub(crate) fn withdraw(&self, kind: Kind) -> usize {
        let mut st = self.shared.state.lock().unwrap();
        let before = st.items.len();
        let mut freed = 0;
        st.items.retain(|(_, it)| {
            let keep = it.kind != kind;
            if !keep {
                freed += it.bytes.len();
            }
            keep
        });
        st.waiting -= freed;
        before - st.items.len()
    }

    /// A stop's place: now. Does not wait.
    pub(crate) fn mark(&self) -> Mark {
        let mut st = self.shared.state.lock().unwrap();
        let at = st.next;
        st.marks.push(at);
        Mark {
            at,
            shared: self.shared.clone(),
            live: true,
        }
    }

    /// Run a stop: see the top of this file. Waits up to `wait` for the
    /// terminal to take the keys, so it is not for a thread that must not
    /// wait.
    pub(crate) fn interrupt(&self, mut mark: Mark, keys: &[u8], wait: Duration) -> Interrupt {
        let mut st = self.shared.state.lock().unwrap();
        let mut out = Interrupt::default();
        let mut freed = 0;
        let at = mark.at;
        st.items.retain(|(id, it)| {
            let keep = *id >= at;
            if !keep {
                freed += it.bytes.len();
                out.discarded.push(Discarded {
                    id: *id,
                    kind: it.kind,
                    from: it.from.clone(),
                    cut: false,
                });
            }
            keep
        });
        st.waiting -= freed;
        if let Some((id, kind, from)) = st.writing.clone().filter(|w| w.0 < at) {
            st.cut = true;
            // Its last piece is already with the terminal and no Enter is
            // owed: it was sent whole, and nobody is told otherwise.
            if st.writing_rest > 0 || st.writing_enter {
                out.discarded.push(Discarded {
                    id,
                    kind,
                    from,
                    cut: true,
                });
            }
        }
        let token = st.next_token;
        st.next_token += 1;
        st.urgent.push_back(Urgent {
            token,
            keys: keys.to_vec(),
        });
        // The keys now stand ahead of everything: what was sent after the
        // mark no longer needs it to wait.
        mark.take(&mut st);
        self.shared.wake.notify_all();
        let until = Instant::now() + wait;
        loop {
            if let Some(i) = st.urgent_done.iter().position(|t| *t == token) {
                st.urgent_done.swap_remove(i);
                out.delivered = true;
                return out;
            }
            let left = until.saturating_duration_since(Instant::now());
            if st.closed || left.is_zero() {
                break;
            }
            st = self.shared.wake.wait_timeout(st, left).unwrap().0;
        }
        // Not taken: the keys are not written late, into whatever the
        // program is doing by then. (If the writer is inside the write of
        // these very keys there is nothing to take back; it is still said
        // to be not delivered, since nothing shows that it was.)
        st.urgent.retain(|u| u.token != token);
        out
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().dropped = true;
        self.shared.wake.notify_all();
    }
}

/// The pane's writer thread. Ends when the pane is dropped and what it was
/// sent is written (keys typed a moment before a daemon lets go of a pane
/// the pane host keeps must still reach the program), or when its terminal
/// is gone.
fn run(shared: Arc<Shared>, mut writer: Box<dyn Write + Send>) {
    loop {
        let job = {
            let mut st = shared.state.lock().unwrap();
            loop {
                if st.closed {
                    return;
                }
                if let Some(u) = st.urgent.pop_front() {
                    st.urgent_writing = Some(u.token);
                    break Job::Urgent(u);
                }
                // A pane that is gone has no stop to wait for.
                let held_from = match st.dropped {
                    true => u64::MAX,
                    false => st.marks.iter().min().copied().unwrap_or(u64::MAX),
                };
                if st.items.front().is_some_and(|(id, _)| *id < held_from) {
                    let (id, item) = st.items.pop_front().unwrap();
                    st.writing = Some((id, item.kind, item.from.clone()));
                    st.writing_rest = item.bytes.len();
                    st.writing_enter = item.enter_after.is_some();
                    st.cut = false;
                    break Job::Item(item);
                }
                // Gone, and everything it was sent is written.
                if st.dropped {
                    return;
                }
                st = shared.wake.wait(st).unwrap();
            }
        };
        let ok = match job {
            Job::Urgent(u) => {
                let done = writer.write_all(&u.keys).and_then(|_| writer.flush());
                let mut st = shared.state.lock().unwrap();
                st.urgent_writing = None;
                if done.is_ok() {
                    st.urgent_done.push(u.token);
                    // A stop nobody waits for any more left its token: bounded.
                    if st.urgent_done.len() > 64 {
                        st.urgent_done.remove(0);
                    }
                }
                done.is_ok()
            }
            Job::Item(item) => write_item(&shared, &mut writer, &item),
        };
        let mut st = shared.state.lock().unwrap();
        st.writing = None;
        st.cut = false;
        if !ok {
            st.closed = true;
            st.waiting = 0;
            st.items.clear();
        }
        drop(st);
        shared.wake.notify_all();
        if !ok {
            return;
        }
    }
}

/// Write one item, a piece at a time, stopping between pieces when a stop
/// cut it. False: the terminal is gone.
fn write_item(shared: &Shared, writer: &mut Box<dyn Write + Send>, item: &Item) -> bool {
    let mut written = 0;
    let mut cut = false;
    for piece in item.bytes.chunks(PIECE) {
        {
            let mut st = shared.state.lock().unwrap();
            if st.cut {
                cut = true;
                break;
            }
            st.writing_rest -= piece.len();
        }
        if writer.write_all(piece).is_err() {
            return false;
        }
        written += piece.len();
        let mut st = shared.state.lock().unwrap();
        st.waiting = st.waiting.saturating_sub(piece.len());
    }
    if !cut && written == item.bytes.len() {
        let Some(after) = item.enter_after else {
            return true;
        };
        // The Enter belongs to the item: a stop that lands in the moment
        // before it takes it away.
        let until = Instant::now() + after;
        let mut st = shared.state.lock().unwrap();
        loop {
            if st.cut || st.closed {
                return true;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            st = shared.wake.wait_timeout(st, left).unwrap().0;
        }
        st.writing_enter = false;
        drop(st);
        return writer.write_all(b"\r").is_ok();
    }
    {
        let mut st = shared.state.lock().unwrap();
        st.waiting = st.waiting.saturating_sub(item.bytes.len() - written);
    }
    // The program was told a paste began and has not been told it ended:
    // say so, or the interrupt key that follows is read as pasted text.
    if written > 0 && item.kind == (Kind::Paste { bracketed: true }) {
        return writer.write_all(PASTE_END).is_ok();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A terminal for the tests: every write is announced, and waits to be
    /// let through when the test holds the gate.
    #[derive(Clone)]
    struct Term {
        got: Arc<Mutex<Vec<u8>>>,
        /// Shut: a write waits, as on a terminal nobody reads.
        gate: Arc<(Mutex<bool>, Condvar)>,
        began: mpsc::Sender<usize>,
    }

    impl Term {
        fn new(gated: bool) -> (Term, mpsc::Receiver<usize>) {
            let (began, rx) = mpsc::channel();
            let term = Term {
                got: Arc::default(),
                gate: Arc::new((Mutex::new(gated), Condvar::new())),
                began,
            };
            (term, rx)
        }
        fn open(&self) {
            *self.gate.0.lock().unwrap() = false;
            self.gate.1.notify_all();
        }
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.got.lock().unwrap()).into_owned()
        }
    }

    impl Write for Term {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let _ = self.began.send(buf.len());
            let mut g = self.gate.0.lock().unwrap();
            while *g {
                g = self.gate.1.wait(g).unwrap();
            }
            drop(g);
            self.got.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn item(kind: Kind, text: &str) -> Item {
        Item {
            kind,
            bytes: text.as_bytes().to_vec(),
            enter_after: None,
            from: Some("deck".into()),
        }
    }

    /// Let the terminal take writes again once `when` is so.
    fn open_when(
        input: &Input,
        term: &Term,
        when: impl Fn(&State) -> bool + Send + 'static,
    ) -> thread::JoinHandle<()> {
        let (shared, term) = (input.shared.clone(), term.clone());
        thread::spawn(move || {
            let mut st = shared.state.lock().unwrap();
            while !when(&st) {
                st = shared.wake.wait(st).unwrap();
            }
            drop(st);
            term.open();
        })
    }

    /// Wait until the writer has taken everything it can.
    fn settle(input: &Input) {
        let mut st = input.shared.state.lock().unwrap();
        while !(st.items.is_empty() && st.writing.is_none() && st.urgent.is_empty()) && !st.closed {
            st = input.shared.wake.wait(st).unwrap();
        }
    }

    /// DECISIONS.md: everything sent to a pane goes through one line in the
    /// order it arrived, whatever kind it is, and a sent line's Enter
    /// follows its own text, not the next item's.
    #[test]
    fn items_of_every_kind_are_written_whole_and_in_order() {
        let (term, _began) = Term::new(false);
        let input = Input::start(Box::new(term.clone())).unwrap();
        input.send(item(Kind::Keys, "a")).unwrap();
        let mut paste = item(
            Kind::Paste { bracketed: true },
            "\x1b[200~two\rlines\x1b[201~",
        );
        paste.enter_after = Some(Duration::from_millis(1));
        input.send(paste).unwrap();
        let mut text = item(Kind::Text, "hello");
        text.enter_after = Some(Duration::from_millis(1));
        input.send(text).unwrap();
        input.send(item(Kind::Nudge, "mail")).unwrap();
        input.send(item(Kind::Keys, "z")).unwrap();
        settle(&input);
        assert_eq!(term.text(), "a\x1b[200~two\rlines\x1b[201~\rhello\rmailz");
    }

    /// REVIEW2 blocker 4: a stop throws away, whole, what was sent before
    /// it and not started; what was typed after the stop was read is kept,
    /// and written after the interrupt key, though the stop ran later.
    #[test]
    fn a_stop_discards_what_was_queued_before_it_and_keeps_what_came_after() {
        let (term, began) = Term::new(true);
        let input = Input::start(Box::new(term.clone())).unwrap();
        input.send(item(Kind::Keys, "first")).unwrap();
        began.recv().unwrap(); // the terminal has "first"; nothing else is started
        let before = input
            .send(item(Kind::Text, "typed before the stop"))
            .unwrap();
        let mark = input.mark();
        input
            .send(item(Kind::Keys, "typed after the stop"))
            .unwrap();
        let opened = open_when(&input, &term, |st| !st.urgent.is_empty());
        let done = input.interrupt(mark, b"\x03", INTERRUPT_WAIT);
        opened.join().unwrap();
        settle(&input);
        assert!(done.delivered);
        // "first" was with the terminal already: it is not said to be
        // discarded. The one not started is, whole, with who sent it.
        assert_eq!(
            done.discarded,
            vec![Discarded {
                id: before,
                kind: Kind::Text,
                from: Some("deck".into()),
                cut: false
            }]
        );
        assert_eq!(term.text(), "first\x03typed after the stop");
    }

    /// What is typed after a stop was read waits for that stop: it is never
    /// written ahead of the interrupt key, which would wipe it.
    #[test]
    fn what_is_sent_after_the_mark_waits_for_the_stop_or_for_the_mark_to_go() {
        let (term, _began) = Term::new(false);
        let input = Input::start(Box::new(term.clone())).unwrap();
        let mark = input.mark();
        input.send(item(Kind::Keys, "later")).unwrap();
        input.send(item(Kind::Keys, "!")).unwrap();
        assert_eq!(input.shared.state.lock().unwrap().items.len(), 2);
        assert_eq!(term.text(), "");
        // A stop that turned out not to be for this pane: nothing is held.
        drop(mark);
        settle(&input);
        assert_eq!(term.text(), "later!");
    }

    /// REVIEW2 blocker 4: a paste that was started is cut at the next
    /// piece and closed with `ESC[201~` before the interrupt key, so the
    /// program does not read the key as pasted text.
    #[test]
    fn a_started_bracketed_paste_is_closed_before_the_interrupt_key() {
        let (term, began) = Term::new(true);
        let input = Input::start(Box::new(term.clone())).unwrap();
        let body = "x".repeat(3 * PIECE);
        let mut paste = item(
            Kind::Paste { bracketed: true },
            &format!("\x1b[200~{body}\x1b[201~"),
        );
        paste.enter_after = Some(Duration::from_millis(1));
        let id = input.send(paste).unwrap();
        began.recv().unwrap(); // the first piece is being written
        let mark = input.mark();
        // Let the first piece through once the stop is in.
        let opened = open_when(&input, &term, |st| st.cut);
        let done = input.interrupt(mark, b"\x1b", INTERRUPT_WAIT);
        opened.join().unwrap();
        settle(&input);
        assert!(done.delivered);
        assert_eq!(
            done.discarded,
            vec![Discarded {
                id,
                kind: Kind::Paste { bracketed: true },
                from: Some("deck".into()),
                cut: true
            }]
        );
        let got = term.text();
        assert_eq!(
            got.len(),
            PIECE + PASTE_END.len() + 1,
            "one piece, the end marker, the key"
        );
        assert!(got.starts_with("\x1b[200~xxx"));
        assert!(
            got.ends_with("x\x1b[201~\x1b"),
            "no Enter, and the key last"
        );
    }

    /// REVIEW2 blocker 4 and "never claim confirmed": a program that is not
    /// reading cannot be given the key. The stop says so, and the key is
    /// not written later, when the program reads again.
    #[test]
    fn a_program_that_is_not_reading_is_not_reported_as_stopped() {
        let (term, began) = Term::new(true);
        let input = Input::start(Box::new(term.clone())).unwrap();
        input
            .send(item(Kind::Paste { bracketed: false }, "pasted"))
            .unwrap();
        began.recv().unwrap(); // the writer is stuck in the terminal
        let mark = input.mark();
        let done = input.interrupt(mark, b"\x03", Duration::from_millis(30));
        assert!(!done.delivered);
        assert!(done.discarded.is_empty(), "the paste is with the terminal");
        // The program reads again: it gets what was in the terminal, and
        // what is typed from now on, but never the old interrupt key.
        input.send(item(Kind::Keys, "later")).unwrap();
        term.open();
        settle(&input);
        assert_eq!(term.text(), "pastedlater");
    }

    /// The mail pump's check is made with the line locked, and a mark set
    /// on the pane takes back a nudge that has not been typed: "check, then
    /// type" cannot pass a Pause.
    #[test]
    fn a_nudge_is_sent_only_if_allowed_and_is_taken_back_when_a_hold_lands() {
        let (term, began) = Term::new(true);
        let input = Input::start(Box::new(term.clone())).unwrap();
        input.send(item(Kind::Keys, "k")).unwrap();
        began.recv().unwrap();
        assert!(input
            .send_if(item(Kind::Nudge, "no"), || false)
            .unwrap()
            .is_none());
        assert!(input
            .send_if(item(Kind::Nudge, "you have mail"), || true)
            .unwrap()
            .is_some());
        input.send(item(Kind::Text, "owner")).unwrap();
        assert_eq!(input.withdraw(Kind::Nudge), 1);
        term.open();
        settle(&input);
        assert_eq!(term.text(), "kowner");
    }
}
