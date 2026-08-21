//! Command-line binary end-to-end integration tests.

use std::process::Command;

#[test]
fn test_cli_help_and_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_fr-cli"))
        .arg("--help")
        .output()
        .expect("failed to execute cli");

    let full_out = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(full_out.contains("fast-routing (Freerouting in Rust)"));
    assert!(full_out.contains("USAGE:"));
}
