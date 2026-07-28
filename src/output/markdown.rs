use crate::models::*;
use colored::Colorize;

fn strip_source_hash(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ':' && i + 40 < chars.len() {
            let hex: String = chars[i + 1..i + 41].iter().collect();
            if hex.chars().all(|c| c.is_ascii_hexdigit()) {
                let rest: String = chars[i + 41..].iter().collect();
                if let Some(rest) = rest.strip_prefix(" : ") {
                    out.push(':');
                    let linenum: String = rest[..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    out.push_str(&linenum);
                    i += 1 + 40 + 3 + linenum.len();
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_register_line(line: &str) -> bool {
    let t = line.trim();
    // Register lines look like "rax = 0x...   rdx = 0x..."
    let parts: Vec<&str> = t.splitn(3, ' ').collect();
    parts.len() >= 3 && parts[1] == "=" && parts[2].starts_with("0x")
}

fn extract_crashed_thread(stackwalk: &str) -> String {
    let mut header_lines: Vec<&str> = Vec::new();
    let mut crashed_thread_lines: Vec<&str> = Vec::new();
    let mut in_crashed_thread = false;

    for line in stackwalk.lines() {
        if line.starts_with("Thread ") {
            if line.contains("(crashed)") {
                in_crashed_thread = true;
                crashed_thread_lines.push(line);
            } else if in_crashed_thread {
                break;
            } else {
                in_crashed_thread = false;
            }
        } else if crashed_thread_lines.is_empty() && !in_crashed_thread {
            if line.starts_with("Crash reason:")
                || line.starts_with("Crash address:")
                || line.starts_with("Process uptime:")
            {
                header_lines.push(line);
            }
        } else if in_crashed_thread {
            crashed_thread_lines.push(line);
        }
    }

    let mut result = header_lines.join("\n");
    if !result.is_empty() && !crashed_thread_lines.is_empty() {
        result.push('\n');
    }
    result.push_str(&crashed_thread_lines.join("\n"));
    result
}

fn format_stack(stack_text: &str, full_stack: bool) -> String {
    let mut out = String::new();
    for line in stack_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !full_stack && (trimmed.starts_with("Found by:") || is_register_line(trimmed)) {
            continue;
        }
        let cleaned = strip_source_hash(trimmed);
        out.push_str(&format!("    {}\n", cleaned.dimmed()));
    }
    out
}

pub fn format_markdown_summary(
    revision: &str,
    push_id: u64,
    jobs: &[JobWithLogs],
    show_stack_traces: bool,
    all_crash_threads: bool,
    full_stack: bool,
    fetch_logs: bool,
) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "{}\n\n",
        "Treeherder Test Results Summary".bold().underline()
    ));
    output.push_str(&format!(
        "{} {}\n",
        "Revision:".cyan().bold(),
        revision.yellow()
    ));
    output.push_str(&format!(
        "{} {}\n\n",
        "Push ID:".cyan().bold(),
        push_id.to_string().yellow()
    ));

    if jobs.is_empty() {
        output.push_str(&format!(
            "{}\n",
            "No jobs found matching criteria.".dimmed()
        ));
        return output;
    }

    let failed_count = jobs
        .iter()
        .filter(|j| {
            j.job.state == "completed" && (j.job.result == "testfailed" || j.job.result == "busted")
        })
        .count();
    let unknown_count = jobs.iter().filter(|j| j.job.result == "unknown").count();

    if failed_count > 0 {
        output.push_str(&format!(
            "{} ({} failures)\n\n",
            "FAILED".red().bold(),
            failed_count
        ));
    } else if unknown_count > 0 {
        output.push_str(&format!(
            "{} ({} total, {} pending/running)\n\n",
            "Jobs".cyan().bold(),
            jobs.len(),
            unknown_count
        ));
    } else {
        output.push_str(&format!("{} ({})\n\n", "Jobs".cyan().bold(), jobs.len()));
    }

    // Job list
    for job_with_logs in jobs {
        let job = &job_with_logs.job;
        let result_str = match job.result.as_str() {
            "success" => format!("[{}]", job.result).green().to_string(),
            "testfailed" | "busted" => format!("[{}]", job.result).red().to_string(),
            _ => format!("[{}]", job.result).yellow().to_string(),
        };
        let errors = job_with_logs.errors.len();
        let err_str = if errors > 0 {
            format!(" — {} error{}", errors, if errors == 1 { "" } else { "s" })
        } else {
            String::new()
        };
        output.push_str(&format!(
            "  {} {} ({}){}\n",
            result_str,
            job.job_type_name,
            job.platform.dimmed(),
            err_str.dimmed()
        ));
    }
    output.push('\n');

    // Job details
    for job_with_logs in jobs {
        let job = &job_with_logs.job;
        let errors = &job_with_logs.errors;
        let log_matches = &job_with_logs.log_matches;

        let result_colored = match job.result.as_str() {
            "success" => job.result.green().to_string(),
            "testfailed" | "busted" => job.result.red().to_string(),
            _ => job.result.yellow().to_string(),
        };

        output.push_str(&format!(
            "{} ({}, {}):\n",
            job.job_type_name.bold(),
            job.platform.dimmed(),
            result_colored
        ));

        if let Some(log_dir) = &job_with_logs.log_dir {
            output.push_str(&format!("  logs: {}\n", log_dir.blue()));
        }

        if !errors.is_empty() {
            for error in errors {
                if let Some(sig) = &error.signature {
                    output.push_str(&format!(
                        "  {} {}\n",
                        "CRASH".red().bold(),
                        sig.chars().take(80).collect::<String>()
                    ));
                } else {
                    let status = error.status.as_deref().unwrap_or("FAIL");
                    let test = error.test.as_deref().unwrap_or("(unknown test)");
                    let msg = error.message.as_ref().map(|m| {
                        let m = if let Some(pos) = m.find("Stack trace:") {
                            &m[..pos]
                        } else {
                            m
                        };
                        m.trim().to_string()
                    });
                    if let Some(msg) = msg {
                        output.push_str(&format!(
                            "  {}  {} — {}\n",
                            status.red(),
                            test,
                            msg.dimmed()
                        ));
                    } else {
                        output.push_str(&format!("  {}  {}\n", status.red(), test));
                    }
                }
            }

            // Native crash stacks
            for error in errors {
                if let Some(native_stack) = &error.stackwalk_stdout {
                    let extracted;
                    let stack_text = if all_crash_threads {
                        native_stack.as_str()
                    } else {
                        extracted = extract_crashed_thread(native_stack);
                        extracted.as_str()
                    };
                    output.push_str(&format!(
                        "\n  {} ({}) for {}:\n",
                        "Native crash stack".yellow().bold(),
                        error.signature.as_deref().unwrap_or("unknown"),
                        error.test.as_deref().unwrap_or("unknown")
                    ));
                    output.push_str(&format_stack(stack_text, full_stack));
                    output.push('\n');
                }
            }

            // JS/Python stack traces
            if show_stack_traces {
                for error in errors {
                    if error.stackwalk_stdout.is_some() {
                        continue;
                    }
                    let stack_trace = if let Some(stack) = &error.stack {
                        Some(stack.as_str())
                    } else if let Some(msg) = &error.message {
                        msg.find("Stack trace:")
                            .map(|pos| &msg[pos + "Stack trace:".len()..])
                    } else {
                        None
                    };
                    if let Some(stack) = stack_trace {
                        output.push_str(&format!(
                            "\n  {} for {}:\n",
                            "Stack trace".yellow().bold(),
                            error.test.as_deref().unwrap_or("unknown")
                        ));
                        output.push_str(&format_stack(stack, full_stack));
                        output.push('\n');
                    }
                }
            }
        } else if !fetch_logs {
            output.push_str(&format!("  {}\n", "no error summary available".dimmed()));
        }

        if fetch_logs && !log_matches.is_empty() {
            output.push_str(&format!(
                "\n  {} ({} matches):\n",
                "Pattern Matches".yellow().bold(),
                log_matches.len()
            ));
            for log_match in log_matches.iter().take(10) {
                output.push_str(&format!(
                    "    {}:{} {}\n",
                    log_match.log_name.cyan(),
                    log_match.line_number.to_string().yellow(),
                    log_match
                        .line_content
                        .chars()
                        .take(100)
                        .collect::<String>()
                        .dimmed()
                ));
            }
            if log_matches.len() > 10 {
                output.push_str(&format!(
                    "    {}\n",
                    format!("... and {} more matches", log_matches.len() - 10).dimmed()
                ));
            }
        }

        output.push('\n');
    }

    output
}

pub fn format_grouped_markdown_summary(
    revision: &str,
    push_id: u64,
    grouped: &[GroupedTestFailure],
) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "{}\n\n",
        "Treeherder Test Results — Grouped by Test"
            .bold()
            .underline()
    ));
    output.push_str(&format!(
        "{} {}\n",
        "Revision:".cyan().bold(),
        revision.yellow()
    ));
    output.push_str(&format!(
        "{} {}\n\n",
        "Push ID:".cyan().bold(),
        push_id.to_string().yellow()
    ));

    if grouped.is_empty() {
        output.push_str(&format!("{}\n", "No test failures found.".green().bold()));
        return output;
    }

    output.push_str(&format!(
        "{} ({} unique tests)\n\n",
        "Test Failures".red().bold(),
        grouped.len()
    ));

    for failure in grouped {
        output.push_str(&format!(
            "  {} — {} platform{}: {}\n",
            failure.test_name.bold(),
            failure.platforms.len(),
            if failure.platforms.len() == 1 {
                ""
            } else {
                "s"
            },
            failure.platforms.join(", ").cyan()
        ));
        for job in &failure.jobs {
            let detail = match (&job.subtest, &job.message) {
                (Some(s), Some(m)) => format!(" ({s}): {m}"),
                (Some(s), None) => format!(" ({s})"),
                (None, Some(m)) => format!(": {m}"),
                (None, None) => String::new(),
            };
            output.push_str(&format!(
                "    {} {}{}\n",
                job.job_type_name.dimmed(),
                job.platform.dimmed(),
                detail.dimmed()
            ));
        }
        output.push('\n');
    }

    output
}

pub fn format_comparison_markdown(result: &ComparisonResult) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "{}\n\n",
        "Treeherder Comparison Results".bold().underline()
    ));
    output.push_str(&format!(
        "{} {}\n",
        "Base:".cyan().bold(),
        result.base_revision.yellow()
    ));
    output.push_str(&format!(
        "{} {}\n\n",
        "Comparing to:".cyan().bold(),
        result.compare_revision.yellow()
    ));

    output.push_str(&format!(
        "  new failures:   {}\n  fixed:          {}\n  still failing:  {}\n\n",
        result.new_failures.len().to_string().red(),
        result.fixed_failures.len().to_string().green(),
        result.still_failing.len().to_string().yellow()
    ));

    if result.new_failures.is_empty() {
        output.push_str(&format!(
            "{} {}\n\n",
            "New Failures:".red().bold(),
            "none".green()
        ));
    } else {
        output.push_str(&format!(
            "{} ({} tests)\n",
            "New Failures".red().bold(),
            result.new_failures.len()
        ));
        for f in &result.new_failures {
            output.push_str(&format!(
                "  {} — {}\n",
                f.test_name.red(),
                f.platforms.join(", ").dimmed()
            ));
        }
        output.push('\n');
    }

    if !result.fixed_failures.is_empty() {
        output.push_str(&format!(
            "{} ({} tests)\n",
            "Fixed".green().bold(),
            result.fixed_failures.len()
        ));
        for f in &result.fixed_failures {
            output.push_str(&format!(
                "  {} — {}\n",
                f.test_name.green(),
                f.platforms.join(", ").dimmed()
            ));
        }
        output.push('\n');
    }

    if !result.still_failing.is_empty() {
        output.push_str(&format!(
            "{} ({} tests)\n",
            "Still Failing".yellow().bold(),
            result.still_failing.len()
        ));
        for f in &result.still_failing {
            output.push_str(&format!(
                "  {} — {}\n",
                f.test_name.yellow(),
                f.platforms.join(", ").dimmed()
            ));
        }
        output.push('\n');
    }

    output
}

pub fn format_range_markdown_summary(result: &RangeJobSummary) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "{}\n\n",
        "Treeherder Range Results".bold().underline()
    ));
    output.push_str(&format!(
        "{} {}\n",
        "Repository:".cyan().bold(),
        result.repo.yellow()
    ));
    output.push_str(&format!(
        "{} {}\n\n",
        "Pushes:".cyan().bold(),
        result.pushes.len().to_string().yellow()
    ));

    for push in &result.pushes {
        output.push_str(&format!(
            "### {} (push {})\n",
            short_revision(&push.push.revision).yellow(),
            push.push.id
        ));

        if push.jobs.is_empty() {
            output.push_str(&format!("  {}\n\n", "no matching jobs".dimmed()));
            continue;
        }

        for job_with_logs in &push.jobs {
            let job = &job_with_logs.job;
            let result_str = match job.result.as_str() {
                "success" => format!("[{}]", job.result).green().to_string(),
                "testfailed" | "busted" => format!("[{}]", job.result).red().to_string(),
                _ => format!("[{}]", job.result).yellow().to_string(),
            };
            let errors = job_with_logs.errors.len();
            let err_str = if errors > 0 {
                format!(" — {} error{}", errors, if errors == 1 { "" } else { "s" })
            } else {
                String::new()
            };
            output.push_str(&format!(
                "  {} {} ({}){}\n",
                result_str,
                job.job_type_name,
                job.platform.dimmed(),
                err_str.dimmed()
            ));
        }
        output.push('\n');
    }

    output
}

pub fn format_range_suspects_markdown(result: &RangeAnalysisResult) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "{}\n\n",
        "Treeherder Range Suspects".bold().underline()
    ));
    output.push_str(&format!(
        "{} {}\n",
        "Repository:".cyan().bold(),
        result.repo.yellow()
    ));
    output.push_str(&format!(
        "{} {}\n",
        "Pushes:".cyan().bold(),
        result.pushes.len().to_string().yellow()
    ));
    if let (Some(first), Some(last)) = (result.pushes.first(), result.pushes.last()) {
        output.push_str(&format!(
            "{} {}..{}\n",
            "Range:".cyan().bold(),
            short_revision(&first.revision).yellow(),
            short_revision(&last.revision).yellow()
        ));
    }
    output.push('\n');

    if result.suspects.is_empty() {
        output.push_str(&format!(
            "{}\n",
            "No first observed failures found.".green()
        ));
        return output;
    }

    output.push_str(&format!(
        "{} ({} failure{})\n\n",
        "Candidate Windows".red().bold(),
        result.suspects.len(),
        if result.suspects.len() == 1 { "" } else { "s" }
    ));

    for suspect in &result.suspects {
        output.push_str(&format!(
            "  {}\n",
            format_failure_key(&suspect.failure_key).bold()
        ));
        output.push_str(&format!(
            "    first failed: {} (push {})\n",
            short_revision(&suspect.first_failed.revision).red(),
            suspect.first_failed.id
        ));
        if let Some(last_pass) = &suspect.last_pass {
            output.push_str(&format!(
                "    last pass:    {} (push {})\n",
                short_revision(&last_pass.revision).green(),
                last_pass.id
            ));
        } else {
            output.push_str(&format!("    last pass:    {}\n", "not observed".yellow()));
        }
        output.push_str(&format!(
            "    confidence:   {}\n",
            format_confidence(suspect.confidence)
        ));
        output.push_str("    candidates:   ");
        let candidate_text = suspect
            .candidate_pushes
            .iter()
            .map(|entry| {
                format!(
                    "{}:{}",
                    short_revision(&entry.push.revision),
                    format_failure_state(entry.state)
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        output.push_str(&candidate_text);
        output.push_str("\n\n");
    }

    output
}

fn short_revision(revision: &str) -> &str {
    &revision[..std::cmp::min(12, revision.len())]
}

fn format_failure_key(key: &FailureKey) -> String {
    let detail = if let Some(signature) = &key.signature {
        signature.as_str()
    } else if let Some(test) = &key.test {
        test.as_str()
    } else {
        "(job-level failure)"
    };

    if let Some(subtest) = &key.subtest {
        format!(
            "{} ({}, {}) :: {}",
            detail, subtest, key.platform, key.job_type_name
        )
    } else {
        format!("{} ({}) :: {}", detail, key.platform, key.job_type_name)
    }
}

fn format_failure_state(state: FailureState) -> &'static str {
    match state {
        FailureState::Fail => "fail",
        FailureState::Pass => "pass",
        FailureState::OtherFail => "other-fail",
        FailureState::NotRun => "not-run",
        FailureState::Pending => "pending",
    }
}

fn format_confidence(confidence: SuspectConfidence) -> String {
    match confidence {
        SuspectConfidence::High => "high".green().to_string(),
        SuspectConfidence::Medium => "medium".yellow().to_string(),
        SuspectConfidence::Low => "low".red().to_string(),
    }
}

pub fn format_perf_markdown(revision: &str, push_id: u64, perf_data: &[JobPerfData]) -> String {
    let mut output = String::new();

    output.push_str(&format!("{}\n\n", "Performance Data".bold().underline()));
    output.push_str(&format!(
        "{} {}\n",
        "Revision:".cyan().bold(),
        revision.yellow()
    ));
    output.push_str(&format!(
        "{} {}\n\n",
        "Push ID:".cyan().bold(),
        push_id.to_string().yellow()
    ));

    let jobs_with_data: Vec<_> = perf_data.iter().filter(|j| j.perf_data.is_some()).collect();

    if jobs_with_data.is_empty() {
        output.push_str(&format!("{}\n", "No performance data available.".dimmed()));
        return output;
    }

    for job_perf in jobs_with_data {
        output.push_str(&format!(
            "  {} ({}):\n",
            job_perf.job_type_name.bold(),
            job_perf.platform.dimmed()
        ));
        if let Some(perf) = &job_perf.perf_data {
            for suite in &perf.suites {
                for subtest in &suite.subtests {
                    output.push_str(&format!(
                        "    {}/{}: {:.2}\n",
                        suite.name, subtest.name, subtest.value
                    ));
                }
            }
        }
        output.push('\n');
    }

    output
}

pub fn format_similar_history_markdown(history: &SimilarJobHistory) -> String {
    let mut output = String::new();

    output.push_str(&format!("{}\n\n", "Similar Job History".bold().underline()));
    output.push_str(&format!(
        "{} {}\n",
        "Job ID:".cyan().bold(),
        history.job_id.to_string().yellow()
    ));
    output.push_str(&format!(
        "{} {}\n",
        "Job Type:".cyan().bold(),
        history.job_type_name.yellow()
    ));
    output.push_str(&format!(
        "{} {}\n",
        "Repository:".cyan().bold(),
        history.repo.yellow()
    ));

    let pass_rate_colored = if history.pass_rate >= 90.0 {
        format!("{:.1}%", history.pass_rate).green().to_string()
    } else if history.pass_rate >= 70.0 {
        format!("{:.1}%", history.pass_rate).yellow().to_string()
    } else {
        format!("{:.1}%", history.pass_rate).red().to_string()
    };

    output.push_str(&format!(
        "{} {} ({} pass / {} fail / {} total)\n\n",
        "Pass Rate:".cyan().bold(),
        pass_rate_colored,
        history.pass_count.to_string().green(),
        history.fail_count.to_string().red(),
        history.total_jobs
    ));

    for job in &history.jobs {
        let result_str = match job.result.as_str() {
            "success" => job.result.green().to_string(),
            "testfailed" | "busted" => job.result.red().to_string(),
            _ => job.result.yellow().to_string(),
        };
        output.push_str(&format!(
            "  push {} — {} ({})\n",
            job.push_id,
            result_str,
            job.platform.dimmed()
        ));
    }

    output
}
