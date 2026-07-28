use crate::models::*;
use anyhow::Result;
use std::collections::{BTreeSet, HashMap};

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
) -> RangeAnalysisResult {
    let mut pushes: Vec<_> = push_jobs.iter().map(|p| p.push.clone()).collect();
    pushes.sort_by_key(|p| p.id);

    let push_jobs_by_id: HashMap<_, _> = push_jobs.iter().map(|p| (p.push.id, p)).collect();
    let failure_keys_by_job = build_failure_key_index(observations);
    let mut all_keys: BTreeSet<FailureKey> = BTreeSet::new();
    for keys in failure_keys_by_job.values() {
        all_keys.extend(keys.iter().cloned());
    }

    let mut suspects = Vec::new();
    for key in all_keys {
        let timeline: Vec<_> = pushes
            .iter()
            .map(|push| {
                let push_jobs = push_jobs_by_id.get(&push.id).copied();
                classify_push_for_key(push, push_jobs, &key, &failure_keys_by_job)
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

fn classify_push_for_key(
    push: &PushRef,
    push_jobs: Option<&PushJobs>,
    key: &FailureKey,
    failure_keys_by_job: &HashMap<(u64, u64), BTreeSet<FailureKey>>,
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

    if let Some(job) = matching_jobs.iter().find(|job| job.result == "success") {
        return push_observation(push, FailureState::Pass, Some(job.id));
    }

    if let Some(job) = matching_jobs
        .iter()
        .find(|job| is_failure_result(&job.result))
    {
        return push_observation(push, FailureState::OtherFail, Some(job.id));
    }

    if let Some(job) = matching_jobs
        .iter()
        .find(|job| job.state != "completed" || job.result == "unknown")
    {
        return push_observation(push, FailureState::Pending, Some(job.id));
    }

    push_observation(push, FailureState::NotRun, None)
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

        let analysis = analyze_range_suspects(&fixture.repo, &fixture.pushes, &observations);
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
        let analysis = analyze_range_suspects(&fixture.repo, &fixture.pushes, &observations);

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
