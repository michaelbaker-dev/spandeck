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
    only when you approve its request on a station that already has it, or when a
    station of its own fleet, which you approved, hands it the key that fleet moved to;
  - a **browser** holds a token it was given when you approved it, once, on a station
    or in a Control Deck that was already signed in;
  - on the station itself, the local token from `spandeck cc`.
- **What that means.** A program on one of your devices that has neither the fleet key
  nor a token gets nothing. Anyone who can run code as your user on a station can read
  that station's key and tokens, and so controls the fleet.
- **Requests are shown on every station, and only you answer them.** A station that
  is asked tells the others in its fleet who is asking and the code to match, so you
  can approve where you are. It does not send them the secret the asker collects with,
  or the key: your answer is passed to the station that was asked, and it decides. An
  agent has no call that answers a request, on any station.
- **What stations tell each other is data.** Stations of a fleet share which
  stations are its members, the notices they raise and the documents every station
  keeps (layouts, looks, agent setup, preferences). A station takes these only from
  a station that signs with the fleet key, checks and bounds what it is told, shows
  it as text, and accepts a member's address only if it is one it would look at by
  itself: an address in its own config, or a device under your Tailscale login. A
  station cannot make another one send a call or a key to an address it chose.
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
- Approving a station approves its whole fleet: every station that holds that fleet's
  key joins with it. The request lists the names the asking station gives, but those
  are only what it says; a station it did not name, or one that is off and still holds
  that key, joins too (for 30 days). Approve a station only if you would approve every
  machine it has shared a fleet with.
- Any station of the fleet can pass on your answer to a request, because any station
  already holds the key and can act as you on the others. The relay adds no power a
  station did not have.
- A signed-in browser's token does not expire. Sign it out under Access in the Control
  Deck (or `spandeck trust --revoke`): every station that is on refuses it at once, and
  a station that is off is told when it is next seen, by whichever station sees it
  first. Until then, that station alone still accepts the token. A sign-out is kept for
  as long as the key that made the token is in use.
- "Open X's own Control Deck" hands this browser's token from one station's deck to
  another's as a message between the two windows. The token is never put in an
  address, so it is not in the browser's history. It is sent only to the window that
  deck opened, and only while that window is at the address the other station gave in
  an answer signed with the fleet key. The other deck keeps a token only after its own
  station has accepted it. A page that opens a deck and sends it a token can only sign
  that browser in with a token the page already holds.
- Joining another fleet replaces the key on the station that asked
  and on every station of the fleet it was in, and signs out every browser that was
  signed in to those. The list of browsers you signed out travels with the key, so a
  station that arrives refuses them too; a station on an older version ignores the list,
  so update it, or sign the browser out again after it joins.
- Every station of a fleet holds the same key. Removing a station therefore gives the
  stations that stay a new key, which also signs every browser out. A station that is
  off at that moment keeps the old key until it is seen again, and until then the
  removed machine could still act on it. Removal does not undo what a machine did while
  it was trusted.
- Signed station calls are accepted for two minutes, so stations' clocks must roughly
  agree, and a captured call could be replayed within that time by someone who can
  already read your Tailscale traffic.
- `remote_start = true` lets a station run a start command on your other machines over
  Tailscale SSH. It is off by default; what it may do there is governed by your
  Tailscale SSH rules, not by spandeck.
- The same switch (`add_machines`) covers two more things a station does over SSH when
  you ask from the Control Deck: making a station that is already running ask to join
  this fleet, and uninstalling spandeck on a station. Uninstalling stops spandeck there,
  removes its login service and deletes its programs; its history and settings are
  deleted only if you tick "Erase everything". It deletes spandeck's own folders and
  nothing else: your project folders are not touched. An agent cannot ask for either,
  and neither can another station.
- `add_machines = true` lets a station connect to another of your machines over SSH and
  install spandeck there when you ask it to from the Control Deck. It is off by default.
  A password you type for it is used for that one connection and kept nowhere, but it
  does pass through the browser and the station's memory on the way. The first
  connection to a machine trusts the host key it shows (and the Tailscale address it
  was reached at); a later change of key is refused. The install is the README's
  script: the release is checked against its published SHA-256, as below.
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
