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

## Deployment Assumptions

### A-D1: Raft Co-location with PACT
**Source:** Deployment architecture, PACT invariants R1/R2
**Assumption:** When Lattice and PACT are deployed together, they share the same physical servers for their Raft quorums but run independent Raft groups. PACT is incumbent (starts first). Lattice can also run standalone without PACT.
**If wrong:** Would need Lattice to bootstrap its own dedicated servers for Raft, increasing infrastructure requirements.
**Critical:** No — standalone mode always works.

## hpc-core Integration Assumptions

### A-HC1: hpc-node Crate Available
**Source:** ADR-015 (hpc-core shared contracts), PACT invariants WI1-WI6
**Assumption:** The hpc-node crate (published from hpc-core workspace) is available as a crates.io dependency. It defines trait-only contracts (`CgroupManager`, `NamespaceProvider`, `NamespaceConsumer`, `MountManager`, `ReadinessGate`) with no implementations. Lattice implements these traits in lattice-node-agent.
**If wrong:** Lattice would define its own resource isolation abstractions. No functional loss, but drift between PACT and Lattice conventions (cgroup paths, socket paths, mount points).
**Critical:** No — Lattice works standalone without shared contracts. Value is interoperability.

### A-HC2: Dual-Mode Operation (Standalone vs PACT-Managed)
**Source:** ADR-015, PACT domain model §2f
**Assumption:** Lattice-node-agent detects PACT presence at runtime by checking for the handoff socket (`/run/pact/handoff.sock`) and readiness file (`/run/pact/ready`). When PACT is present, lattice-node-agent acts as a `NamespaceConsumer` (requests namespaces from PACT via unix socket, receives FDs via SCM_RIGHTS). When PACT is absent, lattice-node-agent falls back to self-service mode (creates its own cgroups, namespaces, and mounts using the same hpc-node conventions).
**If wrong:** Lattice would need explicit configuration to select mode. Worse UX but functional.
**Critical:** No — configuration fallback exists.

### A-HC3: hpc-audit Crate Available
**Source:** ADR-015, PACT invariants O3
**Assumption:** The hpc-audit crate provides a universal `AuditEvent` format (who/what/when/where/outcome) with well-known action constants and an `AuditSink` trait. Lattice wraps `AuditEvent` inside its existing ed25519-signed audit envelope in the Raft state machine. The Raft log provides ordering and tamper evidence; hpc-audit provides the standardized event schema.
**If wrong:** Lattice keeps its ad-hoc audit format. Unified SIEM forwarding requires a translation layer.
**Critical:** No — audit trail integrity is maintained by Raft regardless.

### A-HC3a: Admin/User Audit Correlation Is Temporal
**Source:** IP-12, cross-system audit design
**Assumption:** Cross-system correlation between PACT admin actions and lattice user-visible effects relies on temporal correlation via shared `node_id` in `AuditScope`. Explicit causal references (linking a lattice event to the PACT event that triggered it) are deferred. This works because PACT admin actions and lattice reactions are tightly time-correlated (seconds).
**If wrong:** Auditors cannot distinguish "PACT froze node → lattice requeued" from "lattice requeued independently at the same time as unrelated PACT action." Explicit causal linking would be needed.
**Critical:** No — temporal correlation is sufficient for most regulatory queries. Causal linking is a future enhancement (requires PACT→Lattice notification channel).

### A-HC4: hpc-identity Crate Available
**Source:** ADR-015, ADR-008 (node enrollment certificate lifecycle)
**Assumption:** The hpc-identity crate provides `IdentityCascade` (SPIRE → self-signed → bootstrap) and `CertRotator` for dual-channel certificate rotation. Lattice-node-agent and lattice-quorum use this for mTLS identity instead of static `rcgen` certs. Private keys are generated locally and never transmitted.
**If wrong:** Lattice continues with static certs from `rcgen`. Production mTLS requires manual cert management.
**Critical:** No — but blocks production mTLS deployment without manual intervention.

### A-HC5: Cgroup v2 Filesystem Available
**Source:** hpc-node `CgroupManager` trait
**Assumption:** Target compute nodes run Linux kernels with cgroup v2 unified hierarchy (`/sys/fs/cgroup/`). The `cgroup.kill` interface (Linux 5.14+) is available for scope cleanup. Older kernels fall back to SIGKILL.
**If wrong:** Would need cgroup v1 support or hybrid mode. Significant complexity increase.
**Critical:** No for deployment (modern HPC kernels are 5.14+), but would block older installations.

### A-HC6: Well-Known Paths Shared Between PACT and Lattice
**Source:** hpc-node constants
**Assumption:** Both PACT and Lattice use well-known filesystem paths defined in hpc-node:
- Cgroup hierarchy: `workload.slice/` (lattice owns), `pact.slice/` (pact owns)
- Handoff socket: `/run/pact/handoff.sock`
- Readiness signal: `/run/pact/ready`
- uenv mount base: `/run/pact/uenv/`
- Working directory base: `/run/pact/workdir/`
These paths prevent configuration drift between the two systems.
**If wrong:** Misconfigured paths cause namespace handoff failure. Detection: handoff error logged, fallback to self-service.
**Critical:** No — fallback to self-service (A-HC2).

## Network Topology Assumptions

### A-NET1: Lattice Traffic on HSN
**Source:** PACT ADR-017 (Network Topology — Management Network for Pact, HSN for Lattice)
**Assumption:** All lattice control plane traffic (Raft consensus, node-agent heartbeats, allocation lifecycle, checkpoint coordination) runs on the high-speed network (Slingshot/Ultra Ethernet, 200G+). The management network (1G Ethernet) is used by PACT for admin operations, PXE boot, and BMC access. Lattice does not use the management network.
**If wrong:** Lattice traffic on management network would saturate 1G links at scale (10,000 nodes × 30s telemetry). Raft consensus latency would increase from sub-microsecond to milliseconds.
**Critical:** Yes at scale — management network cannot sustain lattice traffic volume.

### A-NET2: HSN Available Before Lattice Starts
**Source:** PACT ADR-017 boot ordering, ADR-006 (pact as init)
**Assumption:** When PACT is present, HSN is available by the time lattice-node-agent starts (PACT starts `cxi_rh` in Phase 5, lattice-node-agent starts after). In standalone mode (systemd), HSN availability is assumed (operator responsibility).
**If wrong:** Lattice-node-agent cannot connect to quorum. Agent retries with backoff until HSN comes up. No data loss (node enters Booting → Ready once connected).
**Critical:** No — retry with backoff handles transient HSN unavailability.

### A-NET3: SPIRE Bridges Both Networks
**Source:** PACT ADR-017
**Assumption:** SPIRE agent runs locally on each node (unix socket `/run/spire/agent.sock`). SVIDs are X.509 certificates that authenticate identity, not network interfaces. The same SVID works on both management and HSN networks. Identity acquisition has no network dependency (local socket only).
**If wrong:** Would need separate identity providers per network. Significant complexity increase.
**Critical:** No — self-signed CA fallback (hpc-identity cascade) works without SPIRE.

## Authentication Assumptions

### A-Auth1: hpc-auth Crate Available
**Source:** CLI authentication design
**Assumption:** The shared hpc-auth crate (built and published from PACT workspace) is available as a crates.io dependency. Lattice consumes it as an external dependency, same pattern as raft-hpc-core and hpc-scheduler-core.
**If wrong:** Lattice would need to implement OAuth2 flows directly. Duplication with PACT.
**Critical:** No — could vendor the code, but defeats the purpose.

### A-Auth2: lattice-api Exposes Auth Discovery
**Source:** CLI authentication design
**Assumption:** lattice-api exposes an unauthenticated endpoint returning the IdP URL and public client ID. The CLI uses this for auto-configuration.
**If wrong:** Users must manually configure IdP details. Worse UX but functional.
**Critical:** No — manual config fallback exists.

### A-Auth3: FirecREST Is Optional for Auth
**Source:** CLI authentication design
**Assumption:** Lattice authenticates directly against the institutional IdP. FirecREST, when present, is a passthrough gateway for hybrid Slurm deployments, not required for authentication.
**If wrong:** Lattice would depend on FirecREST for all user authentication. Single point of failure.
**Critical:** No — but changes the deployment model significantly.

### A-Auth4: Waldur Is Source of Truth for vCluster Authorization
**Source:** CLI authentication design
**Assumption:** The JWT token identifies the user (authentication). Waldur allocation state determines which vClusters the user can access (authorization). Authorization is checked at request time, not login time.
**If wrong:** Would need to bake vCluster permissions into JWT claims, coupling IdP configuration to Lattice vCluster topology.
**Critical:** No — alternative model works but is operationally more complex.
