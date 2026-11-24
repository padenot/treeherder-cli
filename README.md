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

## Cancel, retrigger, and add jobs

These trigger the push's Taskcluster in-tree actions, like the Treeherder UI does.
Sign in once; this opens the Taskcluster UI to create a client limited to triggering
action hooks, valid 90 days (`--days`). Use `--level 3` for autoland and other level-3
repositories.
Actions support SCM levels 1 and 3, on Firefox (gecko) repositories.
Over SSH, `login` prints the `ssh -L` command that forwards its sign-in callback.

```sh
treeherder-cli login                                      # add --level 3 for level-3 repos
treeherder-cli status
treeherder-cli logout

treeherder-cli a13b9fc22101 --cancel-push                  # the whole push
treeherder-cli a13b9fc22101 --cancel --filter mochitest    # matching pending/running jobs
treeherder-cli a13b9fc22101 --retrigger --times 3          # failed jobs, 3 times each
treeherder-cli a13b9fc22101 --retrigger --match-filter all --filter 'media.*gpu'
treeherder-cli a13b9fc22101 --add-jobs 'linux2404-64/opt-mochitest-media' --dry-run
```

`--retrigger` selects failed jobs and jobs that ended in an exception by default.
It reads each selected label's `retrigger` task tag and reports skipped labels,
such as decision and action jobs, which have none. Firefox clones
eligible labels `--times` times. For labels that only support rerunning, the CLI
requests one in-place rerun per selected task in the failed or exception state.
Use `--allow-many-runs` to submit a plan for more than 100 total requested runs.
The total counts each requested copy and each in-place rerun once.

`--cancel-push` (also spelled `--cancel-all`) cancels the whole push.
`--cancel` triggers one cancel action per job. When it selects every pending and
running task of the push, it uses a single cancel-all action instead, which also
cancels the tasks waiting on them. More than 100 per-job cancels need
`--allow-many-runs`.

`--dry-run` lists the plan, including the total requested runs, and exits.
With `--watch`, the CLI waits for the decision task and action tasks, then follows
the affected tasks in Taskcluster. `--json`
emits one result with the action task IDs and, with `--watch`, the completion summary.
If some action tasks fail, the CLI reports the failures and follows the targets of
successful actions. The summary identifies requested in-place reruns that did not start.
Cancellation summaries count canceled tasks separately from completed tasks, failed
tasks, and other exceptions. Run log, artifact, and other inspection options as a
separate command.
`TASKCLUSTER_CLIENT_ID` and `TASKCLUSTER_ACCESS_TOKEN` take precedence over the stored login,
unless `TASKCLUSTER_ROOT_URL` names another Taskcluster deployment.

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
release verification, retries, and ownership. `just patch`, `just minor` or
`just major` bumps the version, has Codex write the release notes, and pushes the tag.

## License

[MPL-2.0](https://www.mozilla.org/MPL/2.0/).
