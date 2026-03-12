# Consensus Module Interfaces (lattice-quorum)

## QuorumClient

The primary interface to the Raft state machine. All state mutations flow through `propose()`.

```
QuorumClient
  ├── propose(cmd: Command) → Result<CommandResponse, LatticeError>
  │     Blocks until Raft-committed. Returns committed state.
  │     Fails: QuorumUnavailable, LeaderElection, ProposalRejected
  │
  ├── raft() → &Raft<TypeConfig>
  │     Access underlying openraft instance for diagnostics.
  │
  └── state() → &Arc<RwLock<GlobalState>>
        Read committed state snapshot. Eventually consistent
        (follower may lag leader by bounded amount).
```

**Spec source:** IP-01 (proposals), IP-02 (notifications), IP-03 (heartbeats), IP-07 (quotas)

## Trait Implementations

QuorumClient implements three traits from lattice-common:

### NodeRegistry

```
async fn get_node(id: &NodeId) → Result<Node>
async fn list_nodes(filter: Option<NodeState>) → Result<Vec<Node>>
async fn update_node_state(id: &NodeId, state: NodeState, reason: Option<String>) → Result<()>
async fn claim_node(id: &NodeId, ownership: NodeOwnership) → Result<()>
async fn release_node(id: &NodeId) → Result<()>
async fn register_node(node: Node) → Result<()>
```

**Contract:** `claim_node` enforces INV-S1 (exclusive ownership) and INV-S5 (sensitive isolation). Rejects with `LatticeError::OwnershipConflict` if node already owned.

### AllocationStore

```
async fn insert(alloc: Allocation) → Result<()>
async fn get(id: &AllocId) → Result<Allocation>
async fn update_state(id: &AllocId, state: AllocationState, msg: Option<String>, exit: Option<i32>) → Result<()>
async fn list(filter: AllocationFilter) → Result<Vec<Allocation>>
async fn count_by_tenant(tenant: &TenantId) → Result<usize>
```

**Contract:** `insert` enforces INV-S2 (quota check) and INV-C4 (vCluster binding set at creation).

### AuditLog

```
async fn record(entry: AuditEntry) → Result<()>
async fn query(filter: AuditFilter) → Result<Vec<AuditEntry>>
```

**Contract:** `record` is Raft-committed (INV-S3, INV-S4). Append-only — no delete/update variant exists. Must succeed before sensitive action proceeds (INV-O3).

## Command Enum

All state mutations are expressed as commands:

```
Command
  ├── SubmitAllocation(Allocation)
  ├── UpdateAllocationState { id, state, message, exit_code }
  ├── AssignNodes { id, nodes: Vec<NodeId> }
  ├── RegisterNode(Node)
  ├── UpdateNodeState { id, state, reason }
  ├── ClaimNode { id, ownership: NodeOwnership }
  ├── ReleaseNode { id }
  ├── RecordHeartbeat { id, timestamp }
  ├── CreateTenant(Tenant)
  ├── UpdateTenant { id, quota, isolation_level }
  ├── CreateVCluster(VCluster)
  ├── UpdateVCluster { id, cost_weights, allow_borrowing, allow_lending }
  ├── UpdateTopology(TopologyModel)
  ├── SetSensitivePoolSize(Option<u32>)
  └── RecordAudit(AuditEntry)
```

**Validation rules per command:**

| Command | Invariants Checked |
|---|---|
| AssignNodes | INV-S1 (exclusive), INV-S2 (quota), INV-S5 (sensitive) |
| ClaimNode | INV-S1, INV-S5 |
| SubmitAllocation | INV-C4 (vCluster binding) |
| RecordAudit | INV-S4 (append-only) |
| UpdateTenant | INV-S2 (new quota ≥ 0, reduction doesn't preempt) |

## Factory Functions

```
create_quorum_from_config(config: &QuorumConfig) → Result<(QuorumClient, Option<JoinHandle>)>
create_test_quorum() → QuorumClient
create_test_cluster(n: usize) → Vec<QuorumClient>
create_test_grpc_cluster(n: usize) → (Vec<QuorumClient>, Vec<JoinHandle>, Vec<String>)
```

## Backup Interface

```
export_backup(client: &QuorumClient, path: &Path) → Result<()>
restore_backup(state: &Arc<RwLock<GlobalState>>, path: &Path) → Result<()>
verify_backup(path: &Path) → Result<BackupMetadata>
```

**Contract:** `verify_backup` returns metadata (entity counts, timestamp, snapshot index) for integrity validation without loading state.
