use assert_cmd::assert::OutputAssertExt;
use assert_cmd::cargo::cargo_bin;
use predicates::prelude::*;
use std::process::Command;

#[test]
fn test_perf_flag_exists() {
    let mut cmd = Command::new(cargo_bin("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--perf"));
}

#[test]
fn test_perf_help_text() {
    let mut cmd = Command::new(cargo_bin("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert().success().stdout(predicate::str::contains(
        "Show performance/resource usage data",
    ));
}
