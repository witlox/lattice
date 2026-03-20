# API Gateway Interfaces (lattice-api)

## Composition Root: ApiState

The API server is a composition root — it wires trait implementations together. All business logic is delegated to the trait objects.

```
ApiState
  ├── allocations: Arc<dyn AllocationStore>        ── lattice-quorum
  ├── nodes: Arc<dyn NodeRegistry>                 ── lattice-quorum
  ├── audit: Arc<dyn AuditLog>                     ── lattice-quorum
  ├── checkpoint: Arc<dyn CheckpointBroker>        ── lattice-checkpoint
  ├── quorum: Option<Arc<QuorumClient>>            ── lattice-quorum (direct)
  ├── data_dir: Option<PathBuf>                    ── backup path
  ├── events: Arc<EventBus>                        ── in-process streaming
  ├── tsdb: Option<Arc<dyn TsdbClient>>            ── VictoriaMetrics
  ├── storage: Option<Arc<dyn StorageService>>     ── VAST
  ├── accounting: Option<Arc<dyn AccountingService>>── Waldur (feature-gated)
  ├── oidc: Option<Arc<dyn OidcValidator>>         ── OIDC (feature-gated)
  ├── rate_limiter: Option<Arc<RateLimiter>>       ── token bucket
  ├── sovra: Option<Arc<dyn SovraClient>>          ── Sovra (feature-gated)
  ├── pty: Option<Arc<dyn PtyBackend>>             ── attach sessions
  └── agent_pool: Option<Arc<dyn NodeAgentPool>>   ── checkpoint delivery
```

**Design principle:** Every `Option` field is a feature-gated or deployment-role-gated capability. The API server functions with `None` for any optional field — degraded but not broken.

## gRPC Services

### AllocationService (21 RPCs)

| RPC | Request | Response | Streaming | Key Invariants |
|---|---|---|---|---|
| Submit | SubmitRequest (single/dag/taskgroup) | SubmitResponse | - | INV-E2 (DAG acyclic), INV-C5 (DAG size), INV-S2 (quota) |
| Get | GetAllocationRequest | AllocationStatus | - | - |
| List | ListAllocationsRequest | ListAllocationsResponse | - | RBAC: tenant-scoped |
| Cancel | CancelRequest | CancelResponse | - | RBAC: owner or admin |
| Update | UpdateAllocationRequest | AllocationStatus | - | Walltime extension only |
| LaunchTasks | LaunchTasksRequest | LaunchTasksResponse | - | Fans out to node agents |
| Watch | WatchRequest | AllocationEvent | Server stream | EventBus subscription |
| Checkpoint | CheckpointRequest | CheckpointResponse | - | INV-S3 (audit if sensitive) |
| Attach | AttachInput | AttachOutput | Bidirectional | INV-C2, INV-O3 (audit) |
| StreamLogs | LogStreamRequest | LogEntry | Server stream | RBAC: owner or admin |
| QueryMetrics | QueryMetricsRequest | MetricsSnapshot | - | TSDB query |
| StreamMetrics | StreamMetricsRequest | MetricsEvent | Server stream | Fan-out to agents |
| GetDiagnostics | DiagnosticsRequest | DiagnosticsResponse | - | RBAC: owner or admin |
| CompareMetrics | CompareMetricsRequest | CompareMetricsResponse | - | INV: no cross-tenant sensitive |
| GetDag | GetDagRequest | DagStatus | - | - |
| ListDags | ListDagsRequest | ListDagsResponse | - | RBAC: tenant-scoped |
| CancelDag | CancelDagRequest | CancelDagResponse | - | Cancels all allocations |

### AllocationService — Session RPCs (3 RPCs)

| RPC | Request | Response | Key Invariants |
|---|---|---|---|
| CreateSession | CreateSessionRequest | SessionResponse | RBAC: user (own allocation) |
| GetSession | GetSessionRequest | SessionResponse | RBAC: user |
| DeleteSession | DeleteSessionRequest | DeleteSessionResponse | RBAC: user (own session) |

### NodeService (9 RPCs)

| RPC | Request | Response | Key Invariants |
|---|---|---|---|
| ListNodes | ListNodesRequest | ListNodesResponse | RBAC: filtered by role |
| GetNode | GetNodeRequest | NodeStatus | - |
| DrainNode | DrainNodeRequest | DrainNodeResponse | RBAC: operator+ |
| UndrainNode | UndrainNodeRequest | UndrainNodeResponse | RBAC: operator+ |
| DisableNode | DisableNodeRequest | DisableNodeResponse | RBAC: operator+ |
| EnableNode | EnableNodeRequest | EnableNodeResponse | RBAC: operator+ (Down → Ready) |
| RegisterNode | RegisterNodeRequest | RegisterNodeResponse | Raft-committed (IP-03) |
| Heartbeat | HeartbeatRequest | HeartbeatResponse | Sequence check, Raft-committed for state changes |
| Health | HealthRequest | HealthResponse | Unauthenticated (health check) |

### AdminService (15 RPCs)

| RPC | Request | Response | Key Invariants |
|---|---|---|---|
| CreateTenant | CreateTenantRequest | TenantResponse | RBAC: system-admin |
| UpdateTenant | UpdateTenantRequest | TenantResponse | INV-S2 (quota update) |
| ListTenants | ListTenantsRequest | ListTenantsResponse | RBAC: user |
| GetTenant | GetTenantRequest | TenantResponse | RBAC: user |
| CreateVCluster | CreateVClusterRequest | VClusterResponse | RBAC: tenant-admin+ |
| UpdateVCluster | UpdateVClusterRequest | VClusterResponse | RBAC: tenant-admin+ |
| ListVClusters | ListVClustersRequest | ListVClustersResponse | RBAC: user |
| GetVCluster | GetVClusterRequest | VClusterResponse | RBAC: user |
| GetVClusterQueue | GetVClusterQueueRequest | VClusterQueueResponse | RBAC: user |
| QueryAudit | QueryAuditRequest | QueryAuditResponse | RBAC: tenant-admin+ |
| GetAccountingUsage | GetAccountingUsageRequest | AccountingUsageResponse | RBAC: user |
| GetRaftStatus | GetRaftStatusRequest | RaftStatusResponse | RBAC: system-admin |
| BackupVerify | BackupVerifyRequest | BackupVerifyResponse | RBAC: system-admin |
| CreateBackup | CreateBackupRequest | CreateBackupResponse | RBAC: system-admin |
| RestoreBackup | RestoreBackupRequest | RestoreBackupResponse | RBAC: system-admin |

## Middleware Stack

Request processing order (per-request):

```
Incoming request
  │
  ├─1─► Rate Limiter (if enabled)
  │       Token bucket per user identity.
  │       429 Too Many Requests on exhaustion.
  │
  ├─2─► OIDC Validator (if enabled)
  │       Bearer token → JWKS verification → claims extraction.
  │       401 Unauthorized on invalid/expired token.
  │
  ├─3─► RBAC Enforcer (follows OIDC)
  │       Claims → role derivation → permission check (38 operations).
  │       403 Forbidden on insufficient role.
  │       Roles: user, tenant-admin, system-admin, claiming-user, operator, read-only.
  │
  └─4─► Handler
          Business logic delegation to trait objects.
```

**Spec source:** Tenant & Access context, RBAC operations in ubiquitous-language.md

## REST Routes

35+ routes mirroring gRPC surface via axum:

```
POST   /api/v1/allocations           → Submit
GET    /api/v1/allocations           → List
GET    /api/v1/allocations/:id       → Get
PATCH  /api/v1/allocations/:id       → Update
POST   /api/v1/allocations/:id/cancel     → Cancel
POST   /api/v1/allocations/:id/checkpoint → Checkpoint
GET    /api/v1/allocations/:id/logs       → StreamLogs
GET    /api/v1/allocations/:id/metrics    → QueryMetrics
GET    /api/v1/allocations/:id/watch      → Watch (SSE)
POST   /api/v1/sessions              → Create session
GET    /api/v1/sessions/:id          → Get session
DELETE /api/v1/sessions/:id          → Delete session
POST   /api/v1/dags                  → Submit DAG
GET    /api/v1/dags                  → List DAGs
GET    /api/v1/dags/:id              → Get DAG
POST   /api/v1/dags/:id/cancel       → Cancel DAG
GET    /api/v1/audit                 → Query audit log
POST   /api/v1/tenants              → Create tenant
GET    /api/v1/tenants              → List tenants
GET    /api/v1/tenants/:id          → Get tenant
PUT    /api/v1/tenants/:id          → Update tenant
POST   /api/v1/vclusters            → Create vCluster
GET    /api/v1/vclusters            → List vClusters
GET    /api/v1/vclusters/:id        → Get vCluster
GET    /api/v1/vclusters/:id/queue  → Get vCluster queue
GET    /api/v1/accounting/usage     → Accounting usage
GET    /api/v1/nodes                → List nodes
GET    /api/v1/nodes/:id            → Get node
POST   /api/v1/nodes/:id/drain      → Drain
POST   /api/v1/nodes/:id/undrain    → Undrain
POST   /api/v1/nodes/:id/enable     → Enable (re-enable disabled node)
GET    /api/v1/diagnostics/:id      → Get diagnostics
POST   /api/v1/admin/backup         → Create backup
POST   /api/v1/admin/backup/verify  → Verify backup
POST   /api/v1/admin/backup/restore → Restore backup
GET    /api/v1/raft/status          → Raft status
GET    /healthz                     → Health check
GET    /metrics                     → Prometheus metrics
```

## EventBus

```
EventBus
  ├── publish(event: AllocationEvent)
  │     Non-blocking. Dropped if no subscribers.
  │
  └── subscribe(id: &AllocId) → Receiver<AllocationEvent>
        Per-allocation subscription for Watch/SSE endpoints.
```

## Server Configuration

```
serve(state: ApiState, config: ServerConfig) → Result<()>
  Dual-listen: gRPC on grpc_addr, REST on rest_addr.
  Optional TLS (cert + key + optional CA for mTLS).
```
