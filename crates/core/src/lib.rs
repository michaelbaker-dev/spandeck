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
//!   orders    standing orders at five layers
//!   fleet     discovery over Tailscale, trust, peer calls, stop-all
//!   notify    the inbox and push rules
//!   setup     fleet-wide agent setup: MCP servers, skills, model defaults

pub mod client;
pub mod daemon;
pub mod db;
pub mod doctor;
pub mod fleet;
pub mod git;
pub mod hooks;
pub mod memory;
pub mod messages;
pub mod notify;
pub mod orders;
pub mod paths;
pub mod project;
pub mod recipes;
pub mod relay;
pub mod rpc;
pub mod server;
pub mod setup;
pub mod tasks;

pub use daemon::{Authority, BusEvent, Caller, Daemon, Error, StartOpts, PROTOCOL, VERSION};
pub use paths::{Config, Paths};
