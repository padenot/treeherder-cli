use clap::builder::TypedValueParser;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, ValueEnum)]
pub enum MatchFilter {
    Failure,
    Success,
    All,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum GroupBy {
    Test,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Sign in to Taskcluster (opens a browser) to allow cancelling/retriggering jobs
    Login {
        #[arg(
            long,
            default_value_t = 1,
            value_parser = clap::builder::PossibleValuesParser::new(["1", "3"]).map(|s| s.parse::<u8>().unwrap())
        )]
        level: u8,
        #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u32).range(1..))]
        days: u32,
    },
    /// Forget the stored Taskcluster credentials
    Logout,
    /// Show the current Taskcluster login
    Status,
}

#[derive(Parser, Debug)]
#[command(
    name = "treeherder-cli",
    about = "Fetch and summarize Treeherder test results for Firefox developers",
    version,
    args_conflicts_with_subcommands = true
)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,
    #[arg(
        help = "Treeherder URL, revision hash, or Lando commit ID (not needed with --use-cache)"
    )]
    pub input: Option<String>,
    #[arg(
        long,
        help = "Repository name (auto-detected from URL if not specified, defaults to 'try')"
    )]
    pub repo: Option<String>,
    #[arg(
        long,
        default_value_t = true,
        help = "Show stack traces in error summaries"
    )]
    pub show_stack_traces: bool,
    #[arg(
        long,
        help = "Only show jobs matching this regex pattern (applied to job_type_name)"
    )]
    pub filter: Option<String>,
    #[arg(long, help = "Fetch all logs for each job")]
    pub fetch_logs: bool,
    #[arg(
        long,
        value_enum,
        default_value = "failure",
        help = "Filter which jobs to apply pattern matching on"
    )]
    pub match_filter: MatchFilter,
    #[arg(
        long,
        help = "Regex pattern to search for in logs (only used with --fetch-logs)"
    )]
    pub pattern: Option<String>,
    #[arg(
        long,
        help = "Directory to store/read cached logs (persistent storage, not temp)"
    )]
    pub cache_dir: Option<String>,
    #[arg(
        long,
        help = "Use cached logs without downloading (requires --cache-dir)"
    )]
    pub use_cache: bool,
    #[arg(long, help = "Include jobs classified as intermittent")]
    pub include_intermittent: bool,
    #[arg(long, help = "Output results in JSON format")]
    pub json: bool,
    #[arg(long, help = "Poll until all jobs complete")]
    pub watch: bool,
    #[arg(
        long,
        default_value = "300",
        help = "Polling interval in seconds (requires --watch)"
    )]
    pub watch_interval: u64,
    #[arg(
        long,
        help = "Send desktop notification when jobs complete (requires --watch)"
    )]
    pub notify: bool,
    #[arg(long, help = "Only show jobs matching this platform regex pattern")]
    pub platform: Option<String>,
    #[arg(long, help = "Only show jobs that took longer than N seconds")]
    pub duration_min: Option<u64>,
    #[arg(
        long,
        value_enum,
        help = "Group failures by test name across platforms"
    )]
    pub group_by: Option<GroupBy>,
    #[arg(long, help = "Compare with another revision to show new failures")]
    pub compare: Option<String>,
    #[arg(
        long,
        help = "Analyze an inclusive revision range, written as START..END or START...END"
    )]
    pub range: Option<String>,
    #[arg(long, help = "Start revision for range analysis, inclusive")]
    pub from: Option<String>,
    #[arg(long, help = "End revision for range analysis, inclusive")]
    pub to: Option<String>,
    #[arg(
        long,
        help = "Analyze N pushes before INPUT plus INPUT as an inclusive range (default 50, max 300 with --group-history)"
    )]
    pub lookback: Option<u64>,
    #[arg(
        long,
        help = "Infer likely candidate push windows for first observed failures in a range"
    )]
    pub suspects: bool,
    #[arg(
        long,
        help = "Only report suspects whose test name matches this regex (requires --suspects)"
    )]
    pub test: Option<String>,
    #[arg(
        long,
        value_name = "MANIFEST",
        help = "Show pass/fail history of a test manifest (e.g. dom/tests/xpcshell.toml) across the pushes before INPUT"
    )]
    pub group_history: Option<String>,
    #[arg(long, help = "Download job artifacts")]
    pub download_artifacts: bool,
    #[arg(
        long,
        help = "Regex pattern to filter artifacts (e.g., 'screenshot|errorsummary')"
    )]
    pub artifact_pattern: Option<String>,
    #[arg(long, help = "Show performance/resource usage data for jobs")]
    pub perf: bool,
    #[arg(long, help = "Show history for a job ID using similar_jobs API")]
    pub similar_history: Option<u64>,
    #[arg(
        long,
        default_value = "50",
        help = "Number of similar jobs to fetch for --similar-history"
    )]
    pub similar_count: usize,
    #[arg(
        long,
        help = "Stream failures to stdout as they appear, without waiting for all jobs to complete (implies --watch)"
    )]
    pub stream_failures: bool,
    #[arg(
        long,
        help = "Show all threads in native crash stacks instead of only the crashed thread"
    )]
    pub all_crash_threads: bool,
    #[arg(
        long,
        help = "Show full crash stack details including registers and 'Found by' annotations"
    )]
    pub full_stack: bool,
    #[arg(
        long,
        help = "Show N pushes before and after this push (useful for bisecting autoland failures)"
    )]
    pub context: Option<u64>,
    #[arg(
        long,
        conflicts_with_all = ["retrigger", "add_jobs"],
        help = "Cancel the pending/running jobs matching --filter/--platform (needs `treeherder-cli login`)"
    )]
    pub cancel: bool,
    #[arg(
        long,
        conflicts_with_all = ["cancel", "retrigger", "add_jobs"],
        help = "Cancel every job of the push (needs `treeherder-cli login`)"
    )]
    pub cancel_push: bool,
    #[arg(
        long,
        conflicts_with = "add_jobs",
        help = "Retrigger jobs matching --filter/--platform/--match-filter (default: failures and exceptions) (needs `treeherder-cli login`)"
    )]
    pub retrigger: bool,
    #[arg(
        long,
        value_name = "REGEX",
        help = "Add the jobs whose task label matches REGEX, from all the jobs this push can run (needs `treeherder-cli login`)"
    )]
    pub add_jobs: Option<String>,
    #[arg(
        long,
        default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..),
        help = "How many copies to schedule with --retrigger or --add-jobs"
    )]
    pub times: u32,
    #[arg(
        long,
        help = "Allow plans above 100 total runs, or above 100 per-job cancel actions with --cancel"
    )]
    pub allow_many_runs: bool,
    #[arg(
        long,
        help = "Show what --cancel/--cancel-push/--retrigger/--add-jobs would do without doing it"
    )]
    pub dry_run: bool,
}
