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
treeherder-cli badrevision --repo autoland --lookback 20 --suspects --test "test_URIFixup"

# Pass/fail history of one test manifest across the pushes before a revision
treeherder-cli badrevision --repo autoland --group-history docshell/test/unit/xpcshell.toml --lookback 150
treeherder-cli badrevision --repo autoland --group-history docshell/test/unit/xpcshell.toml --filter "linux.*xpcshell"

# Did a job run on this push? List every matching job, not only failures
treeherder-cli badrevision --repo autoland --filter "linux2404-64/opt-xpcshell" --match-filter all --json

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
Failures sharing a window are printed as one block. When the oldest push in the
window already fails, the block says so; widen `--lookback` or use `--group-history`.

`--group-history` reads Treeherder's per-push `group_results` and prints one line
per push, newest first, with the number of tasks where the manifest passed or
failed. A push counts as failing when at least as many tasks fail as pass; pass
`--filter`/`--platform` to pin the count to one job. The header names the first
failing and last passing push, and flags a failure that predates the window.
Pushes are fetched newest first and fetching stops once a pass is followed by one
older push with a result, or by ten older pushes without one, so a wide `--lookback`
(default 50, max 300) only costs time when the failure is old.

## Real Autoland fixtures

Range analysis and manifest history have real Autoland replay fixtures under
`tests/fixtures/`; normal tests use them offline. To refresh a trimmed range fixture:

```bash
TREEHERDER_FIXTURE_RANGE=startrev..endrev \
TREEHERDER_FIXTURE_JOB_FILTER=job-substring \
TREEHERDER_FIXTURE_PLATFORM=platform \
cargo test record_autoland_range_fixture_from_env -- --ignored --nocapture
```

## Releasing

Maintainers with GitHub write access can run `just release VERSION` from `main`.
The recipe updates versions, builds, commits, and pushes a `vVERSION` tag.
GitHub Actions publishes the crates to crates.io using trusted publishing;
no local crates.io token is needed. The existing release workflow publishes binaries.

The **Publish crates** workflow can also be run manually. Leave `dry_run` enabled
to check authentication and package builds without uploading. To resume a failed
publication, select its release tag and disable `dry_run`; versions already
published successfully are skipped. Real uploads require a tag matching the
version in `Cargo.toml`.
