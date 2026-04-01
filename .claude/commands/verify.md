Pre-commit verification. Run this before every commit claim.

1. Format: `cargo fmt --all`
2. Clippy: `cargo clippy --workspace --all-targets --all-features -- -D warnings` — must be 0 errors (matches CI exactly)
3. Deny: `cargo deny check --all-features` — must pass (if cargo-deny is installed)
4. Unit tests: `cargo test -p lattice-common -p lattice-api -p lattice-scheduler -p lattice-node-agent -p lattice-quorum -p lattice-cli` — all must pass
5. Acceptance tests: `cargo test -p lattice-acceptance` — grep for `✘`, must be 0
6. Report: show pass/fail counts for each step

If ANY step fails, do NOT commit. Fix first, then re-run /project:verify.

Note: Step 2 uses `--all-features` to match CI. This catches type inference issues and feature-gated lint differences that don't surface without all features enabled.

On macOS: `--all-features` fails because `aya` (eBPF) requires Linux libc symbols. Use `--features oidc,federation,accounting,nvidia,rocm` instead (all features except ebpf). Or run without `--all-features` and accept that CI may catch additional issues on Linux.
