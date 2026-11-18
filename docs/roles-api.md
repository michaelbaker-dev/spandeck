# The roles API

The station side of Roles for agents (`DECISIONS.md`, "Roles for agents"). The code is
`crates/core/src/roledoc.rs` (the library, its document, filling and sending a prompt) and the
built-in prompts in `crates/core/src/roles/*.md`. The command line is
`crates/cli/src/roles_cmd.rs` (`spandeck roles`); the screen is `crates/web/assets/roles.js`.

## The model in six lines

- **A role** is what one kind of agent in a project is asked to be: a name, a title, a one-line
  summary, a prompt, a kind, and optional defaults. Its **prompt** is sent to an agent as the
  owner's instruction.
- **Seven roles are built in**: `builder`, `critic`, `user` (kind `standing`: on the team for the
  whole job) and `architect`, `performance`, `accessibility`, `security` (kind `visit`: one
  review, then done).
- **The role library is one document every station keeps** (`roles.json`, by the rules of the
  other synced documents). It holds only what the owner changed or added. A built-in role nobody
  changed is not in it and follows the release; one the owner changed is in it whole, marked
  `changed`, with the built-in text still given beside it.
- **A prompt comes from the library and from nowhere else.** A project's file
  (`.spandeck/project.toml`), which travels with a repository, names a role and cannot supply a
  prompt, a title or a budget.
- **A pane started with a role is sent it once**, as a message from the owner, when the role's
  name is in the library. A name the library does not have sends nothing.
- **An agent can read its own role again** (`role`), and nothing else about roles.

## A role

| Field | |
|---|---|
| `name` | Letters, digits, `-`, `_`; 32 at most. The key. |
| `title` | One line, 80 characters at most. |
| `summary` | One line, 200 at most. |
| `prompt` | Markdown, 16,384 bytes at most, not empty. Placeholders below. |
| `kind` | `standing` or `visit`. |
| `agent` | Optional. The program a pane with this role runs when the caller names none (`claude`, `codex`, `opencode`, `antigravity`). |
| `model`, `effort`, `permission_mode` | Optional. Laid over Agent setup's defaults for that program when a pane with this role is started. Letters, digits and `- _ . [ ]`. A permission mode Codex has no flag for is not passed to Codex. |
| `budget` | Optional. One line, 200 at most, for `{{budget}}`. |
| `changed_ms`, `changed_by` | When the entry was last saved and by whom ("you at spark2"). 0 and empty for a built-in role nobody changed. |

What `roles.list` and `roles.get` add to each role:

| Field | |
|---|---|
| `source` | `builtin`, `changed` (a built-in role the owner changed) or `own`. |
| `builtin` | True for the seven. |
| `changed` | True for `source: "changed"`. |
| `builtin_role` | Only when `changed`: the role as this release has it, to compare with. |

### Placeholders

Filled when the prompt is sent, and shown as written when it is viewed.

| | Becomes |
|---|---|
| `{{project}}` | The project's name. For a pane in no project: its folder's name. |
| `{{others}}` | A short paragraph naming the other agent panes of the project and the role of each: "The other agents here: the pane `critic` (role: critic, Codex) and the pane `user` (role: user, Claude Code). Reach them with spandeck's messages." With none: "You are working alone in this project." Both the running panes and those the project's file defines are named. |
| `{{budget}}` | The role's `budget`, or the one given when the pane was started (the pair recipe's `--budget`). With neither: "No budget was set. Say so when a stage is taking long." |

An unknown `{{…}}` is left as written. What is put in is never read for placeholders itself. A
pane's name or a role's name that is more than a name (anything but letters, digits, `-`, `_`,
and `.` in a pane's name) is not put into the text: a project's file names both.

## Rules that hold for every call

- **The owner's only.** Every `roles.*` call below is refused to an agent (a pane's token), to the
  read-only caller and to a station acting for itself (`denied`). `roles.push`, a station offering
  its copy of the document, is the one exception and is not for a screen.
- **Another station is reached as everywhere**: `peer.call {station, method, params}`, the
  owner's request passed on.
- **Which calls go where.** The library is the same everywhere: read and change it at the station
  the page is served by. `roles.send` goes to the station the pane is on.
- **Is the station new enough?** `station.info` has `"roles"` in `caps` and a `roles` object
  `{updated_ms, format}`. A station without the cap knows none of these calls, sends no pane a
  role's prompt, and is never offered the document.
- **Text is data** for whoever shows it: a prompt, a title and a summary are the owner's words
  and are shown as text, never as HTML.
- **Errors** are `{code, message}`; `message` is a sentence for the owner.

## Events

| Event | When | `data` |
|---|---|---|
| `roles.updated` | The library changed on this station (here, or taken from another). Ask `roles.list` again. | `{updated_ms}` |

In the ship's log: `roles.changed` (`{what: set|reset|remove, role}`) and `role.sent`
(`{name, role, text: "built-in"|"owner's", again, message}` with the pane).

## The calls

### `roles.list {}`

```json
{"station": "spark2", "updated_ms": 1795000000000, "updated_by": "you at spark2", "format": 1,
 "roles": [{"name": "builder", "title": "Builder", "summary": "…", "prompt": "# Role: builder\n…",
            "kind": "standing", "agent": null, "model": null, "effort": null,
            "permission_mode": null, "budget": "One round of review a stage…",
            "changed_ms": 0, "changed_by": "", "source": "builtin", "builtin": true,
            "changed": false}],
 "placeholders": [{"name": "{{project}}", "becomes": "the project's name"}],
 "limits": {"prompt_bytes": 16384, "name": 32, "title": 80, "line": 200}}
```

The built-in roles first, in a fixed order, then the owner's own by name.

### `roles.get {name, filled?, project?}`

`{station, updated_ms, role}`. With `filled: true` the role also has `filled`: its prompt with
the placeholders filled as for a pane of `project` (the project's agent panes are the others;
with no project, "this folder" and "working alone"). `not_found` for a name that is no role.

### `roles.set {name, role, was?}`

Create a role or change one. `role` holds the fields to set (`title`, `summary`, `prompt`,
`kind`, `agent`, `model`, `effort`, `permission_mode`, `budget`); a field it does not name stays
as it is. For a new role, `title` is the name and `kind` is `standing` unless given, and `prompt`
is needed. An empty word or `null` takes an optional field away.

`was`: the role's `changed_ms` as the caller read it (0 for a built-in role nobody changed, and
for a role that is to be new). When the role's `changed_ms` is no longer that, **nothing is
saved**:

```json
{"saved": false, "newer": true, "why": "this role was changed by you at m4-macbook after you opened it",
 "role": {"…the role as it now is…"}}
```

A screen keeps the owner's text and says so; sending it again with the new `changed_ms` as `was`
saves it over the other. Without `was` the save is made whatever the role is now.

Otherwise `{saved: true, role, updated_ms}`. A built-in role saved exactly as this release has it
is no longer kept: it is `source: "builtin"` again. `invalid` for a name, a length or a value
that is not allowed, with which.

### `roles.reset {name}`

A built-in role the owner changed is the built-in text again. `invalid` for a role of the owner's
own (it has no built-in text) and for a name that is no role.

### `roles.remove {name}`

A role of the owner's own is taken out. A pane that has it keeps the name and is sent nothing
from then. `invalid` for a built-in role: it can only be reset.

### `roles.send {pane}`

Send a pane its role's prompt again, filled as of now.

```json
{"pane": "p01…", "name": "critic", "sent": true, "role": "critic", "text": "built-in", "message": "m01…"}
```

`{sent: false, why}` when the pane has no role, runs no agent, has ended, or its role's name is
not in the library.

## When a prompt is sent without being asked

A pane is sent its role once, when it is started with one: `pane.spawn` with `role`, a
project's pane slot with a role (`project.open`), a helper started by `delegate`, and the pair
recipe. The message is from the owner and begins "Your role in this project, from the owner:";
then the filled prompt; for a pane of the pair recipe, the recipe's own line after it.

- It takes the road of every message: it is stored at once, and the pane is told of it by the
  rules of messages. It is not typed into a program's first question, and is held while the
  program is only just starting.
- **Once to a pane.** A pane restarted into its conversation, restored after the station was
  restarted, or reopened by its project into the conversation it had, is not sent it again. A pane
  started on a new conversation (a restart with `fresh`, a slot with no conversation to go back
  to) is.
- **A pair's panes are sent theirs by the recipe**, which waits for a brief; from then they are
  as any other pane.
- `pane.set {role}` changes a pane's role and sends nothing: `roles.send` does.

## The agent's call

`role {}`, on the agents' list and as the MCP tool `role`: `{role, text}`, the calling pane's
role filled as it would be sent now. `{role: null, text: "This pane has no role."}` without
one. An agent is answered for its own pane whatever it names.

## The command line

```
spandeck roles                              the list: name, kind, "changed" or "yours", title
spandeck roles show <name> [--filled [--project P]] [--builtin]
spandeck roles edit <name> [--file <path>|-] [--title …] [--summary …] [--kind standing|visit]
                           [--budget …] [--agent …] [--model …] [--effort …] [--permission-mode …]
spandeck roles new <name> [--copy <role>] [the same options]
spandeck roles reset <name>
spandeck roles remove <name>
spandeck roles send <pane>
spandeck pane new --role <name> [--agent …]
spandeck project new --recipe pair [--user [<agent>]] [--budget <minutes>]
```

`roles edit` and `roles new` read the prompt from standard input when it is piped in and no
other option was given. `--json` prints each call's answer.
