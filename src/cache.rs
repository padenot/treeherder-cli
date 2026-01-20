use crate::models::{CachedPushMetadata, Job, JobWithLogs, LogMatch};
use anyhow::Result;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

pub fn save_cache_metadata(cache_dir: &Path, metadata: &CachedPushMetadata) -> Result<()> {
    let metadata_path = cache_dir.join("metadata.json");
    let json = serde_json::to_string_pretty(metadata)?;
    fs::write(metadata_path, json)?;
    Ok(())
}

pub fn load_cache_metadata(cache_dir: &Path) -> Result<CachedPushMetadata> {
    let metadata_path = cache_dir.join("metadata.json");
    let json = fs::read_to_string(metadata_path)?;
    let metadata: CachedPushMetadata = serde_json::from_str(&json)?;
    Ok(metadata)
}

pub fn search_cached_logs(
    cache_dir: &Path,
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

        if let Some(regex) = pattern {
            let log_files = fs::read_dir(&job_dir)?;
            for entry in log_files.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("log") {
                    let log_name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();

                    if let Ok(matches) = search_log_file(&path, regex, &log_name) {
                        log_matches.extend(matches);
                    }
                }
            }
        }

        results.push(JobWithLogs {
            job: job.clone(),
            errors: vec![],
            log_matches,
            log_dir: Some(job_dir.to_string_lossy().to_string()),
        });
    }

    Ok(results)
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
