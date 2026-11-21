//! A list a pane's program draws to choose from, on the station
//! (DECISIONS.md, "No prompt on the terminal is invisible in Chat", 17 Nov
//! 2026; the reader is `spandeck_agents::picker`).
//!
//! Twice a second each agent pane's screen is read. A list that stands
//! (read the same at two looks) is put on the pane's record (`prompt`, in
//! the pane list and in `live`) and the pane needs the owner, saying what
//! it shows; when it is gone the pane is what it was. Two kinds:
//!
//! - `picker`: its structure is whole and it grants nothing. The deck
//!   draws its rows, and a tap is `pane.pick`.
//! - `screen`: it grants something (trust, a permission, a sign-in), or
//!   nothing on it is marked. The deck shows the screen itself with keys,
//!   and the owner decides in full view. One that grants and is read
//!   whole (`exact`, one row marked) is shown with the program's own rows
//!   AND its choices (21 Nov 2026, the owner's request), and a choice is
//!   `pane.pick` too: only the row the owner tapped, only while the
//!   question is word for word the one the owner was shown.
//!
//! **A choice is made as the folder-trust answer is** (`pane.pick`, the
//! owner's only): the screen is read again and must still show that row
//! with those words; the arrow keys that take the program's marker there
//! are sent; the screen is read again, and the confirming key is pressed
//! only when the marker is on the row with the same words. A list that
//! changed, or a marker that is not there, stops it, and the answer says
//! so. Keys, never pasted text; and no agent can call it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use spandeck_agents::picker::{read, showing, waiting, Class, Picker};
use spandeck_agents::AgentState;
use spandeck_terminal::input::Item;

use crate::daemon::{invalid, Daemon, PaneEntry, Result};
use crate::modelswitch::Prompt;
use crate::paths::now_ms;

/// The words Claude Code's `/model` list has for its key that chooses for
/// the session alone (captured from 2.1.326 on 17 Nov 2026).
pub(crate) const SESSION_KEY_WORDS: &str = "s to use this session only";

#[derive(Default)]
struct Watch {
    /// What was read at the last look, by pane.
    last: HashMap<String, Picker>,
    /// The state a pane was in before it was said to need the owner.
    before: HashMap<String, AgentState>,
    /// A pane whose program has shown its ordinary wait for a message
    /// since it was started (by when it was started): only then is the
    /// absence of that wait something it shows.
    armed: HashMap<String, u64>,
    /// When the owner last sent the pane something.
    typed: HashMap<String, u64>,
    /// A screen the owner said is no prompt ("Not a prompt: hide"): it is
    /// not said again while the screen is exactly that.
    hidden: HashMap<String, Picker>,
}

fn watch() -> &'static Mutex<Watch> {
    static WATCH: OnceLock<Mutex<Watch>> = OnceLock::new();
    WATCH.get_or_init(Default::default)
}

fn ours(reason: Option<&str>) -> bool {
    reason.is_some_and(|r| {
        r.starts_with("shows a list to choose from")
            || r.starts_with("shows something in its terminal")
    })
}

/// Said in `caps`: `pane.pick` chooses a row of a screen that grants
/// something, when it is read whole and the owner's deck says which
/// question it showed (the owner's report of 21 Nov 2026). A deck offers
/// the choices of such a screen only on a station that says so: one from
/// before refuses, and its deck shows the screen and the keys. It is also
/// a switch of the station's (`station.flags`, on unless set to false).
pub const GRANT_CAP: &str = "grant_pick";

/// A screen that grants something and is read whole: its choices in one
/// column, exactly one of them marked.
fn grants_whole(p: &Picker) -> bool {
    p.exact
        && p.class == Class::Grants
        && p.choices.iter().filter(|c| c.marked).count() == 1
        && !p.multi
}

/// The same question: its words, and every choice with its words, its
/// number and the words under it, in the same order.
fn same_question(a: &Picker, b: &Picker) -> bool {
    let rows = |p: &Picker| {
        p.choices
            .iter()
            .map(|c| (c.text.clone(), c.detail.clone(), c.number))
            .collect::<Vec<_>>()
    };
    a.title == b.title && a.about == b.about && rows(a) == rows(b)
}

/// The deck's rows are offered only for a list whose structure is whole
/// and that grants nothing.
fn kind_of(p: &Picker) -> &'static str {
    if p.exact && p.class == Class::Plain && !p.choices.is_empty() {
        "picker"
    } else {
        "screen"
    }
}

fn short(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).take(80).collect();
    s.trim().to_string()
}

impl Daemon {
    fn picker_key(&self, pane: &str) -> String {
        format!("{}|{pane}", self.home.display())
    }

    /// The owner sent the pane something, now.
    pub(crate) fn picker_owner_typed(&self, pane: &str) {
        watch()
            .lock()
            .unwrap()
            .typed
            .insert(self.picker_key(pane), now_ms());
    }

    /// What a pane's program shows now: a list read from its screen, or
    /// (the net under it, which needs nothing to be recognised) the
    /// screen itself when the program is not in a turn and what is at the
    /// bottom is known not to be its ordinary wait for a message
    /// (`spandeck_agents::picker::showing`). The second has no rows: the
    /// last rows of the screen are kept with it, so it stands only while
    /// the screen is still.
    fn on_screen(
        &self,
        p: &PaneEntry,
        state: AgentState,
        agent: spandeck_agents::AgentKind,
    ) -> Option<Picker> {
        let lines = p.term.lines();
        // Key names with no rows to choose from, on the screen of a
        // program at work that is still drawing: it is at work, whatever
        // its rows name. One that has stopped to ask stands still. (The
        // real Codex, 18 Nov 2026: a row of its own naming Esc and Enter
        // made the pane need the owner anew each second of its turn.)
        // (Three seconds: a program at work redraws its timer or its
        // spinner every second or oftener, and a screen it has opened in
        // its turn, as Claude Code's own /permissions, is noticed soon.)
        let drawing = state == AgentState::Working
            && now_ms().saturating_sub(p.term.last_output_ms()) < 3_000;
        read(&lines)
            .filter(|pk| !pk.choices.is_empty() || !drawing)
            .or_else(|| {
                // Not in a turn, and known not to be its wait for a message:
                // by what is known of its screens, or (Claude Code, Codex)
                // because it has shown that wait since it started and does
                // not now. In a turn: only a screen that has stood still for
                // ten seconds, since a program at work keeps drawing (its
                // timer, its spinner) and one that has stopped to ask does not.
                let started = p.meta().proc_started_ms.unwrap_or(0);
                let key = self.picker_key(&p.id);
                let known = match waiting(agent, &lines) {
                    Some(true) => {
                        watch().lock().unwrap().armed.insert(key, started);
                        false
                    }
                    Some(false) => watch().lock().unwrap().armed.get(&key) == Some(&started),
                    None => false,
                };
                let still = now_ms().saturating_sub(p.term.last_output_ms()) >= 10_000;
                // (Found with the real Antigravity 1.5.10: for some eight
                // seconds after a message was sent to it, it was beginning its
                // turn, with rows under its box and its state not yet "working",
                // and the pane read as showing something. Nothing unread is
                // said of an Antigravity pane in the twenty seconds after the owner
                // sent it something, nor of any pane whose screen is still being
                // drawn.)
                let sent = watch()
                    .lock()
                    .unwrap()
                    .typed
                    .get(&self.picker_key(&p.id))
                    .copied()
                    .unwrap_or(0);
                let settled = now_ms().saturating_sub(p.term.last_output_ms()) >= 1_500;
                let _ = sent;
                let other = match state {
                    AgentState::Working => still && known,
                    _ => settled && (known || showing(agent, &lines)),
                };
                other.then(|| {
                    let mut rows: Vec<String> = lines
                        .iter()
                        .map(|l| l.trim_end().chars().take(160).collect::<String>())
                        .filter(|l| !l.trim().is_empty())
                        .collect();
                    if rows.len() > 16 {
                        rows.drain(..rows.len() - 16);
                    }
                    Picker {
                        title: String::new(),
                        about: String::new(),
                        tabs: vec![],
                        choices: vec![],
                        multi: false,
                        more_above: false,
                        more_below: false,
                        footer: rows,
                        hint: String::new(),
                        free_text: None,
                        class: Class::Plain,
                        exact: false,
                    }
                })
            })
    }

    /// One look at every agent pane's screen. `skip`: panes whose screen
    /// another reader has already answered for (the folder-trust
    /// question, the review of hooks, "Switch model?"). The panes it
    /// speaks for.
    pub(crate) fn picker_look(&self, skip: &std::collections::HashSet<String>) -> Vec<String> {
        let mut showing = vec![];
        for p in self.list_panes() {
            let m = p.meta();
            if m.exit.is_some() || !m.agent.is_agent() {
                continue;
            }
            let key = self.picker_key(&p.id);
            let had = m
                .prompt
                .as_ref()
                .is_some_and(|x| x.kind == "picker" || x.kind == "screen");
            let seen = if skip.contains(&p.id) {
                None
            } else {
                self.on_screen(&p, m.state_before_prompt(), m.agent)
            };
            // (One the owner hid stays hidden while it is exactly that.)
            let seen = {
                let mut w = watch().lock().unwrap();
                match (&seen, w.hidden.get(&key)) {
                    (Some(now), Some(hid)) if now == hid => None,
                    (_, Some(_)) => {
                        w.hidden.remove(&key);
                        seen
                    }
                    _ => seen,
                }
            };
            let Some(seen) = seen else {
                let mut w = watch().lock().unwrap();
                w.last.remove(&key);
                let before = w.before.remove(&key);
                drop(w);
                if had {
                    p.meta.lock().unwrap().prompt = None;
                    if ours(m.needs_reason.as_deref()) && m.state == AgentState::NeedsYou {
                        self.set_state(&p, before.unwrap_or(AgentState::Idle), None);
                    } else {
                        self.emit("pane.updated", Some(&p.id), m.project.as_deref(), json!({}));
                    }
                }
                continue;
            };
            // It stands when it is read the same at two looks: a screen
            // still being drawn is not said.
            let stood = watch()
                .lock()
                .unwrap()
                .last
                .insert(key.clone(), seen.clone())
                .as_ref()
                == Some(&seen);
            if !stood && !had {
                continue;
            }
            showing.push(p.id.clone());
            if !stood {
                continue;
            }
            let kind = kind_of(&seen);
            let title = short(if seen.title.is_empty() {
                "its own screen"
            } else {
                &seen.title
            });
            let why = match kind {
                "picker" => format!("shows a list to choose from: {title}"),
                _ => format!("shows something in its terminal: {title}"),
            };
            let prompt = Prompt {
                kind: kind.to_string(),
                name: title,
                on_yes: false,
                since_ms: m
                    .prompt
                    .as_ref()
                    .filter(|_| had)
                    .map_or_else(now_ms, |x| x.since_ms),
                session_key: seen.hint.contains(SESSION_KEY_WORDS),
                picker: Some(seen),
            };
            if m.prompt.as_ref() == Some(&prompt) && m.needs_reason.as_deref() == Some(why.as_str())
            {
                continue;
            }
            if !had {
                watch().lock().unwrap().before.insert(key, m.state);
            }
            p.meta.lock().unwrap().prompt = Some(prompt);
            if !self.set_state(&p, AgentState::NeedsYou, Some(why)) {
                self.emit("pane.updated", Some(&p.id), m.project.as_deref(), json!({}));
            }
        }
        showing
    }

    /// Take the program's marker to the row with these words and press
    /// `key` there, or stop. `Ok(None)`: done. `Ok(Some(why))`: stopped,
    /// and nothing was confirmed.
    ///
    /// `granting`: `None` for a plain list, and a screen that grants is
    /// then no list at all. `Some(shown)` only from `pane.pick`, for the
    /// owner's own tap on a screen that grants: the screen must be, word
    /// for word, the question `shown` (what the station said the pane
    /// shows, and the deck said it showed the owner), before the marker
    /// is moved and again before the key is pressed.
    pub(crate) async fn picker_choose(
        &self,
        p: &PaneEntry,
        text: &str,
        detail: Option<&str>,
        key: &[u8],
        granting: Option<&Picker>,
    ) -> Result<Option<String>> {
        let look = || {
            read(&p.term.lines()).filter(|x| match granting {
                None => x.exact && x.class == Class::Plain,
                Some(shown) => grants_whole(x) && same_question(x, shown),
            })
        };
        let Some(now) = look() else {
            return Ok(Some(match granting {
                None => "no list to choose from is on its screen now".into(),
                Some(_) => "its screen is not the question you were shown any more: nothing was chosen. Look again".into(),
            }));
        };
        let at = |x: &Picker| x.choices.iter().position(|c| c.marked);
        let rows: Vec<usize> = now
            .choices
            .iter()
            .enumerate()
            .filter(|(_, c)| c.text == text && detail.is_none_or(|d| c.detail == d))
            .map(|(i, _)| i)
            .collect();
        let (Some(from), [to]) = (at(&now), rows.as_slice()) else {
            return Ok(Some("the list changed: look again".into()));
        };
        let under = now.choices[*to].detail.clone();
        let arrow: &[u8] = if *to > from { b"\x1b[B" } else { b"\x1b[A" };
        for _ in 0..to.abs_diff(from) {
            p.term.send(Item::keys(arrow))?;
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        // Read again: the marker is on the row with the same words, and
        // the list is the one that was chosen from.
        let mut there = false;
        for _ in 0..15 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(x) = look() {
                // (A screen that grants is already held to the whole
                // question by `look`; its row is the same to the words
                // under it.)
                let same = x.title == now.title && x.choices.len() == now.choices.len();
                there = same
                    && at(&x).is_some_and(|i| {
                        x.choices[i].text == text
                            && (granting.is_none() || x.choices[i].detail == under)
                    });
                if there {
                    break;
                }
            }
        }
        if !there {
            return Ok(Some("the list changed, or its marker did not reach that row: nothing was confirmed. Look again".into()));
        }
        p.term.send(Item::keys(key))?;
        Ok(None)
    }

    /// `pane.pick {pane, text, key?}`: the owner chooses a row of the list
    /// on a pane's screen. `key`: `enter` (the default), `space` (tick it,
    /// where several may be ticked), or `session` (Claude Code's `/model`:
    /// "s to use this session only", only when its list says so).
    ///
    /// A screen that grants something (21 Nov 2026): only when the station
    /// has said the pane shows it (`prompt`: `screen`, read whole, one row
    /// marked: never the folder-trust question, the review of hooks or
    /// "Switch model?", which have their own cards), only with `title` and
    /// `about` as the deck showed them, and only Enter on the row tapped.
    /// This is the one place a granting row is chosen from: the owner's
    /// call (`rpc.rs`), and nothing else passes `granting`.
    pub async fn pane_pick(&self, pane: &str, params: &Value, by: &str) -> Result<Value> {
        let p = self.pane(pane)?;
        // "Not a prompt: hide": what is shown now is not said again while
        // the screen is exactly this.
        if params.get("hide").and_then(|h| h.as_bool()) == Some(true) {
            let m = p.meta();
            if let Some(now) = self.on_screen(&p, m.state_before_prompt(), m.agent) {
                watch()
                    .lock()
                    .unwrap()
                    .hidden
                    .insert(self.picker_key(&p.id), now);
            }
            return Ok(json!({"ok": true, "hidden": true}));
        }
        let text = params
            .get("text")
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty() && t.len() <= 400)
            .ok_or_else(|| invalid("missing parameter: text"))?;
        let shown = read(&p.term.lines());
        let detail = params.get("detail").and_then(|t| t.as_str());
        let said = p
            .meta()
            .prompt
            .filter(|x| x.kind == "screen")
            .and_then(|x| x.picker)
            .filter(grants_whole)
            // (The owner's switch at this station, `grant_pick`: off, a
            // screen that grants is answered with its own keys only.)
            .filter(|_| self.flag_on(GRANT_CAP, true));
        let granting = match (&shown, said) {
            (Some(now), Some(said)) if now.class == Class::Grants => {
                let word = |k: &str| params.get(k).and_then(|t| t.as_str());
                if word("title") != Some(said.title.as_str())
                    || word("about").unwrap_or("") != said.about
                {
                    return Ok(json!({"ok": false, "picked": false,
                        "why": "its screen is not the question you were shown: nothing was chosen. Look again"}));
                }
                if params
                    .get("key")
                    .and_then(|k| k.as_str())
                    .unwrap_or("enter")
                    != "enter"
                {
                    return Err(invalid(
                        "a choice that grants something is made with Enter on its row only",
                    ));
                }
                Some(said)
            }
            _ => None,
        };
        let key: &[u8] = match params
            .get("key")
            .and_then(|k| k.as_str())
            .unwrap_or("enter")
        {
            "enter" => b"\r",
            "space" if shown.as_ref().is_some_and(|x| x.multi) => b" ",
            "session"
                if shown
                    .as_ref()
                    .is_some_and(|x| x.hint.contains(SESSION_KEY_WORDS)) =>
            {
                b"s"
            }
            other => return Err(invalid(format!("this list has no key \"{other}\""))),
        };
        self.start_screen_owner_typed(&p.id);
        let stopped = self
            .picker_choose(&p, text, detail, key, granting.as_ref())
            .await?;
        let m = p.meta();
        self.record(
            "pane.pick",
            Some(&p.id),
            m.project.as_deref(),
            None,
            Some(by),
            json!({"list": shown.map(|x| short(&x.title)), "confirmed": stopped.is_none(),
                "grants": granting.is_some(),
                "row": granting.is_some().then(|| short(text))}),
        );
        Ok(match stopped {
            None => json!({"ok": true, "picked": true}),
            Some(why) => json!({"ok": false, "picked": false, "why": why}),
        })
    }

    /// Choose a model for the session alone through Claude Code's own
    /// `/model` list: its command is typed, its list is read, the marker
    /// is taken to the model's row, and `s` is pressed ("s to use this
    /// session only"). Nothing of the owner's defaults is written, and
    /// nothing is started again.
    pub(crate) async fn model_for_session(
        &self,
        p: &PaneEntry,
        command: &str,
        model: &str,
        by: &str,
    ) -> Result<Value> {
        let done = self
            .agents_command(command, Some(&p.id), None, false, false, by)
            .await?;
        if let Some(why) = done["stations"][0]["skipped"][0]["why"].as_str() {
            return Ok(
                json!({"ok": false, "sent": false, "command": command, "why": why, "busy": why == "in the middle of a turn"}),
            );
        }
        // Its list, once it stands, and only the one that says its key.
        let mut list = None;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            list = read(&p.term.lines()).filter(|x| x.exact && x.hint.contains(SESSION_KEY_WORDS));
            if list.is_some() {
                break;
            }
        }
        let Some(list) = list else {
            return Ok(json!({"ok": false, "sent": true, "command": command,
                "why": "its list of models did not appear as expected: look at its terminal"}));
        };
        // The row for the model: its words are the name, or begin with it
        // ("opus" is "Opus 5.5"; "default" is "Default (recommended)").
        let want = model.to_lowercase();
        let rows: Vec<&str> = list
            .choices
            .iter()
            .filter(|c| !c.submenu)
            .map(|c| c.text.as_str())
            .filter(|t| {
                let t = t.to_lowercase();
                t == want || t.split([' ', '(']).next() == Some(want.as_str())
            })
            .collect();
        let [row] = rows.as_slice() else {
            let _ = p.term.send(Item::keys(b"\x1b"));
            return Ok(
                json!({"ok": false, "sent": true, "closed": true, "command": command,
                "why": format!("\"{model}\" is not one row of its list (it may be under More models): the list was closed and nothing was changed")}),
            );
        };
        match self.picker_choose(p, row, None, b"s", None).await? {
            None => Ok(
                json!({"ok": true, "sent": true, "session_only": true, "picked": row, "command": command}),
            ),
            Some(why) => Ok(json!({"ok": false, "sent": true, "command": command, "why": why})),
        }
    }
}
