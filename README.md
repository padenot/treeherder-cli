# treeherder-cli

Fetch and analyze Firefox CI logs from Treeherder.

> **Note:** This tool was previously published as `treeherder-check`. If you're upgrading, please `cargo install treeherder-cli` and update your scripts to use the new name.

## Installation

```bash
cargo install treeherder-cli
```

## Examples

```bash
# Basic: get failed jobs as markdown
treeherder-cli a13b9fc22101

# Use a Treeherder URL or a Lando commit ID instead of a revision hash
treeherder-cli "https://treeherder.mozilla.org/jobs?repo=try&revision=a13b9fc22101"
treeherder-cli 12345   # bare Lando commit ID
treeherder-cli "https://treeherder.mozilla.org/jobs?repo=try&landoCommitID=12345"

# Output as JSON
treeherder-cli a13b9fc22101 --json

# Filter by job name or platform
treeherder-cli a13b9fc22101 --filter "mochitest"
treeherder-cli a13b9fc22101 --platform "linux.*64"

# Group failures by test name (cross-platform view)
treeherder-cli a13b9fc22101 --group-by test

# Compare revisions to find regressions
treeherder-cli a13b9fc22101 --compare b2c3d4e5f678

# Analyze sparse CI across a range of Autoland pushes
treeherder-cli --repo autoland --range goodrevision..badrevision --suspects
treeherder-cli badrevision --repo autoland --lookback 6 --suspects --json

# Check test history for intermittent detection
treeherder-cli --similar-history 543981186 --similar-count 100 --repo autoland

# Include intermittent failures
treeherder-cli a13b9fc22101 --include-intermittent

# Filter long-running jobs (>1 hour)
treeherder-cli a13b9fc22101 --duration-min 3600

# Fetch logs with pattern matching
treeherder-cli a13b9fc22101 --fetch-logs --pattern "ASSERTION|CRASH"

# Download artifacts
treeherder-cli a13b9fc22101 --download-artifacts --artifact-pattern "screenshot|errorsummary"

# Get performance/resource data
treeherder-cli a13b9fc22101 --perf

# Watch mode: poll until all jobs complete, then notify
treeherder-cli a13b9fc22101 --watch --notify
treeherder-cli a13b9fc22101 --watch --watch-interval 60  # poll every minute

# Stream failures as they appear (no need to wait for all jobs)
treeherder-cli a13b9fc22101 --stream-failures

# Show native crash stack details
treeherder-cli a13b9fc22101 --show-stack-traces
treeherder-cli a13b9fc22101 --show-stack-traces --all-crash-threads
treeherder-cli a13b9fc22101 --show-stack-traces --full-stack

# Cache logs for repeated queries
treeherder-cli a13b9fc22101 --fetch-logs --cache-dir ./logs
treeherder-cli --use-cache --cache-dir ./logs --pattern "ERROR"

# Switch repository
treeherder-cli a13b9fc22101 --repo autoland
```

`--suspects` reports candidate cause windows from the last observed pass to the
first observed fail. Pushes where the job did not run stay in the candidate window.

## Real Autoland fixtures

Range analysis has real Autoland replay fixtures under `tests/fixtures/`; normal
tests use them offline. To refresh a trimmed fixture:

```bash
TREEHERDER_FIXTURE_RANGE=startrev..endrev \
TREEHERDER_FIXTURE_JOB_FILTER=job-substring \
TREEHERDER_FIXTURE_PLATFORM=platform \
cargo test record_autoland_range_fixture_from_env -- --ignored --nocapture
```
