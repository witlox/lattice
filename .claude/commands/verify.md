Pre-commit verification. Run this before every commit claim.

## Steps

1. Format:
   ```
   cargo fmt --all
   ```

2. Clippy (MUST match CI — use Linux container on macOS):
   ```
   ./scripts/clippy-linux.sh
   ```
   This runs `cargo clippy --workspace --all-targets --all-features -- -D warnings`
   inside a `rust:latest` Docker container. Catches Linux-only lints (`#[cfg(target_os = "linux")]`
   blocks), aya/eBPF compilation, and feature-gated type inference that macOS clippy misses.

   If Docker isn't available, use the macOS subset (LESS STRICT — CI may still catch issues):
   ```
   cargo clippy --workspace --all-targets --features oidc,federation,accounting,nvidia,rocm -- -D warnings
   ```

3. Deny (if cargo-deny installed):
   ```
   cargo deny check --all-features
   ```

4. Unit + integration tests:
   ```
   cargo test -p lattice-common -p lattice-api -p lattice-scheduler -p lattice-node-agent -p lattice-quorum -p lattice-cli
   ```

5. Acceptance tests — grep for failures:
   ```
   cargo test -p lattice-acceptance 2>&1 | grep "✘" | wc -l
   ```
   Must be 0.

6. Report: show pass/fail counts for each step.

If ANY step fails, do NOT commit. Fix first, then re-run.

## Why Linux clippy matters

Code behind `#[cfg(target_os = "linux")]` (UenvRuntime, PodmanRuntime, signal handling,
waitpid, etc.) does NOT compile on macOS. Local `cargo clippy` never sees these blocks.
Every lint in them can only be caught via the Linux container or CI. The container approach
catches ALL lints in one pass instead of the one-fix-per-push cycle.
