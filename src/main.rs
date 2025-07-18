use anyhow::Result;
use clap::Parser;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use url::Url;

#[derive(Parser, Debug)]
#[command(
    name = "treeherder-check",
    about = "Fetch and summarize Treeherder test results for Firefox developers"
)]
struct Args {
    #[arg(help = "Treeherder URL or revision hash")]
    input: String,
    #[arg(long, default_value = "try", help = "Repository name")]
    repo: String,
    #[arg(long, help = "Show stack traces in error summaries")]
    show_stack_traces: bool,
    #[arg(
        long,
        help = "Only show jobs matching this regex pattern (applied to job_type_name)"
    )]
    filter: Option<String>,
}

#[derive(Deserialize, Debug)]
struct PushResponse {
    results: Vec<PushResult>,
}

#[derive(Deserialize, Debug)]
struct PushResult {
    id: u64,
    #[allow(dead_code)]
    revision: String,
}

#[derive(Deserialize, Debug)]
struct JobsResponse {
    results: Vec<Vec<serde_json::Value>>,
}

#[derive(Deserialize, Debug)]
struct Job {
    id: u64,
    job_type_name: String,
    job_type_symbol: String,
    platform: String,
    #[allow(dead_code)]
    platform_option: String,
    result: String,
    #[allow(dead_code)]
    state: String,
    #[allow(dead_code)]
    failure_classification_id: Option<u64>,
}

#[derive(Deserialize, Debug)]
struct JobDetail {
    #[allow(dead_code)]
    id: u64,
    #[allow(dead_code)]
    job_type_name: String,
    #[allow(dead_code)]
    platform: String,
    #[allow(dead_code)]
    result: String,
    logs: Vec<LogReference>,
}

#[derive(Deserialize, Debug)]
struct LogReference {
    name: String,
    url: String,
}

#[derive(Deserialize, Debug)]
struct ErrorLine {
    action: String,
    #[allow(dead_code)]
    line: u64,
    #[serde(default)]
    test: Option<String>,
    #[serde(default)]
    subtest: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    stack: Option<String>,
}

fn extract_revision(input: &str) -> Result<String> {
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

async fn fetch_push_id(client: &Client, repo: &str, revision: &str) -> Result<u64> {
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

async fn fetch_jobs(client: &Client, push_id: u64) -> Result<Vec<Job>> {
    let url = format!(
        "https://treeherder.mozilla.org/api/jobs/?push_id={}",
        push_id
    );

    let response: JobsResponse = client.get(&url).send().await?.json().await?;

    let mut jobs = Vec::new();
    for job_array in response.results {
        if job_array.len() >= 18 {
            if let (
                Some(id),
                Some(job_type_name),
                Some(job_type_symbol),
                Some(platform),
                Some(result),
                Some(state),
            ) = (
                job_array[1].as_u64(),
                job_array[5].as_str().map(|s| s.to_string()),
                job_array[3].as_str().map(|s| s.to_string()),
                job_array[7].as_str().map(|s| s.to_string()),
                job_array[10].as_str().map(|s| s.to_string()),
                job_array[12].as_str().map(|s| s.to_string()),
            ) {
                let platform_option = job_array[17]
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "".to_string());
                jobs.push(Job {
                    id,
                    job_type_name,
                    job_type_symbol,
                    platform,
                    platform_option,
                    result,
                    state,
                    failure_classification_id: job_array[15].as_u64(),
                });
            }
        }
    }

    Ok(jobs)
}

async fn fetch_job_details(client: &Client, repo: &str, job_id: u64) -> Result<JobDetail> {
    let url = format!(
        "https://treeherder.mozilla.org/api/project/{}/jobs/{}/",
        repo, job_id
    );

    let job_detail: JobDetail = client.get(&url).send().await?.json().await?;

    Ok(job_detail)
}

async fn fetch_error_summary(client: &Client, log_url: &str) -> Result<Vec<ErrorLine>> {
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

async fn fetch_job_details_with_errors(
    client: &Client,
    repo: &str,
    job: Job,
) -> Result<(Job, Vec<ErrorLine>)> {
    let job_detail = fetch_job_details(client, repo, job.id).await?;

    // Fetch error summaries in parallel for all relevant logs
    let error_futures: Vec<_> = job_detail
        .logs
        .iter()
        .filter(|log_ref| log_ref.name.contains("error") || log_ref.name.contains("summary"))
        .map(|log_ref| fetch_error_summary(client, &log_ref.url))
        .collect();

    // Execute all error summary fetches in parallel
    let error_results = futures::future::join_all(error_futures).await;

    // Collect all errors
    let mut all_errors = Vec::new();
    for result in error_results {
        match result {
            Ok(errors) => all_errors.extend(errors),
            Err(e) => eprintln!("Failed to fetch error summary: {}", e),
        }
    }

    Ok((job, all_errors))
}

fn format_markdown_summary(
    revision: &str,
    push_id: u64,
    failed_jobs: &[(Job, Vec<ErrorLine>)],
    show_stack_traces: bool,
) -> String {
    let mut output = String::new();

    output.push_str("# Treeherder Test Results Summary\n\n");
    output.push_str(&format!("**Revision:** `{}`\n", revision));
    output.push_str(&format!("**Push ID:** `{}`\n\n", push_id));

    if failed_jobs.is_empty() {
        output.push_str("✅ **No failed jobs found!**\n");
        return output;
    }

    output.push_str(&format!(
        "## Failed Jobs ({} failures)\n\n",
        failed_jobs.len()
    ));

    for (job, errors) in failed_jobs {
        output.push_str(&format!("### {} - {}\n\n", job.job_type_name, job.platform));
        output.push_str(&format!("- **Job ID:** `{}`\n", job.id));
        output.push_str(&format!("- **Symbol:** `{}`\n", job.job_type_symbol));
        output.push_str(&format!("- **Platform:** `{}`\n\n", job.platform));

        if !errors.is_empty() {
            output.push_str("**Error Summary:**\n\n");
            for error in errors {
                if let Some(test) = &error.test {
                    output.push_str(&format!("**Test:** `{}`\n", test));
                }
                if let Some(subtest) = &error.subtest {
                    output.push_str(&format!("**Subtest:** `{}`\n", subtest));
                }
                if let Some(status) = &error.status {
                    output.push_str(&format!("**Status:** `{}`\n", status));
                }
                if let Some(message) = &error.message {
                    output.push_str(&format!("**Message:** `{}`\n", message));
                }
                if show_stack_traces {
                    if let Some(stack) = &error.stack {
                        output.push_str(&format!("**Stack Trace:**\n```\n{}\n```\n", stack));
                    }
                }
                output.push('\n');
            }
            output.push('\n');
        } else {
            output.push_str("*No error summary available*\n\n");
        }

        output.push_str("---\n\n");
    }

    output
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = Client::new();

    // Create progress bar
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap(),
    );

    pb.set_message("Extracting revision from input");
    let revision = extract_revision(&args.input)?;

    pb.set_message("Fetching push ID");
    let push_id = fetch_push_id(&client, &args.repo, &revision).await?;

    pb.set_message("Fetching jobs");
    let jobs = fetch_jobs(&client, push_id).await?;

    let mut failed_jobs: Vec<_> = jobs
        .into_iter()
        .filter(|job| job.result == "testfailed" || job.result == "busted")
        .collect();

    // Apply job name filter if provided
    if let Some(filter_pattern) = &args.filter {
        failed_jobs.retain(|job| job.job_type_name.contains(filter_pattern));
    }

    if failed_jobs.is_empty() {
        pb.finish_with_message("No failed jobs found");
    } else {
        pb.finish_with_message("Found failed jobs");
    }

    // Create progress bar for job details fetching
    let pb_jobs = ProgressBar::new(failed_jobs.len() as u64);
    pb_jobs.set_style(
        ProgressStyle::default_bar()
            .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap(),
    );
    pb_jobs.set_message("Fetching job details in parallel");

    let client = Arc::new(client);
    let pb_jobs = Arc::new(pb_jobs);

    // Fetch job details in parallel with concurrency limit
    let failed_jobs_with_errors: Vec<_> = stream::iter(failed_jobs)
        .map(|job| {
            let client = Arc::clone(&client);
            let repo = args.repo.clone();
            let pb_jobs = Arc::clone(&pb_jobs);

            async move {
                let result = fetch_job_details_with_errors(&client, &repo, job).await;
                pb_jobs.inc(1);
                result
            }
        })
        .buffer_unordered(10) // Limit concurrent requests to 10
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .filter_map(|result| result.ok())
        .collect();

    pb_jobs.finish_with_message("Completed fetching job details");

    let summary = format_markdown_summary(
        &revision,
        push_id,
        &failed_jobs_with_errors,
        args.show_stack_traces,
    );
    println!("{}", summary);

    Ok(())
}
