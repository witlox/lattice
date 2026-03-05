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

# Run tests (skips tests marked #[ignore], i.e. slow multi-node Raft tests)
test:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v cargo-nextest &>/dev/null; then
        cargo nextest run --workspace --exclude lattice-acceptance
        cargo test -p lattice-acceptance
    else
        cargo test --workspace
    fi

# Run the full test suite including slow tests (~5-10 min)
test-all:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Running full test suite including slow tests..."
    if command -v cargo-nextest &>/dev/null; then
        cargo nextest run --workspace --run-ignored all --exclude lattice-acceptance
        cargo test -p lattice-acceptance
    else
        cargo test --workspace -- --include-ignored
    fi

# Run only the slow (ignored) tests
test-slow:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v cargo-nextest &>/dev/null; then
        cargo nextest run --workspace --run-ignored ignored-only --exclude lattice-acceptance
    else
        cargo test --workspace -- --ignored
    fi

# Run cargo-deny checks
deny:
    cargo deny check

# Run advisory audit only
audit:
    cargo deny check advisories

# Run the full CI suite locally (fast tests)
all: fmt-check lint test deny

# Run the full CI suite locally (all tests)
all-full: fmt-check lint test-all deny
