//! Evidence of which test groups (manifests) a job actually ran.
//!
//! Under reduced per-push scheduling a job only runs a subset of its suite's
//! manifests, so a successful job says nothing about tests outside that
//! subset. The errorsummary log lists the groups the harness set out to run
//! (`test_groups`) and how each finished (`group_result`); that is the
//! evidence used to tell "ran and passed" from "did not run".

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupEvidence {
    /// Groups listed by `test_groups` lines.
    #[serde(default)]
    pub scheduled: BTreeSet<String>,
    /// Groups with a `group_result` line; true when the group finished OK.
    #[serde(default)]
    pub results: BTreeMap<String, bool>,
}

/// How a group fared in one job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupRun {
    Ok,
    Failed,
    /// Scheduled but no result was logged (the job stopped early, for instance).
    NoResult,
    NotRun,
}

impl GroupEvidence {
    /// Reads `test_groups` and `group_result` lines of an errorsummary log.
    /// Returns None when the log has no group lines at all, which says nothing
    /// about what ran.
    pub fn parse(text: &str) -> Option<Self> {
        let mut evidence = GroupEvidence::default();
        let mut seen = false;
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            match value.get("action").and_then(|a| a.as_str()) {
                Some("test_groups") => {
                    seen = true;
                    let groups = value.get("groups").and_then(|g| g.as_array());
                    for group in groups.into_iter().flatten().filter_map(|g| g.as_str()) {
                        evidence.scheduled.insert(group.to_string());
                    }
                }
                Some("group_result") => {
                    seen = true;
                    let (Some(group), Some(status)) = (
                        value.get("group").and_then(|g| g.as_str()),
                        value.get("status").and_then(|s| s.as_str()),
                    ) else {
                        continue;
                    };
                    if status == "SKIP" {
                        continue;
                    }
                    let ok = status == "OK";
                    evidence
                        .results
                        .entry(group.to_string())
                        .and_modify(|prev| *prev &= ok)
                        .or_insert(ok);
                }
                _ => {}
            }
        }
        seen.then_some(evidence)
    }

    pub fn merge(&mut self, other: GroupEvidence) {
        self.scheduled.extend(other.scheduled);
        for (group, ok) in other.results {
            self.results
                .entry(group)
                .and_modify(|prev| *prev &= ok)
                .or_insert(ok);
        }
    }

    /// Combined outcome of the groups matching `manifest`.
    pub fn run_of(&self, manifest: &str) -> GroupRun {
        let mut outcome = None;
        for (_, ok) in self
            .results
            .iter()
            .filter(|(group, _)| group.as_str() == manifest)
        {
            outcome = Some(outcome.unwrap_or(true) && *ok);
        }
        match outcome {
            Some(true) => GroupRun::Ok,
            Some(false) => GroupRun::Failed,
            None if self.scheduled.iter().any(|group| group == manifest) => GroupRun::NoResult,
            None => GroupRun::NotRun,
        }
    }

    pub fn groups(&self) -> impl Iterator<Item = &String> {
        self.scheduled
            .iter()
            .chain(self.results.keys().filter(|g| !self.scheduled.contains(*g)))
    }
}

fn normalize(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    path.strip_prefix("./").unwrap_or(&path).to_string()
}

/// True when `test` lives under the directory `group` covers: the manifest's
/// directory, or the group itself for directory groups such as WPT paths.
pub fn group_contains_test(group: &str, test: &str) -> bool {
    let test = normalize(test);
    normalize(group).split(':').any(|part| {
        let dir = if is_manifest_file(part) {
            part.rsplit_once('/').map_or("", |(dir, _)| dir)
        } else {
            part
        };
        !dir.is_empty() && test.starts_with(dir) && test[dir.len()..].starts_with('/')
    })
}

fn is_manifest_file(path: &str) -> bool {
    [".toml", ".ini", ".list", ".json", ".yml", ".yaml"]
        .iter()
        .any(|ext| path.ends_with(ext))
}

/// True for test-verify and test-coverage jobs, which run the tests a push
/// touches rather than whole manifests.
pub fn runs_selected_tests(job_type_name: &str) -> bool {
    ["-test-verify", "-test-coverage"]
        .iter()
        .any(|kind| job_type_name.contains(kind))
}

/// Rough suite filter used to avoid downloading errorsummaries of jobs that
/// cannot run `manifest`. Unknown manifest kinds match every test job, and
/// WPT verification and coverage jobs match WPT groups.
pub fn job_may_run_group(job_type_name: &str, manifest: &str) -> bool {
    if !job_type_name.starts_with("test-") {
        return false;
    }
    let manifest = normalize(manifest);
    // test-verify and test-coverage jobs run tests of any suite, but only
    // their WPT variants upload an errorsummary to read groups from.
    if runs_selected_tests(job_type_name) {
        return manifest.starts_with('/') && job_type_name.contains("-wpt");
    }
    let file = manifest
        .rsplit(['/', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or("");
    let suites: &[&str] = if manifest.starts_with('/') {
        &["web-platform-tests"]
    } else if file.contains("xpcshell") {
        &["xpcshell"]
    } else if file.ends_with(".list") {
        &["reftest", "crashtest"]
    } else if ["mochitest", "browser", "chrome", "a11y"]
        .iter()
        .any(|prefix| file.starts_with(prefix))
    {
        &["mochitest"]
    } else {
        return true;
    };
    suites.iter().any(|suite| job_type_name.contains(suite))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMMARY: &str = r#"{"groups": ["xpcshell-selftest"], "action": "test_groups", "line": 51}
{"group": "xpcshell-selftest", "status": "OK", "duration": 1, "action": "group_result", "line": 197}
{"groups": ["a/xpcshell.toml", "b/xpcshell.toml", "c/xpcshell-remote.toml:c/xpcshell-common.toml"], "action": "test_groups", "line": 207}
{"test": "a/test_x.js", "group": "a/xpcshell.toml", "status": "FAIL", "expected": "PASS", "action": "test_result", "line": 300}
{"group": "a/xpcshell.toml", "status": "ERROR", "duration": 5, "action": "group_result", "line": 400}
{"group": "b/xpcshell.toml", "status": "OK", "duration": 5, "action": "group_result", "line": 401}
{"group": "xpcshell-selftest", "status": "OK", "duration": 1, "action": "group_result", "line": 402}
"#;

    #[test]
    fn parses_groups_and_results() {
        let evidence = GroupEvidence::parse(SUMMARY).unwrap();
        assert_eq!(evidence.run_of("a/xpcshell.toml"), GroupRun::Failed);
        assert_eq!(evidence.run_of("b/xpcshell.toml"), GroupRun::Ok);
        assert_eq!(
            evidence.run_of("c/xpcshell-remote.toml:c/xpcshell-common.toml"),
            GroupRun::NoResult
        );
        assert_eq!(evidence.run_of("c/xpcshell-common.toml"), GroupRun::NotRun);
        assert_eq!(evidence.run_of("d/xpcshell.toml"), GroupRun::NotRun);
        assert_eq!(evidence.run_of("xpcshell-selftest"), GroupRun::Ok);
    }

    #[test]
    fn log_without_group_lines_is_no_evidence() {
        assert_eq!(GroupEvidence::parse(""), None);
        assert_eq!(
            GroupEvidence::parse(r#"{"action": "log", "message": "hi", "line": 1}"#),
            None
        );
    }

    #[test]
    fn maps_tests_to_groups() {
        assert!(group_contains_test("a/b/xpcshell.toml", "a/b/test_x.js"));
        assert!(group_contains_test("a\\b\\xpcshell.toml", "a/b/test_x.js"));
        assert!(!group_contains_test("a/b/xpcshell.toml", "a/bc/test_x.js"));
        assert!(!group_contains_test("a/b/xpcshell.toml", "a/test_x.js"));
        assert!(group_contains_test("/css/foo", "/css/foo/bar.html"));
        assert!(!group_contains_test("xpcshell-selftest", "test_x.js"));
    }

    #[test]
    fn picks_jobs_by_suite() {
        let xpc = "devtools/shared/heapsnapshot/tests/xpcshell/xpcshell.toml";
        assert!(job_may_run_group("test-linux2404-64/opt-xpcshell", xpc));
        assert!(job_may_run_group(
            "test-android-em-14-x86_64/opt-geckoview-xpcshell-2",
            xpc
        ));
        assert!(!job_may_run_group(
            "test-linux2404-64/opt-mochitest-plain-1",
            xpc
        ));
        assert!(!job_may_run_group("build-linux64/opt", xpc));
        assert!(job_may_run_group(
            "test-linux2404-64/opt-mochitest-browser-chrome-1",
            "browser/base/content/test/general/browser.toml"
        ));
        assert!(job_may_run_group(
            "test-linux2404-64/opt-web-platform-tests-3",
            "/css/css-grid"
        ));
        assert!(job_may_run_group(
            "test-linux2404-64/opt-marionette",
            "testing/m.toml"
        ));
    }

    #[test]
    fn picks_verify_and_coverage_jobs() {
        let xpc = "devtools/shared/heapsnapshot/tests/xpcshell/xpcshell.toml";
        let wpt = "/css/css-grid";
        for job in [
            "test-linux2404-64/opt-test-verify-wpt-1",
            "test-linux2404-64-tsan/opt-test-verify-wpt-1",
            "test-linux2404-64/opt-test-coverage-wpt-1",
        ] {
            assert!(job_may_run_group(job, wpt), "{job}");
            assert!(!job_may_run_group(job, xpc), "{job}");
        }
        // Other verification jobs upload no errorsummary, so reading them
        // would only turn pushes into unknown.
        for job in [
            "test-linux2404-64/opt-test-verify",
            "test-macosx1500-aarch64/opt-test-verify-2",
            "test-windows11-64-25h2/debug-test-coverage",
        ] {
            assert!(!job_may_run_group(job, xpc), "{job}");
            assert!(!job_may_run_group(job, wpt), "{job}");
        }
    }
}
