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
//!   add       add a machine from the Control Deck: SSH in, install, start, ask to join
//!   notify    the inbox and push rules
//!   agents    agent programs here: found, versioned, installed when missing, updated
//!   setup     fleet-wide agent setup: MCP servers, skills, model defaults
//!   deck      fleet-wide deck document: global layouts and pins
//!   vault     AgentVault: station token, key use through the vault's proxy
//!   usage     what each pane's agent has used, and the station's totals
//!   prices    the public list of model prices and context windows, fetched daily
//!   http      the one outgoing HTTPS client builder, and what it trusts

pub mod add;
pub mod agents;
pub mod client;
pub mod daemon;
pub mod db;
pub mod deck;
pub mod doctor;
pub mod fleet;
pub mod git;
pub mod hooks;
pub mod http;
pub mod links;
pub mod manage;
pub mod memory;
pub mod messages;
pub mod notify;
pub mod orders;
pub mod paths;
pub mod prices;
pub mod project;
pub mod recipes;
pub mod relay;
pub mod rpc;
pub mod server;
pub mod setup;
pub mod tasks;
pub mod trust;
pub mod usage;
pub mod vault;

pub use daemon::{Authority, BusEvent, Caller, Daemon, Error, StartOpts, PROTOCOL, VERSION};
pub use paths::{Config, Paths};
