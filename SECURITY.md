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
- **A page cannot tie a station up.** A Control Deck's requests are read in order and
  run side by side, so that typing and Stop never wait behind a station that does not
  answer. That decides only when a request runs: who may call what is checked exactly
  as before, for every request, and no caller gained a method. One connection may have
  256 requests waiting or running for the station it is open at, and fewer, counted
  apart, for each other station; past that a
  request is refused at once and nothing is done. What is sent to one pane, and the
  changes made to one station, keep the order they were sent in.
- **What stations tell each other is data.** Stations of a fleet share which
  stations are its members, the notices they raise and the documents every station
  keeps (layouts, looks, agent setup, preferences). A station takes these only from
  a station that signs with the fleet key, checks and bounds what it is told, shows
  it as text, and accepts a member's address only if it is one it would look at by
  itself: an address in its own config, or a device under your Tailscale login. A
  station cannot make another one send a call or a key to an address it chose.
- **A station that is slow or away costs only itself.** What a station does with
  another station it does in a task of that station's own, cut off after eight
  seconds, never two at once, and less often while that station keeps failing. So a
  machine that holds its connections open, on purpose or not, delays its own share of
  the work and nobody else's. A station's signed call makes this station ask it again
  at once, at most every two seconds, and only at the address it is already listed at;
  an unsigned call changes nothing about when or where this station looks.
- **Channel numbers are not reused from one connection to the next.** What a Control
  Deck types names its pane by a channel number of its connection. Each connection
  starts counting at a random place, and a number it did not give is refused, so a key
  sent with a number from before a reconnect does not reach another pane.
- **Agents are not the owner.** Each pane has its own token. Agents can message, delegate
  and search history; they never get a call that administers a station. Text from agents,
  other stations and session history is shown as text and labelled as quoted, never run
  as markup.
- **One answer for what a deck shows of a station** (`deck.snapshot`, 15 Nov 2026) gives
  nothing that was not already given: it is the seven lists a deck read before, by the
  same functions, and for the station serving the deck four documents it also read
  before, all of them on the read-only list. No agent can ask it. The versions in it are
  short hashes of that same content, used only to say "unchanged"; nothing is decided by
  one, and a caller that sends a wrong one is only sent the section again.
- **A stopped agent stays stopped, and only you release it.** An agent you stopped, and
  one spandeck started again after a restart, gets no messages from other agents until
  you release it: by typing into its pane, with Release, or with
  `spandeck pane release`. `pane.release` is yours alone: an agent's token is refused,
  for its own pane too, and it is not among the calls a station makes for itself (a
  station of your fleet can pass on a request as you; see Known limits). So an agent
  cannot wake itself or another agent that you stopped. What a browser's terminal
  sends by itself (focus, the mouse, its answers to the program) does not count as you
  typing. A message for a held agent is stored and waits; it is not dropped.
- **Rules.** A standing order is shown as enforced only where the agent's hooks can
  really block it. Elsewhere it is guidance the agent may ignore.
- **History.** Session transcripts are copied to the station that ran them and stay
  there. Other agents and stations see a copy with secrets scrubbed by pattern, which is
  best-effort.
- **A pane's conversation.** A signed-in Control Deck can ask a station for an agent
  pane's conversation as text (`docs/conversation-api.md`): what was said, each tool
  call and its output. This is the transcript as written, not the scrubbed copy, so it
  is yours alone: an agent's token is refused, even for its own pane, and it is not
  among the calls a station makes for itself. When the deck is served by another
  station, that station passes your request on and the answer back, as it does a
  pane's terminal, and keeps none of it. (A station of your fleet can pass on a request
  as you for any call; see Known limits.) Text in it is data: the station drops terminal control sequences and returns
  text, never markup.
- **API keys.** With AgentVault connected, agents use keys through the vault and never
  hold them. See `docs/vault.md`, including what that does not protect against.

## Known limits

- A pane's conversation holds whatever its terminal showed, including a secret an agent
  printed or a file it read. Any signed-in browser can read it, and it crosses your
  tailnet to that browser unencrypted by spandeck (Tailscale encrypts between devices),
  as the terminal already does. The scrubbing that protects history from other agents
  does not apply to what you read yourself.
- The conversation is built from Claude Code's and Codex's own session files, which
  their vendors call internal. After an agent update it may show less than happened; it
  says how many records it did not understand, and the terminal is always complete.

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
  already read your Tailscale traffic. A call refused only for its time is told so
  ("clock"), and only when its signature was made with the fleet key; a station also
  says its clock in the unsigned `station.hello`, to any device under your login.
- A fleet stop is aimed using the stopped station's own statement of its clock, from
  that unsigned `station.hello` (15 Nov 2026). The stop itself (`stop.apply`) is still a
  station's signed call, and `stop.all` is still yours alone: no agent has either. A
  machine that answers at a station's address with a false clock can shift which of
  that station's own turns the stop takes, and nothing else: the station never takes
  the moment to be later than the stop's age gives, so the worst a false clock does is
  leave a turn running that should have been stopped, which a machine in that position
  could do anyway by not answering. An answer that names another station is not used.
- Counters for the Performance page and the list of your Tailscale devices with their
  key expiry dates (`perf.get`, `tailnet.view`, `tailnet.report`) are for you and for a
  station's signed call. No agent has them, and `station.info`, which an agent can
  read, holds none of it. The counters hold method names, station names, times and
  counts; never a parameter, a message, a path or a pane's contents.
- Sending a failed task's agent back to it (`task.continue`, "Continue this task") and
  reading or setting a station's switches (`station.flags`) are yours alone. No agent
  has either, another station has neither, and the read-only caller cannot read the
  switches; from a Control Deck opened on another station they arrive as your own
  request, like a restart. `task.continue` types one fixed line into the task's pane
  and nothing a caller wrote; it is refused for a paused pane.
- After a station ends without being stopped, the next start ends what a pane's program
  left running, for a pane it starts again. It ends a process only when the pane's
  record of the program's pid and start time, the process's session and its own start
  time all agree, so that a process which was later given the same number is not
  touched; where they do not agree it does nothing. What a pane that is not started
  again left running is never ended, only named to you. A program's name in that notice
  is text from the machine: bounded, and shown as text.
- On Windows the station can be stopped, and the program that keeps it running told to
  make way for a newer copy, through named objects of your Windows user, with no token
  (`spandeck service --stop`; 15 Nov 2026). That is what a signal is on macOS and Linux: any
  program running as you can do it, an agent in a pane included, as any could already end
  the process. Another account on the machine cannot. The program that keeps the station
  running starts only the `spandeck.exe` beside it, and follows a process only if the
  station's own pid file or the station itself, asked with the token, names it.
- Requests to join or sign in that were waiting are handed to the daemon that takes over
  at an upgrade, in the same private file as the terminals. They hold the hash of what
  the asker collects with, never the key or a token, and one already approved is not
  carried.
- What the database had not taken when the daemon is replaced at an upgrade (it was
  locked, or the disk was full) travels in that same private file and is written by the
  daemon that takes over: pane records with their pane tokens (the file already held
  those), stops, answers to approvals, notices, entries of the ship's log. The file is
  readable only by your user and is deleted once read. A station that cannot write its
  database refuses new messages, tasks and panes; it never refuses Stop, Pause, Kill or
  an answer to an approval, and what those change is held in memory until it can be
  written: if the daemon ends first, a pause or the record of who approved is lost, and
  you were told it was not saved (15 Nov 2026).
- A station keeps the answers to changes for two minutes, so that a command or an
  agent's tool that asks again after losing its connection is given the first answer
  and the change is made once (15 Nov 2026). They are rows of the station's own
  database (`kv`, keys beginning `rid:` and `stoprid:`), readable only by your user:
  the method, a SHA-256 of the parameters (never the parameters), the answer, the time.
  What is in them is what the caller was already told: a pane's description, a
  message's id, the messages an `inbox` call returned (which the database holds
  anyway), a project's description. Who can be answered from a row: only the caller it
  was made for (you, from any of your clients or through any station of your fleet; or
  one agent's pane, told apart by its token, never by anything the request says), and
  only for the same method with the same parameters. An id used for anything else is
  refused. An agent cannot read or plant a row of yours or of another agent, and its
  own hook is never answered from a row. A station of your fleet can ask as you, as it
  always could (`rpc.forward`); it gets nothing from a row that it could not get by
  asking. Never in a row: an answer that carries a token (`fleet.remove`,
  `fleet.uninstall`), a call whose parameters are a password (`fleet.add`: not even
  their hash), what the vault's proxy returned, the agent setup, what was typed or
  pasted, a hook; and any answer with a field named like a secret is dropped before it
  is written. The ids are 128 random bits made by the calling program. Rows are
  bounded (1,024 for one caller, 8,192 for the station, 32 MB of answers; an agent that
  fills its own cannot fill yours) and deleted after two minutes; a
  station killed in between leaves them until it next runs. The same rows, when the
  database had not taken them, travel to the next daemon at an upgrade in the private
  file described above. Known limit: a second try is believed about how long ago its
  first was made; a caller that lies about it can only cause its own change to be made
  twice, which it could do by asking twice.
- A station tells the sender of a message when another station refused it for good
  (15 Nov 2026): a note in the sender's own inbox, from `spandeck@<station>`, and a
  notice to you. The reason in it is the other station's words: it is cut to 200
  characters, stripped of control characters, and stored and shown as text. The note
  gives an agent nothing: it is a message like any other, which agents are told to
  weigh and not to obey, and it names only a message that agent itself sent. Known
  limit: a pane can be named `spandeck`, so the name alone does not prove a note is the
  station's; nothing is decided by it.
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
