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

Version 0.4.10. It is built on one Linux arm64 machine and tested there and in CI on
macOS, Linux and Windows, and runs on real Macs and one real Windows 11 PC. Windows is
new since 0.3.0 and its programs are not code-signed yet: where Smart App Control is
on, Windows blocks them. It is young: see "Not verified" below before you rely on it.

## Requirements

- macOS (Apple silicon or Intel) or Linux (x86-64 or arm64, glibc 2.35 or newer).
- [Tailscale](https://tailscale.com), signed in, on every machine you want as a station.
  spandeck uses it to find your other stations and to know who is connecting.
- `git`, and at least one agent CLI installed and signed in: Claude Code or Codex.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/michaelbaker-dev/spandeck/main/scripts/install.sh | sh
```

Windows (x64, Windows 10 1809 or later; in PowerShell, no administrator needed). Windows
support is new: read "Windows" in `DECISIONS.md` for what differs. A Windows station runs
while you are logged on. spandeck's programs are not signed, so a machine with Smart App
Control on refuses to run them (and builds made from source there too): it has to be off.

```powershell
irm https://raw.githubusercontent.com/michaelbaker-dev/spandeck/main/scripts/install.ps1 | iex
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

To add a second station, use **Add a machine** in the Control Deck (below), or install
spandeck on it and run `spandeck fleet --join <first station>` (if it already has
fleet-mates, they all come with it: there is only ever one fleet), then approve the code on
the first station, or on any station already in its fleet. Stations under your Tailscale
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
- **Hide a pane:** the `▁` button on its title bar, or **Hide** in its menu. The other
  panes take its room, and it stays as a chip beside the layout's name, showing its
  state (amber when it needs you). Click the chip to put it back where it was, at the
  size it had. It keeps running while hidden. **Show only this** hides all the others;
  **Show all** brings them back. What is hidden is saved with the layout.
- A pane in a layout that is not running shows a button to start the project's panes.

Workspaces, the other kind of tab, are kept in the browser only and can hold any pane.

**Who is doing what.** A pane whose agent has others working under it shows an
**Agents** button; it slides a panel in from the right listing the helpers it gave tasks
to, and each agent's own sub-agents, with what each is doing. The **Watch** tab shows
every agent in a project as a small live card, with a line for each task given and each
message in the last ten minutes, and the messages beside them. On a project layout,
**Lines** draws the message lines over the live panes.

## Looking after stations

From the Control Deck, right-click a station (or use the commands on any station):

| To | In the deck | Command |
| --- | --- | --- |
| Restart spandeck, keeping every terminal | Restart spandeck here | `spandeck restart [--station NAME]` |
| Update to the latest release | Update to the latest release | `spandeck update [--station NAME]` |
| Wake a sleeping machine | Wake it | `spandeck wake NAME` |
| Start spandeck where it is not running | Start spandeck there | `spandeck start NAME` |
| End every pane on a station | Close all panes here… | `spandeck pane close --all [--station NAME]` |
| Take a station out of the fleet | Remove this station from the fleet… | `spandeck fleet --remove NAME [--stop]` |

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
- **Close all panes** ends every agent and shell on that station, with everything they
  started, and takes them off the list. A conversation can be resumed later only where
  the agent saved one. Projects, layouts, history and the archive are left alone.
- **Remove this station from the fleet** closes its panes, then takes it out for good.
  Every station shares one fleet key, so the stations that stay get a new one and the
  key the removed machine held stops working. What that costs you:
  - Browsers signed in from other devices (a phone, a laptop) must sign in again. The
    one you remove from stays signed in.
  - A station that is off or unreachable at that moment keeps the old key until it is
    seen again, and is handed the new one then (for 30 days; after that, join it again
    and approve). Until then it shows as "not in this fleet", and the removed machine
    could still act on it.
  - A station that cannot be reached is removed from the fleet's side only: its panes
    keep running until someone stops spandeck there, and it cannot rejoin without a new
    approval.
  - Its AgentVault token stays on that machine and keeps working. Revoke
    `spandeck:<station>` on the vault's Tokens page.
  - With "Also stop spandeck there" (`--stop`), spandeck is stopped on that machine and
    no longer starts at login. The program and its state and config folders stay on
    its disk; the dialog says where.
  - Removing the station you are using is done through another station; if none can be
    reached it leaves by itself, and you remove it at one of the others afterwards.
  To put a station back: `spandeck fleet --join`, and approve.

## Agent programs

Right-click a station and choose **Agent programs and versions…** to see, for every
station, each agent program (Claude Code, Codex, OpenCode, Antigravity), its version, and
buttons to update or reinstall it. `spandeck doctor` shows the same on one station.

- spandeck tells four cases apart: **not installed**; **installed** (it answered when
  asked its version); **installed but not working** (it is there but exits with an error
  or cannot be run, and its own error line is shown); and **installed, but it did not
  answer** in five seconds (not known to be a fault; it is left running, never stopped).
- When a program is in several folders, the first copy that answers is used, not the
  first that exists. spandeck looks on `PATH`, then in `~/.local/bin`, `~/.opencode/bin`,
  `~/.cargo/bin`, `/opt/homebrew/bin` and `/usr/local/bin`.
- A pane started for a program that is not installed, or is installed but not working,
  installs it first, in the pane: it says what was found, shows the command, and
  Ctrl-C skips. Whether it worked is judged by the program answering afterwards.
- **Reinstall** in the dialog does the same from the Control Deck. Where the vendor
  supports it you can pick npm or Homebrew instead of the vendor's own install script,
  if that tool is on the station; the pick is remembered per station and per agent, and
  is what a pane uses too. Only you can do this: an agent cannot install or update.

| Agent | Install script (the default) | npm | Homebrew |
| --- | --- | --- | --- |
| Claude Code | `curl -fsSL https://claude.ai/install.sh \| bash` | `npm install -g @anthropic-ai/claude-code@latest` | `brew install --cask claude-code` |
| Codex | `curl -fsSL https://chatgpt.com/codex/install.sh \| sh` | `npm install -g @openai/codex` | `brew install --cask codex` |
| OpenCode | `curl -fsSL https://opencode.ai/v2/install \| bash` | `npm install -g @opencode/cli` | `brew install anomalyco/tap/opencode-v2` |
| Antigravity | `curl -fsSL https://antigravity.google/cli/install.sh \| bash` | not offered | not offered |

These were read from each vendor's documentation and the package registries on
14 Nov 2026. To use another command on one station, set it in that station's
`config.toml`; it then wins over everything:

```toml
[agents.codex]
install_cmd = ["sh", "-c", "your command"]
```

## Adding a machine

The fleet screen ends in a tile with a plus sign: **Add a machine**. It installs spandeck
on another of your machines and brings it into the fleet without you opening a terminal
there.

1. Turn it on once, on the station whose Control Deck you use: click the tile and choose
   **Turn it on**. It is off by default because it lets that station run commands on your
   other machines; only you can turn it on, and you can turn it off again from the same
   dialog. (`add_machines = true` in that station's `config.toml` does the same.)
2. Click the tile and pick a machine. It lists the Mac, Linux and Windows machines under
   your Tailscale login that are online and not stations yet (no phones or tagged
   machines). A Windows machine needs its OpenSSH server on (Settings, System, Optional
   features, OpenSSH Server; then `Start-Service sshd`), and you logged on there: a Windows
   station runs in your logged-on session.
3. Give your user name on that machine. If Tailscale SSH is on there, or the station
   already has an SSH key that works, nothing more is asked. Otherwise you are asked for
   the password. It is used for that one connection and is not saved or logged.
4. The station connects over SSH and runs the install script above: you see its output
   as it runs. spandeck is then started there, now and at login. A machine that cannot
   reach GitHub is sent the release by the station, over the same SSH connection: the
   station downloads the release for that kind of machine and checks it, or, if it cannot
   reach GitHub either and is the same kind of machine, sends its own copy. The dialog
   says which.
5. The new station asks to join, and the tile shows its code with **Approve** and
   **Refuse**. Approve it, and the tile becomes the new station.

A machine that already runs spandeck but is not in this fleet has a tile of its own, with
**Add … to this fleet**. That does steps 3 and 5 only: nothing is installed, the station
there is made to ask, and you approve its code here. If it is in a fleet with other
stations, they come with it.

**Uninstall spandeck on …**, in a station's menu, is the reverse: the station is removed
from the fleet, spandeck is stopped there and no longer starts at login, and its programs
are deleted. Its history and settings stay on that machine, so adding it back picks them
up, unless you tick **Erase everything**. Your project folders are never touched.

If it does not work, the tile says why and what to do. The usual reasons:

| It says | Do this |
| --- | --- |
| SSH is not turned on | On a Mac: System Settings, General, Sharing, Remote Login. On Linux: install and start the SSH server. Or turn on Tailscale SSH there: `tailscale set --ssh`. |
| It did not accept that user name and password | Check both. The user name is the short account name (`whoami` on that machine). |
| It did not answer | The machine is asleep or off, or Tailscale is not connected there. |
| It answered with a different SSH host key | If you reinstalled that machine, delete its line from `~/.local/state/spandeck/known_hosts`. If you did not, do not connect. |
| No release for its system | spandeck runs on macOS and Linux, on Apple silicon, arm64 and x86-64. |
| It could not reach GitHub | The machine needs internet access to download the release. |
| The install script failed | Its last lines are shown. Running the install command above on that machine shows the same. |

The station needs the OpenSSH client (8.4 or newer). `add_machine_cmd` in the config
replaces the SSH command, with `{user}`, `{host}` and `{known_hosts}` filled in; the
install script arrives on its standard input. The first time a machine is reached its
SSH host key is saved in spandeck's own `known_hosts`; your `~/.ssh/config` and
`~/.ssh/known_hosts` are not used. On a Mac nobody is logged in to, the login service
cannot be started over SSH: spandeck is started directly and starts by itself from the
next login.

## Usage and cost

Each agent pane shows the model in use and how full its context is, in the strip above
its terminal. Click it, or choose **Usage** in the pane's menu, for the token counts
(input, output, cache read and write), a table per model, and an estimated cost.
**Usage** in the top bar adds up every station: per model, per project and per pane, for
today and for all time.

- The token counts are the agent's own: spandeck reads them from Claude Code's and
  Codex's session files and OpenCode's session database as they grow. Sub-agents are
  included, under the model that ran them. Antigravity records nothing spandeck can
  read, so its panes say "not reported by this agent". A conversation spandeck
  never followed is not in the totals.
- **Cost is an estimate at list prices.** spandeck fetches a public price list
  ([models.dev](https://models.dev)) once a day and keeps a copy, so it works offline.
  What you actually pay depends on your plan: a subscription is not billed per token.
  A model the list does not name is shown with its tokens and "no price".
- The context gauge uses the window size the session file states (Codex) or, when it
  states none, the one in the price list.
- A model is looked up by the id in the session file, under its provider; if that is
  not listed, by the id in lower case without a `[…]` suffix, a `provider/` prefix or a
  trailing date. Prices that rise above a context size (tiers) are not applied.

Advanced: `[prices]` in `~/.config/spandeck/config.toml` takes `auto = false` (never
fetch), `url = "…"` (another list in the same shape) and your own figures per model, in
dollars per million tokens, which replace the list's:
`[prices.model."claude-opus-5-5"]` with `input`, `output`, `cache_read`, `cache_write`.

## From your phone

Open any station from a phone on your Tailscale account: `spandeck cc --qr` shows a QR
code for it. Approve the phone once, as above. Then you can:

- see what needs you first (approval requests with Allow and Deny, agents that wait), then
  your sessions, the one that changed last first, with a dot on those that have said
  something you have not read; **By station** shows every station with all its controls;
- open an agent and read its conversation: what was said as text, the tool calls folded
  into one line you can open, an approval as a card, and a message box (**+** sends a
  photo or a file). Claude Code and Codex have a conversation; other agents and shells
  open as their terminal;
- switch to **Terminal** for the pane itself: type or dictate a message and press Send,
  with keys a phone keyboard lacks (Esc, Tab, arrows, Enter, Ctrl-C), and **Fit to
  screen** to resize the pane to the phone so the text is readable;
- start an agent or a shell on any station with **+ New session**; stop a turn or the
  fleet.

On a computer a pane's title bar has **Chat**, which shows the same conversation in place
of the terminal.

Dictation: the message box is an ordinary text field, so the microphone on the phone's
keyboard works everywhere. The 🎤 button uses the browser's own speech recognition; it
appears only where the browser offers it and needs the HTTPS address (below).

**HTTPS, and the deck saved on your phone.** Where Tailscale gives the machine a
certificate (HTTPS certificates switched on for your tailnet, in Tailscale's admin
console under DNS), a station also serves its deck at
`https://<machine>.<tailnet>.ts.net:7392/`; the plain address keeps working. On macOS and
Windows that happens by itself. On Linux Tailscale gives the certificate only to root or
to a user it was told to trust: the deck offers **Turn on secure access (HTTPS)** in the
station's menu, which asks for your password on that machine once and runs one command
with it (`sudo tailscale set --operator=<you>`; the password is not kept). Turning HTTPS
on publishes the machine's Tailscale name in the public certificate-transparency logs;
the machine is still reached only from your tailnet. `[web] https = false` in
`config.toml` turns it off.

Open the HTTPS address on the phone and choose **Add to Home Screen**: the deck is then
kept on the phone and opens even while that station is off.

**When a station stops answering**, a deck that is open (or the saved one, when it is
opened) lists the other stations of the fleet that answer, fastest first, and carries on
through the fastest after five seconds unless you pick another or are writing; what you
were writing stays. When the first station is back the deck offers to go back. A page
loaded over plain HTTP can do this through any updated station; a page loaded over HTTPS
only through a station that has HTTPS too, so turn it on at two stations at least.
`[flags] page_failover = false` turns it off at a station.

(`tailscale serve --bg 7391` still works as before, with the browser's device no longer
checked against your Tailscale login, since every caller then comes from localhost.)

A station can only be reached while it is awake. spandeck keeps it awake while an agent
is working. To keep it awake whenever spandeckd runs, add `stay_awake = true` to
`~/.config/spandeck/config.toml`.

## What v1 does

| Area | What works |
| --- | --- |
| Daemon | Panes survive client disconnects. An upgrade replaces the daemon in place and keeps every terminal (`spandeck upgrade <binary>`), and undoes itself if the new version fails its health check. After a crash or reboot each agent resumes its conversation by session id. |
| Agents | Claude Code and Codex through one hook command. States, session capture, remote approval of permission prompts, stuck detection, pause / stop / kill. |
| Projects | `.spandeck/project.toml` in the repo. Opening a project restores its panes. |
| Fleet | Add a machine from the deck (SSH in, install, start, ask to join). Discovery over Tailscale, one deck for every station, attach through the station you opened, stop-all with three honest states (requested, confirmed, unreachable). |
| Working together | Messages as records (`send`, `inbox`, `reply`, `ack`) through an MCP server; delegation with task records; worktree per writing helper; checkpoint and diff; sign-off bound to a commit; the review loop; shared project memory; session archive with `recall`. |
| Standing orders | Rules and guidance at five layers. Rules are enforced by hooks and shown as enforced only where the agent can really be blocked. |
| API keys | Optional [AgentVault](https://github.com/michaelbaker-dev/agentvault) integration: agents use keys through the vault and never see them. See `docs/vault.md`. |
| Usage | Model, tokens and context per pane; totals per station, project and model for today and all time; cost estimated from a public price list fetched daily. |
| Performance | One page for the fleet, the same from any station: a sentence per station, request and link timings, restarts and why, clocks that differ, and each machine's Tailscale key expiry with warnings at 14, 7, 3 and 1 day. |
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
- **Adding a machine**: tested with the real OpenSSH client against a test SSH server on
  the same machine, and with stand-ins for Tailscale and the release page. It has not
  yet installed spandeck on a second physical machine, used a real password login, or
  met Tailscale SSH or macOS Remote Login.
- **Removing a station**: tested with three daemons on one machine. "Also stop spandeck
  there" has not been run against a real login service on any system (systemd, launchd
  or the Windows Scheduled Task), and ending a pane's whole session has not been tried
  on a Mac.
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
