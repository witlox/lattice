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

## Vault Integration Assumptions

### A-V6: Vault KV v2 Engine Available
**Source:** Secret resolution design
**Assumption:** When Vault is configured, the KV v2 secrets engine is mounted at the configured prefix (default: `secret/`). The response format follows Vault KV v2 conventions (`data.data.{key}`).
**If wrong:** Secret resolution fails at startup with a clear error. Operator enables KV v2 or adjusts mount path.
**Critical:** No — Lattice works without Vault (INV-SEC6).

### A-V7: AppRole Auth Method Available
**Source:** Secret resolution design
**Assumption:** Vault is configured with the AppRole auth method enabled. Lattice server components authenticate using a role ID + secret ID pair.
**If wrong:** Authentication fails, startup fails (INV-SEC3). Operator configures AppRole or switches to non-Vault mode.
**Critical:** No — Lattice works without Vault.

### A-V8: Vault Prefix Convention Matches Operator Namespace
**Source:** Secret resolution design, INV-SEC5
**Assumption:** Operators organize their Vault namespace to match Lattice's convention: `{prefix}/storage`, `{prefix}/accounting`, `{prefix}/quorum`, `{prefix}/sovra`. This requires Vault-side setup.
**If wrong:** Missing paths cause fatal startup errors. Clear error messages guide operators to create the correct paths.
**Critical:** No — operational setup issue, not architectural.

### A-V9: Vault Latency Acceptable at Startup
**Source:** Secret resolution design
**Assumption:** Vault KV v2 reads complete within a few hundred milliseconds. Startup reads ~5 secrets, so total Vault interaction is <2s.
**If wrong:** Startup is slow but succeeds. No timeout beyond TCP connect timeout (configurable). Not a correctness issue.
**Critical:** No.

### A-V10: Restart-to-Rotate Is Operationally Acceptable
**Source:** Secret resolution design, analyst interrogation
**Assumption:** Operators accept that secret rotation requires a rolling restart of consuming components. There is no hot-reload mechanism. For lattice-api: restart instances behind load balancer (zero-downtime). For lattice-quorum: restart one member at a time (Raft tolerates minority loss).
**If wrong:** Would need a secret watch/reload mechanism (Vault lease renewal, file watcher, or signal-based reload). Significant complexity increase.
**Critical:** No — rolling restart is standard practice for credential rotation in HPC environments.

### A-V11: SPIRE as Vault-TLS Bridge
**Source:** Secret resolution design, hpc-identity cascade
**Assumption:** When both Vault and SPIRE are deployed, SPIRE can use Vault's PKI secrets engine as an upstream authority for signing SVIDs. This provides Vault-backed TLS without Lattice needing to resolve TLS keys from Vault directly.
**If wrong:** SPIRE uses its own CA (no Vault involvement in TLS). TLS still works; just not Vault-backed. No functional impact.
**Critical:** No.

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
**Source:** ADR-015, PACT ADR-008 (node enrollment certificate lifecycle, amended 2026-03-17)
**Assumption:** The hpc-identity crate provides `IdentityCascade` and `CertRotator` for mTLS workload identity. Both lattice-node-agent and lattice-quorum use this instead of pre-provisioned static cert files. The cascade order is:
1. **SpireProvider** (primary) — obtains X.509 SVID from local SPIRE agent socket (`/run/spire/agent.sock`). SPIRE is standard infrastructure on HPE Cray systems (target deployment platform). Handles rotation, attestation, and trust bundle management.
2. **SelfSignedProvider** (fallback) — agent generates keypair + CSR, lattice-quorum signs with ephemeral intermediate CA key (same pattern as pact-journal in PACT ADR-008). Used when SPIRE is not deployed (dev, CI, small clusters).
3. **StaticProvider** (bootstrap) — reads cert/key/trust-bundle from SquashFS image or local files. Used during the boot window before SPIRE or quorum is reachable.
Private keys are generated locally and never transmitted (INV-ID2).
**If wrong:** Would need manual cert management or external PKI dependency on the boot path.
**Critical:** No — static cert fallback always works, but limits rotation and automation.

### A-HC4a: SPIRE Available on Target Deployments
**Source:** PACT ADR-008 amendment, HPE Cray infrastructure
**Assumption:** Target deployment hardware (HPE Cray with Slingshot/UE) runs SPIRE agent on every compute node. SPIRE provides workload attestation, X.509 SVID issuance, and automatic rotation. Identity acquisition is via local unix socket (no network dependency). The same SVID works on both management and HSN networks (A-NET3).
**If wrong:** Lattice falls back to SelfSignedProvider (quorum-signed certs) or StaticProvider (bootstrap certs). All functionality works but cert rotation requires manual intervention or quorum availability.
**Critical:** No — cascade handles SPIRE absence gracefully.

### A-HC4b: Lattice-Quorum as Self-Signed CA
**Source:** PACT ADR-008 (ephemeral CA model), adapted for lattice
**Assumption:** When SPIRE is not available, lattice-quorum nodes can act as ephemeral intermediate CAs for signing agent CSRs (same model as pact-journal in ADR-008). The CA key is generated in-memory at quorum startup, never persisted. CSR signing is a local CPU operation (~1ms per cert). Lattice owns its own trust domain independently of PACT — even when co-deployed, lattice-quorum and pact-journal use separate CA keys and separate trust chains.
**If wrong:** Without SPIRE and without quorum CA, only static bootstrap certs are available. No rotation until either SPIRE or quorum CA is implemented.
**Critical:** No — bootstrap certs work for initial deployment, but production requires at least one rotation mechanism.

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

### A-Auth3: FirecREST Is Not Required *(validated)*
**Source:** CLI authentication design
**Status:** Validated — confirmed as architectural decision. Lattice authenticates directly against the institutional IdP via hpc-auth. FirecREST, when present, is a transparent passthrough gateway for hybrid Slurm deployments. It is not part of the authentication path.
**If wrong:** N/A — this is now a validated design decision, not an assumption.
**Critical:** No.

### A-Auth4: Waldur Is Source of Truth for vCluster Authorization
**Source:** CLI authentication design
**Assumption:** The JWT token identifies the user (authentication). Waldur allocation state determines which vClusters the user can access (authorization). Authorization is checked at request time, not login time.
**If wrong:** Would need to bake vCluster permissions into JWT claims, coupling IdP configuration to Lattice vCluster topology.
**Critical:** No — alternative model works but is operationally more complex.

## Dispatch Assumptions

Introduced 2026-04-16 after the OV suite exposed the dispatch gap. Every assumption here was surfaced during analyst Layers 1–5 for the dispatch fix. Architect must resolve every **[UNKNOWN]** before Gate 1; every **[CRITICAL]** if wrong would invalidate the design.

### Validated (checked against code)

### A-D2: Runtime Subsystems Actually Spawn Processes *(validated)*
**Source:** Layer 1 edge validation (2026-04-16).
**Assumption:** `UenvRuntime` and `PodmanRuntime` really spawn OS processes, not simulate.
**Validation:** `crates/lattice-node-agent/src/runtime/uenv.rs:156` calls `tokio::process::Command` for `squashfs-mount`; `podman.rs:339` calls `nsenter` to spawn the entrypoint. Simulation guards exist but only activate when Linux binaries are absent.
**If wrong:** No runtime to hook up; dispatch fix would need to build runtimes from scratch. Multi-week delta.
**Critical:** No — validated.

### A-D3: AllocationManager Supports Idempotent Registration *(validated)*
**Source:** Layer 2 INV-D3.
**Assumption:** The agent's `AllocationManager` can answer "is allocation X already registered?" so that duplicate `RunAllocation` RPCs can short-circuit.
**Validation:** `crates/lattice-node-agent/src/allocation_runner.rs` has `AllocationManager::get(id)` returning `Option<&LocalAllocation>`. `contains()` is a trivial wrapper; if not present, trivial to add.
**If wrong:** INV-D3 enforcement needs a new mechanism. Low risk — add a hashmap check.
**Critical:** No.

### A-D4: Existing Reattach Path Only Needs Real PIDs To Work *(validated)*
**Source:** Layer 1 edge validation.
**Assumption:** The reattach code (`state.rs`, `reattach.rs`) is structurally correct; it has been dead only because no PID was ever populated. Once `run_allocation` spawns real processes and writes real PIDs into `PersistedAllocation.pid`, reattach works without further changes.
**Validation:** `reattach::reattach()` calls `is_process_alive(pid)`; `state::save_state()` serializes `PersistedAllocation` including `pid: Option<u32>`. Code paths exercise correctly when pid is Some(n) for a live n.
**If wrong:** Reattach requires additional work on top of PID tracking. Medium risk.
**Critical:** No.

### A-D5: `KillMode=process` in the systemd unit *(partially validated)*
**Source:** FM-D3 correctness relies on Workload Process surviving agent crash. Memory says this is configured.
**Validation:** Memory entry states "systemd: `KillMode=process` (workloads survive agent restart in own cgroup scopes)". Not re-verified against the current unit file.
**If wrong:** FM-D3 becomes full data loss on every agent crash. Architect must spot-check the unit file before relying on this.
**Critical:** **[CRITICAL]** — unvalidated claim that determines production correctness.

### A-D6: Protobuf Field Addition Is Back-Compat *(validated by protocol)*
**Source:** Layer 4 IP-13 (extend `RegisterNodeRequest` with `agent_address`) and IP-03 (extend `Heartbeat` with `completion_reports`).
**Assumption:** Adding optional fields to protobuf messages is forward- and backward-compatible.
**Validation:** Protobuf3 wire format; all added fields use new tag numbers. No existing consumer should break when the new fields are present (ignored) or absent (default).
**If wrong:** Rolling upgrade of server before agent (or vice versa) would fail. Known protobuf property; would indicate misuse.
**Critical:** No.

### A-D7: Allocation State Machine Already Supports Pending→Staging→Running Transitions *(validated)*
**Source:** Layer 3 happy-path scenarios.
**Validation:** `AllocationState` enum has `Pending, Staging, Running, ..., Completed, Failed, Cancelled` (crates/lattice-common). The scheduler currently skips `Staging` and goes directly Pending→Running; dispatch fix will add the Staging transition driven by the agent's first Completion Report.
**If wrong:** State machine extension required. Low risk.
**Critical:** No.

### Accepted (known trade-off)

### A-D8: Heartbeat-Bounded Completion Latency Is Acceptable
**Source:** Q2(a), INV-D8.
**Assumption:** A completion latency of up to `2 × heartbeat_interval` (default ~20s) is acceptable because scheduler cycles are 5s+ anyway and HPC workload runtimes are minutes-to-hours.
**Trade-off:** Short workloads (e.g., OV `/bin/echo` test) see proportionally higher overhead (a 10ms process ships a 10s-latency Completion Report). Accepted because operational validation tolerates this; production workloads are long-lived.
**If wrong:** Need lower-latency completion path — would require a separate RPC (rejected in Q2) or much shorter heartbeat interval (increases baseline traffic).
**Critical:** No.

### A-D9: Default Policy Knobs Are Site-Adjustable, Not Workload-Adjustable
**Source:** Layer 4 IP-14 (max_dispatch_retries=3), INV-D11 (max_node_dispatch_failures=5), A-D8 (heartbeat_interval).
**Assumption:** These thresholds live in site configuration, not per-allocation spec. Users cannot ask for "more aggressive retry" on a single job.
**Trade-off:** Simpler API and deployment-wide consistency; some users may want per-allocation override.
**If wrong:** Add per-allocation overrides to AllocationSpec. Non-invasive extension.
**Critical:** No.

### A-D10: Bare-Process Runtime Inherits Agent Environment
**Source:** Q1(a) decision, Layer 1 Runtime variants.
**Assumption:** A Bare-Process Runtime spawns the entrypoint with the lattice-agent process's environment (minus agent-specific variables). No synthetic environment construction; no chroot.
**Trade-off:** Matches Slurm's default behaviour where jobs inherit the slurmd environment. Less isolated than Uenv or Podman; users wanting isolation must opt in to one of those.
**If wrong:** Security-conscious sites require isolation by default. Override via site policy: "require image or uenv" flag at allocation admission.
**Critical:** No.

### A-D11: Completion Report Buffer Size (256) Is Ample
**Source:** Layer 1 Completion Report definition.
**Assumption:** 256 buffered reports per agent is more than enough given <10 simultaneously-active allocations per node and a 10s heartbeat interval.
**Trade-off:** If a site runs thousands of short-lived allocations on one node between heartbeats, overflow is possible. FM-D8 degrades gracefully (drop oldest, keep terminal-state). Not a correctness risk; observability degradation only.
**If wrong:** Raise the bound or move to a streaming RPC.
**Critical:** No.

### A-D12: FM-D5 Orphan Output Loss Is Tolerable
**Source:** Layer 5 FM-D5.
**Assumption:** An orphan Workload Process (no state-file entry) had no checkpoint; its output cannot be associated with any allocation; killing it is the only reasonable response.
**Trade-off:** Unavoidable by construction. Operator sees it in audit log; can investigate.
**If wrong:** Would need to rebuild allocation association from cgroup-scope name (if we encode allocation_id in the scope path). Possible but adds complexity.
**Critical:** No.

### Unknown (architect must resolve)

### A-D13: Optimistic Concurrency For Allocation State Transitions *(partially validated)*
**Source:** INV-D6 optimistic version check, IP-14 RollbackDispatch contract.
**Assumption:** A monotonic version mechanism exists so that RollbackDispatch can detect a late Completion Report winning the race.
**Validation:** `crates/lattice-quorum/src/commands.rs:30` has `expected_version: Option<u64>` on scheduler `Propose` commands, with doc: "Heartbeats with stale versions are rejected to prevent post-claim stale health records." `owner_version: u64` also exists (line 60). Mechanism is command-level, not an Allocation field.
**Architect decision needed:** (a) attach `state_version` directly to Allocation entity, or (b) continue the command-level pattern — thread `expected_version` through `RollbackDispatch`. Either works; INV-D6 phrasing can be adapted.
**If absent entirely:** Would have required Raft log index or pessimistic lock. Not required — mechanism is there.
**Critical:** No — validated, but mechanism shape (field vs. command-param) is an architect choice.

### A-D14: Dispatcher Component Placement **[UNKNOWN]**
**Source:** Layer 4 IP-02 claims the Dispatcher lives in `lattice-api`.
**Assumption:** The Dispatcher is a background component inside the `lattice-api` process, not a separate binary.
**Validation gap:** I asserted this placement for Layer 4. Architect may want it in a separate process for independent scaling or restart.
**If different:** New deployment unit, new systemd/Dockerfile, new config. Non-trivial infra impact.
**Critical:** No — either placement works; choice affects operational model.

### A-D15: Dispatcher Concurrency Model **[UNKNOWN]**
**Source:** Layer 4 IP-02 is silent on whether the Dispatcher is single-threaded per lattice-api instance, worker-pooled, or per-allocation-task.
**Assumption:** Single background task that iterates over un-acked Running allocations and processes them with some parallelism. Concrete model: architect's call.
**Validation gap:** Not designed.
**If wrong:** Performance at scale (hundreds of dispatches per minute) may require pooling.
**Critical:** No — single-threaded is correct, just potentially slow.

### A-D16: `node.consecutive_dispatch_failures` Commit Granularity **[UNKNOWN]**
**Source:** INV-D11.
**Assumption:** The counter is Raft-committed on every increment, OR in-memory with only the Degraded transition committed.
**Validation gap:** Not decided.
**If wrong:** Per-increment commits generate Raft traffic; in-memory loses the counter on lattice-api restart. Architect picks.
**Critical:** No — both approaches work with slightly different trade-offs.

### A-D17: Completion Report Raft Proposal Granularity **[UNKNOWN]**
**Source:** IP-03 says Completion Reports are Raft-committed.
**Assumption:** One Raft proposal per phase transition (per Completion Report), OR one proposal per heartbeat that batches all reports in it.
**Validation gap:** Not decided.
**If wrong:** Per-report is simple but generates more Raft traffic; per-heartbeat batches are more efficient but complicate atomicity (what if half the reports in a batch are invalid?).
**Critical:** No — architect choice.

### A-D18: BareProcessRuntime and PACT Mode Interaction **[UNKNOWN]**
**Source:** Layer 1 Bare-Process definition; dual-mode operation is documented but BareProcessRuntime is new.
**Assumption:** BareProcessRuntime follows the same cgroup-scope-creation pattern as Uenv/Podman (standalone: self-service `unshare(2)` or cgroup create; PACT-managed: handoff from PACT). It simply skips the mount-namespace and image-mount steps.
**Validation gap:** Not validated against `CgroupManager` trait from hpc-node.
**If wrong:** Bare-Process may need its own isolation path. Low risk — the existing trait accommodates any runtime.
**Critical:** No.

### A-D19: Final-State Completion Report Preservation Under Buffer Pressure *(validated)*
**Source:** FM-D8.
**Assumption:** The buffer preserves final-state reports (`Completed`, `Failed`) regardless of intermediate churn.
**Validation:** Resolved 2026-04-16 via INV-D13 (latest-wins keyed map). Mechanism: the buffer is keyed by `allocation_id`, holds at most one entry per allocation, and replaces earlier-phase reports with later-phase ones. Because Local Allocation Phases are monotonic, a terminal report once enqueued cannot be overwritten by a non-terminal one. No priority queue or two-tier structure needed.
**If wrong:** Would require falling back to priority queue or separate terminal-state channel. Not needed — INV-D13's construction is correct.
**Critical:** No — mechanism specified and correct-by-construction.

### A-D20: Dispatcher Leader Election Not Required **[UNKNOWN]**
**Source:** FM-D10 claims dispatcher is stateless-over-Raft and multiple instances are safe due to INV-D3.
**Assumption:** Two lattice-api replicas both acting as dispatchers simultaneously is safe — duplicate dispatches are absorbed by the agent's idempotency (INV-D3). No leader election needed.
**Validation gap:** Adversary likely to attack this. Fan-out RPC traffic is 2× at minimum; concurrent rollback proposals race in IP-14 but one wins via version check.
**If wrong:** Add leader election (e.g., via Raft leadership — only leader runs Dispatcher). Non-trivial.
**Critical:** No — correct either way by INV-D3 + version check; efficiency-only concern.

### A-D22: Degraded Node Auto-Recovery Thresholds Are Policy, Not Invariant
**Source:** DEC-DISP-01 (architect, 2026-04-16).
**Assumption:** The degraded-node probe-recovery mechanism uses `degraded_probe_interval` (default 5 min) and a cluster-wide guard `node_dispatch_failure_ratio_threshold` (default 0.3 over `guard_window` 10 min). These are site-tunable knobs, not runtime invariants — an operator may disable auto-recovery by setting the interval to infinity, or disable the ratio guard by setting the threshold to 1.0.
**Trade-off:** Defaults balance "don't trap operators in permanent Degraded state" (the cluster-wide image case) against "don't thrash a genuinely broken node." An aggressive ratio threshold (e.g., 0.9) effectively disables the guard; a loose one (0.1) suspends auto-Degrade for minor issues.
**If wrong:** Operator tunes values per site. Production deployment docs should surface these.
**Critical:** No — both defaults and operator overrides give reasonable behavior; wrong defaults are a tuning bug, not a correctness bug.

### A-D21: Existing BDD Steps Drive State Machine Abstractly *(validated and accepted as limitation)*
**Source:** Layer 3 analysis.
**Assumption:** `crates/lattice-acceptance/features/allocation_lifecycle.feature` and `node_agent.feature` drive state transitions abstractly (e.g., "When the allocation transitions to 'Running'") rather than via the actual RPC wire. This is why these acceptance tests passed while the dispatch bridge was absent.
**Validation:** Confirmed in Layer 3 reading of the feature files and step definitions.
**Implication:** Cannot rely on existing BDD to catch dispatch bugs. `allocation_dispatch.feature` (Layer 3) must drive the real wire. Architect must ensure step implementations create actual agents and actual RPCs, not state-machine harness.
**Critical:** No — known limitation of existing tests; new feature covers the gap.
