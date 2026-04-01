#!/usr/bin/env bash
# Run clippy with --all-features inside a Linux container, matching CI exactly.
# Usage: ./scripts/clippy-linux.sh
#
# Requires: Docker or Podman running locally.
# Uses the same Rust stable toolchain as CI.

set -euo pipefail

CONTAINER_ENGINE="${CONTAINER_ENGINE:-docker}"

echo "=== Running clippy --all-features in Linux container ==="

exec "$CONTAINER_ENGINE" run --rm \
  -v "$(pwd):/workspace" \
  -w /workspace \
  -e CARGO_HOME=/workspace/target/.cargo-docker \
  rust:latest \
  bash -c '
    apt-get update -qq && apt-get install -y -qq protobuf-compiler > /dev/null 2>&1
    rustup component add clippy
    cargo clippy --workspace --all-targets --all-features -- -D warnings
  '
