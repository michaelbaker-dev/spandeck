# The tools API

The station side of Tools for agents (`DECISIONS.md`, "Tools for agents"). This is the contract
a screen is built on. The code is `crates/core/src/toolcat.rs` (the catalogue and the runtime),
`crates/core/src/tooldoc.rs` (the document and the owner's changes to it) and
`crates/core/src/tools.rs` (what a station installs, its jobs, what a pane is given). The
command line is `crates/cli/src/tools_cmd.rs` (`spandeck tools`).

## The model in five lines

- **The tools document** is one document every station keeps (like the deck and look documents):
  which tools, for which stations, which version. It says what is wanted and installs nothing.
- **Each station acts for itself.** It reads the document when it starts and when the document
  changes, and installs what is for it and not there. No station installs on another.
- **A tool is `catalogue`** (spandeck installs it: today `playwright`) **or `custom`** (a command
  or an address of the owner's; spandeck installs nothing and only looks whether the command is
  there).
- **A pane gets a catalogue tool only where it is ready**: installed at a version, and started
  once. A custom tool is given wherever it is chosen, found or not, as an MCP server of Agent
  setup always was.
- **The MCP servers of Agent setup still work unchanged.** They are listed here as custom tools
  for the whole fleet with `from_setup: true`, and stay in the agent setup until the owner
  changes something only a tool has (its scope, the "without asking" tick).

## Rules that hold for every call

- **The owner's only.** Every `tools.*` call below is refused to an agent (a pane's token), to
  the read-only caller and to a station acting for itself (`denied`). The one exception is not
  for a screen: `tools.push`, a station offering its copy of the document to another.
- **Another station is reached as everywhere**: `peer.call {station, method, params}`, which
  that station takes as the owner's request passed on.
- **Which calls go where.**
  - Changes to the document (`tools.add`, `tools.remove`, `tools.scope`, `tools.set`,
    `tools.upgrade`, `tools.rollout`) and `tools.list`, `tools.catalogue`: call the station the
    page is served by. The document reaches the others by itself.
  - `tools.status`, `tools.install`, `tools.check`: per station. Ask each station by itself (a
    call each, side by side), so a slow station holds up only its own column.
- **Is the station new enough?** `station.info` has `"tools"` in `caps` and a `tools` object
  `{updated_ms, format}`. A station without the cap knows none of these calls: show "needs a
  newer spandeck" in its column and do not call it. It still gives its agents the MCP servers of
  Agent setup, as before.
- **Text is data.** `line`, `reason`, `says`, `last_line`, `output` and `command` hold what a
  program printed or a station composed from it: text, never HTML. Escape sequences and control
  characters other than newline are already taken out.
- **A change answers at once.** It waits for no install and for no other station. What happens
  next is in `tools.status`.
- **A second try.** `tools.status`, `tools.list`, `tools.catalogue`, `tools.check` and
  `tools.install` are safe to send again. A change to the document is not kept by its request
  id (its parameters can hold a custom tool's credentials): a second try whose first may have
  arrived is answered "could not confirm"; read `tools.list` to see whether it was done.
- **Errors** are `{code, message}` as everywhere; `message` is a sentence for the owner
  ("there is no station called m5 in this fleet").

## Events

On the deck's event stream, as every event. Both mean "ask again", and carry nothing to show:

| Event | When | `data` |
|---|---|---|
| `tools.updated` | The document changed on this station (here, or taken from another). | `{updated_ms}` |
| `tools.status` | A job on this station started or ended, or a registry answered. | `{tool, station}` or `{catalogue, station}` |

A running job's `last_line` changes without an event: while any row is `installing`, ask that
station's `tools.status` again every second or two.

## A tool, as the document holds it

Returned by `tools.list` (in `tools`) and by every change (in `tool`).

```
{
  "name": "playwright",          // what an agent sees the server called; letters, digits, - and _
  "title": "Playwright",         // the catalogue's name for it; a custom tool's own name
  "kind": "catalogue",           // or "custom"
  "catalogue": "playwright",     // catalogue tools: the entry's id. null for custom
  "command": null, "args": [], "env": {}, "url": null,   // custom tools: a command, or a URL
  "agents": [],                  // "claude", "codex", "opencode", "antigravity"; empty: all
  "fleet": true,                 // every station, including one that joins later
  "stations": [],                // when fleet is false: the stations it is for, by name
  "version": "0.0.85",           // catalogue tools: the version the fleet runs. null for custom
  "station_versions": {"spark2": "0.0.86"},   // stations that run another one
  "allow_without_asking": false,
  "restart": true,               // whether the last change of version restarts running panes
  "from_setup": false            // true: it is an MCP server of Agent setup (see above)
}
```

The version a station is to run is `station_versions[station]` if there is one, else `version`.

## `tools.catalogue`

`{}` → what spandeck can install, and with what.

```
{
  "station": "alpha",
  "tools": [{
    "id": "playwright", "name": "Playwright",
    "gives": "A browser the agent can drive: open pages, click, type, read the page, take screenshots",
    "package": "@playwright/mcp",
    "needs": "Node 18 or newer (spandeck brings its own)",
    "tested": "0.0.85",                 // the version spandeck was run with
    "args": ["--headless", "--isolated", "--browser", "chromium"],
    "source": "…",                      // where each fact was read, and when
    "latest": "0.0.86"                  // the newest this station was last told, or null
  }],
  "runtime": {"node": "24.22.3", "platform": "linux-arm64", "installed": false}
}
```

`runtime.platform` is null on a kind of machine spandeck has no Node for; a catalogue tool
cannot be installed there.

## `tools.list`

`{}` → the document, and after its tools the MCP servers of Agent setup.

```
{"station": "alpha", "updated_ms": 1795000000000, "updated_by": "you at alpha", "format": 1,
 "tools": [ <tool>, … ]}
```

## `tools.status`

`{}` → every tool of the document and every MCP server of Agent setup, as it stands **on the
station asked**. It reads that station's disk and waits for nothing else.

```
{
  "station": "alpha",
  "updated_ms": 1795000000000,      // of this station's copy of the document
  "checks_off": false,              // true: [agents] check_latest = false here
  "runtime": {"node": "24.22.3", "installed": true},
  "tools": [ <row>, … ]
}
```

A station whose copy of the document has not arrived yet has no row for a tool that was just
added: show the cell as waiting, and ask again.

### A row

| Field | Meaning |
|---|---|
| `name`, `title`, `kind`, `catalogue`, `from_setup` | As in the document. |
| `chosen` | Is this station in the tool's scope? |
| `state` | `not_chosen`, `installing`, `ready`, `failed`, `needs`. See below. |
| `line` | `failed`: the program's own line. `needs`: what this station lacks. Else null. |
| `wanted` | Catalogue: the version pinned for this station. |
| `version` | Catalogue: the version that is ready, when `state` is `ready` (it is then `wanted`). Else null. |
| `wired` | Is the tool given to a pane started now? |
| `wired_version` | Catalogue: the version a pane started now is given, or null. It differs from `wanted` while an upgrade is installing or after one failed: agents keep the version that works. Show it as drift. |
| `own_pin` | Catalogue: this station has a version of its own in `station_versions`. |
| `job` | Catalogue: the newest install job here, running or ended, or null. See below. |
| `latest` | Catalogue: is `wanted` the newest published? See below. |
| `on_disk` | Catalogue: the versions installed and ready on this station's disk, oldest first. |
| `allow_without_asking` | As in the document. |
| `panes` | Live panes on this station of the agents the tool is for. |
| `path`, `checked` | Custom: where the command was found (null: not found, or a URL); `checked` is false for a URL, which is not asked whether it answers. |

### `state`

| State | Catalogue tool | Custom tool |
|---|---|---|
| `not_chosen` | Not in scope here. Nothing is installed and nothing is given. `on_disk` may still list versions from when it was. | (the same) |
| `installing` | A job is running: see `job.step`, `job.command`, `job.last_line`. `job` can be null for a moment before the job starts. | never |
| `ready` | The pinned version is installed and was started once. | The command is on this station, or it is a URL. |
| `failed` | The install did not finish. `line` is the program's own line ("npm error 404 Not Found …"); `job.output` is what was printed. It is **not tried again by itself**: offer Retry (`tools.install`). | never |
| `needs` | It installed, and did not start: `line` is what the program said ("… error while loading shared libraries: libnspr4.so …"). Also: this station's spandeck does not know the catalogue entry ("it needs a newer spandeck"), or has no Node for this kind of machine. Retry after putting it right. | "there is no `npx` on alpha (in the folders spandeck looks in)". It is given to agents all the same (`wired: true`), as before. |

### `job`

The same shape and words as an agent program's job (`agents.status`).

```
{
  "id": "job-01…", "tool": "playwright",
  "action": "install",              // or "upgrade", "rollback"
  "state": "running",               // or "done"
  "step": "setup",                  // running: "runtime" (spandeck's Node), "package",
                                    // "setup" (what the tool fetches: its browser), "starting"
  "started_ms": 1795000000000, "ended_ms": null,
  "by": "you at alpha",             // who made the change this station acted on; "you" for a retry
  "command": "…",                   // what is run now, as the owner would type it
  "last_line": "| 50% of 195.7 MiB",
  "from": null, "to": "0.0.85",
  "outcome": null,                  // done: "installed", "upgraded", "rolled_back", "failed",
                                    //       "needs", "interrupted"
  "reason": null,                   // failed, needs, interrupted: why, in one line
  "says": null,                     // done: the result in words, ready to show (below)
  "output": null,                   // done: what the commands printed (its end, 6,000 characters)
  "restarted": [], "restart_when_idle": [],   // panes started again on the new version, by id
  "restart": true, "limit_secs": 900
}
```

`says`, word for word:

- `Installed 0.0.85`
- `Upgraded 0.0.84 → 0.0.85`
- `Rolled back 0.0.85 → 0.0.84`
- `Could not install: ` / `Could not upgrade: ` / `Could not roll back: ` and the program's own line
- `Needs something this station lacks: ` and the program's own line
- `Not known whether it finished: interrupted by a restart of spandeck on alpha: it is started again`

One job at a time per tool and station. A job is given 15 minutes (`limit_secs`; the station's
`agents.job_limit_secs` flag), then stopped and ended as `failed`. A job that was running when
spandeck stopped is ended as `interrupted` at the next start and run again. Grey a row's
buttons while `job.state` is `running`: the greying comes from the station, not the page.

### `latest`

`{"state": …, "version", "why", "checked_ms", "asked", "checking"}`, against `wanted`:

| `state` | Meaning |
|---|---|
| `latest` | Nothing newer is published. |
| `available` | `version` is newer: offer Upgrade. |
| `unknown` | Could not check: `why` ("no route to registry.npmjs.org"). |
| `not_checked` | Nobody has asked yet. |
| `off` | Checks are off on this station. Only `state` is present. |

`checking: true`: an answer is on its way; ask `tools.status` again shortly.

## `tools.check`

`{name?, again?}` → `{"station", "off": false, "asking": ["playwright"]}`

Asks the registry about one tool or every catalogue tool, on the station asked. An answer is
kept three hours (a failure one minute) and reused: `asking` is then empty. `again: true` asks
anew. It answers at once; the answers arrive in `tools.status` (`latest`). Send it when the
section opens and for "Check again", to each station: nothing is asked in the background. With
`[agents] check_latest = false` on a station it answers `off: true` and asks nothing.

## `tools.add`

```
{name?, catalogue?, version?, command?, args?, env?, url?, agents?, fleet?, stations?,
 allow_without_asking?}
```

| Parameter | Meaning |
|---|---|
| `catalogue` | A catalogue entry's id: the tool is installed by spandeck. Without it the tool is custom and needs `command` (with `args`, `env`) or `url`. |
| `name` | Default: the catalogue id. Required for a custom tool. Not `spandeck`, not `runtime`, and not the name of an MCP server of Agent setup. |
| `version` | Catalogue only. Default: the newest the registry publishes (asked now, at most 8 s, unless an answer under three hours old is held); if the registry does not answer or checks are off, the catalogue's `tested` version. |
| `fleet: true` **or** `stations: [names]` | Where. One of the two is required. A station name must be one the fleet knows. |
| `agents` | Default: all. |
| `allow_without_asking` | Default false. |

→ `{"station", "updated_ms", "tool": <tool>, "version_from": "caller" | "registry" | "catalogue" | null}`

Show `version_from: "catalogue"` to the owner: the version was not the registry's answer.

## `tools.remove`

`{name}` → `{"station", "removed": "playwright", "updated_ms"}` (or `"from_setup": true` when it
was an MCP server of Agent setup, which is then taken out of the agent setup).

No pane started from now is given it, on any station. Panes that are running keep what they
were started with. **Files are not deleted**: what a station installed stays under its
`tools/<name>/` folder, so adding the tool again is at once.

## `tools.scope`

`{name, fleet: true}` or `{name, stations: [names]}` → `{"station", "updated_ms", "tool",
"moved_from_setup"}`

Replaces the scope. A station that comes into scope installs the tool by itself; one that
leaves stops giving it to new panes and keeps its files. A pin of a station that leaves goes
with it. `moved_from_setup: true`: the tool was an MCP server of Agent setup and is now a tool
of this document; a station without the `tools` cap no longer gives it to its agents.

## `tools.set`

`{name, agents?, allow_without_asking?}` → as `tools.scope`.

`allow_without_asking` takes effect in panes started from now, and only where the agent's
program has a rule for one server's tools that spandeck already uses for its own:

| Program | What a ticked tool adds |
|---|---|
| Claude Code | `mcp__<name>` to the pane's `--allowedTools` |
| Codex | `-c mcp_servers.<name>.default_tools_approval_mode="approve"` |
| OpenCode | nothing: spandeck writes no such rule for OpenCode today |
| Antigravity | nothing: its allow rules are in the owner's own settings file, which spandeck changes only on the owner's `integration install` and never with a rule for a whole server |

Say so beside the tick for the last two.

## `tools.install`

`{name}` → try the install again **on the station asked**. Catalogue tools only.

| Answer | Meaning |
|---|---|
| `{"station", "tool", "started": true, "job": "job-…", "to": "0.0.85"}` | Started. |
| `{…, "started": false, "running": {id, action, tool, started_ms, by, state, to}}` | One is already running: nothing more was started. |
| `{…, "started": false, "ready": "0.0.85"}` | The pinned version is ready: nothing to do. |

Refused when the tool is not chosen for that station.

## `tools.upgrade`

`{name, station?, to?, restart?}` → `{"station", "updated_ms", "tool", "on": "spark2",
"to": "0.0.86", "restart": true}`

Sets **one station's** version: `station`, default the station that takes the call. `to`
defaults to the newest the registry publishes (as in `tools.add`; refused if it is not known:
"say which with `to`"). `to` may be an earlier version: that is a rollback, and it is at once
when that version is still on the station's disk (a station keeps the version it ran before).
A `to` equal to the fleet's version takes the station's own pin away.

`restart` (default true): after the version changed on a station, its idle panes of the agents
the tool is for restart into their conversations, and a pane in a turn or waiting on the owner
restarts when its turn is over, as after an agent program's update (`job.restarted`,
`job.restart_when_idle`). `restart: false`: it takes effect in panes started from now. A first
install restarts nothing.

## `tools.rollout`

`{name, to?, restart?}` → `{"station", "updated_ms", "tool", "to": "0.0.86", "restart": true}`

Sets the fleet's version and empties `station_versions`: every station in scope installs it by
itself. Without `to` it is the version the stations with a pin of their own run; refused when
none has one, or when they run several ("say which with `to`").

## What a pane's record says

`pane.created` in the ship's log (`log.query`) has, in `data`: `tools` (the names of the
servers the pane was started with, apart from spandeck's own) and `tools_without`
(`[{"tool", "why"}]`: tools chosen for the station that were not ready, for example "it is
still being installed on alpha").

## The command line

```
spandeck tools                                   the grid: tool by station, state and version
spandeck tools add NAME [--station X]… | --fleet [--version V] [--agent A]… [--allow]
                        [--catalogue ID] [--command PROGRAM --arg A… | --url URL]
spandeck tools scope NAME --fleet | --station X…
spandeck tools upgrade NAME [--station X] [--to V] [--no-restart]
spandeck tools rollout NAME [--to V] [--no-restart]
spandeck tools remove NAME
spandeck tools install NAME [--station X]        try again there
spandeck tools check                             ask the registry again, then the grid
```

`--json` as elsewhere; `spandeck tools --no-check` asks no registry. `tools add NAME` with
neither `--station` nor `--fleet` is for the station the command is run on. A NAME that is not
given `--command` or `--url` is taken as a catalogue entry's id.
