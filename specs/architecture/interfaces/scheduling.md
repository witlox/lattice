# Scheduling Module Interfaces (lattice-scheduler)

## Core Interface: VClusterScheduler

```
trait VClusterScheduler: Send + Sync
  ├── schedule(input: SchedulingInput) → SchedulingResult
  │     Evaluate pending allocations, produce placement decisions.
  │     Pure function: no side effects, no state mutation.
  │
  └── name() → &str
        Human-readable scheduler name for diagnostics.
```

**Spec source:** Domain model (vCluster scheduler per type), INV-E1 (preemption ordering)

**Implementations:**
- `HpcBackfillScheduler` — Topology-aware placement, high wait_time + backlog weights
- `ServiceBinPackScheduler` — Homogeneous packing, high conformance weight
- `SensitiveReservationScheduler` — Dedicated nodes, no sharing, no borrowing (INV-S5)
- `InteractiveFifoScheduler` — Strict FIFO, fair-share only, no preemption

**Factory:** `create_scheduler(vcluster: &VCluster) → Box<dyn VClusterScheduler>`

## Cost Function

```
CostEvaluator
  └── score(alloc: &Allocation, ctx: &CostContext, weights: &CostWeights) → f64
        Score = Σ wᵢ · fᵢ(alloc, ctx) × budget_penalty(alloc, ctx)
```

**9 factors:**

| Factor | Input Source | Spec Reference |
|---|---|---|
| f1: priority_class | Allocation.preemption_class | INV-E1 |
| f2: wait_time | Allocation.created_at vs now | Anti-starvation |
| f3: fair_share_deficit | Tenant usage vs target | INV-E6 |
| f4: topology_fitness | Node topology + memory locality | Dragonfly groups, NUMA |
| f5: data_readiness | TSDB query (VAST hot tier %) | IP-06, data staging |
| f6: backlog_pressure | Queue depth (GPU-hours) | Scheduling algorithm |
| f7: energy_cost | TSDB query (electricity price) | IP-06 |
| f8: checkpoint_efficiency | Preemption cost model | Checkpoint broker |
| f9: conformance_fitness | Node fingerprint match | Conformance groups |

## Knapsack Solver

```
KnapsackSolver
  └── solve(
        pending: &[Allocation],
        available_nodes: &[Node],
        topology: &TopologyModel,
        ctx: &CostContext
      ) → SchedulingResult
```

**Algorithm:**
1. Score all pending allocations (descending)
2. For each: filter candidates (operational, unowned, constraints pass)
3. Select nodes: conformance-aware → topology-aware → fallback
4. Produce PlacementDecision (Place | Preempt | Defer)

**Contract:** Never places more nodes than available. Never violates INV-S1 (double-assignment). Defers allocations that cannot fit.

## Scheduling Cycle

```
run_cycle(input: SchedulingInput) → SchedulingResult
```

**Input:**
- Pending allocations from quorum state
- Available nodes from quorum state
- Topology model
- Cost context (tenant usage, TSDB metrics, energy price)
- vCluster config (weights, borrowing policy)

**Output:** `Vec<PlacementDecision>` — to be proposed to quorum.

## Placement Decision

```
PlacementDecision
  ├── Place { id: AllocId, nodes: Vec<NodeId> }
  ├── Preempt { id: AllocId, nodes: Vec<NodeId>, victims: Vec<AllocId> }
  └── Defer { id: AllocId, reason: String }
```

## Sub-Module Interfaces

### Preemption Evaluator

```
evaluate_preemption(
  requester: &Allocation,
  running: &[Allocation],
  nodes: &[Node]
) → Option<PreemptionPlan>
```

**Contract:** Only preempts class-N by class-(N+1)+. Sensitive (class 10) never preempted (INV-E1).

### DAG Controller

```
DagController
  ├── evaluate_edges(dag: &Dag, completed: &AllocId) → Vec<AllocId>
  │     Returns allocations whose dependencies are now satisfied.
  │
  └── validate_dag(dag: &Dag) → Result<()>
        Kahn's algorithm for cycle detection (INV-E2).
```

### Quota Evaluator

```
evaluate_quota(alloc: &Allocation, tenant: &Tenant) → QuotaResult
  QuotaResult: Allowed | HardQuotaExceeded(detail) | SoftQuotaWarning(detail)
```

### Walltime Enforcer

```
WalltimeEnforcer
  ├── register(id: AllocId, walltime: Duration)
  ├── check_expired() → Vec<AllocId>
  └── cancel(id: &AllocId)
```

**Contract:** INV-E4 — walltime takes priority over checkpoints.

### Autoscaler

```
Autoscaler
  └── evaluate(alloc: &Allocation, current_metric: f64) → ScaleDecision
        ScaleDecision: ScaleUp(count) | ScaleDown(count) | NoAction
```

### Borrowing Broker

```
BorrowingBroker
  ├── find_borrowable(vcluster: &VCluster, needed: usize) → Vec<NodeId>
  └── reclaim(vcluster: &VCluster, needed: usize) → Vec<(NodeId, AllocId)>
```

**Contract:** Sensitive vCluster never participates (INV-S5).

### Federation Broker

```
FederationBroker (feature-gated)
  ├── evaluate_offer(request: &Allocation, sites: &[SiteCatalog]) → Option<SiteId>
  └── receive_remote(request: SignedRequest) → Result<Allocation>
```

**Contract:** INV-N3 — rejects sensitive data federation.
