# spandeck decisions

Read this before starting any task. It is a condensed copy of the Decisions tab of the
spec doc (https://claude.ai/artifact/QT8LDppx4qGnuttvvMwmPt). Where it disagrees with
anything else in the repo or the Features tab, this file wins. Copied 13 Nov 2026.

## Decisions

- **Rust, our own daemon, written from scratch.** herdr and Wolfpack are read for lessons, not forked.
- **v1 runs on macOS and Linux.** Windows later, so platform code stays isolated (`crates/terminal/src/pty.rs`).
- **Name**: spandeck; daemon spandeckd; the command center is the Control Deck; machines are stations, agents are crew, the event log is the ship's log.
- **Live handoff on upgrade, no per-pane host process.** The old daemon execs the new binary in place (same pid, inherited PTY descriptors). After a crash or reboot each agent resumes its conversation by session id.
- **Stations under the same Tailscale login find each other automatically, and each joins the fleet on the owner's approval, once** (changed 14 Nov 2026; before, they trusted each other with no step). Tailscale identifies a device, not a program on it, so a station also signs its calls with a fleet key and a browser on another device holds a token the owner approved. An agent reaching another station gets only what its task grants.
- **Agent messages are stored records** sent and received through tools (send, inbox, reply, ack), never keystrokes. A typed one-line nudge only wakes an idle agent.
- **A durable task record** exists for delegated, scheduled and recipe work. A pane you are just talking to has none.
- **Workspaces**: the agent you talk to stays in the main checkout; a delegated helper that writes gets its own worktree and branch; review-loop reviewers read a snapshot at the commit; testers read the same tree. Sign-off is bound to the commit reviewed.
- **Shared project memory** (start digest, findings board, automatic capture from hooks) on by default.
- **Session archive**: spandeckd copies every Claude Code and Codex session file as it grows, keeps raw lines plus a normalised copy, keyword search through `spandeck recall`. Own history always searchable; same-project on by default with secrets scrubbed; cross-project opt-in.
- **Standing orders at five layers** (fleet, group, station, project, role), as guidance and as enforced rules.
- **Review loop is the first recipe**: three rounds, stall check, sign-off before push.
- **Activity strip on every pane.** Tree, timeline, heat view are v1.5.
- **Usage per agent** (the owner's request on 14 Nov 2026; before, usage and cost views were not in v1): each pane shows its agent's model, tokens and context, with an estimated cost; the station and the fleet show totals. See Usage and cost.
- **Phone**: status, blockers, approval requests and stop controls; and, since 0.2 (the owner's request on 14 Nov 2026), talking to any pane: a message box that can be typed or dictated into, the keys a phone lacks, fitting the pane to the screen, and starting a pane. In the browser; no native app.
- **Station-to-station SSH** uses short-lived certificates spandeck issues per task (v1.5, with cross-station delegation).
- Not in v1: Windows, desktop shell, native iPhone app and widget, Officer of the Deck, fleet updates and channels (of spandeck itself), common config, cross-station delegation, flow view, agent tree and timeline. (OpenCode and Antigravity adapters were on this list until 14 Nov 2026: see Agent programs.)

## Pre-build resolutions (13 Nov 2026)

- One `spandeck hook` command for every vendor. Claude Code: hooks first. Codex: hooks where accepted, screen detection otherwise; the Codex adapter is fixture-tested and unverified until a live canary runs.
- Remote approval is in v1: the permission hook holds for an answer from the Control Deck or phone, then falls back to the terminal prompt on timeout.
- spandeckd listens on localhost and the tailnet address. Tailnet connections are identified by a Tailscale whois on the real address and must be the owner's login; they then need the fleet key (stations) or an approved token (browsers). Localhost needs a token. Serve identity headers are never trusted.
- Every pane gets a pane-scoped token. Enforced on cross-station calls; advisory on the same station.
- OSC 7501 is read when present, never required.
- The activity strip shows state, current tool and elapsed time, and since 14 Nov 2026 the model and a context gauge (before: no context gauge or cost).
- Fleet, group and station orders live in `~/.config/spandeck/orders.toml`; project and role orders in `.spandeck/orders.toml` in the repo.
- Cross-station in v1: discovery, fleet view, attach, stop, recall search, message relay. An agent's first route to another station needs one approval.
- The v1 enforcement table covers Claude Code and Codex.
- The host's Tailscale; no embedding.
- v1 notifications: one inbox, browser notifications on connected clients, optional ntfy or webhook sink.
- Backpressure: the PTY reader never waits for a client; slow clients are dropped and resynced from a snapshot.
- Command is `spandeck`; the short command is `sd` (chosen by the user on 13 Nov 2026; note it shares a name with a find-and-replace tool some people install).
- Public repo at github.com/michaelbaker-dev/spandeck, Apache-2.0, releases built by CI (the user's decision on 14 Nov 2026; it replaces "local git only until the trademark check"). The trademark check has not been done.
- One size per pane, set by whoever has control; others letterbox.

## Fleet and device sign-in (0.2, 14 Nov 2026)

- Every station starts as a fleet of one with its own key. `spandeck fleet --join <station>` asks another station for its key; the owner approves there, matching a code shown on both sides. The asking station then replaces its key.
- Calls between stations carry a signature over the exact request, good for two minutes. A task authorization from another station is trusted because the call that carries it is signed.
- A browser on another device asks to sign in, shows a code, and gets a token when the owner approves at a station (`spandeck approve <code>`) or in a signed-in Control Deck. Any station in the fleet accepts the token. It can be revoked.
- Without the key or a token, a device under the owner's login can only ask a station its name and ask to be let in.
- There is no switch that turns this off, in tests or otherwise.

## AgentVault (14 Nov 2026)

- spandeck and AgentVault stay separate programs. spandeck works without a vault.
- The vault is the only place a token is issued, and only when the owner approves on the vault's own page. The vault passphrase is never typed into the Control Deck. Approval opens in a small pop-up window on the vault's address.
- One token per station, held by spandeckd in a file only the user can read. Panes and agents never get the token, and keys are never put in a pane's environment.
- The token can list keys and send requests through the vault's proxy, read-only by default, and is pinned to the station's Tailscale node.
- spandeck finds the vault by its Tailscale name; the address is never hard-coded. `[vault] url` in the config overrides it.
- Tokens do not expire by default. Rotation is a per-station setting (never, or every N hours, at least 1) that only the owner changes; spandeckd rotates by itself.
- The owner is told when the setting changes outside the Control Deck, or the token is swapped or refused.
- Connecting a station is the only step: by default its agents may use every key the owner allowed that station's token at the vault (changed 14 Nov 2026 at the owner's request; before, a project had to list its keys). A project can narrow its agents with `[vault] keys` in `.spandeck/project.toml`.
- When a station has a token, a raw key in an agent's command is blocked and reported to the vault; one pasted into a prompt, or found in a transcript, is reported.
- **Connect prompt** (the owner's request, 14 Nov 2026: "there should be nothing I need to do"). A Control Deck that opens on a station that found a vault and has no working token says so once, in a bar that blocks nothing, with one button that starts the connect flow. "Not now" is kept by the station (so it holds in every browser), is undone from the Vault screen, and is forgotten when the station connects. No prompt when no vault was found or `[vault] enabled = false`.
- **Check and repair.** spandeckd checks the chain from an agent to a key when it starts, when a Control Deck opens (at most once a minute) and when asked (`vault.check`, the owner's only): the vault's address, its lock, the station's token (accepted, and tied to a machine), the key list, and what panes need (spandeck's own program and socket; Antigravity's plugin). It repairs without the owner: a token the vault calls overdue is swapped, Antigravity's plugin is rewritten. Each thing it cannot repair is one sentence with one action, in `vault.status` (`checks`, `health`), on the Vault screen and in `spandeck doctor`, and is told to the owner once (`vault.alert`), not on every check and not again after a restart.
- A repair never gets a token any way but the owner's approval at the vault.
- A connected station shows its token to no address but the one that issued it (a security fix of 14 Nov 2026: no other machine can ask for the token by claiming to be a vault). When that address stops answering and another AgentVault answers on the owner's machines, the station does not try its token there: it says "The vault seems to have moved to <address>" and offers Reconnect, the ordinary approval. Following a move with the existing token is behind `[vault] follow_moves = true`, off by default: relaxing the guard is the owner's decision, and the owner has not made it.

## Agent programs (14 Nov 2026, the owner's request)

- OpenCode and Antigravity are agents a pane can run, wired to spandeck's tools and skill when the pane starts. Neither can block a tool call or pass an approval to the Control Deck, so their rules are guidance.
- A pane started for an agent that is not installed on its station installs it first, in the pane, with the vendor's own install command (or the way the owner chose for that station, see below). That command is fixed in the code (a vendor's script is fetched over HTTPS only) or set in the station's own config; it never comes from a caller or another station.
- Looking for an agent's program has four answers, never fewer (14 Nov 2026, after a fault on the owner's Mac: a Codex whose real binary was gone was called installed, and one that was slow to answer was called not found): not installed; installed (it answered when asked its version); installed but not working (it exits with an error or cannot be run); installed but it did not answer in time, which is unknown, not broken. A program that is slow to say its version is never killed.
- When a program is in several folders spandeck looks in, the first copy that answers is used, not the first that exists.
- A pane started for a program that is installed but not working offers a reinstall in the pane, the same way a missing one is installed: what was found and the program's own error line, the command, Ctrl-C skips. Success is the program answering afterwards, not the installer's exit code. Whether an install should ask before it runs is still open with the owner; for now it announces the command and Ctrl-C skips.
- Each agent has the ways it can be installed that its vendor documents: the vendor's own script (the default), npm, Homebrew. A way is offered on a station only when its tool is there. The owner's choice is kept per station and per agent in the station's state; `install_cmd` in the station's config still wins over everything. Every command is fixed in the code, taken from the vendor's documentation or a package registry; a way that could not be confirmed is left out. Installing, like updating, is the owner's to do (`agents.install`): an agent cannot.
- The owner can see each agent's version on each station and update one or all of them. After an update that changed the version, an idle pane of that agent restarts into its conversation; a pane in a turn, or waiting on the owner, restarts when it is next idle. Updating is the owner's to do: an agent cannot.

## Add a machine (14 Nov 2026, the owner's request)

- The fleet screen ends in an "Add a machine" tile. It lists the Mac and Linux machines under the owner's Tailscale login that are online and not stations; phones, Windows, tagged and shared machines are left out. The station the owner is using connects to the one picked over SSH, runs the same install script as the README, starts spandeck there now and at login, and has it ask to join as `spandeck fleet --join` does. The tile then shows the join code.
- **The approval step stays.** The new station is approved with the usual control, by its code. A code that arrives over SSH is shown only if this station holds a waiting request with the same code. The join step is one place in the code (`JoinBy` in `crates/core/src/add.rs`), so a single-use ticket could replace the code later; no ticket exists.
- **Its own setting, off by default: `add_machines = true`** in the config of the station that does the adding, not `remote_start`. Both let a station run a command on another machine, and both are off until the owner turns them on at that station. They are separate because they differ in what they risk: `remote_start` runs one fixed command on a machine that is already a station, with access the owner already set up; adding installs software on a machine spandeck has never run on, and can use a password typed into the deck. An owner who wants one has not agreed to the other. The picker works while it is off and says how to turn it on; nothing is run and no password is accepted.
- The owner can also turn it on, and off again, from the tile itself (changed 14 Nov 2026: the owner clicked the tile and asked why they could not add a machine). The switch is kept in the station's state, is logged, is the owner's alone (`fleet.add_enable` is on no agent or station list and is never forwarded), and applies only to the station whose Control Deck they are using. It stays off until the owner turns it on.
- **Only the owner, and only at the station being used.** The methods are on no list for agents or other stations, and are never forwarded between stations in either direction, so a password never travels from one station to another.
- **The password** is asked for only after a first try without one fails (Tailscale SSH or an SSH key that already works needs none). It is used for one connection and held in memory until ssh has asked for it once, or the connection ends. It is never written to a file, a log, the ship's log, an event, an error or a command line or environment: ssh gets it by running spandeck itself as its askpass helper, which is handed it once over a private socket, and only for a prompt that asks for a password. A second prompt gets nothing, so a wrong password is tried once. The ship's log records who added which machine as which user and how it went.
- **Host keys.** The address is the one the local Tailscale client gives for the machine picked, and must be a Tailscale address; a caller names a machine, never an address. The first connection saves the machine's host key in `known_hosts` in the state directory (ssh's accept-new); a different key later is an error, shown plainly, and nothing is sent. The owner's own ssh config is not read, so it cannot redirect the connection or switch checking off.
- What the machine prints is shown as it runs. It is text from somewhere else: control characters are dropped and the Control Deck inserts it as text.
- `add_machine_cmd` replaces the ssh command (`{user}`, `{host}`, `{known_hosts}`); the script arrives on its standard input. The script uses only commands every release has, since the release it installs may be older than the station that sends it.

## Usage and cost (14 Nov 2026, the owner's request)

- Token counts are only what an agent wrote in its own session data: Claude Code's and Codex's session files, OpenCode's session database. Antigravity keeps nothing spandeck can read, and is shown as not reported. Nothing is typed in by the owner.
- Counted per conversation and per model, sub-agents included under the model that ran them; a model call written more than once is counted once. A new conversation starts from zero; the station keeps its totals.
- Cost is an estimate at list prices and is called that wherever it appears, with the list's source and date: a subscription is not billed per token.
- Prices and context windows come from a public list (models.dev) that spandeckd fetches by itself once a day and keeps. The request carries nothing about the owner or a project. No price or window size is written into the code; a model the list does not name has no price. `[prices]` in the config is an optional override, never needed.
- The context gauge uses the window the session data states (Codex), else the list's.

## Guarantees (contracts the build must meet)

### Tasks
- Fields: goal, acceptance criteria, agent, station, workspace, dependencies, outputs, review outcome, resume information.
- States: queued, running, waiting for approval, blocked, completed, failed, cancelled.
- An agent going quiet is not completion. A task is completed only by an explicit result or a sign-off.

### Stop, pause and offline
- Work already running continues if the network drops or the controlling station disappears.
- No new remote assignment is accepted without a valid task authorization; these expire.
- Pause: no new work is delivered; the current turn finishes.
- Stop: the agent's current turn is interrupted; the process stays.
- Kill: the process ends.
- The Control Deck shows three separate states: stop requested, stop confirmed, station unreachable. It never implies an unreachable station has stopped.
- A stop request for an unreachable station is delivered on reconnect and applies only to the work that was running when it was issued.

### Messages
- Every message has a unique id. Delivery is at least once, duplicates dropped by id, so a reconnect cannot start the same task twice.
- Ordering is guaranteed within a thread only.
- Received and acted-on are separate acknowledgments.

### Workspaces
- The parent agent, or the user, integrates a helper's branch. A conflict goes back to the helper as a task.
- After a crash the worktree and branch are kept, and the task is marked failed with its resume information.
- A sign-off names the commit reviewed and becomes stale if the code changes.

### Trust
- The user at the Control Deck has full authority on every station.
- An agent has only what its task grants. Permissions are separate: discover, read history, execute commands, administer a station. Agents never get administer.

### Sharing and archive
- Captured events and posted findings: on by default. Transcripts via recall: own always; same project scrubbed; cross-project opt-in.
- Raw history stays on the station that recorded it. Other stations and agents only see the scrubbed view. Scrubbing is best-effort.

### Enforcement
- Observing an agent and enforcing a rule on it are separate channels. Screen detection observes; only a hook or approval channel can block.
- A rule is shown as enforced for an agent only where that agent's integration can block it. Elsewhere it is guidance.

### Upgrades
- Before: a version compatibility check between stations. After: a health check, and rollback to the previous binary if it fails.
- If live handoff does not complete, terminals are lost and agents resume by session id.
- Live handoff is a release acceptance test on macOS and Linux.

## Recommended defaults
- Helpers always start in a fresh pane; helpers cannot delegate further unless a role allows it.
- The ship's log keeps each tool call's name, timing and result; full contents live only in the session archive.
- Processes do not survive a daemon crash; agents resume by session id.

## Testing rules (from the Build and test plan tab)
- Test where spandeck can break: the terminal, each agent integration, the Guarantees, the link between stations, upgrades. Target 150 to 250 tests.
- A test needs a reason: a Guarantee, a phase's acceptance criterion, or a real bug. No tests of trivial code, no mocks of our own modules.
- Every-change tier under 90 seconds; merge tier under 10 minutes. A flaky test is fixed or deleted within a day.
- The station link sits behind a swappable transport so the message relay runs under Turmoil.
