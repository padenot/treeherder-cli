use crate::models::*;
use anyhow::Result;
use serde::Serialize;

#[derive(Serialize)]
pub struct JsonOutput {
    pub revision: String,
    pub push_id: u64,
    pub jobs: Vec<JobWithLogs>,
}

#[derive(Serialize)]
pub struct GroupedJsonOutput {
    pub revision: String,
    pub push_id: u64,
    pub grouped_failures: Vec<GroupedTestFailure>,
}

pub fn format_json_output(revision: &str, push_id: u64, jobs: &[JobWithLogs]) -> Result<String> {
    let output = JsonOutput {
        revision: revision.to_string(),
        push_id,
        jobs: jobs.to_vec(),
    };
    Ok(serde_json::to_string_pretty(&output)?)
}

pub fn format_grouped_json_output(
    revision: &str,
    push_id: u64,
    grouped: &[GroupedTestFailure],
) -> Result<String> {
    let output = GroupedJsonOutput {
        revision: revision.to_string(),
        push_id,
        grouped_failures: grouped.to_vec(),
    };
    Ok(serde_json::to_string_pretty(&output)?)
}

pub fn format_comparison_json(result: &ComparisonResult) -> Result<String> {
    Ok(serde_json::to_string_pretty(result)?)
}

pub fn format_perf_json(revision: &str, push_id: u64, perf_data: &[JobPerfData]) -> Result<String> {
    let output = serde_json::json!({
        "revision": revision,
        "push_id": push_id,
        "jobs": perf_data,
    });
    Ok(serde_json::to_string_pretty(&output)?)
}

pub fn format_similar_history_json(history: &SimilarJobHistory) -> Result<String> {
    Ok(serde_json::to_string_pretty(history)?)
}
