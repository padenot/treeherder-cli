use crate::models::*;
use anyhow::Result;
use regex::Regex;
use reqwest::Client;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use url::Url;

pub async fn fetch_lando_job_status(client: &Client, job_id: u64) -> Result<LandoJobResponse> {
    let url = format!(
        "https://api.lando.services.mozilla.com/landing_jobs/{}",
        job_id
    );

    let response: LandoJobResponse = client.get(&url).send().await?.json().await?;

    if response.id != job_id {
        anyhow::bail!(
            "Lando API returned unexpected job ID: expected {}, got {}",
            job_id,
            response.id
        );
    }

    Ok(response)
}

pub async fn fetch_commit_from_lando_job(client: &Client, job_id: u64) -> Result<String> {
    let response = fetch_lando_job_status(client, job_id).await?;

    if response.status != "LANDED" {
        anyhow::bail!(
            "Lando job {} has not landed yet (status: {}). Only LANDED jobs have commit IDs.",
            job_id,
            response.status
        );
    }

    if let Some(commit_id) = response.commit_id {
        Ok(commit_id)
    } else {
        anyhow::bail!(
            "Lando job {} is marked as LANDED but has no commit_id",
            job_id
        )
    }
}

pub fn extract_revision(input: &str) -> Result<String> {
    if input.starts_with("http") {
        let url = Url::parse(input)?;
        if let Some(revision) = url
            .query_pairs()
            .find(|(key, _)| key == "revision")
            .map(|(_, value)| value.to_string())
        {
            Ok(revision)
        } else {
            anyhow::bail!("No revision found in URL")
        }
    } else {
        Ok(input.to_string())
    }
}

pub async fn fetch_push_id(client: &Client, repo: &str, revision: &str) -> Result<u64> {
    let url = format!(
        "https://treeherder.mozilla.org/api/project/{}/push/?full=true&count=10&revision={}",
        repo, revision
    );

    let response: PushResponse = client.get(&url).send().await?.json().await?;

    response
        .results
        .first()
        .map(|r| r.id)
        .ok_or_else(|| anyhow::anyhow!("No push found for revision"))
}

pub async fn fetch_jobs(client: &Client, push_id: u64) -> Result<Vec<Job>> {
    let url = format!(
        "https://treeherder.mozilla.org/api/jobs/?push_id={}",
        push_id
    );

    let response: JobsResponse = client.get(&url).send().await?.json().await?;

    // Build field name → index mapping from job_property_names
    let field_map: HashMap<&str, usize> = response
        .job_property_names
        .iter()
        .enumerate()
        .map(|(idx, name)| (name.as_str(), idx))
        .collect();

    let mut jobs = Vec::new();
    for job_array in response.results {
        // Helper to safely get field by name for this specific job_array
        let get_field = |field_name: &str| -> Option<&serde_json::Value> {
            field_map
                .get(field_name)
                .and_then(|&idx| job_array.get(idx))
        };

        // Extract fields by NAME instead of hardcoded index
        if let (
            Some(id),
            Some(job_type_name),
            Some(job_type_symbol),
            Some(platform),
            Some(result),
            Some(state),
        ) = (
            get_field("id").and_then(|v| v.as_u64()),
            get_field("job_type_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            get_field("job_type_symbol")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            get_field("platform")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            get_field("result")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            get_field("state")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        ) {
            let platform_option = get_field("platform_option")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();

            let duration = get_field("duration").and_then(|v| v.as_u64());

            let failure_classification_id =
                get_field("failure_classification_id").and_then(|v| v.as_u64());

            jobs.push(Job {
                id,
                job_type_name,
                job_type_symbol,
                platform,
                platform_option,
                result,
                state,
                failure_classification_id,
                duration,
            });
        }
    }

    Ok(jobs)
}

pub async fn fetch_job_details(client: &Client, repo: &str, job_id: u64) -> Result<JobDetail> {
    let url = format!(
        "https://treeherder.mozilla.org/api/project/{}/jobs/{}/",
        repo, job_id
    );

    let job_detail: JobDetail = client.get(&url).send().await?.json().await?;

    Ok(job_detail)
}

pub async fn fetch_job_details_extended(
    client: &Client,
    repo: &str,
    job_id: u64,
) -> Result<JobDetailExtended> {
    let url = format!(
        "https://treeherder.mozilla.org/api/project/{}/jobs/{}/",
        repo, job_id
    );

    let job_detail: JobDetailExtended = client.get(&url).send().await?.json().await?;

    Ok(job_detail)
}

pub async fn fetch_taskcluster_artifacts(
    client: &Client,
    task_id: &str,
    retry_id: u64,
) -> Result<Vec<TaskclusterArtifact>> {
    let url = format!(
        "https://firefox-ci-tc.services.mozilla.com/api/queue/v1/task/{}/runs/{}/artifacts",
        task_id, retry_id
    );

    let response: TaskclusterArtifactsResponse = client.get(&url).send().await?.json().await?;

    Ok(response.artifacts)
}

pub async fn download_artifact(
    client: &Client,
    task_id: &str,
    retry_id: u64,
    artifact_name: &str,
    output_dir: &Path,
) -> Result<PathBuf> {
    let url = format!(
        "https://firefox-ci-tc.services.mozilla.com/api/queue/v1/task/{}/runs/{}/artifacts/{}",
        task_id, retry_id, artifact_name
    );

    let response = client.get(&url).send().await?;
    let bytes = response.bytes().await?;

    let artifact_path = output_dir.join(artifact_name);
    if let Some(parent) = artifact_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(&artifact_path, bytes)?;

    Ok(artifact_path)
}

pub async fn download_job_artifacts(
    client: &Client,
    repo: &str,
    job: &Job,
    output_dir: &Path,
    artifact_pattern: Option<&Regex>,
) -> Result<Vec<String>> {
    let job_detail = fetch_job_details_extended(client, repo, job.id).await?;

    let (task_id, retry_id) = match (job_detail.task_id, job_detail.retry_id) {
        (Some(tid), Some(rid)) => (tid, rid),
        _ => return Ok(vec![]),
    };

    let artifacts = fetch_taskcluster_artifacts(client, &task_id, retry_id).await?;

    let mut downloaded = Vec::new();

    let job_dir = output_dir.join(format!("job-{}", job.id));
    fs::create_dir_all(&job_dir)?;

    for artifact in artifacts {
        if let Some(pattern) = artifact_pattern {
            if !pattern.is_match(&artifact.name) {
                continue;
            }
        }

        match download_artifact(client, &task_id, retry_id, &artifact.name, &job_dir).await {
            Ok(path) => {
                downloaded.push(path.display().to_string());
            }
            Err(e) => {
                eprintln!("Failed to download {}: {}", artifact.name, e);
            }
        }
    }

    Ok(downloaded)
}

pub async fn fetch_error_summary(client: &Client, log_url: &str) -> Result<Vec<ErrorLine>> {
    if log_url.contains("errorsummary") {
        let response = client.get(log_url).send().await?.text().await?;

        let mut errors = Vec::new();
        for line in response.lines() {
            if let Ok(error_line) = serde_json::from_str::<ErrorLine>(line) {
                if error_line.action == "test_result"
                    && error_line.status.as_ref().is_some_and(|s| s == "FAIL")
                {
                    errors.push(error_line);
                }
            }
        }
        Ok(errors)
    } else {
        Ok(vec![])
    }
}

pub async fn fetch_job_details_with_errors(
    client: &Client,
    repo: &str,
    job: Job,
) -> Result<(Job, Vec<ErrorLine>)> {
    let job_detail = fetch_job_details(client, repo, job.id).await?;

    let error_futures: Vec<_> = job_detail
        .logs
        .iter()
        .filter(|log_ref| log_ref.name.contains("error") || log_ref.name.contains("summary"))
        .map(|log_ref| fetch_error_summary(client, &log_ref.url))
        .collect();

    let error_results = futures::future::join_all(error_futures).await;

    let mut all_errors = Vec::new();
    for result in error_results {
        match result {
            Ok(errors) => all_errors.extend(errors),
            Err(e) => eprintln!("Failed to fetch error summary: {}", e),
        }
    }

    Ok((job, all_errors))
}

pub async fn fetch_and_save_log(
    client: &Client,
    log_url: &str,
    log_name: &str,
    job_dir: &Path,
) -> Result<PathBuf> {
    let response = client.get(log_url).send().await?;
    let content = response.text().await?;

    let log_path = job_dir.join(format!("{}.log", log_name));
    fs::write(&log_path, content)?;

    Ok(log_path)
}

pub async fn fetch_job_with_full_logs(
    client: &Client,
    repo: &str,
    job: Job,
    temp_dir: &Path,
    pattern: Option<&Regex>,
) -> Result<JobWithLogs> {
    let job_detail = fetch_job_details(client, repo, job.id).await?;

    let job_dir = temp_dir.join(format!("job_{}", job.id));
    fs::create_dir_all(&job_dir)?;

    let error_futures: Vec<_> = job_detail
        .logs
        .iter()
        .filter(|log_ref| log_ref.name.contains("error") || log_ref.name.contains("summary"))
        .map(|log_ref| fetch_error_summary(client, &log_ref.url))
        .collect();

    let error_results = futures::future::join_all(error_futures).await;
    let mut all_errors = Vec::new();
    for errors in error_results.into_iter().flatten() {
        all_errors.extend(errors);
    }

    let log_futures: Vec<_> = job_detail
        .logs
        .iter()
        .map(|log_ref| fetch_and_save_log(client, &log_ref.url, &log_ref.name, &job_dir))
        .collect();

    let log_results = futures::future::join_all(log_futures).await;

    let mut log_matches = Vec::new();
    if let Some(regex) = pattern {
        for (log_ref, log_path) in job_detail
            .logs
            .iter()
            .zip(log_results.iter().filter_map(|r| r.as_ref().ok()))
        {
            if let Ok(matches) = search_log_file(log_path, regex, &log_ref.name) {
                log_matches.extend(matches);
            }
        }
    }

    Ok(JobWithLogs {
        job,
        errors: all_errors,
        log_matches,
        log_dir: Some(job_dir.to_string_lossy().to_string()),
    })
}

pub async fn fetch_job_perf_data(client: &Client, repo: &str, job: &Job) -> Result<JobPerfData> {
    let job_detail = fetch_job_details_extended(client, repo, job.id).await?;

    let perf_data = if let (Some(task_id), Some(retry_id)) =
        (job_detail.task_id, job_detail.retry_id)
    {
        let perf_url = format!(
            "https://firefox-ci-tc.services.mozilla.com/api/queue/v1/task/{}/runs/{}/artifacts/public/test_info/perfherder-data-resource-usage.json",
            task_id, retry_id
        );

        match client.get(&perf_url).send().await {
            Ok(response) => {
                if let Ok(text) = response.text().await {
                    if let Ok(redirect_info) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(url) = redirect_info.get("url").and_then(|u| u.as_str()) {
                            if let Ok(perf_response) = client.get(url).send().await {
                                (perf_response.json::<PerfherderData>().await).ok()
                            } else {
                                None
                            }
                        } else {
                            serde_json::from_str::<PerfherderData>(&text).ok()
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Err(_) => None,
        }
    } else {
        None
    };

    Ok(JobPerfData {
        job_id: job.id,
        job_type_name: job.job_type_name.clone(),
        platform: job.platform.clone(),
        perf_data,
    })
}

pub async fn fetch_similar_jobs(
    client: &Client,
    repo: &str,
    job_id: u64,
    count: usize,
) -> Result<SimilarJobHistory> {
    let url = format!(
        "https://treeherder.mozilla.org/api/project/{}/jobs/{}/similar_jobs/?count={}",
        repo, job_id, count
    );

    let response: SimilarJobsResponse = client.get(&url).send().await?.json().await?;

    let job_type_name = response
        .results
        .first()
        .map(|j| j.job_type_name.clone())
        .unwrap_or_default();

    let pass_count = response
        .results
        .iter()
        .filter(|j| j.result == "success")
        .count();
    let fail_count = response
        .results
        .iter()
        .filter(|j| j.result == "testfailed" || j.result == "busted")
        .count();
    let total = response.results.len();
    let pass_rate = if total > 0 {
        (pass_count as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    Ok(SimilarJobHistory {
        job_id,
        job_type_name,
        repo: response.meta.repository,
        total_jobs: total,
        pass_count,
        fail_count,
        pass_rate,
        jobs: response.results,
    })
}

fn search_log_file(log_path: &PathBuf, pattern: &Regex, log_name: &str) -> Result<Vec<LogMatch>> {
    let content = fs::read_to_string(log_path)?;
    let mut matches = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        if pattern.is_match(line) {
            matches.push(LogMatch {
                log_name: log_name.to_string(),
                line_number: line_num + 1,
                line_content: line.to_string(),
            });
        }
    }

    Ok(matches)
}
