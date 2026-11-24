default:
    @just --list

# Formatting and lint results don't depend on the OS, so Windows only runs the tests.
# Run the checks CI runs
[unix]
ci:
    cargo fmt -- --check
    cargo clippy --locked --all-targets --all-features -- -D warnings
    cargo test --locked

# Run the checks CI runs
[windows]
ci:
    cargo test --locked

# Release the next patch version (see docs/PUBLISHING.md)
patch:
    just release patch

# Release the next minor version
minor:
    just release minor

# Release the next major version
major:
    just release major

# Bump major/minor/patch, or release an explicit version
release version:
    bash scripts/release.sh "{{version}}"
