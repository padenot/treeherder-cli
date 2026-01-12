# treeherder-cli

Fetch and analyze Firefox CI logs from Treeherder, the CI system of the Firefox
project (and others).

This CLI tool has been written by and for Claude Code.

## Usage

```bash
# From revision hash or Treeherder URL
treeherder-cli abc123
treeherder-cli "https://treeherder.mozilla.org/jobs?repo=try&revision=abc123"

# Download all logs to persistent cache
treeherder-cli abc123 --fetch-logs --cache-dir ./logs --match-filter all

# Query cached logs without re-downloading
treeherder-cli --use-cache --cache-dir ./logs --pattern "dom/media.*\.html"

# Filter specific job types
treeherder-cli abc123 --filter "mochitest"
```

## Features

- Fetch test results from Treeherder
- Download and cache logs persistently
- Search logs with regex patterns (local-only, no re-download)
- Filter by job name and result (failure/success/all)
- Markdown output optimized for LLM consumption

## Cache Workflow

1. Download logs once: `--fetch-logs --cache-dir <path>`
2. Query many times: `--use-cache --cache-dir <path> --pattern <regex>`

Cache structure: `cache_dir/metadata.json` + `job_<id>/*.log` files

## Installation

```bash
cargo install --path .
```
