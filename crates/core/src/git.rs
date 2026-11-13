//! Small git helpers. Everything shells out to the user's git.

use std::path::Path;
use std::process::Command;

pub fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("git not runnable: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn head(cwd: &Path) -> Option<String> {
    git(cwd, &["rev-parse", "HEAD"])
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn is_repo(cwd: &Path) -> bool {
    git(cwd, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|s| s == "true")
}

pub fn is_dirty(cwd: &Path) -> bool {
    git(cwd, &["status", "--porcelain"]).is_ok_and(|s| !s.is_empty())
}

pub fn toplevel(cwd: &Path) -> Option<String> {
    git(cwd, &["rev-parse", "--show-toplevel"])
        .ok()
        .filter(|s| !s.is_empty())
}
