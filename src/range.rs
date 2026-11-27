use crate::groups::{group_contains_test, runs_selected_tests, GroupEvidence, GroupRun};
use crate::models::*;
use anyhow::Result;
use regex::Regex;
use std::collections::{BTreeSet, HashMap, HashSet};

pub fn parse_revision_range(input: &str) -> Result<(String, String)> {
    let separator = if input.contains("...") { "..." } else { ".." };
    let (start, end) = input
        .split_once(separator)
        .ok_or_else(|| anyhow::anyhow!("range must use START..END or START...END"))?;

    let start = start.trim();
    let end = end.trim();
    if start.is_empty() || end.is_empty() {
        anyhow::bail!("range must include both start and end revisions");
    }

    Ok((start.to_string(), end.to_string()))
}

pub fn analyze_range_suspects(
    repo: &str,
    push_jobs: &[PushJobs],
    observations: &[JobObservation],
    group_evidence: &HashMap<u64, GroupEvidence>,
) -> RangeAnalysisResult {
    let mut pushes: Vec<_> = push_jobs.iter().map(|p| p.push.clone()).collect();
    pushes.sort_by_key(|p| p.id);

    let push_jobs_by_id: HashMap<_, _> = push_jobs.iter().map(|p| (p.push.id, p)).collect();
    let failure_keys_by_job = build_failure_key_index(observations);
    let groups_by_key = failure_groups_index(observations, &failure_keys_by_job, group_evidence);
    let mut all_keys: BTreeSet<FailureKey> = BTreeSet::new();
    for keys in failure_keys_by_job.values() {
        all_keys.extend(keys.iter().cloned());
    }

    let empty = BTreeSet::new();
    let mut suspects = Vec::new();
    for key in all_keys {
        let evidence = key.test.as_ref().map(|_| TestEvidence {
            groups: groups_by_key.get(&key).unwrap_or(&empty),
            jobs: group_evidence,
        });
        let timeline: Vec<_> = pushes
            .iter()
            .map(|push| {
                let push_jobs = push_jobs_by_id.get(&push.id).copied();
                classify_push_for_key(
                    push,
                    push_jobs,
                    &key,
                    &failure_keys_by_job,
                    evidence.as_ref(),
                )
            })
            .collect();

        let Some(first_failed_idx) = timeline
            .iter()
            .position(|entry| entry.state == FailureState::Fail)
        else {
            continue;
        };

        let last_pass_idx = timeline[..first_failed_idx]
            .iter()
            .rposition(|entry| entry.state == FailureState::Pass);

        let candidate_start = last_pass_idx.map_or(0, |idx| idx + 1);
        let candidate_pushes = timeline[candidate_start..=first_failed_idx].to_vec();
        let confidence = confidence_for(last_pass_idx, &candidate_pushes);

        suspects.push(SuspectRange {
            failure_key: key,
            first_failed: timeline[first_failed_idx].push.clone(),
            last_pass: last_pass_idx.map(|idx| timeline[idx].push.clone()),
            candidate_pushes,
            confidence,
        });
    }

    suspects.sort_by_key(|s| {
        (
            s.first_failed.id,
            s.failure_key.job_type_name.clone(),
            s.failure_key.platform.clone(),
            s.failure_key.test.clone(),
            s.failure_key.subtest.clone(),
            s.failure_key.signature.clone(),
        )
    });

    RangeAnalysisResult {
        repo: repo.to_string(),
        pushes,
        suspects,
    }
}

pub fn group_suspects(suspects: &[SuspectRange]) -> Vec<SuspectWindow> {
    let mut windows: Vec<SuspectWindow> = Vec::new();
    for suspect in suspects {
        let candidates: Vec<_> = suspect
            .candidate_pushes
            .iter()
            .map(|c| (c.push.id, c.state))
            .collect();
        let existing = windows.iter_mut().find(|window| {
            window.first_failed.id == suspect.first_failed.id
                && window.last_pass.as_ref().map(|p| p.id)
                    == suspect.last_pass.as_ref().map(|p| p.id)
                && window
                    .candidate_pushes
                    .iter()
                    .map(|c| (c.push.id, c.state))
                    .eq(candidates.iter().copied())
        });
        match existing {
            Some(window) => window.failure_keys.push(suspect.failure_key.clone()),
            None => windows.push(SuspectWindow {
                failure_keys: vec![suspect.failure_key.clone()],
                first_failed: suspect.first_failed.clone(),
                last_pass: suspect.last_pass.clone(),
                predates_window: suspect.last_pass.is_none(),
                candidate_pushes: suspect.candidate_pushes.clone(),
                confidence: suspect.confidence,
            }),
        }
    }
    windows
}

fn build_failure_key_index(
    observations: &[JobObservation],
) -> HashMap<(u64, u64), BTreeSet<FailureKey>> {
    let mut index = HashMap::new();
    for observation in observations {
        let keys = failure_keys_for(&observation.job, &observation.errors);
        if !keys.is_empty() {
            index.insert((observation.push.id, observation.job.id), keys);
        }
    }
    index
}

fn failure_keys_for(job: &Job, errors: &[ErrorLine]) -> BTreeSet<FailureKey> {
    let mut keys = BTreeSet::new();

    for error in errors {
        if error.test.is_none() && error.signature.is_none() {
            continue;
        }
        keys.insert(FailureKey {
            job_type_name: job.job_type_name.clone(),
            platform: job.platform.clone(),
            test: error.test.clone(),
            subtest: error.subtest.clone(),
            signature: error.signature.clone(),
        });
    }

    let tests_with_subtests: BTreeSet<_> = keys
        .iter()
        .filter(|key| key.subtest.is_some())
        .filter_map(|key| key.test.clone())
        .collect();
    if !tests_with_subtests.is_empty() {
        keys.retain(|key| {
            !(key.subtest.is_none()
                && key.signature.is_none()
                && key
                    .test
                    .as_ref()
                    .is_some_and(|test| tests_with_subtests.contains(test)))
        });
    }

    if keys.is_empty() && is_failure_result(&job.result) {
        keys.insert(FailureKey {
            job_type_name: job.job_type_name.clone(),
            platform: job.platform.clone(),
            test: None,
            subtest: None,
            signature: None,
        });
    }

    keys
}

/// What tells whether a job ran a failing test: the manifests it failed under
/// and the group evidence of each job.
struct TestEvidence<'a> {
    groups: &'a BTreeSet<String>,
    jobs: &'a HashMap<u64, GroupEvidence>,
}

/// Manifests under which each failure key, so one job type, platform and
/// test, failed at the first push where it failed. A pass only counts when
/// one of these ran OK: the same test included through another parent
/// manifest runs in a different configuration.
fn failure_groups_index(
    observations: &[JobObservation],
    failure_keys_by_job: &HashMap<(u64, u64), BTreeSet<FailureKey>>,
    group_evidence: &HashMap<u64, GroupEvidence>,
) -> HashMap<FailureKey, BTreeSet<String>> {
    let mut first: HashMap<FailureKey, (u64, BTreeSet<String>)> = HashMap::new();
    for observation in observations {
        let Some(keys) = failure_keys_by_job.get(&(observation.push.id, observation.job.id)) else {
            continue;
        };
        for key in keys {
            let Some(test) = &key.test else {
                continue;
            };
            let groups = failing_groups(
                observation,
                key,
                test,
                group_evidence.get(&observation.job.id),
            );
            let push_id = observation.push.id;
            match first.get_mut(key) {
                Some((first_push, known)) if *first_push == push_id => known.extend(groups),
                Some((first_push, _)) if *first_push < push_id => {}
                _ => {
                    first.insert(key.clone(), (push_id, groups));
                }
            }
        }
    }
    first
        .into_iter()
        .map(|(key, (_, groups))| (key, groups))
        .collect()
}

/// Groups a failure happened under in one job: the `group` of the
/// errorsummary lines of that very failure (test, subtest and signature), or
/// of any line of its test, or else every group of the job whose directory
/// holds the test. A directory can hold several manifests, so the last form
/// lists candidates, narrowed to the ones that did not finish OK (failed or
/// never reported a result) when there are any.
fn failing_groups(
    observation: &JobObservation,
    key: &FailureKey,
    test: &str,
    evidence: Option<&GroupEvidence>,
) -> BTreeSet<String> {
    let groups_of = |matches: &dyn Fn(&ErrorLine) -> bool| -> BTreeSet<String> {
        observation
            .errors
            .iter()
            .filter(|error| matches(error))
            .filter_map(|error| error.group.clone())
            .collect()
    };
    let exact = groups_of(&|error| {
        error.test.as_deref() == Some(test)
            && error.subtest == key.subtest
            && error.signature == key.signature
    });
    if !exact.is_empty() {
        return exact;
    }
    let same_test = groups_of(&|error| error.test.as_deref() == Some(test));
    if !same_test.is_empty() {
        return same_test;
    }
    let Some(evidence) = evidence else {
        return BTreeSet::new();
    };
    let holding: BTreeSet<String> = evidence
        .groups()
        .filter(|group| group_contains_test(group, test))
        .cloned()
        .collect();
    let unfinished: BTreeSet<String> = holding
        .iter()
        .filter(|group| evidence.results.get(*group) != Some(&true))
        .cloned()
        .collect();
    if unfinished.is_empty() {
        holding
    } else {
        unfinished
    }
}

/// Completed jobs whose group evidence decides whether a push passed a failing
/// test: jobs of the failing job type on pushes before the last first failure
/// of any test in it, other than the failed jobs already observed.
pub fn jobs_needing_group_evidence(
    push_jobs: &[PushJobs],
    observations: &[JobObservation],
    test_filter: Option<&Regex>,
) -> Vec<Job> {
    let failure_keys_by_job = build_failure_key_index(observations);
    let mut first_failure_by_key: HashMap<&FailureKey, u64> = HashMap::new();
    for observation in observations {
        let Some(keys) = failure_keys_by_job.get(&(observation.push.id, observation.job.id)) else {
            continue;
        };
        let test_keys = keys.iter().filter(|key| {
            key.test
                .as_deref()
                .is_some_and(|test| test_filter.is_none_or(|regex| regex.is_match(test)))
        });
        for key in test_keys {
            first_failure_by_key
                .entry(key)
                .and_modify(|first| *first = (*first).min(observation.push.id))
                .or_insert(observation.push.id);
        }
    }
    // Each test needs evidence up to its own first failure, so a job type is
    // read up to the latest of those.
    let mut first_failure: HashMap<(&str, &str), u64> = HashMap::new();
    for (key, first) in first_failure_by_key {
        first_failure
            .entry((&key.job_type_name, &key.platform))
            .and_modify(|cutoff| *cutoff = (*cutoff).max(first))
            .or_insert(first);
    }

    let observed: HashSet<u64> = observations.iter().map(|o| o.job.id).collect();
    push_jobs
        .iter()
        .flat_map(|push_jobs| {
            let first_failure = &first_failure;
            let observed = &observed;
            push_jobs.jobs.iter().filter(move |job| {
                first_failure
                    .get(&(job.job_type_name.as_str(), job.platform.as_str()))
                    .is_some_and(|first| push_jobs.push.id < *first)
                    && job.state == "completed"
                    && (job.result == "success" || is_failure_result(&job.result))
                    && !observed.contains(&job.id)
            })
        })
        .cloned()
        .collect()
}

fn classify_push_for_key(
    push: &PushRef,
    push_jobs: Option<&PushJobs>,
    key: &FailureKey,
    failure_keys_by_job: &HashMap<(u64, u64), BTreeSet<FailureKey>>,
    test_evidence: Option<&TestEvidence>,
) -> PushFailureObservation {
    let Some(push_jobs) = push_jobs else {
        return push_observation(push, FailureState::NotRun, None);
    };

    let matching_jobs: Vec<_> = push_jobs
        .jobs
        .iter()
        .filter(|job| job.job_type_name == key.job_type_name && job.platform == key.platform)
        .collect();

    if matching_jobs.is_empty() {
        return push_observation(push, FailureState::NotRun, None);
    }

    for job in &matching_jobs {
        if failure_keys_by_job
            .get(&(push.id, job.id))
            .is_some_and(|keys| keys.contains(key))
        {
            return push_observation(push, FailureState::Fail, Some(job.id));
        }
    }

    // A job that only ran some manifests says nothing about tests elsewhere,
    // so a test failure needs evidence that its manifest ran.
    if let Some(evidence) = test_evidence {
        return match matching_jobs
            .iter()
            .map(|job| (classify_job_for_test(job, evidence), job.id))
            .min_by_key(|(state, _)| state_rank(*state))
        {
            Some((FailureState::NotRun, _)) | None => {
                push_observation(push, FailureState::NotRun, None)
            }
            Some((state, job_id)) => push_observation(push, state, Some(job_id)),
        };
    }

    if let Some(job) = matching_jobs.iter().find(|job| job.result == "success") {
        return push_observation(push, FailureState::Pass, Some(job.id));
    }

    if let Some(job) = matching_jobs
        .iter()
        .find(|job| is_failure_result(&job.result))
    {
        return push_observation(push, FailureState::OtherFail, Some(job.id));
    }

    if let Some(job) = matching_jobs.iter().find(|job| is_pending(job)) {
        return push_observation(push, FailureState::Pending, Some(job.id));
    }

    push_observation(push, FailureState::NotRun, None)
}

/// State of one job, which did not report the failure, for a test failure.
fn classify_job_for_test(job: &Job, evidence: &TestEvidence) -> FailureState {
    if is_pending(job) {
        return FailureState::Pending;
    }
    if job.result != "success" && !is_failure_result(&job.result) {
        return FailureState::NotRun;
    }
    let Some(groups) = evidence.jobs.get(&job.id) else {
        return FailureState::Unknown;
    };
    // These jobs run selected tests of a manifest, so the manifest finishing
    // OK does not show that the failing test ran.
    let selects_tests = runs_selected_tests(&job.job_type_name);
    // The failure belongs to one of these groups, or to all of them, and it may
    // not be known which: only a push that ran every one OK passed the test.
    let runs: Vec<_> = evidence.groups.iter().map(|g| groups.run_of(g)).collect();
    if runs.is_empty() {
        FailureState::Unknown
    } else if runs.iter().all(|run| *run == GroupRun::Ok) {
        if selects_tests {
            FailureState::Unknown
        } else {
            FailureState::Pass
        }
    } else if runs.iter().all(|run| *run == GroupRun::NotRun) {
        FailureState::NotRun
    } else if runs
        .iter()
        .all(|run| matches!(run, GroupRun::Ok | GroupRun::Failed))
    {
        FailureState::OtherFail
    } else {
        FailureState::Unknown
    }
}

fn state_rank(state: FailureState) -> u8 {
    match state {
        FailureState::Fail => 0,
        FailureState::Pass => 1,
        FailureState::OtherFail => 2,
        FailureState::Pending => 3,
        FailureState::Unknown => 4,
        FailureState::NotRun => 5,
    }
}

fn is_pending(job: &Job) -> bool {
    job.state != "completed" || job.result == "unknown"
}

fn push_observation(
    push: &PushRef,
    state: FailureState,
    job_id: Option<u64>,
) -> PushFailureObservation {
    PushFailureObservation {
        push: push.clone(),
        state,
        job_id,
    }
}

fn confidence_for(
    last_pass_idx: Option<usize>,
    candidate_pushes: &[PushFailureObservation],
) -> SuspectConfidence {
    if last_pass_idx.is_none() {
        SuspectConfidence::Low
    } else if candidate_pushes.len() == 1 {
        SuspectConfidence::High
    } else {
        SuspectConfidence::Medium
    }
}

fn is_failure_result(result: &str) -> bool {
    result == "testfailed" || result == "busted"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct RangeFixture {
        repo: String,
        pushes: Vec<PushJobs>,
        observations: Vec<FixtureObservation>,
        expected: ExpectedSuspect,
    }

    #[derive(serde::Deserialize)]
    struct LargeRangeFixture {
        source: LargeFixtureSource,
        repo: String,
        pushes: Vec<PushJobs>,
        observations: Vec<FixtureObservation>,
    }

    #[derive(serde::Deserialize)]
    struct LargeFixtureSource {
        push_count: usize,
        retained_jobs: usize,
        observations: usize,
        errors: usize,
        non_intermittent_failed_jobs_seen: usize,
    }

    #[derive(serde::Deserialize)]
    struct FixtureObservation {
        push_id: u64,
        job_id: u64,
        errors: Vec<ErrorLine>,
    }

    #[derive(serde::Deserialize)]
    struct ExpectedSuspect {
        job_type_name: String,
        platform: String,
        test: Option<String>,
        subtest: Option<String>,
        first_failed_push_id: u64,
        last_pass_push_id: Option<u64>,
        candidate_push_ids: Vec<u64>,
        confidence: SuspectConfidence,
    }

    #[test]
    fn parses_double_dot_range() {
        let (start, end) = parse_revision_range("abc123..def456").unwrap();
        assert_eq!(start, "abc123");
        assert_eq!(end, "def456");
    }

    #[test]
    fn parses_triple_dot_range() {
        let (start, end) = parse_revision_range("abc123...def456").unwrap();
        assert_eq!(start, "abc123");
        assert_eq!(end, "def456");
    }

    #[test]
    fn rejects_open_range() {
        assert!(parse_revision_range("abc123..").is_err());
    }

    #[test]
    fn replays_sparse_ci_fixture() {
        let fixture: RangeFixture = serde_json::from_str(include_str!(
            "../tests/fixtures/autoland_sparse_ci_range.json"
        ))
        .unwrap();

        let observations = materialize_observations(&fixture.pushes, &fixture.observations);

        let analysis = analyze_range_suspects(
            &fixture.repo,
            &fixture.pushes,
            &observations,
            &HashMap::new(),
        );
        let expected_key = FailureKey {
            job_type_name: fixture.expected.job_type_name.clone(),
            platform: fixture.expected.platform.clone(),
            test: fixture.expected.test.clone(),
            subtest: fixture.expected.subtest.clone(),
            signature: None,
        };

        let suspect = analysis
            .suspects
            .iter()
            .find(|suspect| suspect.failure_key == expected_key)
            .unwrap();

        assert_eq!(
            suspect.first_failed.id,
            fixture.expected.first_failed_push_id
        );
        assert_eq!(
            suspect.last_pass.as_ref().map(|push| push.id),
            fixture.expected.last_pass_push_id
        );
        assert_eq!(
            suspect
                .candidate_pushes
                .iter()
                .map(|entry| entry.push.id)
                .collect::<Vec<_>>(),
            fixture.expected.candidate_push_ids
        );
        assert_eq!(suspect.confidence, fixture.expected.confidence);
    }

    #[test]
    fn groups_suspects_sharing_a_window() {
        let fixture: LargeRangeFixture = serde_json::from_str(include_str!(
            "../tests/fixtures/autoland_recent_failures_large.json"
        ))
        .unwrap();
        let observations = materialize_observations(&fixture.pushes, &fixture.observations);
        let analysis = analyze_range_suspects(
            &fixture.repo,
            &fixture.pushes,
            &observations,
            &HashMap::new(),
        );

        let windows = group_suspects(&analysis.suspects);
        assert!(windows.len() < analysis.suspects.len());
        assert_eq!(
            windows.iter().map(|w| w.failure_keys.len()).sum::<usize>(),
            analysis.suspects.len()
        );
        assert!(windows
            .iter()
            .all(|w| w.predates_window == w.last_pass.is_none()));
        assert!(windows.iter().any(|w| w.failure_keys.len() > 1));
    }

    #[test]
    fn replays_large_recent_autoland_fixture() {
        let fixture: LargeRangeFixture = serde_json::from_str(include_str!(
            "../tests/fixtures/autoland_recent_failures_large.json"
        ))
        .unwrap();

        assert_eq!(fixture.pushes.len(), fixture.source.push_count);
        assert_eq!(fixture.observations.len(), fixture.source.observations);
        assert_eq!(
            fixture.observations.len(),
            fixture.source.non_intermittent_failed_jobs_seen
        );
        assert_eq!(
            fixture
                .pushes
                .iter()
                .map(|push_jobs| push_jobs.jobs.len())
                .sum::<usize>(),
            fixture.source.retained_jobs
        );
        assert_eq!(
            fixture
                .observations
                .iter()
                .map(|observation| observation.errors.len())
                .sum::<usize>(),
            fixture.source.errors
        );

        let observations = materialize_observations(&fixture.pushes, &fixture.observations);
        let analysis = analyze_range_suspects(
            &fixture.repo,
            &fixture.pushes,
            &observations,
            &HashMap::new(),
        );

        assert_eq!(analysis.pushes.len(), fixture.source.push_count);
        assert!(
            analysis.suspects.len() >= 100,
            "expected a substantial suspect corpus, got {}",
            analysis.suspects.len()
        );
        assert!(
            analysis.suspects.iter().any(|suspect| suspect
                .candidate_pushes
                .iter()
                .any(|candidate| candidate.state == FailureState::NotRun)),
            "expected at least one sparse-CI suspect with a not-run candidate"
        );
        assert!(
            analysis
                .suspects
                .iter()
                .any(|suspect| suspect.confidence == SuspectConfidence::Medium),
            "expected at least one medium-confidence sparse suspect"
        );
        assert!(
            analysis.suspects.iter().all(|suspect| suspect
                .candidate_pushes
                .last()
                .is_some_and(|candidate| candidate.state == FailureState::Fail)),
            "every candidate window should end in the first observed failure"
        );
    }

    #[derive(serde::Deserialize)]
    struct ErrorsummaryFixture {
        repo: String,
        test: String,
        pushes: Vec<PushJobs>,
        errorsummaries: HashMap<u64, String>,
    }

    struct Replay {
        fixture: ErrorsummaryFixture,
        observations: Vec<JobObservation>,
        evidence: HashMap<u64, GroupEvidence>,
    }

    /// Replays recorded errorsummaries the way range mode reads them: failed
    /// jobs become observations, every readable log becomes group evidence.
    fn replay_takecensus() -> Replay {
        let fixture: ErrorsummaryFixture = serde_json::from_str(include_str!(
            "../tests/fixtures/autoland_takecensus_suspects.json"
        ))
        .unwrap();
        let mut observations = Vec::new();
        let mut evidence = HashMap::new();
        for push_jobs in &fixture.pushes {
            for job in &push_jobs.jobs {
                let Some(text) = fixture.errorsummaries.get(&job.id) else {
                    continue;
                };
                if is_failure_result(&job.result) {
                    observations.push(JobObservation {
                        push: push_jobs.push.clone(),
                        job: job.clone(),
                        errors: crate::api::parse_error_summary(text),
                    });
                }
                if let Some(groups) = GroupEvidence::parse(text) {
                    evidence.insert(job.id, groups);
                }
            }
        }
        Replay {
            fixture,
            observations,
            evidence,
        }
    }

    fn analyze(replay: &Replay) -> RangeAnalysisResult {
        analyze_range_suspects(
            &replay.fixture.repo,
            &replay.fixture.pushes,
            &replay.observations,
            &replay.evidence,
        )
    }

    fn suspect_for<'a>(analysis: &'a RangeAnalysisResult, job_type_name: &str) -> &'a SuspectRange {
        analysis
            .suspects
            .iter()
            .find(|s| {
                s.failure_key.job_type_name == job_type_name
                    && s.failure_key.subtest.as_deref() == Some("run_test")
            })
            .unwrap_or_else(|| panic!("no run_test suspect for {job_type_name}"))
    }

    fn state_at(suspect: &SuspectRange, push_id: u64) -> FailureState {
        suspect
            .candidate_pushes
            .iter()
            .find(|c| c.push.id == push_id)
            .unwrap_or_else(|| panic!("push {push_id} is not a candidate"))
            .state
    }

    #[test]
    fn success_without_the_manifest_is_not_a_pass() {
        let replay = replay_takecensus();
        let analysis = analyze(&replay);

        // Both jobs succeeded but ran other manifests; the test first ran at
        // the backstop push 2125431.
        for (job_type_name, no_op_push) in [
            ("test-macosx1015-64-qr/opt-xpcshell", 2125342),
            ("test-linux2404-64-asan/opt-xpcshell", 2125366),
        ] {
            let suspect = suspect_for(&analysis, job_type_name);
            assert!(suspect.failure_key.test.as_deref() == Some(replay.fixture.test.as_str()));
            assert_eq!(suspect.first_failed.id, 2125431, "{job_type_name}");
            assert_eq!(suspect.last_pass, None, "{job_type_name}");
            assert_eq!(state_at(suspect, no_op_push), FailureState::NotRun);
            assert_eq!(suspect.confidence, SuspectConfidence::Low);
            assert_eq!(suspect.candidate_pushes.first().unwrap().push.id, 2125340);
        }
    }

    #[test]
    fn backfill_that_ran_the_manifest_is_a_pass() {
        let replay = replay_takecensus();
        let analysis = analyze(&replay);

        let suspect = suspect_for(&analysis, "test-linux2404-64/opt-xpcshell");
        assert_eq!(suspect.first_failed.id, 2125342);
        assert_eq!(suspect.last_pass.as_ref().map(|p| p.id), Some(2125340));
        assert_eq!(suspect.confidence, SuspectConfidence::High);
    }

    #[test]
    fn every_last_pass_ran_the_manifest() {
        let replay = replay_takecensus();
        let analysis = analyze(&replay);
        let manifest = "devtools/shared/heapsnapshot/tests/xpcshell/xpcshell.toml";

        for suspect in analysis.suspects.iter().filter(|s| s.last_pass.is_some()) {
            let pass = suspect.candidate_pushes.first().map(|c| c.push.id).unwrap();
            let last_pass_id = suspect.last_pass.as_ref().unwrap().id;
            assert!(pass > last_pass_id);
            let push_jobs = replay
                .fixture
                .pushes
                .iter()
                .find(|p| p.push.id == last_pass_id)
                .unwrap();
            assert!(push_jobs.jobs.iter().any(|job| {
                job.job_type_name == suspect.failure_key.job_type_name
                    && replay.evidence.get(&job.id).map(|e| e.run_of(manifest))
                        == Some(crate::groups::GroupRun::Ok)
            }));
        }
    }

    #[test]
    fn success_without_group_evidence_is_unknown() {
        let mut replay = replay_takecensus();
        // Drop the errorsummary of the backfill that passed on 2125340.
        let backfill_pass = replay.fixture.pushes[0]
            .jobs
            .iter()
            .find(|job| job.job_type_name == "test-linux2404-64/opt-xpcshell")
            .unwrap()
            .id;
        replay.evidence.remove(&backfill_pass);
        let analysis = analyze(&replay);

        let suspect = suspect_for(&analysis, "test-linux2404-64/opt-xpcshell");
        assert_eq!(suspect.last_pass, None);
        assert_eq!(state_at(suspect, 2125340), FailureState::Unknown);
        assert_eq!(suspect.confidence, SuspectConfidence::Low);
    }

    #[test]
    fn fetches_evidence_up_to_each_tests_first_failure() {
        let job = |id: u64, result: &str| Job {
            id,
            job_type_name: "test-linux2404-64/opt-xpcshell".to_string(),
            job_type_symbol: "X".to_string(),
            platform: "linux2404-64".to_string(),
            platform_option: "opt".to_string(),
            result: result.to_string(),
            state: "completed".to_string(),
            failure_classification_id: Some(1),
            failure_classification: None,
            duration: Some(10),
            task_id: Some(format!("task{id}")),
        };
        let push = |id: u64| PushRef {
            id,
            revision: format!("{id:040x}"),
            push_timestamp: id,
        };
        let failure = |test: &str| -> ErrorLine {
            serde_json::from_value(serde_json::json!({
                "action": "test_result", "line": 1, "test": test,
                "status": "FAIL", "expected": "PASS",
            }))
            .unwrap()
        };
        // Test A first fails on push 10, test B on push 20, in the same job type.
        let push_jobs: Vec<_> = [
            (10, "testfailed"),
            (19, "success"),
            (20, "testfailed"),
            (21, "success"),
        ]
        .into_iter()
        .map(|(id, result)| PushJobs {
            push: push(id),
            jobs: vec![job(id, result)],
        })
        .collect();
        let observations = vec![
            JobObservation {
                push: push(10),
                job: job(10, "testfailed"),
                errors: vec![failure("a/test_a.js")],
            },
            JobObservation {
                push: push(20),
                job: job(20, "testfailed"),
                errors: vec![failure("b/test_b.js")],
            },
        ];

        let ids = |filter: Option<&Regex>| -> Vec<u64> {
            jobs_needing_group_evidence(&push_jobs, &observations, filter)
                .iter()
                .map(|job| job.id)
                .collect()
        };
        // Push 19 decides whether test B last passed right before failing.
        assert_eq!(ids(None), vec![19]);
        assert_eq!(ids(Some(&Regex::new("test_b").unwrap())), vec![19]);
        assert!(ids(Some(&Regex::new("test_a").unwrap())).is_empty());
    }

    const COMMON: &str = "t/common.toml";
    const REMOTE: &str = "t/remote.toml:t/common.toml";
    const LOCAL: &str = "t/xpcshell-local.toml:t/common.toml";

    fn scoped_job(id: u64, job_type_name: &str, result: &str) -> Job {
        Job {
            id,
            job_type_name: job_type_name.to_string(),
            job_type_symbol: "X".to_string(),
            platform: "linux2404-64".to_string(),
            platform_option: "opt".to_string(),
            result: result.to_string(),
            state: "completed".to_string(),
            failure_classification_id: Some(1),
            failure_classification: None,
            duration: Some(10),
            task_id: Some(format!("task{id}")),
        }
    }

    fn scoped_push(id: u64) -> PushRef {
        PushRef {
            id,
            revision: format!("{id:040x}"),
            push_timestamp: id,
        }
    }

    fn scoped_failure(group: Option<&str>) -> ErrorLine {
        serde_json::from_value(serde_json::json!({
            "action": "test_result", "line": 1, "test": "t/test_c.js",
            "group": group, "status": "FAIL", "expected": "PASS",
        }))
        .unwrap()
    }

    fn evidence(lines: &[(&str, &str)]) -> GroupEvidence {
        let groups: Vec<_> = lines.iter().map(|(group, _)| *group).collect();
        let mut text =
            serde_json::json!({"action": "test_groups", "groups": groups, "line": 1}).to_string();
        for (group, status) in lines {
            text.push('\n');
            text.push_str(
                &serde_json::json!({"action": "group_result", "group": group, "status": status, "line": 2})
                    .to_string(),
            );
        }
        GroupEvidence::parse(&text).unwrap()
    }

    fn scoped_last_pass(analysis: &RangeAnalysisResult, job_type_name: &str) -> Option<u64> {
        analysis
            .suspects
            .iter()
            .find(|s| s.failure_key.job_type_name == job_type_name)
            .unwrap()
            .last_pass
            .as_ref()
            .map(|p| p.id)
    }

    #[test]
    fn manifest_evidence_stays_with_the_failing_job_type() {
        let opt = "test-linux2404-64/opt-xpcshell";
        let other = "test-linux2404-64/opt-xpcshell-other";
        // Push 10 ran only the local configuration on `opt`; push 20 fails the
        // remote configuration on `opt` and the local one on `other`.
        let push_jobs = vec![
            PushJobs {
                push: scoped_push(10),
                jobs: vec![scoped_job(1, opt, "success")],
            },
            PushJobs {
                push: scoped_push(20),
                jobs: vec![
                    scoped_job(2, opt, "testfailed"),
                    scoped_job(3, other, "testfailed"),
                ],
            },
        ];
        let observations = vec![
            JobObservation {
                push: scoped_push(20),
                job: scoped_job(2, opt, "testfailed"),
                errors: vec![scoped_failure(Some(REMOTE))],
            },
            JobObservation {
                push: scoped_push(20),
                job: scoped_job(3, other, "testfailed"),
                errors: vec![scoped_failure(Some(LOCAL))],
            },
        ];
        let group_evidence = HashMap::from([(1, evidence(&[(LOCAL, "OK")]))]);

        let analysis =
            analyze_range_suspects("autoland", &push_jobs, &observations, &group_evidence);
        assert_eq!(scoped_last_pass(&analysis, opt), None);
    }

    #[test]
    fn fallback_uses_the_group_that_failed() {
        let opt = "test-linux2404-64/opt-xpcshell";
        let push_jobs = vec![
            PushJobs {
                push: scoped_push(10),
                jobs: vec![scoped_job(1, opt, "success")],
            },
            PushJobs {
                push: scoped_push(20),
                jobs: vec![scoped_job(2, opt, "testfailed")],
            },
        ];
        // The failure lines carry no group; the failing job's group results do.
        let observations = vec![JobObservation {
            push: scoped_push(20),
            job: scoped_job(2, opt, "testfailed"),
            errors: vec![scoped_failure(None)],
        }];
        let group_evidence = HashMap::from([
            (1, evidence(&[(LOCAL, "OK")])),
            (
                2,
                evidence(&[(REMOTE, "ERROR"), (LOCAL, "OK"), (COMMON, "OK")]),
            ),
        ]);

        let analysis =
            analyze_range_suspects("autoland", &push_jobs, &observations, &group_evidence);
        assert_eq!(scoped_last_pass(&analysis, opt), None);

        // Without a failed group the test may belong to either manifest of its
        // directory, so a push that ran only one of them proves nothing.
        let group_evidence = HashMap::from([
            (1, evidence(&[(LOCAL, "OK")])),
            (2, evidence(&[(REMOTE, "OK"), (LOCAL, "OK")])),
        ]);
        let analysis =
            analyze_range_suspects("autoland", &push_jobs, &observations, &group_evidence);
        assert_eq!(scoped_last_pass(&analysis, opt), None);
        let suspect = &analysis.suspects[0];
        assert_eq!(state_at(suspect, 10), FailureState::Unknown);

        // A push that ran every candidate manifest OK did run the test.
        let group_evidence = HashMap::from([
            (1, evidence(&[(LOCAL, "OK"), (REMOTE, "OK")])),
            (2, evidence(&[(REMOTE, "OK"), (LOCAL, "OK")])),
        ]);
        let analysis =
            analyze_range_suspects("autoland", &push_jobs, &observations, &group_evidence);
        assert_eq!(scoped_last_pass(&analysis, opt), Some(10));

        // And one that ran none of them did not.
        let group_evidence = HashMap::from([
            (1, evidence(&[("t/other.toml", "OK")])),
            (2, evidence(&[(REMOTE, "OK"), (LOCAL, "OK")])),
        ]);
        let analysis =
            analyze_range_suspects("autoland", &push_jobs, &observations, &group_evidence);
        assert_eq!(state_at(&analysis.suspects[0], 10), FailureState::NotRun);
    }

    fn failure_line(test: &str, subtest: Option<&str>, group: Option<&str>) -> ErrorLine {
        serde_json::from_value(serde_json::json!({
            "action": "test_result", "line": 1, "test": test, "subtest": subtest,
            "group": group, "status": "FAIL", "expected": "PASS",
        }))
        .unwrap()
    }

    fn two_push_analysis(
        job_type_name: &str,
        errors: Vec<ErrorLine>,
        group_evidence: HashMap<u64, GroupEvidence>,
    ) -> RangeAnalysisResult {
        let push_jobs = vec![
            PushJobs {
                push: scoped_push(10),
                jobs: vec![scoped_job(1, job_type_name, "success")],
            },
            PushJobs {
                push: scoped_push(20),
                jobs: vec![scoped_job(2, job_type_name, "testfailed")],
            },
        ];
        let observations = vec![JobObservation {
            push: scoped_push(20),
            job: scoped_job(2, job_type_name, "testfailed"),
            errors,
        }];
        analyze_range_suspects("autoland", &push_jobs, &observations, &group_evidence)
    }

    #[test]
    fn fallback_keeps_manifests_that_never_finished() {
        let opt = "test-linux2404-64/opt-xpcshell";
        let (plain, remote) = ("t/xpcshell.toml", "t/xpcshell-remote.toml");
        // The failing job crashed in xpcshell.toml (no group_result) while
        // xpcshell-remote.toml failed on its own; push 10 ran only the latter.
        let failing = GroupEvidence {
            scheduled: BTreeSet::from([plain.to_string(), remote.to_string()]),
            results: [(remote.to_string(), false)].into_iter().collect(),
        };
        let analysis = two_push_analysis(
            opt,
            vec![failure_line("t/test_c.js", None, None)],
            HashMap::from([(1, evidence(&[(remote, "OK")])), (2, failing)]),
        );
        assert_eq!(scoped_last_pass(&analysis, opt), None);
        assert_eq!(state_at(&analysis.suspects[0], 10), FailureState::Unknown);
    }

    #[test]
    fn groups_follow_the_failing_subtest() {
        let opt = "test-linux2404-64/opt-xpcshell";
        let analysis = two_push_analysis(
            opt,
            vec![
                failure_line("t/test_c.js", Some("first"), Some(REMOTE)),
                failure_line("t/test_c.js", Some("second"), Some(LOCAL)),
            ],
            HashMap::from([
                (1, evidence(&[(REMOTE, "OK")])),
                (2, evidence(&[(REMOTE, "ERROR"), (LOCAL, "ERROR")])),
            ]),
        );
        let last_pass = |subtest: &str| {
            analysis
                .suspects
                .iter()
                .find(|s| s.failure_key.subtest.as_deref() == Some(subtest))
                .unwrap()
                .last_pass
                .as_ref()
                .map(|p| p.id)
        };
        assert_eq!(last_pass("first"), Some(10));
        assert_eq!(last_pass("second"), None);
    }

    #[test]
    fn verify_jobs_cannot_prove_a_test_passed() {
        // test-verify runs selected tests, so its manifest can finish OK
        // without the failing test in it.
        let verify = "test-linux2404-64/opt-test-verify";
        let analysis = two_push_analysis(
            verify,
            vec![failure_line("t/test_c.js", None, Some(COMMON))],
            HashMap::from([
                (1, evidence(&[(COMMON, "OK")])),
                (2, evidence(&[(COMMON, "ERROR")])),
            ]),
        );
        assert_eq!(scoped_last_pass(&analysis, verify), None);
        assert_eq!(state_at(&analysis.suspects[0], 10), FailureState::Unknown);
    }

    fn materialize_observations(
        pushes: &[PushJobs],
        observations: &[FixtureObservation],
    ) -> Vec<JobObservation> {
        observations
            .iter()
            .map(|fixture_observation| {
                let push_jobs = pushes
                    .iter()
                    .find(|push_jobs| push_jobs.push.id == fixture_observation.push_id)
                    .unwrap();
                let job = push_jobs
                    .jobs
                    .iter()
                    .find(|job| job.id == fixture_observation.job_id)
                    .unwrap();

                JobObservation {
                    push: push_jobs.push.clone(),
                    job: job.clone(),
                    errors: fixture_observation.errors.clone(),
                }
            })
            .collect()
    }
}
