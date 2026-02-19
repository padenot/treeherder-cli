use assert_cmd::assert::OutputAssertExt;
use predicates::prelude::*;
use std::process::Command;

#[test]
fn test_group_by_flag_exists() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--group-by"));
}

#[test]
fn test_group_by_test_value() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert().success().stdout(predicate::str::contains(
        "Group failures by test name across platforms",
    ));
}

#[test]
#[ignore] // Ignore by default as it requires network access
fn test_group_by_json_output_structure() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--repo")
        .arg("try")
        .arg("--group-by")
        .arg("test")
        .arg("--json")
        .arg("a13b9fc22101b1e7a44ba1392eb275d9bdf202a2");

    let output = cmd.output().unwrap();

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse JSON and check structure
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            assert!(
                json.get("revision").is_some(),
                "JSON should have revision field"
            );
            assert!(
                json.get("push_id").is_some(),
                "JSON should have push_id field"
            );
            assert!(
                json.get("grouped_failures").is_some(),
                "JSON should have grouped_failures field"
            );
            assert!(
                json["grouped_failures"].is_array(),
                "grouped_failures should be an array"
            );
        }
    }
}

#[test]
fn test_duration_min_flag_exists() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--duration-min"));
}

#[test]
fn test_duration_min_help_text() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert().success().stdout(predicate::str::contains(
        "Only show jobs that took longer than N seconds",
    ));
}

#[test]
#[ignore] // Ignore by default as it requires network access
fn test_duration_min_filters_short_jobs() {
    // Filter to show only jobs that took more than 60 seconds
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--repo")
        .arg("try")
        .arg("--duration-min")
        .arg("60")
        .arg("--match-filter")
        .arg("all")
        .arg("--json")
        .arg("a13b9fc22101b1e7a44ba1392eb275d9bdf202a2");

    let output = cmd.output().unwrap();

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse JSON and check durations
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(jobs) = json["jobs"].as_array() {
                for job in jobs {
                    if let Some(duration) = job["job"]["duration"].as_u64() {
                        assert!(
                            duration >= 60,
                            "Job duration {} should be >= 60 seconds",
                            duration
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[ignore] // Ignore by default as it requires network access
fn test_duration_min_high_value_returns_fewer_jobs() {
    // Get all jobs first
    let mut cmd_all = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd_all
        .arg("--repo")
        .arg("try")
        .arg("--match-filter")
        .arg("all")
        .arg("--json")
        .arg("a13b9fc22101b1e7a44ba1392eb275d9bdf202a2");

    let output_all = cmd_all.output().unwrap();

    // Get jobs with duration filter
    let mut cmd_filtered = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd_filtered
        .arg("--repo")
        .arg("try")
        .arg("--duration-min")
        .arg("300")
        .arg("--match-filter")
        .arg("all")
        .arg("--json")
        .arg("a13b9fc22101b1e7a44ba1392eb275d9bdf202a2");

    let output_filtered = cmd_filtered.output().unwrap();

    if output_all.status.success() && output_filtered.status.success() {
        let stdout_all = String::from_utf8_lossy(&output_all.stdout);
        let stdout_filtered = String::from_utf8_lossy(&output_filtered.stdout);

        if let (Ok(json_all), Ok(json_filtered)) = (
            serde_json::from_str::<serde_json::Value>(&stdout_all),
            serde_json::from_str::<serde_json::Value>(&stdout_filtered),
        ) {
            let count_all = json_all["jobs"].as_array().map(|a| a.len()).unwrap_or(0);
            let count_filtered = json_filtered["jobs"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);

            // The filtered count should be less than or equal to the total
            assert!(
                count_filtered <= count_all,
                "Filtered count ({}) should be <= total count ({})",
                count_filtered,
                count_all
            );
        }
    }
}
