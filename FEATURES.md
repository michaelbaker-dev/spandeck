# mterm — research, understanding, and feature list

Status: draft for review (2026-11-12). Edit freely: tick boxes, delete rows, add rows.
Priority column: **P0** = core of v1, **P1** = soon after, **P2** = nice to have, **?** = undecided.

---

## 1. What you're trying to build (my understanding)

A **persistent, always-running terminal "control plane" for AI coding agents**, organized around *projects*, that is:

1. **Like herdr** — panes/tabs per project, agent state awareness (working / waiting / done), agents can be driven by API.
2. **Mouse-first, not keyboard-only** — right-click context menus everywhere, click to focus/split/launch, plus key commands for power use. Most multiplexers (tmux, zellij, herdr TUI) are keyboard-first; you want the opposite default.
3. **Cross-platform** — macOS, Linux, Windows (native, not WSL).
4. **A daemon, not a window** — `mtermd` is always running like a tmux server; UIs (desktop, TUI, phone) are just clients that attach.
5. **Project-centric** — a project = name + one or more directories + saved layout + which agents to launch (Claude Code, Codex/ChatGPT, Antigravity `agy`, plain shell). Opening a project restores its terminals.
6. **Agents talk to each other** — windows in a project can exchange messages over a local IPC/API so Claude can ask Codex to review, etc.
7. **Multi-machine** — one mterm talks to another mterm over Tailscale; all machines' projects appear in one list.
8. **Remote enrollment by QR** — pair a phone or another computer the way ChatGPT/Codex and Claude Code pair devices: scan a QR, done.
9. **iPhone presence** — a home-screen widget showing projects and agent states, a "focus project" you can set, tapping opens the terminals so you can work from the phone.

One-line version: **tmux-style daemon + herdr-style agent awareness + project workspaces + a P2P mesh over Tailscale + a phone app/widget — with a mouse-first UI.**

---

## 2. What already exists (so you know what you're competing with / can borrow)

### Direct competitors — multiplexers built for agents
| Tool | What it does | Relevant to mterm |
|---|---|---|
| **herdr** (herdr.dev, Rust, Apache-2.0) | Spaces per repo, tab per task. Agent status via OSC 7501. `herdr-core` background server keeps agents alive. Add machines over SSH (Tailscale works). **Already has iPhone/iPad/Android apps** (see which agent needs you, open live terminal, reply by typing/voice/screenshot). Config in `~/.config/herdr-app` as JSON + JS "mods". Agents can spawn helper agents and get reports back. macOS/Linux/Windows. | This is the closest thing to your spec. The gaps vs. your list: no home-screen widget, no mterm↔mterm P2P API (machines are added over SSH, hub-and-spoke), keyboard-first TUI, no "project directories" concept beyond a space. |
| **Zellij** (Rust) | 0.44: native Windows, attach to remote sessions over HTTPS, read-only sharing. 0.45: mobile web UI, nested sessions. WASM plugin system. | Candidate *foundation* instead of writing a multiplexer from scratch. Mouse support exists but it's not mouse-first. |
| **WezTerm** | GUI terminal with a built-in mux server (`wezterm-mux-server`), works natively on Windows, remote mux domains over SSH/TLS/unix socket with auto-reconnect, Lua config. | Proves the "daemon + GUI client" model works cross-platform. Could be the rendering layer if you go GUI. |
| **tmux** | The reference daemon. `tmux -CC` control mode is a real bidirectional protocol. No native Windows. | The model for "always running"; not a candidate base because of Windows. |
| **Claude Squad / Agent Deck / Superset / cmux / amux** | TUI/desktop managers that run Claude Code, Codex, OpenCode etc. side by side, often one git worktree per agent. | Worktree-per-agent is a feature you're missing (see §4.3). |
| **Coder mux** | Runs its *own* agent loop (not external CLIs), with isolation runtimes: local, worktree, SSH, Docker, devcontainer. | Opposite philosophy; you want to host the vendors' CLIs. |

### Phone control of agents
| Tool | Model | Takeaway |
|---|---|---|
| **Claude Code Remote Control** (official) | Local session stays the real session; claude.ai/code or the Claude app is a window into it. Connect via URL or **QR code**. Dies if the local `claude` process exits. | mterm can keep the process alive *and* can show the Remote Control QR/URL in its UI. Don't rebuild this; wrap it. |
| **Codex Remote** (official, in ChatGPT app) | QR pairing, one device per host, Noise-encrypted relay, `codex remote-control pair` prints a code for headless hosts. | Same — surface it from mterm rather than reimplement. |
| **Happy / Happier** (open source) | `happy` wrapper around `claude`; QR pairing; E2E encrypted (TweetNaCl) relay; keys never leave device. | Reference implementation of QR enrollment + E2E relay. Open source — read it. |
| **Omnara** (YC S25) | Daemon mirrors Claude Code / Codex to web + mobile; push when agent needs input; approve from phone. | The "notify me when it needs me" UX. |
| **Wolfpack** (open source, MIT) | Browser + PWA command center across machines, **Tailscale for remote access, no hosted relay**, Rust PTY broker so agents survive restarts, `wolfpack doctor`, QR to add phone. | Architecturally the closest to *your* multi-machine + Tailscale idea. Small project (~27 stars). Worth reading the broker design. |
| **Remobi / Faryo / tmux-mobile / muxplex** | Touch-friendly web front-ends for tmux (swipe between windows, pinch zoom). | Evidence that a PWA over xterm.js is a viable *first* phone client before a native app. |
| **Moshi / Blink / Termius** | iOS SSH/mosh terminals; Moshi has an "agents feed" and Apple Watch actions and knows about tmux/zellij/herdr. | Fallback phone path: mterm just needs to be reachable over SSH/mosh. |

### Building blocks
| Piece | Note |
|---|---|
| **OSC 7501 "Program Status Protocol"** (Mitchell Hashimoto, rev 0.2, Oct 2026) | Program writes `OSC 7501 ; key=value:... ST` to report idle/working/blocked/done/error. Herdr uses it; OpenTUI emits it; Cursor "on radar". **Adopt this as mterm's primary agent-state signal** and fall back to heuristics (Claude Code hooks, screen patterns) when absent. |
| **Claude Code hooks** | `Notification` with `permission_prompt` / `idle_prompt` matchers, and `Stop`. `idle_prompt` is ~60s delayed; `Stop` is immediate. Cross-session `SendMessage` exists; agent teams behind `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`. mterm can install a hook that posts state to `mtermd` → instant "needs you" without screen-scraping. |
| **Tailscale `tsnet`** (Go) | Embed a Tailscale node in your own binary: own IP, DNS name, HTTPS cert, ACL tags — no `tailscaled` needed. Rust bindings exist (`tailscale-rs`), Dart/Flutter wrapper (`tailscale` pkg, userspace on iOS, no VPN entitlement). This is how `mtermd` on machine A talks to `mtermd` on machine B without opening ports. |
| **Tailscale Serve / Funnel** | TLS-terminated HTTPS for a web client on the tailnet (Serve) or public internet (Funnel). Port 443 is Serve *or* Funnel, not both. |
| **Agent-to-agent buses** | agent-bus, Patchcord, Repowire, agentbus, Komnet (git-backed), nexscope — all expose "send/read message" as MCP tools backed by SQLite/dir/relay. Pattern to copy: **mterm exposes an MCP server + HTTP API; each agent pane gets an address.** |
| **iOS WidgetKit** | `widgetURL` deep-links a tap to a specific screen; `Button(intent:)` + App Intents for in-place actions (e.g., "set focus project") without opening the app. Needs a native app (Swift, or Expo `apple-targets`). Widgets refresh on a budget — push a tiny state snapshot, not a terminal. |
| **xterm.js + WebSocket (ttyd-style)** | Fastest route to a browser/PWA client; xterm.js shows a keyboard button on touch. Zellij already ships this. |

---

## 3. Options you might be missing (decisions to make)

1. **Build vs. base.** Three realistic paths:
   - **A. Own daemon in Rust or Go** (herdr/wolfpack style) — full control over mouse UI, API, and P2P; most work. Go gets `tsnet` natively; Rust gets `ghostty-vt`/`alacritty_terminal` for a VT parser and `tailscale-rs` for Tailscale.
   - **B. Zellij as the engine** — write mterm as Zellij plugins + a companion daemon. You inherit panes, Windows support, web client, remote attach. You're constrained by Zellij's plugin API for mouse/right-click.
   - **C. tmux/WezTerm-mux as engine** — fastest demo, weakest on Windows (tmux) or on extensibility (WezTerm Lua).
   - *Recommendation:* **A in Go** (tsnet + one static binary per OS + easy PTY handling via `creack/pty`/ConPTY) with a **web-tech GUI client first** (xterm.js in a Tauri/Electron/WebView shell), because a web client gives you mouse/right-click for free, becomes the PWA phone client, and is the same code on all three desktops. TUI client can come later.

2. **Where does the phone connect?** (a) Tailscale-only (private, no relay, needs Tailscale app on phone — or embedded tsnet in your app), (b) your own E2E-encrypted relay (Happy model; works on cellular with no VPN), (c) both. Tailscale-only is simplest and matches "works over Tailscale"; a relay is what makes QR pairing "just work" for someone without Tailscale.

3. **Native iOS app vs. PWA.** The widget *requires* native. Plan: PWA first (ships in weeks), native Swift/Expo app with WidgetKit second. The native app can be a thin shell around the same web terminal.

4. **Agent isolation.** Every competitor ended up adding **git worktree per agent/task** so parallel agents don't collide. Add it to the project model from day one.

5. **Don't rebuild vendor remote control.** Claude Code Remote Control and Codex Remote already do session mirroring with QR pairing. mterm's value is *above* them: keeping processes alive, the project list, cross-machine, the widget. Show their QR codes inside mterm.

6. **Status protocol.** Standardize on OSC 7501 + Claude Code hooks instead of regex on output; herdr already proved this works.

7. **Security model.** Anything that can type into a terminal is shell access. Pairing must bind a *device key* (not a password), be revocable per device, and ideally support a read-only mode for the widget/glance.

8. **Things no one in the space does well yet** (potential differentiators): home-screen widget with "focus project"; mouse-first; peer-to-peer mterm↔mterm (not hub-and-spoke SSH); a project that spans machines (frontend pane on Mac, GPU pane on Linux box, one tab strip).

---

## 4. Feature list (review / change / add)

### 4.1 Core daemon (`mtermd`)
- [ ] **P0** Always-running daemon per machine; survives client disconnect, client crash, and daemon *upgrade* (hand off PTYs or restart-reattach like Wolfpack's broker).
- [ ] **P0** PTY management: spawn, resize, scrollback buffer, VT parsing server-side so late-joining clients get the current screen.
- [ ] **P0** Native on macOS, Linux, Windows (ConPTY); single static binary; `mterm doctor` self-check.
- [ ] **P0** Local API over unix socket / named pipe (JSON-RPC or gRPC) — everything the UI can do, the API can do.
- [ ] **P1** HTTP + WebSocket API (same surface) for web/phone clients.
- [ ] **P1** Auto-start on login (launchd / systemd user / Windows service).
- [ ] **P2** Scrollback persisted to disk per pane; search.
- [ ] **?** Session recording / replay (asciicast).

### 4.2 Projects
- [ ] **P0** Project = name, color/icon, one or more directories, default agent(s), saved layout, env vars.
- [ ] **P0** Create / rename / archive projects; project list across all machines.
- [ ] **P0** "Open project" restores all its terminals (or attaches if already running).
- [ ] **P0** Launchers: Claude Code, Codex, Antigravity (`agy`), Gemini/other, plain shell — per-launcher flags and working dir.
- [ ] **P1** Multiple directories per project (monorepo, frontend+backend) with per-pane cwd.
- [ ] **P1** Git worktree per task/agent, auto-created and cleaned up; show branch + dirty state in tab.
- [ ] **P1** Project templates ("new project from template: Claude + Codex reviewer + test watcher").
- [ ] **P1** "Focus project" (global, synced to widget).
- [ ] **P2** Per-project notes / task list visible to agents (an `AGENTS.md` / `PROJECT.md` mterm maintains).
- [ ] **?** Project spanning machines (panes on different hosts in one tab strip).

### 4.3 Agent awareness
- [ ] **P0** Per-pane state: idle / working / blocked (needs you) / done / error, via **OSC 7501**.
- [ ] **P0** Fallback detectors: Claude Code hooks (`Stop`, `Notification:permission_prompt`), process-name + output heuristics for agents that don't emit 7501.
- [ ] **P0** Sidebar / tab badges and sort "needs you first"; sound + desktop notification.
- [ ] **P1** mterm installs the hook into Claude Code settings for you (opt-in).
- [ ] **P1** Push notification to phone when any agent is blocked (via APNs through your relay, or ntfy/Pushover as a stopgap).
- [ ] **P2** Timeline per pane: when it started, how long waiting, last message.
- [ ] **?** Cost/token meter per pane if the agent exposes it.

### 4.4 Agent ↔ agent communication (IPC)
- [ ] **P0** Every pane has an address (`project/pane-name@machine`). API: `send`, `read inbox`, `wait-until(blocked|done)`, `spawn helper pane`.
- [ ] **P0** Exposed as an **MCP server** so Claude Code, Codex, and Antigravity can use it as tools without wrappers.
- [ ] **P0** Also a CLI (`mterm send codex-review "review the diff"`, `mterm wait --done codex-review`) for scripts and for agents without MCP.
- [ ] **P1** Message delivery semantics: durable inbox, ack, don't silently drop (lesson from agent-bus/agentbus).
- [ ] **P1** Shared project "bulletin board" (key/value + files) agents can read/write.
- [ ] **P1** Cross-machine messaging (same API, routed over the mesh in §4.7).
- [ ] **P2** Orchestration recipes: "fan out to N helpers, collect reports" like herdr's helper agents.
- [ ] **?** Human-in-the-loop gate: agent can request approval; approval arrives from phone.

### 4.5 Desktop UI (mouse-first)
- [ ] **P0** GUI client (web tech in Tauri/Electron, or native) with real mouse: click to focus, drag to resize splits, drag tabs between windows, scroll wheel, text selection + copy.
- [ ] **P0** **Right-click context menus** on: pane (split, rename, restart agent, send to agent, kill, copy path), tab, project, machine, sidebar.
- [ ] **P0** Command palette (Ctrl/Cmd-K) and configurable keybindings for everything the mouse can do.
- [ ] **P0** Sidebar: machines → projects → panes with state badges.
- [ ] **P1** Layouts: save/restore per project; presets (2-up, 3-col, main+stack).
- [ ] **P1** Theming, fonts, ligatures; light/dark follows OS.
- [ ] **P1** Optional **TUI client** (`mterm attach`) for SSH-only situations.
- [ ] **P2** Broadcast input to multiple panes.
- [ ] **P2** Image/Kitty graphics passthrough (Zellij 0.45 just added this).
- [ ] **?** Embedded diff viewer / file browser pane beside the terminal.

### 4.6 Device enrollment (QR)
- [ ] **P0** `mterm pair` shows a QR (and short code for headless). Phone/computer scans → exchanges device public keys → device appears in "Devices" with name, last seen, permissions.
- [ ] **P0** Per-device revoke; permissions: full / read-only / specific projects.
- [ ] **P1** Enrolling a *computer* via QR joins it to your mesh (exchange tsnet identity or your own keypair).
- [ ] **P1** Show vendor QR codes (Claude Code Remote Control `/mobile`, `codex remote-control pair`) inside mterm for people who also want the official apps.
- [ ] **?** Account-less by design (keys only), or optional account for discovery.

### 4.7 Multi-machine mesh (mterm ↔ mterm)
- [ ] **P0** Works over Tailscale: `mtermd` embeds `tsnet` (own node name like `mterm-macbook`) or uses the host's Tailscale; discovery via tailnet peers / MagicDNS.
- [ ] **P0** One client shows projects and panes from all reachable machines; attach to a remote pane with the same UI.
- [ ] **P1** Peer-to-peer (no central server); any machine can be a client of any other.
- [ ] **P1** Fallback transport when Tailscale isn't available: SSH tunnel (like herdr's saved SSH machines).
- [ ] **P1** mTLS / device-key auth on every API call; Tailscale ACL tags respected.
- [ ] **P2** Move or clone a project between machines (sync dirs via git; re-create panes).
- [ ] **?** Optional self-hosted relay for phones on cellular without Tailscale.

### 4.8 iPhone app + widget
- [ ] **P0** Phase 1: PWA (xterm.js over WebSocket, tailnet-only via Tailscale Serve) — project list, agent states, tap to open terminal, touch-friendly (swipe tabs, pinch zoom, on-screen Ctrl/Esc/arrows).
- [ ] **P0** Phase 2: native iOS app (SwiftUI, or Expo with `apple-targets`) wrapping the same client, with push notifications.
- [ ] **P0** **Home-screen widget** (small/medium/large): projects with state badges, "needs you" count, current focus project. Tap a project → deep link opens its terminals.
- [ ] **P1** Widget interactivity (App Intents): set/clear focus project, "approve"/"continue" the blocked agent without opening the app.
- [ ] **P1** Lock-screen widget / Live Activity for "agent running in <project>" with elapsed time.
- [ ] **P1** Voice reply (dictation into the pane) — herdr's mobile app has this.
- [ ] **P2** Apple Watch complication: needs-you count; one-tap "continue".
- [ ] **P2** Android equivalent (widget via Glance).
- [ ] **?** Embedded tsnet in the app so the Tailscale app isn't required on the phone.

### 4.9 Configuration & extensibility
- [ ] **P0** Plain-file config (`~/.config/mterm/*.json|toml`) with a documented schema so your agent can edit it (herdr ships an `AGENTS.md` for this — copy that idea).
- [ ] **P1** Plugin/mod hooks (JS/Lua/WASM) for custom context-menu items, launchers, and status detectors.
- [ ] **P1** `mterm` CLI covers 100% of the API; `--json` output.
- [ ] **P2** Import projects from existing tmux/zellij sessions or from a directory scan.

### 4.10 Security
- [ ] **P0** Treat every client as shell access; no unauthenticated listener, ever; localhost + tailnet only by default.
- [ ] **P0** Device keys, revocation, audit log of who attached/sent input from where.
- [ ] **P1** Read-only attach mode (for widgets, glancing, screen-sharing).
- [ ] **P1** E2E encryption if a relay is ever introduced (Happy's TweetNaCl approach).
- [ ] **?** Approval policy: certain agent actions require phone confirmation.

---

## 5. Suggested v1 scope (if you want a cut line)

**v1 (desktop + mesh):** §4.1 P0, §4.2 P0, §4.3 P0, §4.4 P0, §4.5 P0, §4.7 P0 (Tailscale only), §4.6 P0 for computers, PWA phone client from §4.8.
**v1.5 (phone):** native iOS app + widget + push.
**v2:** worktrees, cross-machine projects, plugins, relay.

---

## 6. Open questions for you

1. Language/stack preference: Go (easiest Tailscale, PTY, single binary) vs Rust (herdr/zellij ecosystem, `ghostty-vt`)? Do you have a strong preference for the GUI layer (Tauri/Electron/native)?
2. Should mterm *host* its own Zellij/tmux-style pane engine, or build on Zellij's plugin system to skip 6 months of multiplexer work at the cost of mouse-UI control?
3. Phone: is "must have Tailscale app installed on the phone" acceptable for v1?
4. Is an iOS developer account / App Store release in scope, or TestFlight/personal sideload only? (Widget needs a native app either way.)
5. Which agents on day one: Claude Code, Codex, Antigravity — anything else (Gemini, OpenCode, Amp, aider)?
6. Do you want mterm to be open source? (Affects whether you can borrow from AGPL projects like Coder mux vs Apache/MIT ones like herdr/Wolfpack/Happy.)

---

## Sources
- herdr: https://herdr.dev/ · https://github.com/herdrdev/herdr · https://betterstack.com/community/guides/ai/herdr-ai-agent/ · https://www.bitdoze.com/herdr-agent-multiplexer/
- Antigravity CLI: https://virtualizationreview.com/articles/2026/05/19/google-moves-gemini-cli-into-antigravity-cli-as-agent-platform-expands.aspx · https://dev.to/arindam_1729/antigravity-cli-a-hands-on-guide-to-googles-terminal-coding-agent-5bc7
- Claude Code Remote Control: https://code.claude.com/docs/en/remote-control
- Claude Code hooks / messaging: https://code.claude.com/docs/en/hooks · https://code.claude.com/docs/en/cross-session-messaging · https://claudeissues.com/issue/13024-feature-add-hook-for-when-claude-is-waiting-for-user-input
- Codex Remote / QR pairing: https://codex.danielvaughan.com/2026/07/28/codex-remote-ga-noise-encrypted-relay-qr-pairing-mobile-agent-control-headless-ci-cd/ · https://aiweekly.co/alerts/openai-codex-remote-reaches-all-chatgpt-plans-with-mobile-access
- Happy: https://apps.apple.com/us/app/6748571505 · https://alternativeto.net/software/happy-claude-code-client/about
- Omnara: https://docs.omnara.com/how-it-works · https://github.com/omnara-ai/omnara
- Wolfpack: https://dev.to/sendlogz/i-built-a-private-control-room-for-coding-agents-running-on-my-machines-32ni · https://socket.dev/npm/package/wolfpack-bridge
- Remobi / Faryo / amux: https://ai.engineer/talks/remobi-app-don-t-change-your-terminal-workflow-for-mobile · https://github.com/snailflyer/faryo · https://amux.io/features/mobile-pwa/
- Moshi: https://getmoshi.app/articles/best-ios-terminal-app-coding-agent
- Agent managers: https://nimbalyst-website.pages.dev/blog/best-agent-management-tools-2026/ · https://superset.sh/compare/superset-vs-mux
- Zellij: https://zellij.dev/news/remote-sessions-windows-cli/ · https://zellij.dev/news/nested-sessions-kitty-graphics-new-ui/ · https://zellij.dev/tutorials/web-client/
- WezTerm mux: https://www.besthub.dev/articles/wezterm-the-ultimate-windows-terminal-for-claude-code-codex-7a3110fe1b88 · https://workmux.raine.dev/guide/wezterm
- OSC 7501: https://pkg.go.dev/tuios.dev/progstatus · https://zeli.app/story/49984159 · https://forum.cursor.com/t/emit-osc-7501-program-status-in-cursor-agent-idle-working-blocked-done/177003
- Tailscale tsnet: https://tailscale.com/docs/features/tsnet · https://tailscale.com/blog/easier-building-with-tailscale · https://pub.dev/documentation/tailscale/0.9.0/
- ttyd / Tailscale Serve: https://fedora.mist-walleye.ts.net/articles/claude-ttyd-tailscale-caddy · https://mrkaran.dev/posts/web-terminal-homelab/
- Agent-to-agent buses: https://github.com/rishabhjava/agent-bus · https://github.com/ppravdin/patchcord · https://dev.to/prassannaravishankar/repowire-a-mesh-network-for-ai-coding-agents-3h2b
- WidgetKit: https://developer.apple.com/documentation/widgetkit/adding-interactivity-to-widgets-and-live-activities.md · https://developer.apple.com/tutorials/data/documentation/widgetkit/linking-to-specific-app-scenes-from-your-widget-or-live-activity.md · https://reactnativerelay.com/article/react-native-ios-widgets-expo-apple-targets-2026
