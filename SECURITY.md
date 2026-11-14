# Security

spandeck starts AI agents that can run commands on your machines. Treat a station like
a machine you have given a colleague a shell on.

## Reporting a vulnerability

Use **Security → Report a vulnerability** on the GitHub repository. Please do not open a
public issue for a security problem. This is a one-person project, so expect a reply in
days, not hours.

## How it is meant to be safe

- **Where it listens.** spandeckd listens on localhost and on the machine's Tailscale
  address. It does not listen on your LAN or the internet unless you change `web_bind`.
- **Who is trusted.** Two things, both required. First, the connection must come from
  a device under the same Tailscale login as the station; the station asks its local
  Tailscale daemon, and never trusts identity headers. Second, the caller must prove
  itself, because Tailscale says which device is calling, not which program on it:
  - another **station** signs every call with the fleet key. A station gets the key
    only when you approve its request on a station that already has it;
  - a **browser** holds a token it was given when you approved it, once, on a station
    or in a Control Deck that was already signed in;
  - on the station itself, the local token from `spandeck cc`.
- **What that means.** A program on one of your devices that has neither the fleet key
  nor a token gets nothing. Anyone who can run code as your user on a station can read
  that station's key and tokens, and so controls the fleet.
- **Agents are not the owner.** Each pane has its own token. Agents can message, delegate
  and search history; they never get a call that administers a station. Text from agents,
  other stations and session history is shown as text and labelled as quoted, never run
  as markup.
- **Rules.** A standing order is shown as enforced only where the agent's hooks can
  really block it. Elsewhere it is guidance the agent may ignore.
- **History.** Session transcripts are copied to the station that ran them and stay
  there. Other agents and stations see a copy with secrets scrubbed by pattern, which is
  best-effort.
- **API keys.** With AgentVault connected, agents use keys through the vault and never
  hold them. See `docs/vault.md`, including what that does not protect against.

## Known limits

- Approving a station or a browser gives it control of every station. Check the code
  it shows matches the one you are asked to approve.
- A signed-in browser's token does not expire. Sign it out under Access in the Control
  Deck (or `spandeck trust --revoke`); stations that are off at the time learn of it only
  from a later sign-out. Joining another fleet replaces the key and signs every browser
  out.
- Signed station calls are accepted for two minutes, so stations' clocks must roughly
  agree, and a captured call could be replayed within that time by someone who can
  already read your Tailscale traffic.
- `remote_start = true` lets a station run a start command on your other machines over
  Tailscale SSH. It is off by default; what it may do there is governed by your
  Tailscale SSH rules, not by spandeck.
- An update is checked against the SHA-256 published beside it. That catches a damaged
  or swapped download, not a release that was malicious when published.
- No sandbox. An agent runs with your user's permissions; spandeck observes and can
  block tool calls through hooks, but a determined or confused agent is limited by your
  operating system account, not by spandeck.
- The Control Deck is served over plain HTTP inside the tailnet (Tailscale encrypts the
  link). Use `tailscale serve` if you want HTTPS.
- The local token and the vault token are files readable only by your user, not held in
  a keychain.
- Release binaries are not signed with an Apple or other vendor certificate. Each has a
  SHA-256 file and a GitHub build attestation.

## Checks that run on this repository

- `cargo deny`: known-vulnerable, yanked or badly licensed dependencies, on every push
  and daily.
- gitleaks: secrets committed by mistake, across the whole history.
- The test suite, including tests that plant made-up keys and check they are caught.
