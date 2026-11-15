# spandeck decisions

Read this before starting any task. It is a condensed copy of the Decisions tab of the
spec doc (https://claude.ai/artifact/QT8LDppx4qGnuttvvMwmPt). Where it disagrees with
anything else in the repo or the Features tab, this file wins. Copied 13 Nov 2026.

## Decisions

- **Rust, our own daemon, written from scratch.** herdr and Wolfpack are read for lessons, not forked.
- **v1 runs on macOS and Linux.** Windows is being added (see Windows, below); platform code stays isolated (`crates/terminal/src/sys/`).
- **Name**: spandeck; daemon spandeckd; the command center is the Control Deck; machines are stations, agents are crew, the event log is the ship's log.
- **Live handoff on upgrade, no per-pane host process.** The old daemon execs the new binary in place (same pid, inherited PTY descriptors). After a crash or reboot each agent resumes its conversation by session id.
- **Stations under the same Tailscale login find each other automatically, and each joins the fleet on the owner's approval, once** (changed 14 Nov 2026; before, they trusted each other with no step). There is one fleet: a station that joins brings its fleet-mates, and a request can be approved at any station (15 Nov 2026). Tailscale identifies a device, not a program on it, so a station also signs its calls with a fleet key and a browser on another device holds a token the owner approved. An agent reaching another station gets only what its task grants.
- **Agent messages are stored records** sent and received through tools (send, inbox, reply, ack), never keystrokes. A typed one-line nudge only wakes an idle agent.
- **A durable task record** exists for delegated, scheduled and recipe work. A pane you are just talking to has none.
- **Workspaces**: the agent you talk to stays in the main checkout; a delegated helper that writes gets its own worktree and branch; review-loop reviewers read a snapshot at the commit; testers read the same tree. Sign-off is bound to the commit reviewed.
- **Shared project memory** (start digest, findings board, automatic capture from hooks) on by default.
- **Session archive**: spandeckd copies every Claude Code and Codex session file as it grows, keeps raw lines plus a normalised copy, keyword search through `spandeck recall`. Own history always searchable; same-project on by default with secrets scrubbed; cross-project opt-in.
- **Standing orders at five layers** (fleet, group, station, project, role), as guidance and as enforced rules.
- **Review loop is the first recipe**: three rounds, stall check, sign-off before push.
- **Activity strip on every pane.** Tree, timeline, heat view are v1.5.
- **Usage per agent** (the owner's request on 14 Nov 2026; before, usage and cost views were not in v1): each pane shows its agent's model, tokens and context, with an estimated cost; the station and the fleet show totals. See Usage and cost.
- **Phone**: status, blockers, approval requests and stop controls; and, since 0.2 (the owner's request on 14 Nov 2026), talking to any pane: a message box that can be typed or dictated into, the keys a phone lacks, fitting the pane to the screen, and starting a pane. In the browser; no native app.
- **Station-to-station SSH** uses short-lived certificates spandeck issues per task (v1.5, with cross-station delegation).
- Not in v1: desktop shell, native iPhone app and widget, Officer of the Deck, fleet updates and channels (of spandeck itself), common config, cross-station delegation, flow view, agent tree and timeline. (OpenCode and Antigravity adapters were on this list until 14 Nov 2026: see Agent programs.)
- **Planned, not built: a browser pane** (the owner's decision on 15 Nov 2026: "add the web page stream to the future plan"). A Chrome runs on the station, beside the agent and its dev server, and what it shows is streamed into a pane of the Control Deck, so it works for any site and from the phone. The agent drives it through tools (open, read, click, type) and the owner watches or takes over. To settle when it is built: the browser has a profile of its own, never the owner's sign-ins; the owner approves which sites an agent may act on; an agent's browser is its own, and another agent reaches it only if its task allows; the streamed page is kept apart from the Control Deck so that a page an agent built cannot act as the owner; what happens on a station with no Chrome. A pane that only frames a web page in the owner's browser was considered and set aside: most sites refuse to be framed, and it cannot see a station's localhost.

## Pre-build resolutions (13 Nov 2026)

- One `spandeck hook` command for every vendor. Claude Code: hooks first. Codex: hooks where accepted, screen detection otherwise; the Codex adapter is fixture-tested and unverified until a live canary runs.
- Remote approval is in v1: the permission hook holds for an answer from the Control Deck or phone, then falls back to the terminal prompt on timeout.
- spandeckd listens on localhost and the tailnet address. Tailnet connections are identified by a Tailscale whois on the real address and must be the owner's login; they then need the fleet key (stations) or an approved token (browsers). Localhost needs a token. Serve identity headers are never trusted.
- Every pane gets a pane-scoped token. Enforced on cross-station calls; advisory on the same station.
- OSC 7501 is read when present, never required.
- The activity strip shows state, current tool and elapsed time, and since 14 Nov 2026 the model and a context gauge (before: no context gauge or cost).
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

### One fleet: a join is a merge, and a request is answered at any station (15 Nov 2026, the owner's request)

"Should never have multiple fleets, every spandeck should be the same." "Shouldn't all machines get the request? ... you should be able to approve from any machine you're on at the time." What happened: a station that had two fleet-mates asked to join a lone station, took its key and left its mates behind, so there were two fleets.

- **A station that joins brings the stations of its fleet with it**, whichever side asked. After the approval the asking station takes the new key, keeps the old one in `fleet.retired`, and hands the new key to every station of the fleet it was in, in a call signed with the old key (`fleet.rekey`, the call station removal uses, with no station named as removed and the fleet joined named instead). It does so the next time it looks for stations, which it does at once. A station that was off is handed the key when it is next seen at the address it had, for 30 days, exactly as after a removal; after that it joins on the owner's approval. Each station that is handed the key keeps the old one for the same purpose, so a station that only one of them can reach is still told.
- **The key is taken first and the mates are told second.** A station that stops halfway is left with a list of stations still to tell, never with a fleet that moved without it.
- **What joins is whoever holds the old key, not a list of names.** The request does carry the names of the asking station's fleet-mates, so the owner reads "spark2 and 2 other stations (M4-Macbook, LLM-Server) ask to join this fleet" in the notice, under Access and in `spandeck trust`. Those names are text from another machine: cleaned, at most twenty, shown as text, never acted on.
- **Browsers.** A token is made with the fleet's key, so every browser signed in from another device to a station that moves signs in again, and those stations stop listing them. The join dialog says how many stations that is. Browsers signed in to the fleet that was joined are not affected. The station's own Control Deck (`spandeck cc`) is not affected.
- **A request is listed on every station of the fleet.** A station that is asked (by a station to join, or by a browser to sign in) tells the other stations of its fleet, in a signed call only a station can make (`trust.relay`), which requests wait there: the id, the kind, the names, the address it came from, the code, when it lapses. Never the value the asker collects with, and never the key. It sends the whole list again when one is answered, and each time it looks for stations while one waits. Each station lists them in `trust.status` with the station that was asked, raises the same notice, and drops one at the time it lapses.
- **It is answered at any of them.** `trust.approve` and `trust.deny` (the Control Deck, `spandeck approve <code>`, `spandeck deny <code>`) first look for the request at the station they are given to; one that waits at another station is passed to it as the owner's request (`rpc.forward`). The station that was asked decides, and hands over the key or the token as before. Its log says who decided and through which station ("you (via bravo)"); the station the owner used logs that it passed the answer on. The first answer to arrive stands; a later one is told there is nothing to decide, and the request leaves every list.
- **Only the owner answers.** `trust.approve` and `trust.deny` are on no agent's list and no station's, at any station; an agent has no call that reaches another station as the owner. `trust.relay` is a station's signed call, refused from the owner and from an agent: it can only add to or clear what is listed, and what it lists cannot be approved unless the station it names holds such a request.
- **A station on 0.4.0** takes the key when a mate hands it over (its notice words it as a removal, with no station named), and joins a fleet as before; but when it is the one that asks to join, it still leaves its mates behind, and it neither tells its mates of a request nor lists theirs. Update a station before using it to join.
- The ship's log records who asked for a join (`trust.join_asked`), which it did not before.

## Closing every pane, and removing a station (15 Nov 2026, the owner's request)

"I should be able to delete a machine and it should kill all the sessions that are running, or delete all sessions on a machine."

- **Close all panes on a station** (`station.close_all`; the station's menu; `spandeck pane close --all [--station NAME]`). Every pane is ended and taken off the list, as "Kill and remove" does for one. Projects, layouts, history and the archive are not touched. The owner's alone: on no agent's list and no station's; it reaches another station only as the owner's forwarded request, like `station.restart`. The ship's log says who and how many.
- **Ending a pane ends its whole session, not only its process group** (Unix; a change to Kill for every pane). A shell puts a job (`sleep 300 &`) in a group of its own, and that job used to outlive "Kill and remove". Kill now also reaches every process in the session the pane's program started. A program that started a session of its own (`setsid`, a daemon) is still not reached. On Windows the job object already ended the tree.
- **A station is removed by replacing the fleet key, because there is no other honest way.** Every station of a fleet holds the same key, and that key signs station calls and makes browser tokens. A list of removed stations would not stop a machine that still holds a key the others accept. So `fleet.remove` (the station's menu; `spandeck fleet --remove NAME [--stop]`) does this, in order:
  1. The station being removed, if it answers (`station.leave`): closes every pane, takes a new key of its own (a fleet of one), and forgets the other stations, its links to their projects and the browsers signed in to it.
  2. The station the owner is using makes a new fleet key and hands it to every other station that answers (`fleet.rekey`, a station's signed call only, never the owner's or an agent's), then takes it itself.
  3. The removed station is forgotten: the list of known stations, pinned panes and layouts that point at it, its panes in global layouts (a layout left empty goes), project links to its projects, stops and messages waiting for it. Project files in the owner's folders are not changed.
- **What follows, and is said in the dialog before anything is done:**
  - Every browser signed in from another device signs in again, and connections made under the old key are closed at once. The browser that asked for the removal gets its new token in the reply and stays signed in. The token of a station's own Control Deck (`spandeck cc`) does not rest on the key and is unaffected.
  - **A station that was away at that moment keeps the old key.** Until it has the new one it shows as not in the fleet, and the removed station, if it kept the old key, could still act on it. Each station that changed keeps the old key in a private file (`fleet.retired`) for one purpose, never to accept anything (a fleet that joins another one uses the same file the same way): when a station it knew is seen again at the address it had, still showing the old fleet, it is handed the new key in a call signed with the old one. No approval is needed for that, since it is a station the owner already approved. After 30 days it is no longer offered, and that station rejoins with `fleet --join` and an approval.
  - **A station that cannot be reached is removed from the fleet's side.** Nothing is done on it: its panes keep running until someone stops spandeck there. It keeps a key nobody accepts, so it cannot act on the fleet, and it cannot rejoin without a new approval. If spandeck is still running there it is listed again as "not in this fleet", like any machine under the owner's login that never joined.
  - **The vault token is that station's own.** A removed station keeps it and it keeps working there; the owner is told to revoke `spandeck:<station>` at the vault. spandeck does not revoke it: only the vault ends a token.
  - **"Also stop spandeck there and stop it starting at login"** is a tick box, off by default, not offered for a station that cannot be reached. It removes the login service (`systemctl --user disable` and the unit file on Linux; the launchd agent on macOS; the Scheduled Task on Windows) and the daemon then ends by itself. The programs, the state folder and the config folder stay on that machine's disk, and the dialog names them.
  - **Removing the last other station** leaves the station in use as a fleet of one with a new key. **Removing the station in use** is done through another station that answers, which does steps 2 and 3 for the rest; with none in reach the station leaves by itself (it throws its key away), says which stations were not told, and the owner removes it at one of them as well, which is what replaces their key. The only station of a fleet of one cannot be removed from it.
- **What this does not do.** It does not undo what a station did while it was trusted: a machine that was already hostile could have left something behind on the others. Removal is for a machine that is retired, sold, lost or no longer wanted.
- A machine that is not in the fleet and stops answering leaves the list (before, it stayed as "unreachable" until the daemon restarted). Stations of the fleet that are off stay listed, as before.
- Both dialogs name the station and its counts, open on Cancel, and act only on the red button. `fleet.forget` ("Forget this station") stays: it only takes an unreachable station off the list.

## A new pane can continue the last conversation (15 Nov 2026, the owner's request)

"We should have a resume session checkbox when creating a new pane, this way if it's there it would load, if it's not then it just start a new one."

- **`pane.spawn` takes `resume_last`** (the New pane dialog's tick, off until ticked and then remembered in that browser; `spandeck pane new --resume-last`). The agent goes back to the conversation it last used in the pane's folder; with none, a new one starts and the pane says so (`resumed` in the pane and in `pane.created`). A caller's own `resume` id wins. A shell ignores it.
- **The agent's own saved conversations are read, not spandeck's records**, so one started outside spandeck counts. Claude Code: the newest session file in its directory for the folder that holds a message (not an empty file, not a sub-agent's). Codex: the first line of its session files, the 200 most recently started only. OpenCode: its database. Antigravity: none can be found, so a new one always starts. Only read, never changed.
- **Two live panes are never put in one conversation.** One a live pane on the station is in is passed over for the next most recent, or a new one.
- **`pane.last_session`** says what would be continued and when it was last used, before the pane is started. It reads only, and is for whoever may start a pane: on no agent's list, no station's and not the read-only one.

## Themes: how the deck looks (15 Nov 2026, the owner's request)

"I also want to think about custom theme and borders colors etc. … if I create one I like I should be able to set as a theme to reuse." "Ligatures should work, emoji should work, you should be able to customize, or edit these option in a pane or in a layout."

- **A theme skins the whole Control Deck, not only the terminals.** A theme is a set of settings by name: the terminal's colours (background, text, cursor, selection, the 16 colours programs use), the deck's colours (page, bars, buttons, lines, text, the five state colours, the outline of the pane with the keyboard and its width), the terminal's font (family, size, line height, letter spacing, weights, ligatures on or off), the deck's font and size, the cursor's shape and blink, and a contrast floor for terminal text.
- **Three levels, the most specific wins: the deck, a layout, a pane.** Each may name a theme, change single settings, or both; its settings win over its theme, and what it does not say follows the level above. The page's colours and the deck's font follow the deck, or the layout on screen. A pane's own look changes only that pane's terminal and its frame. "Appearance" in the top bar (and in ⌘K), "Appearance…" in a layout's menu and in a pane's menu open the same editor for that level; a layout and a pane can go back to "the deck's".
- **The editor shows the change on the real page as it is made**, in that window only, with a sample terminal; Cancel puts everything back and nothing is saved until Save. It warns, and never blocks, when text has less than 4.5:1 contrast with what is behind it, or two of the state colours (green, amber, red, blue) are close. A look can be saved as a theme under a name, and themes can be duplicated, renamed, deleted, exported to a file and imported from one (spandeck's own format, and other terminals': see Fonts, icons and other terminals' themes).
- **Built-in themes are in the Control Deck's code and cannot be changed or deleted, only duplicated:** Spandeck Dark, Spandeck Light, Solarized Dark and Light, Dracula, Gruvbox Dark, Nord (NOTICE has each palette's licence). **Spandeck Dark is the default and is the deck exactly as it was before themes**, so nothing changes for anyone until they choose. For it the worked-out shades (a pressed button, a chip's border) are the values written out before; for every other look they are mixed from the theme's colours, which is what makes a light theme light everywhere. The outline of the focused pane has width 0 in every built-in theme, because the deck had none.
- **Stored in a document of its own, `look.json`, kept the same on every station** (`look.get`, `look.set`, `look.theme.save`, `look.theme.remove`; `look.push` between stations), by the deck document's rules: only the owner changes it, the station where they did offers it to the others, the newer copy is kept. **It is not part of the deck document, and a layout's look is not written inside the layout**, because a station from before themes (0.3.0) reads the deck document and a project file into the fields it knows and writes only those back: it would drop the themes and, at its next change, push the document without them to the whole fleet. Such a station never holds this document: it is offered only to a station that says it keeps one. The same loss could happen between two later versions, so the document has a format number: a station takes no copy in a format newer than it knows, takes none in a format older than the copy it holds, and does not change a copy newer than it knows.
- **A layout's look and a pane's look are entries in that document**, named `global:<layout>`, `<station>/<project>/<layout>` and `<station>/<pane id>`. So both kinds of layout work the same way, and a pane on a station from before themes can have a look. A pane's look goes when the pane is closed (its station sees to it, and prunes looks of panes it no longer has at its next change; the Control Deck also asks, for a station that does not know how); a layout's goes when the layout is deleted and moves when it is renamed; removing a station forgets its panes' and its projects' layouts' looks.
- **Changing how the deck looks is the owner's.** No agent has a `look.` method, not even the read; another station has only `look.push`, as it has `deck.push`.
- **What is in a theme is data**, since it comes from a file someone imported or from another station. The station checks every value: a colour is `#rgb`, `#rrggbb` or `#rrggbbaa` and nothing else; a number is within its range; a font is a list of at most 8 names of letters, digits, spaces, `-`, `_` and `.`; names and counts are bounded; a setting it does not know is dropped. The Control Deck checks again and applies a look only as the value of a custom property (`style.setProperty`) and as options of the terminal: no style sheet or markup is built from it. A theme's name is put on the page as text.
- **A font is named.** The font setting is a plain list of names. A name is a font installed on the device showing the deck, or one the owner uploaded (next section).
- **Ligatures** are the font's and need nothing added; the switch turns them off with a style on the terminal's rows. **Emoji take two cells** in every terminal (the Unicode 11 widths, from xterm.js's own add-on): before, the terminal gave them one and drew the next character over them. One flaw stays: a character that a variation selector turns into an emoji (❤️) still counts as one cell, which is also what the station's own screen believes.
- **A font change follows the sizing rule.** The client in control of a pane fits it again and tells the station; anyone else only scales what it shows. A resize to the size a client already holds a pane at is not taken as losing control, so two windows that fit a pane alike do not both let go when a font change reaches them at the same moment.
- The deck's own look is also remembered in the browser, so the page (and the sign-in page) opens in it before the station has answered. The colour a phone's home-screen icon was installed with (`manifest.webmanifest`) does not follow the theme.

## Fonts, icons and other terminals' themes (15 Nov 2026, the owner's request)

"Think about how to setup fonts special characters colors etc." "Ligatures should work, emoji should work."

- **A font file can be uploaded** in the Appearance editor (WOFF2, WOFF, TTF, OTF; 5 MB each, 24 files) under a family name, and a theme then names it like any font. It reaches every browser that opens the deck, a phone too. A family can have several files: regular, bold, italic, bold italic, or one variable file for every weight. Removing a file takes it off every station; the editor first says which themes still name it, and they keep the name and fall back to the next font in their list.
- **Which fonts there are is in the look document** (`fonts`: id, name, face, format, size), so the list is the same everywhere and travels by that document's rules. **That is format 2.** A document is written as format 1 until the first font is uploaded, and never goes back: a fleet with a station from before uploaded fonts keeps sharing themes with it until then, and after that the old station keeps the copy it has until it is updated.
- **The files do not travel in the document.** Each is kept in the station's `fonts` folder (beside `look.json`) under an id that is the first 32 hex digits of the SHA-256 of its bytes. A station that takes a document naming a font it does not hold asks the sender for it, and after every look for stations asks any station that keeps fonts (`look.font.fetch`). What arrives is kept only if its bytes give that id and are in the format the document says. A file no longer on the list is deleted.
- **The file is not trusted.** Only the four formats are taken, told by their first four bytes; size and count are bounded; the name follows the rule for every font name and may not be one that means a kind of font (`monospace`). The station never reads into a font: the browser that draws with it checks it, as it does any web font.
- **Served at `/api/font/<id>`, to the owner's signed-in browsers only** (the token in a header; a pane's token is refused), found by looking the id up in the document, never by making a path from what was asked. It goes out with its font type, `nosniff`, and may be kept by the browser for good, since an id always names the same bytes. The Control Deck makes a `FontFace` object from the bytes; a font's name is never written into a style sheet.
- **Rule 7:** `look.font.add` and `look.font.remove` are the owner's. Another station has `look.font.fetch` and nothing else new. No agent has any of them.
- **A terminal measures again when a font arrives late**, because it measures its cells once, with whatever font stood in. Then the sizing rule, as for any font change: the client in control fits the pane and tells the station, anyone else rescales.
- **Prompt icons and powerline signs are drawn on any device** from a bundled font, last in every terminal's font list: Symbols Nerd Font Mono 3.5.1 (1.2 MB), under the name `Spandeck Symbols`. Its `unicode-range` is only where Nerd Fonts' icons live (the private-use ranges U+E000–E00A, E0A0–E0A3, E200–E2A9, E300–E3E3, E5FA–E6BB, E700–E958, EA60–EC84, ED00–F385, F400–F533, F0001–F1AF0, and the power signs U+23FB–23FE and 2B58), so the browser fetches the file only when such a character is on screen, and keeps it (the file's name has the version in it). The font's ♥ (2665), ⚡ (26A1), ☰ (2630) and angle brackets (276C–2771) are left to the device's own fonts, so it never replaces a letter, an emoji or a box-drawing line. A font earlier in the list that has an icon (a Nerd Font, or Fira Code's own powerline signs) wins.
- **Icons are one cell wide**: the font's glyphs are one em square, shown at 60%. **Powerline separators (U+E0B0–E0D7) come from a second file of 7 KB**, made from the same release by `scripts/make-powerline-font.py`: the same glyphs scaled to a cell 0.6 em wide and 1.2 em tall. A browser draws a fallback font's glyph at its own shape, so without this a separator is half the height of its row. A terminal whose font has an unusual cell (JetBrains Mono's is taller) shows a hairline gap; `font.lineHeight` over 1 does too, as in any terminal.
- **Import reads iTerm2 colour files (`.itermcolors`) and Windows Terminal colour schemes**, alone or in a whole `settings.json` (with its comments; the editor asks which scheme when there are several). They say only the terminal's colours. The deck's are worked out from them: the page is the terminal's background, bars, buttons and lines are steps from it toward the text, the five state colours are the palette's own (the normal or the bright of each, whichever reads better, moved toward the text until it has 3:1), and light or dark follows the background. The result is shown in the editor, unsaved, to be changed and then saved as a theme. An iTerm2 file is parsed as XML, which runs and fetches nothing, and only numbers are read from it; every colour goes through the same check as any theme's. iTerm2's colour space is not converted (P3 values are read as sRGB), and its colour for selected text and its bold colour have no setting here.

## AgentVault (14 Nov 2026)

- spandeck and AgentVault stay separate programs. spandeck works without a vault.
- The vault is the only place a token is issued, and only when the owner approves on the vault's own page. The vault passphrase is never typed into the Control Deck. Approval opens in a small pop-up window on the vault's address.
- One token per station, held by spandeckd in a file only the user can read. Panes and agents never get the token, and keys are never put in a pane's environment.
- The token can list keys and send requests through the vault's proxy, read-only by default, and is pinned to the station's Tailscale node.
- spandeck finds the vault by its Tailscale name; the address is never hard-coded. `[vault] url` in the config overrides it.
- Tokens do not expire by default. Rotation is a per-station setting (never, or every N hours, at least 1) that only the owner changes; spandeckd rotates by itself.
- The owner is told when the setting changes outside the Control Deck, or the token is swapped or refused.
- Connecting a station is the only step: by default its agents may use every key the owner allowed that station's token at the vault (changed 14 Nov 2026 at the owner's request; before, a project had to list its keys). A project can narrow its agents with `[vault] keys` in `.spandeck/project.toml`.
- When a station has a token, a raw key in an agent's command is blocked and reported to the vault; one pasted into a prompt, or found in a transcript, is reported.
- **Connect prompt** (the owner's request, 14 Nov 2026: "there should be nothing I need to do"). A Control Deck that opens on a station that found a vault and has no working token says so once, in a bar that blocks nothing, with one button that starts the connect flow. "Not now" is kept by the station (so it holds in every browser), is undone from the Vault screen, and is forgotten when the station connects. No prompt when no vault was found or `[vault] enabled = false`.
- **Check and repair.** spandeckd checks the chain from an agent to a key when it starts, when a Control Deck opens (at most once a minute) and when asked (`vault.check`, the owner's only): the vault's address, its lock, the station's token (accepted, and tied to a machine), the key list, and what panes need (spandeck's own program and socket; Antigravity's plugin). It repairs without the owner: a token the vault calls overdue is swapped, Antigravity's plugin is rewritten. Each thing it cannot repair is one sentence with one action, in `vault.status` (`checks`, `health`), on the Vault screen and in `spandeck doctor`, and is told to the owner once (`vault.alert`), not on every check and not again after a restart.
- A repair never gets a token any way but the owner's approval at the vault.
- A connected station shows its token to no address but the one that issued it (a security fix of 14 Nov 2026: no other machine can ask for the token by claiming to be a vault). When that address stops answering and another AgentVault answers on the owner's machines, the station does not try its token there: it says "The vault seems to have moved to <address>" and offers Reconnect, the ordinary approval. Following a move with the existing token is behind `[vault] follow_moves = true`, off by default: relaxing the guard is the owner's decision, and the owner has not made it.

## Agent programs (14 Nov 2026, the owner's request)

- OpenCode and Antigravity are agents a pane can run, wired to spandeck's tools and skill when the pane starts. Neither can block a tool call or pass an approval to the Control Deck, so their rules are guidance.
- A pane started for an agent that is not installed on its station installs it first, in the pane, with the vendor's own install command (or the way the owner chose for that station, see below). That command is fixed in the code (a vendor's script is fetched over HTTPS only) or set in the station's own config; it never comes from a caller or another station.
- Looking for an agent's program has four answers, never fewer (14 Nov 2026, after a fault on the owner's Mac: a Codex whose real binary was gone was called installed, and one that was slow to answer was called not found): not installed; installed (it answered when asked its version); installed but not working (it exits with an error or cannot be run); installed but it did not answer in time, which is unknown, not broken. A program that is slow to say its version is never killed.
- When a program is in several folders spandeck looks in, the first copy that answers is used, not the first that exists.
- A pane started for a program that is installed but not working offers a reinstall in the pane, the same way a missing one is installed: what was found and the program's own error line, the command, Ctrl-C skips. Success is the program answering afterwards, not the installer's exit code. Whether an install should ask before it runs is still open with the owner; for now it announces the command and Ctrl-C skips.
- Each agent has the ways it can be installed that its vendor documents: the vendor's own script (the default), npm, Homebrew. A way is offered on a station only when its tool is there. The owner's choice is kept per station and per agent in the station's state; `install_cmd` in the station's config still wins over everything. Every command is fixed in the code, taken from the vendor's documentation or a package registry; a way that could not be confirmed is left out. Installing, like updating, is the owner's to do (`agents.install`): an agent cannot.
- The owner can see each agent's version on each station and update one or all of them. After an update that changed the version, an idle pane of that agent restarts into its conversation; a pane in a turn, or waiting on the owner, restarts when it is next idle. Updating is the owner's to do: an agent cannot.

## Add a machine (14 Nov 2026, the owner's request)

- The fleet screen ends in an "Add a machine" tile. It lists the Mac and Linux machines under the owner's Tailscale login that are online and not stations; phones, Windows, tagged and shared machines are left out. The station the owner is using connects to the one picked over SSH, runs the same install script as the README, starts spandeck there now and at login, and has it ask to join as `spandeck fleet --join` does. The tile then shows the join code.
- **The approval step stays.** The new station is approved with the usual control, by its code. A code that arrives over SSH is shown only if this station holds a waiting request with the same code. The join step is one place in the code (`JoinBy` in `crates/core/src/add.rs`), so a single-use ticket could replace the code later; no ticket exists.
- **Its own setting, off by default: `add_machines = true`** in the config of the station that does the adding, not `remote_start`. Both let a station run a command on another machine, and both are off until the owner turns them on at that station. They are separate because they differ in what they risk: `remote_start` runs one fixed command on a machine that is already a station, with access the owner already set up; adding installs software on a machine spandeck has never run on, and can use a password typed into the deck. An owner who wants one has not agreed to the other. The picker works while it is off and says how to turn it on; nothing is run and no password is accepted.
- The owner can also turn it on, and off again, from the tile itself (changed 14 Nov 2026: the owner clicked the tile and asked why they could not add a machine). The switch is kept in the station's state, is logged, is the owner's alone (`fleet.add_enable` is on no agent or station list and is never forwarded), and applies only to the station whose Control Deck they are using. It stays off until the owner turns it on.
- **Only the owner, and only at the station being used.** The methods are on no list for agents or other stations, and are never forwarded between stations in either direction, so a password never travels from one station to another.
- **The password** is asked for only after a first try without one fails (Tailscale SSH or an SSH key that already works needs none). It is used for one connection and held in memory until ssh has asked for it once, or the connection ends. It is never written to a file, a log, the ship's log, an event, an error or a command line or environment: ssh gets it by running spandeck itself as its askpass helper, which is handed it once over a private socket, and only for a prompt that asks for a password. A second prompt gets nothing, so a wrong password is tried once. The ship's log records who added which machine as which user and how it went.
- **Host keys.** The address is the one the local Tailscale client gives for the machine picked, and must be a Tailscale address; a caller names a machine, never an address. The first connection saves the machine's host key in `known_hosts` in the state directory (ssh's accept-new); a different key later is an error, shown plainly, and nothing is sent. The owner's own ssh config is not read, so it cannot redirect the connection or switch checking off.
- What the machine prints is shown as it runs. It is text from somewhere else: control characters are dropped and the Control Deck inserts it as text.
- `add_machine_cmd` replaces the ssh command (`{user}`, `{host}`, `{known_hosts}`); the script arrives on its standard input. The script uses only commands every release has, since the release it installs may be older than the station that sends it.

## Usage and cost (14 Nov 2026, the owner's request)

- Token counts are only what an agent wrote in its own session data: Claude Code's and Codex's session files, OpenCode's session database. Antigravity keeps nothing spandeck can read, and is shown as not reported. Nothing is typed in by the owner.
- Counted per conversation and per model, sub-agents included under the model that ran them; a model call written more than once is counted once. A new conversation starts from zero; the station keeps its totals.
- Cost is an estimate at list prices and is called that wherever it appears, with the list's source and date: a subscription is not billed per token.
- Prices and context windows come from a public list (models.dev) that spandeckd fetches by itself once a day and keeps. The request carries nothing about the owner or a project. No price or window size is written into the code; a model the list does not name has no price. `[prices]` in the config is an optional override, never needed.
- The context gauge uses the window the session data states (Codex), else the list's.

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
- A station removed from the fleet can no longer act on the stations that took the new key: its signed calls are refused and browser tokens made with the old key are not accepted.
- A station that joins another fleet never leaves a station of its own fleet behind: each takes the same key, at once or when it is next seen.
- A request to be let in can be answered at any station of the fleet, by the owner only, never by an agent.

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

## Windows (14 Nov 2026, the owner's request: a Windows PC as a full station)

Built and tested on a real Windows 11 machine (rtx6000) since 14 Nov 2026; each line says
what was decided and why, and "unlike Unix" lines say what the owner gets less of. What has
not been tried on a real machine is said where it applies.

- **Native, not WSL.** x86-64, Windows 10 1809 or later (the first with ConPTY) and Windows 11.
- **Rule 4 of AGENTS.md now reads:** platform code lives only in `crates/terminal/src/sys/`:
  `unix.rs`, `windows.rs`, and `mod.rs` which picks one. Everything that differs by system
  (terminals, the local socket, private files, process control, the shell, start at login,
  keep-awake) is behind that one interface, and no other file in any crate names `cfg(unix)`,
  `cfg(windows)`, `nix`, `libc` or `windows-sys`. A value that differs (a folder, a command)
  is asked of `sys`, or chosen by `if sys::WINDOWS` in ordinary code that is compiled and
  checked on every system, never with a `cfg` where it is used. Exceptions, each one line:
  `spandeckd.rs` (the attribute that makes it a windowless program); a test that cannot run
  on Windows carries `#[cfg_attr(windows, ignore = "<the reason>")]`; a test file for one
  system only begins `#![cfg(unix)]` or `#![cfg(windows)]`. `pty.rs` keeps the types every
  system shares. Reason: the rule was written so a Windows port replaces one
  place; it is now a folder instead of a file because the port is more than the PTY.
- **Terminals: ConPTY** (`CreatePseudoConsole`), one per pane, with the program started by
  `CreateProcessW` inside a job object that is ended when it closes. Kill is ending the job
  (the whole tree, as `killpg` is on Unix). Hang-up is closing the pseudoconsole, which tells
  every program on it to close. Interrupt is Ctrl-C typed into the pane, as on Unix. A pane's
  exit is its first program's exit code (there are no signals, so `ExitInfo::Signal` never
  appears). A program that is a `.cmd` or `.bat` file (npm's `claude.cmd`) is run through
  `cmd.exe`, with every argument quoted the way Rust's standard library does it so that `&`,
  `|` and `%NAME%` in an argument stay text; an argument with a line break is refused.
  Because the job ends with the daemon, no pane's program outlives a daemon crash: the
  job belongs to the pane host (below), not to the daemon.
- **Local socket: a named pipe**, `\\.\pipe\spandeck-<hash of the state folder>`, in place of
  the Unix socket, behind one `sys::local` interface (listener, async stream, blocking client)
  that the server, the client, the askpass helper and the tests all use. Only the same user
  can open it: the pipe's access list names that user alone, remote clients are refused, and
  the daemon must be the pipe's first creator. The client checks that the pipe it opened is
  owned by its own user before it sends its token, so a pipe another account made first under
  the same name gets nothing. The token is still required, as on Unix.
- **The pane host** (the owner's decision, 14 Nov 2026: "build the extra helper"). Windows
  cannot replace a process's program in place, and a pseudoconsole belongs to the process
  that made it, so on Windows the panes are not in the daemon: `spandeck-pane-host.exe`, a
  small program of its own, owns every pseudoconsole, job and program, and the daemon is its
  client over a named pipe only the same user can open. So an upgrade, a restart and going
  back to the previous binary keep every terminal on Windows as they do on Unix
  (`live_handoff` is true everywhere), and the Upgrades Guarantee and its tests are the same
  on both. The new daemon is a new process there (on Unix it is the same one).
  - **The screen stays in the daemon.** The host knows nothing of screens, agents or
    projects: per pane it keeps the terminal, the exit status, and the last 8 MB the program
    wrote, each byte numbered. At an upgrade the old daemon hands the new one its picture of
    each screen (as on Unix) and the number of the first byte it did not take; the new one
    asks the host for output from that byte on. Nothing is lost or shown twice unless a pane
    writes more than 8 MB while no daemon is reading, and then the oldest of it is.
  - **A daemon crash no longer ends the panes on Windows.** The next daemon finds them in
    the host and rebuilds each screen by replaying what the host kept: the screen is right,
    and scrollback goes back as far as those 8 MB. This changes a recommended default
    ("processes do not survive a daemon crash") for Windows only; on Unix it stands. So that
    panes nobody can reach are not kept for ever, a host with no daemon ends its panes and
    itself after ten minutes (the log-on task restarts a failed daemon within one or two).
    Stopping the station on purpose ends the panes and the host at once.
  - **The host is not upgraded while it holds a pane**: it cannot be without ending them.
    Its protocol is versioned and only grows; a daemon speaks every host protocol back to 1
    and never asks an older host for what it does not have, so an old host keeps serving its
    panes under a new daemon for as long as they live. A host older than the daemon is
    replaced by itself at the first moment it holds no pane (`station.info` shows
    `pane_host.outdated` until then). A change the old protocol cannot carry would need the
    panes ended: such a release must say so before it is installed, and the owner decides
    when. None exists.
  - If the host itself dies, its panes die with it (their jobs close); the daemon says each
    ended and starts a new host for the next pane. Agents are then resumed by session id at
    the next daemon start, as after a reboot.
  - On Unix the host is not used (exec in place needs none); `SPANDECK_PANE_HOST=1` turns
    it on there, which is how it is tested on Linux.
- **The installed program is replaced by renaming.** A running `.exe` cannot be overwritten
  but can be renamed, so an update moves `spandeck.exe` to `spandeck.exe.old` and puts the new
  one in its place; the old file is removed at the next start. `state\bin\running` and
  `previous` are hard links as on Unix (same volume), else copies. The same goes for the
  pane host's file while it runs.
- **Start at login: a per-user Scheduled Task** ("spandeckd", at log on, restart on failure,
  no time limit, runs on battery), not a Windows Service: a service runs in session 0 without
  the user's profile, PATH or the agents' logins. The task runs `spandeckd.exe`, a windowless
  program that starts `spandeck.exe daemon` with no console window and waits for it, so no
  window opens at log on and the task's restart-on-failure sees the daemon's exit code.
  **Unlike Linux with linger, the daemon runs only while the owner is logged in** (a locked
  screen is fine; logged out is not). The owner accepted this (14 Nov 2026: the machine logs
  on by itself).
- **Keep-awake** is `SetThreadExecutionState`, held by `spandeck keep-awake` (the same shape
  as `caffeinate` and `systemd-inhibit`: a helper that holds the machine awake while it runs).
  The helper ends when the daemon does. It does not stop a laptop sleeping when its lid shuts.
- **Shells.** A shell pane runs PowerShell 7 (`pwsh.exe`) when installed, else Windows
  PowerShell, else `cmd.exe`. One shell line (an install or update command from the catalogue)
  runs through Windows PowerShell 5.1, which every Windows has
  (`powershell -NoProfile -ExecutionPolicy Bypass -Command`); nothing needs PowerShell 7. Install-before-launch
  is not a second script in another language: on Windows the pane runs
  `spandeck install-then-launch`, the same steps written in Rust (say what was found, show the
  command, run it in the pane, Ctrl-C skips, start the first copy that answers).
- **The agent catalogue has a Windows list**, read from each vendor's documentation on
  14 Nov 2026: Claude Code (PowerShell script, npm, WinGet), Codex (PowerShell script, npm),
  OpenCode (npm only: its page has no Windows installer and says Windows package managers are
  not supported), Antigravity (PowerShell script). All four document native Windows. The
  update commands of Codex, OpenCode and Antigravity were read from the programs on Linux and
  are not confirmed for Windows.
- **Hooks.** Claude Code on Windows runs a hook in Git Bash when that is installed and in
  PowerShell when it is not; no one command line is right in both, so spandeck's entries name
  PowerShell (`"shell": "powershell"`) and use `& '<path>' hook claude`. Each hook then costs a
  PowerShell start; if that proves slow on the real machine, the no-shell form (`command` plus
  `args`) is the next thing to try. Codex documents no way to name the shell for a hook on
  Windows: the same line is written and is unverified, and screen detection covers Codex
  where its hooks do not fire, as on Unix.
- **Folders.** State in `%LOCALAPPDATA%\spandeck`, config in `%APPDATA%\spandeck`
  (`SPANDECK_STATE_DIR` and `SPANDECK_CONFIG_DIR` still win). Home is `%USERPROFILE%`; `~` means
  that. A program name is looked for with each extension in `PATHEXT`. Paths are compared as
  the file system compares them (by the file's identity, not its spelling).
- **Private files** (the client token, the fleet key, the vault token, agent setup): the
  state and config folders get an access list of the user, SYSTEM and Administrators only,
  not inherited from the parent, when the daemon starts; files written in them inherit it.
  That is what a default profile folder already has; setting it makes it true when
  `SPANDECK_STATE_DIR` points somewhere else. Administrators can read them, as root can on Unix.
- **Tailscale** is asked through its CLI on every system (`tailscale status --json`,
  `whois --json`), never LocalAPI, so Windows needs only the program's place:
  `tailscale.exe` on PATH, else `%ProgramFiles%\Tailscale\tailscale.exe`.
- **Install and update.** `scripts/install.ps1` is `install.sh` for Windows: the release zip
  for `x86_64-pc-windows-msvc`, SHA-256 checked, installed in `%LOCALAPPDATA%\Programs\spandeck`,
  added to the user's PATH, the task registered. The release has `spandeck.exe`, `sd.exe` and
  `spandeckd.exe` in a `.zip`; `station.update` picks it and unpacks with the `tar.exe` Windows
  ships. Other processors (ARM64) are not built yet.
- **Add a machine can add a Windows machine** (the owner's decision, 14 Nov 2026). The
  picker lists Windows machines too. The station connects with the same `ssh` command and
  hands the machine a Windows PowerShell 5.1 script with `install.ps1` inside it (as the Unix
  script carries `install.sh`): install, register and run the log-on task, wait for spandeck
  to answer, ask to join. The line that reads the script there has no `$` and no quote of
  its own inside, so it means the same whether that machine's OpenSSH hands it to `cmd.exe`
  (the default) or to PowerShell. The approval step is unchanged. It needs the OpenSSH
  server on there, which Windows ships switched off and only an administrator can add, and
  the owner logged on there (the station runs in their session, not in the SSH session,
  which ends). Its own failure sentences: the OpenSSH server not installed or not running;
  Windows refusing to run spandeck (Smart App Control blocks programs it does not know, and
  releases are not signed); installed but not started because nobody is logged on. The
  script is run on a real Windows in a test as far as its network check; a whole add of a
  Windows machine has not been done yet. A Windows station adding another machine uses the
  same code with `ssh.exe`; typing a password that way (askpass) is untried on Windows.
- **Certificates on Windows: the system's verifier, with the Mozilla roots as a second
  source** (15 Nov 2026, after a fault on rtx6000: the first contact with the vault failed
  with `UnknownIssuer` on a good Let's Encrypt chain, and worked once curl had connected to
  the same address). Windows keeps only the roots it has needed and fetches others on
  demand; that fetch did not happen for spandeck's check. Which of Windows' rules stopped it
  was not established: the machine has the root now, so the state cannot be made again. The
  fix does not depend on the answer: when Windows cannot build a chain to a root it has,
  the same Windows verifier is asked again with the Mozilla root list carried inside
  spandeck (`webpki-root-certs`). Verification is not weakened: Windows still checks names,
  dates, usage and signatures, and a root an organisation added to Windows still works. The
  cost: a public root the owner removed from Windows by hand is still trusted by spandeck if
  Mozilla lists it, and the carried list is as old as the release. Every outgoing client is
  made in one place (`crates/core/src/http.rs`). macOS and Linux are unchanged.
- **Smart App Control.** Where it is on, Windows refuses programs it does not know, which
  includes every spandeck build, since releases are not signed. The owner turned it off on
  rtx6000. Signing the Windows release is what would make that unnecessary; it is not done.
- **Not on Windows yet:** wake-on-LAN of a Windows station (its network card's address is
  not read), starting a stopped Windows station over SSH (`fleet.start` runs a Unix line),
  usage figures for OpenCode (where its database is on Windows is not documented), and the
  Codex sandbox repair (a Linux problem). ARM64.
- **Tests.** On rtx6000 the whole suite runs (`cargo test --workspace`): everything passes
  except the eight tests that drive a browser, because no Chrome is installed there; they
  have never run on Windows. A test that needs a Unix shell, a signal or a tar.gz release is
  marked ignored on Windows with its reason (about thirty); nothing changes on macOS or
  Linux. Windows-only tests cover what is new: ConPTY (input, exit code, resize,
  environment, a `.cmd` file's arguments, kill ending the whole tree, hang-up), a station
  end to end (a shell pane over the pipe, an agent's hooks, every pane kept through a daemon
  crash, the state folder's access list), `install.ps1` against a local release, and the
  script Add a machine sends. The pane host and the local socket are tested on every
  system. CI has a `windows` job with the same tests; the scenario step reports without
  failing the job until the browser tests have been seen to pass there.
