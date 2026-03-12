# Escalation: GPU HBM Wipe Not Covered by Redfish Secure Erase

**Type:** Spec Gap
**Feature:** Sensitive workload isolation (wipe-on-release)
**Filed by:** Analyst (retroactive extraction from A-U3)
**Date:** 2026-03-12

## What I need

The sensitive wipe-on-release invariant (INV-S6) requires that ALL data remnants are cleared before a node returns to the general pool. The current design assumes OpenCHAMI's Redfish `SecureErase` covers everything.

## What's blocking

Redfish `SecureErase` covers NVMe storage. BMC OEM extensions may cover RAM scrub. But **GPU HBM wipe is not a Redfish operation**. It requires driver-level commands:

- NVIDIA: `nvidia-smi --gpu-reset` or CUDA API `cuMemFree`/`cudaDeviceReset` — but these require a running driver, not BMC access
- AMD: `rocm-smi --resetgpu` — same constraint

The node agent's epilogue must include an explicit GPU memory clear step for sensitive allocations, BEFORE the node is handed to OpenCHAMI for storage/RAM wipe and reimage.

## Proposed resolution

1. **Add a GPU HBM clear step to the sensitive epilogue** in the node agent, before reporting allocation completion:
   - NVIDIA: call `nvidia-smi --gpu-reset` for each GPU on the node
   - AMD: call `rocm-smi --resetgpu` for each GPU
   - Verify: re-read GPU memory and confirm zeros (or use vendor-specific verification)
2. **Make GPU clear completion an auditable event**: Raft-commit a "GPU memory cleared on nodes N1-N4" audit entry
3. **If GPU clear fails**: quarantine the node (same as wipe failure)
4. **Sequence**: GPU clear (node agent) → report to quorum → OpenCHAMI NVMe erase + reimage → wipe confirmation → node returns to pool

## Impact of waiting

This is a **regulatory compliance gap** in the current design. If a sensitive allocation uses GPU HBM (which all GPU workloads do), the current wipe-on-release flow does not guarantee GPU data destruction.

Severity: **Critical** for sensitive workload correctness. Not blocking for non-sensitive workloads.

## Status

Open — needs implementation in the node agent's sensitive epilogue path.
