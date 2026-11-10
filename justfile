# Release a new version: just release 0.19.0
# Crates and binaries are published by GitHub Actions on tag push.
release version:
    #!/usr/bin/env bash
    set -euo pipefail

    sed -i.bak "s/^version = \".*\"/version = \"{{version}}\"/" Cargo.toml

    rm Cargo.toml.bak

    cargo fmt
    cargo clippy --all-targets --all-features
    cargo build --release

    git add -A
    git commit -m "Bump version to {{version}}"
    git tag -a "v{{version}}" -m "Release v{{version}}"
    git push origin main
    git push origin "v{{version}}"

    echo "GitHub Actions will publish the crate and attach binaries to the GitHub release."
