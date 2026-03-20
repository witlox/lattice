# Sweep Plan
Status: IN PROGRESS
Started: 2026-03-20

## Surface

| Type | Count | Assessed | Remaining |
|------|-------|----------|-----------|
| BDD feature files | 31 | 3 | 28 |
| BDD scenarios | 321 | 42 | 279 |
| Trait boundaries | 35 | 17 | 18 |
| ADRs | 23 | 4 | 19 |
| Unit/integration tests | 1423 | 93 | 1330 |
| Source modules | 165 | 12 | 153 |

## Chunks (ordered by risk)

| # | Scope | Specs | Traits | Status | Session |
|---|-------|-------|--------|--------|---------|
| 1 | Consensus + State Machine | quorum (global_state, commands), types (state transitions) | Raft StateMachineState, NodeRegistry, AllocationStore | DONE | 2026-03-20 |
| 2 | Scheduling Core | cost, knapsack, cycle, loop_runner, autoscaler, resource_timeline | SchedulerStateReader, SchedulerCommandSink, VClusterScheduler, ScaleExecutor | PENDING | — |
| 3 | Node Agent Lifecycle | agent, allocation_runner, prologue, epilogue, heartbeat, health, liveness, probe_manager | Runtime, HeartbeatSink, HealthObserver, DataStageExecutor, SensitiveWiper | PENDING | — |
| 4 | Service Workloads | service_binpack, reconciler, service registry, endpoint registration | ServiceBinPack scoring, reconciliation logic, endpoint lifecycle | PENDING | — |
| 5 | Security + Auth | OIDC, RBAC, mTLS, sensitive workloads, audit log | OidcValidator, RbacPolicy | PENDING | — |
| 6 | Networking + Topology | network domains, VNI, GPU topology, memory topology, conformance | GpuDiscoveryProvider, MemoryDiscoveryProvider | PENDING | — |
| 7 | Data Plane | data staging, checkpointing, PMI-2, attach/PTY | CheckpointBroker, NodeAgentPool, PtyBackend, FenceTransport | PENDING | — |
| 8 | API + CLI + Client | gRPC services, REST routes, CLI commands, client SDK, proto conversion | TsdbClient, SovraClient, AccountingClient | PENDING | — |
| 9 | Observability + Telemetry | streaming, log buffer, metrics, eBPF, proc_collector | EbpfCollector, S3Sink | PENDING | — |
| 10 | Federation + Multi-tenant | federation broker, borrowing, quota, tenant management | FederationBroker, BorrowingBroker | PENDING | — |
| 11 | Cross-cutting | ADR enforcement, dead specs, orphan tests, coverage gaps, feature flags | — | PENDING | — |
