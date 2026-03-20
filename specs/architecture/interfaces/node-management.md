# Node Management Module Interfaces (lattice-node-agent)

## Runtime Trait

The core abstraction for process execution. Platform-specific implementations behind `cfg(target_os)`.

```
trait Runtime: Send + Sync
  ├── prepare(config: PrepareConfig) → Result<()>
  │     Prologue: pull image, mount uenv, stage data, setup scratch.
  │     Contract: INV-O2 — must complete before entrypoint.
  │
  ├── spawn(id: &AllocId, entrypoint: &str, args: &[String], env: HashMap) → Result<ProcessHandle>
  │     Start user process in prepared environment.
  │
  ├── signal(handle: &ProcessHandle, signal: Signal) → Result<()>
  │     Deliver signal (SIGTERM, SIGUSR1, SIGKILL) to running process.
  │
  ├── stop(handle: &ProcessHandle, grace: Duration) → Result<ExitStatus>
  │     SIGTERM → wait grace → SIGKILL. Returns exit status.
  │
  ├── wait(handle: &ProcessHandle) → Result<ExitStatus>
  │     Block until process exits naturally.
  │
  └── cleanup(id: &AllocId) → Result<()>
        Epilogue: unmount, wipe scratch, release resources.
```

**Implementations:**
- `UenvRuntime` — SquashFS mount via `squashfs-mount` + `nsenter`
- `SarusRuntime` — OCI container via `sarus run`
- `DmtcpRuntime` — Transparent checkpoint via `dmtcp_launch`
- `MockRuntime` — In-memory stub for testing

## Heartbeat Interface

```
trait HeartbeatSink: Send + Sync
  └── send(heartbeat: HeartbeatPayload) → Result<()>

trait HealthObserver: Send + Sync
  └── collect() → HealthSnapshot

HeartbeatLoop<S: HeartbeatSink, H: HealthObserver>
  └── run(interval: Duration) → never returns
        Periodic: collect health → send heartbeat → sleep.
        Buffers on send failure. Replays on reconnection.
```

**Spec source:** IP-03 (heartbeat/state report), FM-04/FM-09 (grace periods)

**HeartbeatPayload:** `{ node_id, healthy, issues, running_allocations, conformance_fingerprint, sequence }`

**Contract:** Sequence number is monotonically increasing. Quorum uses it to detect stale/replayed heartbeats (IP-03 ordering).

## Telemetry Collection

```
trait GpuDiscoveryProvider: Send + Sync
  └── discover() → Result<Vec<GpuInfo>>

trait MemoryDiscoveryProvider: Send + Sync
  └── discover() → Result<MemoryTopology>

TelemetryCollector
  ├── collect_sample() → TelemetrySample
  │     Reads /proc/stat, /proc/meminfo, /proc/net/dev,
  │     GPU metrics (nvml/rocm-smi), memory topology (sysfs).
  │
  └── push(sample: TelemetrySample, endpoint: &str) → Result<()>
        Push to VictoriaMetrics in Prometheus format.
```

**Implementations:**
- `NvidiaDiscovery` — nvml-wrapper (feature-gated: `nvidia`)
- `AmdDiscovery` — rocm-smi CLI parsing (feature-gated: `rocm`)
- `AutoDiscovery` — tries NVIDIA, falls back to AMD, then stub
- `SysfsMemoryDiscovery` — Linux sysfs NUMA topology
- `UnifiedMemoryDiscovery` — GH200/MI300A superchip detection
- `StubMemoryDiscovery` — non-Linux fallback

## Data Staging

```
trait DataStageExecutor: Send + Sync
  ├── stage(mounts: &[DataMount], alloc_id: &AllocId) → Result<StagingResult>
  │     Pre-stage data to hot tier. Set QoS. Create scratch dirs.
  │     Threshold: 95% readiness before reporting ready.
  │
  └── cleanup(mounts: &[DataMount], alloc_id: &AllocId) → Result<()>
        Release QoS, cleanup scratch. Non-fatal on failure.
```

**Implementations:**
- `RealDataStageExecutor<S: StorageService>` — VAST API calls
- `MockDataStageExecutor` — test stub
- `NoopDataStageExecutor` — when no data mounts

## Resource Isolation (hpc-node contracts)

Lattice-node-agent implements four hpc-node traits for resource isolation. In standalone mode, all implementations are self-contained. In PACT-managed mode, namespace and mount operations delegate to PACT.

```
trait CgroupManager: Send + Sync
  ├── create_hierarchy() → Result<()>
  │     Boot-time: creates /sys/fs/cgroup/workload.slice/ (standalone)
  │     or verifies it exists (PACT-managed). Idempotent.
  │
  ├── create_scope(parent_slice: &str, name: &str, limits: ResourceLimits) → Result<CgroupHandle>
  │     Creates scoped cgroup for an allocation under workload.slice/.
  │     ResourceLimits: memory_max (bytes), cpu_weight (1-10000), io_max (bytes/sec).
  │     Returns opaque handle for process placement.
  │
  ├── destroy_scope(handle: CgroupHandle) → Result<()>
  │     Kills all processes via cgroup.kill (Linux 5.14+) or SIGKILL fallback.
  │     Returns CgroupError::KillFailed if processes in D-state.
  │
  ├── read_metrics(path: &str) → Result<CgroupMetrics>
  │     Reads memory.current, memory.max, cpu.stat (usage_usec), cgroup.procs count.
  │     No ownership check (shared read access).
  │
  └── is_scope_empty(handle: &CgroupHandle) → Result<bool>
        Detects allocation completion (cgroup.procs == 0).
```

**Implementation:** `LinuxCgroupManager` (target_os = "linux") — direct cgroup v2 filesystem API.

**Spec source:** INV-RI1 (slice ownership), A-HC5 (cgroup v2 available)

---

```
trait NamespaceConsumer: Send + Sync
  └── request_namespaces(request: NamespaceRequest) → Result<NamespaceResponse>
        PACT-managed: connect to /run/pact/handoff.sock → send request →
          receive FDs via SCM_RIGHTS → return response.
        Standalone: unshare(2) for each requested namespace type →
          mount uenv if requested → return response with local FDs.
        Fallback: if socket unavailable (1s timeout), switch to standalone.

NamespaceRequest:
  allocation_id: String
  namespaces: Vec<NamespaceType>  (Pid, Net, Mount)
  uenv_image: Option<String>

NamespaceResponse:
  allocation_id: String
  fd_types: Vec<NamespaceType>
  uenv_mount_path: Option<String>
```

**Implementation:** `DualModeNamespaceConsumer` — probes socket at startup, caches mode. Falls back per-request on socket failure (INV-RI2).

**Spec source:** IP-11 (namespace handoff), A-HC2 (dual-mode), A-HC6 (well-known paths)

---

```
trait MountManager: Send + Sync
  ├── acquire_mount(image_path: &str) → Result<MountHandle>
  │     First reference: mount SquashFS image at /run/pact/uenv/{name}.
  │     Subsequent: increment refcount, prepare bind-mount for allocation.
  │     Standalone: always mount (no refcounting).
  │
  ├── release_mount(handle: MountHandle) → Result<()>
  │     Decrement refcount. At zero: start cache hold timer (default 60s).
  │     After timer: lazy unmount. Standalone: immediate unmount.
  │
  ├── force_unmount(image_path: &str) → Result<()>
  │     Emergency: override hold timer, unmount immediately.
  │
  └── reconstruct_state(active_allocations: &[AllocId]) → Result<()>
        On agent restart: scan /proc/mounts, correlate with active allocations.
        Orphaned mounts get refcount=0 and start hold timers (INV-RI3).
```

**Implementation:** `RefcountedMountManager` (PACT-managed, shared cache) and `SimpleMountManager` (standalone, per-allocation). Selected based on mode detection.

**Spec source:** INV-RI3 (refcount consistency)

---

```
trait ReadinessGate: Send + Sync
  ├── is_ready() → bool
  │     Fast, non-blocking check: /run/pact/ready file exists.
  │
  └── wait_ready(timeout: Duration) → Result<()>
        Polls file with backoff until present or timeout (INV-RI4).
        Standalone: always returns Ok immediately (no readiness gate).
```

**Implementation:** `PactReadinessGate` (checks file) and `NoopReadinessGate` (standalone).

**Spec source:** INV-RI4 (non-blocking), FM-22 (readiness timeout)

## Identity Management (hpc-identity)

### Provider Configuration

```
SpireProvider:
  socket: /run/spire/agent.sock   (SPIRE agent, standard on HPE Cray)
  spiffe_id: auto-detect           (from node attestation)
  timeout: 30s

SelfSignedProvider:
  signing_endpoint: lattice-quorum gRPC   (quorum acts as ephemeral CA)
  cert_lifetime: 3 days (259200s)         (same as PACT ADR-008)
  Agent generates keypair → submits CSR → quorum signs with in-memory CA key
  CA key: ephemeral, generated at quorum startup, never persisted

StaticProvider:
  cert_path: /etc/lattice/tls/bootstrap.crt   (from SquashFS or cloud-init)
  key_path: /etc/lattice/tls/bootstrap.key
  trust_bundle_path: /etc/lattice/tls/ca-bundle.crt
```

### Startup Sequence

```
T+0.0s  lattice-node-agent starts (under PACT supervision or systemd)
T+0.1s  IdentityCascade constructed:
        [SpireProvider(socket), SelfSignedProvider(quorum), StaticProvider(files)]
T+0.2s  cascade.get_identity():
        - SpireProvider.is_available() → check socket exists
        - [yes] SpireProvider.get_identity() → SVID (primary path on HPE Cray)
        - [no]  SelfSignedProvider.is_available() → check quorum reachable
          - [yes] generate keypair, submit CSR, receive signed cert
          - [no]  StaticProvider.is_available() → check bootstrap cert files
            - [yes] read cert/key from disk (bootstrap identity)
            - [no]  IdentityError::NoProviderAvailable → agent fails to start
T+0.3s  Configure tonic TLS with WorkloadIdentity cert/key/trust_bundle
T+0.4s  Connect to lattice-quorum on HSN (mTLS)
T+0.5s  Schedule CertRotator at 2/3 certificate lifetime

Boot window upgrade (if started with StaticProvider):
T+1.0s  SPIRE agent becomes available (started by PACT or systemd)
T+1.1s  CertRotator fires: cascade.get_identity() → SpireProvider now available
T+1.2s  Dual-channel swap: bootstrap → SVID
T+1.3s  Bootstrap identity discarded
```

### CertRotator Protocol

```
CertRotator (triggered at 2/3 cert lifetime or on provider upgrade):
  1. cascade.get_identity() → new WorkloadIdentity
  2. Build passive tonic Channel with new TLS config
  3. Health-check passive channel (connect + handshake to quorum)
  4. Atomic swap: passive → active, old active → draining
  5. Drain old channel (wait for in-flight RPCs to complete)
  6. Drop old channel
  7. On failure at any step: abort, keep current active channel
     (retry at next scheduled interval)
```

### Lattice-Quorum as Ephemeral CA (SelfSignedProvider)

When SPIRE is not deployed, lattice-quorum provides the `SelfSignedProvider` signing endpoint:

```
Quorum startup:
  1. Generate ephemeral intermediate CA key in memory (never persisted)
  2. Derive CA cert (self-signed or signed by site root CA if configured)
  3. Expose CSR signing via authenticated gRPC endpoint

Agent CSR signing:
  1. Agent generates keypair locally (in RAM)
  2. Agent sends CSR to quorum (authenticated via bootstrap cert)
  3. Quorum validates: caller's identity matches a registered node
  4. Quorum signs CSR with ephemeral CA key (~1ms CPU, no network)
  5. Returns signed cert + trust bundle
  6. Agent builds mTLS channel with new cert

Trust separation:
  - Lattice-quorum and pact-journal use SEPARATE CA keys
  - Even when co-deployed, trust domains are independent
  - A lattice cert cannot authenticate to pact, and vice versa
```

**Spec source:** INV-ID1 (cascade order), INV-ID2 (key locality), INV-ID3 (non-disruptive rotation), FM-20 (cascade exhaustion), FM-23 (rotation failure), A-HC4/A-HC4a/A-HC4b (SPIRE + self-signed + bootstrap), PACT ADR-008 (ephemeral CA model)

## Attach / PTY

```
trait PtyBackend: Send + Sync
  ├── spawn_pty(alloc_id: &AllocId, node_id: &NodeId, cmd: &str, rows: u16, cols: u16) → Result<PtySession>
  └── resize(session: &PtySession, rows: u16, cols: u16) → Result<()>
```

**Contract:** INV-C2 — sensitive allocations allow at most one active session. Attach handler checks before spawning.

## MPI Process Management

```
PMI2Server (Unix domain socket per allocation)
  ├── handle_init(rank: u32) → PmiInitResponse
  ├── handle_kvs_put(key: &str, value: &str) → Result<()>
  ├── handle_kvs_get(key: &str) → Result<String>
  └── handle_fence() → Result<()>
        Cross-node KVS exchange via gRPC PmiFence to head node.

Cross-Node Coordination (gRPC between node agents):
  ├── LaunchProcesses(request) → accepted
  ├── PmiFence(kvs_entries, node_index) → merged_kvs
  └── AbortProcesses(launch_id, reason) → success
```

**Spec source:** A-V4 (PMI-2 coverage), INV-N2 (no SSH)

## Node Agent gRPC Server

Receives commands from the control plane:

```
NodeAgentService (gRPC)
  ├── RunAllocation(request) → response
  │     Triggers prologue → spawn → monitor.
  │
  ├── StopAllocation(request) → response
  │     SIGTERM → grace → SIGKILL.
  │
  ├── Attach(stream) ↔ (stream)
  │     Bidirectional PTY stream.
  │
  ├── StreamLogs(request) → stream LogEntry
  │     Node-local log streaming from ring buffer.
  │
  ├── LaunchProcesses(request) → response
  │     MPI rank launch (fan-out from head node).
  │
  ├── PmiFence(request) → response
  │     PMI-2 KVS exchange (head node aggregation).
  │
  └── AbortProcesses(request) → response
        Kill all ranks for a launch.
```

## Node Agent gRPC Client

Reports to the control plane:

```
GrpcNodeRegistry (implements NodeRegistry)
  ├── register_node(node: Node) → Result<()>
  │     Calls NodeService.RegisterNode RPC.
  │
  └── (heartbeat via GrpcHeartbeatSink)

GrpcHeartbeatSink (implements HeartbeatSink)
  └── send(heartbeat) → Result<()>
        Calls NodeService.Heartbeat RPC.
```

## Agent Command Enum

Internal commands for the agent state machine:

```
AgentCommand
  ├── StartAllocation { id, entrypoint }
  ├── StopAllocation { id }
  ├── Checkpoint { id }
  ├── Drain
  ├── Undrain
  └── UpdateTelemetryMode(TelemetryMode)
```
