# Release a new version: just release 0.19.0
# Requires CARGO_REGISTRY_TOKEN env var.
release version:
    #!/usr/bin/env bash
    set -euo pipefail

    sed -i '' "s/^version = \".*\"/version = \"{{version}}\"/" Cargo.toml

    cargo fmt
    cargo clippy --all-targets --all-features
    cargo build --release

    git add -A
    git commit -m "Bump version to {{version}}"
    git tag -a "v{{version}}" -m "Release v{{version}}"
    git push origin main
    git push origin "v{{version}}"

    CARGO_REGISTRY_TOKEN="${CARGO_REGISTRY_TOKEN}" cargo publish

    echo "Binaries will be built and attached to the GitHub release by CI on tag push."
