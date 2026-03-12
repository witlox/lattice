# Assumptions Log

Assumptions surfaced from architecture docs, ADRs, and domain analysis. Categorized by validation status. Assumptions marked **unknown** need investigation; those marked **critical** would invalidate architectural decisions if wrong.

## Validated (tested or proven)

### A-V1: Raft Commit Latency Acceptable for Scheduling
**Source:** ADR-001
**Assumption:** Raft commit latency (few ms) is acceptable for scheduling granularity of tens-to-hundreds of large allocations.
**Validation:** Lattice schedules HPC-scale allocations (minutes to hours of runtime). A few ms of commit latency per scheduling cycle is negligible.
**If wrong:** Would need to move node ownership out of Raft. Fundamental redesign.

### A-V2: Greedy Knapsack Sufficient at Scale
**Source:** ADR-002
**Assumption:** A greedy heuristic with backfill is sufficient for tens-to-hundreds of pending large allocations. Global optimality is not required.
**Validation:** HPC scheduling practice (Slurm, PBS) demonstrates greedy + backfill works at this scale. RM-Replay simulator validates weight tuning.
**If wrong:** Would need ILP solver or more sophisticated heuristic. Performance concern, not correctness.

### A-V3: uenv Near-Zero Overhead
**Source:** ADR-003
**Assumption:** SquashFS mount namespace has negligible performance overhead compared to running on bare metal.
**Validation:** Proven at CSCS Alps (10,752 GH200 GPUs). Mount namespace adds no per-syscall overhead.
**If wrong:** Would need to evaluate containerization alternatives. Unlikely given production evidence.

### A-V4: PMI-2 Covers Target MPI Implementations
**Source:** ADR-010
**Assumption:** OpenMPI, MPICH, and Cray MPICH all support PMI-2 natively.
**Validation:** Confirmed in documentation for all three. OpenMPI requires `OMPI_MCA_pmix=pmi2` (set automatically).
**If wrong:** Would need to implement PMIx or fall back to SSH (violates INV-N2).

### A-V5: eBPF Overhead Below 0.3%
**Source:** ADR-022
**Assumption:** Always-on eBPF telemetry collection imposes <0.3% overhead.
**Validation:** Standard eBPF overhead benchmarks. CSCS production experience.
**If wrong:** Would need sampling-based collection. Loses always-on visibility.

## Accepted (acknowledged risk)

### A-A1: Eventual Consistency Window for Job Queues
**Source:** ADR-017
**Assumption:** Submissions acknowledged by the API server but not yet committed to Raft can be lost on API server crash. The window is <30s (scheduling cycle time).
**Risk:** Users see "submission accepted" but the allocation disappears if the API server crashes before the next scheduling cycle.
**Mitigation:** Users can query status; "not found" is a detectable failure. API server WAL is a future enhancement.
**If wrong (window much larger):** User trust erosion. Would need synchronous Raft commit on submission.
**Critical:** No — detectable failure, not silent data loss.

### A-A2: VNI Pool Sufficient
**Source:** ADR-013
**Assumption:** 3095 VNIs (default pool 1000-4095) is sufficient for typical HPC deployments.
**Risk:** Large multi-tenant deployments with many concurrent Network Domains could exhaust the pool.
**Mitigation:** Pool is expandable (configuration change + restart). Alert on approaching exhaustion.
**If wrong:** Allocations requiring new domains are blocked until VNIs are freed.
**Critical:** No — expandable and recoverable.

### A-A3: 7-Year Audit Retention Sufficient
**Source:** Sensitive workload design
**Assumption:** 7 years satisfies all target regulatory frameworks (Swiss FADP, EU GDPR, institutional compliance).
**Risk:** Some jurisdictions or future regulations may require longer retention.
**Mitigation:** Retention is configurable. Cold tier storage (S3-compatible immutable) can be extended.
**If wrong:** Would need to extend retention and verify no automatic deletion occurs.
**Critical:** No — configurable.

### A-A4: Conformance Fingerprint Granularity Sufficient
**Source:** ADR-014
**Assumption:** SHA-256 of GPU driver + NIC firmware + BIOS + kernel captures the meaningful configuration dimensions. Two nodes with the same fingerprint behave identically for multi-node workloads.
**Risk:** Fingerprint misses relevant dimensions (e.g., BIOS settings within a version, microcode patches, NIC configuration registers).
**Mitigation:** Fingerprint composition is extensible — add more inputs.
**If wrong:** Subtle performance degradation or correctness issues (NCCL hangs) despite matching fingerprints.
**Critical:** No for correctness (soft constraint), but user-visible (hard constraint for sensitive).

### A-A5: Single Metric Sufficient for Autoscaling
**Source:** Autoscaling design
**Assumption:** Scaling decisions based on a single metric threshold (e.g., GPU utilization) are sufficient for reactive allocations.
**Risk:** Complex services may need multi-metric scaling (e.g., scale on GPU util AND request latency).
**Mitigation:** Custom TSDB metrics with label matchers provide flexibility. Multi-metric scaling is a future enhancement.
**If wrong:** Users must implement their own scaling logic externally.
**Critical:** No — workaround exists.

### A-A6: Federation Catalog Staleness Tolerable
**Source:** Federation design
**Assumption:** Eventually consistent catalog with up to 2-hour staleness for site capabilities is acceptable for routing decisions.
**Risk:** Stale catalog causes jobs routed to sites without capacity, resulting in remote rejection and local retry.
**Mitigation:** Remote rejection is handled gracefully (retry locally). Stale peers deprioritized. No data loss.
**If wrong (staleness causes frequent rejections):** User experience degradation. Would need tighter sync or proactive capacity polling.
**Critical:** No — graceful degradation.

### A-A7: Wipe Timeout of 30 Minutes Sufficient
**Source:** Node lifecycle / sensitive workloads
**Assumption:** OpenCHAMI secure wipe completes within 30 minutes for typical node configurations.
**Risk:** Large NVMe arrays (multi-TB) may take longer for cryptographic erase.
**Mitigation:** Timeout is configurable. Wipe failure quarantines the node (safe failure mode).
**If wrong:** More nodes enter quarantine, reducing available sensitive pool.
**Critical:** No — safe failure mode, configurable.

## Unknown (needs investigation)

### A-U1: Slingshot VNI Configuration Propagation Latency
**Source:** ADR-013
**Assumption:** VNI configuration propagation to Slingshot NICs takes ~50ms and is reliable.
**Status:** Accepted risk — validate at scale. Slingshot is HPC-native; VNI propagation is a core feature. Performance should be "decent" but unvalidated under concurrent scheduling load (hundreds of VNI changes per cycle).
**If wrong:** Allocation startup delayed or fails. Network isolation violated if propagation is partial.
**Action:** Validate at scale during initial deployment. Monitor `lattice_network_vni_setup_duration_seconds` histogram.

### A-U2: VAST API Latency for QoS and Pre-staging
**Source:** Data staging design
**Assumption:** VAST REST API calls for QoS setting and data pre-staging complete within reasonable latency (seconds, not minutes).
**Status:** Accepted risk — validate at scale. VAST is HPC-native storage; its API is designed for this use case. Unvalidated under concurrent staging from many allocations.
**If wrong:** Data staging becomes a bottleneck. f5 scores are stale because staging is slow.
**Action:** Validate at scale during initial deployment. Monitor staging duration metrics.

### A-U3: GPU HBM Wipe Not Available via Redfish **[CRITICAL]**
**Source:** Sensitive workload design
**Assumption (original):** OpenCHAMI's secure erase (via Redfish BMC) performs a complete wipe including GPU HBM.
**Finding:** Redfish `SecureErase` covers NVMe storage. BMC OEM extensions may cover RAM scrub. But GPU HBM wipe is NOT a Redfish operation — it requires `nvidia-smi` or driver-level commands, not BMC. OpenCHAMI's Redfish path does not cover GPU memory.
**Revised assumption:** Sensitive node wipe requires a two-step process: (1) Node agent clears GPU HBM via driver commands (`nvidia-smi` GPU reset or CUDA memset) BEFORE node release, (2) OpenCHAMI Redfish handles NVMe erase and node reimage.
**If wrong (GPU HBM not cleared):** Sensitive data remnants in GPU memory accessible to next tenant. Regulatory violation.
**Action:** Verify the node agent's epilogue includes explicit GPU memory clear for sensitive allocations. Verify this is auditable (Raft-committed confirmation that GPU clear completed).
**Critical:** Yes — regulatory compliance depends on this.

### A-U4: Raft Snapshot Size Manageable
**Source:** ADR-001
**Assumption:** The Raft state machine (node ownership + allocation states + sensitive audit) fits in reasonable snapshot sizes for timely recovery.
**Unknown:** Snapshot size growth rate. Time to restore from snapshot + WAL replay. Impact of 7-year audit log accumulation on snapshot size.
**If wrong:** Recovery time after complete quorum loss could be excessive. May need log compaction or audit log archival strategy.
**Investigation needed:** Model snapshot size growth over 7 years of audit log accumulation.

### A-U5: Tree-Based Fence at Scale
**Source:** ADR-010
**Assumption:** PMI-2 fence with star topology (all node agents exchange KVs via leader) is sufficient. Tree-based reduction is deferred until needed.
**Unknown:** At what node count does star topology become a bottleneck? What latency is acceptable for MPI_Init?
**If wrong:** MPI_Init takes too long for large jobs (>1000 nodes). Users see unexplained startup delays.
**Investigation needed:** Benchmark fence latency at 100, 500, 1000, 5000 nodes.

### A-U6: Ultra Ethernet VNI Compatibility
**Source:** ADR-013, network domains design
**Assumption:** Ultra Ethernet will support VNI-based isolation compatible with the current Slingshot VNI model.
**Unknown:** Ultra Ethernet VNI semantics may differ. Migration path from Slingshot VNIs to UE VNIs is undefined.
**If wrong:** Network domain implementation needs a hardware abstraction layer. May need software fallback.
**Investigation needed:** Monitor Ultra Ethernet specification progress.
