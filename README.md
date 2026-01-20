# treeherder-cli

Fetch and analyze Firefox CI logs from Treeherder.

## Installation

```bash
cargo install --path .
```

## Examples

```bash
# Basic: get failed jobs as JSON
treeherder-cli a13b9fc22101 --json

# Filter by job name or platform
treeherder-cli a13b9fc22101 --filter "mochitest" --json
treeherder-cli a13b9fc22101 --platform "linux.*64" --json

# Group failures by test name (cross-platform view)
treeherder-cli a13b9fc22101 --group-by test --json

# Compare revisions to find regressions
treeherder-cli a13b9fc22101 --compare b2c3d4e5f678 --json

# Check test history for intermittent detection
treeherder-cli --history "test_audio_playback" --history-count 10 --repo try --json

# Include intermittent failures
treeherder-cli a13b9fc22101 --include-intermittent --json

# Filter long-running jobs (>1 hour)
treeherder-cli a13b9fc22101 --duration-min 3600 --json

# Fetch logs with pattern matching
treeherder-cli a13b9fc22101 --fetch-logs --pattern "ASSERTION|CRASH" --json

# Download artifacts
treeherder-cli a13b9fc22101 --download-artifacts --artifact-pattern "screenshot|errorsummary"

# Get performance/resource data
treeherder-cli a13b9fc22101 --perf --json

# Watch mode with notification (default: poll every 5min)
treeherder-cli a13b9fc22101 --watch --notify
treeherder-cli a13b9fc22101 --watch --watch-interval 60  # poll every minute

# Cache logs for repeated queries
treeherder-cli a13b9fc22101 --fetch-logs --cache-dir ./logs
treeherder-cli --use-cache --cache-dir ./logs --pattern "ERROR" --json

# Switch repository
treeherder-cli a13b9fc22101 --repo autoland --json

# Efficient job history via similar_jobs API
treeherder-cli --similar-history 543981186 --similar-count 100 --repo autoland --json
```
