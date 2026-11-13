//! spandeck terminal layer: PTYs, server-side screen state, and panes.
//!
//! Platform code (PTY, signals) lives in `pty.rs` only, so a Windows port
//! replaces one file.

pub mod pane;
pub mod pty;
pub mod screen;

pub use nix::sys::signal::Signal;
pub use pane::{Pane, PaneEvent, PaneHandoff, Sink};
pub use pty::{pid_alive, ExitInfo, Pty, SpawnSpec};
pub use screen::{Screen, ScreenEvent, ScreenInfo, DEFAULT_HISTORY};
