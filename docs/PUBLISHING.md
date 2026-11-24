# Publishing treeherder-cli

**Run `just patch`, `just minor` or `just major` on an up-to-date `main`.** It bumps
the version, writes release notes, and pushes a `vVERSION` tag. GitHub Actions then
publishes `treeherder-cli` on crates.io, and binaries with the release notes on
GitHub Releases.

## Access

Accept your GitHub repository invitation before starting. Write access lets you
push release tags and rerun workflows. Routine releases need no local registry
token, crates.io ownership, or access to another maintainer's machine.

## Release

Start with a clean working tree on an up-to-date `main` whose CI has passed:

```sh
git switch main
git pull --ff-only
just patch        # or: just minor, just major, just release 0.3.3
```

The release needs the GitHub CLI (`gh`) and a signed-in Codex CLI (`codex`).
`scripts/release.sh`:

1. Checks that `main` is clean and matches `origin/main`, that the version is new
   and greater than the current one, and waits for CI on the commit to pass. CI
   covers macOS and Windows, which the local checks don't.
2. Has Codex write Markdown release notes from the commits since the previous tag.
   The tree is untouched until this succeeds.
3. Bumps `[package].version` in `Cargo.toml` and its `Cargo.lock` entry, then runs
   `just ci` and a release build.
4. Commits `Bump version to VERSION`, tags `vVERSION` with the notes as the tag
   message, and pushes `main` and the tag.

The tag push starts publication. There is no local `cargo publish` step.

To release by hand instead, make the same version edit and checks, then push an
annotated tag; its message becomes the GitHub release notes:

```sh
release_version=0.3.3
git commit -am "Bump version to ${release_version}"
git tag -a "v${release_version}" -F release-notes.md
git push origin main "v${release_version}"
```

The tagged commit's Cargo version must match the tag exactly; for example,
`v0.3.3` must point to version `0.3.3`.

## Check the result

Open [Actions](https://github.com/padenot/treeherder-cli/actions) and check
**Publish crates**. It validates the tag, runs formatting, Clippy and tests, obtains
a temporary crates.io token, and verifies each package before uploading it.

The separate **Release** workflow creates the GitHub release with the tag's notes
and attaches CLI binaries for Linux (glibc and musl, x86_64 and aarch64), macOS,
and Windows (x86_64 and ARM64). Check both workflows; a successful crate upload does not guarantee
every binary was built.

Confirm the new version on:

- [crates.io: treeherder-cli](https://crates.io/crates/treeherder-cli)
- [GitHub Releases](https://github.com/padenot/treeherder-cli/releases)

## Retry a partial release

If **Publish crates** failed without changing the release's source, select it in
Actions, choose **Run workflow**, enter the release tag, and set `dry_run` to
`false`. With GitHub CLI, from this repository:

```sh
gh workflow run publish-crates.yml -f release_tag=v0.3.3 -f dry_run=false
```

An already-published, non-yanked version is skipped. A yanked version causes a
failure.

For failed binary jobs, rerun the failed jobs in the **Release** workflow.

If fixing the failure requires source or workflow changes, make a new commit and
release version. Published crate versions cannot be overwritten; do not move a
published release's tag to different code.

To test authentication and packaging without uploading, run on `main` with the
default `dry_run=true`:

```sh
gh workflow run publish-crates.yml -f dry_run=true
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
