# The conversation API

The station side of the conversation view (mobile plan, phase M1; `DECISIONS.md`, "The
conversation view"). This is the contract the view is built on (`crates/web/assets/chat.js`,
phase M2). The code is
`crates/archive/src/conversation.rs` (session file to items), `crates/core/src/conversation.rs`
(the calls) and `watch_local` / `watch_remote` in `crates/web/src/lib.rs` (the live stream).
Answering a question or a plan (phase M3, 16 Nov 2026) is `crates/core/src/questions.rs`:
`live.questions` and `question.answer`, below. Since 16 Nov 2026 every pane has a conversation
for a caller that asks for it (`DECISIONS.md`, "Chat for every pane"): the agent's own where
the station can read it (`source: "transcript"`), else one made from the pane's screen
(`source: "screen"`; `crates/core/src/screentalk.rs`); and a Codex pane's session is found
among Codex's own files when no hook has named it (`crates/core/src/discover.rs`). See
"From the screen" and "How a pane's session is found", below.

## Rules that hold for every call

- **The owner's only.** A pane's token (an agent) is refused, and so is the read-only caller.
  No station calls these for itself. A pane on another station is reached the way every pane
  call is: `peer.call {station, method, params}`, which the pane's station takes as the owner's
  request passed on. The stream takes `station` itself (below).
- **Text is data.** Every string is text from a session file: a prompt, markdown source, a
  command, a tool's output. Nothing is HTML and nothing is to be inserted as HTML. The station
  has already dropped terminal escape sequences and control characters other than newline and
  tab; the view still builds DOM nodes with `textContent` only.
- **Thinking is never served.** A `thinking` item says how long, and nothing else. Pictures in a
  message are counted, never sent.
- **Times are the station's.** `ts_ms`, `tool_since_ms` and the rest are on the pane's station's
  clock, and `live.now_ms` is that clock when the answer was made. Work out "42 s" as
  `live.now_ms - live.tool_since_ms`, plus the time since the answer arrived. Do not compare
  with the browser's clock.
- **Not token by token.** An agent in a terminal writes its reply to the file a block at a time.
  A `text` item appears whole.

## `pane.conversation`

```
{pane, after?, before?, limit?, session?, screen?}
```

| Parameter | Meaning |
|---|---|
| `pane` | The pane's id (or `id@station`, or a unique name, as everywhere). |
| `screen: true` | The caller can show a conversation made from the screen. A pane with no transcript then answers with one (`available: true`, `source: "screen"`) where it would have answered `available: false`. Without it, every answer is what it was before 16 Nov 2026. Send it with every call for the pane, the watch included. A station from before ignores it. |
| neither `after` nor `before` | The newest `limit` items. What a view opens with. |
| `after: N` | Items on session lines after `N`, oldest first, at most `limit`. `N` is a `cursor` from an earlier answer. |
| `before: N` | The `limit` items just before line `N`. `N` is a `first` from an earlier answer. The page above. |
| `limit` | 1 to 200, default 50. A session line is never split between two answers, so an answer can hold a few more. |
| `session` | The `session` the caller's cursor came from. When the pane has moved to another session (it was restarted, or `/clear` began a new one), `after` and `before` are ignored, the answer is the newest items of the new session and `reset` is true: throw away what you hold. Always send it with `after`. |

Before it answers, the station copies any new complete lines of the pane's session file, so an
answer is as fresh as the file whoever asks.

### Answer, when the pane has a conversation

```json
{
  "pane": "p_01jx", "station": "spark2", "agent": "claude",
  "available": true,
  "source": "transcript", "session_from": "hooks",
  "session": "5b1e7c0a-0000-4000-8000-00000000c0de",
  "reset": false,
  "title": "Heron counts for the marsh survey",
  "items": [
    {"id": "4.0", "line": 4, "kind": "user", "ts_ms": 1794736800000,
     "text": "Count the herons in survey.csv and fix the tally script. [Image #1]",
     "images": 1, "origin": "you"},
    {"id": "8.0", "line": 8, "kind": "thinking", "ts_ms": 1794736804000, "duration_ms": 4200},
    {"id": "9.0", "line": 9, "kind": "text", "ts_ms": 1794736804500,
     "text": "I'll count the rows first, then fix `tally.py`."},
    {"id": "10.0", "line": 10, "kind": "tool", "ts_ms": 1794736805000,
     "name": "Bash", "summary": "wc -l survey.csv", "status": "ok", "duration_ms": 2000},
    {"id": "11.0", "line": 11, "kind": "tool", "ts_ms": 1794736805200,
     "name": "Edit", "summary": "/home/ada/marsh/tally.py", "status": "ok",
     "duration_ms": 800, "added": 2, "removed": 1},
    {"id": "17.0", "line": 17, "kind": "question", "status": "answered", "duration_ms": 19000,
     "questions": [{"header": "Species", "question": "Which herons count?", "multi": false,
       "options": [{"label": "Grey herons only", "description": "Skip egrets and bitterns"},
                   {"label": "All herons", "description": "Count every bird in the heron family"}]}],
     "answers": [["Which herons count?", "Grey herons only"]]},
    {"id": "19.0", "line": 19, "kind": "plan", "status": "approved",
     "text": "# Tally plan\n\n1. Filter the rows to grey herons\n2. Run the tests again"},
    {"id": "21.0", "line": 21, "kind": "tasks", "name": "Explore",
     "summary": "Look for other tally scripts", "status": "ok", "background": true},
    {"id": "26.0", "line": 26, "kind": "notice", "what": "turn_ended", "duration_ms": 61000}
  ],
  "updated": [],
  "cursor": 43,
  "first": 4,
  "more_before": false,
  "more_after": false,
  "not_understood": 0,
  "not_understood_kinds": [],
  "older_dropped": false,
  "live": {
    "state": "working", "state_since_ms": 1794736800100, "needs_reason": null,
    "tool": "Bash", "tool_since_ms": 1794736805000,
    "subagents": [{"kind": "Explore", "since_ms": 1794736851000}],
    "paused": false, "exited": false, "resumed": false, "hooks_seen": true,
    "held": null,
    "trust": null,
    "approvals": [],
    "questions": [],
    "unread": true, "read_line": 9,
    "now_ms": 1794736806200
  }
}
```

| Field | Meaning |
|---|---|
| `source` | `transcript`: the items are the agent's own conversation, read from its session file. `screen`: they are made from the pane's terminal (below). A station from before 16 Nov 2026 sends no `source`: it is `transcript`. |
| `session_from` | How the session is known. `hooks`: the agent's hooks named it. `resume`: the pane was started into it. `files`: found among the program's own session files, because no hook has named one (below). Absent with `source: "screen"`. |
| `items` | The items asked for, oldest first. |
| `updated` | Only with `after`: items you were sent before (their line is at or before the `after` you gave) that a line since has changed, which is a call that got its result, or a call the turn left behind. Replace the one you hold by `id`. An item in `items` is already complete as far as the file goes; the same item may come again later in `updated`, and replacing is always right. |
| `cursor` | The session line this answer reaches. Ask `after` it for what comes next. Keep it with `session`. In an answer to `before` it is the line of the last item of that older page: do not take it as your place in the stream. |
| `first` | The line of the first item here (`null` when `items` is empty). Ask `before` it for the page above. |
| `more_before`, `more_after` | Whether there are items above the first one here, and after the last. With `after`, `more_after: true` means the answer was cut at `limit`: ask again from `cursor`. |
| `title` | The session's own title when its file has one (Claude Code writes one), else `null`: show the pane's name. |
| `not_understood` | How many records of this session the station did not understand and so left out. Above 0, say so ("3 records not shown: open the terminal to see everything"). It is for the whole session, not for the page. |
| `not_understood_kinds` | `[kind, count]` pairs, eight kinds at most, for a bug report: the record's type name as written in the file. |
| `older_dropped` | The session has more items than the station indexes (100,000); the oldest are not served. |
| `live` | What the pane is doing now. Below. |

### Answer, when it has none

```json
{
  "pane": "p_01jy", "station": "spark2", "agent": "shell",
  "available": false,
  "reason": "no conversation for this pane: it runs a shell, not an agent",
  "items": [], "updated": [],
  "live": {"state": "idle", "...": "..."}
}
```

`reason` always begins `no conversation for this pane: `. Show the terminal, and the reason
where the Chat switch would be. This is the answer only to a caller that did not send
`screen: true`, or from a station from before 16 Nov 2026; with `screen: true` the same pane
answers from its screen, with the same `reason` ("From the screen", below). The reasons:

| Pane | Reason ends |
|---|---|
| A shell, or a program spandeck does not know | `it runs a shell, not an agent` |
| OpenCode, Antigravity | `<Agent> keeps its conversation where spandeck cannot read it yet` |
| Claude Code before its first hook | `Claude Code has not said which session it is in yet (nothing has come from its hooks)` |
| Codex with no hook yet and no session file since the pane started (every Codex pane before its first message) | `Codex has not said which session it is in yet (nothing has come from its hooks), and no session file of Codex's for this folder has appeared since the pane started` |
| Codex with no hook, and another hookless Codex pane in the same folder | `…, and another pane runs Codex in this folder and reports nothing either, so a session file begun since both started could be either pane's: spandeck does not guess` |
| Codex with no hook, and several session files in its folder written to at the same time | `…, and 2 of Codex's session files for this folder were written to at the same time since the pane started, so more than one Codex is at work here: spandeck does not guess which is this pane's` |
| A pane started into a saved session whose file is not found | `<Agent> has not said where its session file is` |
| A session that is known and not in the archive | `its session is not in the archive` |

### From the screen (`source: "screen"`)

The answer to a caller that sent `screen: true`, for a pane with no transcript:

```json
{
  "pane": "p_01jy", "station": "spark2", "agent": "shell",
  "available": true, "source": "screen",
  "reason": "no conversation for this pane: it runs a shell, not an agent",
  "session": "screen:1794736800000", "reset": false, "title": null,
  "items": [
    {"id": "0.0", "line": 0, "kind": "screen", "ts_ms": 1794736800100, "lines": 1, "text": "Last login: Mon Nov 16"},
    {"id": "1.0", "line": 1, "kind": "user", "ts_ms": 1794736811000, "origin": "you", "text": "ls reeds"},
    {"id": "2.0", "line": 2, "kind": "screen", "ts_ms": 1794736811200, "lines": 3, "open": true,
     "text": "$ ls reeds\navon.csv  exe.csv\n$"}
  ],
  "updated": [],
  "cursor": 5, "first": 0, "more_before": false, "more_after": false,
  "not_understood": 0, "not_understood_kinds": [], "older_dropped": false,
  "live": {"state": "idle", "...": "..."}
}
```

- It has the shape of any answer and is applied the same way: `items` are new, `updated`
  replace the ones held by `id`, `reset` means throw away what is held. `reason` is why there
  is no transcript, in the words of the table above: say it ("From the screen: it runs a shell,
  not an agent"), where "Terminal only" was said.
- **Items.** `user`: something sent to the pane through `pane.paste` or `pane.send_text`
  (`origin`: `you`, or `agent` with `from`, the sending pane's name), and the first message a
  pane was started with. `screen`: what the pane printed after it, as plain text in the
  terminal's own layout (show it in a fixed-width font; lines are not wrapped).

| `screen` field | Meaning |
|---|---|
| `text` | The last 16 KB of the block, from a line's beginning. Control characters are dropped. An empty `text` in `updated` means the block came to nothing: stop showing it. |
| `lines` | How many lines the block has. |
| `more` | `text` is not all of it: `pane.conversation.detail` has the rest (a frozen block: its last 256 KB; the one being printed: its last 4,000 lines, read from the terminal then). |
| `open` | This is the block being printed now: it comes again in `updated` as it grows. When the next message is sent it is frozen: it comes once more without `open`, and never changes again. |
| `whole` | The block is the whole screen as it was at that moment, not what was printed since the message: the program draws its screen itself (a full-screen program, or one that redrew over the place the message was sent at), or the place has left the terminal's history. |

- **`cursor` is a count of changes, not a line.** Send it back as `after`, with `session`, and
  do nothing else with it. `first` and `before` are an item's `line`, as always. `line` and
  `id` (`<line>.0`) only ever grow, and a block keeps its from its first line to frozen.
- **`session`** begins `screen:` and changes when the pane's program is started again. When
  the pane gets a transcript (its first message begins its session file, or its first hook
  arrives), the next answer has the transcript's `session`, `source: "transcript"` and
  `reset: true`; the other way round too.
- **`pane.conversation.detail {pane, item, session, offset?}`** with a `screen:` session gives
  `{pane, session, item, text: {text, offset, total, next}}`, paged like any text.
- `title` is `null`, `live.unread` is false and `pane.read` is refused (`invalid`): there is
  nothing to mark. `live` is the same, so approvals, the folder-trust question and the reasons
  a pane needs the owner are shown as in any conversation.
- Kept in memory at the pane's station: 400 items and 2 MB a pane (`older_dropped` when the
  oldest have gone), and it begins again when the station does.
- The owner's only, like every answer here. No method was added.

### How a pane's session is found

In order: **the session its hooks named** (always, and it takes the place of anything below);
**the session the pane was started into** (`resume`; for Codex its file is found by the id in
the file's name, checked against the file's first line); and, for Codex only, **a session file
found for the pane** (`session_from: "files"`):

- the file is under `<CODEX_HOME or ~/.codex>/sessions/YYYY/MM/DD/`, named `rollout-*.jsonl`;
  its first line is `session_meta` with an `id`, a `cwd` that is the pane's working folder,
  a `timestamp` at or after the moment the pane's program was started, and a `source` that is
  not an object (an object marks a conversation Codex began itself, for a sub-agent);
- no other pane on the station is in that session;
- no other pane that runs Codex in that folder and has had no hook either was already
  started at the file's `timestamp`;
- of those left, the newest, provided every older one was last written to before it began.
  Otherwise none.

It is looked for when the pane's conversation is asked for: at most twice a second while none
is found, every two seconds after, so a new session begun with `/new` is followed. It is only
read: nothing is ever answered or sent to a session, only to a pane. Claude Code is given its
hooks at launch and is never looked for; OpenCode and Antigravity have no reader.

A pane that has no conversation now can have one a moment later (the agent starts and its first
hook arrives). Ask again, or watch: the stream says when.

A station on a version from before this answers `not found: method pane.conversation`
(error code `not_found`). Treat it as no conversation, with the reason that the station needs
an update.

**Mixed versions (16 Nov 2026).** A caller from before sends no `screen` and is answered as
before. A station from before (0.4.7 to 0.4.9) ignores `screen` and answers a pane with no
transcript `available: false` with its reason: show the terminal, and the reason where the
switch would be. There is no `caps` entry: the answer itself says which it is.

### Items

Every item has `id`, `line` and `kind`. `id` is `<session line>.<block in that line>` and never
changes for a session. `ts_ms` is there when the record carries a time. A field that does not
apply is absent, not null.

| `kind` | Fields | Notes |
|---|---|---|
| `user` | `text`, `images?`, `origin`, `from?`, `more?` | `origin`: `you` (typed), `queued` (typed while the agent worked, sent when it was free), `agent` (another agent's message; `from` is what that agent is called), `task` (a background task's report in a shape the station did not recognise; the ones it does are `notice` items), `schedule` (a scheduled wake-up), `system` (Claude Code's own). A slash command is `text: "/compact keep the tally"`. Codex: always `you`. |
| `text` | `text`, `more?`, `phase?` | The agent's words, as markdown source. Codex adds `phase`: `commentary` (said while working) or `final_answer`. |
| `thinking` | `duration_ms?` | Never the thinking. Codex gives no duration. |
| `tool` | `name`, `summary`, `status`, `duration_ms?`, `added?`, `removed?`, `background?` | `summary` is one line, 200 characters at most: the command, the file, the pattern, the address. May be empty. `added` / `removed`: lines an edit changed. `background`: started and left running. |
| `question` | `questions`, `status`, `answers?` | Claude Code's AskUserQuestion. `questions[]`: `header`, `question`, `multi`, `options[] {label, description?}`. `answers`: `[question, answer]` pairs, once answered. While it waits it may be held for the owner: see `live.questions`. |
| `plan` | `text`, `status`, `more?` | Claude Code's ExitPlanMode. `text` is the plan, markdown source: for a plan that was held, the text its hook was given (what the owner was shown); else the plan the result of an approved call carries; else what the call's record holds, which can be an earlier plan when Claude Code changed the plan in the same step, or empty. The detail's `text` follows the same order. `rejected`: the owner declined it in the terminal, or sent it back from the deck. |
| `tasks` | `name`, `summary`, `status`, `background?` | A sub-agent the agent started: `name` is its kind ("Explore"), `summary` what it was asked. One item per sub-agent; fold neighbours into "3 tasks". Which are running now is `live.subagents`. |
| `notice` | `what`, and by `what`: | Something that happened, not something said. |
| | `turn_ended`: `duration_ms?` | The agent finished its turn. |
| | `interrupted` | The owner stopped the turn. |
| | `compacted`: `trigger?`, `pre_tokens?`, `post_tokens?`, `duration_ms?` | The conversation was compacted. |
| | `summary`: `text`, `more?` | The summary a compaction left. Fold it. |
| | `error`: `text`, `level?` | An API error, or an error the agent program reported. |
| | `info`: `text`, `level` | A line the agent program showed (`info`, `notice`, `warning`, `suggestion`). |
| | `away_summary`: `text` | Claude Code's "while you were away". |
| | `command_output`: `text` | What a slash command printed. |
| | `task`: `text`, `level?` | A background task reported: `text` is its summary, `level` its status (`completed`, `failed`). |

`status`:

| Kind | Values |
|---|---|
| `tool`, `tasks` | `running`; `ok`; `error`; `denied` (refused by the owner or a rule); `interrupted` (stopped, or the turn went on without a result); `done` (finished, and the file does not say how: Codex) |
| `question` | `waiting`, `answered`, `dismissed` |
| `plan` | `waiting`, `approved`, `rejected`, `dismissed` |

`more: true` means `text` was cut at 16 KB; the whole of it is `pane.conversation.detail`.

An unknown `kind`, `what`, `status` or `origin` may appear in a later version. Show an unknown
kind as a quiet line with its name, never as an error.

### `live`

| Field | Meaning |
|---|---|
| `state`, `state_since_ms`, `needs_reason` | The pane's state as everywhere in the deck: `idle`, `working`, `needs_you`, `done`, `error`. |
| `tool`, `tool_since_ms` | The tool running now, from the agent's hooks, or `null`. For "Working · Bash · 42 s". The command is the `summary` of the last `tool` item with `status: "running"`. |
| `subagents` | `[{kind, since_ms}]`: the agent's own sub-agents running now. |
| `paused`, `exited`, `resumed`, `hooks_seen` | As in the pane list. `resumed`: the program was started into a saved conversation. |
| `trust` | `null`, or the pane's folder-trust question (16 Nov 2026): `{chosen, asks, answered, folder}`. `asks` is `null` or `{since_ms, button, why}`: the program asks whether to trust the folder and nothing was pressed (`why`: `not_chosen`, `own_choice_is_no`, `not_the_known_screen`, `switched_off`); with `button: true` show "Trust this folder", which is `pane.trust_folder {pane, remember}` (the owner's only; it moves the program's marker to the trusting choice if need be, checks, and confirms). `answered` is `null` or `{at_ms, how, by}` (`how`: `standing`, by spandeck on the owner's choice; `tap`). `chosen` is `null` or `{by, why}`: the choice the pane was started with. The same `trust` is in `pane.list` and `pane.get`. A station from before this has no `trust`. |
| `held` | `null`, or why the agent takes no mail until the owner releases it: `{stopped, restored, mail}`. `stopped` is `null` or `{at_ms, by, reached}`: the owner (or a "stop every agent") stopped it; `reached: false` means the program was not reading and the stop did not reach it ("use Kill"). `restored` is `null` or `{at_ms, why}`: the station started it again after a restart (`why`: `crash`, `stop`, `upgrade`). `mail` is how many messages wait. Show it with a Release button: `pane.release {pane}` (the owner's only; `{all: true}` for every pane on the station), answered `{ok, pane, released, still_paused, saved}`. Sending the pane anything (`pane.send_text`, `pane.paste`, `pane.input`) releases it too. `paused` is apart: Release never changes it. The same `held` is in `pane.list` and `pane.get`. A station from before this has no `held`. |
| `approvals` | Requests held for the owner for this pane, oldest first. Each: `{kind: "approval", id: "approval:<approval id>", approval, what, summary, tool, ts_ms, item}`. `approval` is the id `approval.answer {id, allow, reason?}` takes, as today. `what` is the kind of request (`permission`, `route`). `item` is the `id` of the `tool` item the request is about, or `null`: draw the card anyway. It is set only when the station is sure which call it is: the call with the id the hook gave, else the newest running call of that tool with the same command or file. It can name an item on a line after this answer's `cursor`, which the next answer brings. They are not in `items`: they are not in the session file. |
| `questions` | A question or a plan the station holds for the owner for this pane, oldest first (eight at most). Below. A station from before 16 Nov 2026 has no such field. |
| `unread`, `read_line` | Whether the agent has said something after the read mark, and the mark. Below. |
| `now_ms` | The station's clock. |

### `live.questions`: a question or a plan held for the owner

Claude Code asks the owner with two tools: `AskUserQuestion` (item kind `question`) and
`ExitPlanMode` (item kind `plan`). When the pane's session has the hook that can hold such a
call (sessions started after 16 Nov 2026; `DECISIONS.md`, "Answer in place"), the station holds
it, the agent's terminal shows no prompt, and the call is listed here until it is answered,
let go, or its time runs out. They are not in `approvals` and not in `approval.list`.

```json
"questions": [
  {"kind": "question", "id": "a_01jx7q", "item": "17.0",
   "ts_ms": 1794736821000, "until_ms": 1794737421000, "attached": true,
   "questions": [
     {"header": "Species", "question": "Which herons count?", "multi": false,
      "options": [{"label": "Grey herons only", "description": "Skip egrets and bitterns"},
                  {"label": "All herons", "description": "Count every bird in the heron family"}]},
     {"header": "Rivers", "question": "Which rivers?", "multi": true,
      "options": [{"label": "Avon"}, {"label": "Exe"}, {"label": "Tamar"}]}]},
  {"kind": "plan", "id": "a_01jx7r", "item": "19.0",
   "ts_ms": 1794736841000, "until_ms": 1794737441000, "attached": true,
   "text": "# Tally plan\n\n1. Filter the rows to grey herons\n2. Run the tests again", "more": false}
]
```

| Field | Meaning |
|---|---|
| `kind` | `question` or `plan`. |
| `id` | What `question.answer` takes. |
| `item` | The `id` of the `question` or `plan` item this is, or `null` (the item is on a line the answer has not reached yet, or the station is not sure which it is): draw the card anyway, from what is here. Tied by the call's own id; else to the only call of that tool that waits. |
| `ts_ms`, `until_ms` | When it was first asked, and until when the station holds it, on the station's clock: `until_ms - live.now_ms` is how long it still waits here. After that the agent shows its own prompt in the terminal. Counted from the first asking, through a restart. |
| `attached` | `false` while the agent's hook is finding its way back after the station restarted. It can be answered all the same: the answer is kept for the hook. |
| `questions` | For a question: as in a `question` item (`header`, `question`, `multi`, `options[] {label, description?}`), cleaned and cut the same way. One to eight questions, each with one to sixteen options. An answer names an option by its place in `options`, counted from 0. |
| `text`, `more` | For a plan: the plan as the hook was given it, markdown source, cut at 64 KB (`more`). Claude Code may write the call to the session file without the plan, so prefer this to the item's `text`. |

A `question` or `plan` item with `status: "waiting"` that is **not** in `live.questions` is not
held: it waits in the terminal (the hold ran out, the owner let it go, the session was started
before it could be held, or holding is switched off with `question_hold_s = 0`). Show it and
send the owner to the terminal. `live.needs_reason` then begins `its question is in the
terminal now` or `its plan is in the terminal now`, with why in brackets, or reads `asks you a
question, in the terminal` / `asks you to approve its plan, in the terminal` when it was never
held. It is never in `approvals`: Claude Code asks permission for these two tools when it shows
its own prompt, and that request is not held (there is nothing to allow or deny).

**`approvals` and `approval.list`.** A permission request leaves both when it is answered, when
the tool call it was for has run (the owner answered the prompt in the terminal), and when its
turn ends: `approval.resolved` with `status: "cancelled"`. A row of `approval.list` has `tool`
(the tool's name) and `summary` at its top.

**In the pane list.** `pane.list` and `pane.get` say `asks: null`, or
`asks: {kind: "question" | "plan", until_ms}` for a pane with something held: which kind and
until when, never what it says (agents read the pane list). The pane's state is `needs_you`
and its `needs_reason` reads `asks you a question: answer it in Chat (in the terminal from
14:12 UTC)` (or `asks you to approve its plan: …`).

## `question.answer`

The owner's only: an agent's token is refused (`denied`), for its own pane too, and so are a
station's own call and the read-only caller. For a pane on another station:
`peer.call {station, method: "question.answer", params}`.

```
{id, answers: [{picked?: [n, …], other?: "text"}, …]}     a question
{id, approve: true}                                       a plan: approve it
{id, approve: false, text?: "what to change"}             a plan: keep planning
{id, release: true}                                       either: answer in the terminal instead
```

| Parameter | Meaning |
|---|---|
| `id` | The `id` from `live.questions`. |
| `answers` | One entry for each question, in order. `picked`: places in that question's `options` (from 0). `other`: an answer of the owner's own, 4,000 characters at most. A question with `multi: false` takes exactly one of the two, and one place; one with `multi: true` takes any number of places and, beside or instead, `other`. Every question must have an answer. |
| `approve`, `text` | For a plan. `text`: a note for the agent, sent only with `approve: false`, 4,000 characters at most. |
| `release` | Let it go: the hook says nothing and the agent shows its own prompt in the terminal at once. Nothing is answered. |

```json
{"ok": true, "saved": true, "delivered": true}
{"ok": true, "saved": true, "delivered": false, "note": "recorded: the agent will get it when it reconnects"}
{"ok": true, "released": true, "saved": true}
```

- `delivered: false`: the agent's hook is not connected at this moment (the station has just
  restarted); the answer is kept and given to it when it is back.
- **Once.** A second answer is refused (`invalid`: "that question is no longer waiting here
  …"). The same answer sent again with its request id and `again_ms` (the answer to the first
  try was lost) is told `{"ok": true, "repeat": true}` and changes nothing.
- Errors: `not_found` (no question or plan with that id), `invalid` (it is no longer waiting;
  or the answer does not fit the question: a place that is not an option, two answers where
  one is taken, a question with no answer, `answers` for a plan), `denied`.
  `approval.answer` with a question's id is refused (`invalid`), and names this call.
- An answer from here counts as the owner's input to the pane: like typing into it, it releases
  a pane held after a stop or a restart (`held`), and never a paused one. `release` releases
  nothing.
- The station emits `question.resolved` (`data: {id, status}`; `allowed`, `denied`, `released`,
  `expired`, `cancelled`) to decks connected to it; the stream sends the changed `live`.

**What the agent is given** (Claude Code's hook reference, "Tools that require user
interaction"; run by hand with Claude Code 2.1.325 on 16 Nov 2026). The call's whole input, with the answer in it:

```json
{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
  "updatedInput": {"questions": [ …as the agent wrote them… ],
    "answers": {"Which herons count?": "All herons", "Which rivers?": "Avon, Tamar, Dart"}}}}
```

Each question's text maps to the label chosen; several labels are joined with `, `; `other` is
given as typed, after the labels when there are both. A plan that is approved: `allow`, with
`updatedInput` the input as it came (`plan`, `planFilePath`). A plan that is sent back:
`"permissionDecision": "deny"` and `"permissionDecisionReason": "The owner read this plan in
spandeck and did not approve it: keep planning, and do not start the work. The owner's note:
…"`. Let go, or not answered in time: the hook prints nothing.

**How long.** Ten minutes, or the station's `question_hold_s` (seconds; `[flags]` or
`station.flags`; 0: never held). Never past the hook's own timeout less 5 seconds: the entry
spandeck installs gives it an hour.

## `pane.conversation.detail`

```
{pane, item, offset?, session?}
```

One item in full. `item` is an item's `id`. `session`, when given, must be the pane's session
now, or the call is refused (`invalid`): the item is from a conversation that is gone from this
pane. An item that is not there answers `not_found`.

```json
{
  "pane": "p_01jx", "session": "5b1e7c0a-…",
  "item": {"id": "11.0", "line": 11, "kind": "tool", "name": "Edit", "summary": "/home/ada/marsh/tally.py",
           "status": "ok", "duration_ms": 800, "added": 2, "removed": 1},
  "input": {"fields": {"file_path": "/home/ada/marsh/tally.py", "old_string": "    return len(rows)",
                       "new_string": "    herons = …", "replace_all": "false"}, "more": false},
  "output": {"text": "The file /home/ada/marsh/tally.py has been updated.", "offset": 0, "total": 51, "next": null},
  "diff": {"files": [{"file": "/home/ada/marsh/tally.py", "created": false, "hunks": [
      {"old_start": 10, "old_lines": 2, "new_start": 10, "new_lines": 3,
       "lines": [" def tally(rows):", "-    return len(rows)",
                 "+    herons = [r for r in rows if r.species == 'heron']", "+    return len(herons)"]}]}],
    "more": false}
}
```

| Field | For | Meaning |
|---|---|---|
| `item` | all | The item as `pane.conversation` gives it. |
| `input` | `tool`, `tasks`, `question` | `fields`: what the call was given, each value as text (a value that is not text is its JSON), each cut at 8 KB; 32 fields at most. `more`: something was cut. Codex: one field, `input`, or the fields of its `arguments`. |
| `output` | `tool`, `tasks`, `question`, once there is a result | `text`: up to 64 KB of what came back (for a shell command its output, then its error output). `total`: the length of all of it in bytes. `next`: the `offset` to ask for the rest with, or `null` at the end. `images`: pictures in the result, counted, not sent. |
| `diff` | `tool` that changed files | `files[] {file, created, deleted?, hunks[]}`; a hunk is `old_start`, `old_lines`, `new_start`, `new_lines` and `lines`, each line beginning with a space, `+` or `-`. 2,000 lines in all; `more` when cut. Claude Code only (its own record of the patch). |
| `text` | `user`, `text`, `plan`, `notice` whose `more` is true; every `plan` | The same paging as `output`: `text`, `offset`, `total`, `next`. |

A `thinking` item's detail is the item and nothing else.

## The live stream: `pane.conversation.watch`

On the deck's WebSocket (`/api/ws`), like `pane.attach`. Not on the local socket or over HTTP.

```
→ {"id": 7, "method": "pane.conversation.watch",
   "params": {"pane": "p_01jx", "station": "spark2", "session": "…", "after": 43, "limit": 50}}
← {"id": 7, "result": { …exactly a pane.conversation answer…, "chan": 5 }}
← {"chan": 5, "conversation": { …a pane.conversation answer for after: <the cursor before>… }}
← {"chan": 5, "conversation": { … }}
→ {"id": 8, "method": "pane.conversation.unwatch", "params": {"chan": 5}}
```

- The parameters are `pane.conversation`'s, plus `station` when the pane is on another station
  than the one serving the deck. The reply is the `pane.conversation` answer for them, plus
  `chan`. So a view that opens sends no `after` and gets the newest page; a view that comes back
  after a dropped connection sends its `session` and `after: <its cursor>` and is caught up in
  the reply, with no item twice and none missed.
- After the reply, the station looks at the pane's session file every 200 ms and sends a
  `conversation` message whenever there is something to tell: new `items`, `updated` items,
  a `reset`, or a change in `live`, `title`, `available`, `reason` or `not_understood`. Each
  message is a whole `pane.conversation` answer for `after` the cursor of the one before; apply
  it the same way. A change in `live` alone comes with empty `items`.
- A pane with no conversation can be watched: the reply has `available: false`, and a message
  comes when that changes (with `reset: true` and the newest items).
- `{"chan": 5, "conversation_end": {"message": "…"}}`: the pane was closed. Nothing more comes.
- `{"chan": 5, "unreachable": true}`: the pane's station stopped answering the station serving
  the deck. Nothing more comes on this channel: watch again when the station is back.
- `pane.conversation.unwatch {chan}` (or `pane.detach {chan}`) ends it; so does closing the
  socket. The station stops looking at the file when nobody watches.
- An agent's token is refused (`denied`), as for `pane.attach`.
- **Through another station** it works the way `pane.attach` does: the station serving the deck
  opens a connection to the pane's station, signed with the fleet key, and passes each message
  through with the channel number changed. It keeps nothing. The pane's station on an older
  version answers the watch with an error (`not_found`); fall back to the terminal.
- The event bus has no replay, and this stream does not need one: the cursor is the resume
  point. If the socket drops, watch again with `session` and `after`.

Polling works too and is correct by the same cursor: `pane.conversation {pane, session, after}`
(through `peer.call` for another station) every few seconds is cheap when nothing is new (measured: 0.07 ms for
the archive's part, on a 51 MB session).

## Unread: `pane.read`

```
{pane, upto?}   →   {"pane": "p_01jx", "session": "…", "read_line": 43, "unread": false}
```

- The owner has read the pane's conversation up to session line `upto` (a `cursor`, or an item's
  `line`). Without `upto`: all of it. The mark only moves forward, and never past the end.
- The mark is kept at the pane's station, per pane and per session, so it is the same from every
  station's deck and every device. For a pane on another station call it through `peer.call`.
- **Every pane in `pane.list` and `pane.get` now has `unread: true|false`**: the agent has said
  something (a `text` item) on a line after the mark. It is worked out from the archive's
  search index, not from the items, so that the pane list stays cheap; the two can differ
  for a line the items leave out on purpose (an API error shown as a notice counts as said). A pane that was never read and has a reply
  is unread. A shell, or an agent with no conversation, is never unread. A tool call alone does
  not make a pane unread; a pane that needs the owner already says so with its state.
- `live.unread` and `live.read_line` say the same in a conversation answer, and the stream sends
  a message when they change, so a second device watching the pane clears its dot at once. Other
  decks see it at their next look at the pane list.
- The station also emits a `pane.read` event (`data: {session, read_line, unread}`) to decks
  connected to it.
- Asking for a conversation does not mark it read. The view decides when the owner has seen it
  (the newest item is on screen and the page is visible) and calls `pane.read`.
- None of these three calls counts as the owner being at a keyboard, so a phone that shows a
  conversation is still sent the push for another pane.

## Titles for a list: `pane.titles`

```
{}   →   {"titles": {"p_01jx": "Heron counts for the marsh survey"}}
```

- The title of each of this station's panes' sessions, for the panes whose session file has
  one (Claude Code writes one; Codex does not). A pane that is not there has none: show its
  name. It is the `title` a conversation answer gives, for every pane at once.
- A title is made from what was said, so this is **the owner's only**, like the other calls
  here, and it is not in `pane.list`, which agents read. For another station:
  `peer.call {station, method: "pane.titles"}`.
- Cheap enough for a list that asks every few seconds: the station copies each file's new
  lines, reads only the lines written since it last looked, and does not build a session's
  index (which is kept for eight sessions only).
- It does not count as the owner being at a keyboard.
- A station from before this answers `not_found`: its panes go by their names.

## Errors

The usual shape, `{"error": {"code", "message"}}`: `denied` (an agent, or a read-only caller),
`not_found` (no such pane, no such item, or a station from before this), `not_reading`
(`pane.stop` could not reach the program: it is not reading input, was not stopped, and Kill is
what is left; show the message), `invalid` (bad
parameters; `after` and `before` together; `detail` or `read` on a pane with no conversation;
an item of another session), `unreachable` (the pane's station, through the gateway).

## What each agent program gets

| | Claude Code | Codex | OpenCode, Antigravity | Shell |
|---|---|---|---|---|
| Conversation | Yes | Yes, read from 0.169.0 files; named by its hooks, else found among its files. Before its first message it has no session: the screen | From the screen (`screen: true`); else the reason, and the terminal | From the screen (`screen: true`); else the reason |
| Title | Yes | No | | |
| Tool status | `ok` / `error` / `denied` / `interrupted` | `done` (the file does not say) | | |
| Tool duration | Yes | Yes, from the times of the two records | | |
| Diff of an edit | Yes | No | | |
| Thinking duration | Yes | No | | |
| Question, plan | Shown, with the answer once given; held for the owner and answered with `question.answer` (sessions started after 16 Nov 2026) | Not in its files; what Codex asks is in its terminal | The terminal | |
| Sub-agents as `tasks` | Yes | No (`live.subagents` still counts them from hooks) | | |
| Compaction, turn end, interrupted, error | Yes | Turn end with duration; the others are written from Codex's public format and were not in the files read | | |
| `live`, approvals, `unread` | Yes | Yes (approvals where Codex's hooks hold them) | `live` only | `live` only |
