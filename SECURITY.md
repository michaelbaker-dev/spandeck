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
- **The deck over HTTPS** (17 Nov 2026). Where Tailscale gives the machine a
  certificate for its own name (`<machine>.<tailnet>.ts.net`), spandeckd also answers
  TLS on a second port (7392) and ends the TLS connection itself, so the caller's real
  address is still what is checked against your Tailscale login; `tailscale serve` is
  not used. The plain port is unchanged and nothing is redirected unless you set
  `[web] redirect = true`. No `Strict-Transport-Security` header is ever sent: it
  belongs to a host name and would turn every plain-HTTP address of that machine, on
  every port, into HTTPS in your browser. The certificate's key is fetched by each
  station for itself, kept in its state folder for that user only, never logged, in no
  answer of the API and never sent to another station. `[web] https = false` turns it
  off and deletes the key.
- **The one other origin a station accepts.** A browser's token was already taken by
  every station of the fleet. Since 17 Nov 2026 a page that was loaded from one
  station's deck may open its connection to another station when its own stops
  answering. That station accepts the other origin only on two WebSocket addresses
  (`/api/ws`, and `/api/ping`, which says who answers and nothing else), only when the
  origin is exactly the deck of a member of its own roster (by the address it itself
  looks at, or by the name its own Tailscale client gives that address and the port
  that member announced), and only with a browser's own token: never the station's
  local token, a pane's token or a station's signature. No cookie is used anywhere, so
  another site cannot ride on your sign-in. `/api/rpc`, `/api/peer/rpc`,
  `/api/device/*`, uploaded fonts and every file still refuse other origins, and no
  answer carries `Access-Control-Allow-Origin`. `[flags] page_failover = false`
  refuses the other origin again.
- **The copy of the deck saved on a device** (HTTPS and localhost only) holds the
  page's own files: exactly the list the station's build names. It never holds an
  answer of the API, what a terminal shows, or anything with a token; it adds nothing
  while the page runs.
- **The machine's password, for one command.** On Linux Tailscale gives the
  certificate only to root or to the user it was told to trust. "Turn on secure access
  (HTTPS)" in the deck asks for your password on that machine and runs exactly
  `sudo -S -k -p '' tailscale set --operator=<the station's own user>` there: an
  argument vector, no shell, the user name read from the system, `sudo` and
  `tailscale` taken only from system folders and only if they are root's own files.
  The password goes to sudo's standard input and nowhere else: never logged, never
  stored, not in the ship's log (which records each use and its outcome), wiped when
  the command ends. It is taken only by the station it is for, from a page connected
  to that station, and is never passed from one station to another; an agent, another
  station and a read-only caller cannot make the call. Three wrong tries a minute.

- **Asking whether an agent program is the latest** (17 Nov 2026). When you open
  Agent programs, run `spandeck agents`, or press "Check again", each station asks the
  release host of each program it has for the newest version number: a plain GET of a
  public address (`downloads.claude.ai`, `releases.openai.com`, `opencode.ai`, Google's
  updater host for Antigravity), or `npm view` / `brew info` for a copy installed that
  way. The request carries nothing about you, the station or a project; what the host
  learns is that a machine at that address asked for a public version number. Nothing
  is asked in the background or when an agent starts, an answer is reused for three
  hours, and nothing is downloaded or installed by asking. What comes back is read as
  a version number or thrown away, and is shown as text. `[agents] check_latest = false`
  in a station's `config.toml` turns it off on that station. Only you can make a
  station ask (`agents.check`): an agent, a read-only caller and another station
  cannot.

- **What a pane's header says at a glance** (19 Nov 2026). While a Control Deck shows
  a station's panes, that station runs `git status` in each pane's folder (once per
  folder, at most every ten seconds, for two seconds at most) and reads how much
  processor and memory each pane's programs use. Nobody asked for that `git status`,
  and a pane can be in a folder copied from anywhere, so it is run so that it runs
  nothing the repository names: `core.fsmonitor` (a command in a repository's own
  settings that `git status` would start) is switched off for the call, and it takes
  no lock there. A branch's name comes back as data and is shown as text. The call
  that serves all this (`pane.glance`) is not on the agents' list: an agent reads pane
  views, and learns no other project's branch or load from spandeck. The update chip
  is made from what the station already holds; a header never makes a station ask a
  release host.

- **Asking whether a newer spandeck is published** (19 Nov 2026). About once a day, and
  when you choose "Check for a newer spandeck now" or run `spandeck update --check`,
  each station asks its release host (GitHub, or `[update] url` in its config) for the
  newest release's name: one GET of `<releases>/latest`. The answer is a redirect, which
  is read and not followed. The request carries nothing about you, the station or a
  project, and not the version the station runs; what the host learns is that a machine
  at that address asked for a public version number. Unlike the check of agent programs,
  this one is made in the background. Nothing is downloaded or installed by asking: an
  update is still your act, and still checked against its published SHA-256. What comes
  back is read as a version number or thrown away, and is shown as text. The link to the
  release's page is made by the station from its own release address and that version,
  never from an address in the answer or one another station gives. What another station
  says it learned is read the same way. `[update] check = false` in a station's
  `config.toml`, or the switch `update_check = false`, turns it off on that station.
  Only you can make a station ask now (`update.check`): an agent, a read-only caller and
  another station cannot. A station's `station.info`, which an agent in a pane may read,
  says what was learned: the public version number, or that the host could not be
  reached and its name.

- **A station reached over your own SSH** (a link, 20 Nov 2026). Where there is no
  Tailscale, a station can show another station's Control Deck by running your own
  `ssh` to it. Only a station whose `config.toml` has a `[[link]]` does any of this; a
  station without one starts nothing and listens nowhere new. What is new, and how each
  part is kept safe:
  - *spandeck runs `ssh` by itself.* The host and the command are named only in that
    station's `config.toml`, which you write or `spandeck link add` writes at your
    command line. No call of the deck, of an agent or of another station can
    add or change a link. `ssh` is run with options that come before yours and win:
    nothing is forwarded (no ports, no agent, no X11), no command of your configuration
    is run, no terminal is asked for, nothing can prompt (`BatchMode`), and a host whose
    key is unknown or has changed is refused, never accepted for you. Your own
    `~/.ssh/config` and `known_hosts` are used. spandeck stores no password, key or
    ticket. The command that was run is in the ship's log.
  - *A helper on the far side* (`spandeck link --stdio`). It adds no authority: it
    reaches its station as any `spandeck` command there does, through the station's
    private local socket with the token only that user can read. Whoever can run it
    could already run every other command. It opens no port. If no station is running
    there it starts one (your login service there, where it has one), and says so; it
    installs, updates and replaces nothing.
  - *A parser that faces the far side.* The link's frames are a small fixed format with
    a longest frame, a largest number of streams and a window for each stream; anything
    outside them ends the link. The station you are at opens streams and refuses every
    stream the far side tries to open. It runs nothing the far side sends.
  - *One more listening port for each link*, on `127.0.0.1` only. What answers there is
    the far station itself, with its own rules: every call needs a token, the Host and
    the Origin are checked. The far station answers to `localhost` with that port only
    while the helper is attached. While the link is down the port answers with a short
    plain-text sentence and nothing from the far side.
  - *A browser entry made over a link* (`spandeck link open`). Made at the far station
    at the word of the helper, which is you, as that user, on that machine. It is an
    ordinary entry: listed under Access, signed out like any other. The address you open
    carries no token: it carries a code the far station exchanges for the token once,
    within two minutes, with a caller on its own machine. The far station's own token
    never leaves it, and no token passes through the station you are at.
  - *Who may ask.* The calls of a link are taken only on a station's private local
    socket and only with that station's own token. An agent, another station, a
    signed-in browser and a request passed on between stations are all refused them.
  - *`spandeck link add`: an `ssh` you answer yourself, run by spandeck at your command
    line.* It is a command, not a call: nothing in the deck, no agent and no other
    station can start it or name its host. It runs your own `ssh` once, with your
    `~/.ssh/config` and `known_hosts`, with forwarding off and no command of your
    configuration's, and this once without `BatchMode` and without insisting the host's
    key is known: you are at the terminal, and what `ssh` asks (a password, a code,
    whether to trust a key) it asks you. spandeck reads none of your answers and keeps
    nothing. Only a destination and ssh's own options are taken from what you type: a
    command for the far side, and options that would turn ssh into something else
    (`-N`, `-f`, `-t`, `-W` and the like), are refused before anything runs. What the
    far machine prints is text: cleaned, cut short, printed, never run. The one thing
    taken from it is the name of its platform, which must be one that has a release.
  - *A release sent to a machine you named.* Where the far machine has no spandeck, the
    station on your machine gets the release for that platform exactly as Add a machine
    does (the published archive, refused unless it matches its published SHA-256; a
    copy kept from an earlier download; its own program only for a machine of its own
    kind; never anything older than itself) and your command line writes it into the
    ssh session. It is checked by SHA-256 on arrival and installed under your home
    folder there. The far machine downloads nothing. The call that hands over the
    release (`link.release`) names a platform and nothing else, answers with a file on
    your own machine, and is yours alone at the command line, as is the one that reads
    the config file's links again (`link.reload`). Your command line checks that the
    file is the size and SHA-256 the station said before it sends it. A spandeck that is
    already there is replaced only when you say yes; the default is No. As for an update
    through the fleet, releases are not signed: the far machine is protected by the
    check your own machine made against the release host, and by the ssh session.
  - *On the far machine it changes only your home folder*: the program in
    `~/.local/bin`, a config file only where there is none, a systemd user service or
    one marked line in your crontab (shown to you, never written twice, never written
    when the crontab could not be read), and with `--uninstall` it removes them again.
    It asks for no administrator and uses none.
  - *`[pane_env]`: values at rest.* A station's `config.toml` can name variables that
    every pane's process is given (a company proxy, the company's certificates). A
    value may be a credential, such as a proxy password. It is kept in that file and
    nowhere else: it is not logged, not shown, in no answer of the API and not in the
    pane's record in the database; `spandeck doctor` names the variables and never their
    values. `link add` copies such variables from your login shell on the far machine
    only when you say yes (the default is No), names them to you by name only, and has
    the far machine's own spandeck read and write them: no value crosses the link or
    appears on a command line. It leaves the far config file readable by you alone. A
    config file you wrote yourself keeps the permissions you gave it: if you put a
    password in `[pane_env]` by hand, make the file yours alone (`chmod 600`). The
    programs in a pane have these variables as they have the rest of their environment,
    and can print them: an agent in a pane can read a proxy password it is given.
    Names beginning `SPANDECK_` are refused, so the table cannot replace a pane's token
    or socket.

  **What a far side that has been taken over can do to the machine you are at:** serve
  a hostile page at one `localhost` origin in your browser. That page can show false
  information, read what you type into that page, and use the browser's token for that
  far station, which whoever took it over controls already. It cannot open a stream to
  your station, read your station's token or another link's (each link is an origin of
  its own), reach your files, make `ssh` forward anything, or make spandeck run
  anything. The two stations are not one fleet: neither holds the other's key.

## Known limits

- With a link, a browser on your machine holds a token for the far station. Whoever can
  use that browser profile can use that station, until you sign the entry out there.
- A station reached over a link still has its web listener on `127.0.0.1` of its own
  machine. On a host other people can log in to, they can connect to that port; every
  call there needs a token, and the open calls (a station's name and version, a request
  to sign in that only you can approve) are as on any station. A station with no TCP
  listener at all is not built.
- A link is as private as its carrier. Over `ssh` that is SSH's encryption. A carrier
  you name yourself (`cmd`) is yours to judge.
- `spandeck link add` trusts the machine it is run at: a near machine that has been
  taken over can send the far one anything, as it could over your `ssh` without
  spandeck. During its one session `ssh` may accept a host key at your word; spandeck
  cannot tell whether you looked at the fingerprint.
- A password in `[pane_env]` is in a file in plain text, and in the environment of
  every program in every pane of that station. Use it where the proxy leaves you no
  other way, and prefer a proxy that needs no password in its address.
- `link add` has been run only against a stand-in for `ssh`: not against a real `sshd`,
  a bastion, a real `crontab` or a real user service manager.
- Turning HTTPS on at a station publishes its name, `<machine>.<tailnet>.ts.net`, in
  the public certificate-transparency logs, for good: the machine's name and your
  tailnet's. The names resolve only inside your tailnet and nothing becomes reachable
  from outside it.
- The password you type for "Turn on secure access (HTTPS)" passes through the
  browser and the station's memory. On the way to a station at its plain address it
  is protected by the tailnet's own encryption (WireGuard, device to device), like
  everything else the deck sends there, and by nothing else.
- After that command, programs running as your user on that machine, your agents
  included, can change Tailscale's settings there without a password (`tailscale
  set`, `up`, `down`, `serve`, `logout`). That is what Tailscale's "operator" means;
  spandeck needs only the certificate. Undo it in the same dialog, or with `sudo
  tailscale set --operator=` at the machine. (A narrower right, `TS_PERMIT_CERT_UID`
  in tailscaled's own environment, exists; it needs root and a restart of tailscaled
  at the machine, and spandeck does not set it.)
- A deck at `http://<machine>.<tailnet>.ts.net:7391` keeps its token for that plain
  origin. On a network where Tailscale is off and something else answers for that
  name, that something could serve a page at the same origin and read the token; it
  could use it only from a device that is on your tailnet. The HTTPS origin is not
  open to this (nobody else can have a certificate for the name); prefer it.
- With `page_failover` on, a page at the deck origin of any member of the fleet can
  open a connection to any other member. A member's machine is already trusted with
  the fleet key; what this adds is that a page served from that exact address and
  port (by whatever answers there) could try, and it still needs a browser's token,
  which only your signed-in browser has for that origin.

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
