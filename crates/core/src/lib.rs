//! spandeck core: the daemon.
//!
//! Read DECISIONS.md at the repo root first. Module map:
//!   daemon    panes, event bus, ship's log, crash recovery, live handoff
//!   hooks     vendor hook -> normalized event -> decision; approvals; stuck detection
//!   rpc       the API surface (one dispatcher for socket, HTTP, WebSocket, peers)
//!   server    the local Unix-socket API; client is its blocking counterpart
//!   project   .spandeck/project.toml, open/close/restore
//!   relay     message records and the station-to-station relay (transport-agnostic)
//!   messages  agent mail: addressing, route approval, nudges
//!   tasks     task records, delegation, workspaces, checkpoints, sign-offs
//!   recipes   the review loop
//!   memory    shared project memory and the start digest
//!   links     project links: two projects whose agents message each other and share findings
//!   orders    standing orders at five layers
//!   fleet     discovery over Tailscale, trust, peer calls, stop-all
//!   rounds    work for another station: a round per station, its schedule and back-off
//!   loops     the periodic work, each kind in a loop of its own
//!   stops     stop-all apart from the daemon: rows, sending, the cut-off, applying once
//!   add       add a machine from the Control Deck: SSH in, install, start, ask to join
//!   notify    the inbox and push rules
//!   agents    agent programs here: found, versioned, installed when missing, updated
//!   setup     fleet-wide agent setup: MCP servers, skills, model defaults
//!   deck      fleet-wide deck document: global layouts and pins
//!   look      fleet-wide look document: themes, and what the deck, a layout or a pane uses
//!   fonts     font files the owner uploaded: checked, kept by content id, fetched between stations
//!   prefs     fleet-wide owner preferences: small choices that follow the owner
//!   sync      what the synced documents share: stamps, catching up, taking a newer copy
//!   vault     AgentVault: station token, key use through the vault's proxy
//!   usage     what each pane's agent has used, and the station's totals
//!   prices    the public list of model prices and context windows, fetched daily
//!   perf      counters for the Performance page: where time goes on this station
//!   tailnet   Tailscale key expiry: read from the local client, shown, warned of
//!   http      the one outgoing HTTPS client builder, and what it trusts
//!   restore   after a restart: what is put right and listed, the one notice, `[flags]`
//!   discover  a pane's session found among its program's own files, when no hook names it
//!   screentalk  the screen as a conversation: what was sent to a pane and what it printed after
//!   holds     a stopped or restarted agent stays put: the marks, what they gate, release
//!   rids      a change tried again is done once: request ids, the answers kept, who is given one

pub mod add;
pub mod agents;
pub mod approvals;
pub mod beat;
pub mod client;
pub mod conversation;
pub mod daemon;
pub mod db;
pub mod deck;
pub mod discover;
pub mod doctor;
pub mod fleet;
pub mod foldertrust;
pub mod fonts;
pub mod git;
pub mod guard;
pub mod holds;
pub mod hooks;
pub mod hookstrust;
pub mod http;
pub mod https;
pub mod links;
pub mod look;
pub mod loops;
pub mod manage;
pub mod memory;
pub mod messages;
pub mod modelswitch;
pub mod notify;
pub mod orders;
pub mod paths;
pub mod perf;
pub mod picker;
pub mod prefs;
pub mod prices;
pub mod project;
pub mod questions;
pub mod recipes;
pub mod reflow;
pub mod relay;
pub mod remove;
pub mod restore;
pub mod rids;
pub mod rounds;
pub mod rpc;
pub mod screentalk;
pub mod server;
pub mod setup;
pub mod snapshot;
pub mod startscreen;
pub mod stops;
pub mod supply;
pub mod supply_fleet;
pub mod sync;
pub mod tailnet;
pub mod tasks;
pub mod trust;
pub mod typed;
pub mod usage;
pub mod vault;
pub mod wake;

pub use daemon::{Authority, BusEvent, Caller, Daemon, Error, StartOpts, PROTOCOL, VERSION};
pub use paths::{Config, Paths};
