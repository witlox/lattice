Pre-commit verification. Run this before every commit claim.

1. Format: `cargo fmt --all`
2. Clippy: `cargo clippy --workspace --all-targets` — must be clean (0 warnings)
3. Unit tests: `cargo test -p lattice-common -p lattice-api -p lattice-scheduler -p lattice-node-agent -p lattice-quorum -p lattice-cli` — all must pass
4. Acceptance tests: `cargo test -p lattice-acceptance` — grep for `✘`, must be 0
5. Report: show pass/fail counts for each step

If ANY step fails, do NOT commit. Fix first, then re-run /project:verify.
