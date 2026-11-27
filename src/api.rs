use crate::groups::GroupEvidence;
use crate::models::*;
use anyhow::Result;
use futures::stream::{self, StreamExt};
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use url::Url;

pub const TC_ROOT_URL: &str = "https://firefox-ci-tc.services.mozilla.com";

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

pub fn extract_repo_from_url(input: &str) -> Option<String> {
    if input.starts_with("http") {
        Url::parse(input).ok().and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "repo")
                .map(|(_, value)| value.to_string())
        })
    } else {
        None
    }
}

pub fn extract_lando_commit_id(input: &str) -> Option<u64> {
    if input.starts_with("http") {
        Url::parse(input).ok().and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "landoCommitID")
                .and_then(|(_, value)| value.parse().ok())
        })
    } else if input.chars().all(|c| c.is_ascii_digit()) {
        input.parse().ok()
    } else {
        None
    }
}

pub fn extract_lando_instance(input: &str) -> Option<String> {
    if input.starts_with("http") {
        Url::parse(input).ok().and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "landoInstance")
                .map(|(_, value)| value.to_string())
        })
    } else {
        None
    }
}

fn lando_base_url(instance: &str) -> &'static str {
    match instance {
        "lando-dev" => "api.dev.lando.nonprod.cloudops.mozgcp.net",
        "lando-dev-2025" => "lando-dev.allizom.org",
        "lando-prod" => "api.lando.services.mozilla.com",
        _ => "lando.moz.tools",
    }
}

fn client_builder() -> reqwest::ClientBuilder {
    Client::builder().user_agent(concat!("treeherder-cli/", env!("CARGO_PKG_VERSION")))
}

pub fn build_client() -> Result<Client> {
    Ok(client_builder().build()?)
}

pub async fn fetch_revision_from_lando(instance: Option<&str>, commit_id: u64) -> Result<String> {
    let base_url = lando_base_url(instance.unwrap_or("lando-prod-2025"));
    let url = format!("https://{}/landing_jobs/{}/", base_url, commit_id);

    fetch_lando_job(&url, commit_id).await
}

async fn fetch_lando_job(url: &str, commit_id: u64) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct LandoJob {
        commit_id: Option<String>,
    }

    // Lando's CDN can return an empty 406 to reqwest over HTTP/2.
    let client = client_builder().http1_only().build()?;
    let request = || {
        client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
    };
    let mut body = request().send().await?.error_for_status()?.bytes().await?;
    // Retry one empty successful response before attempting to parse JSON.
    if body.is_empty() {
        body = request().send().await?.error_for_status()?.bytes().await?;
    }
    anyhow::ensure!(
        !body.is_empty(),
        "Empty Lando response for job {}",
        commit_id
    );
    let job: LandoJob = serde_json::from_slice(&body)?;
    job.commit_id
        .ok_or_else(|| anyhow::anyhow!("No commit_id in Lando response for job {}", commit_id))
}

pub async fn fetch_push_id(client: &Client, repo: &str, revision: &str) -> Result<u64> {
    Ok(fetch_push(client, repo, revision).await?.id)
}

pub async fn fetch_push(client: &Client, repo: &str, revision: &str) -> Result<PushRef> {
    fetch_push_matches(client, repo, revision)
        .await?
        .results
        .first()
        .map(PushRef::from)
        .ok_or_else(|| anyhow::anyhow!("No push found for revision"))
}

async fn fetch_push_matches(client: &Client, repo: &str, revision: &str) -> Result<PushResponse> {
    let mut url = Url::parse("https://treeherder.mozilla.org/api/project/")?;
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .extend([repo, "push", ""]);
    url.query_pairs_mut()
        .extend_pairs([("full", "true"), ("count", "10"), ("revision", revision)]);
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

pub fn validate_action_target(repo: &str, revision: &str) -> Result<()> {
    anyhow::ensure!(
        !repo.is_empty()
            && repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "Invalid Treeherder repository name"
    );
    anyhow::ensure!(
        (1..=40).contains(&revision.len()) && revision.bytes().all(|b| b.is_ascii_hexdigit()),
        "Actions require a nonempty hexadecimal revision or a Lando commit ID"
    );
    Ok(())
}

pub async fn find_action_push(
    client: &Client,
    repo: &str,
    revision: &str,
) -> Result<Option<PushRef>> {
    let response = fetch_push_matches(client, repo, revision).await?;
    select_action_push(&response.results, repo, revision)
}

fn select_action_push(
    pushes: &[PushResult],
    repo: &str,
    revision: &str,
) -> Result<Option<PushRef>> {
    match pushes {
        [] => Ok(None),
        [push]
            if push.revision.len() == 40
                && push.revision.bytes().all(|b| b.is_ascii_hexdigit())
                && push.revision.starts_with(revision) =>
        {
            Ok(Some(PushRef::from(push)))
        }
        [_] => anyhow::bail!("Treeherder returned a push that does not match {revision} on {repo}"),
        _ => anyhow::bail!(
            "Revision {revision} identifies multiple pushes on {repo}. Use the full revision."
        ),
    }
}

#[cfg(test)]
#[test]
fn action_push_resolution_requires_one_matching_revision() {
    assert!(validate_action_target("try", "").is_err());
    assert!(validate_action_target("try", "abcdef").is_ok());
    assert!(validate_action_target("try", "something else").is_err());
    let push = |id, revision| PushResult {
        id,
        revision,
        push_timestamp: 0,
    };
    let first = || push(1, "abc".to_owned() + &"d".repeat(37));
    let second = || push(2, "abc".to_owned() + &"e".repeat(37));
    assert!(select_action_push(&[], "try", "abc").unwrap().is_none());
    assert_eq!(
        select_action_push(&[first()], "try", "abc")
            .unwrap()
            .unwrap()
            .id,
        1
    );
    assert!(select_action_push(&[first()], "try", "def").is_err());
    assert!(select_action_push(&[first(), second()], "try", "abc").is_err());
}

pub async fn fetch_pushes_around(
    client: &Client,
    repo: &str,
    push_id: u64,
    count: u64,
) -> Result<(Vec<PushResult>, Vec<PushResult>)> {
    let before_url = format!(
        "https://treeherder.mozilla.org/api/project/{}/push/?count={}&id__lt={}&ordering=-push_timestamp",
        repo, count, push_id
    );
    let after_url = format!(
        "https://treeherder.mozilla.org/api/project/{}/push/?count={}&id__gt={}&ordering=push_timestamp",
        repo, count, push_id
    );

    let (before_resp, after_resp) = tokio::try_join!(
        async {
            client
                .get(&before_url)
                .send()
                .await?
                .json::<PushResponse>()
                .await
                .map_err(anyhow::Error::from)
        },
        async {
            client
                .get(&after_url)
                .send()
                .await?
                .json::<PushResponse>()
                .await
                .map_err(anyhow::Error::from)
        }
    )?;

    Ok((before_resp.results, after_resp.results))
}

pub async fn fetch_pushes_between(
    client: &Client,
    repo: &str,
    start_push_id: u64,
    end_push_id: u64,
) -> Result<Vec<PushRef>> {
    let low = start_push_id.min(end_push_id);
    let high = start_push_id.max(end_push_id);
    let url = format!(
        "https://treeherder.mozilla.org/api/project/{}/push/?count=500&id__gte={}&id__lte={}&ordering=push_timestamp",
        repo, low, high
    );

    let response: PushResponse = client.get(&url).send().await?.json().await?;
    let mut pushes: Vec<_> = response.results.iter().map(PushRef::from).collect();
    pushes.sort_by_key(|push| push.id);

    if pushes.len() >= 500 {
        anyhow::bail!("range returned 500 pushes; choose a smaller range");
    }

    Ok(pushes)
}

pub async fn fetch_push_window_ending_at(
    client: &Client,
    repo: &str,
    end_push: &PushRef,
    lookback: u64,
) -> Result<Vec<PushRef>> {
    let url = format!(
        "https://treeherder.mozilla.org/api/project/{}/push/?count={}&id__lt={}&ordering=-push_timestamp",
        repo, lookback, end_push.id
    );

    let response: PushResponse = client.get(&url).send().await?.json().await?;
    let mut pushes: Vec<_> = response.results.iter().map(PushRef::from).collect();
    pushes.push(end_push.clone());
    pushes.sort_by_key(|push| push.id);
    pushes.dedup_by_key(|push| push.id);

    Ok(pushes)
}

pub async fn fetch_pushes_by_ids(client: &Client, repo: &str, ids: &[u64]) -> Result<Vec<PushRef>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let id_list = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let url = format!(
        "https://treeherder.mozilla.org/api/project/{}/push/?count={}&id__in={}",
        repo,
        ids.len(),
        id_list
    );

    let response: PushResponse = client.get(&url).send().await?.json().await?;
    Ok(response.results.iter().map(PushRef::from).collect())
}

pub type GroupResults = HashMap<String, HashMap<String, bool>>;

pub async fn fetch_group_results(
    client: &Client,
    repo: &str,
    revision: &str,
) -> Result<GroupResults> {
    let url = format!(
        "https://treeherder.mozilla.org/api/project/{}/push/group_results/?revision={}",
        repo, revision
    );

    let text = client.get(&url).send().await?.text().await?;
    serde_json::from_str(&text).map_err(|err| {
        let preview: String = text.chars().take(120).collect();
        anyhow::anyhow!(
            "group_results for {} did not return task results ({}): {}",
            revision,
            err,
            preview
        )
    })
}

pub async fn fetch_jobs_by_push(client: &Client, pushes: &[PushRef]) -> Result<Vec<PushJobs>> {
    let futures: Vec<_> = pushes
        .iter()
        .map(|push| async move {
            let jobs = fetch_jobs(client, push.id).await?;
            Ok::<_, anyhow::Error>(PushJobs {
                push: push.clone(),
                jobs,
            })
        })
        .collect();
    let results = futures::future::join_all(futures).await;

    let mut push_jobs = Vec::new();
    for result in results {
        push_jobs.push(result?);
    }
    push_jobs.sort_by_key(|push_jobs| push_jobs.push.id);

    Ok(push_jobs)
}

pub async fn fetch_jobs_multi(client: &Client, push_ids: &[u64]) -> Result<Vec<Job>> {
    let futures: Vec<_> = push_ids.iter().map(|&id| fetch_jobs(client, id)).collect();
    let results = futures::future::join_all(futures).await;
    let mut all = Vec::new();
    for result in results {
        all.extend(result?);
    }
    Ok(all)
}

const MAX_JOB_PAGES: usize = 100;

pub async fn fetch_jobs(client: &Client, push_id: u64) -> Result<Vec<Job>> {
    let mut url = format!(
        "https://treeherder.mozilla.org/api/jobs/?push_id={}",
        push_id
    );

    let mut jobs = Vec::new();
    for _ in 0..MAX_JOB_PAGES {
        let response: JobsResponse = client.get(&url).send().await?.json().await?;
        let next = response.next.clone();
        jobs.extend(parse_jobs_page(response));

        match next {
            Some(next_url) => url = next_url,
            None => return Ok(jobs),
        }
    }

    anyhow::bail!(
        "push {} has more than {} pages of jobs; refusing to report a partial result",
        push_id,
        MAX_JOB_PAGES
    )
}

fn parse_jobs_page(response: JobsResponse) -> Vec<Job> {
    // Build field name -> index mapping from job_property_names
    let field_map: HashMap<&str, usize> = response
        .job_property_names
        .iter()
        .enumerate()
        .map(|(idx, name)| (name.as_str(), idx))
        .collect();

    let mut jobs = Vec::new();
    for job_array in &response.results {
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
            let task_id = get_field("task_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            jobs.push(Job {
                id,
                job_type_name,
                job_type_symbol,
                platform,
                platform_option,
                result,
                state,
                failure_classification_id,
                failure_classification: failure_classification_id.map(failure_classification_name),
                duration,
                task_id,
            });
        }
    }

    jobs
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
        "{}/api/queue/v1/task/{}/runs/{}/artifacts",
        TC_ROOT_URL, task_id, retry_id
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
        "{}/api/queue/v1/task/{}/runs/{}/artifacts/{}",
        TC_ROOT_URL, task_id, retry_id, artifact_name
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
        let response = fetch_text_following_taskcluster_redirect(client, log_url).await?;

        Ok(parse_error_summary(&response))
    } else {
        Ok(vec![])
    }
}

fn parse_error_summary(text: &str) -> Vec<ErrorLine> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<ErrorLine>(line).ok())
        .filter(|error| {
            let unexpected_result = matches!(error.action.as_str(), "test_result" | "test_end")
                && error.status.as_deref().is_some_and(|status| {
                    !matches!(status, "PASS" | "OK")
                        || error
                            .expected
                            .as_deref()
                            .is_some_and(|expected| expected != status)
                            && !error.known_intermittent.iter().any(|known| known == status)
                });
            (unexpected_result || error.signature.is_some()) && !is_harness_noise(error)
        })
        .collect()
}

fn is_harness_noise(error: &ErrorLine) -> bool {
    if error.subtest.is_some() || error.signature.is_some() {
        return false;
    }
    let Some(message) = error.message.as_deref() else {
        return false;
    };
    message == "xpcshell return code: 0"
        || message.starts_with("profile uploaded in ")
        || (message.starts_with("Finished in ") && message.ends_with("ms"))
}

async fn fetch_text_following_taskcluster_redirect(client: &Client, url: &str) -> Result<String> {
    let text = client.get(url).send().await?.text().await?;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(artifact_url) = value.get("url").and_then(|url| url.as_str()) {
            return Ok(client.get(artifact_url).send().await?.text().await?);
        }
    }
    Ok(text)
}

fn parse_live_log_failures(text: &str) -> Vec<ErrorLine> {
    let mut seen = std::collections::HashSet::new();
    let mut errors = Vec::new();
    for line in text.lines() {
        if let Some((idx, marker, status)) = [
            ("TEST-UNEXPECTED-FAIL | ", "FAIL"),
            ("TEST-UNEXPECTED-OK | ", "OK"),
            ("TEST-UNEXPECTED-PASS | ", "PASS"),
        ]
        .iter()
        .find_map(|(marker, status)| line.find(marker).map(|idx| (idx, *marker, *status)))
        {
            let rest = &line[idx + marker.len()..];
            let parts: Vec<&str> = rest.splitn(3, " | ").collect();
            if !parts.is_empty() {
                let test_name = parts[0].trim().to_string();
                let message = parts.get(1).map(|s| s.trim().to_string());
                if seen.insert(test_name.clone()) {
                    errors.push(ErrorLine {
                        action: "test_result".to_string(),
                        line: 0,
                        test: Some(test_name),
                        subtest: None,
                        status: Some(status.to_string()),
                        expected: None,
                        known_intermittent: vec![],
                        message,
                        stack: None,
                        signature: None,
                        stackwalk_stdout: None,
                    });
                }
            }
        } else if line.contains("ERROR - ") && line.contains("timed out") {
            if let Some(idx) = line.find("ERROR - ") {
                let msg = line[idx + "ERROR - ".len()..].trim().to_string();
                errors.push(ErrorLine {
                    action: "timeout".to_string(),
                    line: 0,
                    test: None,
                    subtest: None,
                    status: Some("TIMEOUT".to_string()),
                    expected: None,
                    known_intermittent: vec![],
                    message: Some(msg),
                    stack: None,
                    signature: None,
                    stackwalk_stdout: None,
                });
                break;
            }
        }
    }
    errors
}

pub async fn fetch_job_details_with_errors(
    client: &Client,
    repo: &str,
    job: Job,
) -> Result<(Job, Vec<ErrorLine>)> {
    let job_detail = fetch_job_details(client, repo, job.id).await?;

    let has_errorsummary = job_detail
        .logs
        .iter()
        .any(|l| l.url.contains("errorsummary"));

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

    if all_errors.is_empty() && !has_errorsummary {
        if let Some(live_log) = job_detail
            .logs
            .iter()
            .find(|l| l.name == "live_backing_log")
        {
            if let Ok(text) = client.get(&live_log.url).send().await?.text().await {
                all_errors = parse_live_log_failures(&text);
            }
        }
    }

    Ok((job, all_errors))
}

/// Job log listing; Treeherder accepts several job_id parameters per request.
#[derive(Deserialize)]
struct JobLogUrl {
    job_id: u64,
    name: String,
    url: String,
}

const JOB_LOG_URL_BATCH: usize = 40;
const ERRORSUMMARY_CONCURRENCY: usize = 8;

const TREEHERDER_API: &str = "https://treeherder.mozilla.org/api";

/// Errorsummary log URLs of each job. A batch that cannot be listed leaves its
/// jobs out, so they stay unknown, without losing the other batches.
async fn fetch_errorsummary_urls(
    client: &Client,
    api: &str,
    repo: &str,
    job_ids: &[u64],
) -> HashMap<u64, Vec<String>> {
    let mut urls: HashMap<u64, Vec<String>> = HashMap::new();
    for chunk in job_ids.chunks(JOB_LOG_URL_BATCH) {
        let query = chunk
            .iter()
            .map(|id| format!("job_id={}", id))
            .collect::<Vec<_>>()
            .join("&");
        let url = format!("{}/project/{}/job-log-url/?{}", api, repo, query);
        let logs = match list_job_logs(client, &url).await {
            Ok(logs) => logs,
            Err(e) => {
                eprintln!(
                    "Failed to list errorsummary logs of {} jobs: {}",
                    chunk.len(),
                    e
                );
                continue;
            }
        };
        for log in logs.into_iter().filter(|l| l.name == "errorsummary_json") {
            urls.entry(log.job_id).or_default().push(log.url);
        }
    }
    urls
}

async fn list_job_logs(client: &Client, url: &str) -> Result<Vec<JobLogUrl>> {
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

/// Group evidence of each job from its errorsummary logs. A job maps to None
/// when it has no errorsummary or it could not be read: nothing is known about
/// what it ran.
pub async fn fetch_group_evidence_for_jobs(
    client: &Client,
    repo: &str,
    job_ids: &[u64],
) -> HashMap<u64, Option<GroupEvidence>> {
    let mut evidence: HashMap<u64, Option<GroupEvidence>> =
        job_ids.iter().map(|id| (*id, None)).collect();
    if job_ids.is_empty() {
        return evidence;
    }
    let urls = fetch_errorsummary_urls(client, TREEHERDER_API, repo, job_ids).await;
    let fetched: Vec<_> = stream::iter(urls)
        .map(|(job_id, urls)| async move {
            let mut groups: Option<GroupEvidence> = None;
            for url in urls {
                match fetch_text_following_taskcluster_redirect(client, &url).await {
                    Ok(text) => {
                        if let Some(parsed) = GroupEvidence::parse(&text) {
                            groups
                                .get_or_insert_with(GroupEvidence::default)
                                .merge(parsed);
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to fetch error summary for job {}: {}", job_id, e);
                        return (job_id, None);
                    }
                }
            }
            (job_id, groups)
        })
        .buffer_unordered(ERRORSUMMARY_CONCURRENCY)
        .collect()
        .await;
    evidence.extend(fetched);
    evidence
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
    job: Job,
    job_detail: JobDetail,
    temp_dir: &Path,
    pattern: Option<&Regex>,
) -> Result<JobWithLogs> {
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

    let is_failure = job.result == "testfailed" || job.result == "busted";
    let logs_to_fetch: Vec<_> = job_detail
        .logs
        .iter()
        .filter(|log_ref| is_failure || log_ref.name != "live_backing_log")
        .collect();

    let log_futures: Vec<_> = logs_to_fetch
        .iter()
        .map(|log_ref| fetch_and_save_log(client, &log_ref.url, &log_ref.name, &job_dir))
        .collect();

    let log_results = futures::future::join_all(log_futures).await;

    let mut log_matches = Vec::new();
    if let Some(regex) = pattern {
        for (log_ref, log_path) in logs_to_fetch
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
            "{}/api/queue/v1/task/{}/runs/{}/artifacts/public/test_info/perfherder-data-resource-usage.json",
            TC_ROOT_URL,
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

    let mut response: SimilarJobsResponse = client.get(&url).send().await?.json().await?;

    let mut push_ids: Vec<u64> = response.results.iter().map(|j| j.push_id).collect();
    push_ids.sort_unstable();
    push_ids.dedup();
    let pushes: HashMap<u64, PushRef> = fetch_pushes_by_ids(client, repo, &push_ids)
        .await?
        .into_iter()
        .map(|push| (push.id, push))
        .collect();
    for job in &mut response.results {
        if let Some(push) = pushes.get(&job.push_id) {
            job.revision = Some(push.revision.clone());
            job.push_timestamp = Some(push.push_timestamp);
        }
        job.failure_classification = job
            .failure_classification_id
            .map(failure_classification_name);
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::{analyze_range_suspects, parse_revision_range};
    use reqwest::Client;

    fn jobs_page(next: Option<&str>, ids: &[u64]) -> JobsResponse {
        let results = ids
            .iter()
            .map(|id| {
                serde_json::json!([
                    id,
                    "test-linux64/opt",
                    "T",
                    "linux64",
                    "opt",
                    "testfailed",
                    "completed"
                ])
                .as_array()
                .unwrap()
                .clone()
            })
            .collect();
        JobsResponse {
            results,
            job_property_names: vec![
                "id".into(),
                "job_type_name".into(),
                "job_type_symbol".into(),
                "platform".into(),
                "platform_option".into(),
                "result".into(),
                "state".into(),
            ],
            next: next.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_parse_jobs_page_extracts_all_results() {
        let jobs = parse_jobs_page(jobs_page(None, &[1, 2, 3]));
        assert_eq!(
            jobs.iter().map(|job| job.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(jobs[0].result, "testfailed");
    }

    #[test]
    fn test_pages_accumulate_across_next_links() {
        let mut all = parse_jobs_page(jobs_page(Some("https://example.com/?page=2"), &[1, 2]));
        all.extend(parse_jobs_page(jobs_page(None, &[3])));
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_jobs_response_deserializes_next_link() {
        let response: JobsResponse = serde_json::from_str(
            r#"{"count": 4507, "next": "https://treeherder.mozilla.org/api/jobs/?page=2&push_id=1", "previous": null, "results": [], "job_property_names": []}"#,
        )
        .unwrap();
        assert_eq!(
            response.next.as_deref(),
            Some("https://treeherder.mozilla.org/api/jobs/?page=2&push_id=1")
        );
    }

    #[test]
    fn test_extract_lando_commit_id_from_url_with_lando_instance() {
        let url = "https://treeherder.mozilla.org/jobs?repo=try&landoInstance=lando-prod-2025&landoCommitID=42199";
        assert_eq!(extract_lando_commit_id(url), Some(42199));
    }

    #[test]
    fn test_extract_repo_from_lando_url() {
        let url = "https://treeherder.mozilla.org/jobs?repo=try&landoInstance=lando-prod-2025&landoCommitID=42199";
        assert_eq!(extract_repo_from_url(url), Some("try".to_string()));
    }

    #[test]
    fn test_extract_lando_instance() {
        let url = "https://treeherder.mozilla.org/jobs?repo=try&landoInstance=lando-prod-2025&landoCommitID=42199";
        assert_eq!(
            extract_lando_instance(url),
            Some("lando-prod-2025".to_string())
        );
    }

    #[tokio::test]
    async fn log_listing_keeps_batches_around_a_failed_one() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}/api", listener.local_addr().unwrap());
        let log = |job_id: u64| {
            format!(
                r#"[{{"job_id":{job_id},"name":"errorsummary_json","url":"https://example/{job_id}.log"}},
                    {{"job_id":{job_id},"name":"live_backing_log","url":"https://example/{job_id}.txt"}}]"#
            )
        };
        // Three batches: the second one fails, the others list one log each.
        let responses = [
            ("200 OK", log(1)),
            ("500 Internal Server Error", "oops".to_string()),
            ("200 OK", log(81)),
        ];
        let server = tokio::spawn(async move {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).await.unwrap();
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = Client::builder().http1_only().build().unwrap();
        let job_ids: Vec<u64> = (1..=81).collect();

        let urls = fetch_errorsummary_urls(&client, &api, "autoland", &job_ids).await;
        server.abort();
        assert_eq!(
            urls.get(&1),
            Some(&vec!["https://example/1.log".to_string()])
        );
        assert_eq!(
            urls.get(&81),
            Some(&vec!["https://example/81.log".to_string()])
        );
        assert_eq!(urls.len(), 2);
    }

    #[tokio::test]
    async fn lando_retries_an_empty_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/landing_jobs/120923/",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            for body in ["", r#"{"commit_id":"6d15fbee"}"#] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 1024];
                let n = stream.read(&mut request).await.unwrap();
                assert!(String::from_utf8_lossy(&request[..n])
                    .starts_with("GET /landing_jobs/120923/ HTTP/1.1\r\n"));
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        assert_eq!(fetch_lando_job(&url, 120923).await.unwrap(), "6d15fbee");
        server.await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn record_autoland_range_fixture_from_env() -> Result<()> {
        let repo =
            std::env::var("TREEHERDER_FIXTURE_REPO").unwrap_or_else(|_| "autoland".to_string());
        let range = std::env::var("TREEHERDER_FIXTURE_RANGE")
            .map_err(|_| anyhow::anyhow!("set TREEHERDER_FIXTURE_RANGE=START..END"))?;
        let job_filter = std::env::var("TREEHERDER_FIXTURE_JOB_FILTER").map_err(|_| {
            anyhow::anyhow!("set TREEHERDER_FIXTURE_JOB_FILTER to trim the recorded fixture")
        })?;
        let platform_filter = std::env::var("TREEHERDER_FIXTURE_PLATFORM").ok();

        let client = Client::builder()
            .user_agent("treeherder-cli fixture recorder")
            .build()?;
        let (start_revision, end_revision) = parse_revision_range(&range)?;
        let start_push = fetch_push(&client, &repo, &extract_revision(&start_revision)?).await?;
        let end_push = fetch_push(&client, &repo, &extract_revision(&end_revision)?).await?;
        let pushes = fetch_pushes_between(&client, &repo, start_push.id, end_push.id).await?;
        let mut push_jobs = fetch_jobs_by_push(&client, &pushes).await?;

        for push in &mut push_jobs {
            push.jobs.retain(|job| {
                job.job_type_name.contains(&job_filter)
                    && platform_filter
                        .as_ref()
                        .is_none_or(|platform| job.platform == *platform)
            });
        }

        let failed_jobs: Vec<_> = push_jobs
            .iter()
            .flat_map(|push_jobs| {
                push_jobs
                    .jobs
                    .iter()
                    .filter(|job| job.result == "testfailed" || job.result == "busted")
                    .cloned()
                    .map(|job| (push_jobs.push.clone(), job))
            })
            .collect();

        let futures = failed_jobs.into_iter().map(|(push, job)| {
            let client = client.clone();
            let repo = repo.clone();
            async move {
                fetch_job_details_with_errors(&client, &repo, job)
                    .await
                    .map(|(job, errors)| JobObservation { push, job, errors })
            }
        });
        let observations = futures::future::join_all(futures)
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        let analysis = analyze_range_suspects(&repo, &push_jobs, &observations);

        let fixture_observations = observations
            .iter()
            .map(|observation| {
                serde_json::json!({
                    "push_id": observation.push.id,
                    "job_id": observation.job.id,
                    "errors": observation.errors,
                })
            })
            .collect::<Vec<_>>();

        let fixture = serde_json::json!({
            "name": format!("{} {}", repo, range),
            "source": {
                "repo": repo,
                "range": range,
                "job_filter": job_filter,
                "platform_filter": platform_filter,
            },
            "repo": analysis.repo,
            "pushes": push_jobs,
            "observations": fixture_observations,
            "analysis": analysis,
        });

        println!("{}", serde_json::to_string_pretty(&fixture)?);
        Ok(())
    }
}

#[cfg(test)]
mod unexpected_success_tests {
    use super::{parse_error_summary, parse_live_log_failures};

    #[test]
    fn summary_keeps_unexpected_successes_and_ignores_expected_successes() {
        let summary = r#"{"action":"test_end","line":1,"test":"unexpected-ok","status":"OK","expected":"FAIL"}
{"action":"test_result","line":2,"test":"unexpected-pass","status":"PASS","expected":"FAIL"}
{"action":"test_result","line":3,"test":"expected-pass","status":"PASS"}
{"action":"test_result","line":4,"test":"expected-ok","status":"OK","expected":"OK"}
{"action":"test_result","line":5,"test":"failure","status":"FAIL"}
{"action":"test_result","line":6,"test":"intermittent-ok","status":"OK","expected":"FAIL","known_intermittent":["OK"]}
{"action":"test_result","line":7,"test":"unexpected-result-ok","status":"OK","expected":"FAIL"}"#;
        let errors = parse_error_summary(summary);
        let tests: Vec<_> = errors.iter().map(|e| e.test.as_deref().unwrap()).collect();
        assert_eq!(
            tests,
            [
                "unexpected-ok",
                "unexpected-pass",
                "failure",
                "unexpected-result-ok"
            ]
        );
    }

    #[test]
    fn live_log_keeps_unexpected_successes() {
        let errors = parse_live_log_failures(
            "TEST-UNEXPECTED-OK | test.html | expected FAIL
TEST-UNEXPECTED-PASS | other.html | expected FAIL",
        );
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].test.as_deref(), Some("test.html"));
        assert_eq!(errors[0].status.as_deref(), Some("OK"));
        assert_eq!(errors[1].status.as_deref(), Some("PASS"));
    }
}
