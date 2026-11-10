# treeherder-cli

Fetch failed Firefox CI jobs, logs, and artifacts from Treeherder. Compare pushes,
inspect intermittent failures, and narrow down regressions across sparse CI runs.
Output is Markdown by default, or JSON with `--json`.

## Install

```sh
cargo install treeherder-cli
```

[Prebuilt binaries](https://github.com/padenot/treeherder-cli/releases) are also available.
The former package name was `treeherder-check`; update old commands to `treeherder-cli`.

## Find failures

Input can be a revision, Treeherder URL, or Lando commit ID. Revisions default to
the `try` repository; URLs supply their repository, or use `--repo` explicitly.

```sh
treeherder-cli a13b9fc22101
treeherder-cli 'https://treeherder.mozilla.org/jobs?repo=try&revision=a13b9fc22101'
treeherder-cli 12345
treeherder-cli a13b9fc22101 --filter mochitest --platform 'linux.*64' --json
treeherder-cli a13b9fc22101 --group-by test
treeherder-cli a13b9fc22101 --compare b2c3d4e5f678
```

## Investigate regressions

```sh
# Candidate cause windows across a range of Autoland pushes
treeherder-cli --repo autoland --range goodrevision..badrevision --suspects

# Limit candidates to one test
treeherder-cli badrevision --repo autoland --lookback 20 --suspects --test test_URIFixup

# Pass/fail history of a manifest
treeherder-cli badrevision --repo autoland \
  --group-history docshell/test/unit/xpcshell.toml --lookback 150

# Include successful jobs when checking whether a job ran
treeherder-cli badrevision --repo autoland \
  --filter 'linux2404-64/opt-xpcshell' --match-filter all --json

# History of similar jobs, starting from a job ID
treeherder-cli --similar-history 543981186 --similar-count 100 --repo autoland
```

`--suspects` reports the interval between the last observed pass and first observed
failure. Pushes where the job did not run remain candidates. If the oldest push
already fails, widen `--lookback` or inspect `--group-history`.

`--group-history` counts a push as failing when at least as many tasks fail as pass.
Use `--filter` / `--platform` to restrict it to one job. Lookback defaults to 50
pushes, with a maximum of 300; fetching can stop earlier once the boundary is found.

## Logs, artifacts, and watch mode

```sh
treeherder-cli a13b9fc22101 --fetch-logs --pattern 'ASSERTION|CRASH'
treeherder-cli a13b9fc22101 --download-artifacts --artifact-pattern 'screenshot|errorsummary'
treeherder-cli a13b9fc22101 --watch --watch-interval 60 --notify
treeherder-cli a13b9fc22101 --stream-failures
treeherder-cli a13b9fc22101 --fetch-logs --cache-dir ./logs
treeherder-cli --use-cache --cache-dir ./logs --pattern ERROR
```

Other useful options: `--include-intermittent`, `--duration-min SECONDS`, `--perf`,
`--all-crash-threads`, and `--full-stack`. Run `treeherder-cli --help` for details.

## Development

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
```

Normal tests replay the Autoland fixtures under `tests/fixtures/` offline.
To record a trimmed range fixture:

```sh
TREEHERDER_FIXTURE_RANGE=startrev..endrev \
TREEHERDER_FIXTURE_JOB_FILTER=job-substring \
TREEHERDER_FIXTURE_PLATFORM=platform \
cargo test record_autoland_range_fixture_from_env -- --ignored --nocapture
```

## Publishing

**Pushing a `vVERSION` tag publishes the crate and release binaries.**
The tag must match the version in `Cargo.toml`. GitHub write access is enough for
routine releases; no local crates.io token is needed.

See [Publishing](docs/PUBLISHING.md) for version updates, checks, tag commands,
release verification, retries, and ownership. `just release VERSION` is optional.

## License

[MPL-2.0](https://www.mozilla.org/MPL/2.0/).
