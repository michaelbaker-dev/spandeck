//! What every system's terminal has in common: how a pane's program is
//! described, how it ended, and what can be asked of it.
//!
//! The terminal itself (a Unix PTY, or a Windows pseudoconsole) is in
//! `sys`, the one place platform code lives. See DECISIONS.md, "Windows".

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use crate::sys::{pid_alive, Pty, PtyReader, PtyWriter};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpawnSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Added to the daemon's environment.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Removed from the daemon's environment before `env` is applied.
    #[serde(default)]
    pub env_remove: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

impl SpawnSpec {
    pub fn shell(cols: u16, rows: u16) -> Self {
        let (program, args) = crate::sys::default_shell();
        SpawnSpec {
            program,
            args,
            cwd: None,
            env: vec![],
            env_remove: vec![],
            cols,
            rows,
        }
    }
}

/// How a pane's process ended.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExitInfo {
    Code(i32),
    /// Ended by a signal. Never seen on Windows, which has none.
    Signal(i32),
    /// The process is gone but its status could not be collected.
    Unknown,
}

/// What a pane's programs can be told, all of them at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// The terminal is going away. Unix: SIGHUP to the process group.
    /// Windows: the pseudoconsole closes, which asks every program on it to end.
    Hangup,
    /// Please end. Unix: SIGTERM. Windows has no such request: the job is ended.
    Terminate,
    /// End now. Unix: SIGKILL to the process group. Windows: the job is ended.
    Kill,
}
