//! Guarantee (Upgrades): live handoff is a release acceptance test.

use std::process::Command;

#[test]
fn a_running_terminal_survives_exec_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let out = Command::new(env!("CARGO_BIN_EXE_handoff-probe"))
        .args(["run", state.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && stdout.contains("HANDOFF_OK"), "stdout: {stdout}\nstderr: {stderr}");
}
