//! The screen as a conversation (DECISIONS.md, "Chat for every pane";
//! `docs/conversation-api.md`, "From the screen").
//!
//! A pane with no transcript to read (a shell, an agent whose sessions
//! spandeck cannot read, an agent whose session is not found yet) still has
//! a Chat face: what was sent to it, and what it printed after each.
//!
//! - What was sent is known here: the message box, the paste dialog and
//!   `pane.send_text` all come through `rpc.rs`, which tells `sent`.
//! - What it printed is the terminal's own text, cut where each message
//!   was sent: the screen model tags that row (`Screen::mark`), and the
//!   block is frozen as text when the next message is sent. So an old block
//!   never changes, whatever the terminal does afterwards.
//! - While the program draws its screen itself (a full-screen program, or
//!   one that redrew over the tagged row), there is no log to cut: the
//!   block is the screen as it is, and says so (`whole`).
//!
//! - What an *agent* program printed is also given as parts to read
//!   (`parts`: reflow.rs): prose, drawn like any reply, and rows whose
//!   layout is their meaning. A shell's block is its rows, as they were.
//! - When the station sees an agent's turn end (its state goes from
//!   working to idle or done), the block is frozen there and a "turn
//!   ended" notice follows it, as in a conversation read from a file.
//!
//! Kept in memory, for each pane: it begins again when the station does.
//! Served only through `pane.conversation`, so to the owner alone.

use std::collections::VecDeque;

use serde_json::{json, Value};
use spandeck_archive::conversation::{cap, clean, paged, PAGE_DEFAULT, PAGE_MAX, TEXT_CAP};

use crate::daemon::{invalid, Daemon, PaneEntry, PaneMeta, Result};
use crate::paths::now_ms;
use crate::reflow::{reflow, Part};

/// Items kept for a pane; past this the oldest go.
const ITEMS_MAX: usize = 400;
/// Bytes of text kept for a pane in all, and for one block.
const BYTES_MAX: usize = 2 << 20;
const BLOCK_MAX: usize = 256 * 1024;
/// Lines of the terminal read for one block when it is frozen or asked for
/// in full, and for the block that is still being printed.
const FROZEN_LINES: usize = 4000;
const OPEN_LINES: usize = 400;

#[derive(Debug, Clone)]
enum What {
    /// Something sent to the pane. `origin`: `you`, or `agent` with `from`.
    Sent {
        origin: &'static str,
        from: Option<String>,
    },
    /// What the pane printed. `lines`: how many there were.
    Printed { whole: bool, lines: usize },
    /// The agent's turn ended here, after so long.
    Ended { duration_ms: u64 },
}

#[derive(Debug, Clone)]
struct Kept {
    line: u64,
    /// The revisions at which it was first served and last changed.
    made: u64,
    changed: u64,
    ts_ms: u64,
    what: What,
    text: String,
    /// What an agent program printed, as parts to read. `None` for a
    /// shell, whose rows are shown as they were.
    parts: Option<Vec<Part>>,
}

impl Kept {
    /// A block that came to nothing: not shown.
    fn blank(&self) -> bool {
        self.text.is_empty() && !matches!(self.what, What::Ended { .. })
    }
}

/// What an agent program printed, as parts to read (reflow.rs).
fn shape(printed: &spandeck_terminal::Printed, agent: bool) -> Option<Vec<Part>> {
    agent.then(|| {
        reflow(&printed.lines, &printed.joined, printed.cursor)
            .into_iter()
            .map(|p| Part {
                prose: p.prose,
                text: clean(&p.text),
            })
            .collect()
    })
}

/// One pane's sent messages and printed blocks.
#[derive(Debug, Default)]
pub struct Talk {
    items: VecDeque<Kept>,
    /// The line the block being printed now has (and keeps when frozen).
    next: u64,
    /// Counts every change: what a caller holds as its cursor.
    rev: u64,
    bytes: usize,
    dropped: bool,
    /// The block being printed, as last read, and the pane's output count
    /// then: not read again while nothing more was printed.
    open: Option<Kept>,
    open_at: Option<u64>,
}

/// The end of `text`, from a line's beginning, in at most `max` bytes.
fn tail(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut at = text.len() - max;
    while !text.is_char_boundary(at) {
        at += 1;
    }
    let at = text[at..].find('\n').map_or(at, |n| at + n + 1);
    (&text[at..], true)
}

impl Talk {
    fn bump(&mut self) -> u64 {
        self.rev += 1;
        self.rev
    }

    fn push(&mut self, mut k: Kept) {
        k.line = self.next;
        self.next += 1;
        self.bytes += k.text.len();
        self.items.push_back(k);
        while self.items.len() > ITEMS_MAX || self.bytes > BYTES_MAX {
            let Some(old) = self.items.pop_front() else {
                break;
            };
            self.bytes -= old.text.len();
            self.dropped = true;
        }
    }

    /// The screen drawn whole that was frozen last is what `block` (and,
    /// for an agent, `parts`: so that a status row that ticks is no new
    /// screen) says again.
    fn said_last(&self, block: &str, parts: &Option<Vec<Part>>) -> bool {
        self.items
            .iter()
            .rev()
            .find(|k| matches!(k.what, What::Printed { whole: true, .. }))
            .is_some_and(|k| match parts {
                Some(_) => k.parts == *parts,
                None => k.text == block,
            })
    }

    /// What the pane had printed until now is frozen as a block.
    fn freeze(&mut self, printed: spandeck_terminal::Printed, agent: bool) {
        let now = now_ms();
        let parts = shape(&printed, agent);
        let block = clean(&printed.lines.join("\n"));
        let (block, _) = tail(&block, BLOCK_MAX);
        // A screen that was drawn whole and has not changed since it was
        // last frozen is not said twice.
        let same = printed.whole && self.said_last(block, &parts);
        let open = self.open.take();
        self.open_at = None;
        if !block.is_empty() && !same {
            let rev = self.bump();
            self.push(Kept {
                line: 0,
                made: open.as_ref().map_or(rev, |o| o.made),
                changed: rev,
                ts_ms: open.as_ref().map_or(now, |o| o.ts_ms),
                what: What::Printed {
                    whole: printed.whole,
                    lines: printed.total,
                },
                text: block.to_string(),
                parts,
            });
        } else if let Some(open) = open {
            // It was served while it was being printed, and nothing of it
            // is this block's after all (only the prompt the message was
            // typed at, or a screen already frozen): said once more, empty.
            let rev = self.bump();
            self.push(Kept {
                line: 0,
                made: open.made,
                changed: rev,
                ts_ms: open.ts_ms,
                what: What::Printed {
                    whole: printed.whole,
                    lines: 0,
                },
                text: String::new(),
                parts: None,
            });
        } else {
            self.next += 1;
        }
    }

    /// Something was sent to the pane: what it had printed until then is
    /// frozen as a block, and the message follows it.
    fn sent(
        &mut self,
        printed: spandeck_terminal::Printed,
        agent: bool,
        text: &str,
        origin: &'static str,
        from: Option<String>,
    ) {
        self.freeze(printed, agent);
        let rev = self.bump();
        let (text, _) = cap(&clean(text), BLOCK_MAX);
        self.push(Kept {
            line: 0,
            made: rev,
            changed: rev,
            ts_ms: now_ms(),
            what: What::Sent { origin, from },
            text,
            parts: None,
        });
    }

    /// The agent's turn ended: what it printed in it is frozen as a block,
    /// and a notice follows it.
    fn ended(&mut self, printed: spandeck_terminal::Printed, duration_ms: u64) {
        self.freeze(printed, true);
        let rev = self.bump();
        self.push(Kept {
            line: 0,
            made: rev,
            changed: rev,
            ts_ms: now_ms(),
            what: What::Ended { duration_ms },
            text: String::new(),
            parts: None,
        });
    }

    /// The block being printed now, as the terminal has it.
    fn look(
        &mut self,
        bytes_out: u64,
        agent: bool,
        read: impl FnOnce() -> spandeck_terminal::Printed,
    ) {
        if self.open_at == Some(bytes_out) {
            return;
        }
        self.open_at = Some(bytes_out);
        let printed = read();
        let parts = shape(&printed, agent);
        let text = clean(&printed.lines.join("\n"));
        // The screen that was frozen last, still as it was: not said again.
        if self.open.is_none() && printed.whole && self.said_last(&text, &parts) {
            return;
        }
        let what = What::Printed {
            whole: printed.whole,
            lines: printed.total,
        };
        let same = self.open.as_ref().map(|o| o.text == text);
        match same {
            Some(true) => {}
            None if text.is_empty() => {}
            Some(false) => {
                let rev = self.bump();
                let o = self.open.as_mut().unwrap();
                o.text = text;
                o.parts = parts;
                o.what = what;
                o.changed = rev;
            }
            None => {
                let rev = self.bump();
                self.open = Some(Kept {
                    line: self.next,
                    made: rev,
                    changed: rev,
                    ts_ms: now_ms(),
                    what,
                    text,
                    parts,
                });
            }
        }
    }

    fn all(&self) -> impl DoubleEndedIterator<Item = &Kept> {
        self.items
            .iter()
            .chain(self.open.as_ref().filter(|o| !o.text.is_empty()))
    }

    /// What a page shows: not a block that came to nothing.
    fn shown(&self) -> impl DoubleEndedIterator<Item = &Kept> {
        self.all().filter(|k| !k.blank())
    }
}

/// An item as it is served: the text cut to its last 16 KB.
fn served(k: &Kept, open: bool) -> Value {
    let mut item = json!({"id": format!("{}.0", k.line), "line": k.line, "ts_ms": k.ts_ms});
    match &k.what {
        What::Sent { origin, from } => {
            let (text, more) = cap(&k.text, TEXT_CAP);
            item["kind"] = json!("user");
            item["text"] = json!(text);
            item["origin"] = json!(origin);
            if let Some(from) = from {
                item["from"] = json!(from);
            }
            if more {
                item["more"] = json!(true);
            }
        }
        What::Printed { whole, lines } => {
            let (text, cut) = tail(&k.text, TEXT_CAP);
            item["kind"] = json!("screen");
            item["text"] = json!(text);
            item["lines"] = json!(lines);
            if *whole {
                item["whole"] = json!(true);
            }
            if open {
                item["open"] = json!(true);
            }
            if cut || *lines > k.text.lines().count() {
                item["more"] = json!(true);
            }
            // The same end of it, as parts to read.
            if let Some(parts) = &k.parts {
                let mut left = TEXT_CAP;
                let mut out: Vec<Value> = vec![];
                for p in parts.iter().rev() {
                    if left == 0 {
                        break;
                    }
                    let (text, _) = tail(&p.text, left);
                    left -= text.len();
                    out.push(json!({"prose": p.prose, "text": text}));
                }
                out.reverse();
                item["parts"] = json!(out);
            }
        }
        What::Ended { duration_ms } => {
            item["kind"] = json!("notice");
            item["what"] = json!("turn_ended");
            item["duration_ms"] = json!(duration_ms);
        }
    }
    item
}

impl Daemon {
    /// Something is about to be sent to a pane as a message (the message
    /// box, a paste, `pane.send_text`): its place on the screen is marked.
    /// Called before the item is handed to the pane's input.
    pub(crate) fn screen_sent(&self, pane: &str, text: &str, by_owner: bool, who: &str) {
        let Ok(p) = self.pane(pane) else { return };
        if text.trim().is_empty() {
            return;
        }
        let printed = p.term.printed_and_mark(FROZEN_LINES);
        let agent = p.meta.lock().unwrap().agent.is_agent();
        let (origin, from) = if by_owner {
            ("you", None)
        } else {
            // An agent's pane id: said by its name, when it is here.
            let name = self
                .pane(who)
                .map(|p| p.meta.lock().unwrap().name.clone())
                .unwrap_or_else(|_| who.to_string());
            ("agent", Some(name))
        };
        p.talk
            .lock()
            .unwrap()
            .sent(printed, agent, text, origin, from);
    }

    /// The station saw an agent's turn end (its state went from working to
    /// idle or done) after `duration_ms`: said in the screen's
    /// conversation, where no transcript says it.
    pub(crate) fn screen_turn_ended(&self, p: &PaneEntry, duration_ms: u64) {
        let printed = p.term.printed_and_mark(FROZEN_LINES);
        p.talk.lock().unwrap().ended(printed, duration_ms);
    }

    /// The first message a pane's program was started with.
    pub(crate) fn screen_started_with(&self, p: &PaneEntry, prompt: &str) {
        if prompt.trim().is_empty() {
            return;
        }
        let mut talk = p.talk.lock().unwrap();
        talk.next += 1;
        let rev = talk.bump();
        let (text, _) = cap(&clean(prompt), BLOCK_MAX);
        talk.push(Kept {
            line: 0,
            made: rev,
            changed: rev,
            ts_ms: now_ms(),
            what: What::Sent {
                origin: "you",
                from: None,
            },
            text,
            parts: None,
        });
    }

    fn screen_session(meta: &PaneMeta) -> String {
        format!("screen:{}", meta.proc_started_ms.unwrap_or(meta.created_ms))
    }

    /// `pane.conversation` for a pane with no transcript, when the caller
    /// asked for the screen (`screen: true`): the answer has the same shape
    /// and `source: "screen"`. `out` has the pane's fields already; `why`
    /// is why there is no transcript.
    pub(crate) fn screen_conversation(
        &self,
        p: &PaneEntry,
        meta: &PaneMeta,
        params: &Value,
        why: &str,
        mut out: Value,
    ) -> Result<Value> {
        let num = |key: &str| params.get(key).and_then(|v| v.as_u64());
        let limit = num("limit")
            .map_or(PAGE_DEFAULT, |l| l as usize)
            .clamp(1, PAGE_MAX);
        let session = Self::screen_session(meta);
        let reset = params
            .get("session")
            .and_then(|s| s.as_str())
            .is_some_and(|s| s != session);
        if !reset && num("after").is_some() && num("before").is_some() {
            return Err(invalid("give after or before, not both"));
        }
        let bytes_out = p.term.bytes_out();
        let mut talk = p.talk.lock().unwrap();
        // The pane's own lock is taken inside the talk's, and nowhere the
        // other way round (`screen_sent` reads the screen first).
        talk.look(bytes_out, meta.agent.is_agent(), || {
            p.term.printed(OPEN_LINES)
        });
        let open_line = talk.open.as_ref().map(|o| o.line);
        let serve = |k: &Kept| served(k, Some(k.line) == open_line);
        let (mut items, mut updated): (Vec<Value>, Vec<Value>) = (vec![], vec![]);
        let mut more_after = false;
        match (
            num("after").filter(|_| !reset),
            num("before").filter(|_| !reset),
        ) {
            (Some(rev), _) => {
                for k in talk.all() {
                    if k.made > rev && k.blank() {
                        continue;
                    }
                    if k.made > rev {
                        if items.len() < limit {
                            items.push(serve(k));
                        } else {
                            more_after = true;
                        }
                    } else if k.changed > rev {
                        updated.push(serve(k));
                    }
                }
            }
            (None, before) => {
                let upto = before.unwrap_or(u64::MAX);
                let mut page: Vec<&Kept> = talk
                    .shown()
                    .rev()
                    .filter(|k| k.line < upto)
                    .take(limit)
                    .collect();
                page.reverse();
                items = page.into_iter().map(serve).collect();
            }
        }
        let first = items.first().and_then(|i| i["line"].as_u64());
        let more_before = first.is_some_and(|f| talk.shown().next().is_some_and(|k| k.line < f));
        // A page that was cut is asked for again from where it ended.
        let cursor = if more_after {
            talk.all()
                .filter(|k| items.iter().any(|i| i["line"].as_u64() == Some(k.line)))
                .map(|k| k.made)
                .max()
                .unwrap_or(talk.rev)
        } else {
            talk.rev
        };
        let dropped = talk.dropped;
        drop(talk);
        out["available"] = json!(true);
        out["source"] = json!("screen");
        out["reason"] = json!(format!("no conversation for this pane: {why}"));
        out["session"] = json!(session);
        out["reset"] = json!(reset);
        out["title"] = Value::Null;
        out["items"] = json!(items);
        out["updated"] = json!(updated);
        out["cursor"] = json!(cursor);
        out["first"] = json!(first);
        out["more_before"] = json!(more_before);
        out["more_after"] = json!(more_after);
        out["not_understood"] = json!(0);
        out["not_understood_kinds"] = json!([]);
        out["older_dropped"] = json!(dropped);
        out["live"] = self.conversation_live(p, None);
        Ok(out)
    }

    /// `pane.conversation.detail` for an item of the screen's conversation:
    /// its text in full, a page at a time. The block being printed is read
    /// from the terminal again, further back.
    pub(crate) fn screen_detail(
        &self,
        p: &PaneEntry,
        meta: &PaneMeta,
        params: &Value,
    ) -> Result<Value> {
        let item = params
            .get("item")
            .and_then(|i| i.as_str())
            .ok_or_else(|| invalid("missing parameter: item"))?;
        if params.get("session").and_then(|s| s.as_str()) != Some(&Self::screen_session(meta)) {
            return Err(invalid(
                "that item is from another session of this pane: ask for the conversation again",
            ));
        }
        let offset = params.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) as usize;
        let talk = p.talk.lock().unwrap();
        let open = talk
            .open
            .as_ref()
            .filter(|o| format!("{}.0", o.line) == item)
            .cloned();
        let kept = talk
            .items
            .iter()
            .find(|k| format!("{}.0", k.line) == item)
            .cloned();
        drop(talk);
        let (k, text) = match (kept, open) {
            (Some(k), _) => {
                let text = k.text.clone();
                (k, text)
            }
            (None, Some(o)) => {
                let text = clean(&p.term.printed(FROZEN_LINES).lines.join("\n"));
                (o, text)
            }
            _ => return Err(crate::daemon::Error::NotFound(format!("item {item}"))),
        };
        Ok(json!({
            "pane": p.id, "session": Self::screen_session(meta),
            "item": served(&k, false), "text": paged(&text, offset),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spandeck_terminal::Printed;

    fn printed(lines: &[&str], whole: bool) -> Printed {
        Printed {
            lines: lines.iter().map(|l| l.to_string()).collect(),
            total: lines.len(),
            whole,
            ..Default::default()
        }
    }

    /// The owner's report of 16 Nov 2026: Chat for a pane with no
    /// transcript. What a caller holds stays right as the pane prints: an
    /// item's id never changes, a block that grew comes as an update and
    /// not as a second item, and a caller that asks from its cursor is
    /// told nothing twice.
    #[test]
    fn a_block_keeps_its_id_from_first_line_to_frozen() {
        let mut t = Talk::default();
        let ids = |t: &Talk, after: u64| -> (Vec<u64>, Vec<u64>) {
            let made = t.all().filter(|k| k.made > after).map(|k| k.line).collect();
            let changed = t
                .all()
                .filter(|k| k.made <= after && k.changed > after)
                .map(|k| k.line)
                .collect();
            (made, changed)
        };
        t.look(10, false, || printed(&["$"], false));
        assert_eq!(ids(&t, 0), (vec![0], vec![]));
        let seen = t.rev;
        // Nothing more printed: not read again, and nothing to tell.
        t.look(10, false, || panic!("read again with nothing printed"));
        assert_eq!(ids(&t, seen), (vec![], vec![]));
        t.look(20, false, || printed(&["$ ls", "a.txt"], false));
        assert_eq!(ids(&t, seen), (vec![], vec![0]));
        let seen = t.rev;
        // A message: the block is frozen under its line, the message follows.
        t.sent(
            printed(&["$ ls", "a.txt", "b.txt"], false),
            false,
            "pwd",
            "you",
            None,
        );
        assert_eq!(ids(&t, seen), (vec![1], vec![0]));
        let lines: Vec<(u64, &str)> = t.all().map(|k| (k.line, k.text.as_str())).collect();
        assert_eq!(lines, vec![(0, "$ ls\na.txt\nb.txt"), (1, "pwd")]);
        let seen = t.rev;
        t.look(30, false, || printed(&["$ pwd", "/home/ada"], false));
        assert_eq!(ids(&t, seen), (vec![2], vec![]));
        // A screen drawn whole that has not changed is not frozen twice.
        t.sent(printed(&["menu"], true), false, "one", "you", None);
        t.sent(printed(&["menu"], true), false, "two", "you", None);
        let texts: Vec<&str> = t.all().map(|k| k.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["$ ls\na.txt\nb.txt", "pwd", "menu", "one", "two"]
        );
        // The end of a long block is what is served, from a line's start.
        let (end, cut) = tail("first line\nsecond line\nthird", 12);
        assert_eq!((end, cut), ("third", true));
    }

    /// The owner's report of 16 Nov 2026: Chat made from an agent's screen
    /// was a box of terminal text with no end to a turn. For an agent the
    /// block has parts to read, a turn's end is a notice after the block
    /// that was printed in it, and a screen whose status row alone has
    /// changed is not a new screen.
    #[test]
    fn an_agents_turn_ends_with_a_notice_and_its_screen_is_said_once() {
        let mut t = Talk::default();
        let screen = |status: &str| Printed {
            cursor: Some(2),
            ..printed(&["  Nine herons nest here.", "", "> ", status], true)
        };
        t.look(10, true, || screen("tern-2 · 12 s"));
        t.ended(screen("tern-2 · 14 s"), 14_000);
        let kinds: Vec<Value> = t.shown().map(|k| served(k, false)).collect();
        assert_eq!(kinds.len(), 2, "{kinds:?}");
        assert_eq!(
            kinds[0]["parts"],
            json!([{"prose": true, "text": "Nine herons nest here."}])
        );
        assert!(kinds[0]["text"].as_str().unwrap().contains("tern-2 · 14 s"));
        assert_eq!(
            (
                &kinds[1]["kind"],
                &kinds[1]["what"],
                &kinds[1]["duration_ms"]
            ),
            (&json!("notice"), &json!("turn_ended"), &json!(14_000))
        );
        // The clock in its status row moves on: nothing new to show.
        let seen = t.rev;
        t.look(20, true, || screen("tern-2 · idle 3 s"));
        assert_eq!(t.rev, seen);
        t.look(30, true, || Printed {
            cursor: Some(3),
            ..printed(
                &["  Nine herons nest here.", "  And two grebes.", "", "> "],
                true,
            )
        });
        assert_eq!(t.shown().count(), 3);
    }
}
