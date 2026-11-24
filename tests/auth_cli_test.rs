mod common;

use assert_cmd::assert::OutputAssertExt;
use predicates::prelude::*;

#[test]
fn test_login_help_succeeds() {
    let mut cmd = common::treeherder_cli();
    cmd.arg("login").arg("--help").env("CLAUDECODE", "1");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--level").and(predicate::str::contains("--days")));
}

#[test]
fn test_status_without_credentials_reports_not_logged_in() {
    let config_dir = tempfile::tempdir().unwrap();
    let mut cmd = common::treeherder_cli();
    cmd.arg("status")
        .env("XDG_CONFIG_HOME", config_dir.path())
        .env_remove("TASKCLUSTER_CLIENT_ID")
        .env_remove("TASKCLUSTER_ACCESS_TOKEN")
        .env_remove("TASKCLUSTER_CERTIFICATE");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Not logged in"));
}
