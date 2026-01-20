use assert_cmd::cargo::cargo_bin;
use std::process::Command;

#[test]
fn test_similar_history_flag_exists() {
    let mut cmd = Command::new(cargo_bin("treeherder-cli"));
    cmd.arg("--help");
    let output = cmd.output().expect("Failed to execute command");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--similar-history"));
}

#[test]
fn test_similar_history_help_text() {
    let mut cmd = Command::new(cargo_bin("treeherder-cli"));
    cmd.arg("--help");
    let output = cmd.output().expect("Failed to execute command");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("similar_jobs API"));
}

#[test]
fn test_similar_count_flag_exists() {
    let mut cmd = Command::new(cargo_bin("treeherder-cli"));
    cmd.arg("--help");
    let output = cmd.output().expect("Failed to execute command");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--similar-count"));
}

#[test]
fn test_similar_count_help_text() {
    let mut cmd = Command::new(cargo_bin("treeherder-cli"));
    cmd.arg("--help");
    let output = cmd.output().expect("Failed to execute command");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("similar jobs to fetch"));
}
