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
