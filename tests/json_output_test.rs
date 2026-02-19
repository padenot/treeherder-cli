use assert_cmd::assert::OutputAssertExt;
use predicates::prelude::*;
use std::process::Command;

#[test]
fn test_json_flag_exists() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

#[test]
#[ignore] // Ignore by default as it requires network access
fn test_json_output_structure() {
    // This test verifies that JSON output contains expected fields
    // Run with: cargo test -- --ignored
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--repo")
        .arg("mozilla-central")
        .arg("--json")
        .arg("--match-filter")
        .arg("all")
        .arg("d4c62df049fd"); // A stable old revision

    let output = cmd.output().unwrap();

    // Check if output is valid JSON
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Try to parse as JSON
        match serde_json::from_str::<serde_json::Value>(&stdout) {
            Ok(json) => {
                // Verify expected fields exist
                assert!(
                    json.get("revision").is_some(),
                    "JSON should have revision field"
                );
                assert!(
                    json.get("push_id").is_some(),
                    "JSON should have push_id field"
                );
                assert!(json.get("jobs").is_some(), "JSON should have jobs field");

                // Verify jobs is an array
                assert!(json["jobs"].is_array(), "jobs should be an array");
            }
            Err(e) => {
                panic!("Output is not valid JSON: {}\nOutput: {}", e, stdout);
            }
        }
    }
}
