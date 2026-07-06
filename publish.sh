#!/usr/bin/env bash
set -euo pipefail

if [ -z "${CARGO_REGISTRY_TOKEN:-}" ]; then
    echo "CARGO_REGISTRY_TOKEN not set"
    exit 1
fi

echo "=== Publishing crate ==="
cargo publish
