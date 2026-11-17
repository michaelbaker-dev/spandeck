//! A question the agent asks the owner, and a plan it wants approved,
//! answered from the Control Deck (DECISIONS.md, "The conversation view",
//! phase M3; the calls are in `docs/conversation-api.md`).
//!
//! Claude Code asks with two tools, `AskUserQuestion` and `ExitPlanMode`,
//! and shows a prompt in its terminal. Its hook reference ("Tools that
//! require user interaction", read 16 Nov 2026) says a hook that runs
//! before such a call can answer it: allow, with the call's whole input
//! given back and the answer in it. So the hook of the entry spandeck
//! installs for those two tools (agents/install.rs) asks here, and this
//! holds it for the owner, the way a permission request is held: a row in
//! the approvals' book, found again by the hook's id after a restart,
//! answered once (approvals.rs). Run by hand with Claude Code 2.1.325 on
//! 16 Nov 2026; that a hold of more than ten minutes is honoured was not
//! known then.
//!
//! What never happens: an answer nobody gave. When the owner does not
//! answer in time, lets it go ("Answer in the terminal instead"), or
//! spandeck is away, the hook says nothing, and the agent shows its own
//! prompt in the terminal, as it always did.
//!
//! Whose it is: the owner's. A question and a plan are what was said in a
//! conversation, so they are read through the conversation's calls only,
//! and answered by `question.answer`, which is on no agent's list, no
//! station's and not the read-only one.

use std::time::Duration;

use serde_json::{json, Map, Value};
use spandeck_agents::{adapter, AgentKind, AgentState, HookDecision, HookReply};
use spandeck_archive::conversation::{cap, clean, questions_of, DETAIL_CAP};

use crate::approvals::{Answer, Asked, HELD_KINDS};
use crate::daemon::{invalid, ApprovalAnswer, Daemon, Error, PaneEntry, Result};
use crate::paths::now_ms;

/// Said in `caps`: this station holds a question and a plan for the owner
/// and has `question.answer`.
pub const CAP: &str = "question_answer";
/// The switch (`[flags] question_hold_s = 600`, or `station.flags`): how
/// long a question or a plan waits for an answer from the deck before it
/// moves to the terminal, in seconds. 0: never held, as before this.
pub const SWITCH: &str = "question_hold_s";
/// Ten minutes: long enough to see a push on a phone and answer, short
/// enough that someone who went to the terminal instead is not kept from
/// the prompt for long (the card and "Answer in the terminal instead" end
/// the wait at once).
pub const HOLD_DEFAULT: u64 = 600;
/// The station gives up this long before the hook's own timeout, so that
/// it is the station that decides and never the agent cancelling a hook
/// (the hook itself answers 2 seconds before, guard.rs).
const BEFORE_DEADLINE: u64 = 5;
/// A call's input above this is not held: its prompt is the terminal's.
const INPUT_MAX: usize = 256 * 1024;
/// What the owner may type as an answer of their own, or as a note on a
/// plan, in characters.
const TYPED_MAX: usize = 4000;

const QUESTION: &str = "question";
const PLAN: &str = "plan";

/// Which kind of held request a tool's call is.
pub fn kind_of_tool(name: &str) -> Option<&'static str> {
    match name {
        "AskUserQuestion" => Some(QUESTION),
        "ExitPlanMode" => Some(PLAN),
        _ => None,
    }
}

/// What a pane says when its question or plan is the terminal's and the
/// station never held it (or no longer knows why).
pub fn in_terminal_words(kind: &str) -> &'static str {
    match kind {
        PLAN => "asks you to approve its plan, in the terminal",
        _ => "asks you a question, in the terminal",
    }
}

/// Is this reason one of the station's own for a question or a plan that
/// waits in the terminal? Such a reason is not replaced by the agent's
/// own, vaguer notice.
pub fn says_in_terminal(reason: &str) -> bool {
    reason.contains("is in the terminal now") || reason.ends_with(", in the terminal")
}

/// How long to hold, from the switch and the timeout the hook was told.
/// None: not at all.
pub fn hold_for(switch: Option<&Value>, deadline: Option<u64>) -> Option<Duration> {
    let wanted = match switch {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::Bool(false)) => 0,
        _ => HOLD_DEFAULT,
    };
    // A hook that was told no timeout cannot be held safely: the agent
    // would cancel it at a moment nobody here knows.
    let most = deadline?.checked_sub(BEFORE_DEADLINE)?;
    match wanted.min(most) {
        0 => None,
        secs => Some(Duration::from_secs(secs)),
    }
}

/// One question as the agent's own input has it: what the answer is
/// keyed by, and the labels an answer is made of.
struct Raw<'a> {
    question: &'a str,
    multi: bool,
    labels: Vec<&'a str>,
}

/// The questions of a call, when they are in a shape that can be shown
/// whole and answered: one to eight, each with its text and one to
/// sixteen options that have a label, no two questions the same (the
/// answers are keyed by the question's text).
fn raw_questions(input: &Value) -> Option<Vec<Raw<'_>>> {
    let list = input.get("questions")?.as_array()?;
    if list.is_empty() || list.len() > 8 {
        return None;
    }
    let mut out: Vec<Raw> = Vec::new();
    for q in list {
        let question = q.get("question")?.as_str().filter(|t| !t.is_empty())?;
        let options = q.get("options")?.as_array()?;
        if options.is_empty() || options.len() > 16 || out.iter().any(|o| o.question == question) {
            return None;
        }
        let labels = options
            .iter()
            .map(|o| o.get("label")?.as_str().filter(|l| !l.is_empty()))
            .collect::<Option<Vec<&str>>>()?;
        out.push(Raw {
            question,
            multi: q.get("multiSelect") == Some(&json!(true)),
            labels,
        });
    }
    Some(out)
}

/// The owner's choices, as the `answers` object the agent expects: the
/// question's text to the chosen label; several labels joined with ", "
/// (the hook reference: "Multi-select answers join labels with commas");
/// what the owner typed instead ("Other") as it was typed.
///
/// `given`: one entry per question, in order: `{picked: [n, …], other?}`,
/// `picked` being places in that question's options. Everything is
/// checked against the call's own input: an answer can only be made of
/// its labels and of what the owner typed.
pub fn answers_for(
    input: &Value,
    given: &Value,
) -> std::result::Result<Map<String, Value>, String> {
    let questions = raw_questions(input).ok_or("this question cannot be answered from here")?;
    let given = given
        .as_array()
        .ok_or("answers is a list with one entry for each question")?;
    if given.len() != questions.len() {
        return Err(format!(
            "{} answers for {} questions: give one for each",
            given.len(),
            questions.len()
        ));
    }
    let mut out = Map::new();
    for (n, (q, g)) in questions.iter().zip(given).enumerate() {
        let nth = n + 1;
        let mut picked: Vec<usize> = match g.get("picked") {
            None | Some(Value::Null) => vec![],
            Some(Value::Array(a)) => a
                .iter()
                .map(|i| i.as_u64().map(|i| i as usize))
                .collect::<Option<Vec<usize>>>()
                .ok_or(format!(
                    "question {nth}: picked is a list of option numbers"
                ))?,
            Some(_) => {
                return Err(format!(
                    "question {nth}: picked is a list of option numbers"
                ))
            }
        };
        if picked.iter().any(|i| *i >= q.labels.len()) {
            return Err(format!("question {nth}: it has no such option"));
        }
        picked.sort_unstable();
        let before = picked.len();
        picked.dedup();
        if picked.len() != before {
            return Err(format!("question {nth}: an option was picked twice"));
        }
        let other = match g.get("other") {
            None | Some(Value::Null) => "",
            Some(Value::String(t)) => t.trim(),
            Some(_) => return Err(format!("question {nth}: other is text")),
        };
        if other.chars().count() > TYPED_MAX {
            return Err(format!(
                "question {nth}: an answer of your own is at most {TYPED_MAX} characters"
            ));
        }
        let count = picked.len() + usize::from(!other.is_empty());
        if count == 0 {
            return Err(format!("question {nth} has no answer"));
        }
        if count > 1 && !q.multi {
            return Err(format!("question {nth} takes one answer"));
        }
        let mut parts: Vec<&str> = picked.iter().map(|i| q.labels[*i]).collect();
        if !other.is_empty() {
            parts.push(other);
        }
        out.insert(q.question.to_string(), json!(parts.join(", ")));
    }
    Ok(out)
}

/// What the owner's `question.answer` comes to for a row of this kind:
/// allow or not, and what is kept as the answer (`reason` in the row).
/// For a question: its `answers` object as JSON. For a plan: nothing when
/// it is approved, the owner's note when it is sent back.
fn verdict(
    kind: &str,
    input: &Value,
    params: &Value,
) -> std::result::Result<(bool, Option<String>), String> {
    if kind == QUESTION {
        // "Cancel": the owner declines to answer. The agent is told so.
        if params.get("cancel") == Some(&json!(true)) {
            return Ok((false, Some(DECLINED_MARK.to_string())));
        }
        let given = params
            .get("answers")
            .ok_or("missing parameter: answers (or release: true)")?;
        let answers = answers_for(input, given)?;
        return Ok((true, Some(Value::Object(answers).to_string())));
    }
    let approve = params
        .get("approve")
        .and_then(|a| a.as_bool())
        .ok_or("missing parameter: approve (true or false), or release: true")?;
    let note = params
        .get("text")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .unwrap_or("");
    if note.chars().count() > TYPED_MAX {
        return Err(format!("a note is at most {TYPED_MAX} characters"));
    }
    Ok(match approve {
        true => (true, None),
        false => (false, Some(note.to_string()).filter(|n| !n.is_empty())),
    })
}

/// What is kept as the answer of a question the owner cancelled.
const DECLINED_MARK: &str = "declined";

/// What the agent is told when the owner cancels its question (the reason
/// of a refused call is shown to the agent: Claude Code's hook reference).
pub const DECLINED: &str = "The owner saw this question in spandeck and chose not to answer it. Do not ask it again as it is: go on as you judge best with what you know, or stop and say what you need.";

/// What the agent is told when the owner sends a plan back.
pub fn keep_planning(note: Option<&str>) -> String {
    // The archive knows a plan that was sent back by these first words.
    let mut text = format!(
        "{}: keep planning, and do not start the work.",
        spandeck_archive::conversation::SENT_BACK
    );
    if let Some(note) = note.filter(|n| !n.is_empty()) {
        text.push_str(" The owner's note: ");
        text.push_str(note);
    }
    text
}

/// The hook's decision for an answer the owner gave. `input`: the call's
/// input as the hook has it now, which is given back whole.
pub fn decision_for(kind: &str, input: &Value, answer: &ApprovalAnswer) -> HookDecision {
    match (kind, answer.allow) {
        (QUESTION, true) => {
            let answers = answer
                .reason
                .as_deref()
                .and_then(|r| serde_json::from_str::<Value>(r).ok())
                .filter(|a| a.is_object());
            match (answers, input.is_object()) {
                (Some(answers), true) => {
                    let mut input = input.clone();
                    input["answers"] = answers;
                    HookDecision::answer(input)
                }
                // Not an answer that can be given: the terminal's prompt.
                _ => HookDecision::none(),
            }
        }
        (PLAN, true) if input.is_object() => HookDecision::answer(input.clone()),
        (PLAN, false) => HookDecision::deny(keep_planning(answer.reason.as_deref())),
        // The owner cancelled the question (and only then: a question
        // that ran out of time, or was let go, is the terminal's).
        (QUESTION, false) if answer.reason.as_deref() == Some(DECLINED_MARK) => {
            HookDecision::deny(DECLINED.to_string())
        }
        _ => HookDecision::none(),
    }
}

/// A held row as the owner's view shows it: the question or the plan,
/// cleaned and cut the way a conversation's items are. Never the hook's
/// mark, and never the raw input.
fn shown(row: &Value) -> Value {
    let kind = row["kind"].as_str().unwrap_or("");
    let input = &row["detail"]["input"];
    let mut out = json!({
        "kind": kind, "id": row["id"], "ts_ms": row["ts"],
        "until_ms": row["detail"]["until"], "attached": row["attached"],
        "tool_use_id": row["detail"]["tool"]["id"],
    });
    if kind == QUESTION {
        out["questions"] = json!(questions_of(input));
    } else {
        let (text, more) = cap(&clean(input["plan"].as_str().unwrap_or("")), DETAIL_CAP);
        out["text"] = json!(text);
        out["more"] = json!(more);
    }
    out
}

impl Daemon {
    /// The questions and plans held for the owner now, oldest first, for
    /// one pane. The owner's only (conversation.rs).
    pub(crate) fn questions_held(&self, pane: &str) -> Vec<Value> {
        let mut rows: Vec<Value> = self
            .approvals
            .held(true)
            .iter()
            .filter(|r| r["pane"] == pane)
            .map(shown)
            .collect();
        rows.sort_by_key(|r| r["ts_ms"].as_i64().unwrap_or(0));
        rows.truncate(8);
        rows
    }

    /// The plan each held `ExitPlanMode` call of a pane was given by its
    /// hook, by the call's own id. Claude Code writes a plan to a file and
    /// puts what that file held into the call when the call is made; a
    /// change to the file in the same step is not in it (seen with 2.1.325
    /// on 16 Nov 2026: the second plan's record held the first plan). What
    /// the hook was given is what the owner was shown and answered.
    pub(crate) fn plans_given(&self, pane: &str) -> Vec<(String, String)> {
        self.approvals
            .held(false)
            .iter()
            .filter(|r| r["pane"] == pane && r["kind"] == PLAN)
            .filter_map(|r| {
                Some((
                    r["detail"]["tool"]["id"].as_str()?.to_string(),
                    r["detail"]["input"]["plan"].as_str()?.to_string(),
                ))
            })
            .collect()
    }

    /// What kind of thing a pane is asking the owner, for the pane list:
    /// `question` or `plan`, and until when it waits here. Nothing of what
    /// it says: agents read the pane list.
    pub(crate) fn asks_view(&self, pane: &str) -> Value {
        if !self.approvals.any_waiting() {
            return Value::Null;
        }
        self.approvals
            .held(true)
            .iter()
            .filter(|r| r["pane"] == pane)
            .min_by_key(|r| r["ts"].as_i64().unwrap_or(0))
            .map_or(
                Value::Null,
                |r| json!({"kind": r["kind"], "until_ms": r["detail"]["until"]}),
            )
    }

    fn held_time(until_ms: u64) -> String {
        // "14:12 UTC"; the words around it say UTC once.
        crate::restore::clock_utc(until_ms).replace(" UTC", "")
    }

    /// The hook of the entry for a question or a plan asks: hold it for
    /// the owner, and answer with what the owner chose. Everything that is
    /// not exactly that says nothing, and the agent's own prompt appears.
    ///
    /// The tool call itself is seen by the other hook, the one every tool
    /// call has (`hook_ingest`): the pane's tool, the ship's log and the
    /// rules are its business. `deadline`: the timeout this hook was told.
    pub async fn hook_answer(
        &self,
        pane_id: &str,
        vendor: &str,
        payload: Value,
        hook_id: Option<&str>,
        deadline: Option<u64>,
    ) -> Result<HookReply> {
        let p = self.pane(pane_id)?;
        let none = Ok(HookReply::default());
        let Some(a) = AgentKind::parse(vendor)
            .and_then(adapter)
            .filter(|a| a.capabilities().answer_questions)
        else {
            return none;
        };
        let event = payload["hook_event_name"].as_str().unwrap_or("");
        let Some(kind) = payload["tool_name"].as_str().and_then(kind_of_tool) else {
            return none;
        };
        let Some(hold) = hold_for(self.flag(SWITCH).as_ref(), deadline) else {
            return none;
        };
        let input = payload.get("tool_input").cloned().unwrap_or(Value::Null);
        if event != "PreToolUse" || !input.is_object() || input.to_string().len() > INPUT_MAX {
            return none;
        }
        if kind == QUESTION && raw_questions(&input).is_none() {
            return none;
        }
        let meta = p.meta();
        // A rule that refuses this call is said by the other hook; the
        // owner is not asked about a call that will not run.
        let refused = a
            .normalize(&payload)
            .and_then(|ev| ev.tool)
            .is_some_and(|tool| self.orders_check(&meta, &tool).is_some());
        if refused || meta.exit.is_some() {
            return none;
        }
        let now = now_ms();
        let until = now + hold.as_millis() as u64;
        let summary = match kind {
            QUESTION => "asks you a question",
            _ => "asks you to approve its plan",
        };
        let asked = self.approvals.ask(
            &p.id,
            meta.project.as_deref(),
            kind,
            summary,
            json!({
                "tool": {"name": payload["tool_name"], "id": payload["tool_use_id"]},
                "input": input, "until": until,
            }),
            hook_id,
            now,
            hold,
        );
        let log = |what: &str, data: Value| {
            self.record(
                what,
                Some(&p.id),
                meta.project.as_deref(),
                meta.task.as_deref(),
                None,
                data,
            )
        };
        let (id, waiter, rx, left) = match asked {
            Asked::Wait {
                id,
                waiter,
                rx,
                left,
                new,
            } => {
                // What is said outside the conversation never says what
                // was asked: the ship's log, the notice and the pane list
                // are read by callers who may not read a conversation.
                if new {
                    log(
                        "question.asked",
                        json!({"id": id, "kind": kind, "pane_name": meta.name, "until_ms": until}),
                    );
                    self.notify(
                        "approval",
                        Some(&p.id),
                        meta.project.as_deref(),
                        &format!("{} {summary}", meta.name),
                        &format!(
                            "Answer it in its conversation. It moves to the terminal at {} UTC.",
                            Self::held_time(until)
                        ),
                    );
                } else {
                    log("question.reattached", json!({"id": id}));
                }
                (id, waiter, rx, left)
            }
            Asked::Answered { id, answer } => {
                log(
                    "question.collected",
                    json!({"id": id, "allow": answer.allow}),
                );
                self.set_state(&p, AgentState::Working, None);
                return Ok(a.reply(event, &decision_for(kind, &input, &answer)));
            }
            Asked::Gone => return none,
        };
        // Until when it waits here is in the row; the pane says only that
        // it asks, and where to answer.
        let row_until = self
            .approvals
            .held(true)
            .iter()
            .find(|r| r["id"] == id.as_str())
            .and_then(|r| r["detail"]["until"].as_u64())
            .unwrap_or(until);
        self.set_state(
            &p,
            AgentState::NeedsYou,
            Some(format!(
                "{summary}: answer it in Chat (in the terminal from {} UTC)",
                Self::held_time(row_until)
            )),
        );
        if let Some(task) = &meta.task {
            self.task_follow(task, "running", None);
            self.task_follow(task, "waiting_for_approval", Some(summary));
        }
        let answer = tokio::time::timeout(left, rx).await;
        if let Some(task) = &meta.task {
            self.task_follow(task, "running", None);
        }
        let in_terminal = |why: &str| {
            let what = if kind == QUESTION { "question" } else { "plan" };
            self.set_state(
                &p,
                AgentState::NeedsYou,
                Some(format!("its {what} is in the terminal now ({why})")),
            );
        };
        match answer {
            Ok(Ok(answer)) => {
                self.set_state(&p, AgentState::Working, None);
                Ok(a.reply(event, &decision_for(kind, &input, &answer)))
            }
            _ => {
                if self.approvals.finish(&id, waiter) {
                    // Its time ran out with this hook still the one
                    // waiting: nobody answered here.
                    self.approvals.expire(&id, now_ms());
                    log("question.lapsed", json!({"id": id, "kind": kind}));
                    self.emit(
                        "question.resolved",
                        Some(&p.id),
                        None,
                        json!({"id": id, "status": "expired"}),
                    );
                    in_terminal("nobody answered it here in time");
                } else if self
                    .approvals
                    .kind_and_status(&id)
                    .is_some_and(|(_, status)| status == "released")
                {
                    in_terminal("you chose to answer it there");
                }
                // Otherwise: the hook came back and waits again, or the
                // pane was stopped or ended. Nothing to say.
                none
            }
        }
    }

    /// `question.answer {id, answers | approve, text?} | {id, release: true}`:
    /// the owner's answer to a held question or plan, or letting it go to
    /// the terminal. Applied once: a second answer finds nothing waiting.
    pub fn answer_question(&self, id: &str, params: &Value, by: &str) -> Result<Value> {
        let gone = || {
            invalid("that question is no longer waiting here (answered, moved to the terminal, or the agent moved on)")
        };
        let row = self
            .approvals
            .held(false)
            .into_iter()
            .find(|r| r["id"] == id)
            .ok_or_else(|| Error::NotFound(format!("question {id}")))?;
        let kind = row["kind"].as_str().unwrap_or("").to_string();
        if params.get("release") == Some(&json!(true)) {
            let pane = self.approvals.release(id, by, now_ms()).ok_or_else(gone)?;
            let saved = self.approvals.saved(id);
            self.record(
                "question.released",
                Some(&pane),
                None,
                None,
                Some(by),
                json!({"id": id, "kind": kind, "saved": saved}),
            );
            self.emit(
                "question.resolved",
                Some(&pane),
                None,
                json!({"id": id, "status": "released"}),
            );
            return Ok(json!({"ok": true, "released": true, "saved": saved}));
        }
        let (allow, reason) = verdict(&kind, &row["detail"]["input"], params).map_err(invalid)?;
        let alive = |pane: &str| {
            self.pane(pane)
                .is_ok_and(|p| p.meta.lock().unwrap().exit.is_none())
        };
        let (pane, delivered) = match self
            .approvals
            .answer(id, allow, reason, by, now_ms(), alive)
        {
            Answer::Delivered { pane } => (pane, true),
            Answer::Recorded { pane } => (Some(pane), false),
            Answer::NotWaiting { expired } => {
                if let Some(pane) = expired {
                    self.emit(
                        "question.resolved",
                        Some(&pane),
                        None,
                        json!({"id": id, "status": "expired", "why": "restart"}),
                    );
                }
                return Err(gone());
            }
        };
        // The owner's answer is the owner's own input to this pane, as
        // typing it in the terminal would be: it ends a stop's hold and a
        // restart's, and never a Pause (holds.rs).
        if let Some(pane) = &pane {
            self.release_by_typing(pane, by);
        }
        let saved = self.approvals.saved(id);
        // Who answered and how it stands; never what was answered.
        self.record(
            "question.answered",
            pane.as_deref(),
            None,
            None,
            Some(by),
            json!({
                "id": id, "kind": kind, "status": if allow { "allowed" } else { "denied" },
                "saved": saved, "delivered": delivered,
            }),
        );
        self.emit(
            "question.resolved",
            pane.as_deref(),
            None,
            json!({"id": id, "status": if allow { "allowed" } else { "denied" }}),
        );
        let mut out = json!({"ok": true, "saved": saved, "delivered": delivered});
        if !delivered {
            out["note"] = json!(crate::approvals::RECORDED);
        }
        Ok(out)
    }

    /// A second try of `question.answer` whose first try's answer was
    /// lost: was the first carried out? By the row itself, as for an
    /// approval. None: it was not, and the call is made.
    pub(crate) fn question_done(&self, id: &str, params: &Value) -> Option<Result<Value>> {
        let row = self
            .approvals
            .held(false)
            .into_iter()
            .find(|r| r["id"] == id)?;
        let status = row["status"].as_str()?;
        let done = if params.get("release") == Some(&json!(true)) {
            status == "released"
        } else {
            let (allow, reason) =
                verdict(row["kind"].as_str()?, &row["detail"]["input"], params).ok()?;
            status == if allow { "allowed" } else { "denied" } && row["reason"] == json!(reason)
        };
        done.then(|| {
            Ok(json!({
                "ok": true, "repeat": true,
                "saved": !self.db.unsaved(&format!("approval-status:{id}")),
            }))
        })
    }

    /// The turn this pane's question belonged to is over (it ended, was
    /// interrupted, or a new prompt began): what it was asking is no
    /// longer a thing the owner can answer.
    pub(crate) fn questions_moot(&self, p: &PaneEntry) {
        if !self.approvals.any_waiting() {
            return;
        }
        for id in self.approvals.cancel_held(&p.id, now_ms()) {
            self.emit(
                "question.resolved",
                Some(&p.id),
                None,
                json!({"id": id, "status": "cancelled"}),
            );
        }
    }
}

/// Is this the row of a question or a plan?
pub fn is_held(kind: &str) -> bool {
    HELD_KINDS.contains(&kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn herons() -> Value {
        json!({"questions": [
            {"header": "Species", "question": "Which herons count?", "multiSelect": false,
             "options": [{"label": "Grey herons only", "description": "Skip egrets"}, {"label": "All herons"}]},
            {"header": "Rivers", "question": "Which rivers?", "multiSelect": true,
             "options": [{"label": "Avon"}, {"label": "Exe"}, {"label": "Tamar"}]},
        ]})
    }

    /// Mobile plan, phase M3: "a question's answer returns the right
    /// `updatedInput`". What the owner picked on the card becomes the
    /// `answers` object Claude Code documents: the question's text to the
    /// label; several labels joined with commas; what was typed under
    /// "Other" as typed, alone or beside labels of a multi-select. And an
    /// answer is made only of the call's own labels and the owner's text:
    /// a place that is not an option, two answers to a single-select, a
    /// question left out, are refused, never guessed.
    #[test]
    fn the_owners_choices_become_the_answers_the_agent_expects() {
        let input = herons();
        let ok = |given: Value| Value::Object(answers_for(&input, &given).unwrap());
        assert_eq!(
            ok(json!([{"picked": [0]}, {"picked": [2, 0]}])),
            json!({"Which herons count?": "Grey herons only", "Which rivers?": "Avon, Tamar"})
        );
        assert_eq!(
            ok(
                json!([{"other": "  only the ones on the marsh "}, {"picked": [1], "other": "Dart"}])
            ),
            json!({"Which herons count?": "only the ones on the marsh", "Which rivers?": "Exe, Dart"})
        );
        for (bad, why) in [
            (json!([{"picked": [0]}]), "1 answers for 2 questions"),
            (json!([{"picked": [2]}, {"picked": [0]}]), "no such option"),
            (
                json!([{"picked": [0, 1]}, {"picked": [0]}]),
                "takes one answer",
            ),
            (
                json!([{"picked": [0], "other": "x"}, {"picked": [0]}]),
                "takes one answer",
            ),
            (json!([{"picked": [0]}, {}]), "question 2 has no answer"),
            (json!([{"picked": [0]}, {"picked": [1, 1]}]), "picked twice"),
            (
                json!([{"picked": ["Grey herons only"]}, {"picked": [0]}]),
                "option numbers",
            ),
            (json!({"Which herons count?": "anything"}), "a list"),
        ] {
            let err = answers_for(&input, &bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
        // A call whose questions cannot be shown whole or keyed is not
        // answerable from here at all: it stays the terminal's.
        for odd in [
            json!({}),
            json!({"questions": []}),
            json!({"questions": [{"question": "Q?", "options": []}]}),
            json!({"questions": [{"question": "Q?", "options": [{"label": "a"}]}, {"question": "Q?", "options": [{"label": "b"}]}]}),
            json!({"questions": [{"question": "Q?", "options": [{"description": "no label"}]}]}),
        ] {
            assert!(raw_questions(&odd).is_none(), "{odd}");
        }
    }

    /// The exchange with the agent, as Claude Code's hook reference gives
    /// it ("Tools that require user interaction"): a question is answered
    /// by allowing the call with its whole input given back and `answers`
    /// added; a plan is approved by allowing it with its input given back,
    /// and sent back by denying it, with words for the agent and the
    /// owner's note. An answer that cannot be read is no answer: the
    /// hook says nothing and the terminal asks.
    #[test]
    fn an_answer_goes_back_as_the_calls_own_input() {
        let a = adapter(AgentKind::Claude).unwrap();
        let input = herons();
        // The owner's report of 17 Nov 2026 ("there was no way to
        // cancel"): a cancelled question is refused with words for the
        // agent; one that only ran out of time says nothing.
        let (allow, reason) = verdict(QUESTION, &input, &json!({"cancel": true})).unwrap();
        let cancelled = ApprovalAnswer { allow, reason };
        let out: Value = serde_json::from_str(
            &a.reply("PreToolUse", &decision_for(QUESTION, &input, &cancelled))
                .stdout,
        )
        .unwrap();
        assert_eq!(
            (
                &out["hookSpecificOutput"]["permissionDecision"],
                &out["hookSpecificOutput"]["permissionDecisionReason"]
            ),
            (&json!("deny"), &json!(DECLINED))
        );
        let lapsed = ApprovalAnswer {
            allow: false,
            reason: None,
        };
        assert!(a
            .reply("PreToolUse", &decision_for(QUESTION, &input, &lapsed))
            .stdout
            .is_empty());
        let answers = json!({"Which herons count?": "All herons", "Which rivers?": "Avon"});
        let given = ApprovalAnswer {
            allow: true,
            reason: Some(answers.to_string()),
        };
        let reply = a.reply("PreToolUse", &decision_for(QUESTION, &input, &given));
        let out: Value = serde_json::from_str(&reply.stdout).unwrap();
        let mut want = input.clone();
        want["answers"] = answers;
        assert_eq!(
            out,
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": want}})
        );
        assert_eq!(reply.exit_code, 0);

        let plan = json!({"plan": "# Tally plan\n\n1. Count", "planFilePath": "/home/ada/.claude/plans/tally.md"});
        let yes = ApprovalAnswer {
            allow: true,
            reason: None,
        };
        let out: Value = serde_json::from_str(
            &a.reply("PreToolUse", &decision_for(PLAN, &plan, &yes))
                .stdout,
        )
        .unwrap();
        assert_eq!(
            out,
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": plan}})
        );
        let no = ApprovalAnswer {
            allow: false,
            reason: Some("count the egrets too".into()),
        };
        let out: Value = serde_json::from_str(
            &a.reply("PreToolUse", &decision_for(PLAN, &plan, &no))
                .stdout,
        )
        .unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            out["hookSpecificOutput"]["permissionDecisionReason"],
            "The owner read this plan in spandeck and did not approve it: keep planning, and do not start the work. The owner's note: count the egrets too"
        );
        assert!(out["hookSpecificOutput"].get("updatedInput").is_none());

        for unreadable in [None, Some("not json".to_string()), Some("[1]".to_string())] {
            let given = ApprovalAnswer {
                allow: true,
                reason: unreadable,
            };
            assert!(decision_for(QUESTION, &input, &given).is_none());
        }
    }

    /// How long a question waits here: ten minutes unless the owner set
    /// the switch; never past the hook's own timeout less a margin, so
    /// that the station decides and the agent never cancels the hook; not
    /// at all with the switch at 0, or for a hook that was told no
    /// timeout.
    #[test]
    fn how_long_a_question_is_held() {
        let s = Duration::from_secs;
        assert_eq!(hold_for(None, Some(3600)), Some(s(600)));
        assert_eq!(hold_for(Some(&json!(3000)), Some(3600)), Some(s(3000)));
        assert_eq!(hold_for(Some(&json!(86400)), Some(3600)), Some(s(3595)));
        assert_eq!(hold_for(Some(&json!(600)), Some(75)), Some(s(70)));
        assert_eq!(hold_for(Some(&json!("soon")), Some(3600)), Some(s(600)));
        assert_eq!(hold_for(Some(&json!(0)), Some(3600)), None);
        assert_eq!(hold_for(Some(&json!(false)), Some(3600)), None);
        assert_eq!(hold_for(None, None), None);
        assert_eq!(hold_for(None, Some(4)), None);
    }

    /// Rule 6, and the scrubbing Guarantee: what the view is given of a
    /// held question is cleaned text (no terminal escapes) and never the
    /// hook's mark or the raw input.
    #[test]
    fn what_is_shown_of_a_held_question_is_cleaned_text() {
        let row = json!({
            "id": "a1", "ts": 5, "kind": "question", "attached": true,
            "detail": {"tool": {"name": "AskUserQuestion", "id": "toolu_1"}, "until": 605, "hook": "abcd",
                "input": {"questions": [{"header": "Sp\u{1b}[31mecies", "question": "Which\u{7} herons?", "multiSelect": true,
                    "options": [{"label": "<b>Grey</b>", "description": "x"}]}]}},
        });
        let out = shown(&row);
        assert_eq!(
            out,
            json!({"kind": "question", "id": "a1", "ts_ms": 5, "until_ms": 605, "attached": true, "tool_use_id": "toolu_1",
                "questions": [{"header": "Species", "question": "Which herons?", "multi": true,
                    "options": [{"label": "<b>Grey</b>", "description": "x"}]}]})
        );
        let plan = json!({"id": "a2", "ts": 6, "kind": "plan", "attached": false,
            "detail": {"tool": {"name": "ExitPlanMode", "id": null}, "until": 606, "input": {"plan": "# Plan\u{1b}[0m\n\n1. Count"}}});
        let out = shown(&plan);
        assert_eq!(out["text"], "# Plan\n\n1. Count");
        assert_eq!(out["more"], false);
        assert!(out.get("questions").is_none());
    }
}
