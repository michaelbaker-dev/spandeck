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
- **Who is trusted.** A connection over Tailscale is identified by asking the local
  Tailscale daemon who owns the connecting address. Only the same Tailscale login that
  runs the station gets control. Identity headers are never trusted. A connection from
  localhost needs the station's local token.
- **What that means.** Anyone who can sign in to your Tailscale account, or run code as
  your user on a station, controls every station. Other people's devices on a shared
  tailnet do not.
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
