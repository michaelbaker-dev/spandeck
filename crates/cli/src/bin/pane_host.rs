//! `spandeck-pane-host`: the process that owns a station's terminals so
//! they outlive the daemon (DECISIONS.md, "Windows"). Started by spandeckd,
//! never by hand. It is its own small program, not a mode of `spandeck`,
//! because it keeps running across upgrades: it must not hold the installed
//! `spandeck` open, and there is little in it to go wrong.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    std::process::exit(spandeck_terminal::host::main());
}
