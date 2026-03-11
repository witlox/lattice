# MPI Process Management

## Design Principle

Lattice must launch and manage multi-node MPI processes without relying on SSH between compute nodes. The node agent provides process management infrastructure (PMI) so that MPI implementations (OpenMPI, MPICH, Cray MPICH) can perform rank discovery and key-value exchange through Lattice rather than through SSH or a Slurm-specific launcher.

## Problem Statement

In Slurm, `srun` serves as both a process launcher (fan-out to nodes) and a PMI server (rank discovery, KV exchange). Lattice replaces `srun` with `lattice launch` / the `LaunchTasks` RPC, but the current implementation is a stub that does not:

1. Fan out process launch to node agents
2. Provide PMI wire-up so MPI ranks can discover each other
3. Manage CXI credentials for Slingshot/Ultra Ethernet fabric access

Without this, users calling `mpirun` directly fall back to SSH for remote process spawning, which is:
- A security risk (SSH keys between compute nodes)
- Incompatible with network-domain-only L3 reachability
- Incompatible with the sensitive workload isolation model
- Operationally fragile (SSH host key management, authorized_keys distribution)

## Supported MPI Implementations

| Implementation | PMI-2 Support | PMIx Support | Default Launcher | Notes |
|---|---|---|---|---|
| **MPICH** | Native (PMI-2 origin) | Via external PMIx | Hydra (SSH) | PMI-2 is the natural fit |
| **OpenMPI** | Yes (`OMPI_MCA_pmix=pmi2`) | Preferred (PRRTE) | ORTE/PRRTE (SSH) | PMI-2 fully functional |
| **Cray MPICH** | Native (via PALS) | Via PALS | PALS | PMI-2 without PALS works |

All three support PMI-2. PMIx is preferred by OpenMPI but not required.

## Architecture

### Two-Tier Design

```
┌─────────────────────────────────────────────────────────┐
│  Default: Native PMI-2 Server (built into node agent)   │
│  Simple, no external dependencies, covers 95%+ of MPI   │
│  workloads. ~8 wire commands over Unix domain socket.    │
├─────────────────────────────────────────────────────────┤
│  Optional: OpenPMIx Sidecar (feature-flagged)           │
│  Full PMIx v4/v5 support for workloads that require     │
│  PMIx-specific features (spawn, tools API, events).     │
│  Node agent manages OpenPMIx server lifecycle.           │
└─────────────────────────────────────────────────────────┘
```

### Launch Flow

```
User: lattice launch --alloc=123 -n 256 --tasks-per-node=4 ./my_mpi_app

  │
  ▼
lattice-api (LaunchTasks RPC)
  │
  ├─ Validates: allocation is Running, user owns it
  ├─ Computes rank layout: N nodes × tasks_per_node = total ranks
  │   Rank assignment: node 0 gets ranks [0..3], node 1 gets [4..7], ...
  ├─ Generates launch_id, PMI job attributes (appnum, size, universe_size)
  ├─ Provisions CXI credentials if Slingshot fabric (see below)
  │
  ▼ Fan-out: gRPC LaunchProcesses to each node agent in the allocation

Node Agent 0                 Node Agent 1                 Node Agent N-1
  │                            │                            │
  ├─ Creates PMI-2 server      ├─ Creates PMI-2 server      ├─ ...
  │  (Unix domain socket)      │  (Unix domain socket)      │
  │                            │                            │
  ├─ Spawns local ranks        ├─ Spawns local ranks        │
  │  rank 0: ./my_mpi_app     │  rank 4: ./my_mpi_app     │
  │  rank 1: ./my_mpi_app     │  rank 5: ./my_mpi_app     │
  │  rank 2: ./my_mpi_app     │  rank 6: ./my_mpi_app     │
  │  rank 3: ./my_mpi_app     │  rank 7: ./my_mpi_app     │
  │                            │                            │
  │  Each rank inherits:       │                            │
  │  - PMI_FD (socket fd)      │                            │
  │  - PMI_RANK (global rank)  │                            │
  │  - PMI_SIZE (world size)   │                            │
  │                            │                            │
  ▼                            ▼                            ▼
  MPI_Init() → PMI-2 fullinit → local KVS puts (libfabric endpoint addr)
  │                            │                            │
  ▼ ─────────── kvsfence (cross-node KVS exchange via gRPC) ────────────
  │                            │                            │
  MPI_Init() completes         MPI_Init() completes         ...
  │                            │                            │
  (application runs)           (application runs)           ...
  │                            │                            │
  MPI_Finalize() → PMI-2 finalize
```

### PMI-2 Wire Protocol

The PMI-2 wire protocol is text-based over a Unix domain socket. The node agent implements these commands:

| Command | Direction | Purpose |
|---|---|---|
| `fullinit` | rank → agent | Initialize PMI connection, receive rank/size/appnum |
| `job-getinfo` | rank → agent | Query job attributes (e.g., universe size) |
| `kvsput` | rank → agent | Store a key-value pair (e.g., libfabric endpoint address) |
| `kvsget` | rank → agent | Retrieve a key-value pair |
| `kvsfence` | rank → agent | Barrier + distribute all KV pairs across all ranks |
| `finalize` | rank → agent | Clean shutdown of PMI connection |
| `abort` | rank → agent | Signal abnormal termination |
| `spawn` | rank → agent | Dynamic process spawning (optional, rarely used) |

### Cross-Node KVS Exchange (Fence)

The `kvsfence` operation is the only cross-node PMI operation. It requires all ranks across all nodes to synchronize and exchange accumulated KV pairs. This is implemented via gRPC between node agents:

```
kvsfence triggered on all nodes
  │
  ▼
Phase 1: Local collection
  Each node agent collects all kvsput entries from its local ranks.

Phase 2: Exchange (star topology via designated head node)
  ┌─────────────┐
  │ Head Agent   │ ◄──── gRPC PmiFence(local_kvs) ──── Agent 1
  │ (rank 0's   │ ◄──── gRPC PmiFence(local_kvs) ──── Agent 2
  │  node)       │ ◄──── gRPC PmiFence(local_kvs) ──── Agent N-1
  │              │
  │ Merges all   │
  │ KVS entries  │
  │              │
  │ Broadcasts   │ ────► gRPC PmiFenceComplete(merged_kvs) ──► Agent 1
  │ merged KVS   │ ────► gRPC PmiFenceComplete(merged_kvs) ──► Agent 2
  │              │ ────► gRPC PmiFenceComplete(merged_kvs) ──► Agent N-1
  └─────────────┘

Phase 3: Local completion
  Each node agent unblocks its local ranks' kvsfence.
  Ranks can now kvsget any key from any node.
```

The head agent is the node agent hosting rank 0. For large jobs (>128 nodes), a tree-based reduction can be used instead of a star to reduce head-node pressure.

### Node Agent gRPC Extensions

New RPCs on the node agent service for MPI process management:

```protobuf
service NodeAgentService {
  // Existing RPCs...

  // Launch MPI ranks on this node (called by API server during fan-out)
  rpc LaunchProcesses(LaunchProcessesRequest) returns (LaunchProcessesResponse);

  // PMI fence exchange between node agents
  rpc PmiFence(PmiFenceRequest) returns (PmiFenceResponse);

  // PMI fence completion broadcast from head agent
  rpc PmiFenceComplete(PmiFenceCompleteRequest) returns (PmiFenceCompleteResponse);

  // Notify all local ranks to abort (e.g., one node failed)
  rpc AbortProcesses(AbortProcessesRequest) returns (AbortProcessesResponse);
}

message LaunchProcessesRequest {
  string launch_id = 1;
  string allocation_id = 2;
  string entrypoint = 3;
  repeated string args = 4;
  uint32 tasks_per_node = 5;
  uint32 first_rank = 6;        // global rank offset for this node
  uint32 world_size = 7;        // total ranks across all nodes
  map<string, string> env = 8;  // additional env vars
  PmiMode pmi_mode = 9;         // PMI2 (default) or PMIX
  // CXI credentials for Slingshot fabric
  optional CxiCredentials cxi_credentials = 10;
  // Peer node agents for fence exchange
  repeated PeerInfo peers = 11;
  // Index of the head node (for fence coordination)
  uint32 head_node_index = 12;
}

message PeerInfo {
  string node_id = 1;
  string grpc_address = 2;  // node agent address (reachable via management network)
  uint32 first_rank = 3;
  uint32 num_ranks = 4;
}

enum PmiMode {
  PMI2 = 0;
  PMIX = 1;
}

message CxiCredentials {
  uint32 vni = 1;
  bytes auth_key = 2;
  uint32 svc_id = 3;
}
```

### PMI-2 Server Implementation

Each node agent runs a PMI-2 server per launch (one Unix socket per `launch_id`):

```
Node Agent
  │
  ├─ LaunchProcesses received
  │   ├─ Create Unix socket: /tmp/lattice-pmi-{launch_id}.sock
  │   ├─ Start PMI-2 server task (tokio)
  │   ├─ Fork/exec ranks with:
  │   │   PMI_FD={fd}           # inherited socket fd
  │   │   PMI_RANK={rank}       # global rank
  │   │   PMI_SIZE={world_size} # world size
  │   │   PMI_SPAWNED=0         # not dynamically spawned
  │   │   LATTICE_LAUNCH_ID={launch_id}
  │   │   LATTICE_ALLOC_ID={allocation_id}
  │   │   LATTICE_NODELIST={comma-separated node list}
  │   │   LATTICE_NNODES={node_count}
  │   │   LATTICE_NPROCS={world_size}
  │   │   # CXI env (if Slingshot):
  │   │   FI_CXI_DEFAULT_VNI={vni}
  │   │   FI_CXI_AUTH_KEY={key}
  │   └─ Monitor all rank processes, report exit status
  │
  ├─ PMI-2 server handles:
  │   ├─ fullinit → return rank, size, appnum, debug flag
  │   ├─ kvsput → store in local HashMap
  │   ├─ kvsget → lookup local, or merged (post-fence)
  │   ├─ kvsfence → collect local, trigger cross-node exchange, block until complete
  │   ├─ finalize → mark rank done
  │   └─ abort → signal all local ranks, notify head agent
  │
  └─ Cleanup on launch completion
      ├─ Remove Unix socket
      ├─ Report per-rank exit codes to API server
      └─ Clean up CXI credentials
```

### Environment Variables

Lattice sets these environment variables for MPI processes:

| Variable | Value | Purpose |
|---|---|---|
| `PMI_FD` | fd number | PMI-2 socket (inherited) |
| `PMI_RANK` | global rank | MPI rank |
| `PMI_SIZE` | world size | MPI world size |
| `PMI_SPAWNED` | `0` | Not dynamically spawned |
| `LATTICE_LAUNCH_ID` | UUID | Launch identifier |
| `LATTICE_ALLOC_ID` | UUID | Allocation identifier |
| `LATTICE_NODELIST` | comma-separated | All nodes in this launch |
| `LATTICE_NNODES` | integer | Node count |
| `LATTICE_NPROCS` | integer | Total rank count |
| `LATTICE_LOCAL_RANK` | 0..tasks_per_node-1 | Node-local rank |
| `LATTICE_LOCAL_SIZE` | tasks_per_node | Ranks on this node |
| `FI_CXI_DEFAULT_VNI` | VNI number | Slingshot VNI (if applicable) |
| `FI_CXI_AUTH_KEY` | hex string | CXI auth key (if applicable) |
| `FI_PROVIDER` | `cxi` or `verbs` | libfabric provider hint |

For Slurm compatibility (`compat.set_slurm_env=true`), also set `SLURM_PROCID`, `SLURM_NPROCS`, `SLURM_LOCALID`, `SLURM_NODELIST`.

## CXI Credential Management (Slingshot)

On Slingshot systems, MPI communication requires CXI (Cassini eXtended Interface) credentials tied to the allocation's VNI. Without valid credentials, libfabric's CXI provider refuses to open endpoints.

### Credential Lifecycle

```
1. Allocation scheduled → network domain assigned → VNI allocated
2. LaunchTasks RPC → API server requests CXI credentials from fabric manager
   - Input: VNI, allocation ID, node list
   - Output: auth_key, svc_id (bound to VNI + node set)
3. Credentials included in LaunchProcessesRequest to each node agent
4. Node agent sets FI_CXI_DEFAULT_VNI and FI_CXI_AUTH_KEY for spawned ranks
5. On launch completion → API server revokes CXI credentials
```

### Fabric Manager Integration

The Slingshot fabric manager provides a REST API for credential management:

| Operation | Endpoint | When |
|---|---|---|
| Create CXI service | `POST /fabric/cxi/services` | Launch start |
| Get auth key | `GET /fabric/cxi/services/{id}/auth` | Launch start |
| Revoke CXI service | `DELETE /fabric/cxi/services/{id}` | Launch end |

This is a new integration point, similar to the existing VAST API integration for storage.

## Optional: OpenPMIx Sidecar (Feature-Flagged)

For workloads requiring full PMIx v4/v5 support (dynamic process spawning, PMIx tools API, event notification, PMIx groups), Lattice can run an OpenPMIx server as a managed sidecar process.

### When to Use PMIx Mode

| Scenario | PMI-2 (default) | PMIx (optional) |
|---|---|---|
| Standard MPI (init, communication, finalize) | Yes | Yes |
| Multi-application launch (MPMD) | Limited | Yes |
| Dynamic process spawning (`MPI_Comm_spawn`) | No | Yes |
| PMIx tools API (debugger attach) | No | Yes |
| PMIx event notification | No | Yes |
| OpenMPI with PMIx-only features | No | Yes |

### Architecture

```
Node Agent
  │
  ├─ PmiMode::PMIX requested in LaunchProcessesRequest
  │
  ├─ Spawns OpenPMIx server (pmix_server binary)
  │   ├─ Configured via tmpdir/pmix-{launch_id}/
  │   ├─ Node agent implements the PMIx "host" callback interface
  │   │   via a small C shim library (libpmix-lattice-host.so)
  │   │   that calls back to the node agent via Unix socket
  │   ├─ Cross-node exchange: host callbacks route to node agent gRPC
  │   └─ pmix_server provides Unix rendezvous socket for ranks
  │
  ├─ Spawns ranks with:
  │   PMIX_SERVER_URI={rendezvous_uri}
  │   PMIX_NAMESPACE={launch_id}
  │   PMIX_RANK={rank}
  │   (instead of PMI_FD/PMI_RANK/PMI_SIZE)
  │
  └─ On completion: stops pmix_server, cleans up
```

### Host Callback Shim

The OpenPMIx server requires the host (resource manager) to provide certain callbacks for cross-node operations. These are implemented via a small C shared library (`libpmix-lattice-host.so`) that:

1. Is loaded by `pmix_server` at startup via `--host-lib` or `LD_PRELOAD`
2. Implements: `pmix_server_fencenb_fn`, `pmix_server_dmodex_fn`, `pmix_server_spawn_fn`
3. Each callback sends a request over a Unix socket to the node agent
4. Node agent handles cross-node coordination via gRPC (same as PMI-2 fence)

This keeps the C code minimal (~200 lines) while leveraging the full OpenPMIx implementation.

### Build and Deployment

```toml
# Cargo.toml (lattice-node-agent)
[features]
pmix = []  # enables PMIx sidecar support
```

When the `pmix` feature is enabled:
- `pmix_server` binary must be installed on compute nodes (packaged separately or via uenv)
- `libpmix-lattice-host.so` is built from `infra/pmix-host/` and installed alongside the node agent
- The node agent detects `pmix_server` availability at startup and reports it as a node capability

When disabled: `PmiMode::PMIX` requests return an error with a clear message.

## Integration with Existing Runtimes

### uenv Runtime

PMI-2 socket and environment variables are available inside the mount namespace with no special handling (mount namespace does not isolate Unix sockets in the parent namespace).

### Sarus Runtime

The PMI-2 Unix socket must be bind-mounted into the container:
```
sarus run --mount=type=bind,source=/tmp/lattice-pmi-{launch_id}.sock,destination=/tmp/lattice-pmi.sock ...
```

The `--mpi` flag in Sarus already handles MPI wire-up for Slurm; for Lattice, we configure Sarus to use the Lattice-provided PMI socket instead. This requires the Sarus MPI hook to be configured for PMI-2 mode rather than Slurm PMI mode.

### DMTCP (Checkpoint/Restart)

DMTCP wraps the MPI process. The PMI-2 socket is outside the DMTCP checkpoint boundary. On restart, the node agent creates a new PMI-2 server and the restarted ranks re-initialize PMI. DMTCP's MPI plugin handles reconnecting MPI communicators.

## Failure Handling

### Rank Failure

```
1. Rank exits with non-zero code (or is killed by signal)
2. Local node agent detects via process monitor
3. Node agent sends RankFailed notification to head agent
4. Head agent:
   a. If allocation requeue policy = "on_any_failure": abort all ranks, requeue allocation
   b. If MPI_ERRORS_RETURN semantics: notify remaining ranks via PMI-2 abort
   c. Default: abort all ranks, report failure to API server
```

### Node Agent Failure

```
1. Node agent crashes or becomes unreachable
2. Head agent detects via gRPC timeout during fence (or heartbeat miss)
3. Head agent aborts the launch on all surviving nodes
4. API server handles allocation state transition (same as node failure)
```

### Fence Timeout

```
1. kvsfence does not complete within timeout (default: 60s, configurable)
2. Head agent declares fence failure
3. All ranks aborted with PMI-2 abort message
4. Launch reported as failed with "PMI fence timeout" reason
```

## User-Facing Changes

### `lattice launch` (CLI)

```bash
# MPI launch (replaces srun -n 256 ./app)
lattice launch --alloc=123 -n 256 ./my_mpi_app

# With tasks-per-node control
lattice launch --alloc=123 --tasks-per-node=4 ./my_mpi_app

# Force PMIx mode (requires pmix feature on nodes)
lattice launch --alloc=123 -n 256 --pmi=pmix ./my_mpi_app

# Launch with environment variables
lattice launch --alloc=123 -n 256 --env OMP_NUM_THREADS=8 ./my_mpi_app
```

### Submission Script

```bash
#!/bin/bash
#LATTICE nodes=64
#LATTICE walltime=2:00:00
#LATTICE vcluster=hpc-batch
#LATTICE network_domain=my-training-run

# No SSH, no mpirun, no srun needed.
# The entrypoint IS the MPI program; Lattice handles process launch and PMI.
lattice launch -n 256 --tasks-per-node=4 ./my_mpi_training

# Or for Slurm compatibility:
# srun -n 256 ./my_mpi_training   (compat layer translates to lattice launch)
```

### Direct `mpirun` (Escape Hatch)

Users who want to call `mpirun` directly can still do so. Lattice provides a Hydra-compatible launcher script (`lattice-mpi-launcher`) that uses the node agent gRPC instead of SSH:

```bash
# mpirun detects the Lattice launcher via:
#   HYDRA_LAUNCHER=manual
#   HYDRA_LAUNCHER_EXEC=lattice-mpi-launcher
# These are set automatically by the node agent when an allocation starts.

# So this "just works" inside an allocation:
mpirun -np 256 ./my_mpi_app
```

The `lattice-mpi-launcher` script:
1. Receives the launch command from Hydra/ORTE
2. Calls the local node agent's `LaunchProcesses` gRPC to spawn on the target node
3. Returns the PID to the MPI launcher

This provides backward compatibility for scripts that use `mpirun` directly while still avoiding SSH.

## Performance Considerations

| Operation | Latency | Bottleneck | Mitigation |
|---|---|---|---|
| Launch fan-out | ~100ms for 256 nodes | gRPC round-trips | Parallel fan-out from API server |
| PMI-2 fence (star) | ~10ms for <128 nodes | Head agent merge | Acceptable for typical HPC |
| PMI-2 fence (tree) | ~20ms for 1000+ nodes | Tree depth (log N) | Only needed at extreme scale |
| CXI credential provisioning | ~50ms | Fabric manager API | Cached for allocation lifetime |

MPI_Init typically takes 100-500ms. The Lattice PMI overhead is well within this budget.

## Cross-References

- [network-domains.md](network-domains.md) -- VNI allocation, L3 reachability
- [security.md](security.md) -- CXI credentials, network isolation
- [slurm-migration.md](slurm-migration.md) -- srun replacement
- [node-lifecycle.md](node-lifecycle.md) -- Node agent process management
- [failure-modes.md](failure-modes.md) -- Rank and node failure handling
- [checkpoint-broker.md](checkpoint-broker.md) -- DMTCP + MPI checkpoint interaction
- [sessions.md](sessions.md) -- Interactive allocations with MPI launch
- ADR-010: Native PMI-2 with optional PMIx sidecar
