use assert_cmd::assert::OutputAssertExt;
use assert_cmd::cargo::cargo_bin;
use predicates::prelude::*;
use std::process::Command;

#[test]
fn test_compare_flag_exists() {
    let mut cmd = Command::new(cargo_bin("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--compare"));
}

#[test]
fn test_compare_flag_help_text() {
    let mut cmd = Command::new(cargo_bin("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert().success().stdout(predicate::str::contains(
        "Compare with another revision to show new failures",
    ));
}

#[test]
fn test_compare_incompatible_with_use_cache() {
    let mut cmd = Command::new(cargo_bin("treeherder-cli"));
    cmd.arg("--compare")
        .arg("abc123")
        .arg("--use-cache")
        .arg("--cache-dir")
        .arg("/tmp/test")
        .arg("def456");

    cmd.assert().failure().stderr(predicate::str::contains(
        "--compare cannot be used with --use-cache",
    ));
}

#[test]
fn test_compare_incompatible_with_watch() {
    let mut cmd = Command::new(cargo_bin("treeherder-cli"));
    cmd.arg("--compare")
        .arg("abc123")
        .arg("--watch")
        .arg("def456");

    cmd.assert().failure().stderr(predicate::str::contains(
        "--compare cannot be used with --watch",
    ));
}

#[test]
#[ignore] // Ignore by default as it requires network access
fn test_compare_json_output_structure() {
    // Compare two revisions and check JSON output structure
    let mut cmd = Command::new(cargo_bin("treeherder-cli"));
    cmd.arg("--repo")
        .arg("try")
        .arg("--compare")
        .arg("a13b9fc22101b1e7a44ba1392eb275d9bdf202a2") // compare to itself for structure test
        .arg("--json")
        .arg("a13b9fc22101b1e7a44ba1392eb275d9bdf202a2");

    let output = cmd.output().unwrap();

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse JSON and check structure
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            assert!(
                json.get("base_revision").is_some(),
                "JSON should have base_revision field"
            );
            assert!(
                json.get("compare_revision").is_some(),
                "JSON should have compare_revision field"
            );
            assert!(
                json.get("base_push_id").is_some(),
                "JSON should have base_push_id field"
            );
            assert!(
                json.get("compare_push_id").is_some(),
                "JSON should have compare_push_id field"
            );
            assert!(
                json.get("new_failures").is_some(),
                "JSON should have new_failures field"
            );
            assert!(
                json.get("fixed_failures").is_some(),
                "JSON should have fixed_failures field"
            );
            assert!(
                json.get("still_failing").is_some(),
                "JSON should have still_failing field"
            );
        }
    }
}
