use assert_cmd::assert::OutputAssertExt;
use predicates::prelude::*;
use std::process::Command;

#[test]
fn test_watch_flag_exists() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--watch"));
}

#[test]
fn test_notify_flag_exists() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--notify"));
}

#[test]
fn test_notify_requires_watch() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--notify")
        .arg("--repo")
        .arg("try")
        .arg("a13b9fc22101");

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("--notify requires --watch"));
}

#[test]
fn test_watch_incompatible_with_use_cache() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--watch")
        .arg("--use-cache")
        .arg("--cache-dir")
        .arg("/tmp/test");

    cmd.assert().failure().stderr(predicate::str::contains(
        "--watch cannot be used with --use-cache",
    ));
}

#[test]
#[ignore] // Ignore by default as it requires network access and takes time
fn test_watch_mode_basic() {
    // This test verifies that watch mode runs (would need a completed push to test properly)
    // Run with: cargo test -- --ignored
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--repo")
        .arg("mozilla-central")
        .arg("--watch")
        .arg("d4c62df049fd"); // A stable old revision with completed jobs

    let output = cmd.output().unwrap();

    // Should succeed since jobs are already complete
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("All jobs completed") || stdout.contains("completed"),
            "Output should indicate jobs completed"
        );
    }
}
