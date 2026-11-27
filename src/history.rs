use crate::api::GroupResults;
use crate::groups::{group_includes, job_may_run_group, GroupEvidence, GroupRun};
use crate::models::*;
use std::collections::{BTreeSet, HashMap, HashSet};

pub struct PushGroupResults {
    pub push: PushRef,
    pub results: GroupResults,
    /// Task ids of jobs selected by --filter/--platform; None counts every task.
    pub allowed_tasks: Option<HashSet<String>>,
    /// Group evidence read from the errorsummary of tasks without a result for
    /// the manifest in `results`; None when the task had no readable errorsummary.
    pub evidence: HashMap<String, Option<GroupEvidence>>,
}

impl PushGroupResults {
    /// True when --filter/--platform select the task, or no filter is set.
    pub fn allows(&self, task_id: &str) -> bool {
        self.allowed_tasks
            .as_ref()
            .is_none_or(|allowed| allowed.contains(task_id))
    }
}

/// Results older than the last pass needed before the history stops fetching.
pub const SETTLED_RESULTS_AFTER_PASS: usize = 1;
/// Pushes older than the last pass that settle the history even without a result,
/// so a manifest scheduled on few pushes does not drag the scan across the window.
pub const SETTLED_PUSHES_AFTER_PASS: usize = 10;

/// Jobs of a push whose errorsummary can tell whether they ran `manifest`:
/// test jobs of a suite that runs such manifests, selected by --filter, with
/// no result for `manifest` in push/group_results. The endpoint can be empty,
/// and it lists a task as soon as any of its groups has a result, so a task
/// being listed does not settle the manifest.
pub fn jobs_needing_evidence<'a>(
    manifest: &str,
    jobs: &'a [Job],
    results: &GroupResults,
    allowed_tasks: Option<&HashSet<String>>,
) -> Vec<&'a Job> {
    jobs.iter()
        .filter(|job| job.result != "retry")
        .filter(|job| job_may_run_group(&job.job_type_name, manifest))
        .filter(|job| {
            job.task_id.as_ref().is_some_and(|task_id| {
                !has_endpoint_result(results, task_id, manifest)
                    && allowed_tasks.is_none_or(|allowed| allowed.contains(task_id))
            })
        })
        .collect()
}

fn has_endpoint_result(results: &GroupResults, task_id: &str, manifest: &str) -> bool {
    results
        .get(task_id)
        .is_some_and(|groups| groups.contains_key(manifest))
}

pub fn build_group_history(
    manifest: &str,
    mut entries: Vec<GroupHistoryPush>,
    window_pushes: usize,
) -> GroupHistory {
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.push_id));

    let newest_fail = entries
        .iter()
        .position(|entry| entry.state == GroupState::Fail);
    let last_passed_idx = match newest_fail {
        Some(idx) => entries[idx..]
            .iter()
            .position(|entry| entry.state.counts_as_pass())
            .map(|offset| idx + offset),
        None => entries
            .iter()
            .position(|entry| entry.state.counts_as_pass()),
    };
    let first_failed = newest_fail.map(|idx| {
        let end = last_passed_idx.unwrap_or(entries.len());
        entries[idx..end]
            .iter()
            .rev()
            .find(|entry| entry.state == GroupState::Fail)
            .cloned()
            .unwrap()
    });
    let predates_window = entries
        .iter()
        .rev()
        .find(|entry| entry.state.has_result())
        .is_some_and(|entry| entry.state == GroupState::Fail);

    GroupHistory {
        manifest: manifest.to_string(),
        window_pushes,
        last_passed: last_passed_idx.map(|idx| entries[idx].clone()),
        first_failed,
        predates_window,
        pushes: entries,
        included_as: Vec::new(),
    }
}

/// Group names of the tasks selected by --filter/--platform on a push that
/// hold `manifest` as one side of `parent.toml:included.toml`.
pub fn included_group_names(manifest: &str, push: &PushGroupResults) -> BTreeSet<String> {
    let endpoint = push
        .results
        .iter()
        .filter(|(task_id, _)| push.allows(task_id))
        .flat_map(|(_, groups)| groups.keys());
    let errorsummaries = push
        .evidence
        .iter()
        .filter(|(task_id, _)| push.allows(task_id))
        .filter_map(|(_, evidence)| evidence.as_ref())
        .flat_map(|evidence| evidence.groups());
    endpoint
        .chain(errorsummaries)
        .filter(|group| group_includes(group, manifest))
        .cloned()
        .collect()
}

/// Lists `names` when no push has a result for the manifest itself, so a
/// manifest that only runs through parent manifests is not just "not-run".
pub fn suggest_included_names(history: &mut GroupHistory, names: BTreeSet<String>) {
    if !history.pushes.iter().any(|push| push.state.has_result()) {
        history.included_as = names.into_iter().collect();
    }
}

/// True once the newest-first entries contain a pass older than the newest failure
/// followed by enough further results that older pushes cannot change the answer.
pub fn is_settled<'a>(entries: impl IntoIterator<Item = &'a GroupHistoryPush>) -> bool {
    let mut sorted: Vec<_> = entries.into_iter().collect();
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.push_id));
    let newest_fail = sorted
        .iter()
        .position(|entry| entry.state == GroupState::Fail)
        .unwrap_or(0);
    let Some(pass_idx) = sorted[newest_fail..]
        .iter()
        .position(|entry| entry.state.counts_as_pass())
        .map(|offset| newest_fail + offset)
    else {
        return false;
    };
    let older = &sorted[pass_idx + 1..];
    older.len() >= SETTLED_PUSHES_AFTER_PASS
        || older
            .iter()
            .filter(|entry| entry.state.has_result())
            .count()
            >= SETTLED_RESULTS_AFTER_PASS
}

pub fn summarize_push(manifest: &str, push: PushGroupResults) -> GroupHistoryPush {
    let (mut ok, mut fail, mut unknown) = (0, 0, 0);
    for (_, groups) in push
        .results
        .iter()
        .filter(|(task_id, _)| push.allows(task_id))
    {
        match groups.get(manifest) {
            Some(true) => ok += 1,
            Some(false) => fail += 1,
            None => {}
        }
    }
    for (task_id, evidence) in push
        .evidence
        .iter()
        .filter(|(task_id, _)| push.allows(task_id))
    {
        if has_endpoint_result(&push.results, task_id, manifest) {
            continue;
        }
        match evidence.as_ref().map(|evidence| evidence.run_of(manifest)) {
            Some(GroupRun::Ok) => ok += 1,
            Some(GroupRun::Failed) => fail += 1,
            Some(GroupRun::NotRun) => {}
            Some(GroupRun::NoResult) | None => unknown += 1,
        }
    }
    let state = match GroupState::from_counts(ok, fail) {
        GroupState::NotRun if unknown > 0 => GroupState::Unknown,
        state => state,
    };
    GroupHistoryPush {
        push_id: push.push.id,
        revision: push.push.revision,
        timestamp: push.push.push_timestamp,
        ok,
        fail,
        state,
    }
}

impl GroupState {
    /// A push fails when at least as many tasks fail as pass.
    pub fn from_counts(ok: usize, fail: usize) -> Self {
        match (ok, fail) {
            (0, 0) => GroupState::NotRun,
            (_, 0) => GroupState::Pass,
            (ok, fail) if fail >= ok => GroupState::Fail,
            _ => GroupState::Mixed,
        }
    }

    pub fn counts_as_pass(self) -> bool {
        matches!(self, GroupState::Pass | GroupState::Mixed)
    }

    /// True when some task reported a result for the manifest.
    pub fn has_result(self) -> bool {
        !matches!(self, GroupState::NotRun | GroupState::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[derive(serde::Deserialize)]
    struct Fixture {
        manifest: String,
        pushes: Vec<FixturePush>,
        expected: Expected,
    }

    #[derive(serde::Deserialize)]
    struct FixturePush {
        push: PushRef,
        jobs: Vec<Job>,
        group_results: GroupResults,
    }

    #[derive(serde::Deserialize)]
    struct Expected {
        last_passed_push_id: u64,
        first_failed_push_id: u64,
        predates_window: bool,
        filtered: FilteredExpected,
    }

    #[derive(serde::Deserialize)]
    struct FilteredExpected {
        filter: String,
        last_passed_push_id: u64,
        first_failed_push_id: u64,
    }

    fn load_fixture() -> Fixture {
        serde_json::from_str(include_str!(
            "../tests/fixtures/autoland_group_history.json"
        ))
        .unwrap()
    }

    fn inputs(fixture: &Fixture, filter: Option<&Regex>) -> Vec<GroupHistoryPush> {
        fixture
            .pushes
            .iter()
            .map(|push| PushGroupResults {
                push: push.push.clone(),
                results: push.group_results.clone(),
                allowed_tasks: filter.map(|regex| {
                    push.jobs
                        .iter()
                        .filter(|job| regex.is_match(&job.job_type_name))
                        .filter_map(|job| job.task_id.clone())
                        .collect()
                }),
                evidence: HashMap::new(),
            })
            .map(|push| summarize_push(&fixture.manifest, push))
            .collect()
    }

    #[test]
    fn replays_manifest_history_fixture() {
        let fixture = load_fixture();
        let entries = inputs(&fixture, None);
        let history = build_group_history(&fixture.manifest, entries.clone(), entries.len());

        let ids: Vec<_> = history.pushes.iter().map(|p| p.push_id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(ids, sorted, "pushes are newest first");

        assert_eq!(
            history.last_passed.as_ref().map(|p| p.push_id),
            Some(fixture.expected.last_passed_push_id)
        );
        assert_eq!(
            history.first_failed.as_ref().map(|p| p.push_id),
            Some(fixture.expected.first_failed_push_id)
        );
        assert_eq!(history.predates_window, fixture.expected.predates_window);
        assert!(history
            .pushes
            .iter()
            .any(|p| p.state == GroupState::NotRun && p.ok == 0 && p.fail == 0));
    }

    #[test]
    fn joins_tasks_to_filtered_jobs() {
        let fixture = load_fixture();
        let regex = Regex::new(&fixture.expected.filtered.filter).unwrap();
        let entries = inputs(&fixture, Some(&regex));
        let history = build_group_history(&fixture.manifest, entries.clone(), entries.len());

        assert_eq!(
            history.last_passed.as_ref().map(|p| p.push_id),
            Some(fixture.expected.filtered.last_passed_push_id)
        );
        let first_failed = history.first_failed.unwrap();
        assert_eq!(
            first_failed.push_id,
            fixture.expected.filtered.first_failed_push_id
        );
        assert_eq!((first_failed.ok, first_failed.fail), (0, 1));
    }

    #[test]
    fn failure_predates_window_when_oldest_result_fails() {
        let fixture = load_fixture();
        let failing_only: Vec<_> = inputs(&fixture, None)
            .into_iter()
            .filter(|push| push.push_id >= fixture.expected.first_failed_push_id)
            .collect();
        let history = build_group_history(&fixture.manifest, failing_only.clone(), 20);

        assert!(!is_settled(&failing_only));
        assert_eq!(history.window_pushes, 20);
        assert!(history.predates_window);
        assert!(history.last_passed.is_none());
        assert_eq!(
            history.first_failed.map(|p| p.push_id),
            Some(fixture.expected.first_failed_push_id)
        );
    }

    #[test]
    fn settles_after_a_pass_and_older_results() {
        let fixture = load_fixture();
        let mut entries = inputs(&fixture, None);
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.push_id));
        let pass_idx = entries
            .iter()
            .position(|entry| entry.push_id == fixture.expected.last_passed_push_id)
            .unwrap();

        assert!(!is_settled(&entries[..pass_idx + 1]));
        assert!(!is_settled(
            &entries[..pass_idx + SETTLED_RESULTS_AFTER_PASS]
        ));
        assert!(is_settled(
            &entries[..pass_idx + 1 + SETTLED_RESULTS_AFTER_PASS]
        ));
        assert!(is_settled(&entries));
    }

    #[test]
    fn settles_after_a_pass_and_enough_not_run_pushes() {
        let entry = |push_id: u64, state| GroupHistoryPush {
            push_id,
            revision: format!("{push_id:x}"),
            timestamp: push_id,
            ok: 0,
            fail: 0,
            state,
        };
        let mut entries = vec![entry(100, GroupState::Fail), entry(99, GroupState::Pass)];
        let not_run = SETTLED_PUSHES_AFTER_PASS as u64 - 1;
        entries.extend((0..not_run).map(|i| entry(98 - i, GroupState::NotRun)));

        assert!(!is_settled(&entries));
        entries.push(entry(50, GroupState::NotRun));
        assert!(is_settled(&entries));
    }

    #[derive(serde::Deserialize)]
    struct ErrorsummaryFixture {
        manifest: String,
        pushes: Vec<ErrorsummaryPush>,
    }

    #[derive(serde::Deserialize)]
    struct ErrorsummaryPush {
        push: PushRef,
        group_results: GroupResults,
        jobs: Vec<Job>,
        errorsummaries: HashMap<u64, String>,
    }

    /// Replays a push the way --group-history reads it: group_results first,
    /// then the errorsummary of each job that may run the manifest.
    fn replay_errorsummary_push(
        manifest: &str,
        push: &ErrorsummaryPush,
        unreadable: &HashSet<u64>,
    ) -> GroupHistoryPush {
        let evidence = jobs_needing_evidence(manifest, &push.jobs, &push.group_results, None)
            .into_iter()
            .map(|job| {
                let evidence = push
                    .errorsummaries
                    .get(&job.id)
                    .filter(|_| !unreadable.contains(&job.id))
                    .and_then(|text| GroupEvidence::parse(text));
                (job.task_id.clone().unwrap(), evidence)
            })
            .collect();
        summarize_push(
            manifest,
            PushGroupResults {
                push: push.push.clone(),
                results: push.group_results.clone(),
                allowed_tasks: None,
                evidence,
            },
        )
    }

    fn load_errorsummary_fixture() -> ErrorsummaryFixture {
        serde_json::from_str(include_str!(
            "../tests/fixtures/autoland_group_history_errorsummary.json"
        ))
        .unwrap()
    }

    #[test]
    fn empty_group_results_fall_back_to_errorsummaries() {
        let fixture = load_errorsummary_fixture();
        assert!(fixture.pushes.iter().all(|p| p.group_results.is_empty()));
        let entries: Vec<_> = fixture
            .pushes
            .iter()
            .map(|push| replay_errorsummary_push(&fixture.manifest, push, &HashSet::new()))
            .collect();
        let by_id: HashMap<_, _> = entries.iter().map(|e| (e.push_id, e)).collect();

        // The backstop push ran the manifest on most xpcshell platforms.
        let backstop = by_id[&2125431];
        assert_eq!(backstop.state, GroupState::Fail);
        assert!(backstop.fail >= 20, "fail={}", backstop.fail);
        // Backfills (X-98faa66dfc9-bk) ran it on earlier pushes.
        for push_id in [2125342, 2125366] {
            let entry = by_id[&push_id];
            assert_eq!(
                (entry.ok, entry.fail, entry.state),
                (0, 1, GroupState::Fail)
            );
        }
        let before = by_id[&2125340];
        assert_eq!(
            (before.ok, before.fail, before.state),
            (1, 0, GroupState::Pass)
        );

        let history = build_group_history(&fixture.manifest, entries.clone(), entries.len());
        assert_eq!(history.last_passed.map(|p| p.push_id), Some(2125340));
        assert_eq!(history.first_failed.map(|p| p.push_id), Some(2125342));
        assert!(!history.predates_window);
    }

    #[test]
    fn group_results_without_the_manifest_fall_back_per_manifest() {
        let manifest = "dom/tests/xpcshell.toml";
        let job = |id: u64| -> Job {
            serde_json::from_value(serde_json::json!({
                "id": id, "job_type_name": "test-linux2404-64/opt-xpcshell",
                "job_type_symbol": "X", "platform": "linux2404-64", "platform_option": "opt",
                "result": "testfailed", "state": "completed", "failure_classification_id": 1,
                "task_id": format!("task{id}"),
            }))
            .unwrap()
        };
        // task1 reported only its self-test to group_results and stopped
        // before the manifest finished; task2 has an endpoint result for it.
        let group_results: GroupResults = serde_json::from_value(serde_json::json!({
            "task1": {"xpcshell-selftest": true},
            "task2": {manifest: false},
        }))
        .unwrap();
        let summary = |status: &str| {
            format!(
                "{}\n{}\n",
                serde_json::json!({"action": "test_groups", "groups": ["xpcshell-selftest", manifest], "line": 1}),
                serde_json::json!({"action": "group_result", "group": "xpcshell-selftest", "status": status, "line": 2}),
            )
        };
        let push = ErrorsummaryPush {
            push: PushRef {
                id: 1,
                revision: "abc".to_string(),
                push_timestamp: 1,
            },
            group_results,
            jobs: vec![job(1), job(2)],
            errorsummaries: HashMap::from([(1, summary("OK")), (2, summary("OK"))]),
        };

        let needing: Vec<_> =
            jobs_needing_evidence(manifest, &push.jobs, &push.group_results, None)
                .iter()
                .map(|job| job.id)
                .collect();
        assert_eq!(needing, vec![1]);

        // The endpoint result of task2 is kept and task1 adds nothing: it
        // scheduled the manifest but has no result for it.
        let entry = replay_errorsummary_push(manifest, &push, &HashSet::new());
        assert_eq!((entry.ok, entry.fail), (0, 1));

        let mut only_task1 = push;
        only_task1.group_results.remove("task2");
        only_task1.jobs.truncate(1);
        let entry = replay_errorsummary_push(manifest, &only_task1, &HashSet::new());
        assert_eq!(
            (entry.ok, entry.fail, entry.state),
            (0, 0, GroupState::Unknown)
        );
    }

    #[test]
    fn unreadable_errorsummary_is_unknown_not_not_run() {
        let fixture = load_errorsummary_fixture();
        let push = fixture
            .pushes
            .iter()
            .find(|p| p.push.id == 2125340)
            .unwrap();
        let unreadable: HashSet<_> = push.jobs.iter().map(|job| job.id).collect();
        let entry = replay_errorsummary_push(&fixture.manifest, push, &unreadable);
        assert_eq!((entry.ok, entry.fail), (0, 0));
        assert_eq!(entry.state, GroupState::Unknown);
    }

    #[test]
    fn suggests_names_of_manifests_run_through_a_parent() {
        let manifest = "toolkit/x/xpcshell-common.toml";
        let remote = "toolkit/x/xpcshell-remote.toml:toolkit/x/xpcshell-common.toml";
        let plain = "toolkit/x/xpcshell.toml:toolkit/x/xpcshell-common.toml";
        let push = PushGroupResults {
            push: PushRef {
                id: 1,
                revision: "abc".to_string(),
                push_timestamp: 1,
            },
            results: HashMap::from([(
                "t1".to_string(),
                HashMap::from([(remote.to_string(), false)]),
            )]),
            allowed_tasks: None,
            evidence: HashMap::from([(
                "t2".to_string(),
                GroupEvidence::parse(&format!(
                    r#"{{"groups": ["{plain}", "other/xpcshell.toml"], "action": "test_groups", "line": 1}}"#
                )),
            )]),
        };

        let names = included_group_names(manifest, &push);

        // With --filter/--platform, only selected tasks suggest names.
        let filtered = PushGroupResults {
            push: push.push.clone(),
            results: push.results.clone(),
            allowed_tasks: Some(HashSet::from(["t2".to_string()])),
            evidence: push.evidence.clone(),
        };
        assert_eq!(
            included_group_names(manifest, &filtered),
            BTreeSet::from([plain.to_string()])
        );
        let filtered = PushGroupResults {
            allowed_tasks: Some(HashSet::from(["t1".to_string()])),
            ..filtered
        };
        assert_eq!(
            included_group_names(manifest, &filtered),
            BTreeSet::from([remote.to_string()])
        );
        assert_eq!(
            names,
            BTreeSet::from([plain.to_string(), remote.to_string()])
        );

        // Neither variant is counted as the manifest itself.
        let entry = summarize_push(manifest, push);
        assert_eq!(
            (entry.ok, entry.fail, entry.state),
            (0, 0, GroupState::NotRun)
        );
        let mut history = build_group_history(manifest, vec![entry], 1);
        suggest_included_names(&mut history, names.clone());
        assert_eq!(
            history.included_as,
            vec![remote.to_string(), plain.to_string()]
        );

        // Once the manifest has results of its own the names are not needed.
        let mut history = build_group_history(
            manifest,
            vec![GroupHistoryPush {
                push_id: 1,
                revision: "abc".to_string(),
                timestamp: 1,
                ok: 1,
                fail: 0,
                state: GroupState::Pass,
            }],
            1,
        );
        suggest_included_names(&mut history, names);
        assert!(history.included_as.is_empty());
    }

    #[test]
    fn state_from_counts() {
        assert_eq!(GroupState::from_counts(0, 0), GroupState::NotRun);
        assert_eq!(GroupState::from_counts(3, 0), GroupState::Pass);
        assert_eq!(GroupState::from_counts(25, 1), GroupState::Mixed);
        assert_eq!(GroupState::from_counts(3, 3), GroupState::Fail);
        assert_eq!(GroupState::from_counts(0, 2), GroupState::Fail);
    }
}
