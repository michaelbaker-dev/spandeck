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

**One chat for every pane (16 Nov 2026; `DECISIONS.md`).** OpenCode's and Antigravity's own
conversations are read too (`crates/archive/src/stores.rs`): their panes answer
`source: "transcript"` with the same items as Claude Code's. What an agent printed on its
screen is served as parts to read (`parts`; `crates/core/src/reflow.rs`), and a turn's end as
a `notice`, so the screen's conversation is drawn by the same code as any other. Nothing was
added to the calls, and there is no `caps` entry: an answer says what it holds.

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
    "subagents": [{"kind": "Explore", "since_ms": 1794736851000, "id": "a3f9c1d2e4b5a6c70"}],
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
| `source` | `transcript`: the items are the agent's own conversation, read from its session file (Claude Code, Codex), its transcript (Antigravity) or its database (OpenCode). `screen`: they are made from the pane's terminal (below). A station from before 16 Nov 2026 sends no `source`: it is `transcript`. |
| `session_from` | How the session is known. `hooks`: the agent's hooks named it. `resume`: the pane was started into it (for OpenCode: under the id spandeck chose at launch). `files`: found in the program's own store, because no hook has named one (below). Absent with `source: "screen"`. |
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
| OpenCode, before anything was said in the pane | `OpenCode does not say which conversation it is in (it has no hooks), and no conversation of OpenCode's for this folder has appeared since the pane started (OpenCode begins one with its first message)` |
| Antigravity with no hook yet and no conversation since the pane started | `Antigravity has not said which session it is in yet (nothing has come from its hooks), and no conversation of Antigravity's for this folder has appeared since the pane started (Antigravity begins one with its first message)` |
| Antigravity whose hook named a conversation that has no transcript yet | `the transcript of the conversation Antigravity named is not there yet (Antigravity begins one with its first message)` |
| OpenCode or Antigravity, and another such pane in the same folder, or several conversations at once | The two reasons below, with "conversation" where they say "session file" |
| Claude Code before its first hook | `Claude Code has not said which session it is in yet (nothing has come from its hooks)` |
| Codex with no hook yet and no session file since the pane started (every Codex pane before its first message) | `Codex has not said which session it is in yet (nothing has come from its hooks), and no session file of Codex's for this folder has appeared since the pane started (Codex begins one with its first message)` |
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
  pane was started with. `screen`: what the pane printed after it. For a shell that is its
  rows, in the terminal's own layout (`text`: show it in a fixed-width font, never wrapped,
  scrolling sideways inside its own block). For an agent it is also given as parts to read
  (`parts`, below): draw those, and not `text`. `notice` with `what: "turn_ended"` and
  `duration_ms`: the station saw the agent's turn end here (its state went from working to
  idle or done, having worked for at least a second); the block printed in that turn is
  frozen before it. A turn the station did not see end has no notice.

| `screen` field | Meaning |
|---|---|
| `text` | The last 16 KB of the block, from a line's beginning. Control characters are dropped. An empty `text` in `updated` means the block came to nothing: stop showing it. |
| `lines` | How many lines the block has. |
| `more` | `text` is not all of it: `pane.conversation.detail` has the rest (a frozen block: its last 256 KB; the one being printed: its last 4,000 lines, read from the terminal then). |
| `open` | This is the block being printed now: it comes again in `updated` as it grows. When the next message is sent it is frozen: it comes once more without `open`, and never changes again. |
| `parts` | An agent's pane only (never a shell's): the same end of the block as parts to read, in order: `[{"prose": true, "text": "…"}, {"prose": false, "text": "…"}]`. `prose`: markdown source, to be drawn exactly as a `text` item is. Otherwise rows whose layout is their meaning (columns, a drawing): fixed-width, never wrapped, scrolling sideways inside its own block. An empty list: there is nothing to read (only the program's own frame and message box); show nothing for a frozen block. A caller that does not know `parts` shows `text`, as before. `pane.conversation.detail` gives `text` only: a block shown in full is shown as its rows. |
| `whole` | The block is the whole screen as it was at that moment, not what was printed since the message: the program draws its screen itself (a full-screen program, or one that redrew over the place the message was sent at), or the place has left the terminal's history. |

- **How `parts` are made** (`crates/core/src/reflow.rs`), from the rows and from nothing else:
  a row the terminal wrapped at its right edge goes on in the next (the screen model says
  which; nothing is guessed from the pane's width); a row made only of rule characters is the
  program's frame and is left out, and a row between two bars keeps what is between them; on
  a `whole` screen the frame the cursor is in is the program's message box and up to three
  rows under it are its status row, left out (with more rows under it, or a cursor on a row
  with no frame, they stay); the margin every row has is dropped; a bullet the program drew
  (`•`, `●`) is a list item; on a `whole` screen a row of a paragraph goes on from the row
  above it, as a paragraph's lines do in markdown, since the rows are the program's own
  wrapping (a heading, a list item, a quote, a table row and a code fence each still begin
  their own; in a log every line the program ended stays a line); and a block (rows with no
  empty row between them) in which a row has box-drawing characters or three blanks between
  two words is not prose. No word is changed. A `whole` screen that differs from the one
  frozen last only in what is left out (a status row that ticks) is not said again.
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
**the session the pane was started into** (`resume`; its file is found by the session's id);
and, for Codex, OpenCode and Antigravity, **a conversation found for the pane in the program's
own store** (`session_from: "files"`). The rule is one (`choose`, `crates/core/src/discover.rs`):

- it is one of the program's own conversations (not one it began itself, for a sub-agent), it
  records the pane's working folder, and it was begun at or after the moment the pane's
  program was started;
- no other pane on the station is in it;
- no other pane that runs the same program in that folder and has had no hook either was
  already started when it was begun;
- of those left, the newest, provided every older one was last written to before it began.
  Otherwise none, and the reason says why.

Where each program keeps them, and what is read (read-only, always):

| Program | Store | A conversation's folder, start and last write |
|---|---|---|
| Codex 0.169.0 | `<CODEX_HOME or ~/.codex>/sessions/YYYY/MM/DD/rollout-*.jsonl` | Its first line, `session_meta`: `id`, `cwd`, `timestamp`, and a `source` that is not an object (an object marks a sub-agent's). The file's modified time. |
| OpenCode 2.3.0 | One SQLite database, `<XDG_DATA_HOME or ~/.local/share>/opencode/opencode.db` | Table `session_v2`: `id`, `directory`, `time_created`, `time_updated`; not a row with a `parent_id` or a `time_archived`. |
| Antigravity CLI 1.5.9 | `~/.gemini/antigravity-cli/brain/<conversation id>/.system_generated/logs/transcript_full.jsonl` (else `transcript.jsonl`, the same with long fields cut at 4 KB) | `conversation_summaries.db` beside `brain/`: `conversation_id`, `workspace_uris` (a JSON list of `file://` addresses), no `parent_conversation_id`. Its start is its transcript's first line (`created_at`, to the second: a conversation begun in the same second as the pane, before it, is not taken); its last write is the transcript's modified time. |

- **Antigravity's hooks name the conversation** (`conversationId`) and its transcript
  (`transcriptPath`). The transcript read is the one found by the id in the layout above, else
  the file the hook named; the whole one where there is one; and once lines of one file were
  read the session stays with that file.
- **OpenCode has no hooks.** spandeck starts it under a session id of its own choosing
  (`--session ses_…`), which OpenCode leaves with `/new`. So its database is looked through
  as for a pane that named none, and what is found wins (`files`); when nothing is found (a
  second OpenCode pane in the folder, say) the id it was started under counts, if OpenCode
  has such a conversation (`resume`).
- **OpenCode's rows become lines.** Table `session_message` (`session_id`, `seq`, `type`,
  `time_created`, `data`: a JSON object) has a row for each message. A row is rewritten while
  its message is being made, so a row is taken only once it is settled (`time.completed` or
  an `error`, and no tool part `pending` or `running`; or a later row exists), in `seq`
  order, and nothing after an unsettled row is taken before it. Each becomes one line of the
  archive: `{type, id, seq, ts, data}` with OpenCode's `data` untouched, plus `turn_ms` and
  `error_said` on an `idle` row. So a reply appears whole, when it is finished. The database
  is opened only when it or its write-ahead log has changed (or after half a minute), never
  written and never checkpointed. `archive.restore` refuses such a session: there is no file
  to put back.

It is looked for when the pane's conversation is asked for: at most twice a second while none
is found, every two seconds after, so a new session begun with `/new` is followed. It is only
read: nothing is ever answered or sent to a session, only to a pane. Claude Code is given its
hooks at launch and is never looked for.

**These stores hold the owner's conversations** and are the programs' own. They are read where
a session file is read and go where its lines go: the station's archive, raw, served to the
owner's browser only; `recall` and other stations see the scrubbed copy, as for any session.
No fixture, test or log holds anything from a real one: the fixtures are made-up words in the
layouts above.

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
| `tool` | `name`, `summary`, `status`, `duration_ms?`, `added?`, `removed?`, `background?`, `sent?` | `summary` is one line, 200 characters at most: the command, the file, the pattern, the address. May be empty. `added` / `removed`: lines an edit changed. `background`: started and left running. `sent`: files the call sent to the owner (below). |
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
| | `away_summary`: `text` | Claude Code's "while you were away" (its `system` record of the subtype `away_summary`). It ends with the program's own hint, "(disable recaps in /config)". |
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
| `subagents` | `[{kind, since_ms, id?}]`: the agent's own sub-agents running now, as its hooks say (`id`: the program's own id for one, when its hook gave it; 20 Nov 2026). Non-empty while sub-agents run. The rows to show are in `tasks`. |
| `paused`, `exited`, `resumed`, `hooks_seen` | As in the pane list. `resumed`: the program was started into a saved conversation. |
| `trust` | `null`, or the pane's folder-trust question (16 Nov 2026): `{chosen, asks, answered, folder}`. `asks` is `null` or `{since_ms, button, why}`: the program asks whether to trust the folder and nothing was pressed (`why`: `not_chosen`, `own_choice_is_no`, `not_the_known_screen`, `switched_off`); with `button: true` show "Trust this folder", which is `pane.trust_folder {pane, remember}` (the owner's only; it moves the program's marker to the trusting choice if need be, checks, and confirms). `answered` is `null` or `{at_ms, how, by}` (`how`: `standing`, by spandeck on the owner's choice; `tap`). `chosen` is `null` or `{by, why}`: the choice the pane was started with. The same `trust` is in `pane.list` and `pane.get`. A station from before this has no `trust`. |
| `hooks_trust` | In `pane.list` and `pane.get` only (16 Nov 2026): `null`, or the pane's review of hooks (Codex's "Hooks need review"): `{chosen, asks, answered}` shaped as in `trust`. `asks.why` is `not_only_spandecks` (the hooks may not all be spandeck's: `button` is false and `asks.says`, also the pane's `needs_reason`, says which source was found), or `not_chosen`, `you_typed`, `not_the_known_screen`, `marker_did_not_move`; with `button: true` show "Trust spandeck's hooks", which is `pane.trust_hooks {pane}` (the owner's only). A station from before this has no `hooks_trust`. |
| `stuck` | `null`, or (20 Nov 2026) why the station says the agent may be stuck, while the pane is `needs_you` for that reason: `{kind, says}`. `kind` is `idle_turns` (so many turns in a row each ran five or more tools and changed no file) or `same_failure` (one command failed so many times in a row); `says` is what was counted, in words. Nothing waits at a prompt: a view shows a notice, not the terminal's rows and keys, with "It is fine" (`pane.stuck_fine {pane}`, the owner's alone), after which the pane is not `needs_you` and that kind is quiet for it until its session changes or a file is changed. Also in `pane.list` and `pane.get`. |
| `held` | `null`, or why the agent takes no mail until the owner releases it: `{stopped, restored, mail}`. `stopped` is `null` or `{at_ms, by, reached}`: the owner (or a "stop every agent") stopped it; `reached: false` means the program was not reading and the stop did not reach it ("use Kill"). `restored` is `null` or `{at_ms, why}`: the station started it again after a restart (`why`: `crash`, `stop`, `upgrade`). `mail` is how many messages wait. Show it with a Release button: `pane.release {pane}` (the owner's only; `{all: true}` for every pane on the station), answered `{ok, pane, released, still_paused, saved}`. Sending the pane anything (`pane.send_text`, `pane.paste`, `pane.input`) releases it too. `paused` is apart: Release never changes it. The same `held` is in `pane.list` and `pane.get`. A station from before this has no `held`. |
| `approvals` | Requests held for the owner for this pane, oldest first. Each: `{kind: "approval", id: "approval:<approval id>", approval, what, summary, tool, ts_ms, item}`. `approval` is the id `approval.answer {id, allow, reason?}` takes, as today. `what` is the kind of request (`permission`, `route`). `item` is the `id` of the `tool` item the request is about, or `null`: draw the card anyway. It is set only when the station is sure which call it is: the call with the id the hook gave, else the newest running call of that tool with the same command or file. It can name an item on a line after this answer's `cursor`, which the next answer brings. They are not in `items`: they are not in the session file. |
| `questions` | A question or a plan the station holds for the owner for this pane, oldest first (eight at most). Below. A station from before 16 Nov 2026 has no such field. |
| `tasks` | The agent's sub-agents, what else it has running beside its turn, and what of it is over (17 Nov 2026; a sub-agent's row says more since 20 Nov 2026). Below. A station from before has no such field: count `subagents`. |
| `model` | `null` for a shell; else what the model chip shows and offers (17 Nov 2026). Below. |
| `cleared` | The ids of the notes of this conversation the owner cleared (`pane.notes.clear`, below): leave those items out. |
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

### `live.tasks`: sub-agents and background tasks

```json
"tasks": {
  "running": [
    {"id": "21.0", "kind": "agent", "name": "Explore", "title": "Look for other tally scripts",
     "status": "running", "state": "working", "background": true, "started_ms": 1794736851000,
     "transcript": true, "depth": 1, "current": true, "now": "Bash · grep -rn tally scripts/"},
    {"id": "sub:a3f9c1d2e4b5a6c70", "kind": "agent", "name": "general-purpose", "title": "Read tally.py",
     "status": "running", "state": "working", "background": false, "started_ms": 1794736861000,
     "transcript": true, "parent": "21.0", "depth": 2, "current": true, "now": "Read · scripts/tally.py"},
    {"id": "30.0", "kind": "shell", "title": "cargo watch -x test", "status": "running",
     "background": true, "started_ms": 1794736900000, "transcript": false},
    {"id": "pane:p_01jz", "kind": "helper", "name": "builder-3f2a", "title": "build the importer",
     "pane": "p_01jz", "status": "running", "state": "working", "background": true,
     "started_ms": 1794736700000, "can_stop": true}
  ],
  "finished": [ {"id": "12.0", "kind": "agent", "status": "completed", "ended_ms": 1794736800000, "...": "..."} ],
  "finished_total": 152
}
```

| Field | Meaning |
|---|---|
| `running` | Everything that runs, oldest first (50 at most). Its length is the "N running tasks" of the view. |
| `finished`, `finished_total` | The 30 newest that are over, newest first, and how many there are in all (the station keeps 200 a session). |
| `id` | For a task of the agent's own: the `id` of the item of the call that started it. `pane:<pane id>` for a helper pane; `child:<id>` for a conversation (or thread) the program began itself; `sub:<id>` for a sub-agent that a sub-agent of Claude Code's began; `hook:<id>` for one only the agent's hooks tell of. Opaque: give it back as `task`. |
| `kind` | `agent` (a sub-agent), `shell` (a command left running), `helper` (a pane the agent started through spandeck: `pane` is its id, `name` its name). |
| `name`, `title` | A sub-agent's kind (or role, or name: the program's own word), and what it was asked or the command, in one line (200 characters). Either may be empty: **a row never says more than its program did.** With both empty a view says only that a sub-agent is running. |
| `state` | An `agent` row (20 Nov 2026): `working`, `done`, `failed` or `stopped`, made from `status` (`stopped`: interrupted, denied, lost, killed). For a helper it is its pane's state, as before. A station from before has none: use `status`. |
| `parent`, `depth` | An `agent` row: `depth` is 1 for a sub-agent of the pane's agent, 2 for one that a sub-agent began, and so on; `parent` is then the `id` of the row it was begun under (absent at depth 1). A parent may be in the other list, or cut from it. |
| `current` | An `agent` row: it is at work, or it is of the turn now (begun since the last message someone sent). **The view's chip counts the `current` rows and no others**: a finished sub-agent leaves the chip with the next message. |
| `now` | A working `agent` row: its latest step in one line (100 characters), from the end of its own record: `<tool> · <what>` ("Bash · cargo test"), `Thinking`, or the first words of what it wrote. Absent where no record of its own was read. |
| `said` | A finished `agent` row: the first line of the last thing it wrote (or of what the program kept as its output). Absent where none was read. |
| `from` | `hooks`: the row is what the agent's hooks said and nothing else (a kind and since when). For a `child:` row: the program (`codex`, `opencode`, `antigravity`). |
| `status` | `running`; for a task left in the background, the program's own word when it reported it over (`completed`, `failed`, …; `done` when it gave none); for a sub-agent that was waited for, its result's (`ok`, `error`, `denied`, `interrupted`); `lost`: the file has it as running, and the pane's program has ended or was started again since it began; for a helper its pane's state, or `ended`. |
| `started_ms`, `ended_ms` | When the records say; on the station's clock for a helper. Absent when a record carries no time. |
| `transcript` | The sub-agent has a conversation of its own that can be read: `pane.conversation.detail {pane, task: <id>, session}`, below. |
| `can_stop` | A helper only: its turn can be stopped with `pane.stop {pane: <its pane>}`. No other task has a stop: an agent's own sub-agents and commands stop with its turn. |
| `sure` | `false`: the program's store does not say whether this one still runs (an Antigravity store with no `status`; a Codex thread whose file ends with no mark of its turn); it is `running` while its record was written to in the last fifteen seconds. |

`finished` has the `current` sub-agents first, then the newest of the rest.

Where the rows come from, per program (20 Nov 2026; what was seen and what was not is in
DECISIONS.md, "Sub-agents are shown in a drawer of the agent's pane, for every program"):

- **Claude Code**: its session file (a sub-agent: its `Agent`/`Task` call, in the background
  when the result says `async_launched`/`isAsync`, over with its `<task-notification>`, tied
  by `tool-use-id` or `task-id`; a command: a call whose result has `backgroundTaskId`), and
  the files it keeps beside the session for each sub-agent: `subagents/agent-<id>.meta.json`
  (`agentType`, `description`, `toolUseId`, `spawnDepth`, `parentAgentId`, `finished`) ties a
  sub-agent to its call before any result is written, and is the only word of one that a
  sub-agent began (`sub:<id>`); `now` and `said` are from the end of `agent-<id>.jsonl`.
- **Codex**: the threads whose own file names this conversation as the one they were begun
  under (`session_meta.parent_thread_id`), and theirs; at work between `task_started` and
  `task_complete`/`turn_aborted` at the end of that file. Its reviewer of approvals
  (`thread_source: guardian_review`) is not a row.
- **OpenCode**: `session_v2` rows with this `parent_id` (`agent`, `time_busy`, `time_idle`,
  `idle_outcome`), with what its `job` table says of each.
- **Antigravity**: `conversation_summaries` rows with this `parent_conversation_id`
  (`status`, `agent_name`, `killed`); `title` is the role the parent gave it.
- **Every program with hooks**: a sub-agent its hooks report and no record names is a
  `hook:` row (`from: "hooks"`), and one they said is over stays as a finished row.
- **Every agent**: the panes whose `parent_pane` is this pane.

A store is asked, and a folder listed, at most every two seconds; the end of a record is read
again only when it has grown, at most once a second, for the first 24 sub-agents. A view of a
station from before 20 Nov 2026 shows `subagents` as rows when `running` has no `agent`.

**A sub-agent's own conversation: `pane.conversation.detail {pane, task, session?}`** answers
`{pane, session, task, items, cut}`: `items` are items as everywhere (the last 200), read from
the sub-agent's own record (Claude Code: the last 2 MB of
`<session file without .jsonl>/subagents/agent-<agentId>.jsonl`; Codex: the last 2 MB of the
thread's file; OpenCode and Antigravity: the child conversation's store; a `hook:` row: the
file its hook named, only where that is one of the program's own), when asked for and kept
nowhere. `cut`: its beginning is not here. There is no detail of an item of it and no watch:
ask again when its row's `now` or `status` changes. **It is to read only**: no method sends a
sub-agent anything. `not_found`: no such task, or it has no record to read (`transcript` is
false).

### `live.model`: the model, and how hard it thinks

```json
"model": {"model": "claude-fable-5-2", "effort": "medium",
  "models": [{"id": "fable", "name": "Fable", "says": "For your hardest and longest-running tasks"}],
  "efforts": ["low", "medium", "high", "xhigh", "max"],
  "can_set": true, "picker": null,
  "note": "Claude Code also keeps a model chosen here as the default for new sessions.",
  "source": "code.claude.com/docs/en/model-config.md and commands.md, read 16 Nov 2026"}
```

| Field | Meaning |
|---|---|
| `model` | The model behind the agent's latest reply, from its session data (as `usage.model` in the pane list), or `null`. |
| `effort` | What the agent's hooks last said (Claude Code: `effort.level`), or `null`. |
| `models` | What the station's adapter lists for this program (`crates/agents/src/models.rs`): never the whole truth, so always offer a field for another name. May be empty. |
| `efforts` | The levels `pane.model {effort}` takes; empty when the program has no such typed command. |
| `can_set` | `pane.model {model}` types a command. `false`: the program chooses its model only in a picker. |
| `picker` | The command that opens that picker in its terminal (`agents.command {pane, command}`, then show the terminal), or `null`. Never send keys into a picker. |

| Program | Model | Effort | Read (16 Nov 2026) |
|---|---|---|---|
| Claude Code | `/model <alias or name>` | `/effort <level>` | code.claude.com/docs: model-config, commands |
| Codex 0.169.0 | `/model <model>` | `/model --reasoning=<level>` | its commands page; `codex-rs/tui` |
| Antigravity | picker (`/model`) | `/effort <level>` | antigravity.google/docs/cli |
| OpenCode | picker (`/models`) | none | opencode.ai/docs/tui |

### `live.prompt`: a question of the program's own

`null`, or `{kind: "model_switch", name, on_yes, since_ms}`: Claude Code's "Switch model?" is
on the pane's screen, read exactly (17 Nov 2026; `crates/core/src/modelswitch.rs`). `name` is
the model it would switch to, as the program names it; `on_yes`: its marker is on "Yes", so
Enter (`pane.input {data: "\r"}`) switches and Esc goes back. The pane's state is
`needs_you` and its `needs_reason` begins `asks whether to switch model to`. The same
`prompt` is in `pane.list` and `pane.get`. When the question follows `pane.model`'s own
command the station answers it and no `prompt` is ever set.

**A list the program draws to choose from** (17 Nov 2026; `crates/agents/src/picker.rs`,
`crates/core/src/picker.rs`). `prompt` is also
`{kind: "picker" | "screen", name, since_ms, session_key, picker}`:

| Field | Meaning |
|---|---|
| `kind` | `picker`: a list read whole from the screen that grants nothing: draw its rows; a row is chosen with `pane.pick`. `screen`: the program shows something with key names under it that is a grant (trust, a permission, a sign-in: `picker.class == "grants"`) or has nothing marked (`choices` empty): show the screen itself (`pane.text`) with keys (`pane.input`), never a row. |
| `name` | Its title. The pane is `needs_you`, with `needs_reason` `shows a list to choose from: <title>` or `shows something in its terminal: <title>`. |
| `picker.title`, `about`, `tabs` | The first row of words above the list, the rows under it, and the title row's parts when it is a row of tabs. |
| `picker.choices[]` | `{text, detail?, marked, checked?, number?, current?, submenu?}`: the row's words; its second column or the words under it; the program's marker is on it; ticked (where `multi`); the program's own number; the program marks it as in use (`✔`); it opens another list (`▸`). Exactly one is `marked`. Only what the screen shows now: 40 at most. |
| `picker.multi`, `more_above`, `more_below` | Several may be ticked; the program says its list goes on (send arrow keys with `pane.input` and the next look has the rows). |
| `picker.free_text` | `null`, or the words in its field to type a filter in: type through with `pane.input`, a letter at a time. |
| `picker.footer`, `hint` | Rows between the list and the key names, and the key names as the program wrote them. |
| `session_key` | Its key names say `s` chooses for the session alone (Claude Code's `/model`). |

A `screen` is also what the pane says when nothing can be read and the program is known
not to be at its ordinary wait for a message (Antigravity: something under its message box
but its status row; OpenCode: a window whose title row ends with `esc`), while it is not in
a turn and the screen is still (and for Claude Code and Codex: the pane has shown its
ordinary wait for a message since its program started, and shows something else in its
place; in a turn, only after the screen has stood still for ten seconds): `picker.choices` is empty, `picker.footer` holds the
screen's last rows (16 at most), and `name` is `its own screen`. `pane.pick {pane, hide:
true}` hides what is shown now while the screen is exactly that.

Read by structure, not by words: the screen ends with a row of key names; above it rows in
one column of which exactly one carries the program's marker (`❯`, `›`), set in from the
edge. It is said once it is read the same at two looks (half a second), and cleared when it
is gone. Known from real screens: Claude Code 2.1.326's `/model`, `/config`, `/resume` and
`/permissions`, Codex 0.169.0's `/model` and Antigravity 1.5.9's `/model` (their marker is
at the left edge: read as a list's only with choices straight above or below it and key
names under them). OpenCode 2.3.0's `/models` has no marker a row carries: a `screen`.

## `pane.pick`

The owner's only (an agent, a station for itself and the read-only caller are refused).

```
{pane, text, key?}   →   {"ok": true, "picked": true}
                         {"ok": false, "picked": false, "why": "the list changed, or its marker did not reach that row: nothing was confirmed. Look again"}
```

`text` is a choice's `text` as `prompt.picker` gave it; it must be one row of the list on
the screen now. The station sends the arrow keys that take the program's marker there,
reads the screen again, and presses the key **only when the marker is on the row with the
same words and the list is the same list**; else nothing is confirmed. `key`: `enter` (the
default), `space` (tick it; only where `multi`), `session` (`s`; only where `session_key`).
Never for a `screen`. Keys only, never pasted text.

`pane.model {pane, model, session_only: true}` (Claude Code): types `/model`, waits for its
list, takes the marker to the model's row ("opus" is the row that begins "Opus") and
presses `s`. `{session_only: true, picked}`; or `ok: false` with `why` (and `closed: true`
when the model is not a row of the list, which is then closed with Esc).
`live.model.only_session` says the program has such a list.

**While a question of the program's own is on a pane's screen** (this one, `trust.asks`,
`hooks_trust.asks`, or a `picker`/`screen` prompt), `pane.paste`, `pane.send_text`, `agents.command` and `pane.model` type
nothing: the first two answer `invalid` ("not sent: … is asking … in this pane"), the others
`sent: false` with `why`. Keep what was written and say why. `pane.input` is not refused.

## `pane.model`

The owner's only (an agent, a station for itself and the read-only caller are refused).

```
{pane, model: "opus"}      or      {pane, effort: "high"}
→ {"ok": true, "sent": true, "command": "/model opus"}
→ {"ok": false, "sent": false, "busy": true, "why": "in the middle of a turn", "command": "/model opus"}
```

The station types the program's own command into the pane, with Enter, as `agents.command`
does and through it: only while the agent is not in the middle of a turn. Claude Code may
then ask "Switch model?": following this command, the station answers it "Yes" once
(`live.prompt`, above). `live.model.saves` says what the typed command changes beyond the
pane (Claude Code saves it as the default for new sessions): say it before calling.

`{pane, model | effort, only_pane: true}` types nothing: the pane's program is started again
with its own flags (`live.model.only_pane.model` / `.effort` say whether it has them; Claude
Code: `--model`, `--effort`) and resumes its conversation, as `pane.restart` does. Answered
`{"ok": true, "restarted": true, "args": ["--model", "opus"]}`; `busy` in the middle of a
turn; `invalid` for a pane with no saved conversation or a program without the flags. The
flags stay with the pane (`only_pane.args`).

**Which word on the model holds: the last one, in the conversation's own order** (17 Nov
2026; `model_now`, `crates/core/src/conversation.rs`). Three things say what a pane's model
is, and `live.model` gives at most one of `set` and `chosen` beside `model`:

| Field | What says it | Holds |
|---|---|---|
| `model` | The last reply that names the model that wrote it (the agent's session data). Always given when known. | When neither of the two below does. |
| `set` | The program's own name for a model a command of its own set ("Sonnet 5.5"), from what it printed: Claude Code's "Set model to `<name>` …". | While no reply that names its model has been written **after** those words. |
| `chosen` | The model the owner chose for this pane alone (`only_pane`): the value of the flag its program was started with ("opus"). | From the moment the pane is back, while no reply that names its model has been written **after** it was started again. |

- A reply written **after** `set` or `chosen` is the truth and they are no longer given; one
  written **before** does not count against them. So a command typed before the session's
  first reply is overtaken by that reply, and one typed after the last reply holds until the
  next.
- In one session the order is the lines' of its file. When the program began another
  session on being started again, it is the records' own times; a reply with no time is
  taken to be the newer, so nothing is claimed that a reply may have overtaken.
- Of `set` and `chosen`, the later holds.
- Both are kept with the pane through `pane.restart` and "Only this pane".
- Show `chosen` or `set` in place of `model` when present, as not yet answered on.
- `effort` is whichever spoke last of the agent's hooks, what the program printed for an
  effort command, and the flag the pane was started with; `effort_from` says which
  (`hooks`, `said`, `chosen`). `null` until one of them has spoken: a pane just started has
  none until its first hook that carries one. `model` is one
word (letters, digits and `. _ : / @ [ ] -`, 120 at most); `effort` is one of `efforts`.
`invalid`: anything else, both at once, a shell, or a program that takes no such command
(the message names its picker). Nothing says the program took it: `live.model` follows
what the agent then reports.

## `pane.notes.clear`

The owner's only.

```
{pane, session, item}          hide this item for the owner
{pane, session, restore: true} bring every cleared one back
→ {"pane": "p_01jx", "session": "…", "cleared": ["27.0"]}
```

`session` is the answer's `session`; `item` an item's `id`. Kept at the pane's station (200 a
pane; a clearing made in another session of the pane counts for nothing; it goes when the
pane is closed), so it is the same on every device. Nothing is removed from the session file
or the archive, and the item is still served: `live.cleared` says which to leave out. Meant
for notes: a `notice` whose `what` is `away_summary`, `info`, `task`, `command_output` or
`summary`.

## `question.answer`

The owner's only: an agent's token is refused (`denied`), for its own pane too, and so are a
station's own call and the read-only caller. For a pane on another station:
`peer.call {station, method: "question.answer", params}`.

```
{id, answers: [{picked?: [n, …], other?: "text"}, …]}     a question
{id, approve: true}                                       a plan: approve it
{id, approve: false, text?: "what to change"}             a plan: keep planning
{id, release: true}                                       either: answer in the terminal instead
{id, cancel: true}                                        a question: the owner declines to answer
```

| Parameter | Meaning |
|---|---|
| `id` | The `id` from `live.questions`. |
| `answers` | One entry for each question, in order. `picked`: places in that question's `options` (from 0). `other`: an answer of the owner's own, 4,000 characters at most. A question with `multi: false` takes exactly one of the two, and one place; one with `multi: true` takes any number of places and, beside or instead, `other`. Every question must have an answer. |
| `approve`, `text` | For a plan. `text`: a note for the agent, sent only with `approve: false`, 4,000 characters at most. |
| `release` | Let it go: the hook says nothing and the agent shows its own prompt in the terminal at once. Nothing is answered. |
| `cancel` | A question only (17 Nov 2026): the call is refused, and the agent is told "The owner saw this question in spandeck and chose not to answer it. Do not ask it again as it is: go on as you judge best with what you know, or stop and say what you need." The item's `status` becomes `dismissed`. |

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

## Files with a message: `pane.attach.put`, `pane.attach.get`, `pane.attach.remove`

The owner's only (17 Nov 2026; `crates/core/src/attach.rs`). For a pane on another station:
`peer.call`, as every pane call.

```
pane.attach.put {pane, name, data_b64, last?}            the first piece  →  {id, name, size, image, whole}
pane.attach.put {pane, id, data_b64, last?}              the next ones
pane.attach.remove {pane, id}                            one that was not sent
pane.attach.get {pane, id, offset?}                      →  {id, name, size, image, data_b64, next}
pane.paste {pane, text, enter: true, files: [id, …]}     the message, with its files
```

- A piece is at most 256 KB of bytes (send them one after another); a file 64 MB; ten files
  with a message; `last: true` closes a file. `name` is the device's: shown as text, never
  used as a path. `image`: its name ends as a picture's does.
- `pane.paste` with `files` types one message: `text`, then for each file a line
  `Attached file: <path on the pane's station>`. `text` may be empty. `invalid` when a file
  is not whole or not there: nothing is typed.
- A `user` item whose message went with files has `attachments: [{id, name, size, image}]`,
  and its `text` is the owner's words without those lines. Fetch a picture with
  `pane.attach.get`, piece by piece (`next` is the next `offset`, or `null`).
- Kept under the station's state for the pane (200 files), removed when the pane is closed.

## What an agent made: `media`, `pane.media.get`, `pane.media.attach`

17 Nov 2026; `crates/core/src/made.rs`. A `text` item (and a `screen` item, and a `tool`
item by its `summary`) that names a picture or a video file on the pane's station has

```json
"media": [{"id": "b2158dfd33ca282b79b0", "name": "heron.png", "size": 3312640, "kind": "image",
           "w": 1672, "h": 941, "path": "/home/ada/.codex/generated_images/5b1e/exec-77aa.png"}],
"media_more": 2
```

- `kind` is `image` or `video`, by the file's first bytes. `w`, `h`: a PNG's size. `path`:
  where it is, to copy as text; never to be sent back as a way to read it. `media_more`:
  how many more the item names than the eight shown.
- `pane.media.get {pane, id, offset?}` → `{id, name, size, kind, data_b64, next}`: pieces of
  256 KB, as `pane.attach.get`. `not_found`: no such id for this pane, or "that file is not
  there any more".
- `pane.media.attach {pane, id}` → `{id, name, size, image}`: the file is made one of the
  owner's next message to this pane, at the station; use the `id` in `pane.paste`'s `files`.
- Both are the owner's only. The item's own `text` is unchanged.

## Files an agent sent: `sent`

21 Nov 2026; `crates/archive/src/conversation.rs`. A `tool` item of Claude Code's
`SendUserFile` (its input: `{"files": [path, ...], "caption"}`) has

```json
"sent": {"caption": "The film, and a contact sheet.",
         "files": [{"path": "/home/ada/film/film_720p.mp4", "name": "film_720p.mp4"}],
         "more": 3}
```

- `files`: twelve at the most; `more` is how many more the call named. `path` is as the
  agent wrote it, or under the folder the record says it was in when it was not whole.
  `caption` is cut at 2,000 characters and absent when empty. The item's `summary` is the names.
- Every word of it is the agent's text: show it as text. The station has not looked at the
  files: one may be gone, or not what its name says. A deck asks for each as the owner, with
  the `files.` methods on the pane's station (`files.stat`, and `files.ticket` with `inline`
  for a preview), which decide what it is by its first bytes.
- A station that sends it says `sent_files` in `caps`. One that does not sends the same
  step with no `sent`, and it is shown as any tool step.

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

| | Claude Code | Codex | OpenCode | Antigravity | Shell |
|---|---|---|---|---|---|
| Conversation | Yes | Yes, read from 0.169.0 files; named by its hooks, else found among its files. Before its first message it has no session: the screen | Yes, read from its database (2.3.0), a message once it is finished; found for the pane, else by the id it was started under. Before its first message: the screen | Yes, read from its transcript (1.5.9); named by its hooks, else found for the pane. Before its first message: the screen | From the screen (`screen: true`); else the reason |
| Title | Yes | No | No | No | |
| Tool status | `ok` / `error` / `denied` / `interrupted` | `done` (the file does not say) | `ok` / `error` by the exit code where there is one, else `done`; `error` | `done` / `error` | |
| Tool duration | Yes | Yes, from the times of the two records | Yes | No | |
| Tool input and output | Yes | Yes | Yes | Yes (the output without the two lines of times the program puts before it) | |
| Diff of an edit | Yes | No | No | No | |
| Thinking duration | Yes | No | Yes | No | |
| Question, plan | Shown, with the answer once given; held for the owner and answered with `question.answer` (sessions started after 16 Nov 2026) | Not in its files; what Codex asks is in its terminal | The terminal | The terminal | |
| Sub-agents: `tasks` items in the conversation | Yes | No | No | No | |
| Sub-agents: rows of `live.tasks`, each with its own conversation | Yes: its call, its `meta.json`, its own file; nested ones | Yes: the threads begun under this one (not its reviewer); else what its hooks report | Yes: child conversations, with its `job` table | Yes: child conversations, with `status`, `agent_name`, `killed` | |
| Compaction, turn end, interrupted, error | Yes | Turn end with duration; the others are written from Codex's public format and were not in the files read | Turn end with duration (`idle`, `succeeded`); an error (a failed message, or a failed `idle` whose message did not say why) | Turn end, with no duration: an answer that calls no tool is the last of its turn (the program's own hooks guide: a turn stops with `NO_TOOL_CALL`); an error a step carries | From the screen: a turn's end when the station saw one |
| What is left out | | Its notes to itself | Reasoning is a length of time, never its words | The program's wrapping of what the owner wrote (`<USER_REQUEST>`), its notes beside it (the time, a setting changed), and thinking's words | |
| `live`, approvals, `unread` | Yes | Yes (approvals where Codex's hooks hold them) | `live` and `unread` (no hooks: its state is read from its screen, approvals are answered in its terminal) | `live` and `unread` (its hooks say working and done; it has no hook that holds an approval) | `live` only |
