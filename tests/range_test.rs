use assert_cmd::assert::OutputAssertExt;
use predicates::prelude::*;
use std::process::Command;

#[test]
fn test_range_flags_exist() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--range"))
        .stdout(predicate::str::contains("--from"))
        .stdout(predicate::str::contains("--to"))
        .stdout(predicate::str::contains("--lookback"))
        .stdout(predicate::str::contains("--suspects"));
}

#[test]
fn test_range_rejects_open_range() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--range")
        .arg("abc123..")
        .arg("--repo")
        .arg("autoland");

    cmd.assert().failure().stderr(predicate::str::contains(
        "range must include both start and end",
    ));
}

#[test]
fn test_from_and_to_must_be_used_together() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--from")
        .arg("abc123")
        .arg("--repo")
        .arg("autoland");

    cmd.assert().failure().stderr(predicate::str::contains(
        "--from and --to must be used together",
    ));
}

#[test]
fn test_range_incompatible_with_watch() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--range")
        .arg("abc123..def456")
        .arg("--watch")
        .arg("--repo")
        .arg("autoland");

    cmd.assert().failure().stderr(predicate::str::contains(
        "range analysis cannot be used with --watch",
    ));
}
