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
- **A connection that goes silent is let go of, and nobody is trusted on a guess**
  (16 Nov 2026). Every connection a station holds is pinged: a Control Deck's is closed
  after 30 seconds with nothing heard on it, another station's connection for one pane
  after 6, and what it held (attached panes, watched conversations) is freed, so a client
  that connects and then says nothing cannot hold a station's resources for good. A
  page's own ping is answered only on a connection that has signed in; it is a frame,
  not a request, names no method, and its answer carries a number of milliseconds since
  the station started and nothing else. When the local Tailscale client does not answer
  the question "who is at this address", the station lets nobody in from another machine
  ("Tailscale is not answering here") and asks again at the next connection: it neither
  takes the caller for the owner nor remembers them as a stranger. What one station
  shows of a failed call to another no longer includes the address it called.
- **Channel numbers are not reused from one connection to the next.** What a Control
  Deck types names its pane by a channel number of its connection. Each connection
  starts counting at a random place, and a number it did not give is refused, so a key
  sent with a number from before a reconnect does not reach another pane.
- **Attachment names are chosen by the page and never reused** (16 Nov 2026). A page
  names each pane it attaches with a number it uses once in its life, across every
  reconnect, and the station takes what is typed only for a name attached on that
  connection: a key that carries a name from an earlier connection reaches no pane. A
  station refuses a name that is already attached on the connection, and refuses a pane
  without a name from a page that says it names them.
- **A page that reconnects replaces only its own earlier connection** (16 Nov 2026). A
  page sends at sign-in a value it made when it loaded (64 random bits, kept in memory
  only, never stored). A new connection closes an older one only when it was let in as
  the same caller and gives the same client id and the same value; through another
  station, also only for the same pane. The client id alone is not enough (a duplicated
  tab has it), an agent's token is never fenced and cannot close the owner's connection,
  and another station can close only connections it made itself. A connection that
  sends no such value is never closed by this rule. What the closed connection had
  waiting to be typed is thrown away, never written later.
- **What is typed is not delivered late** (16 Nov 2026). A page stamps what it types
  with its own clock and the receiving station refuses it when it arrives more than two
  seconds and twice the round trip after it was typed (ten seconds for a message from
  the message box), and takes a numbered key once. This guards the owner against a link
  that stalls and returns (keys typed minutes ago arriving in a shell), not against the
  sender: the stamp and the round trip are the sender's own word, a sender can only make
  its own input count as late or as fresh, and whoever can type into a pane could type
  the same thing now. What is refused is reported as a count, a number and an age,
  never as what was typed. The clock a station reads from a ping is a number of
  milliseconds with no date in it, kept in memory for that connection (for another
  station: for that station) and nowhere else.
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
  really block it. Elsewhere it is guidance the agent may ignore. Where one is
  enforced, it is not skipped because spandeck is not running: the agent's tool call
  waits (a minute; 8 seconds in a session started before 16 Nov 2026) and is then
  refused. See Known limits for what that rests on.
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
- **Answering an agent's question from the deck.** When Claude Code asks you a question
  or asks you to approve a plan, its hook can hold the call so that you answer it in the
  Control Deck instead of the terminal (`DECISIONS.md`, "Answer in place"). This is a new
  way for your browser to put something into an agent's tool call, and it is bounded:
  only a signed-in deck can answer (`question.answer` is yours alone: an agent is
  refused, for its own question too, and so are a station acting for itself and the
  read-only caller); the answer reaches only the hook that asked, in the pane that asked,
  once; and it is made of the question's own options, named by their place and looked up
  by the station in the call the agent made, plus what you type yourself (an answer of
  your own, or a note on a plan, 4,000 characters at most). Nothing else of the call can
  be changed from the browser. Nobody answers for you: when you do not answer in time,
  the agent shows its own prompt in its terminal. What was asked is part of your
  conversation, so it is not in the list of approvals, the notice or the ship's log,
  which only say that something is asked.
- **Answering a program's "trust this folder?" for you.** An agent program started in a
  folder it has not seen asks whether you trust it. You can tick "Trust this folder for
  <agent> if it asks" when you start the pane, or switch it on for a project
  (`DECISIONS.md`, "The folder-trust question"). This is one of the two places where
  spandeck presses a consent key for you, on a choice you made earlier (the other is the
  next entry), and it is bounded. The
  choice is yours alone: starting a pane with it, the list of folders and a project's
  switch are refused to an agent, to a station acting for itself and to the read-only
  caller, and a helper an agent asks for is answered only in a project you switched on.
  It is kept by the station, never in a project's files, so a repository cannot switch
  it on for itself. The key is Enter, pressed once, and only on the program's own first
  screen, word for word, for that pane's folder, within 20 seconds of the program's
  start, with the program's marker seen on "Yes, I trust this folder". A screen that
  says anything more (Claude Code lists what a folder's settings would pre-approve or
  run), a permission prompt, a login, or the same words later in a session are never
  answered (Codex's review of hooks has its own, narrower rule: the next entry). Every answer is in the ship's log and on the pane.
  The programs' own trust files are not written. **What you are granting** is the
  program's own trust, which is wider than it sounds: the program will read, edit and
  run files there, and with Claude Code the folder's own settings then apply without
  asking (permission rules, extra folders, helper commands), for the whole git
  repository the folder is in. **Do not tick it, and do not switch a project on, for a
  folder copied from somewhere you do not trust**: what is in it can then run on this
  machine as you. Claude Code's own choice on its screen is "No, exit": for a folder you
  ticked spandeck moves the marker to "Yes" itself, reads the screen again and only then
  confirms (your decision of 16 Nov 2026). `folder_trust.move = false` turns the moving
  off; then nothing is pressed there and you are offered one tap instead. The tick itself
  is still off for a folder you have never used.
- **Answering Codex's "Hooks need review" for you.** Codex runs no hook until you have
  reviewed it, and asks again whenever spandeck's hooks change. "Trust spandeck's own
  hooks" (the tick where you start a pane, the switch per station in Agent setup,
  `spandeck pane new --no-trust-hooks`) is **on unless you turn it off**
  (`DECISIONS.md`, "Trusting spandeck's own hooks"). It is the second and last place
  where spandeck presses a consent key for you. The choice on that screen, "Trust all
  and continue", trusts every hook Codex found, so spandeck first reads the files Codex
  takes hooks from and presses nothing unless they show that the hooks can only be
  spandeck's own: its hooks file holds exactly what this version of spandeck installs
  and nothing else, and there is no other source (no hooks in Codex's config, in the
  pane's folder or any folder above it, or in `/etc/codex`; no plugin switched on; no
  hook setting on the command line; no other `CODEX_HOME`). If any of that fails the
  pane says which, nothing is pressed, and no one-tap "trust all" is offered: you review
  them in the terminal. It is held to the same limits as the folder question (the
  program's own first screen, whole, within 20 seconds of its start, once, not after you
  typed). The choice is yours alone, is kept by the station and never in a project's
  files, and is refused to an agent, to a station acting for itself and to the read-only
  caller; a helper an agent asks for gets your standing choice as it is. Codex's own
  trust records are never written. **What it does not see**: a hook source that Codex
  has and this list does not name, and a file changed in the moment between Codex
  reading it and spandeck reading it. It is not used on Windows, and only the station
  that keeps the agent settings in your home answers at all.
- **API keys.** With AgentVault connected, agents use keys through the vault and never
  hold them. See `docs/vault.md`, including what that does not protect against.

- **What one agent can put into another agent's terminal** (16 Nov 2026). One line,
  written by spandeck: how many messages wait, who sent them, and to read them with the
  inbox tool and act on them. A message's text is never typed: it is read through the
  inbox tool, where it is labelled as another agent's words. The sender's name in that
  line is another agent's or another station's text, so it is cut to the characters an
  address is made of before it is typed: no Enter, no escape sequence and no sentence
  can ride in on a name. The line is typed again less and less often and then not at
  all, never on top of a copy that was not sent, so a program that cannot take a turn
  is not typed into for ever.
- **Only the station of your account writes into the agents' own settings in your home
  folder** (16 Nov 2026, after a test station with the real home folder rewrote
  Antigravity's spandeck plugin to point at its own binary). That station is the one in
  the usual state directory with the login service, or one whose own config file says
  `agent_home = true`. Any other station gives a pane what it needs on the pane's own
  command line or environment, or in a folder under its own state directory, and where
  a program can only be wired from the home folder (Antigravity) it starts the program
  without spandeck's tools and says so. This closes an accident, not an attack: any
  program running as you can write those folders.
- **spandeck does not answer an agent program's own prompts.** A sign-in, a quota
  screen, "trust this folder", "trust these hooks", "allow calling this tool" are
  shown as needing you, with the reason; nothing is typed at them. (One exception:
  "trust this folder", and only for a folder you chose beforehand; see "Answering a
  program's 'trust this folder?' for you" above.) The one thing that
  can stop Antigravity asking is your own command, `spandeck integration install
  antigravity`, which adds allow rules to Antigravity's own settings for four tools
  and no others: `inbox`, `send`, `reply` and `ack` of spandeck's own MCP server. With
  them an Antigravity agent reads and sends agent mail without asking you each time,
  as Claude Code and Codex panes already do; it still asks before every other tool,
  spandeck's included. No hook of spandeck's answers a permission for you.
- **Mail goes to the pane it was sent to.** When a pane is closed, what it had not
  read goes back to the senders; a pane later given the same name does not get it. A
  reply can mark as answered only a message that was sent to the pane that replies.

## Known limits

- A screen is recognised by words on it (16 Nov 2026). A program, or anything that
  prints in its pane, can show those words and make the pane read "needs you", or
  show a time in a quota message that is then shown to you as text. That costs your
  attention and holds back the pane's mail nudges while it lasts; it answers nothing
  and blocks nothing.
- The allow rules `spandeck integration install antigravity` writes rest on
  Antigravity's documentation as read on 16 Nov 2026 and have not been tried with the
  real program. If a later Antigravity names a plugin's server differently, the rules
  match nothing and it asks as before.
- What an agent's hook does while spandeck is not running rests on a file,
  `<state>/panes/<pane>.guard`, that says whether a rule is enforced for that pane. It
  is private to your account, and any program running as you can change or delete it,
  an agent included (an agent's own shell runs as you). Deleting or damaging it makes
  the hook refuse; rewriting it to say "not enforced" makes the hook allow. So this
  closes an accident (spandeck is down, and a rule silently stops applying), not an
  attack by an agent that sets out to get round a rule: such an agent could as well
  edit its own hook settings. Hooks are a guardrail, as their vendors say.
- While spandeck is not running, every tool call of a Claude Code pane with an
  enforced rule is refused, not only the calls the rule names: the hook cannot check
  the rule. Two checks are not covered at all and are not applied then: raw API keys
  in a command (with a vault connected) and sign-off before push. The agent's timeout
  for a hook is the vendor's to honour: if an agent cancels the hook sooner than it
  says, the tool call goes ahead. Codex, OpenCode and Antigravity are never refused
  this way (their rules are guidance).
- An approval's hook finds its approval again after a restart by a random id it made.
  A hash of the id is stored with the approval and the id is in no list. Any program
  with the pane's own token could ask with an id, but only for that pane, and only the
  hook that made an id knows it.

- A question an agent asks is the agent's own text. The card that shows it is drawn as
  text, never as markup, but what it says is still whatever the agent, or anything that
  runs as you with that pane's token, chose to write: read it as you would read the
  prompt in the terminal. What you type as an answer goes to that agent as its tool's
  result, as it would from the terminal. That the answer is taken as documented was seen
  with Claude Code 2.1.325; that a timeout of more than ten minutes is honoured rests on
  its hook reference. If a hook is cancelled early, the prompt appears in the
  terminal and the card can stay for up to ten minutes with nobody behind it.
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
- A machine that cannot download the release is sent it by the station that adds it,
  inside the same SSH session (17 Nov 2026). What that changes, and what it does not:
  - **No new power for the adding station.** It already runs a script of its choosing on
    that machine as your user; a station that has been taken over could install anything
    there before this, and can now. Sending the program adds no way in and opens no port.
  - **Where the release host can be reached from the adding station, what is sent is the
    published archive, checked against the SHA-256 published beside it**, by the same
    code as an update, from the address in the code or in that station's own config. A
    download that does not match is not sent. A copy kept from an earlier add is checked
    again against the SHA-256 published now before it is sent; when the release host
    does not answer, against the one it was checked with when it was downloaded. Someone
    who can change files in a station's state folder can change that copy and its
    checksum together, and can as easily change the station.
  - **Where it cannot be reached, a machine of the same kind is sent the station's own
    running program**, and the dialog says so. That copy is trusted exactly as far as the
    station is: it has no published checksum to be held against, and a station whose
    program was tampered with passes the tampering on. The new machine checks only that
    what arrived is what was sent.
  - **Never older than the station that sends it**, so a stale or lying copy of the
    release page cannot have an old release with a known fault sent to a new machine.
    (A machine that downloads for itself takes what its release page calls newest, as
    before.)
  - **The new machine decides nothing** but which of five platforms it is, and that must
    agree with what Tailscale says it runs. It cannot name an address to download from, a
    file to send, or a version, and it is answered once.
  - On the new machine the archive is written to a temporary folder of that user's,
    checked, and deleted; a transfer that is cut short or altered installs nothing.
  - The Windows side of this has not been run on a real machine (DECISIONS.md).
- A station that cannot reach the release host is supplied its update by a station of
  its fleet that can (17 Nov 2026), and a station adding a machine may get another
  kind's release the same way.
  - **Whom it believes.** The station being updated cannot check the release host
    itself, so it believes a station of its fleet that says "this is the SHA-256 I
    checked against the published one". That is no new power: every station of a fleet
    holds the same key and can already act as you on every other. A station of your
    fleet that has been taken over could already run anything on the others; it can now
    also hand one of them a bad program when that one asks for an update.
  - **What it checks anyway.** That the bytes have the SHA-256 that was offered; that
    what it is given is the version and kind of machine it asked for; that it is not
    older than what it runs, unless you named that version; and, when more than one
    station holds or can fetch the release, that they all give the same SHA-256, or
    nothing is installed. So one station with a spoiled or swapped copy is found out
    wherever a second one can supply the same release; a fleet of two has no second.
  - **What a station hands on.** Only an archive it checked against its published
    SHA-256 (a station that was itself supplied keeps no copy to hand on), from the
    release host in the code or in its own config. The exception is when no station can
    reach the release host at all: then a station of the same kind sends its own running
    program, and the notice says it is that station's own copy. That copy is trusted as
    far as that station is.
  - **Who may ask.** Only a station of the fleet, with a call signed with the fleet's
    key. No agent and no read-only caller has these calls, the owner's own connection is
    refused them, and a browser cannot reach them. Asking makes a station download a
    release from its own release host and nothing else; it names one of five platforms
    and a version, never an address or a file.
  - It is the station that updates that asks. Nothing lets one station push a program
    onto another.
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
