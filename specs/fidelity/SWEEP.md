# Sweep Plan
Status: COMPLETE
Started: 2026-03-20

## Surface

| Type | Count | Assessed | Remaining |
|------|-------|----------|-----------|
| BDD feature files | 31 | 10 | 21 |
| BDD scenarios | 321 | 122 | 199 |
| Trait boundaries | 35 | 27 | 8 |
| ADRs | 23 | 15 | 8 |
| Unit/integration tests | 1423 | 466 | 957 |
| Source modules | 165 | 43 | 122 |

## Chunks (ordered by risk)

| # | Scope | Specs | Traits | Status | Session |
|---|-------|-------|--------|--------|---------|
| 1 | Consensus + State Machine | quorum (global_state, commands), types (state transitions) | Raft StateMachineState, NodeRegistry, AllocationStore | DONE | 2026-03-20 |
| 2 | Scheduling Core | cost, knapsack, cycle, loop_runner, autoscaler, resource_timeline | SchedulerStateReader, SchedulerCommandSink, VClusterScheduler, ScaleExecutor | DONE | 2026-03-20 |
| 3 | Node Agent Lifecycle | agent, allocation_runner, prologue, epilogue, heartbeat, health, liveness, probe_manager | Runtime, HeartbeatSink, HealthObserver, DataStageExecutor, SensitiveWiper | DONE | 2026-03-20 |
| 4 | Service Workloads | service_binpack, reconciler, service registry, endpoint registration | ServiceBinPack scoring, reconciliation logic, endpoint lifecycle | DONE | 2026-03-20 |
| 5 | Security + Auth | OIDC, RBAC, mTLS, sensitive workloads, audit log | OidcValidator, RbacPolicy | DONE | 2026-03-20 |
| 6 | Networking + Topology | network domains, VNI, GPU topology, memory topology, conformance | GpuDiscoveryProvider, MemoryDiscoveryProvider | DONE | 2026-03-20 |
| 7 | Data Plane | data staging, checkpointing, PMI-2, attach/PTY | CheckpointBroker, NodeAgentPool, PtyBackend, FenceTransport | DONE | 2026-03-20 |
| 8 | API + CLI + Client | gRPC services, REST routes, CLI commands, client SDK, proto conversion | TsdbClient, SovraClient, AccountingClient | DONE | 2026-03-20 |
| 9 | Observability + Telemetry | streaming, log buffer, metrics, eBPF, proc_collector | EbpfCollector, S3Sink | DONE | 2026-03-20 |
| 10 | Federation + Multi-tenant | federation broker, borrowing, quota, tenant management | FederationBroker, BorrowingBroker | DONE | 2026-03-20 |
| 11 | Cross-cutting | ADR enforcement, dead specs, orphan tests, coverage gaps, feature flags | — | DONE | 2026-03-20 |
