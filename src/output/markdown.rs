use crate::models::*;
use colored::Colorize;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};

pub fn format_markdown_summary(
    revision: &str,
    push_id: u64,
    jobs: &[JobWithLogs],
    show_stack_traces: bool,
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
            "✓ No jobs found matching criteria!".green().bold()
        ));
        return output;
    }

    let failed_count = jobs
        .iter()
        .filter(|j| {
            j.job.state == "completed"
                && (j.job.result == "testfailed" || j.job.result == "busted")
        })
        .count();

    let unknown_count = jobs
        .iter()
        .filter(|j| j.job.result == "unknown")
        .count();

    // Show header based on whether there are failures
    if failed_count > 0 {
        output.push_str(&format!(
            "{} ({} failures)\n\n",
            "Failed Jobs".red().bold(),
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
        output.push_str(&format!(
            "{} ({})\n\n",
            "Jobs".cyan().bold(),
            jobs.len()
        ));
    }

    // Always show the table when there are jobs
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Job ID").add_attribute(Attribute::Bold),
            Cell::new("Job Type").add_attribute(Attribute::Bold),
            Cell::new("Platform").add_attribute(Attribute::Bold),
            Cell::new("Result").add_attribute(Attribute::Bold),
            Cell::new("Errors").add_attribute(Attribute::Bold),
        ]);

    for job_with_logs in jobs {
        let job = &job_with_logs.job;
        let result_cell = match job.result.as_str() {
            "success" => Cell::new(&job.result).fg(Color::Green),
            "testfailed" | "busted" => Cell::new(&job.result).fg(Color::Red),
            _ => Cell::new(&job.result).fg(Color::Yellow),
        };

        table.add_row(vec![
            Cell::new(job.id),
            Cell::new(&job.job_type_name),
            Cell::new(&job.platform),
            result_cell,
            Cell::new(job_with_logs.errors.len()),
        ]);
    }

    output.push_str(&format!("{}\n\n", table));

    for job_with_logs in jobs {
        let job = &job_with_logs.job;
        let errors = &job_with_logs.errors;
        let log_matches = &job_with_logs.log_matches;

        let result_colored = match job.result.as_str() {
            "success" => job.result.green(),
            "testfailed" | "busted" => job.result.red(),
            _ => job.result.yellow(),
        };

        output.push_str(&format!(
            "{} {} - {}\n",
            "▶".cyan(),
            job.job_type_name.bold(),
            job.platform.dimmed()
        ));
        output.push_str(&format!(
            "  {} {} | {} {} | {} {}\n",
            "ID:".dimmed(),
            job.id.to_string().cyan(),
            "Symbol:".dimmed(),
            job.job_type_symbol.cyan(),
            "Result:".dimmed(),
            result_colored
        ));

        if let Some(log_dir) = &job_with_logs.log_dir {
            output.push_str(&format!("  {} {}\n", "Logs:".dimmed(), log_dir.blue()));
        }

        if !errors.is_empty() {
            output.push_str(&format!("\n  {}:\n", "Errors".red().bold()));

            let mut error_table = Table::new();
            error_table
                .load_preset(UTF8_FULL)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_header(vec![
                    Cell::new("Test").add_attribute(Attribute::Bold),
                    Cell::new("Subtest").add_attribute(Attribute::Bold),
                    Cell::new("Status").add_attribute(Attribute::Bold),
                    Cell::new("Message").add_attribute(Attribute::Bold),
                ]);

            for error in errors {
                let test = error.test.as_deref().unwrap_or("-");
                let subtest = error.subtest.as_deref().unwrap_or("-");
                let status = error.status.as_deref().unwrap_or("-");
                let message = error
                    .message
                    .as_ref()
                    .map(|m| {
                        let msg_only = if let Some(pos) = m.find("Stack trace:") {
                            &m[..pos]
                        } else {
                            m
                        };
                        msg_only.trim().chars().take(60).collect::<String>()
                    })
                    .unwrap_or_else(|| "-".to_string());

                error_table.add_row(vec![
                    Cell::new(test),
                    Cell::new(subtest),
                    Cell::new(status).fg(Color::Red),
                    Cell::new(message),
                ]);
            }

            output.push_str(&format!("{}\n", error_table));

            if show_stack_traces {
                for error in errors {
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
                        for line in stack.lines() {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                output.push_str(&format!("    {}\n", trimmed.dimmed()));
                            }
                        }
                        output.push('\n');
                    }
                }
            }
        } else if !fetch_logs {
            output.push_str(&format!("  {}\n", "No error summary available".dimmed()));
        }

        if fetch_logs && !log_matches.is_empty() {
            output.push_str(&format!(
                "\n  {} ({} matches):\n",
                "Pattern Matches".yellow().bold(),
                log_matches.len()
            ));
            let max_matches_to_show = 10;
            for log_match in log_matches.iter().take(max_matches_to_show) {
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
            if log_matches.len() > max_matches_to_show {
                output.push_str(&format!(
                    "    {} more matches (see log files)\n",
                    format!("... and {}", log_matches.len() - max_matches_to_show).dimmed()
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
        "Treeherder Test Results - Grouped by Test"
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
        output.push_str(&format!("{}\n", "✓ No test failures found!".green().bold()));
        return output;
    }

    output.push_str(&format!(
        "{} ({} unique tests)\n\n",
        "Test Failures".red().bold(),
        grouped.len()
    ));

    for failure in grouped {
        output.push_str(&format!("{} {}\n", "▶".cyan(), failure.test_name.bold()));
        output.push_str(&format!(
            "  {} {} platforms: {}\n\n",
            "Affected on".dimmed(),
            failure.platforms.len().to_string().yellow(),
            failure.platforms.join(", ").cyan()
        ));

        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("Platform").add_attribute(Attribute::Bold),
                Cell::new("Job").add_attribute(Attribute::Bold),
                Cell::new("Subtest").add_attribute(Attribute::Bold),
                Cell::new("Message").add_attribute(Attribute::Bold),
            ]);

        for job in &failure.jobs {
            let subtest = job.subtest.as_deref().unwrap_or("-");
            let message = job
                .message
                .as_ref()
                .map(|m| m.chars().take(50).collect::<String>())
                .unwrap_or_else(|| "-".to_string());

            table.add_row(vec![
                Cell::new(&job.platform),
                Cell::new(&job.job_type_name),
                Cell::new(subtest),
                Cell::new(message),
            ]);
        }

        output.push_str(&format!("{}\n\n", table));
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
        "Base revision:".cyan().bold(),
        result.base_revision.yellow()
    ));
    output.push_str(&format!(
        "{} {}\n\n",
        "Comparing to:".cyan().bold(),
        result.compare_revision.yellow()
    ));

    let mut summary_table = Table::new();
    summary_table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Category").add_attribute(Attribute::Bold),
            Cell::new("Count").add_attribute(Attribute::Bold),
        ]);

    summary_table.add_row(vec![
        Cell::new("New Failures").fg(Color::Red),
        Cell::new(result.new_failures.len()).fg(if result.new_failures.is_empty() {
            Color::Green
        } else {
            Color::Red
        }),
    ]);
    summary_table.add_row(vec![
        Cell::new("Fixed").fg(Color::Green),
        Cell::new(result.fixed_failures.len()).fg(Color::Green),
    ]);
    summary_table.add_row(vec![
        Cell::new("Still Failing").fg(Color::Yellow),
        Cell::new(result.still_failing.len()).fg(Color::Yellow),
    ]);

    output.push_str(&format!("{}\n\n", summary_table));

    if result.new_failures.is_empty() {
        output.push_str(&format!(
            "{} {}\n\n",
            "New Failures:".red().bold(),
            "✓ None!".green()
        ));
    } else {
        output.push_str(&format!(
            "{} ({} tests)\n",
            "New Failures".red().bold(),
            result.new_failures.len()
        ));
        output.push_str(&format!(
            "{}\n\n",
            "These tests are now failing but passed in the comparison revision:".dimmed()
        ));

        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("Test").add_attribute(Attribute::Bold),
                Cell::new("Platforms").add_attribute(Attribute::Bold),
            ]);

        for failure in &result.new_failures {
            table.add_row(vec![
                Cell::new(&failure.test_name).fg(Color::Red),
                Cell::new(failure.platforms.join(", ")),
            ]);
        }
        output.push_str(&format!("{}\n\n", table));
    }

    if result.fixed_failures.is_empty() {
        output.push_str(&format!(
            "{} {}\n\n",
            "Fixed Failures:".green().bold(),
            "None".dimmed()
        ));
    } else {
        output.push_str(&format!(
            "{} ({} tests)\n",
            "Fixed Failures".green().bold(),
            result.fixed_failures.len()
        ));
        output.push_str(&format!(
            "{}\n\n",
            "These tests were failing but now pass:".dimmed()
        ));

        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("Test").add_attribute(Attribute::Bold),
                Cell::new("Platforms").add_attribute(Attribute::Bold),
            ]);

        for failure in &result.fixed_failures {
            table.add_row(vec![
                Cell::new(&failure.test_name).fg(Color::Green),
                Cell::new(failure.platforms.join(", ")),
            ]);
        }
        output.push_str(&format!("{}\n\n", table));
    }

    if !result.still_failing.is_empty() {
        output.push_str(&format!(
            "{} ({} tests)\n",
            "Still Failing".yellow().bold(),
            result.still_failing.len()
        ));
        output.push_str(&format!(
            "{}\n\n",
            "These tests fail in both revisions:".dimmed()
        ));

        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("Test").add_attribute(Attribute::Bold),
                Cell::new("Platforms").add_attribute(Attribute::Bold),
            ]);

        for failure in &result.still_failing {
            table.add_row(vec![
                Cell::new(&failure.test_name).fg(Color::Yellow),
                Cell::new(failure.platforms.join(", ")),
            ]);
        }
        output.push_str(&format!("{}\n\n", table));
    }

    output
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
        output.push_str(&format!(
            "{}\n",
            "No performance data available for selected jobs".dimmed()
        ));
        return output;
    }

    for job_perf in jobs_with_data {
        output.push_str(&format!(
            "{} {}\n",
            "▶".cyan(),
            job_perf.job_type_name.bold()
        ));
        output.push_str(&format!(
            "  {} {} | {} {}\n",
            "Platform:".dimmed(),
            job_perf.platform.cyan(),
            "Job ID:".dimmed(),
            job_perf.job_id.to_string().cyan()
        ));

        if let Some(perf) = &job_perf.perf_data {
            output.push_str(&format!(
                "  {} {}\n\n",
                "Framework:".dimmed(),
                perf.framework.name.yellow()
            ));

            if !perf.suites.is_empty() {
                let mut table = Table::new();
                table
                    .load_preset(UTF8_FULL)
                    .set_content_arrangement(ContentArrangement::Dynamic)
                    .set_header(vec![
                        Cell::new("Suite").add_attribute(Attribute::Bold),
                        Cell::new("Metric").add_attribute(Attribute::Bold),
                        Cell::new("Value").add_attribute(Attribute::Bold),
                    ]);

                for suite in &perf.suites {
                    for subtest in &suite.subtests {
                        table.add_row(vec![
                            Cell::new(&suite.name),
                            Cell::new(&subtest.name),
                            Cell::new(format!("{:.2}", subtest.value)).fg(Color::Cyan),
                        ]);
                    }
                }

                output.push_str(&format!("{}\n", table));
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
    output.push_str(&format!(
        "{} {}\n",
        "Total Jobs:".cyan().bold(),
        history.total_jobs.to_string().yellow()
    ));

    let pass_rate_colored = if history.pass_rate >= 90.0 {
        format!("{:.1}%", history.pass_rate).green()
    } else if history.pass_rate >= 70.0 {
        format!("{:.1}%", history.pass_rate).yellow()
    } else {
        format!("{:.1}%", history.pass_rate).red()
    };

    output.push_str(&format!(
        "{} {} ({} pass, {} fail)\n\n",
        "Pass Rate:".cyan().bold(),
        pass_rate_colored,
        history.pass_count.to_string().green(),
        history.fail_count.to_string().red()
    ));

    output.push_str(&format!("{}\n\n", "Recent Results".bold()));

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Push ID").add_attribute(Attribute::Bold),
            Cell::new("Result").add_attribute(Attribute::Bold),
            Cell::new("Platform").add_attribute(Attribute::Bold),
        ]);

    for job in &history.jobs {
        let (result_color, result_text) = match job.result.as_str() {
            "success" => (Color::Green, &job.result),
            "testfailed" | "busted" => (Color::Red, &job.result),
            _ => (Color::Yellow, &job.result),
        };

        table.add_row(vec![
            Cell::new(job.push_id),
            Cell::new(result_text).fg(result_color),
            Cell::new(&job.platform),
        ]);
    }

    output.push_str(&format!("{}\n", table));
    output
}
