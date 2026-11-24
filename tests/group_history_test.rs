mod common;

use assert_cmd::assert::OutputAssertExt;
use predicates::prelude::*;

#[test]
fn test_history_flags_exist() {
    common::treeherder_cli()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--group-history"))
        .stdout(predicate::str::contains("--test"));
}

#[test]
fn test_group_history_requires_input() {
    common::treeherder_cli()
        .args([
            "--group-history",
            "dom/tests/xpcshell.toml",
            "--repo",
            "autoland",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--group-history requires INPUT"));
}

#[test]
fn test_group_history_caps_lookback() {
    common::treeherder_cli()
        .args([
            "abc123",
            "--repo",
            "autoland",
            "--group-history",
            "dom/tests/xpcshell.toml",
            "--lookback",
            "301",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("capped at 300"));
}

#[test]
fn test_group_history_rejects_range() {
    common::treeherder_cli()
        .args([
            "abc123",
            "--group-history",
            "dom/tests/xpcshell.toml",
            "--range",
            "a..b",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not --range/--from/--to"));
}

#[test]
fn test_test_flag_requires_suspects() {
    common::treeherder_cli()
        .args(["abc123", "--lookback", "5", "--test", "test_foo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--test requires --suspects"));
}
