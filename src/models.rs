use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Deserialize, Debug)]
pub struct PushResponse {
    pub results: Vec<PushResult>,
}

#[derive(Deserialize, Debug)]
pub struct PushResult {
    pub id: u64,
    #[allow(dead_code)]
    pub revision: String,
}

#[derive(Deserialize, Debug)]
pub struct JobsResponse {
    pub results: Vec<Vec<serde_json::Value>>,
    pub job_property_names: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Job {
    pub id: u64,
    pub job_type_name: String,
    pub job_type_symbol: String,
    pub platform: String,
    #[allow(dead_code)]
    pub platform_option: String,
    pub result: String,
    #[allow(dead_code)]
    pub state: String,
    #[allow(dead_code)]
    pub failure_classification_id: Option<u64>,
    #[serde(default)]
    pub duration: Option<u64>,
}

#[derive(Deserialize, Debug)]
pub struct JobDetail {
    #[allow(dead_code)]
    pub id: u64,
    #[allow(dead_code)]
    pub job_type_name: String,
    #[allow(dead_code)]
    pub platform: String,
    #[allow(dead_code)]
    pub result: String,
    pub logs: Vec<LogReference>,
}

#[derive(Deserialize, Debug)]
pub struct LogReference {
    pub name: String,
    pub url: String,
}

#[derive(Deserialize, Debug)]
pub struct TaskclusterArtifactsResponse {
    pub artifacts: Vec<TaskclusterArtifact>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct TaskclusterArtifact {
    pub name: String,
    #[allow(dead_code)]
    #[serde(rename = "storageType")]
    pub storage_type: String,
    #[allow(dead_code)]
    pub expires: String,
    #[allow(dead_code)]
    #[serde(rename = "contentType")]
    pub content_type: Option<String>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct JobDetailExtended {
    pub id: u64,
    pub job_type_name: String,
    pub platform: String,
    pub result: String,
    pub logs: Vec<LogReference>,
    pub task_id: Option<String>,
    pub retry_id: Option<u64>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ErrorLine {
    pub action: String,
    #[allow(dead_code)]
    pub line: u64,
    #[serde(default)]
    pub test: Option<String>,
    #[serde(default)]
    pub subtest: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub stack: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct LogMatch {
    pub log_name: String,
    pub line_number: usize,
    pub line_content: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct JobWithLogs {
    pub job: Job,
    pub errors: Vec<ErrorLine>,
    pub log_matches: Vec<LogMatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_dir: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CachedPushMetadata {
    pub revision: String,
    pub push_id: u64,
    pub repo: String,
    pub jobs: Vec<Job>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupedTestFailure {
    pub test_name: String,
    pub platforms: Vec<String>,
    pub jobs: Vec<GroupedJobInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupedJobInfo {
    pub job_id: u64,
    pub platform: String,
    pub job_type_name: String,
    pub subtest: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparisonResult {
    pub base_revision: String,
    pub compare_revision: String,
    pub base_push_id: u64,
    pub compare_push_id: u64,
    pub new_failures: Vec<ComparisonFailure>,
    pub fixed_failures: Vec<ComparisonFailure>,
    pub still_failing: Vec<ComparisonFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparisonFailure {
    pub test_name: String,
    pub platforms: Vec<String>,
    pub job_type: String,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct PerfherderData {
    pub framework: PerfherderFramework,
    pub suites: Vec<PerfherderSuite>,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct PerfherderFramework {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct PerfherderSuite {
    pub name: String,
    #[serde(default)]
    pub subtests: Vec<PerfherderSubtest>,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct PerfherderSubtest {
    pub name: String,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobPerfData {
    pub job_id: u64,
    pub job_type_name: String,
    pub platform: String,
    pub perf_data: Option<PerfherderData>,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct SimilarJob {
    pub id: u64,
    pub job_type_name: String,
    pub platform: String,
    pub result: String,
    pub state: String,
    pub push_id: u64,
    #[serde(default)]
    pub start_timestamp: Option<u64>,
    #[serde(default)]
    pub end_timestamp: Option<u64>,
}

#[derive(Deserialize, Debug)]
pub struct SimilarJobsResponse {
    pub results: Vec<SimilarJob>,
    pub meta: SimilarJobsMeta,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct SimilarJobsMeta {
    pub count: usize,
    pub repository: String,
}

#[derive(Deserialize, Debug)]
pub struct LandoJobResponse {
    pub id: u64,
    pub status: String,
    pub commit_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SimilarJobHistory {
    pub job_id: u64,
    pub job_type_name: String,
    pub repo: String,
    pub total_jobs: usize,
    pub pass_count: usize,
    pub fail_count: usize,
    pub pass_rate: f64,
    pub jobs: Vec<SimilarJob>,
}

pub fn group_failures_by_test(jobs: &[JobWithLogs]) -> Vec<GroupedTestFailure> {
    let mut test_map: HashMap<String, Vec<GroupedJobInfo>> = HashMap::new();

    for job_with_logs in jobs {
        for error in &job_with_logs.errors {
            if let Some(test_name) = &error.test {
                let info = GroupedJobInfo {
                    job_id: job_with_logs.job.id,
                    platform: job_with_logs.job.platform.clone(),
                    job_type_name: job_with_logs.job.job_type_name.clone(),
                    subtest: error.subtest.clone(),
                    message: error.message.clone(),
                };
                test_map.entry(test_name.clone()).or_default().push(info);
            }
        }
    }

    let mut grouped: Vec<GroupedTestFailure> = test_map
        .into_iter()
        .map(|(test_name, jobs)| {
            let platforms: Vec<String> = jobs
                .iter()
                .map(|j| j.platform.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            GroupedTestFailure {
                test_name,
                platforms,
                jobs,
            }
        })
        .collect();

    grouped.sort_by(|a, b| b.platforms.len().cmp(&a.platforms.len()));
    grouped
}

pub fn compare_failures(
    base_jobs: &[JobWithLogs],
    compare_jobs: &[JobWithLogs],
    base_revision: &str,
    compare_revision: &str,
    base_push_id: u64,
    compare_push_id: u64,
) -> ComparisonResult {
    let base_failures: std::collections::HashSet<(String, String)> = base_jobs
        .iter()
        .filter(|j| j.job.result != "success")
        .flat_map(|j| {
            j.errors
                .iter()
                .filter_map(|e| e.test.clone())
                .map(move |test| (test, j.job.platform.clone()))
        })
        .collect();

    let compare_failures: std::collections::HashSet<(String, String)> = compare_jobs
        .iter()
        .filter(|j| j.job.result != "success")
        .flat_map(|j| {
            j.errors
                .iter()
                .filter_map(|e| e.test.clone())
                .map(move |test| (test, j.job.platform.clone()))
        })
        .collect();

    let new_failures_set: std::collections::HashSet<_> = base_failures
        .difference(&compare_failures)
        .cloned()
        .collect();

    let fixed_failures_set: std::collections::HashSet<_> = compare_failures
        .difference(&base_failures)
        .cloned()
        .collect();

    let still_failing_set: std::collections::HashSet<_> = base_failures
        .intersection(&compare_failures)
        .cloned()
        .collect();

    let mut new_by_test: HashMap<String, Vec<String>> = HashMap::new();
    for (test, platform) in new_failures_set {
        new_by_test.entry(test).or_default().push(platform);
    }

    let mut fixed_by_test: HashMap<String, Vec<String>> = HashMap::new();
    for (test, platform) in fixed_failures_set {
        fixed_by_test.entry(test).or_default().push(platform);
    }

    let mut still_by_test: HashMap<String, Vec<String>> = HashMap::new();
    for (test, platform) in still_failing_set {
        still_by_test.entry(test).or_default().push(platform);
    }

    let new_failures: Vec<_> = new_by_test
        .into_iter()
        .map(|(test_name, platforms)| ComparisonFailure {
            test_name,
            platforms,
            job_type: String::new(),
        })
        .collect();

    let fixed_failures: Vec<_> = fixed_by_test
        .into_iter()
        .map(|(test_name, platforms)| ComparisonFailure {
            test_name,
            platforms,
            job_type: String::new(),
        })
        .collect();

    let still_failing: Vec<_> = still_by_test
        .into_iter()
        .map(|(test_name, platforms)| ComparisonFailure {
            test_name,
            platforms,
            job_type: String::new(),
        })
        .collect();

    ComparisonResult {
        base_revision: base_revision.to_string(),
        compare_revision: compare_revision.to_string(),
        base_push_id,
        compare_push_id,
        new_failures,
        fixed_failures,
        still_failing,
    }
}
