use crate::models::Job;
use anyhow::{Context, Result};
use notify_rust::Notification;
use std::io::Write;
use std::path::Path;

pub fn write_atomic(path: &Path, contents: &[u8], sync: bool) -> Result<()> {
    let dir = path.parent().context("file has no parent directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(contents)?;
    if sync {
        file.as_file().sync_all()?;
    }
    file.persist(path)
        .map_err(|err| err.error)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

pub fn is_llm_environment() -> bool {
    std::env::var("CLAUDECODE").is_ok()
        // Codex exports CODEX_THREAD_ID to every command; older versions only
        // set CODEX_SANDBOX, and only for sandboxed ones.
        || std::env::var("CODEX_THREAD_ID").is_ok()
        || std::env::var("CODEX_SANDBOX").is_ok()
        || std::env::var("GEMINI_CLI").is_ok()
        || std::env::var("OPENCODE").is_ok()
}

/// Tells the reader to run `command`. Agents can't complete an interactive
/// login, so they are asked to relay it to the user, who runs it with `!`.
pub fn run_hint(command: &str) -> String {
    if is_llm_environment() {
        format!("Ask the user to run: ! {command}")
    } else {
        format!("Run: {command}")
    }
}

pub fn scope_satisfies(granted: &str, required: &str) -> bool {
    granted == required
        || granted
            .strip_suffix('*')
            .is_some_and(|prefix| required.starts_with(prefix))
}

pub fn are_all_jobs_complete(jobs: &[Job]) -> bool {
    jobs.iter().all(|job| job.state == "completed")
}

pub fn count_job_states(jobs: &[Job]) -> (usize, usize, usize) {
    let completed = jobs.iter().filter(|j| j.state == "completed").count();
    let running = jobs.iter().filter(|j| j.state == "running").count();
    let pending = jobs.iter().filter(|j| j.state == "pending").count();
    (completed, running, pending)
}

pub fn send_notification(title: &str, message: &str) -> Result<()> {
    Notification::new().summary(title).body(message).show()?;
    Ok(())
}

pub fn format_utc_minutes(timestamp: u64) -> String {
    let days = timestamp / 86_400;
    let seconds = timestamp % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        year,
        month,
        day,
        seconds / 3600,
        (seconds % 3600) / 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_unix_timestamp_as_utc() {
        assert_eq!(format_utc_minutes(0), "1970-01-01 00:00");
        assert_eq!(format_utc_minutes(1_789_113_246), "2026-09-11 07:54");
        assert_eq!(format_utc_minutes(951_782_400), "2000-02-29 00:00");
    }
}
