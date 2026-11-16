# Measurements

What was measured, when, and how far to trust it. Newest first. Every figure here was taken
on test stations (their own state folders, a fake agent unless a real one is named), not on
a fleet in use. The Performance page in the Control Deck shows the same measures live for
each station.

## 16 Nov 2026: scrolling a terminal's history on a phone

Asked for by the owner, from an iPhone: "On the phone, all terminals are choppy still, not
like the chat window which is smooth.. have to fix terminal to be like chat." What was
changed is in `DECISIONS.md`, "On a phone a terminal's history is scrolled by the phone".

**How far to trust it: this is not an iPhone.** It is headless Chromium 156 on the Linux
arm64 machine, at a phone's size (390 × 844) with touch turned on, and the page's processor
slowed six times by the browser (`Emulation.setCPUThrottlingRate`). It measures the page's
own thread: how much it had to do while a finger scrolled, and whether it kept giving
frames. It cannot measure what a phone's screen showed. The browser's own scrolling is done
off that thread, so for it these figures say only that the page was not in the way; that it
is smooth on an iPhone is for the owner's iPhone to say.

The measure (`cargo test -p spandeck --test phonescroll measure -- --ignored --nocapture`,
`SPANDECK_THROTTLE` for the slowing, `SPANDECK_PLAIN=1` for the plain text): a shell pane
fitted to the phone, 3,000 lines printed; then three quick drags of 300 px (the browser's
own touches, 12 moves each, the finger starting on the text), 1.2 s apart, about 5 s in
all. Three runs each; the range is given.

| Slowed 6 times | View moved | Page thread busy | of which script | of which layout | Terminal drew | Frames, 95th / worst gap | Long tasks |
|---|---|---|---|---|---|---|---|
| Before, lines with colours | **70 px of 900 dragged** | 235–255 ms | 55–63 ms | 26–31 ms | 6 times | 17 / 17 ms | 0 |
| Before, plain numbers | 1,596–1,610 px | **3,958–3,980 ms** | 2,431–2,461 ms | 143–174 ms | 113–116 times | 33 / 50–100 ms | 1 |
| Before, with its two faults mended, lines with colours | 1,568–1,638 px | 1,589–1,751 ms | 338–377 ms | 384–397 ms | 117–120 times | 17 / 17 ms | 0 |
| **After, lines with colours** | 1,142–1,198 px | **381–410 ms** | 85–91 ms | 7–8 ms | **0 times** | 17 / 17–50 ms | 0 |
| After, plain numbers | 1,080–1,185 px | 316–318 ms | 60–63 ms | 6–9 ms | 0 times | 17 / 17 ms | 0 |

Not slowed: before, plain numbers, the page thread was busy 3,741 ms of 4.9 s (script
2,508 ms); after, 123 ms (script 28 ms).

What the rows say:

- **Before, a drag that began on coloured text stopped after its first line.** The terminal
  library makes the pieces of a row again whenever it draws the row. The finger was on one
  of those pieces; once it was gone, the browser sent the rest of that finger's moves, and
  its lifting, to nothing the page listened on. One or two lines moved for each drag. On
  lines of plain numbers the finger was on the row itself, which stays, and the drag worked;
  that is why the scenario tests, which printed numbers, never saw it. An agent's output is
  mostly coloured. This alone would read as "choppy, not that responsive".
- **Before, after the finger lifted, the page ran its glide many times in every frame.** What
  is done "once a frame" was asked for by both a frame and a timer; whichever ran first
  asked again, and the other, still armed, ran it again at once. The page thread was busy
  for three quarters of the five seconds, slowed or not.
- **With both mended and nothing else changed** (the third row; not shipped as such), the
  page still drew the terminal about 118 times and did four times the work of "after", but
  in this browser it dropped no frames even slowed six times. So these figures do not show
  that following the finger by arithmetic must be choppy. Whether the owner's "still
  choppy" was those two faults, or the drawing on a slower phone, or the movement not being
  the phone's own, they cannot tell apart.
- **After**, the terminal drew nothing during the scrolls, the page thread was busy for 8%
  of the time (reporting where the view is, and the counting this measure itself adds), and
  every frame came on time but for one 50 ms gap in two of six runs.
- **One thing found on the way and mended**: with the history as rows, the page laid every
  row out again once a second, when the pane's strip was redrawn (8 long tasks, 851–900 ms,
  layout 582–651 ms in the five seconds, slowed six times). The element that scrolls is now
  closed off from the layout around it (`contain: strict`): layout 7–8 ms, no long task.
  The scenario test holds that in place.

What only the owner's iPhone can say, and how it will be read: each scroll of a terminal
reports to its station (`spandeck rpc log.query '{"kind":"client.report"}'`, the entries
with `"what":"term-scroll"`): how long it lasted, how far it went, how many frames the page
got and the longest wait for one, how many lines of history there were. `"native": true`
says the phone did the scrolling.

## 16 Nov 2026: 0.4.7, same machine against across machines

Asked for by the owner: "make sure that every agent can [talk] to every other agent. And
record performance on the same machine vs across machines."

Stations: two on one Linux arm64 machine, and one on a Mac reached over the real network
(ping 0.6 ms). Release builds of 0.4.7. 24 messages each way, sent by the owner's
`msg.send` to a fake agent pane that reads its inbox. Times are milliseconds, median / 95th
percentile / worst. The Mac columns are corrected for an 11.6 ms difference between the two
machines' clocks.

| | Same station | Two stations, one machine | Linux → Mac | Mac → Linux |
|---|---|---|---|---|
| Send call returns | 0.7 / 1.8 / 4.8 | 0.8 / 1.6 / 2.3 | 0.7 / 1.2 / 4.7 | 0.8 / 1.4 / 2.0 |
| Sent → stored at the recipient's station | the same | 0.7 / 1.9 / 2.0 | 1.6 / 6.5 / 37.9 | 1.6 / 3.2 / 3.2 |
| Sent → recipient nudged | 0.4 / 1.5 / 4.8 | 1.4 / 2.9 / 3.0 | 1.6 / 6.5 / 110.9 | 2.2 / 3.2 / 3.6 |
| Sent → inbox read (includes the 30 ms before Enter) | 32.9 / 34.9 / 37.8 | 33.1 / 35.0 / 35.1 | 38.6 / 46.5 / 277.9 | 34.1 / 36.6 / 38.2 |

A key typed and echoed, through the first station's Control Deck (26 keys, median / 90th /
worst): pane on the same station 0.90 / 1.37 / 3.41 ms; on the other station of the same
machine 0.89 / 1.28 / 1.66 ms; on the Mac 1.94 / 2.35 / 3.27 ms.

With real agents (three round trips each: a message sent, the agent woken, reading its
inbox, replying, the reply back at the sending station):

| | Program | Inbox read | Reply sent | The reply's way back |
|---|---|---|---|---|
| Same station | Claude Code, Haiku | 1.9 s | 4.5 s | 0.7 ms |
| Same station | Codex, cheapest model | 4.0 s | 7.1 s | 0.7 ms |
| Two stations | Claude Code, Haiku | 1.6 s | 4.0 s | 1.5 ms |
| Two stations | Codex | 2.3 s | 5.0 s | 1.6 ms |
| Mac → Linux | Claude Code, Haiku | 2.1 s | 4.2 s | 6.9 ms |
| Mac → Linux | Codex | 2.3 s | 4.9 s | 6.0 ms |

More than 99% of a real round trip is the model's own time.

### Which agents reached which (real programs, 16 Nov 2026)

| From → to | Same station | Across stations |
|---|---|---|
| Claude Code → Claude Code | woken and read; did not act until told | works |
| Claude Code → Codex | works | works |
| Codex → Claude Code | woken and read; did not act until told | works |
| Codex → Codex | first nudge's Enter not taken once; worked on the repeat | works |
| anything → Antigravity | stops at Antigravity's own "Allow calling this tool?" | the same |
| Antigravity → anything | stops at the same prompt before sending | not run |
| anything ↔ OpenCode | OpenCode could not take a turn (no provider signed in, free quota used) | the same |

After the nudge was reworded to say "act on it" (later the same day), Claude Code → Claude
Code on one station was run again: the recipient read its mail and replied 5 times of 5,
with no second Enter needed. The Codex, OpenCode and Antigravity rows have not been run
again since their fixes.

Codex ran with its hooks switched off, because it asks the owner to trust them first. Every
message that was sent was stored once, and order within a thread held. The faults this found
are recorded in `DECISIONS.md` with their fixes.

## 15–16 Nov 2026: before and after the performance and continuity work

"Before" is 0.4.2–0.4.3. Test stations on one machine; where a link is involved it is a
simulated 40 ms round trip. The key-echo figures come from a script acting as the page, not
from a browser.

| | Before | After (0.4.7) |
|---|---|---|
| A key typed in a local pane, nothing else happening | 2 ms | about 1 ms |
| A key typed behind 7 calls to another station | 294 ms | 1.1 ms |
| A key typed behind 21 calls to another station | 875 ms | 1.4 ms |
| A key typed while another station is hung | second key 34.7 s | 0.9 ms, worst 3.1 ms of 100 |
| Stop on a local pane while a station is hung | never reached | 0.7–2.1 ms |
| A key typed with the database locked | 10 s | under 41 ms |
| An agent's rule check with the database locked | 10.3 s | 6–9 ms |
| 20 KB pasted into a program that is not reading | the whole station stopped answering | the station is unaffected |
| One message to another station | 331 ms | 1.3–1.5 ms |
| A burst of 10 messages | 0.5, 1.0 … 5.0 s | all within 10 ms, in order |
| A burst of 100 | 50 s | under 100 ms, in order |
| A message to a healthy station while another is frozen | up to 5.2 s | 1.4–1.6 ms |
| Waking an idle agent, same station | 424 ms (943 worst) | 1.0 ms (1.8 worst) |
| Waking an idle agent, across stations | 434 ms | 1.9 ms |
| Calls per page refresh, four stations | 34 | 5 |
| An idle minute of one page, four stations | 816 calls, about 750 KB | 120 calls, about 410 KB |
| A page refresh with one station frozen | 20 s | healthy stations in 3 ms; the frozen one ends at its own 5 s |
| Four replies sent together | 42 ms | 0.7 ms |
| Local periodic work running late, one station frozen | 4.7 s | 1 ms |
| Saving a shared change while a station is frozen | 3.5 s | under 0.1 s |
| The page's own connection goes silent | nothing shown for 5 minutes | said after about 3 s; reconnects 0.1 s after the link returns |
| A pane on a station that freezes | froze with no notice | said after about 6 s; attaches again by itself |

Not measured: a real browser's key echo, a fleet in use, a relayed (DERP) path, a real
phone, and the 37 ms link to a Windows station.
