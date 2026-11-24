mod common;

use assert_cmd::assert::OutputAssertExt;
use predicates::prelude::*;

#[test]
fn test_download_artifacts_flag_exists() {
    let mut cmd = common::treeherder_cli();
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--download-artifacts"));
}

#[test]
fn test_download_artifacts_help_text() {
    let mut cmd = common::treeherder_cli();
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Download job artifacts"));
}

#[test]
fn test_artifact_pattern_flag_exists() {
    let mut cmd = common::treeherder_cli();
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--artifact-pattern"));
}

#[test]
fn test_artifact_pattern_help_text() {
    let mut cmd = common::treeherder_cli();
    cmd.arg("--help");

    cmd.assert().success().stdout(predicate::str::contains(
        "Regex pattern to filter artifacts",
    ));
}
