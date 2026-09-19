//! Drives the binary as a subprocess against `--version` and `--schema`,
//! the two questions `shep adopt` asks before this dog opens a socket or a
//! file.
//!
//! A test that called `shep_client::dogs::probe` directly would pass
//! against a `main` that never calls it. Spawning the built binary is what
//! pins the order: `shep adopt` reads one line and kills the process
//! group, so a dog that has already started connecting is answering late.
//!
//! Both invocations point `SHEP_HOME` at a fresh temporary directory. There
//! is a real shepherd on this machine, under the developer's own
//! `$SHEP_HOME`, and this test must never reach it, whether or not `probe`
//! itself reads the variable today.

use std::process::Command;

use tempfile::TempDir;

#[test]
fn the_binary_answers_the_version_flag_before_it_opens_anything() {
    let home = TempDir::new().expect("temp dir");
    let output = Command::new(env!("CARGO_BIN_EXE_shep-discord"))
        .arg("--version")
        .env("SHEP_HOME", home.path())
        .output()
        .expect("spawned");

    assert!(
        output.status.success(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(lines[0].starts_with("shep-discord "), "{stdout}");
    assert!(lines[1].starts_with("shep-protocol:"), "{stdout}");
}

#[test]
fn the_binary_answers_the_schema_flag_with_parseable_json_naming_the_secret() {
    let home = TempDir::new().expect("temp dir");
    let output = Command::new(env!("CARGO_BIN_EXE_shep-discord"))
        .arg("--schema")
        .env("SHEP_HOME", home.path())
        .output()
        .expect("spawned");

    assert!(
        output.status.success(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let schema: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON schema");
    assert_eq!(
        schema["properties"]["token"]["x-shep-secret"], true,
        "{schema}"
    );
}
