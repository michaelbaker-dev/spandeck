# spandeck

An always-running daemon (`spandeckd`) and a web Control Deck for running AI coding
agents (Claude Code, Codex) in panes across your machines.

- **Stations**: every machine under your Tailscale login that runs spandeckd. They find
  and trust each other with no pairing step.
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

From another device on your tailnet, open `http://<station>:7391/`. No token is needed
there: the station asks Tailscale who is connecting, and only your own Tailscale login
gets control. On the station itself, always use the address `spandeck cc` prints (it
carries a local token), even when you open it by its Tailscale name.

For a phone, or any HTTPS need: `tailscale serve --bg 7391` and open the HTTPS name with
the token from `spandeck cc` appended as `#token=…` once.

To update a station without losing its terminals: install the new release, then
`spandeck upgrade ~/.local/bin/spandeck`.

## What v1 does

| Area | What works |
| --- | --- |
| Daemon | Panes survive client disconnects. An upgrade replaces the daemon in place and keeps every terminal (`spandeck upgrade <binary>`). After a crash or reboot each agent resumes its conversation by session id. |
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
- **Phone**: tested in a phone-sized browser, not on a real phone.

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
