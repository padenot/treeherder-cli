use assert_cmd::assert::OutputAssertExt;
use predicates::prelude::*;
use std::process::Command;

#[test]
fn test_platform_flag_exists() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--platform"));
}

#[test]
fn test_platform_flag_help_text() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--help");

    cmd.assert().success().stdout(predicate::str::contains(
        "Only show jobs matching this platform regex pattern",
    ));
}

#[test]
#[ignore] // Ignore by default as it requires network access
fn test_platform_filter_linux() {
    // Filter to only show linux jobs
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--repo")
        .arg("try")
        .arg("--platform")
        .arg("linux")
        .arg("--match-filter")
        .arg("all")
        .arg("--json")
        .arg("a13b9fc22101b1e7a44ba1392eb275d9bdf202a2");

    let output = cmd.output().unwrap();

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse JSON and check platforms
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(jobs) = json["jobs"].as_array() {
                for job in jobs {
                    let platform = job["job"]["platform"].as_str().unwrap_or("");
                    assert!(
                        platform.contains("linux"),
                        "Platform '{}' should contain 'linux'",
                        platform
                    );
                }
            }
        }
    }
}

#[test]
#[ignore] // Ignore by default as it requires network access
fn test_platform_filter_windows() {
    // Filter to only show windows jobs
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--repo")
        .arg("try")
        .arg("--platform")
        .arg("windows")
        .arg("--match-filter")
        .arg("all")
        .arg("--json")
        .arg("a13b9fc22101b1e7a44ba1392eb275d9bdf202a2");

    let output = cmd.output().unwrap();

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse JSON and check platforms
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(jobs) = json["jobs"].as_array() {
                for job in jobs {
                    let platform = job["job"]["platform"].as_str().unwrap_or("");
                    assert!(
                        platform.contains("windows"),
                        "Platform '{}' should contain 'windows'",
                        platform
                    );
                }
            }
        }
    }
}

#[test]
#[ignore] // Ignore by default as it requires network access
fn test_platform_filter_regex() {
    // Test regex pattern (64-bit platforms)
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--repo")
        .arg("try")
        .arg("--platform")
        .arg("64")
        .arg("--match-filter")
        .arg("all")
        .arg("--json")
        .arg("a13b9fc22101b1e7a44ba1392eb275d9bdf202a2");

    let output = cmd.output().unwrap();

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse JSON and check platforms
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(jobs) = json["jobs"].as_array() {
                for job in jobs {
                    let platform = job["job"]["platform"].as_str().unwrap_or("");
                    assert!(
                        platform.contains("64"),
                        "Platform '{}' should contain '64'",
                        platform
                    );
                }
            }
        }
    }
}

#[test]
fn test_platform_filter_invalid_regex() {
    // Test that invalid regex is reported as an error
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    cmd.arg("--repo")
        .arg("try")
        .arg("--platform")
        .arg("[invalid") // Invalid regex
        .arg("a13b9fc22101");

    cmd.assert().failure();
}
