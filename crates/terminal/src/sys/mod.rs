//! The platform layer: the only place in spandeck that names `cfg(unix)`,
//! `cfg(windows)`, `nix`, `libc` or `windows-sys` (DECISIONS.md, "Windows").
//!
//! `unix.rs` and `windows.rs` each provide the same items:
//!
//!   terminals     `Pty`, `PtyReader`, `PtyWriter`, `pid_alive`, `LOCAL_HANDOFF`,
//!                 `PANE_HOST`
//!   local socket  `local::{Listener, Stream, Conn, connect, pipe_name}`
//!   files         `write_private`, `private_dir`, `is_executable`,
//!                 `executable_names`, `default_dirs`, `replace_file`,
//!                 `set_executable`, `file_mode`
//!   processes     `replace_process`, `UPGRADE_KEEPS_PID`, `spawn_detached`,
//!                 `end_leftover_child`, `close_inherited`,
//!                 `wait_for_exit`, `shutdown_signal`, `no_window`, `ignore_interrupt`,
//!                 `interrupted`, `request_stop`
//!   supervising   `supervise`, `Supervisor`, `Watched`, `supervised`,
//!                 `process_started`, `record_successor_in` (Windows; no-ops
//!                 on Unix, where the login service does it)
//!   the machine   `hostname`, `load_average`, `random_bytes`, `keep_awake_cmd`,
//!                 `hold_awake`
//!   shells        `default_shell`, `shell_line`
//!   a console     `stdin_raw`
//!
//! A caller that only needs a different value on Windows (a folder name, a
//! command) branches on `WINDOWS` in ordinary code, which is then compiled
//! and checked on every system.

pub mod cmdline;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// True on Windows. For choosing a value, not for hiding code.
pub const WINDOWS: bool = cfg!(windows);
