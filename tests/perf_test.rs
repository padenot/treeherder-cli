mod common;

use assert_cmd::assert::OutputAssertExt;
use predicates::prelude::*;

#[test]
fn test_perf_flag_exists() {
    let mut cmd = common::treeherder_cli();
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--perf"));
}

#[test]
fn test_perf_help_text() {
    let mut cmd = common::treeherder_cli();
    cmd.arg("--help");

    cmd.assert().success().stdout(predicate::str::contains(
        "Show performance/resource usage data",
    ));
}
