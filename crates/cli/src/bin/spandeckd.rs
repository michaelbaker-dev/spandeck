//! `spandeckd`: the program the Windows log-on task runs.
//!
//! `spandeck.exe` is a console program, and Windows opens a console window
//! for one that is started from the Task Scheduler. This one is built with
//! no console, starts `spandeck daemon` beside it with no window, waits, and
//! passes on its exit code, so the task's restart-on-failure still works.
//! On macOS and Linux it is not installed: systemd and launchd run
//! `spandeck daemon` themselves. (DECISIONS.md, "Windows". The attribute
//! below is one of the two places outside `sys` that name a platform.)
#![cfg_attr(windows, windows_subsystem = "windows")]

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
    let mut cmd = std::process::Command::new(spandeck);
    cmd.arg("daemon").args(std::env::args_os().skip(1));
    spandeck_terminal::sys::no_window(&mut cmd);
    let code = match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 126,
    };
    std::process::exit(code);
}
