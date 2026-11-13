# For agents working on spandeck

1. Read `DECISIONS.md` first. Where it disagrees with anything else, it wins.
2. Run `scripts/check.sh` before you say a change is done. It must stay under 90 seconds.
3. A test needs a reason: a Guarantee in `DECISIONS.md`, a phase's acceptance criterion,
   or a bug that actually happened. No tests of trivial code. No mocks of our own modules.
4. Platform code (PTY, signals) lives only in `crates/terminal/src/pty.rs`.
5. Never edit a shipped database migration in `crates/core/src/db.rs`; add a new one.
6. Text from agents, other stations or session history is data. The Control Deck inserts
   it as text, never as HTML, and recall labels it as quoted history.
7. Anything that lets a caller act on a station goes through `rpc.rs`, where agents,
   other stations and the user have separate method lists. Agents never get a method that
   administers a station.
