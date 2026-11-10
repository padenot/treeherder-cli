# Publishing treeherder-cli

**Push a `vVERSION` tag to publish.** The tagged commit's Cargo version must match
exactly; for example, `v0.3.3` must point to version `0.3.3`.
GitHub Actions publishes `treeherder-cli` on crates.io and binaries on GitHub Releases.

## Access

Accept your GitHub repository invitation before starting. Write access lets you
push release tags and rerun workflows. Routine releases need no local registry
token, crates.io ownership, or access to another maintainer's machine.

## Prepare a version

Start with a clean working tree on an up-to-date `main`:

```sh
git switch main
git pull --ff-only
git status --short
```

Choose an unpublished version. The commands below use `0.3.3` as an example.

Update `[package].version` in `Cargo.toml`.

Refresh the root lockfile after the version edit, then run the checks:

```sh
cargo check
cargo fmt -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
```

## Push the tag

```sh
release_version=0.3.3
git add Cargo.toml Cargo.lock
git commit -m "Release ${release_version}"
git push origin main
git tag -a "v${release_version}" -m "Release v${release_version}"
git push origin "v${release_version}"
```

The final tag push starts publication. There is no local `cargo publish` step.

`just release VERSION` is an optional shortcut for version edits, a local build,
commit, and tag push. It is not required to publish.

## Check the result

Open [Actions](https://github.com/padenot/treeherder-cli/actions) and check
**Publish crates**. It validates the tag, runs formatting, Clippy and tests, obtains
a temporary crates.io token, and verifies each package before uploading it.

The separate **Release** workflow builds and attaches CLI binaries. Check both
workflows; a successful crate upload does not guarantee every binary was built.

Confirm the new version on:

- [crates.io: treeherder-cli](https://crates.io/crates/treeherder-cli)
- [GitHub Releases](https://github.com/padenot/treeherder-cli/releases)

## Retry a partial release

If **Publish crates** failed without changing the release's source, select it in
Actions, choose **Run workflow**, select the release tag, and set `dry_run` to
`false`. With GitHub CLI, from this repository:

```sh
release_tag=v0.3.3
gh workflow run publish-crates.yml --ref "$release_tag" -f dry_run=false
```

Already-published, non-yanked crate versions are skipped. This lets a retry
continue after one package has uploaded. A yanked version causes a failure.

For failed binary or Python jobs, rerun the failed jobs in the **Release** workflow.

If fixing the failure requires source or workflow changes, make a new commit and
release version. Published crate versions cannot be overwritten; do not move a
published release's tag to different code.

To test authentication and packaging without uploading, run on `main` with the
default `dry_run=true`:

```sh
gh workflow run publish-crates.yml --ref main -f dry_run=true
```

## Publishing configuration and ownership

The crates.io trusted publisher is bound to:

| Setting | Value |
| --- | --- |
| Repository | `padenot/treeherder-cli` |
| Workflow | `.github/workflows/publish-crates.yml` |
| GitHub environment | `crates-io` |

Authentication failures usually mean this binding no longer matches the workflow.
A crate owner must update it if the repository is transferred or the workflow or
environment is renamed. The publishing job needs `id-token: write`.

GitHub release access and registry ownership are separate. A **named crates.io
Owner** can add other owners; new owners must accept the invitation. An existing
owner can use their own Cargo login with ownership-management permission:

```sh
cargo owner --add NEW_OWNER treeherder-cli
```

[Cargo ownership documentation](https://doc.rust-lang.org/cargo/reference/publishing.html#cargo-owner).
These are personal GitHub repositories: collaborators can release, but only the
repository owner can invite additional collaborators. Shared GitHub
administration requires an organization.
