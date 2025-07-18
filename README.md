# treeherder-check

A CLI tool for Firefox developers to fetch and summarize Treeherder test failure
information as Markdown. Written by and for Claude Code.

## Usage

```bash
# From a Treeherder URL
treeherder-check "https://treeherder.mozilla.org/jobs?repo=try&revision=abc123"

# From a revision hash
treeherder-check abc123

# Show stack traces
treeherder-check --show-stack-traces abc123

# Filter jobs by name (regexp or substring matching)
treeherder-check --filter "mochitest" abc123

# Specify repository (defaults to "try")
treeherder-check --repo autoland abc123
```

## Installation

Clone, then:

```bash
cargo install --path .
```
