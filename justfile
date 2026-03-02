# Lattice — development task runner
# Install: cargo install just
# Usage:  just <recipe>

default:
    @just --list

# Type-check the workspace
check:
    cargo check --workspace

# Format all code
fmt:
    cargo fmt --all

# Check formatting (CI mode)
fmt-check:
    cargo fmt --all -- --check

# Run clippy lints (deny warnings)
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run tests (prefers nextest, falls back to cargo test)
test:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v cargo-nextest &>/dev/null; then
        cargo nextest run --workspace
    else
        cargo test --workspace
    fi

# Run cargo-deny checks
deny:
    cargo deny check

# Run advisory audit only
audit:
    cargo deny check advisories

# Run the full CI suite locally
all: fmt-check lint test deny
