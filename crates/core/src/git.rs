//! Small git helpers. Everything shells out to the user's git.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

/// A folder's branch and how many of its files are changed: what a pane's
/// header says of its folder (glance.rs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brief {
    /// The branch's name. On a detached HEAD, the first seven characters
    /// of the commit it is at.
    pub branch: String,
    pub detached: bool,
    /// Files changed, staged or not, and files git does not know (a folder
    /// of them counts once); ignored files are not counted.
    pub changed: usize,
}

/// What `brief` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    Repo(Brief),
    /// Not in a git repository, or git cannot be run here.
    NotRepo,
    /// git did not finish in the time given, and was ended.
    TooSlow,
}

/// One `git status`, given `limit` and no longer. It takes no lock in the
/// repository (`--no-optional-locks`), so it never stands in the way of
/// the git an agent runs there, and it runs nothing the repository's own
/// settings name (`core.fsmonitor` is a command: it is switched off for
/// this one call, which is made without the owner asking).
pub fn brief(cwd: &Path, limit: Duration) -> Asked {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "status",
            "--porcelain=v2",
            "--branch",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    spandeck_terminal::sys::no_window(&mut cmd);
    let Ok(mut child) = cmd.spawn() else {
        return Asked::NotRepo;
    };
    let Some(out) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Asked::NotRepo;
    };
    // Read as it is written: a repository with very many changes fills
    // the pipe, and git would wait for a reader until the limit.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut head, mut oid, mut changed) = (String::new(), String::new(), 0usize);
        for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
            let line = String::from_utf8_lossy(&line);
            if let Some(v) = line.strip_prefix("# branch.head ") {
                head = v.to_string();
            } else if let Some(v) = line.strip_prefix("# branch.oid ") {
                oid = v.to_string();
            } else if !line.starts_with('#') && !line.is_empty() {
                changed += 1;
            }
        }
        let _ = tx.send((head, oid, changed));
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < limit => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Asked::TooSlow;
            }
        }
    };
    if !status.success() {
        return Asked::NotRepo;
    }
    let Ok((head, oid, changed)) = rx.recv_timeout(Duration::from_millis(500)) else {
        return Asked::TooSlow;
    };
    let detached = head == "(detached)";
    let branch = if detached {
        oid.chars().take(7).collect()
    } else {
        head
    };
    if branch.is_empty() {
        return Asked::NotRepo;
    }
    Asked::Repo(Brief {
        branch,
        detached,
        changed,
    })
}

pub fn toplevel(cwd: &Path) -> Option<String> {
    git(cwd, &["rev-parse", "--show-toplevel"])
        .ok()
        .filter(|s| !s.is_empty())
}
