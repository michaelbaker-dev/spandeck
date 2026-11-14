//! The pane host, for this crate's own tests. The one that ships is
//! `spandeck-pane-host` in the `spandeck` package; both are `host::main`.

fn main() {
    std::process::exit(spandeck_terminal::host::main());
}
