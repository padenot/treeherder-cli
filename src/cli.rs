use clap::{Parser, ValueEnum};

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

#[derive(Parser, Debug)]
#[command(
    name = "treeherder-cli",
    about = "Fetch and summarize Treeherder test results for Firefox developers"
)]
pub struct Args {
    #[arg(
        help = "Treeherder URL or revision hash (not needed with --use-cache)",
        conflicts_with = "lando_job_id"
    )]
    pub input: Option<String>,
    #[arg(long, default_value = "try", help = "Repository name")]
    pub repo: String,
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
        help = "Use a Lando job ID to fetch the commit hash (alternative to INPUT)",
        conflicts_with = "input"
    )]
    pub lando_job_id: Option<u64>,
}
