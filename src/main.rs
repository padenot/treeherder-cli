mod api;
mod cache;
mod cli;
mod models;
mod output;
mod util;

use anyhow::Result;
use api::*;
use cache::*;
use clap::Parser;
use cli::{Args, MatchFilter};
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use models::*;
use output::*;
use regex::Regex;
use reqwest::Client;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use util::*;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();

    if !args.json && is_running_under_coding_agent() {
        args.json = true;
    }

    if !args.use_cache && args.input.is_none() && args.similar_history.is_none() {
        anyhow::bail!("INPUT is required when not using --use-cache or --similar-history");
    }

    if args.notify && !args.watch {
        anyhow::bail!("--notify requires --watch to be enabled");
    }

    if args.watch && args.use_cache {
        anyhow::bail!("--watch cannot be used with --use-cache");
    }

    if args.compare.is_some() && args.use_cache {
        anyhow::bail!("--compare cannot be used with --use-cache");
    }

    if args.compare.is_some() && args.watch {
        anyhow::bail!("--compare cannot be used with --watch");
    }

    if let Some(job_id) = args.similar_history {
        let client = Client::new();
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap(),
        );
        pb.set_message(format!("Fetching similar jobs for job {}", job_id));

        let history = fetch_similar_jobs(&client, &args.repo, job_id, args.similar_count).await?;

        pb.finish_with_message("Similar jobs fetched");

        if args.json {
            let json_output = format_similar_history_json(&history)?;
            println!("{}", json_output);
        } else {
            let markdown_output = format_similar_history_markdown(&history);
            println!("{}", markdown_output);
        }

        return Ok(());
    }

    if args.use_cache {
        let cache_dir = args
            .cache_dir
            .ok_or_else(|| anyhow::anyhow!("--use-cache requires --cache-dir to be specified"))?;
        let cache_path = PathBuf::from(&cache_dir);

        if !cache_path.exists() {
            anyhow::bail!("Cache directory does not exist: {}", cache_path.display());
        }

        println!("Loading cached data from: {}", cache_path.display());

        let metadata = load_cache_metadata(&cache_path)?;
        println!(
            "Push ID: {}, Revision: {}",
            metadata.push_id, metadata.revision
        );
        println!("Cached jobs: {}", metadata.jobs.len());

        let mut filtered_jobs = metadata.jobs.clone();

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

        if let Some(filter_pattern) = &args.filter {
            filtered_jobs.retain(|job| job.job_type_name.contains(filter_pattern));
        }

        if let Some(platform_pattern) = &args.platform {
            let platform_regex = Regex::new(platform_pattern)?;
            filtered_jobs.retain(|job| platform_regex.is_match(&job.platform));
        }

        if let Some(min_duration) = args.duration_min {
            filtered_jobs.retain(|job| job.duration.is_some_and(|d| d >= min_duration));
        }

        if !args.include_intermittent {
            filtered_jobs.retain(|job| job.failure_classification_id != Some(4));
        }

        println!("Jobs matching filter: {}", filtered_jobs.len());

        let pattern = if let Some(pattern_str) = &args.pattern {
            Some(Regex::new(pattern_str)?)
        } else {
            None
        };

        let jobs_with_logs = search_cached_logs(&cache_path, &filtered_jobs, pattern.as_ref())?;

        if args.group_by.is_some() {
            let grouped = group_failures_by_test(&jobs_with_logs);
            if args.json {
                let json_output =
                    format_grouped_json_output(&metadata.revision, metadata.push_id, &grouped)?;
                println!("{}", json_output);
            } else {
                let summary =
                    format_grouped_markdown_summary(&metadata.revision, metadata.push_id, &grouped);
                println!("{}", summary);
            }
        } else if args.json {
            let json_output =
                format_json_output(&metadata.revision, metadata.push_id, &jobs_with_logs)?;
            println!("{}", json_output);
        } else {
            let summary = format_markdown_summary(
                &metadata.revision,
                metadata.push_id,
                &jobs_with_logs,
                args.show_stack_traces,
                true,
            );
            println!("{}", summary);
        }

        return Ok(());
    }

    let client = Client::new();

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap(),
    );

    pb.set_message("Extracting revision from input");
    let input = args.input.as_ref().unwrap();
    let revision = extract_revision(input)?;

    pb.set_message("Fetching push ID");
    let push_id = fetch_push_id(&client, &args.repo, &revision).await?;

    if let Some(compare_revision_input) = &args.compare {
        pb.set_message("Comparison mode: fetching both revisions");

        let compare_revision = extract_revision(compare_revision_input)?;
        let compare_push_id = fetch_push_id(&client, &args.repo, &compare_revision).await?;

        pb.set_message("Fetching jobs for base revision");
        let base_jobs = fetch_jobs(&client, push_id).await?;

        pb.set_message("Fetching jobs for comparison revision");
        let compare_jobs = fetch_jobs(&client, compare_push_id).await?;

        let base_failed: Vec<_> = base_jobs
            .into_iter()
            .filter(|job| job.result == "testfailed" || job.result == "busted")
            .collect();

        let compare_failed: Vec<_> = compare_jobs
            .into_iter()
            .filter(|job| job.result == "testfailed" || job.result == "busted")
            .collect();

        let base_filtered: Vec<_> = if args.include_intermittent {
            base_failed
        } else {
            base_failed
                .into_iter()
                .filter(|job| job.failure_classification_id != Some(4))
                .collect()
        };

        let compare_filtered: Vec<_> = if args.include_intermittent {
            compare_failed
        } else {
            compare_failed
                .into_iter()
                .filter(|job| job.failure_classification_id != Some(4))
                .collect()
        };

        pb.set_message("Fetching error details for base revision");
        let client_arc = Arc::new(client);

        let pb_base = ProgressBar::new(base_filtered.len() as u64);
        pb_base.set_style(
            ProgressStyle::default_bar()
                .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb_base.set_message("Fetching base job errors");
        let pb_base = Arc::new(pb_base);

        let base_jobs_with_errors: Vec<_> = stream::iter(base_filtered)
            .map(|job| {
                let client = Arc::clone(&client_arc);
                let repo = args.repo.clone();
                let pb = Arc::clone(&pb_base);
                async move {
                    let result = fetch_job_details_with_errors(&client, &repo, job).await;
                    pb.inc(1);
                    result
                }
            })
            .buffer_unordered(10)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|result| result.ok())
            .collect();

        pb_base.finish_with_message("Done fetching base errors");

        let pb_compare = ProgressBar::new(compare_filtered.len() as u64);
        pb_compare.set_style(
            ProgressStyle::default_bar()
                .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb_compare.set_message("Fetching comparison job errors");
        let pb_compare = Arc::new(pb_compare);

        let compare_jobs_with_errors: Vec<_> = stream::iter(compare_filtered)
            .map(|job| {
                let client = Arc::clone(&client_arc);
                let repo = args.repo.clone();
                let pb = Arc::clone(&pb_compare);
                async move {
                    let result = fetch_job_details_with_errors(&client, &repo, job).await;
                    pb.inc(1);
                    result
                }
            })
            .buffer_unordered(10)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|result| result.ok())
            .collect();

        pb_compare.finish_with_message("Done fetching comparison errors");
        pb.finish_with_message("Comparison complete");

        let base_with_logs: Vec<_> = base_jobs_with_errors
            .into_iter()
            .map(|(job, errors)| JobWithLogs {
                job,
                errors,
                log_matches: vec![],
                log_dir: None,
            })
            .collect();

        let compare_with_logs: Vec<_> = compare_jobs_with_errors
            .into_iter()
            .map(|(job, errors)| JobWithLogs {
                job,
                errors,
                log_matches: vec![],
                log_dir: None,
            })
            .collect();

        let comparison_result = compare_failures(
            &base_with_logs,
            &compare_with_logs,
            &revision,
            &compare_revision,
            push_id,
            compare_push_id,
        );

        if args.json {
            let json_output = format_comparison_json(&comparison_result)?;
            println!("{}", json_output);
        } else {
            let markdown_output = format_comparison_markdown(&comparison_result);
            println!("{}", markdown_output);
        }

        return Ok(());
    }

    pb.set_message("Fetching jobs");
    let mut all_jobs = fetch_jobs(&client, push_id).await?;

    if args.watch {
        pb.finish_with_message("Watch mode: monitoring job progress");

        let watch_pb = ProgressBar::new_spinner();
        watch_pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap(),
        );

        while !are_all_jobs_complete(&all_jobs) {
            let (completed, running, pending) = count_job_states(&all_jobs);
            watch_pb.set_message(format!(
                "Jobs: {} completed, {} running, {} pending",
                completed, running, pending
            ));

            tokio::time::sleep(tokio::time::Duration::from_secs(args.watch_interval)).await;
            all_jobs = fetch_jobs(&client, push_id).await?;
        }

        watch_pb.finish_with_message("All jobs completed!");

        if args.notify {
            let (completed, _, _) = count_job_states(&all_jobs);
            let failed_count = all_jobs
                .iter()
                .filter(|j| j.result == "testfailed" || j.result == "busted")
                .count();

            let message = if failed_count > 0 {
                format!("{} of {} jobs failed", failed_count, completed)
            } else {
                format!("All {} jobs passed!", completed)
            };

            if let Err(e) = send_notification("Treeherder Jobs Complete", &message) {
                eprintln!("Failed to send notification: {}", e);
            }
        }
    }

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

    if let Some(filter_pattern) = &args.filter {
        filtered_jobs.retain(|job| job.job_type_name.contains(filter_pattern));
    }

    if let Some(platform_pattern) = &args.platform {
        let platform_regex = Regex::new(platform_pattern)?;
        filtered_jobs.retain(|job| platform_regex.is_match(&job.platform));
    }

    if let Some(min_duration) = args.duration_min {
        filtered_jobs.retain(|job| job.duration.is_some_and(|d| d >= min_duration));
    }

    if !args.include_intermittent {
        filtered_jobs.retain(|job| job.failure_classification_id != Some(4));
    }

    if filtered_jobs.is_empty() {
        pb.finish_with_message("No jobs found matching criteria");
        println!("No jobs found matching the specified criteria");
        return Ok(());
    }

    pb.finish_with_message(format!(
        "Found {} jobs matching criteria",
        filtered_jobs.len()
    ));

    let (_temp_dir_guard, log_storage_path) = if args.fetch_logs {
        if let Some(cache_dir) = &args.cache_dir {
            let cache_path = PathBuf::from(cache_dir);
            fs::create_dir_all(&cache_path)?;
            (None, cache_path)
        } else {
            let temp_dir = TempDir::new()?;
            let temp_path = temp_dir.path().to_path_buf();
            (Some(temp_dir), temp_path)
        }
    } else {
        (None, PathBuf::from("/tmp"))
    };

    let pattern = if let Some(pattern_str) = &args.pattern {
        Some(Regex::new(pattern_str)?)
    } else {
        None
    };

    let client = Arc::new(client);

    if args.fetch_logs {
        let pb_logs = ProgressBar::new(filtered_jobs.len() as u64);
        pb_logs.set_style(
            ProgressStyle::default_bar()
                .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb_logs.set_message("Fetching and processing logs");

        let pb_logs = Arc::new(pb_logs);

        let jobs_with_logs: Vec<_> = stream::iter(filtered_jobs.clone())
            .map(|job| {
                let client = Arc::clone(&client);
                let repo = args.repo.clone();
                let pb_logs = Arc::clone(&pb_logs);
                let log_path = log_storage_path.clone();
                let pattern = pattern.as_ref();

                async move {
                    let result =
                        fetch_job_with_full_logs(&client, &repo, job, &log_path, pattern).await;
                    pb_logs.inc(1);
                    result
                }
            })
            .buffer_unordered(5)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|result| result.ok())
            .collect();

        pb_logs.finish_with_message("Completed fetching and processing logs");

        if args.cache_dir.is_some() {
            let metadata = CachedPushMetadata {
                revision: revision.clone(),
                push_id,
                repo: args.repo.clone(),
                jobs: filtered_jobs.clone(),
            };
            save_cache_metadata(&log_storage_path, &metadata)?;
            if !args.json {
                println!(
                    "\nMetadata saved to: {}",
                    log_storage_path.join("metadata.json").display()
                );
            }
        }

        if args.group_by.is_some() {
            let grouped = group_failures_by_test(&jobs_with_logs);
            if args.json {
                let json_output = format_grouped_json_output(&revision, push_id, &grouped)?;
                println!("{}", json_output);
            } else {
                let summary = format_grouped_markdown_summary(&revision, push_id, &grouped);
                println!("{}", summary);
            }
        } else if args.json {
            let json_output = format_json_output(&revision, push_id, &jobs_with_logs)?;
            println!("{}", json_output);
        } else {
            let summary = format_markdown_summary(
                &revision,
                push_id,
                &jobs_with_logs,
                args.show_stack_traces,
                args.fetch_logs,
            );
            println!("{}", summary);
        }

        if !args.json {
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
                println!(
                    "Use --use-cache --cache-dir {} to query these logs later.",
                    log_storage_path.display()
                );
            }
        }
    } else if args.download_artifacts {
        let artifact_dir = if let Some(cache_dir) = &args.cache_dir {
            let cache_path = PathBuf::from(cache_dir);
            fs::create_dir_all(&cache_path)?;
            cache_path
        } else {
            let dir = PathBuf::from(format!("artifacts-{}", revision));
            fs::create_dir_all(&dir)?;
            dir
        };

        let artifact_pattern = if let Some(pattern_str) = &args.artifact_pattern {
            Some(Regex::new(pattern_str)?)
        } else {
            None
        };

        let pb_artifacts = ProgressBar::new(filtered_jobs.len() as u64);
        pb_artifacts.set_style(
            ProgressStyle::default_bar()
                .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb_artifacts.set_message("Downloading artifacts");

        let pb_artifacts = Arc::new(pb_artifacts);

        let all_downloaded: Vec<_> = stream::iter(filtered_jobs)
            .map(|job| {
                let client = Arc::clone(&client);
                let repo = args.repo.clone();
                let pb = Arc::clone(&pb_artifacts);
                let output_dir = artifact_dir.clone();
                let pattern = artifact_pattern.as_ref();

                async move {
                    let result =
                        download_job_artifacts(&client, &repo, &job, &output_dir, pattern).await;
                    pb.inc(1);
                    result
                }
            })
            .buffer_unordered(3)
            .collect::<Vec<_>>()
            .await;

        pb_artifacts.finish_with_message("Completed downloading artifacts");

        let total_files: usize = all_downloaded
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|v| v.len())
            .sum();

        if args.json {
            let output = serde_json::json!({
                "revision": revision,
                "push_id": push_id,
                "artifact_dir": artifact_dir.display().to_string(),
                "total_files": total_files,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("\n## Artifacts Downloaded\n");
            println!("**Revision:** `{}`", revision);
            println!("**Output directory:** `{}`", artifact_dir.display());
            println!("**Total files:** {}", total_files);
        }
    } else if args.perf {
        let pb_perf = ProgressBar::new(filtered_jobs.len() as u64);
        pb_perf.set_style(
            ProgressStyle::default_bar()
                .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb_perf.set_message("Fetching performance data");

        let pb_perf = Arc::new(pb_perf);

        let perf_data: Vec<_> = stream::iter(filtered_jobs)
            .map(|job| {
                let client = Arc::clone(&client);
                let repo = args.repo.clone();
                let pb = Arc::clone(&pb_perf);

                async move {
                    let result = fetch_job_perf_data(&client, &repo, &job).await;
                    pb.inc(1);
                    result
                }
            })
            .buffer_unordered(5)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|r| r.ok())
            .collect();

        pb_perf.finish_with_message("Completed fetching performance data");

        if args.json {
            let json_output = format_perf_json(&revision, push_id, &perf_data)?;
            println!("{}", json_output);
        } else {
            let markdown_output = format_perf_markdown(&revision, push_id, &perf_data);
            println!("{}", markdown_output);
        }
    } else {
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

        let jobs_with_logs: Vec<_> = jobs_with_errors
            .into_iter()
            .map(|(job, errors)| JobWithLogs {
                job,
                errors,
                log_matches: vec![],
                log_dir: None,
            })
            .collect();

        if args.group_by.is_some() {
            let grouped = group_failures_by_test(&jobs_with_logs);
            if args.json {
                let json_output = format_grouped_json_output(&revision, push_id, &grouped)?;
                println!("{}", json_output);
            } else {
                let summary = format_grouped_markdown_summary(&revision, push_id, &grouped);
                println!("{}", summary);
            }
        } else if args.json {
            let json_output = format_json_output(&revision, push_id, &jobs_with_logs)?;
            println!("{}", json_output);
        } else {
            let summary = format_markdown_summary(
                &revision,
                push_id,
                &jobs_with_logs,
                args.show_stack_traces,
                args.fetch_logs,
            );
            println!("{}", summary);
        }
    }

    Ok(())
}
