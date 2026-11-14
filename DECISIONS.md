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
- **Phone**: status, blockers, approval requests and stop controls; and, since 0.2 (the owner's request on 14 Nov 2026), talking to any pane: a message box that can be typed or dictated into, the keys a phone lacks, fitting the pane to the screen, and starting a pane. In the browser; no native app.
- **Station-to-station SSH** uses short-lived certificates spandeck issues per task (v1.5, with cross-station delegation).
- Not in v1: Windows, desktop shell, native iPhone app and widget, Officer of the Deck, usage and cost views, fleet updates and channels, common config, cross-station delegation, flow view, agent tree and timeline, OpenCode and Antigravity adapters.

## Pre-build resolutions (13 Nov 2026)

- One `spandeck hook` command for every vendor. Claude Code: hooks first. Codex: hooks where accepted, screen detection otherwise; the Codex adapter is fixture-tested and unverified until a live canary runs.
- Remote approval is in v1: the permission hook holds for an answer from the Control Deck or phone, then falls back to the terminal prompt on timeout.
- spandeckd listens on localhost and the tailnet address. Tailnet connections are identified by a Tailscale whois on the real address and must be the owner's login; they then need the fleet key (stations) or an approved token (browsers). Localhost needs a token. Serve identity headers are never trusted.
- Every pane gets a pane-scoped token. Enforced on cross-station calls; advisory on the same station.
- OSC 7501 is read when present, never required.
- v1 activity strip shows state, current tool and elapsed time (no context gauge or cost).
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
- A project names the keys its agents may use (`[vault] keys` in `.spandeck/project.toml`).
- When a station has a token, a raw key in an agent's command is blocked and reported to the vault; one pasted into a prompt, or found in a transcript, is reported.

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
