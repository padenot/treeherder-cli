mod actions;
mod api;
mod auth;
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
use clap::{parser::ValueSource, CommandFactory, FromArgMatches};
use cli::{Args, Command, MatchFilter};
use futures::stream::{self, StreamExt};
use history::*;
use indicatif::{ProgressBar, ProgressStyle};
use models::*;
use output::*;
use range::*;
use regex::Regex;
use reqwest::Client;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use util::*;

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
login (human, opens a browser): ! treeherder-cli login [--level 3 for autoland]|status|logout
--cancel-push whole push|--cancel pending/running jobs matching --filter/--platform (one is required)
--retrigger jobs selected by --filter/--platform/--match-filter (default failures and exceptions)|--retrigger-failures only failed jobs|--add-jobs <regex> task labels runnable on the push
--times <N> runs per job (--allow-many-runs above 100 total runs or cancels)|--dry-run list what would be triggered|with --watch: wait for decision task, action task, then jobs
Question -> flags:
  did job X run on this push?            REV --filter X --match-filter all --json
  did manifest M run / pass on this push? REV --group-history M --lookback 0
  when did manifest M last pass?          REV --group-history M --lookback 150 [--filter X]
  is this failure older than my window?   look for "failure predates window" in --suspects or --group-history
  which pushes could have caused test T?  REV --lookback 20 --suspects --test T
  is job X intermittent?                  --similar-history <job-id> (shows revision, date, classification name)
  retrigger failures to check them?       REV --retrigger-failures --times 5 --dry-run, then without --dry-run
  run more jobs on my push?               REV --add-jobs 'regex' --dry-run, then without --dry-run
  stop my push?                           REV --cancel-push
  "Not logged in" / login expired?        ask the user to run: ! treeherder-cli login
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

fn has_action_request(args: &Args) -> bool {
    args.cancel || args.cancel_push || requests_scheduling(args)
}

fn requests_scheduling(args: &Args) -> bool {
    args.retrigger || args.retrigger_failures || args.add_jobs.is_some()
}

fn validate_action_args(args: &Args, matches: &clap::ArgMatches) -> Result<()> {
    let schedules_jobs = requests_scheduling(args);
    let explicit = |id: &str| matches.value_source(id) == Some(ValueSource::CommandLine);
    if !schedules_jobs && explicit("times") {
        anyhow::bail!("--times requires --retrigger, --retrigger-failures or --add-jobs");
    }
    if !schedules_jobs && !args.cancel && explicit("allow_many_runs") {
        anyhow::bail!(
            "--allow-many-runs requires --cancel, --retrigger, --retrigger-failures or --add-jobs"
        );
    }
    if !has_action_request(args) {
        if args.dry_run {
            anyhow::bail!(
                "--dry-run requires --cancel, --cancel-push, --retrigger, --retrigger-failures or --add-jobs"
            );
        }
        return Ok(());
    }

    for (flag, pattern) in [
        ("--filter", &args.filter),
        ("--platform", &args.platform),
        ("--add-jobs", &args.add_jobs),
    ] {
        if pattern
            .as_deref()
            .is_some_and(|pattern| pattern.trim().is_empty())
        {
            anyhow::bail!("{flag} requires a nonempty regex for Taskcluster actions");
        }
    }
    let action = [
        ("--cancel", args.cancel),
        ("--cancel-push", args.cancel_push),
        ("--retrigger", args.retrigger),
        ("--retrigger-failures", args.retrigger_failures),
        ("--add-jobs", args.add_jobs.is_some()),
    ]
    .into_iter()
    .find_map(|(flag, enabled)| enabled.then_some(flag))
    .unwrap();
    let conflicts: Vec<_> = [
        "artifact_pattern",
        "cache_dir",
        "compare",
        "context",
        "download_artifacts",
        "fetch_logs",
        "from",
        "full_stack",
        "group_by",
        "group_history",
        "lookback",
        "pattern",
        "perf",
        "range",
        "show_stack_traces",
        "similar_count",
        "similar_history",
        "stream_failures",
        "suspects",
        "test",
        "to",
        "use_cache",
        "all_crash_threads",
    ]
    .into_iter()
    .filter(|id| explicit(id))
    .map(|id| format!("--{}", id.replace('_', "-")))
    .collect();
    if !conflicts.is_empty() {
        anyhow::bail!(
            "{action} cannot be combined with {}. Run inspection options in a separate command.",
            conflicts.join(", ")
        );
    }
    let match_filter_was_explicit = explicit("match_filter");
    if args.cancel && args.filter.is_none() && args.platform.is_none() {
        anyhow::bail!(
            "--cancel needs --filter and/or --platform to select jobs; use --cancel-push to cancel the whole push"
        );
    }
    if args.cancel_push
        && (args.filter.is_some()
            || args.platform.is_some()
            || args.duration_min.is_some()
            || args.include_intermittent
            || match_filter_was_explicit)
    {
        anyhow::bail!(
            "--cancel-push takes no job selectors. Use --cancel with --filter or --platform to select jobs."
        );
    }
    if args.add_jobs.is_some()
        && (args.filter.is_some()
            || args.platform.is_some()
            || args.duration_min.is_some()
            || args.include_intermittent
            || match_filter_was_explicit)
    {
        anyhow::bail!(
            "--add-jobs selects jobs using its task-label regex. Remove the other job selectors."
        );
    }
    if args.cancel && match_filter_was_explicit {
        anyhow::bail!(
            "--cancel selects pending and running jobs and does not accept --match-filter"
        );
    }
    anyhow::ensure!(args.input.is_some(), "{action} requires INPUT (the push)");
    Ok(())
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

async fn resolve_revision(client: &Client, input: &str) -> Result<String> {
    if let Some(lando_commit_id) = extract_lando_commit_id(input) {
        let lando_instance = extract_lando_instance(input);
        fetch_revision_from_lando(client, lando_instance.as_deref(), lando_commit_id).await
    } else {
        extract_revision(input)
    }
}

async fn resolve_push_input(client: &Client, repo: &str, input: &str) -> Result<PushRef> {
    let revision = resolve_revision(client, input).await?;
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

fn matches_retrigger_result(job: &Job, args: &Args) -> bool {
    matches_result(job, &args.match_filter)
        || (args.retrigger
            && matches!(args.match_filter, MatchFilter::Failure)
            && job.result == "exception")
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

    let mut argv = std::env::args_os().skip(1);
    if is_llm_environment()
        && argv
            .next()
            .is_some_and(|arg| arg == "--help" || arg == "-h")
        && argv.next().is_none()
    {
        print_llm_help();
        version_checker.print_warning();
        return Ok(());
    }

    let result = run().await;

    version_checker.print_warning();

    result
}

async fn run() -> Result<()> {
    let matches = Args::command().get_matches();
    let match_filter_was_explicit =
        matches.value_source("match_filter") == Some(ValueSource::CommandLine);
    let mut args = Args::from_arg_matches(&matches).unwrap_or_else(|err| err.exit());
    if let Some(command) = args.command.take() {
        return match command {
            Command::Login { level, days } => auth::login(level, days).await,
            Command::Logout => auth::logout(),
            Command::Status => auth::status().await,
        };
    }
    validate_action_args(&args, &matches)?;
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

    if has_action_request(&args) {
        return run_action_mode(&client, &repo, &args, &selector, &pb).await;
    }

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

const PUSH_INGESTION_POLL: Duration = Duration::from_secs(5);
const DECISION_TASK_POLL: Duration = Duration::from_secs(30);
const ACTION_TASK_POLL: Duration = Duration::from_secs(10);
const ACTION_SETUP_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const ACTION_TASK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DEFAULT_RUN_LIMIT: u64 = 100;

fn jobs_count(count: usize) -> String {
    format!("{} job{}", count, if count == 1 { "" } else { "s" })
}

fn planned_run_count(labels: usize, times: u32, reruns: usize) -> Result<u64> {
    (labels as u64)
        .checked_mul(u64::from(times))
        .and_then(|runs| runs.checked_add(reruns as u64))
        .ok_or_else(|| anyhow::anyhow!("The plan contains too many runs to count"))
}

fn short_revision(revision: &str) -> &str {
    &revision[..std::cmp::min(12, revision.len())]
}

async fn wait_for_push(
    client: &Client,
    repo: &str,
    revision: &str,
    watch: bool,
    pb: &ProgressBar,
) -> Result<PushRef> {
    loop {
        if let Some(push) = find_action_push(client, repo, revision).await? {
            return Ok(push);
        }
        if !watch {
            anyhow::bail!(
                "Treeherder has no push {} on {}. If it was just pushed, ingestion takes ~10 s: retry shortly, or add --watch to wait.",
                short_revision(revision),
                repo
            );
        }
        pb.set_message("Waiting for Treeherder to ingest the push");
        tokio::time::sleep(PUSH_INGESTION_POLL).await;
    }
}

/// Actions are defined by the decision task's actions.json, so they are only
/// available once it has completed.
async fn wait_for_decision_task(
    client: &Client,
    repo: &str,
    push: &PushRef,
    watch: bool,
    pb: &ProgressBar,
) -> Result<actions::DecisionRun> {
    let mut seen_unindexed = false;
    loop {
        let state = if let Some(task_id) =
            actions::find_decision_task(client, repo, &push.revision).await?
        {
            if let Some(decision) = actions::fetch_decision_run(client, task_id).await? {
                return Ok(decision);
            }
            "latest run is in progress".to_string()
        } else {
            let jobs = fetch_jobs(client, push.id).await?;
            // A retried decision task has a job per run; the latest one counts.
            let decision_job = jobs
                .iter()
                .filter(|job| {
                    job.job_type_symbol == "D" && job.job_type_name.ends_with("Decision Task")
                })
                .max_by_key(|job| job.id);
            match decision_job {
                Some(job) if job.state != "completed" => job.state.clone(),
                Some(job) if job.result == "retry" => "retrying".to_string(),
                // The index entry appears moments after completion. When it
                // doesn't, the push is not on a Firefox (gecko) repository.
                Some(job) if job.result == "success" => {
                    anyhow::ensure!(
                        watch && !seen_unindexed,
                        "The decision task of push {} on {repo} completed, but Taskcluster has no gecko.v2.{repo} index entry for it: actions only support Firefox (gecko) repositories.{}",
                        short_revision(&push.revision),
                        if watch {
                            ""
                        } else {
                            " If the decision task just completed, retry in a few seconds."
                        }
                    );
                    seen_unindexed = true;
                    "completed, waiting for the Taskcluster index".to_string()
                }
                Some(job) => anyhow::bail!(
                    "The decision task of push {} on {} finished with result '{}': no actions are available on this push.",
                    short_revision(&push.revision),
                    repo,
                    job.result
                ),
                None => "not scheduled yet".to_string(),
            }
        };
        if !watch {
            anyhow::bail!(
                "The decision task of push {} on {} hasn't completed (state: {}). Actions become available once it completes, usually a few minutes after pushing; add --watch to wait.",
                short_revision(&push.revision),
                repo,
                state
            );
        }
        pb.set_message(format!("Waiting for the decision task ({})", state));
        tokio::time::sleep(DECISION_TASK_POLL).await;
    }
}

struct PlannedTrigger<'a> {
    action: &'a actions::Action,
    task_id: Option<String>,
    input: serde_json::Value,
}

impl PlannedTrigger<'_> {
    /// Whether the action creates tasks, rather than acting on existing ones.
    fn creates_tasks(&self) -> bool {
        matches!(
            self.action.name.as_str(),
            "add-new-jobs" | "retrigger-multiple"
        )
    }
}

struct SubmittedAction {
    id: String,
    target: Option<String>,
    creates_tasks: bool,
}

#[derive(Default)]
struct RetriggerSelection {
    clone_labels: BTreeSet<String>,
    rerun_jobs: BTreeMap<String, String>,
    skipped_labels: BTreeSet<String>,
}

/// Sorts jobs by their label's `retrigger` tag, which the graph transforms set
/// on every task: "true" labels are cloned, "false" tasks are rerun in place.
/// Tasks without the tag, like decision and action tasks, are skipped.
fn classify_retrigger_jobs<'a>(
    jobs: impl IntoIterator<Item = &'a Job>,
    label_tags: &HashMap<String, HashMap<String, String>>,
) -> RetriggerSelection {
    let mut selection = RetriggerSelection::default();
    for job in jobs {
        let label = &job.job_type_name;
        let retrigger = label_tags
            .get(label)
            .and_then(|tags| tags.get("retrigger"))
            .map(String::as_str);
        match (retrigger, &job.task_id) {
            (Some("true"), _) => {
                selection.clone_labels.insert(label.clone());
            }
            (Some("false"), Some(id)) => {
                selection.rerun_jobs.insert(id.clone(), label.clone());
            }
            _ => {
                selection.skipped_labels.insert(label.clone());
            }
        }
    }
    selection
}

#[derive(Default, serde::Serialize)]
struct WatchSummary {
    completed: usize,
    failed: usize,
    exception: usize,
    canceled: usize,
    reruns_not_started: BTreeSet<String>,
}

impl WatchSummary {
    fn record(&mut self, status: &actions::TaskStatus) {
        match status.state {
            actions::TaskState::Completed => self.completed += 1,
            actions::TaskState::Failed => self.failed += 1,
            actions::TaskState::Exception if status.was_canceled() => self.canceled += 1,
            actions::TaskState::Exception => self.exception += 1,
            _ => {}
        }
    }

    fn message(&self, cancellation: bool) -> String {
        if cancellation {
            return format!(
                "Taskcluster: {} canceled, {} finished before cancellation ({} completed, {} failed), {} other exceptions",
                self.canceled,
                self.completed + self.failed,
                self.completed,
                self.failed,
                self.exception
            );
        }
        let mut message = format!(
            "Taskcluster: {} completed, {} failed, {} exceptions, {} canceled",
            self.completed, self.failed, self.exception, self.canceled
        );
        if !self.reruns_not_started.is_empty() {
            message.push_str(&format!(
                ", {} requested reruns did not start",
                self.reruns_not_started.len()
            ));
        }
        message
    }
}

/// Resolves each action task to Ok once it completes, or to why it did not.
async fn wait_for_action_tasks<'a>(
    queue: &taskcluster::Queue,
    tasks: &'a [SubmittedAction],
) -> Result<Vec<(&'a SubmittedAction, Result<()>)>> {
    let deadline = tokio::time::Instant::now() + ACTION_TASK_TIMEOUT;
    let mut waiting: Vec<&SubmittedAction> = tasks.iter().collect();
    let mut results = Vec::new();
    loop {
        let states: HashMap<String, actions::TaskState> =
            actions::fetch_task_statuses(queue, waiting.iter().map(|task| task.id.as_str()))
                .await?
                .into_iter()
                .map(|status| (status.task_id, status.state))
                .collect();
        waiting.retain(|task| {
            let state = states[&task.id];
            if state == actions::TaskState::Completed {
                results.push((*task, Ok(())));
            } else if state.is_terminal() {
                results.push((
                    *task,
                    Err(anyhow::anyhow!(
                        "Action task {} ended in {state:?}. {TC_ROOT_URL}/tasks/{}",
                        task.id,
                        task.id
                    )),
                ));
            } else {
                return true;
            }
            false
        });
        if waiting.is_empty() {
            return Ok(results);
        }
        if tokio::time::Instant::now() >= deadline {
            results.extend(waiting.into_iter().map(|task| {
                let timeout = anyhow::anyhow!("Timed out waiting for action task {}", task.id);
                (task, Err(timeout))
            }));
            return Ok(results);
        }
        tokio::time::sleep(ACTION_TASK_POLL).await;
    }
}

struct ActionWatch<'a> {
    client: &'a Client,
    decision_task_id: &'a str,
    action_tasks: &'a [SubmittedAction],
    targets: BTreeSet<String>,
    previous_reruns: HashMap<String, usize>,
    interval: Duration,
    pb: &'a ProgressBar,
}

impl ActionWatch<'_> {
    async fn run(
        mut self,
        queue: &taskcluster::Queue,
        errors: &mut Vec<String>,
    ) -> Result<Option<WatchSummary>> {
        self.pb.set_message("Waiting for Taskcluster action tasks");
        let mut succeeded = Vec::new();
        for (task, result) in wait_for_action_tasks(queue, self.action_tasks).await? {
            match result {
                Ok(()) => succeeded.push(task),
                Err(err) => {
                    if let Some(target) = &task.target {
                        self.targets.remove(target);
                        self.previous_reruns.remove(target);
                    }
                    errors.push(format!("{err:#}"));
                }
            }
        }
        if succeeded.is_empty() {
            return Ok(None);
        }

        for task in succeeded.into_iter().filter(|task| task.creates_tasks) {
            match self.find_created_tasks(queue, &task.id).await {
                Ok(ids) => self.targets.extend(ids),
                Err(err) => errors.push(format!("Action task {}: {err:#}", task.id)),
            }
        }

        let mut summary = WatchSummary::default();
        let statuses =
            actions::fetch_task_statuses(queue, self.previous_reruns.keys().map(String::as_str))
                .await?;
        for status in statuses {
            if status.reruns() > self.previous_reruns[&status.task_id] {
                if status.state.is_terminal() {
                    summary.record(&status);
                } else {
                    self.targets.insert(status.task_id);
                }
            } else {
                summary.reruns_not_started.insert(status.task_id);
            }
        }
        while !self.targets.is_empty() {
            self.pb
                .set_message(format!("Watching {} Taskcluster tasks", self.targets.len()));
            let statuses =
                actions::fetch_task_statuses(queue, self.targets.iter().map(String::as_str))
                    .await?;
            for status in statuses
                .into_iter()
                .filter(|status| status.state.is_terminal())
            {
                self.targets.remove(&status.task_id);
                summary.record(&status);
            }
            if !self.targets.is_empty() {
                tokio::time::sleep(self.interval).await;
            }
        }
        Ok(Some(summary))
    }

    async fn find_created_tasks(
        &self,
        queue: &taskcluster::Queue,
        action_task_id: &str,
    ) -> Result<BTreeSet<String>> {
        let created = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                if let Some(created) =
                    actions::fetch_created_tasks(self.client, action_task_id).await?
                {
                    return Ok::<_, anyhow::Error>(created);
                }
                tokio::time::sleep(ACTION_TASK_POLL).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("task-graph.json did not become available"))??;
        tokio::time::timeout(
            Duration::from_secs(120),
            self.find_created_copies(queue, action_task_id, created),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Taskcluster did not list all created copies"))?
    }

    async fn find_created_copies(
        &self,
        queue: &taskcluster::Queue,
        action_task_id: &str,
        mut created: actions::CreatedTasks,
    ) -> Result<BTreeSet<String>> {
        while !created.all_listed() {
            self.pb.set_message(format!(
                "Waiting for copies from action task {action_task_id}"
            ));
            let group = actions::fetch_task_group(queue, self.decision_task_id).await?;
            created.include_group_children(group, action_task_id);
            if !created.all_listed() {
                tokio::time::sleep(ACTION_TASK_POLL).await;
            }
        }
        Ok(created.ids)
    }
}

/// --cancel / --cancel-push / --retrigger / --add-jobs: resolves the push's in-tree actions, triggers them,
/// and with --watch waits for the affected Taskcluster tasks.
async fn run_action_mode(
    client: &Client,
    repo: &str,
    args: &Args,
    selector: &JobSelector,
    pb: &ProgressBar,
) -> Result<()> {
    let resolve = async {
        let input = args.input.as_ref().unwrap();
        if let Some(input_repo) = extract_repo_from_url(input) {
            anyhow::ensure!(
                input_repo == repo,
                "The input URL points to repository {input_repo}, but --repo specifies {repo}"
            );
        }
        let revision = resolve_revision(client, input).await?;
        validate_action_target(repo, &revision)?;
        let revision = revision.to_ascii_lowercase();
        let push = wait_for_push(client, repo, &revision, args.watch, pb).await?;
        let decision = wait_for_decision_task(client, repo, &push, args.watch, pb).await?;
        Ok::<_, anyhow::Error>((push, decision))
    };
    let (push, decision) = tokio::time::timeout(ACTION_SETUP_TIMEOUT, resolve)
        .await
        .map_err(|_| anyhow::anyhow!("Timed out resolving the push and its decision task"))??;
    let decision_task_id = decision.task_id.as_str();

    pb.set_message("Fetching jobs and actions");
    let (jobs, actions, runnable_labels) = tokio::try_join!(
        async {
            if args.cancel || args.retrigger || args.retrigger_failures {
                fetch_jobs(client, push.id).await
            } else {
                Ok(vec![])
            }
        },
        actions::fetch_actions(client, &decision),
        async {
            match args.add_jobs {
                Some(_) => actions::fetch_runnable_labels(client, &decision).await,
                None => Ok(vec![]),
            }
        }
    )?;

    // The SCM level comes from actions.json alone, so the login is checked
    // before the per-task work.
    let possible_actions: &[&str] = if args.add_jobs.is_some() {
        &["add-new-jobs"]
    } else if args.cancel_push {
        &["cancel-all"]
    } else if args.cancel {
        &["cancel", "cancel-all"]
    } else {
        &["retrigger-multiple", "rerun"]
    };
    let level = actions.level(possible_actions)?;
    let credentials = if args.dry_run {
        None
    } else {
        Some(pb.suspend(|| auth::require_credentials(level))?)
    };
    let queue = taskcluster::Queue::new(taskcluster::ClientBuilder::new(TC_ROOT_URL))?;

    let mut skipped_labels = Vec::new();
    let mut rerun_labels = BTreeSet::new();
    let mut rerun_task_ids = Vec::new();
    let mut skipped_rerun_task_ids = Vec::new();
    let mut previous_reruns = HashMap::new();
    let mut watch_targets = BTreeSet::new();
    let (summary, labels, planned, planned_runs) = if let Some(pattern) = &args.add_jobs {
        let regex = Regex::new(pattern)?;
        let labels: Vec<String> = runnable_labels
            .into_iter()
            .filter(|label| regex.is_match(label))
            .collect();
        if labels.is_empty() {
            anyhow::bail!(
                "No task label of this push matches --add-jobs '{}'",
                pattern
            );
        }
        let planned_runs = planned_run_count(labels.len(), args.times, 0)?;
        let planned = vec![PlannedTrigger {
            action: actions.find("add-new-jobs", None)?,
            task_id: None,
            input: serde_json::json!({ "tasks": labels, "times": args.times }),
        }];
        (
            format!("add {} ×{}", jobs_count(labels.len()), args.times),
            labels,
            planned,
            Some(planned_runs),
        )
    } else if args.cancel_push {
        let planned = vec![PlannedTrigger {
            action: actions.find("cancel-all", None)?,
            task_id: None,
            input: serde_json::json!({}),
        }];
        ("cancel all jobs".to_string(), vec![], planned, None)
    } else if args.cancel {
        let unfinished = |job: &&Job| job.state == "pending" || job.state == "running";
        let mut seen = HashSet::new();
        let targets: Vec<(&Job, &str)> = jobs
            .iter()
            .filter(|job| unfinished(job) && selector.matches(job))
            .filter_map(|job| Some((job, job.task_id.as_deref()?)))
            .filter(|(_, task_id)| seen.insert(*task_id))
            .collect();
        if targets.is_empty() {
            anyhow::bail!("No pending or running jobs match --filter/--platform on this push");
        }
        // One cancel-all action replaces a cancel action per job when the
        // selection covers every pending and running task of the push. It also
        // cancels the unscheduled tasks: they wait on the selected ones and could
        // not run anyway. Treeherder's view is checked first, as listing the
        // task group is expensive.
        let cancel_all = targets.len() > 1
            && jobs
                .iter()
                .filter(unfinished)
                .all(|job| job.task_id.as_deref().is_some_and(|id| seen.contains(id)))
            && {
                pb.set_message("Checking the push for other unfinished tasks");
                let group = actions::fetch_task_group(&queue, decision_task_id).await?;
                let in_group: HashSet<&str> = group
                    .iter()
                    .map(|task| task.status.task_id.as_str())
                    .collect();
                seen.is_subset(&in_group)
                    && group.iter().all(|task| {
                        !matches!(
                            task.status.state,
                            actions::TaskState::Pending | actions::TaskState::Running
                        ) || seen.contains(task.status.task_id.as_str())
                    })
            };
        let planned = if cancel_all {
            vec![PlannedTrigger {
                action: actions.find("cancel-all", None)?,
                task_id: None,
                input: serde_json::json!({}),
            }]
        } else {
            pb.set_message(format!(
                "Fetching task tags for {}",
                jobs_count(targets.len())
            ));
            let tags = actions::fetch_task_tags(&queue, targets.iter().map(|(_, id)| *id)).await?;
            targets
                .iter()
                .map(|(_, task_id)| {
                    Ok(PlannedTrigger {
                        action: actions.find("cancel", Some(&tags[*task_id]))?,
                        task_id: Some(task_id.to_string()),
                        input: serde_json::json!({}),
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };
        let labels = targets
            .iter()
            .map(|(job, _)| job.job_type_name.clone())
            .collect();
        watch_targets.extend(targets.iter().map(|(_, id)| id.to_string()));
        let mut summary = format!("cancel {}", jobs_count(targets.len()));
        if cancel_all {
            summary.push_str(
                " (every pending and running task of the push, with one cancel-all action)",
            );
        }
        (summary, labels, planned, None)
    } else {
        let candidates: Vec<_> = jobs
            .iter()
            .filter(|job| matches_retrigger_result(job, args) && selector.matches(job))
            .collect();
        if candidates.is_empty() {
            anyhow::bail!(
                "No jobs match the retrigger selection on this push (intermittents need --include-intermittent)"
            );
        }
        // Tasks of a label share its tags, so one task per label is enough.
        let label_tasks: BTreeMap<&str, &str> = candidates
            .iter()
            .filter_map(|job| Some((job.job_type_name.as_str(), job.task_id.as_deref()?)))
            .collect();
        pb.set_message(format!(
            "Fetching task tags for {} labels",
            label_tasks.len()
        ));
        let tags = actions::fetch_task_tags(&queue, label_tasks.values().copied()).await?;
        let label_tags: HashMap<String, HashMap<String, String>> = label_tasks
            .into_iter()
            .map(|(label, task_id)| (label.to_string(), tags[task_id].clone()))
            .collect();
        let RetriggerSelection {
            clone_labels,
            rerun_jobs,
            skipped_labels: mut skipped,
        } = classify_retrigger_jobs(candidates, &label_tags);
        let mut planned = Vec::new();
        if !clone_labels.is_empty() {
            planned.push(PlannedTrigger {
                action: actions.find("retrigger-multiple", None)?,
                task_id: None,
                input: serde_json::json!({ "requests": [{ "tasks": clone_labels, "times": args.times }] }),
            });
        }
        if !rerun_jobs.is_empty() {
            pb.set_message("Checking selected task runs");
            let mut statuses =
                actions::fetch_task_statuses(&queue, rerun_jobs.keys().map(String::as_str)).await?;
            statuses.sort_by(|a, b| a.task_id.cmp(&b.task_id));
            for status in statuses {
                let label = &rerun_jobs[&status.task_id];
                if status.can_rerun() {
                    if args.watch {
                        previous_reruns.insert(status.task_id.clone(), status.reruns());
                    }
                    rerun_labels.insert(label.clone());
                    rerun_task_ids.push(status.task_id.clone());
                    planned.push(PlannedTrigger {
                        action: actions.find("rerun", Some(&label_tags[label]))?,
                        task_id: Some(status.task_id),
                        input: serde_json::json!({}),
                    });
                } else {
                    skipped.insert(label.clone());
                    skipped_rerun_task_ids.push(status.task_id);
                }
            }
        }
        let labels: Vec<_> = clone_labels.union(&rerun_labels).cloned().collect();
        skipped_labels = skipped.difference(&rerun_labels).cloned().collect();
        if labels.is_empty() {
            anyhow::bail!(
                "None of the matching jobs can be cloned or rerun. Skipped: {}",
                skipped_labels.join(", ")
            );
        }
        let mut operations = Vec::new();
        if !clone_labels.is_empty() {
            operations.push(format!(
                "retrigger {} ×{}",
                jobs_count(clone_labels.len()),
                args.times
            ));
        }
        if !rerun_task_ids.is_empty() {
            operations.push(format!(
                "rerun {} in place",
                jobs_count(rerun_task_ids.len())
            ));
        }
        let planned_runs = planned_run_count(clone_labels.len(), args.times, rerun_task_ids.len())?;
        (
            operations.join(" and "),
            labels,
            planned,
            Some(planned_runs),
        )
    };

    // Each per-job cancel is an action task, so they count against the limit too.
    let limited = planned_runs.map(|runs| (runs, "runs")).or_else(|| {
        args.cancel
            .then_some((planned.len() as u64, "cancel actions"))
    });
    let runs_over_limit =
        limited.filter(|&(count, _)| count > DEFAULT_RUN_LIMIT && !args.allow_many_runs);
    if let Some((count, what)) = runs_over_limit.filter(|_| !args.dry_run) {
        anyhow::bail!(
            "The plan requests {count} {what}, exceeding the default limit of {DEFAULT_RUN_LIMIT}.{} Use --dry-run to inspect it or pass --allow-many-runs to proceed.",
            if args.cancel {
                " --cancel-push cancels the whole push with a single action."
            } else {
                ""
            }
        );
    }

    let mut action_names: Vec<&str> = planned.iter().map(|p| p.action.name.as_str()).collect();
    action_names.dedup();
    actions::verify_action_hooks(client, decision_task_id, planned.iter().map(|p| p.action))
        .await?;
    let payloads = planned
        .iter()
        .map(|p| {
            p.action.render_payload(
                &actions.variables,
                decision_task_id,
                p.task_id.as_deref(),
                p.input.clone(),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let push_url = format!(
        "https://treeherder.mozilla.org/jobs?repo={}&revision={}",
        repo, push.revision
    );

    let mut action_tasks = Vec::new();
    let mut trigger_errors = Vec::new();
    if let Some(credentials) = credentials {
        if args.watch && args.cancel_push {
            pb.set_message("Finding unfinished tasks in the group");
            let group = actions::fetch_task_group(&queue, decision_task_id).await?;
            anyhow::ensure!(
                !group.is_empty(),
                "Taskcluster returned an empty decision task group"
            );
            watch_targets.extend(
                group
                    .into_iter()
                    .map(|task| task.status)
                    .filter(|status| !status.state.is_terminal())
                    .map(|status| status.task_id),
            );
        }
        let fix_hint = credentials.fix_hint(level);
        // A timeout can mean the hook was accepted. Never retry these requests.
        let hooks = taskcluster::Hooks::new(
            taskcluster::ClientBuilder::new(TC_ROOT_URL)
                .credentials(credentials.into_credentials())
                .retry(taskcluster::Retry {
                    retries: 0,
                    ..Default::default()
                }),
        )?;
        pb.set_message(format!("Triggering {}", action_names.join(", ")));
        let mut results = stream::iter(planned.iter().zip(&payloads))
            .map(|(plan, payload)| {
                let hooks = &hooks;
                let fix_hint = fix_hint.as_str();
                async move {
                    let result = actions::trigger(hooks, plan.action, payload, fix_hint).await;
                    (plan, result)
                }
            })
            .buffered(actions::ACTION_CONCURRENCY);
        while let Some((plan, result)) = results.next().await {
            match result {
                Ok(id) => action_tasks.push(SubmittedAction {
                    id,
                    target: plan.task_id.clone(),
                    creates_tasks: plan.creates_tasks(),
                }),
                Err(err) => {
                    if let Some(target) = plan.task_id.as_deref() {
                        watch_targets.remove(target);
                        previous_reruns.remove(target);
                        trigger_errors.push(format!("Target {target}: {err:#}"));
                    } else {
                        trigger_errors.push(format!("{err:#}"));
                    }
                }
            }
        }
    }

    let action_task_ids: Vec<_> = action_tasks.iter().map(|task| &task.id).collect();
    if !args.json {
        pb.suspend(|| {
            if args.dry_run {
                println!(
                    "Dry run: would {} on {} {}",
                    summary,
                    repo,
                    short_revision(&push.revision)
                );
            } else {
                println!(
                    "Requested: {} on {} {}",
                    summary,
                    repo,
                    short_revision(&push.revision)
                );
            }
            if let Some(runs) = planned_runs {
                println!("Planned runs: {runs}");
            }
            if runs_over_limit.is_some() {
                println!("This plan requires --allow-many-runs to submit.");
            }
            for label in &labels {
                if rerun_labels.contains(label) {
                    println!("  {label} (rerun existing task)");
                } else {
                    println!("  {label}");
                }
            }
            for label in &skipped_labels {
                println!("  Skipped: {label}");
            }
            for id in &rerun_task_ids {
                println!("  Rerun target: {TC_ROOT_URL}/tasks/{id}");
            }
            for id in &skipped_rerun_task_ids {
                println!("  Skipped ineligible rerun target: {TC_ROOT_URL}/tasks/{id}");
            }
            for task_id in &action_task_ids {
                println!("Action task: {}/tasks/{}", TC_ROOT_URL, task_id);
            }
            println!("Push: {}", push_url);
            if !action_task_ids.is_empty() && !args.watch {
                println!(
                "The action task does the work; add --watch to wait for it and the resulting jobs."
            );
            }
        });
    }

    let mut watch_summary = None;
    let mut watch_errors = Vec::new();
    if args.watch && !action_tasks.is_empty() {
        let watch = ActionWatch {
            client,
            decision_task_id,
            action_tasks: &action_tasks,
            targets: watch_targets,
            previous_reruns,
            interval: Duration::from_secs(args.watch_interval.max(1)),
            pb,
        };
        let result = tokio::select! {
            result = watch.run(&queue, &mut watch_errors) => result,
            signal = tokio::signal::ctrl_c() => signal
                .map_err(anyhow::Error::from)
                .and_then(|()| Err(anyhow::anyhow!("Stopped watching; submitted actions may still run"))),
        };
        match result {
            Ok(Some(summary)) => {
                let message = summary.message(args.cancel || args.cancel_push);
                if !args.json {
                    pb.suspend(|| {
                        println!("{message}");
                        for id in &summary.reruns_not_started {
                            println!("Rerun did not start: {TC_ROOT_URL}/tasks/{id}");
                        }
                    });
                }
                if args.notify {
                    if let Err(err) = send_notification("Treeherder Action Complete", &message) {
                        eprintln!("Failed to send notification: {err}");
                    }
                }
                watch_summary = Some(summary);
            }
            Ok(None) => {}
            Err(err) => watch_errors.push(format!("{err:#}")),
        }
    }
    let watch_error = (!watch_errors.is_empty()).then(|| watch_errors.join("; "));

    pb.finish_and_clear();
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "actions": action_names,
                "repo": repo,
                "revision": push.revision,
                "dry_run": args.dry_run,
                "labels": labels,
                "skipped_labels": skipped_labels,
                "rerun_labels": rerun_labels,
                "rerun_task_ids": rerun_task_ids,
                "skipped_rerun_task_ids": skipped_rerun_task_ids,
                "times": planned
                    .iter()
                    .any(PlannedTrigger::creates_tasks)
                    .then_some(args.times),
                "planned_runs": planned_runs,
                "requires_allow_many_runs": runs_over_limit.is_some(),
                "action_task_ids": action_task_ids,
                "trigger_errors": trigger_errors,
                "watch_summary": watch_summary,
                "watch_error": watch_error,
                "push_url": push_url,
            }))?
        );
    }
    if !trigger_errors.is_empty() {
        anyhow::bail!(
            "{} of {} triggers succeeded. {}{}",
            action_task_ids.len(),
            planned.len(),
            trigger_errors.join("; "),
            watch_error
                .as_deref()
                .map_or(String::new(), |err| format!("; watch: {err}"))
        );
    }
    if let Some(err) = watch_error {
        anyhow::bail!("{err}");
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
    use clap::Parser;

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

    fn action_args(argv: &[&str]) -> Result<()> {
        let matches = Args::command()
            .try_get_matches_from(std::iter::once("treeherder-cli").chain(argv.iter().copied()))?;
        let args = Args::from_arg_matches(&matches)?;
        validate_action_args(&args, &matches)
    }

    #[test]
    fn action_flags_need_a_push_and_exclude_other_modes() {
        assert!(action_args(&["abc", "--cancel-push"]).is_ok());
        assert!(action_args(&["abc", "--cancel-all", "--dry-run"]).is_ok());
        assert!(action_args(&["abc", "--cancel-all", "--filter", "x"]).is_err());
        assert!(action_args(&["abc", "--cancel", "--filter", "media"]).is_ok());
        assert!(action_args(&["abc", "--cancel", "--platform", "linux"]).is_ok());
        assert!(action_args(&["abc", "--cancel"]).is_err());
        assert!(action_args(&["abc", "--cancel-push", "--cancel", "--filter", "x"]).is_err());
        assert!(action_args(&["abc", "--cancel-push", "--retrigger"]).is_err());
        assert!(action_args(&["abc", "--cancel-push", "--filter", "x"]).is_err());
        assert!(action_args(&["abc", "--cancel-push", "--platform", "linux"]).is_err());
        assert!(action_args(&["abc", "--cancel-push", "--times", "2"]).is_err());
        assert!(action_args(&["abc", "--retrigger", "--times", "3", "--dry-run"]).is_ok());
        assert!(action_args(&["abc", "--retrigger-failures", "--times", "3", "--dry-run"]).is_ok());
        for conflicting_flag in ["--retrigger", "--cancel-all"] {
            assert!(action_args(&["abc", "--retrigger-failures", conflicting_flag]).is_err());
        }
        assert!(action_args(&["abc", "--retrigger-failures", "--match-filter", "all"]).is_err());
        assert!(action_args(&["abc", "--retrigger-failures", "--add-jobs", "media"]).is_err());
        assert!(action_args(&["--retrigger-failures"]).is_err());
        assert!(action_args(&["--cancel-push"]).is_err());
        assert!(action_args(&["abc", "--cancel", "--retrigger"]).is_err());
        assert!(action_args(&["abc", "--cancel", "--filter", "x", "--times", "2"]).is_err());
        assert!(action_args(&["abc", "--retrigger", "--times", "0"]).is_err());
        assert!(action_args(&["abc", "--retrigger", "--times", "100"]).is_ok());
        assert!(
            action_args(&["abc", "--retrigger", "--times", "101", "--allow-many-runs"]).is_ok()
        );
        assert!(action_args(&[
            "abc",
            "--add-jobs",
            "media",
            "--times",
            "101",
            "--allow-many-runs"
        ])
        .is_ok());
        assert!(action_args(&["abc", "--cancel-push", "--allow-many-runs"]).is_err());
        assert!(action_args(&["abc", "--cancel", "--filter", "x", "--allow-many-runs"]).is_ok());
        assert!(action_args(&["abc", "--retrigger", "--compare", "def"]).is_err());
        assert!(action_args(&["abc", "--cancel-push", "--lookback", "5"]).is_err());
        assert!(action_args(&["abc", "--add-jobs", "media", "--times", "2"]).is_ok());
        assert!(action_args(&["abc", "--add-jobs", "media", "--platform", "linux"]).is_err());
        assert!(action_args(&["abc", "--cancel-push", "--match-filter", "failure"]).is_err());
        assert!(action_args(&["abc", "--add-jobs", "media", "--retrigger"]).is_err());
        assert!(action_args(&["abc", "--add-jobs", "media", "--cancel-push"]).is_err());
        assert!(action_args(&["abc", "--add-jobs", "media", "--cancel", "--filter", "x"]).is_err());
        assert!(action_args(&["abc", "--dry-run"]).is_err());
        assert!(action_args(&["abc", "--times", "2"]).is_err());
    }

    #[test]
    fn planned_run_count_includes_each_clone_and_in_place_rerun() {
        assert_eq!(planned_run_count(2, 50, 1).unwrap(), DEFAULT_RUN_LIMIT + 1);
        assert_eq!(planned_run_count(0, 101, 1).unwrap(), 1);
    }

    #[test]
    fn action_flags_reject_empty_selectors_and_inspection_options() {
        for args in [
            ["abc", "--cancel", "--filter", ""],
            ["abc", "--cancel", "--platform", ""],
            ["abc", "--retrigger", "--filter", " "],
        ] {
            assert!(action_args(&args)
                .unwrap_err()
                .to_string()
                .contains("nonempty regex"));
        }
        assert!(action_args(&["abc", "--add-jobs", ""])
            .unwrap_err()
            .to_string()
            .contains("nonempty regex"));
        assert!(action_args(&["abc", "--filter", ""]).is_ok());
        let error = action_args(&[
            "abc",
            "--cancel-push",
            "--pattern",
            "linux",
            "--download-artifacts",
            "--dry-run",
        ])
        .unwrap_err()
        .to_string();
        assert!(error.contains("--cancel-push cannot be combined with"));
        assert!(error.contains("--pattern"));
        assert!(error.contains("--download-artifacts"));
    }

    #[test]
    fn retrigger_selection_keeps_the_selected_task_id_for_in_place_reruns() {
        let mut failed = job("toolchain", "linux", "busted", None);
        failed.task_id = Some("failed-task-id".into());
        let mut succeeded = job("toolchain", "linux", "success", None);
        succeeded.task_id = Some("successful-task-id".into());
        let mut exception = job("toolchain", "linux", "exception", None);
        exception.task_id = Some("exception-task-id".into());
        assert!(!matches_result(&exception, &MatchFilter::Failure));
        let jobs = [
            job("Gecko Decision Task", "linux", "busted", None),
            job("Action Task", "linux", "busted", None),
            job("mochitest", "linux", "testfailed", None),
            failed,
            succeeded,
            exception,
        ];
        let retrigger = |value: &str| HashMap::from([("retrigger".to_string(), value.to_string())]);
        let label_tags = HashMap::from([
            ("mochitest".to_string(), retrigger("true")),
            ("toolchain".to_string(), retrigger("false")),
            ("Gecko Decision Task".to_string(), HashMap::new()),
        ]);
        let args = Args::parse_from(["treeherder-cli", "abc", "--retrigger"]);
        let selected = classify_retrigger_jobs(
            jobs.iter()
                .filter(|job| matches_retrigger_result(job, &args)),
            &label_tags,
        );
        assert_eq!(selected.clone_labels, BTreeSet::from(["mochitest".into()]));
        assert_eq!(
            selected.rerun_jobs,
            BTreeMap::from([
                ("failed-task-id".into(), "toolchain".into()),
                ("exception-task-id".into(), "toolchain".into())
            ])
        );
        assert_eq!(
            selected.skipped_labels,
            BTreeSet::from(["Action Task".into(), "Gecko Decision Task".into()])
        );

        let args = Args::parse_from(["treeherder-cli", "abc", "--retrigger-failures"]);
        let selected = classify_retrigger_jobs(
            jobs.iter()
                .filter(|job| matches_retrigger_result(job, &args)),
            &label_tags,
        );
        assert_eq!(selected.clone_labels, BTreeSet::from(["mochitest".into()]));
        assert_eq!(
            selected.rerun_jobs,
            BTreeMap::from([("failed-task-id".into(), "toolchain".into())])
        );
    }

    #[test]
    fn cancellation_watch_counts_final_run_outcomes() {
        let mut summary = WatchSummary::default();
        for (state, reason) in [
            ("exception", "canceled"),
            ("exception", "deadline-exceeded"),
            ("completed", "completed"),
            ("failed", "failed"),
        ] {
            let status: actions::TaskStatus = serde_json::from_value(serde_json::json!({
                "taskId": "task-id",
                "state": state,
                "runs": [
                    {"reasonCreated": "scheduled", "reasonResolved": "canceled"},
                    {"reasonCreated": "rerun", "reasonResolved": reason}
                ]
            }))
            .unwrap();
            summary.record(&status);
        }
        assert_eq!(
            serde_json::to_value(&summary).unwrap(),
            serde_json::json!({
                "canceled": 1, "completed": 1, "failed": 1, "exception": 1,
                "reruns_not_started": []
            })
        );
        assert!(summary
            .message(true)
            .contains("1 canceled, 2 finished before cancellation"));
    }

    #[tokio::test]
    async fn action_watch_keeps_tracking_targets_after_an_action_failure() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let failed = "A".repeat(22);
        let succeeded = "B".repeat(22);
        let failed_target = "C".repeat(22);
        let canceled_target = "D".repeat(22);
        let no_rerun_target = "E".repeat(22);
        let statuses = HashMap::from([
            (failed.clone(), ("failed", "failed")),
            (succeeded.clone(), ("completed", "completed")),
            (canceled_target.clone(), ("exception", "canceled")),
            (no_rerun_target.clone(), ("completed", "completed")),
        ]);
        // Answers the queue's batch status endpoint.
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut content_length = 0;
                loop {
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; content_length];
                stream.read_exact(&mut body).await.unwrap();
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let entries: Vec<_> = request["taskIds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|id| {
                        let id = id.as_str().unwrap();
                        let (state, reason) = statuses[id];
                        serde_json::json!({ "taskId": id, "status": {
                            "taskId": id, "state": state,
                            "runs": [{ "reasonCreated": "scheduled", "reasonResolved": reason }]
                        }})
                    })
                    .collect();
                let body = serde_json::json!({ "statuses": entries }).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let queue = taskcluster::Queue::new(taskcluster::ClientBuilder::new(root)).unwrap();
        let tasks = [
            SubmittedAction {
                id: failed.clone(),
                target: Some(failed_target.clone()),
                creates_tasks: false,
            },
            SubmittedAction {
                id: succeeded,
                target: Some(canceled_target.clone()),
                creates_tasks: false,
            },
        ];
        let watch = ActionWatch {
            client: &build_client().unwrap(),
            decision_task_id: "unused",
            action_tasks: &tasks,
            targets: BTreeSet::from([failed_target, canceled_target]),
            previous_reruns: HashMap::from([(no_rerun_target.clone(), 0)]),
            interval: Duration::from_secs(1),
            pb: &ProgressBar::hidden(),
        };
        let mut errors = Vec::new();
        let summary = tokio::time::timeout(Duration::from_secs(5), watch.run(&queue, &mut errors))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(summary.canceled, 1);
        assert_eq!(
            summary.reruns_not_started,
            BTreeSet::from([no_rerun_target])
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains(&failed));
        server.abort();
    }
}
