mod api;
mod cache;
mod cli;
mod history;
mod models;
mod output;
mod range;
mod util;

use anyhow::Result;
use api::*;
use cache::*;
use clap::Parser;
use cli::{Args, MatchFilter};
use futures::stream::{self, StreamExt};
use history::*;
use indicatif::{ProgressBar, ProgressStyle};
use models::*;
use output::*;
use range::*;
use regex::Regex;
use reqwest::Client;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use util::*;

fn is_llm_environment() -> bool {
    std::env::var("CLAUDECODE").is_ok()
        || std::env::var("CODEX_SANDBOX").is_ok()
        || std::env::var("GEMINI_CLI").is_ok()
        || std::env::var("OPENCODE").is_ok()
}

fn print_llm_help() {
    print!(
        r#"treeherder-cli: Firefox CI results from Treeherder
INPUT: revision hash|Treeherder URL|Lando commit ID (numeric or URL with ?landoCommitID=N)
--repo <R> try(default)|autoland|mozilla-central|...
--json output JSON|--watch poll until complete|--stream-failures print failures as they appear
--notify desktop notification on completion (requires --watch or --stream-failures)
--watch-interval <N> poll interval seconds (default 300)
--filter <regex> filter by job name|--platform <regex> filter by platform
--include-intermittent include intermittent failures
--group-by test group failures by test name across platforms
--compare <REV> show only failures not in REV
--range <A..B>|--from <A> --to <B>|--lookback <N> analyze a range of pushes
--suspects infer candidate push windows for first observed failures in a range
--show-stack-traces show crash stack traces|--all-crash-threads all threads|--full-stack registers+annotations
--fetch-logs download full logs|--pattern <regex> search logs|--match-filter failure|success|all
--cache-dir <DIR> store logs|--use-cache read from cache (no download)
--download-artifacts download job artifacts|--artifact-pattern <regex>
--perf show performance/resource data
--similar-history <job-id> job history via similar_jobs API|--similar-count <N> (default 50)
--group-history <manifest> pass/fail of a test manifest per push before INPUT|--lookback <N> (default 50, max 300)
  fetches newest first and stops after the last pass; widen --lookback only on "failure predates window"
--test <regex> restrict --suspects to matching test names
--duration-min <N> only jobs longer than N seconds
--context <N> show N pushes before and after this push (for bisecting autoland failures)
Question -> flags:
  did job X run on this push?            REV --filter X --match-filter all --json
  did manifest M run / pass on this push? REV --group-history M --lookback 0
  when did manifest M last pass?          REV --group-history M --lookback 150 [--filter X]
  is this failure older than my window?   look for "failure predates window" in --suspects or --group-history
  which pushes could have caused test T?  REV --lookback 20 --suspects --test T
  is job X intermittent?                  --similar-history <job-id> (shows revision, date, classification name)
Ex: treeherder-cli a13b9fc22101|treeherder-cli 12345 --stream-failures|treeherder-cli a13b9fc22101 --json
Ex: treeherder-cli a13b9fc22101 --filter mochitest --platform linux|treeherder-cli a13b9fc22101 --compare b2c3d4e5
Ex: treeherder-cli a13b9fc22101 --repo autoland --context 5
Ex: treeherder-cli --repo autoland --range good..bad --suspects --json
Ex: treeherder-cli f9baddcc4cdc --repo autoland --group-history docshell/test/unit/xpcshell.toml --lookback 150
"#
    );
}

const MAX_GROUP_HISTORY_LOOKBACK: u64 = 300;
const DEFAULT_GROUP_HISTORY_LOOKBACK: u64 = 50;
const GROUP_HISTORY_CONCURRENCY: usize = 20;

fn has_range_request(args: &Args) -> bool {
    args.group_history.is_none()
        && (args.range.is_some()
            || args.from.is_some()
            || args.to.is_some()
            || args.lookback.is_some())
}

fn validate_range_args(args: &Args) -> Result<()> {
    if args.test.is_some() && !args.suspects {
        anyhow::bail!("--test requires --suspects");
    }
    if args.group_history.is_some() {
        if args.input.is_none() {
            anyhow::bail!("--group-history requires INPUT as the ending revision");
        }
        if args.range.is_some() || args.from.is_some() || args.to.is_some() {
            anyhow::bail!(
                "--group-history takes its window from --lookback, not --range/--from/--to"
            );
        }
        if args
            .lookback
            .is_some_and(|n| n > MAX_GROUP_HISTORY_LOOKBACK)
        {
            anyhow::bail!(
                "--lookback for --group-history is capped at {}",
                MAX_GROUP_HISTORY_LOOKBACK
            );
        }
    }
    if args.range.is_some() && (args.from.is_some() || args.to.is_some()) {
        anyhow::bail!("--range cannot be combined with --from or --to");
    }
    if args.from.is_some() != args.to.is_some() {
        anyhow::bail!("--from and --to must be used together");
    }
    if args.lookback.is_some() && (args.range.is_some() || args.from.is_some() || args.to.is_some())
    {
        anyhow::bail!("--lookback cannot be combined with --range, --from, or --to");
    }
    if args.lookback.is_some() && args.input.is_none() {
        anyhow::bail!("--lookback requires INPUT as the ending revision");
    }
    if has_range_request(args) {
        if args.use_cache {
            anyhow::bail!("range analysis cannot be used with --use-cache");
        }
        if args.watch {
            anyhow::bail!("range analysis cannot be used with --watch");
        }
        if args.stream_failures {
            anyhow::bail!("range analysis cannot be used with --stream-failures");
        }
        if args.compare.is_some() {
            anyhow::bail!("range analysis cannot be used with --compare");
        }
        if args.fetch_logs {
            anyhow::bail!("range analysis cannot be used with --fetch-logs yet");
        }
        if args.download_artifacts {
            anyhow::bail!("range analysis cannot be used with --download-artifacts yet");
        }
        if args.perf {
            anyhow::bail!("range analysis cannot be used with --perf yet");
        }
        if args.group_by.is_some() {
            anyhow::bail!("range analysis cannot be used with --group-by yet");
        }
    }
    Ok(())
}

async fn resolve_push_input(client: &Client, repo: &str, input: &str) -> Result<PushRef> {
    let revision = if let Some(lando_commit_id) = extract_lando_commit_id(input) {
        let lando_instance = extract_lando_instance(input);
        fetch_revision_from_lando(client, lando_instance.as_deref(), lando_commit_id).await?
    } else {
        extract_revision(input)?
    };

    fetch_push(client, repo, &revision).await
}

async fn resolve_range_pushes(client: &Client, repo: &str, args: &Args) -> Result<Vec<PushRef>> {
    if let Some(lookback) = args.lookback {
        let input = args.input.as_ref().unwrap();
        let end_push = resolve_push_input(client, repo, input).await?;
        return fetch_push_window_ending_at(client, repo, &end_push, lookback).await;
    }

    let (from, to) = if let Some(range) = &args.range {
        parse_revision_range(range)?
    } else {
        (
            args.from.as_ref().unwrap().clone(),
            args.to.as_ref().unwrap().clone(),
        )
    };

    let start_push = resolve_push_input(client, repo, &from).await?;
    let end_push = resolve_push_input(client, repo, &to).await?;
    fetch_pushes_between(client, repo, start_push.id, end_push.id).await
}

/// Job filters shared by every mode: --filter, --platform, --duration-min, --include-intermittent.
struct JobSelector {
    filter: Option<Regex>,
    platform: Option<Regex>,
    duration_min: Option<u64>,
    include_intermittent: bool,
}

impl JobSelector {
    fn from_args(args: &Args) -> Result<Self> {
        Ok(Self {
            filter: args.filter.as_deref().map(Regex::new).transpose()?,
            platform: args.platform.as_deref().map(Regex::new).transpose()?,
            duration_min: args.duration_min,
            include_intermittent: args.include_intermittent,
        })
    }

    fn has_name_filter(&self) -> bool {
        self.filter.is_some() || self.platform.is_some()
    }

    fn matches_name(&self, job: &Job) -> bool {
        self.filter
            .as_ref()
            .is_none_or(|regex| regex.is_match(&job.job_type_name))
            && self
                .platform
                .as_ref()
                .is_none_or(|regex| regex.is_match(&job.platform))
    }

    fn matches(&self, job: &Job) -> bool {
        self.matches_name(job)
            && self
                .duration_min
                .is_none_or(|min| job.duration.is_some_and(|d| d >= min))
            && (self.include_intermittent || job.failure_classification_id != Some(4))
    }
}

fn matches_result(job: &Job, match_filter: &MatchFilter) -> bool {
    match match_filter {
        MatchFilter::Failure => job.result == "testfailed" || job.result == "busted",
        MatchFilter::Success => job.result == "success",
        MatchFilter::All => true,
    }
}

fn describe_matched_jobs(matched: &[&Job], selector: &JobSelector) -> String {
    if matched.is_empty() {
        return "filter matched no jobs on this push".to_string();
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut hidden_intermittent = 0;
    for job in matched {
        if selector.matches(job) {
            *counts.entry(job.result.as_str()).or_default() += 1;
        } else if job.failure_classification_id == Some(4) {
            hidden_intermittent += 1;
        }
    }
    let mut parts: Vec<_> = counts.into_iter().collect();
    parts.sort_by_key(|(result, count)| (Reverse(*count), *result));
    let mut summary: Vec<String> = parts
        .into_iter()
        .map(|(result, count)| format!("{} {}", count, result))
        .collect();
    if hidden_intermittent > 0 {
        summary.push(format!(
            "{} intermittent (hidden; --include-intermittent)",
            hidden_intermittent
        ));
    }
    format!(
        "filter matched {} job{}: {}",
        matched.len(),
        if matched.len() == 1 { "" } else { "s" },
        summary.join(", ")
    )
}

fn filter_push_jobs(
    push_jobs: &[PushJobs],
    selector: &JobSelector,
    match_filter: Option<MatchFilter>,
) -> Vec<PushJobs> {
    push_jobs
        .iter()
        .map(|push_jobs| {
            let jobs = push_jobs
                .jobs
                .iter()
                .filter(|job| {
                    match_filter
                        .as_ref()
                        .is_none_or(|match_filter| matches_result(job, match_filter))
                        && selector.matches(job)
                })
                .cloned()
                .collect();

            PushJobs {
                push: push_jobs.push.clone(),
                jobs,
            }
        })
        .collect()
}

async fn fetch_observations_for_jobs(
    client: Arc<Client>,
    repo: &str,
    jobs: Vec<(PushRef, Job)>,
    message: &'static str,
) -> Result<Vec<JobObservation>> {
    let pb = ProgressBar::new(jobs.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );
    pb.set_message(message);
    let pb = Arc::new(pb);

    let observations = stream::iter(jobs)
        .map(|(push, job)| {
            let client = Arc::clone(&client);
            let repo = repo.to_string();
            let pb = Arc::clone(&pb);
            async move {
                let result = fetch_job_details_with_errors(&client, &repo, job).await;
                pb.inc(1);
                result.map(|(job, errors)| JobObservation { push, job, errors })
            }
        })
        .buffer_unordered(10)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .filter_map(|result| match result {
            Ok(observation) => Some(observation),
            Err(err) => {
                eprintln!("Failed to fetch job details: {}", err);
                None
            }
        })
        .collect();

    pb.finish_with_message("Completed fetching job details");
    Ok(observations)
}

fn failed_jobs_from_pushes(push_jobs: &[PushJobs]) -> Vec<(PushRef, Job)> {
    push_jobs
        .iter()
        .flat_map(|push_jobs| {
            push_jobs
                .jobs
                .iter()
                .filter(|job| job.result == "testfailed" || job.result == "busted")
                .cloned()
                .map(|job| (push_jobs.push.clone(), job))
        })
        .collect()
}

fn all_jobs_from_pushes(push_jobs: &[PushJobs]) -> Vec<(PushRef, Job)> {
    push_jobs
        .iter()
        .flat_map(|push_jobs| {
            push_jobs
                .jobs
                .iter()
                .cloned()
                .map(|job| (push_jobs.push.clone(), job))
        })
        .collect()
}

async fn run_range_mode(client: Client, repo: String, args: &Args, pb: ProgressBar) -> Result<()> {
    pb.set_message("Resolving range");
    let pushes = resolve_range_pushes(&client, &repo, args).await?;
    if pushes.is_empty() {
        pb.finish_with_message("No pushes found in range");
        println!("No pushes found in range");
        return Ok(());
    }

    pb.set_message(format!("Fetching jobs for {} pushes", pushes.len()));
    let push_jobs = fetch_jobs_by_push(&client, &pushes).await?;
    pb.finish_with_message("Fetched range jobs");

    let client = Arc::new(client);
    let selector = JobSelector::from_args(args)?;

    if args.suspects {
        let filtered_push_jobs = filter_push_jobs(&push_jobs, &selector, None);
        let failed_jobs = failed_jobs_from_pushes(&filtered_push_jobs);
        let observations =
            fetch_observations_for_jobs(client, &repo, failed_jobs, "Fetching failed job details")
                .await?;
        let mut analysis = analyze_range_suspects(&repo, &filtered_push_jobs, &observations);
        if let Some(test_regex) = args.test.as_deref().map(Regex::new).transpose()? {
            analysis.suspects.retain(|suspect| {
                suspect
                    .failure_key
                    .test
                    .as_deref()
                    .is_some_and(|test| test_regex.is_match(test))
            });
        }

        if args.json {
            println!("{}", format_range_suspects_json(&analysis)?);
        } else {
            println!("{}", format_range_suspects_markdown(&analysis));
        }
    } else {
        let filtered_push_jobs =
            filter_push_jobs(&push_jobs, &selector, Some(args.match_filter.clone()));
        let jobs = all_jobs_from_pushes(&filtered_push_jobs);
        if jobs.is_empty() {
            println!("No jobs found matching the specified criteria");
            return Ok(());
        }

        let observations =
            fetch_observations_for_jobs(client, &repo, jobs, "Fetching range job details").await?;
        let pushes = filtered_push_jobs
            .iter()
            .map(|push_jobs| {
                let jobs = observations
                    .iter()
                    .filter(|observation| observation.push.id == push_jobs.push.id)
                    .map(|observation| JobWithLogs {
                        job: observation.job.clone(),
                        errors: observation.errors.clone(),
                        log_matches: vec![],
                        log_dir: None,
                    })
                    .collect();

                RangePushSummary {
                    push: push_jobs.push.clone(),
                    jobs,
                }
            })
            .collect();
        let summary = RangeJobSummary { repo, pushes };

        if args.json {
            println!("{}", format_range_json(&summary)?);
        } else {
            println!("{}", format_range_markdown_summary(&summary));
        }
    }

    Ok(())
}

async fn run_group_history_mode(
    client: Client,
    repo: String,
    manifest: &str,
    args: &Args,
    pb: ProgressBar,
) -> Result<()> {
    let selector = JobSelector::from_args(args)?;
    let lookback = args.lookback.unwrap_or(DEFAULT_GROUP_HISTORY_LOOKBACK);

    pb.set_message("Resolving push window");
    let end_push = resolve_push_input(&client, &repo, args.input.as_ref().unwrap()).await?;
    let pushes = if lookback == 0 {
        vec![end_push]
    } else {
        fetch_push_window_ending_at(&client, &repo, &end_push, lookback).await?
    };

    let window_pushes = pushes.len();
    let mut newest_first = pushes;
    newest_first.sort_by_key(|push| Reverse(push.id));

    let client = Arc::new(client);
    let selector = Arc::new(selector);
    let mut fetches = stream::iter(newest_first.into_iter().enumerate())
        .map(|(index, push)| {
            let client = Arc::clone(&client);
            let repo = repo.clone();
            let selector = Arc::clone(&selector);
            async move {
                let allowed_tasks = if selector.has_name_filter() {
                    Some(
                        fetch_jobs(&client, push.id)
                            .await?
                            .into_iter()
                            .filter(|job| selector.matches_name(job))
                            .filter_map(|job| job.task_id)
                            .collect::<HashSet<_>>(),
                    )
                } else {
                    None
                };
                let results = fetch_group_results(&client, &repo, &push.revision).await?;
                Ok::<_, anyhow::Error>((
                    index,
                    PushGroupResults {
                        push,
                        results,
                        allowed_tasks,
                    },
                ))
            }
        })
        .buffer_unordered(GROUP_HISTORY_CONCURRENCY);

    let mut slots: Vec<Option<GroupHistoryPush>> = vec![None; window_pushes];
    let mut settled_prefix = 0;
    while let Some(result) = fetches.next().await {
        let (index, push) = result?;
        slots[index] = Some(summarize_push(manifest, push));
        while slots.get(settled_prefix).is_some_and(Option::is_some) {
            settled_prefix += 1;
        }
        pb.set_message(format!(
            "Fetched group results for {} of {} pushes",
            settled_prefix, window_pushes
        ));
        if is_settled(slots[..settled_prefix].iter().flatten()) {
            break;
        }
    }
    drop(fetches);
    pb.finish_and_clear();

    let entries: Vec<GroupHistoryPush> = slots.drain(..settled_prefix).flatten().collect();
    let history = build_group_history(manifest, entries, window_pushes);

    if args.json {
        println!("{}", format_group_history_json(&history)?);
    } else {
        println!("{}", format_group_history_markdown(&history));
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let version_checker =
        moz_cli_version_check::VersionChecker::new("treeherder-cli", env!("CARGO_PKG_VERSION"));
    version_checker.check_async();

    if is_llm_environment() && std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        print_llm_help();
        version_checker.print_warning();
        return Ok(());
    }

    let result = run().await;

    version_checker.print_warning();

    result
}

async fn run() -> Result<()> {
    let mut args = Args::parse();
    let match_filter_was_explicit = std::env::args_os()
        .any(|arg| arg == "--match-filter" || arg.to_string_lossy().starts_with("--match-filter="));
    validate_range_args(&args)?;

    if !args.use_cache
        && args.input.is_none()
        && args.similar_history.is_none()
        && args.group_history.is_none()
        && !has_range_request(&args)
    {
        anyhow::bail!(
            "INPUT is required when not using --use-cache, --similar-history, or range analysis"
        );
    }

    if args.notify && !args.watch && !args.stream_failures {
        anyhow::bail!("--notify requires --watch or --stream-failures to be enabled");
    }

    if args.watch && args.use_cache {
        anyhow::bail!("--watch cannot be used with --use-cache");
    }

    if args.stream_failures && args.use_cache {
        anyhow::bail!("--stream-failures cannot be used with --use-cache");
    }

    if args.compare.is_some() && args.use_cache {
        anyhow::bail!("--compare cannot be used with --use-cache");
    }

    if args.compare.is_some() && args.watch {
        anyhow::bail!("--compare cannot be used with --watch");
    }

    if let Some(job_id) = args.similar_history {
        let client = build_client()?;
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap(),
        );
        pb.set_message(format!("Fetching similar jobs for job {}", job_id));

        let history = fetch_similar_jobs(
            &client,
            args.repo.as_deref().unwrap_or("try"),
            job_id,
            args.similar_count,
        )
        .await?;

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
            .as_ref()
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

        let selector = JobSelector::from_args(&args)?;
        let filtered_jobs: Vec<Job> = metadata
            .jobs
            .iter()
            .filter(|job| matches_result(job, &args.match_filter) && selector.matches(job))
            .cloned()
            .collect();

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
            let json_output = format_json_output(
                &metadata.revision,
                metadata.push_id,
                jobs_with_logs.len(),
                &jobs_with_logs,
            )?;
            println!("{}", json_output);
        } else {
            let summary = format_markdown_summary(
                &metadata.revision,
                metadata.push_id,
                &jobs_with_logs,
                args.show_stack_traces,
                args.all_crash_threads,
                args.full_stack,
                true,
            );
            println!("{}", summary);
        }

        return Ok(());
    }

    let client = build_client()?;

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap(),
    );

    pb.set_message("Resolving push");
    if args.repo.is_none() {
        if let Some(input) = args.input.as_ref() {
            if let Some(repo_from_url) = extract_repo_from_url(input) {
                args.repo = Some(repo_from_url);
            }
        }
    }
    let repo = args.repo.clone().unwrap_or_else(|| "try".to_string());

    if let Some(manifest) = &args.group_history {
        return run_group_history_mode(client, repo, manifest, &args, pb).await;
    }

    if has_range_request(&args) {
        return run_range_mode(client, repo, &args, pb).await;
    }

    let selector = JobSelector::from_args(&args)?;

    let input = args.input.as_ref().unwrap();
    let (revision, push_ids) = if let Some(lando_commit_id) = extract_lando_commit_id(input) {
        let lando_instance = extract_lando_instance(input);
        let revision =
            fetch_revision_from_lando(&client, lando_instance.as_deref(), lando_commit_id).await?;
        let push_id = fetch_push_id(&client, &repo, &revision).await?;
        (revision, vec![push_id])
    } else {
        let revision = extract_revision(input)?;
        let push_id = fetch_push_id(&client, &repo, &revision).await?;
        (revision, vec![push_id])
    };
    let push_id = push_ids[0];

    if let Some(context) = args.context {
        pb.set_message("Fetching surrounding pushes");
        let (before, after) = fetch_pushes_around(&client, &repo, push_id, context).await?;
        pb.finish_and_clear();

        let base_url = format!("https://treeherder.mozilla.org/jobs?repo={}", repo);

        println!(
            "## Pushes around {}",
            &revision[..std::cmp::min(12, revision.len())]
        );
        println!();

        let mut after_sorted = after;
        after_sorted.sort_by_key(|p| p.id);
        for p in &after_sorted {
            println!(
                "  after  {} (push {})  {}&revision={}",
                &p.revision[..std::cmp::min(12, p.revision.len())],
                p.id,
                base_url,
                p.revision
            );
        }
        println!(
            "> this   {} (push {})",
            &revision[..std::cmp::min(12, revision.len())],
            push_id
        );
        let mut before_sorted = before;
        before_sorted.sort_by_key(|p| Reverse(p.id));
        for p in &before_sorted {
            println!(
                "  before {} (push {})  {}&revision={}",
                &p.revision[..std::cmp::min(12, p.revision.len())],
                p.id,
                base_url,
                p.revision
            );
        }
        println!();
        return Ok(());
    }

    if args.stream_failures {
        pb.finish_and_clear();
        let client = Arc::new(client);
        let mut reported_ids: HashSet<u64> = HashSet::new();

        loop {
            let all_jobs = fetch_jobs_multi(&client, &push_ids).await?;

            let newly_failed: Vec<Job> = all_jobs
                .iter()
                .filter(|j| {
                    (j.result == "testfailed" || j.result == "busted")
                        && !reported_ids.contains(&j.id)
                        && (args.include_intermittent || j.failure_classification_id != Some(4))
                })
                .cloned()
                .collect();

            for job in newly_failed {
                reported_ids.insert(job.id);
                match fetch_job_details_with_errors(&client, &repo, job).await {
                    Ok((job, errors)) => {
                        let jwl = JobWithLogs {
                            job,
                            errors,
                            log_matches: vec![],
                            log_dir: None,
                        };
                        if args.json {
                            println!("{}", serde_json::to_string(&jwl)?);
                        } else {
                            let summary = format_markdown_summary(
                                &revision,
                                push_id,
                                &[jwl],
                                args.show_stack_traces,
                                args.all_crash_threads,
                                args.full_stack,
                                false,
                            );
                            print!("{}", summary);
                        }
                        std::io::stdout().flush()?;
                    }
                    Err(e) => eprintln!("Failed to fetch details for job: {}", e),
                }
            }

            if are_all_jobs_complete(&all_jobs) {
                if args.notify {
                    let failed_count = all_jobs
                        .iter()
                        .filter(|j| j.result == "testfailed" || j.result == "busted")
                        .count();
                    let message = if failed_count > 0 {
                        format!("{} of {} jobs failed", failed_count, all_jobs.len())
                    } else {
                        format!("All {} jobs passed!", all_jobs.len())
                    };
                    if let Err(e) = send_notification("Treeherder Jobs Complete", &message) {
                        eprintln!("Failed to send notification: {}", e);
                    }
                }
                break;
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(args.watch_interval)).await;
        }

        return Ok(());
    }

    if let Some(compare_revision_input) = &args.compare {
        pb.set_message("Comparison mode: fetching both revisions");

        let compare_revision = extract_revision(compare_revision_input)?;
        let compare_push_id = fetch_push_id(&client, &repo, &compare_revision).await?;

        pb.set_message("Fetching jobs for base revision");
        let base_jobs = fetch_jobs_multi(&client, &push_ids).await?;

        pb.set_message("Fetching jobs for comparison revision");
        let compare_jobs = fetch_jobs(&client, compare_push_id).await?;

        let base_filtered: Vec<_> = base_jobs
            .into_iter()
            .filter(|job| matches_result(job, &MatchFilter::Failure) && selector.matches(job))
            .collect();

        let compare_filtered: Vec<_> = compare_jobs
            .into_iter()
            .filter(|job| matches_result(job, &MatchFilter::Failure) && selector.matches(job))
            .collect();

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
                let repo = repo.clone();
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
                let repo = repo.clone();
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
    let mut all_jobs = fetch_jobs_multi(&client, &push_ids).await?;

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
            all_jobs = fetch_jobs_multi(&client, &push_ids).await?;
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

    let effective_match_filter = if args.download_artifacts && !match_filter_was_explicit {
        MatchFilter::All
    } else {
        args.match_filter.clone()
    };

    let success_count = all_jobs
        .iter()
        .filter(|job| job.result == "success")
        .count();
    let success_platforms: HashSet<_> = all_jobs
        .iter()
        .filter(|job| job.result == "success")
        .map(|job| job.platform.clone())
        .collect();

    let name_matched: Vec<&Job> = all_jobs
        .iter()
        .filter(|job| selector.matches_name(job))
        .collect();
    let matched_jobs = name_matched.len();
    let filtered_summary = describe_matched_jobs(&name_matched, &selector);

    let filtered_jobs: Vec<Job> = all_jobs
        .iter()
        .filter(|job| matches_result(job, &effective_match_filter) && selector.matches(job))
        .cloned()
        .collect();

    if filtered_jobs.is_empty() {
        pb.finish_with_message("No jobs found matching criteria");
        if args.json {
            println!(
                "{}",
                format_json_output(&revision, push_id, matched_jobs, &[])?
            );
        } else if selector.has_name_filter() {
            println!("{}", filtered_summary);
        } else if matches!(effective_match_filter, MatchFilter::Failure) && success_count > 0 {
            println!(
                "{} passing job{} across {} platform{}, no failures found",
                success_count,
                if success_count == 1 { "" } else { "s" },
                success_platforms.len(),
                if success_platforms.len() == 1 {
                    ""
                } else {
                    "s"
                }
            );
        } else {
            println!("No jobs found matching the specified criteria");
        }
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
        let pb_details = ProgressBar::new(filtered_jobs.len() as u64);
        pb_details.set_style(
            ProgressStyle::default_bar()
                .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb_details.set_message("Fetching job details");
        let pb_details = Arc::new(pb_details);

        let job_and_details: Vec<_> = stream::iter(filtered_jobs.clone())
            .map(|job| {
                let client = Arc::clone(&client);
                let repo = repo.clone();
                let pb = Arc::clone(&pb_details);
                async move {
                    let result = fetch_job_details(&client, &repo, job.id).await;
                    pb.inc(1);
                    result.map(|d| (job, d))
                }
            })
            .buffer_unordered(50)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|r| r.ok())
            .collect();

        pb_details.finish_with_message("Fetched job details");

        let pb_logs = ProgressBar::new(job_and_details.len() as u64);
        pb_logs.set_style(
            ProgressStyle::default_bar()
                .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb_logs.set_message("Fetching and processing logs");
        let pb_logs = Arc::new(pb_logs);

        let pattern = pattern.as_ref();

        let jobs_with_logs: Vec<_> = stream::iter(job_and_details)
            .map(|(job, detail)| {
                let client = Arc::clone(&client);
                let pb_logs = Arc::clone(&pb_logs);
                let log_path = log_storage_path.clone();
                async move {
                    let result =
                        fetch_job_with_full_logs(&client, job, detail, &log_path, pattern).await;
                    pb_logs.inc(1);
                    result
                }
            })
            .buffer_unordered(50)
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
                repo: repo.clone(),
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
            let json_output =
                format_json_output(&revision, push_id, matched_jobs, &jobs_with_logs)?;
            println!("{}", json_output);
        } else {
            let summary = format_markdown_summary(
                &revision,
                push_id,
                &jobs_with_logs,
                args.show_stack_traces,
                args.all_crash_threads,
                args.full_stack,
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
                let repo = repo.clone();
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
                let repo = repo.clone();
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
                let repo = repo.clone();
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
            let json_output =
                format_json_output(&revision, push_id, matched_jobs, &jobs_with_logs)?;
            println!("{}", json_output);
        } else {
            let summary = format_markdown_summary(
                &revision,
                push_id,
                &jobs_with_logs,
                args.show_stack_traces,
                args.all_crash_threads,
                args.full_stack,
                args.fetch_logs,
            );
            println!("{}", summary);
        }
    }

    Ok(())
}

fn build_client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("treeherder-cli/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(name: &str, platform: &str, result: &str, classification: Option<u64>) -> Job {
        Job {
            id: 1,
            job_type_name: name.to_string(),
            job_type_symbol: String::new(),
            platform: platform.to_string(),
            platform_option: String::new(),
            result: result.to_string(),
            state: "completed".to_string(),
            failure_classification_id: classification,
            failure_classification: None,
            duration: None,
            task_id: None,
        }
    }

    fn selector(filter: Option<&str>, platform: Option<&str>) -> JobSelector {
        JobSelector {
            filter: filter.map(|f| Regex::new(f).unwrap()),
            platform: platform.map(|p| Regex::new(p).unwrap()),
            duration_min: None,
            include_intermittent: false,
        }
    }

    #[test]
    fn filter_is_a_regex() {
        let selector = selector(Some("xpcshell|mochitest-plain"), None);
        assert!(selector.matches_name(&job("test-linux/opt-xpcshell", "linux", "success", None)));
        assert!(selector.matches_name(&job(
            "test-linux/opt-mochitest-plain-1",
            "linux",
            "success",
            None
        )));
        assert!(!selector.matches_name(&job("test-linux/opt-reftest", "linux", "success", None)));
    }

    #[test]
    fn matched_jobs_summary_counts_results() {
        let selector = selector(Some("xpcshell"), None);
        let jobs = [
            job("a-xpcshell", "linux", "success", Some(1)),
            job("b-xpcshell", "linux", "success", Some(1)),
            job("c-xpcshell", "linux", "testfailed", Some(4)),
        ];
        let matched: Vec<&Job> = jobs.iter().collect();
        assert_eq!(
            describe_matched_jobs(&matched, &selector),
            "filter matched 3 jobs: 2 success, 1 intermittent (hidden; --include-intermittent)"
        );
        assert_eq!(
            describe_matched_jobs(&[], &selector),
            "filter matched no jobs on this push"
        );
    }
}
