use anyhow::Result;
use clap::{Parser, ValueEnum};
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use url::Url;

#[derive(Debug, Clone, ValueEnum)]
enum MatchFilter {
    Failure,
    Success,
    All,
}

#[derive(Parser, Debug)]
#[command(
    name = "treeherder-cli",
    about = "Fetch and summarize Treeherder test results for Firefox developers"
)]
struct Args {
    #[arg(help = "Treeherder URL or revision hash (not needed with --use-cache)")]
    input: Option<String>,
    #[arg(long, default_value = "try", help = "Repository name")]
    repo: String,
    #[arg(long, help = "Show stack traces in error summaries")]
    show_stack_traces: bool,
    #[arg(
        long,
        help = "Only show jobs matching this regex pattern (applied to job_type_name)"
    )]
    filter: Option<String>,
    #[arg(long, help = "Fetch all logs for each job")]
    fetch_logs: bool,
    #[arg(
        long,
        value_enum,
        default_value = "failure",
        help = "Filter which jobs to apply pattern matching on"
    )]
    match_filter: MatchFilter,
    #[arg(
        long,
        help = "Regex pattern to search for in logs (only used with --fetch-logs)"
    )]
    pattern: Option<String>,
    #[arg(
        long,
        help = "Directory to store/read cached logs (persistent storage, not temp)"
    )]
    cache_dir: Option<String>,
    #[arg(
        long,
        help = "Use cached logs without downloading (requires --cache-dir)"
    )]
    use_cache: bool,
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

#[derive(Deserialize, Serialize, Debug, Clone)]
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

#[derive(Debug)]
struct LogMatch {
    log_name: String,
    line_number: usize,
    line_content: String,
}

#[derive(Debug)]
struct JobWithLogs {
    job: Job,
    errors: Vec<ErrorLine>,
    log_matches: Vec<LogMatch>,
    log_dir: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug)]
struct CachedPushMetadata {
    revision: String,
    push_id: u64,
    repo: String,
    jobs: Vec<Job>,
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

async fn fetch_and_save_log(
    client: &Client,
    log_url: &str,
    log_name: &str,
    job_dir: &PathBuf,
) -> Result<PathBuf> {
    let response = client.get(log_url).send().await?;
    let content = response.text().await?;

    let log_path = job_dir.join(format!("{}.log", log_name));
    fs::write(&log_path, content)?;

    Ok(log_path)
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

fn save_cache_metadata(cache_dir: &PathBuf, metadata: &CachedPushMetadata) -> Result<()> {
    let metadata_path = cache_dir.join("metadata.json");
    let json = serde_json::to_string_pretty(metadata)?;
    fs::write(metadata_path, json)?;
    Ok(())
}

fn load_cache_metadata(cache_dir: &PathBuf) -> Result<CachedPushMetadata> {
    let metadata_path = cache_dir.join("metadata.json");
    let json = fs::read_to_string(metadata_path)?;
    let metadata: CachedPushMetadata = serde_json::from_str(&json)?;
    Ok(metadata)
}

fn search_cached_logs(
    cache_dir: &PathBuf,
    jobs: &[Job],
    pattern: Option<&Regex>,
) -> Result<Vec<JobWithLogs>> {
    let mut results = Vec::new();

    for job in jobs {
        let job_dir = cache_dir.join(format!("job_{}", job.id));

        if !job_dir.exists() {
            eprintln!("Warning: Job directory not found: {}", job_dir.display());
            continue;
        }

        let mut log_matches = Vec::new();

        // Search all log files in the job directory
        if let Some(regex) = pattern {
            let log_files = fs::read_dir(&job_dir)?;
            for entry in log_files {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("log") {
                        let log_name = path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();

                        if let Ok(matches) = search_log_file(&path, regex, &log_name) {
                            log_matches.extend(matches);
                        }
                    }
                }
            }
        }

        results.push(JobWithLogs {
            job: job.clone(),
            errors: vec![], // Error summaries not loaded from cache
            log_matches,
            log_dir: Some(job_dir),
        });
    }

    Ok(results)
}

async fn fetch_job_with_full_logs(
    client: &Client,
    repo: &str,
    job: Job,
    temp_dir: &PathBuf,
    pattern: Option<&Regex>,
) -> Result<JobWithLogs> {
    let job_detail = fetch_job_details(client, repo, job.id).await?;

    // Create directory for this job
    let job_dir = temp_dir.join(format!("job_{}", job.id));
    fs::create_dir_all(&job_dir)?;

    // Fetch error summaries (existing functionality)
    let error_futures: Vec<_> = job_detail
        .logs
        .iter()
        .filter(|log_ref| log_ref.name.contains("error") || log_ref.name.contains("summary"))
        .map(|log_ref| fetch_error_summary(client, &log_ref.url))
        .collect();

    let error_results = futures::future::join_all(error_futures).await;
    let mut all_errors = Vec::new();
    for result in error_results {
        if let Ok(errors) = result {
            all_errors.extend(errors);
        }
    }

    // Fetch all logs and save them
    let log_futures: Vec<_> = job_detail
        .logs
        .iter()
        .map(|log_ref| fetch_and_save_log(client, &log_ref.url, &log_ref.name, &job_dir))
        .collect();

    let log_results = futures::future::join_all(log_futures).await;

    // Search logs for pattern if provided
    let mut log_matches = Vec::new();
    if let Some(regex) = pattern {
        for (log_ref, log_result) in job_detail.logs.iter().zip(log_results.iter()) {
            if let Ok(log_path) = log_result {
                if let Ok(matches) = search_log_file(log_path, regex, &log_ref.name) {
                    log_matches.extend(matches);
                }
            }
        }
    }

    Ok(JobWithLogs {
        job,
        errors: all_errors,
        log_matches,
        log_dir: Some(job_dir),
    })
}

fn format_markdown_summary(
    revision: &str,
    push_id: u64,
    jobs: &[JobWithLogs],
    show_stack_traces: bool,
    fetch_logs: bool,
) -> String {
    let mut output = String::new();

    output.push_str("# Treeherder Test Results Summary\n\n");
    output.push_str(&format!("**Revision:** `{}`\n", revision));
    output.push_str(&format!("**Push ID:** `{}`\n\n", push_id));

    if jobs.is_empty() {
        output.push_str("✅ **No jobs found matching criteria!**\n");
        return output;
    }

    let failed_jobs: Vec<_> = jobs.iter().filter(|j| j.job.result != "success").collect();

    if !failed_jobs.is_empty() {
        output.push_str(&format!(
            "## Failed Jobs ({} failures)\n\n",
            failed_jobs.len()
        ));
    } else {
        output.push_str("✅ **No failed jobs found!**\n\n");
    }

    for job_with_logs in jobs {
        let job = &job_with_logs.job;
        let errors = &job_with_logs.errors;
        let log_matches = &job_with_logs.log_matches;

        output.push_str(&format!("### {} - {}\n\n", job.job_type_name, job.platform));
        output.push_str(&format!("- **Job ID:** `{}`\n", job.id));
        output.push_str(&format!("- **Symbol:** `{}`\n", job.job_type_symbol));
        output.push_str(&format!("- **Platform:** `{}`\n", job.platform));
        output.push_str(&format!("- **Result:** `{}`\n", job.result));

        if let Some(log_dir) = &job_with_logs.log_dir {
            output.push_str(&format!("- **Logs saved to:** `{}`\n", log_dir.display()));
        }
        output.push('\n');

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
        } else if !fetch_logs {
            output.push_str("*No error summary available*\n\n");
        }

        if fetch_logs && !log_matches.is_empty() {
            output.push_str(&format!("**Pattern Matches ({} matches):**\n\n", log_matches.len()));
            let max_matches_to_show = 20;
            for (idx, log_match) in log_matches.iter().take(max_matches_to_show).enumerate() {
                output.push_str(&format!(
                    "{}. **{}:{}** - `{}`\n",
                    idx + 1,
                    log_match.log_name,
                    log_match.line_number,
                    log_match.line_content.chars().take(200).collect::<String>()
                ));
            }
            if log_matches.len() > max_matches_to_show {
                output.push_str(&format!(
                    "\n*... and {} more matches (see log files)*\n",
                    log_matches.len() - max_matches_to_show
                ));
            }
            output.push('\n');
        }

        output.push_str("---\n\n");
    }

    output
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Validate arguments
    if !args.use_cache && args.input.is_none() {
        anyhow::bail!("INPUT is required when not using --use-cache");
    }

    // Handle use-cache mode (local grep only)
    if args.use_cache {
        let cache_dir = args.cache_dir
            .ok_or_else(|| anyhow::anyhow!("--use-cache requires --cache-dir to be specified"))?;
        let cache_path = PathBuf::from(&cache_dir);

        if !cache_path.exists() {
            anyhow::bail!("Cache directory does not exist: {}", cache_path.display());
        }

        println!("Loading cached data from: {}", cache_path.display());

        let metadata = load_cache_metadata(&cache_path)?;
        println!("Push ID: {}, Revision: {}", metadata.push_id, metadata.revision);
        println!("Cached jobs: {}", metadata.jobs.len());

        // Apply filters to cached jobs
        let mut filtered_jobs = metadata.jobs.clone();

        // Apply match filter
        filtered_jobs = match args.match_filter {
            MatchFilter::Failure => filtered_jobs
                .into_iter()
                .filter(|job| job.result == "testfailed" || job.result == "busted")
                .collect(),
            MatchFilter::Success => filtered_jobs
                .into_iter()
                .filter(|job| job.result == "success")
                .collect(),
            MatchFilter::All => filtered_jobs,
        };

        // Apply job name filter
        if let Some(filter_pattern) = &args.filter {
            filtered_jobs.retain(|job| job.job_type_name.contains(filter_pattern));
        }

        println!("Jobs matching filter: {}", filtered_jobs.len());

        // Compile regex pattern if provided
        let pattern = if let Some(pattern_str) = &args.pattern {
            Some(Regex::new(pattern_str)?)
        } else {
            None
        };

        // Search cached logs
        let jobs_with_logs = search_cached_logs(&cache_path, &filtered_jobs, pattern.as_ref())?;

        let summary = format_markdown_summary(
            &metadata.revision,
            metadata.push_id,
            &jobs_with_logs,
            args.show_stack_traces,
            true, // fetch_logs mode for formatting
        );
        println!("{}", summary);

        return Ok(());
    }

    // Normal download mode
    let client = Client::new();

    // Create progress bar
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap(),
    );

    pb.set_message("Extracting revision from input");
    let input = args.input.as_ref().unwrap(); // Safe because we validated above
    let revision = extract_revision(input)?;

    pb.set_message("Fetching push ID");
    let push_id = fetch_push_id(&client, &args.repo, &revision).await?;

    pb.set_message("Fetching jobs");
    let all_jobs = fetch_jobs(&client, push_id).await?;

    // Filter jobs based on match_filter
    let mut filtered_jobs: Vec<_> = match args.match_filter {
        MatchFilter::Failure => all_jobs
            .into_iter()
            .filter(|job| job.result == "testfailed" || job.result == "busted")
            .collect(),
        MatchFilter::Success => all_jobs
            .into_iter()
            .filter(|job| job.result == "success")
            .collect(),
        MatchFilter::All => all_jobs,
    };

    // Apply job name filter if provided
    if let Some(filter_pattern) = &args.filter {
        filtered_jobs.retain(|job| job.job_type_name.contains(filter_pattern));
    }

    if filtered_jobs.is_empty() {
        pb.finish_with_message("No jobs found matching criteria");
        println!("No jobs found matching the specified criteria");
        return Ok(());
    }

    pb.finish_with_message(format!("Found {} jobs matching criteria", filtered_jobs.len()));

    // Determine storage location for logs
    let (_temp_dir_guard, log_storage_path) = if args.fetch_logs {
        if let Some(cache_dir) = &args.cache_dir {
            // Use persistent cache directory
            let cache_path = PathBuf::from(cache_dir);
            fs::create_dir_all(&cache_path)?;
            (None, cache_path)
        } else {
            // Use temporary directory
            let temp_dir = TempDir::new()?;
            let temp_path = temp_dir.path().to_path_buf();
            (Some(temp_dir), temp_path)
        }
    } else {
        (None, PathBuf::from("/tmp"))
    };

    // Compile regex pattern if provided
    let pattern = if let Some(pattern_str) = &args.pattern {
        Some(Regex::new(pattern_str)?)
    } else {
        None
    };

    let client = Arc::new(client);

    if args.fetch_logs {
        // Create progress bar for log fetching
        let pb_logs = ProgressBar::new(filtered_jobs.len() as u64);
        pb_logs.set_style(
            ProgressStyle::default_bar()
                .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb_logs.set_message("Fetching and processing logs");

        let pb_logs = Arc::new(pb_logs);

        // Fetch jobs with full logs
        let jobs_with_logs: Vec<_> = stream::iter(filtered_jobs.clone())
            .map(|job| {
                let client = Arc::clone(&client);
                let repo = args.repo.clone();
                let pb_logs = Arc::clone(&pb_logs);
                let log_path = log_storage_path.clone();
                let pattern = pattern.as_ref();

                async move {
                    let result = fetch_job_with_full_logs(&client, &repo, job, &log_path, pattern).await;
                    pb_logs.inc(1);
                    result
                }
            })
            .buffer_unordered(5) // Limit concurrent requests to 5 for full log fetching
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|result| result.ok())
            .collect();

        pb_logs.finish_with_message("Completed fetching and processing logs");

        // Save metadata if using persistent cache
        if args.cache_dir.is_some() {
            let metadata = CachedPushMetadata {
                revision: revision.clone(),
                push_id,
                repo: args.repo.clone(),
                jobs: filtered_jobs.clone(),
            };
            save_cache_metadata(&log_storage_path, &metadata)?;
            println!("\nMetadata saved to: {}", log_storage_path.join("metadata.json").display());
        }

        let summary = format_markdown_summary(
            &revision,
            push_id,
            &jobs_with_logs,
            args.show_stack_traces,
            args.fetch_logs,
        );
        println!("{}", summary);

        if let Some(temp_dir) = _temp_dir_guard.as_ref() {
            println!(
                "\nLogs are stored in temporary directory: {}",
                temp_dir.path().display()
            );
            println!("The directory will be automatically cleaned up when the program exits.");
        } else if args.cache_dir.is_some() {
            println!(
                "\nLogs are stored persistently in: {}",
                log_storage_path.display()
            );
            println!("Use --use-cache --cache-dir {} to query these logs later.", log_storage_path.display());
        }
    } else {
        // Original behavior: fetch only error summaries
        let pb_jobs = ProgressBar::new(filtered_jobs.len() as u64);
        pb_jobs.set_style(
            ProgressStyle::default_bar()
                .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb_jobs.set_message("Fetching job details");

        let pb_jobs = Arc::new(pb_jobs);

        let jobs_with_errors: Vec<_> = stream::iter(filtered_jobs)
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
            .buffer_unordered(10)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|result| result.ok())
            .collect();

        pb_jobs.finish_with_message("Completed fetching job details");

        // Convert to JobWithLogs format
        let jobs_with_logs: Vec<_> = jobs_with_errors
            .into_iter()
            .map(|(job, errors)| JobWithLogs {
                job,
                errors,
                log_matches: vec![],
                log_dir: None,
            })
            .collect();

        let summary = format_markdown_summary(
            &revision,
            push_id,
            &jobs_with_logs,
            args.show_stack_traces,
            args.fetch_logs,
        );
        println!("{}", summary);
    }

    Ok(())
}
