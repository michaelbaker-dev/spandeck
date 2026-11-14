# spandeck

An always-running daemon (`spandeckd`) and a web Control Deck for running AI coding
agents (Claude Code, Codex) in panes across your machines.

- **Stations**: every machine under your Tailscale login that runs spandeckd. They find
  each other by themselves; you approve each one into your fleet once.
- **Crew**: agents in panes. Hooks report what each one is doing: working, needs you,
  done, error.
- **Control Deck**: the web app every station serves. Zoom from fleet to station to pane.
  Mouse first: click to focus, drag to resize, right-click for everything.
- **Ship's log**: every state change, tool call (name and result, not contents), message
  and block, in SQLite on the station that saw it.

Read `DECISIONS.md` before changing anything. It wins over every other document.

## Status

Version 0.1.0 is the first release. It was built and tested on one Linux arm64 machine.
The macOS and Linux x86-64 binaries are built and tested by CI and have had little use
on real machines yet. See "Not verified" below before you rely on it.

## Requirements

- macOS (Apple silicon or Intel) or Linux (x86-64 or arm64, glibc 2.35 or newer).
- [Tailscale](https://tailscale.com), signed in, on every machine you want as a station.
  spandeck uses it to find your other stations and to know who is connecting.
- `git`, and at least one agent CLI installed and signed in: Claude Code or Codex.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/michaelbaker-dev/spandeck/main/scripts/install.sh | sh
```

This downloads the release for your machine from
[Releases](https://github.com/michaelbaker-dev/spandeck/releases), checks its SHA-256,
and puts `spandeck` and the short command `sd` in `~/.local/bin`. To do it by hand,
download the `.tar.gz` for your machine and its `.sha256`, check it, and copy the two
binaries onto your `PATH`. Each release file also carries a GitHub build attestation:
`gh attestation verify <file> --repo michaelbaker-dev/spandeck`.

Or build from source with a current Rust toolchain: `cargo build --release`.

## Quick start

```sh
spandeck service --install     # start spandeckd at login (or run `spandeck daemon` in a terminal)
spandeck doctor                # check the station
spandeck cc                    # print the Control Deck address
spandeck project new --pane dev:claude:author   # in a repo
```

From another device on your tailnet, open `http://<station>:7391/`. The first time, the
page shows a code and asks you to approve the browser: run `spandeck approve <code>` on
the station, or approve it under **Access** in a Control Deck that is already signed in.
On the station itself, use the address `spandeck cc` prints (it carries a local token).

To add a second station, install spandeck on it and run `spandeck fleet --join <first
station>`, then approve the code on the first station. Stations under your Tailscale
login see each other without this, but neither controls the other until you approve.

To update a station without losing its terminals: install the new release, then
`spandeck upgrade ~/.local/bin/spandeck`. If the new version does not start properly,
the station goes back to the one it was running and keeps every terminal.

## Layouts

A project can have named layouts: which of its panes are on screen and how they are
arranged. They are saved in the project file, so they are the same on every device.

- **New layout:** the `＋` beside the tabs, then "New layout in <project>". It starts
  with the project's running panes.
- **Resize:** drag the line between two panes, at any time.
- **Move panes:** press **Arrange**, then drag a pane onto another. Dropping on an edge
  puts it on that side; dropping in the middle swaps the two. Drag panes in from the
  list on the left, or pick a shape (side by side, two over one, one + stack, grid).
- A pane in a layout that is not running shows a button to start the project's panes.

Workspaces, the other kind of tab, are kept in the browser only and can hold any pane.

## Looking after stations

From the Control Deck, right-click a station (or use the commands on any station):

| To | In the deck | Command |
| --- | --- | --- |
| Restart spandeck, keeping every terminal | Restart spandeck here | `spandeck restart [--station NAME]` |
| Update to the latest release | Update to the latest release | `spandeck update [--station NAME]` |
| Wake a sleeping machine | Wake it | `spandeck wake NAME` |
| Start spandeck where it is not running | Start spandeck there | `spandeck start NAME` |

- **Update** downloads the release on that station, checks it against its published
  SHA-256, and swaps it in. If the new version does not start properly, the station
  goes back to the old one. It then replaces the installed binary, so a reboot starts
  the new version.
- **Wake** sends wake-on-LAN packets. They reach only a machine on the same network as
  the station you send from, and only if that machine is set to wake on them.
- **Start** runs a command on the other machine over Tailscale SSH. It is off until you
  put `remote_start = true` in the config of the station that does the starting, and
  Tailscale SSH must be on for the machine being started. It starts the login service
  if there is one, else `~/.local/bin/spandeck daemon`.
- A station that is off stays in the list as unreachable. One that does not start at
  login is marked; fix that with `spandeck service --install`.

## From your phone

Open any station from a phone on your Tailscale account: `spandeck cc --qr` shows a QR
code for it. Approve the phone once, as above. Then you can:

- see every station and agent, answer approval requests, and stop a turn or the fleet;
- open any pane and talk to it: type or dictate a message and press Send, with keys a
  phone keyboard lacks (Esc, Tab, arrows, Enter, Ctrl-C);
- press **Fit to screen** to resize the pane to the phone so the text is readable;
- start a new agent or shell on any station with **New**.

Dictation: the message box is an ordinary text field, so the microphone on the phone's
keyboard works everywhere. The 🎤 button uses the browser's own speech recognition; it
appears only where the browser offers it and needs the HTTPS address (below).

For HTTPS: `tailscale serve --bg 7391`, then open the station's `https://` Tailscale name.

A station can only be reached while it is awake. spandeck keeps it awake while an agent
is working. To keep it awake whenever spandeckd runs, add `stay_awake = true` to
`~/.config/spandeck/config.toml`.

## What v1 does

| Area | What works |
| --- | --- |
| Daemon | Panes survive client disconnects. An upgrade replaces the daemon in place and keeps every terminal (`spandeck upgrade <binary>`), and undoes itself if the new version fails its health check. After a crash or reboot each agent resumes its conversation by session id. |
| Agents | Claude Code and Codex through one hook command. States, session capture, remote approval of permission prompts, stuck detection, pause / stop / kill. |
| Projects | `.spandeck/project.toml` in the repo. Opening a project restores its panes. |
| Fleet | Discovery over Tailscale, one deck for every station, attach through the station you opened, stop-all with three honest states (requested, confirmed, unreachable). |
| Working together | Messages as records (`send`, `inbox`, `reply`, `ack`) through an MCP server; delegation with task records; worktree per writing helper; checkpoint and diff; sign-off bound to a commit; the review loop; shared project memory; session archive with `recall`. |
| Standing orders | Rules and guidance at five layers. Rules are enforced by hooks and shown as enforced only where the agent can really be blocked. |
| API keys | Optional [AgentVault](https://github.com/michaelbaker-dev/agentvault) integration: agents use keys through the vault and never see them. See `docs/vault.md`. |
| Agent setup | MCP servers, skills and default model and effort, set once and applied on every station; run a command (`/login`, `/model …`) in one agent or all of them. |

## Not verified

- **macOS**: CI runs the test suite and the live-handoff release gate on macOS, but
  nobody has used it day to day on a Mac yet. The vault token is kept in a file readable
  only by you, not in the keychain.
- **Codex**: its adapter follows the published hooks reference and is fixture-tested
  only. The enforcement table shows Codex rules as guidance until a live canary runs.
  Codex also needs its hooks trusted once with `/hooks`.
- **More than one real station**: discovery, attach and the message relay are tested
  with several daemons on one machine and under network simulation, not yet between
  two physical machines.
- **Phone**: tested in a phone-sized desktop browser, not on a real phone. Dictation
  with the 🎤 button has not been tried on any device.

## Security

spandeck runs agents that can run commands, so read `SECURITY.md` before exposing a
station. In short: it listens on localhost and your Tailscale address only, trusts only
your own Tailscale login, and treats everything an agent or another station says as
data. Report a vulnerability privately through the repository's Security tab.

## Layout

```
crates/terminal   PTY, server-side screen, snapshots for late joiners      (platform code: pty.rs)
crates/agents     one adapter per agent: normalize hooks, reply, install, detect
crates/archive    session files: follow, normalise, scrub, search
crates/core       the daemon: panes, hooks, projects, tasks, relay, orders, fleet, setup
crates/web        HTTP + WebSocket API, the gateway to other stations, the Control Deck assets
crates/cli        the `spandeck` binary, the MCP server, the scriptable fake agent, scenario tests
```

## Tests

Tests exist where spandeck can break: the terminal, each agent integration, the
Guarantees, the link between stations, and upgrades.

```sh
scripts/check.sh            # every change: format, lint, tests. Budget 90 s (about 55 s today)
scripts/canary.sh           # live: real Claude Code in a pane; spends a little quota
cargo test -p spandeck --test soak --release -- --ignored    # nightly: 50 streaming panes
```

| Kind | Where |
| --- | --- |
| Replays of real terminal sessions against saved screens | `crates/terminal/tests/replay.rs`, `recordings/` |
| Hook payload fixtures per agent version | `crates/agents/fixtures/`, `tests/fixtures.rs` |
| Scenarios with a scriptable fake agent, one per Guarantee | `crates/cli/tests/phase*.rs` |
| Network simulation of the message relay (Turmoil) | `crates/core/tests/relay_sim.rs` |
| Property tests (dedupe, task states, secret scrubber) | `crates/core/tests/properties.rs`, `crates/archive/src/scrub.rs` |
| Browser smoke tests | `crates/cli/tests/phase2.rs`, `phase5.rs`, `agent_setup.rs` |
| Live canary and handoff | `--ignored canary`, `crates/terminal/tests/handoff.rs`, `phase1.rs` |

A test needs a reason: a Guarantee, a phase's acceptance criterion, or a bug that
happened. Record more terminal sessions with `cargo run -p spandeck-terminal --example record`.
