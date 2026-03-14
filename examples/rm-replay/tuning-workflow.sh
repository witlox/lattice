#!/usr/bin/env bash
set -euo pipefail

# tuning-workflow.sh — RM-Replay weight tuning workflow
#
# This script demonstrates how to use rm-replay to test cost function
# weight changes before deploying them to production. The workflow:
#   1. Run with default (baseline) weights
#   2. Run with custom (tuned) weights
#   3. Compare the scheduling order to see the impact
#
# rm-replay supports two scoring modes:
#   --mode real   : Uses the actual CostEvaluator from lattice-scheduler
#   --mode simple : Uses the legacy heuristic scoring (for comparison)

EXAMPLES_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TRACE="${EXAMPLES_DIR}/rm-replay/synthetic-trace.json"
BASELINE_WEIGHTS="${EXAMPLES_DIR}/config/weight-profiles/baseline.yaml"
FAIRNESS_WEIGHTS="${EXAMPLES_DIR}/config/weight-profiles/fairness-tuned.yaml"
ENERGY_WEIGHTS="${EXAMPLES_DIR}/config/weight-profiles/energy-optimized.yaml"

# Build rm-replay if needed.
echo "=== Building rm-replay ==="
cd "$(dirname "$0")/../../tools/rm-replay"
cargo build --quiet --release
RM_REPLAY="./target/release/rm-replay"

echo ""
echo "=== Run 1: Default weights (baseline) ==="
echo "Using weights from: ${BASELINE_WEIGHTS}"
$RM_REPLAY \
  --input "$TRACE" \
  --weights "$BASELINE_WEIGHTS" \
  --mode real \
  --output /tmp/lattice-baseline-results.json

echo ""
echo "=== Run 2: Fairness-tuned weights ==="
echo "Using weights from: ${FAIRNESS_WEIGHTS}"
$RM_REPLAY \
  --input "$TRACE" \
  --weights "$FAIRNESS_WEIGHTS" \
  --mode real \
  --output /tmp/lattice-fairness-results.json

echo ""
echo "=== Run 3: Energy-optimized weights ==="
echo "Using weights from: ${ENERGY_WEIGHTS}"
$RM_REPLAY \
  --input "$TRACE" \
  --weights "$ENERGY_WEIGHTS" \
  --mode real \
  --energy-price 0.9 \
  --output /tmp/lattice-energy-results.json

echo ""
echo "=== Comparison ==="
echo ""
echo "Baseline scheduling order:"
python3 -c "
import json
with open('/tmp/lattice-baseline-results.json') as f:
    results = json.load(f)
for r in results:
    print(f\"  #{r['rank']}: {r['allocation_id']} (score: {r['score']:.4f})\")
"

echo ""
echo "Fairness-tuned scheduling order:"
python3 -c "
import json
with open('/tmp/lattice-fairness-results.json') as f:
    results = json.load(f)
for r in results:
    print(f\"  #{r['rank']}: {r['allocation_id']} (score: {r['score']:.4f})\")
"

echo ""
echo "Energy-optimized scheduling order:"
python3 -c "
import json
with open('/tmp/lattice-energy-results.json') as f:
    results = json.load(f)
for r in results:
    print(f\"  #{r['rank']}: {r['allocation_id']} (score: {r['score']:.4f})\")
"

echo ""
echo "Results written to /tmp/lattice-{baseline,fairness,energy}-results.json"
echo "Each result file includes per-factor score breakdowns for detailed analysis."
