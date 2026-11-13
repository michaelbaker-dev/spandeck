# Phase 0 spike results (13 Nov 2026, spark2: Linux aarch64, 20 cores)

## 1. Terminal engine: alacritty_terminal

`cargo run --release -p spandeck-terminal --example bench_engines`

| Engine | 50 panes x 8 MB agent-like stream | Per pane |
| --- | --- | --- |
| alacritty_terminal 0.26 (via `Screen`) | 0.47 s wall, 897 MB/s total | 17.9 MB/s |
| vt100 0.18 | 0.36 s wall, 1173 MB/s total | 23.5 MB/s |

- Late-joiner snapshot with 5000 lines of history: 1.46 ms, about 350 KB.
- Visible-grid text for a thumbnail: 0.011 ms.

Decision: **alacritty_terminal**. It is about 25% slower than vt100 but both are two
orders of magnitude beyond what agents emit, and it tracks the modes a real client needs
(mouse, bracketed paste, alt screen, wide characters, kitty keyboard). ghostty-vt was not
measured: it needs the Zig toolchain, which is not installed, and the published Rust crate
is third-party. Revisit only if a rendering bug forces it.

## 2. Live handoff: exec in place

`cargo test -p spandeck-terminal --test handoff`

The old process stops its reader, records `{master fd, child pid, size, screen snapshot}`,
and `exec`s the new binary. Same pid, so:

- PTY masters are inherited by number (dup without CLOEXEC).
- The new image is still the parent of every child: signals and exit statuses work with
  plain `waitpid`. No subreaper, no kqueue, identical on macOS and Linux.
- Output produced during the switch waits in the kernel PTY buffer and is read by the new
  image: the probe checks for no gap and no duplicate across the handoff.
- Rollback: the new image can exec the previous binary with the same state file.

Not covered by exec in place: a new binary that crashes hard after exec. Then terminals are
lost and agents resume by session id, which is what Guarantees allows.

Not run: macOS. The code is POSIX-only (`openpty`, `dup`, `exec`, `waitpid`), but the test
has only run on Linux.

## 3. Session file keeps pre-compaction turns

Claude Code 2.1.323, a real session with two automatic compactions (2921 lines):

- All 1175 lines before the first compaction are still in the file (118 user, 207 assistant).
- Compaction appends a `system` line with `subtype: "compact_boundary"` and
  `compactMetadata` (trigger, preTokens, postTokens), then a `user` line with
  `isCompactSummary: true`. Nothing earlier is rewritten.

So the archive can follow the file by offset and treat `compact_boundary` as a marker.

Codex: **not verified**. Codex is not installed on this station.

## 4. Codex hooks

Not run: Codex is not installed. The Codex adapter is built from documented payloads and is
marked unverified in the enforcement table until a live canary runs on a station with Codex.
