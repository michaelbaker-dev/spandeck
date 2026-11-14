//! spandeck terminal layer: PTYs, server-side screen state, and panes.
//!
//! Platform code lives in `sys` only: `sys/unix.rs` and `sys/windows.rs`
//! behind one interface (DECISIONS.md, "Windows").

pub mod host;
pub mod hosted;
pub mod pane;
pub mod pty;
pub mod screen;
pub mod sys;

pub use hosted::HostClient;
pub use pane::{Pane, PaneEvent, PaneHandoff, Sink};
pub use pty::{pid_alive, ExitInfo, Pty, Signal, SpawnSpec};
pub use screen::{Screen, ScreenEvent, ScreenInfo, DEFAULT_HISTORY};
