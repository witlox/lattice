Pre-commit verification. Run this before every commit claim.

CI runs on Linux with `--all-features`. On macOS, `aya` (eBPF) fails because
it needs Linux libc. Use the macOS-safe feature set below.

## Steps

1. Format:
   ```
   cargo fmt --all
   ```

2. Clippy (must match CI — 0 errors):
   - **Linux/CI**: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
   - **macOS**: `cargo clippy --workspace --all-targets --features oidc,federation,accounting,nvidia,rocm -- -D warnings`

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

## Common CI-only failures

- `unused_mut` / `dead_code` — code behind `#[cfg(target_os = "linux")]` compiles
  differently on CI (Linux) vs dev (macOS). Use `#[allow(dead_code)]` for helpers
  only used in one cfg branch.
- `--all-features` type inference — feature flags change which impls are visible,
  sometimes requiring explicit type annotations that aren't needed without flags.
- `aya` crate — requires Linux libc symbols. Only compiles on Linux.
