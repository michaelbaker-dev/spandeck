//! `spandeckd`: the program the Windows log-on task runs. It keeps the
//! station running.
//!
//! `spandeck.exe` is a console program, and Windows opens a console window
//! for one that is started from the Task Scheduler. This one is built with
//! no console and starts `spandeck daemon` beside it with no window.
//!
//! It supervises the station, not the process it started: Windows cannot
//! swap a running program in place, so after an upgrade the daemon is a
//! process this one never started (DECISIONS.md, "Windows", 15 Nov 2026).
//! It follows whichever daemon the station's pid file names, waits for it to
//! end, and then:
//!
//!   another daemon has taken over (an upgrade, a restart in place, going
//!   back to the previous binary)            -> follow that one
//!   it was asked to stop (exit code 0 and the clean-stop mark)
//!                                           -> end; nothing is started
//!   anything else (a crash, a kill)         -> start a daemon, after 1 s,
//!                                              then 2, 4, ... up to 5 minutes
//!
//! One per station: a second copy ends at once and starts nothing, unless
//! it was started to take over from the one that is running (after an
//! update, by the daemon), which then leaves.
//!
//! On macOS and Linux it is not installed: systemd and launchd run
//! `spandeck daemon` themselves, restart it, and follow it through an
//! upgrade because it stays one process. There this program only starts the
//! daemon and passes on its exit code. (The attribute below is one of the
//! two places outside `sys` that name a platform.)
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use spandeck_core::paths::Paths;
use spandeck_terminal::sys;

/// A daemon that ran this long was in service: the next failure waits one
/// second again. The same ten minutes the daemon counts its own unclean
/// starts over (`restore.rs`), which holds agents back at the third.
const STEADY: Duration = Duration::from_secs(600);
/// The longest wait between two starts.
const LONGEST_WAIT: Duration = Duration::from_secs(300);
/// After a daemon ends with success and no clean-stop mark, how long the
/// one that takes over is waited for. The new daemon writes the pid file
/// only once it has waited for the old one; a daemon from before
/// 15 Nov 2026 does not write it on the new one's behalf.
const HANDOVER: Duration = Duration::from_secs(30);
/// How long the supervisor that is running is given to leave.
const TAKE_OVER: Duration = Duration::from_secs(15);

fn main() {
    let beside = |name: &str| {
        std::env::current_exe()
            .ok()
            .map(|me| me.with_file_name(name))
            .filter(|p| p.is_file())
    };
    let Some(spandeck) = beside("spandeck.exe").or_else(|| beside("spandeck")) else {
        // No console to say so in: the exit code is all there is.
        std::process::exit(127);
    };
    let take_over = std::env::var_os("SPANDECKD_TAKE_OVER").is_some();
    std::env::remove_var("SPANDECKD_TAKE_OVER");
    if !sys::WINDOWS {
        let code = match daemon(&spandeck).status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(_) => 126,
        };
        std::process::exit(code);
    }
    let paths = Paths::from_env();
    let wait = if take_over { TAKE_OVER } else { Duration::ZERO };
    let Some(claim) = sys::supervise(&paths.socket(), wait) else {
        // This station has its supervisor.
        std::process::exit(0);
    };
    let log = Log::open(&paths);
    log.say(&format!(
        "supervising (pid {}, spandeck {}{})",
        std::process::id(),
        spandeck_core::VERSION,
        if take_over { ", took over" } else { "" }
    ));
    let code = supervise(&claim, &paths, &spandeck, &log);
    std::process::exit(code);
}

/// `spandeck daemon` with the arguments this program was given, no window.
fn daemon(spandeck: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(spandeck);
    cmd.arg("daemon").args(std::env::args_os().skip(1));
    sys::no_window(&mut cmd);
    cmd
}

/// The wait before the start that follows this many failures in a row.
fn backoff(failures: u32) -> Duration {
    let secs = 1u64 << failures.saturating_sub(1).min(16);
    Duration::from_secs(secs).min(LONGEST_WAIT)
}

/// The daemon of this station that is running now, if any.
///
/// A pid alone proves nothing: Windows gives a pid out again. The pid file
/// is believed when the process with that pid was started before the file
/// was written, which only the daemon that wrote it, or the daemon it was
/// written for, can be. Otherwise the station is asked on its pipe, which
/// only a daemon of this state folder answers.
/// The pid the pid file names, running or not.
fn named(paths: &Paths) -> Option<u32> {
    std::fs::read_to_string(paths.pid_file())
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

fn current(paths: &Paths) -> Option<u32> {
    let file = paths.pid_file();
    let named = named(paths);
    let written = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
    if let (Some(pid), Some(written)) = (named, written) {
        if sys::process_started(pid).is_some_and(|started| started <= written) {
            return Some(pid);
        }
    }
    let token = std::fs::read_to_string(paths.token_file()).ok()?;
    let mut station = spandeck_core::client::Client::connect(&paths.socket(), token.trim()).ok()?;
    station.set_timeout(Some(Duration::from_secs(3)));
    let info = station.call("station.info", serde_json::json!({})).ok()?;
    let pid = u32::try_from(info["pid"].as_u64()?).ok()?;
    sys::process_started(pid).map(|_| pid)
}

/// Keep the station running until it is stopped on purpose or another
/// supervisor takes over. Returns this program's exit code.
fn supervise(claim: &sys::Supervisor, paths: &Paths, spandeck: &Path, log: &Log) -> i32 {
    // Daemons that ended without being asked to, one after another.
    let mut failures = 0u32;
    loop {
        let mut pid = current(paths);
        if pid.is_none() && failures > 0 {
            let wait = backoff(failures);
            log.say(&format!("starting it again in {} s", wait.as_secs()));
            if claim.leave_within(wait) {
                log.say("another supervisor takes over");
                return 0;
            }
            pid = current(paths);
        }
        // Kept until the wait is over, so its pid stays its own.
        let mut started = None;
        let pid = match pid {
            Some(pid) => {
                log.say(&format!("following the daemon that is running (pid {pid})"));
                pid
            }
            None => match daemon(spandeck).spawn() {
                Ok(child) => {
                    let pid = child.id();
                    log.say(&format!("started a daemon (pid {pid})"));
                    started = Some(child);
                    pid
                }
                Err(e) => {
                    log.say(&format!("could not start {}: {e}", spandeck.display()));
                    failures += 1;
                    continue;
                }
            },
        };
        let since = Instant::now();
        let ended = claim.watch(pid);
        drop(started);
        let code = match ended {
            sys::Watched::Leave => {
                log.say("another supervisor takes over");
                return 0;
            }
            sys::Watched::Ended(code) => Some(code),
            sys::Watched::Gone => None,
        };
        log.say(&format!(
            "the daemon (pid {pid}) ended{}",
            code.map(|c| format!(" with code {c}")).unwrap_or_default()
        ));
        // An upgrade, a restart in place, going back: the next daemon is
        // already there, named in the pid file by the one that started it.
        if current(paths).is_some() {
            continue;
        }
        if code == Some(0) {
            // Success is a stop on request or a hand-over, nothing else.
            if spandeck_core::restore::stopped_clean(paths) == Some(true) {
                log.say("it was stopped on purpose: not started again");
                return 0;
            }
            let deadline = Instant::now() + HANDOVER;
            let mut next = None;
            // The daemon that took over may itself have ended before this
            // one's end was seen here (found on a slow machine, 16 Nov 2026:
            // thirty seconds were then spent waiting for a daemon that had
            // come and gone). The pid file names it; it is not waited for.
            let mut came_and_went = None;
            while next.is_none() && came_and_went.is_none() && Instant::now() < deadline {
                if claim.leave_within(Duration::from_millis(250)) {
                    log.say("another supervisor takes over");
                    return 0;
                }
                next = current(paths);
                if next.is_none() {
                    came_and_went = named(paths)
                        .filter(|&after| after != pid && sys::process_started(after).is_none());
                }
            }
            if next.is_some() {
                continue;
            }
            if let Some(after) = came_and_went {
                if spandeck_core::restore::stopped_clean(paths) == Some(true) {
                    log.say(&format!(
                        "the daemon that took over (pid {after}) was stopped on purpose: not started again"
                    ));
                    return 0;
                }
                log.say(&format!(
                    "the daemon that took over (pid {after}) has ended too"
                ));
                failures += 1;
                continue;
            }
            // Started again only when the station says it was not stopped.
            if spandeck_core::restore::stopped_clean(paths) != Some(false) {
                log.say("it ended with success and nothing took over: not started again");
                return 0;
            }
            log.say("it handed over to a daemon that never came up");
        }
        if since.elapsed() >= STEADY {
            failures = 0;
        }
        failures += 1;
    }
}

/// What the supervisor did and why, for the owner: it has no console.
/// `spandeckd-supervisor.log` in the state folder, started afresh when it
/// has grown past 256 KB.
struct Log(PathBuf);

impl Log {
    fn open(paths: &Paths) -> Log {
        let _ = std::fs::create_dir_all(&paths.state_dir);
        let file = paths.state_dir.join("spandeckd-supervisor.log");
        if std::fs::metadata(&file).is_ok_and(|m| m.len() > 256 * 1024) {
            let _ = std::fs::remove_file(&file);
        }
        Log(file)
    }

    fn say(&self, what: &str) {
        // Seconds since 1970, then the time of day for a reader.
        let now = spandeck_core::paths::now_ms() / 1000;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
        {
            let _ = writeln!(
                f,
                "{now} {:02}:{:02}:{:02} UTC  {what}",
                (now / 3600) % 24,
                (now / 60) % 60,
                now % 60
            );
        }
    }
}
