# Security Architecture

## Design Principle

Defense in depth with zero-trust internal communication. Every component authenticates to every other component. Trust boundaries are explicit and enforced by mTLS, RBAC, and network segmentation.

## Trust Boundaries

```
User ──OIDC──→ lattice-api (direct, via hpc-auth) ──mTLS──→ quorum
                                        │                    │
                                        │ mTLS               │ mTLS
                                        ▼                    ▼
                                   node-agents ──namespace──→ workloads
                                        │
                                        │ mTLS/REST
                                        ▼
                                   VAST / OpenCHAMI

Federation (optional):
  quorum ──Sovra mTLS──→ federation-broker ──Sovra mTLS──→ remote quorum
```

## STRIDE Threat Analysis

### Spoofing

| Boundary | Attack | Mitigation |
|----------|--------|------------|
| User → lattice-api | Stolen OIDC token | Short-lived tokens (5 min), token binding to client cert, MFA enforcement at IdP |
| Internal services | Rogue node agent | mTLS with site PKI (OpenCHAMI OPAAL-issued certificates). Node agents receive certs during boot via cloud-init. Cert CN must match node identity in quorum. |
| Federation | Rogue remote site | Sovra workspace-scoped certificates. Each site's identity is cryptographically bound to its Sovra workspace. Revocable. |

### Tampering

| Boundary | Attack | Mitigation |
|----------|--------|------------|
| Quorum ↔ node agent | Fake heartbeat / state update | mTLS + message signing. Heartbeats include monotonic sequence number — replay detection. |
| uenv images | Compromised image | Image signing with site PKI (or Sovra PKI for federated images). Node agent verifies signature + hash before mount. Unsigned images rejected. |
| Raft log | Log manipulation | Raft log entries are chained (each entry references previous). Stored on local SSD with integrity checks. Snapshot checksums verified on restore. |
| API requests | Request modification in transit | TLS for all external connections. mTLS for all internal connections. |

### Repudiation

| Boundary | Attack | Mitigation |
|----------|--------|------------|
| Sensitive actions | User denies accessing sensitive data | Raft-committed audit log with user identity (from OIDC). Cryptographically signed entries (Sovra keys if available, otherwise site PKI). 7-year retention. Tamper-evident chain. |
| Allocation submission | User denies submitting allocation | All API requests logged with authenticated user identity. Audit trail in lattice-api access logs. |
| Node claims | Deny claiming sensitive nodes | Node claim is a Raft-committed operation with user identity. Cannot be repudiated. |

### Information Disclosure

| Boundary | Attack | Mitigation |
|----------|--------|------------|
| Node ↔ storage | Data exfiltration via network sniffing | Encrypted transport: NFS-over-TLS (VAST supports), S3 over HTTPS. Sensitive: encrypted at rest (VAST encrypted pool). |
| Cross-tenant | Side-channel via co-location | Full-node scheduling (ADR-007): no co-location of different tenants by default. Interactive vCluster uses Sarus containers with seccomp for intra-node isolation. |
| Telemetry | Metric leakage between tenants | Label-based access control on TSDB queries. lattice-api injects tenant/user scope filters. |
| Memory | Data remnants after allocation | Node agent zeroes GPU memory and clears scratch storage (NVMe or tmpfs) on allocation release. Sensitive: full node wipe via OpenCHAMI. |
| API responses | Enumeration of other tenants' data | RBAC filtering on all list/query endpoints. Users see only their own allocations; tenant admins see their tenant. |

### Denial of Service

| Boundary | Attack | Mitigation |
|----------|--------|------------|
| User → API | API flooding | Rate limiting per tenant (token bucket). Admission control: reject requests that exceed tenant's request quota. lattice-api provides rate limiting via Tower middleware. |
| Node → quorum | Heartbeat storm | Heartbeat coalescing: node agents batch heartbeats. Quorum-side rate limiting per node (max 1 heartbeat per interval). |
| Scheduling | Malicious allocation specs | Validation at API layer: max resource requests bounded, max array size bounded, DAG cycle detection. Reject before reaching scheduler. |
| Storage | Storage exhaustion | Per-tenant storage quotas enforced by VAST. Checkpoint storage bounded per allocation. |

### Elevation of Privilege

| Boundary | Attack | Mitigation |
|----------|--------|------------|
| User → scheduler | Escalate priority class | RBAC: priority class tied to tenant contract, not user request. Users cannot set priority above their tenant's maximum. |
| Node agent → host | Container/namespace escape | Sarus: seccomp profile, no root in container, read-only rootfs. uenv: mount namespace only (no user namespace needed), processes run as submitting user. No setuid binaries in uenv images (enforced at build time). |
| Tenant admin → system admin | Escalate administrative scope | Distinct RBAC roles with no implicit promotion. System admin requires separate authentication (not derivable from tenant admin token). |
| Workload → network | Break out of network domain | Slingshot VNI enforcement at NIC level (hardware-enforced). Workloads can only communicate within their assigned network domain. |

## Internal Service Authentication

All inter-component communication uses mTLS:

| Component | Certificate Source | Rotation |
|-----------|--------------------|----------|
| Quorum members | Pre-provisioned during deployment | Annual rotation, Raft membership change for re-keying |
| Node agents | OpenCHAMI OPAAL (issued during node boot via cloud-init) | On every node reboot (new cert) |
| API servers | Pre-provisioned or OPAAL | Annual rotation |
| vCluster schedulers | Pre-provisioned or OPAAL | Annual rotation |
| Checkpoint broker | Pre-provisioned or OPAAL | Annual rotation |

Certificate CN format: `{component}.{site}.lattice.internal` (e.g., `node-042.alps.lattice.internal`).

CA trust chain: Site root CA → intermediate CA (OPAAL) → component certificates.

## Secret Management

Sensitive values are never stored in configuration files:

| Secret | Storage | Access Pattern |
|--------|---------|----------------|
| Waldur API token | Secrets manager (HashiCorp Vault or equivalent) | Referenced by path: `vault://lattice/waldur-token` |
| VAST API credentials | Secrets manager | Referenced by path |
| TLS private keys | Local filesystem (mode 0600) or TPM | Loaded at startup |
| OIDC client secret | Secrets manager | Used by hpc-auth (CLI) or lattice-api (server-side validation) |
| Sovra workspace key | Sovra key store (HSM-backed) | Used by federation broker |

Configuration files reference secrets by path, never by value:
```yaml
waldur:
  token_secret_ref: "vault://lattice/waldur-token"
vast:
  credentials_ref: "vault://lattice/vast-creds"
```

## RBAC Model

Three base roles, plus a sensitive-specific role:

| Role | Scope | Permissions |
|------|-------|-------------|
| **user** | Own allocations | Submit, cancel, query own allocations. View own metrics. Attach to own sessions. |
| **tenant-admin** | Tenant's allocations | All user permissions for any allocation in tenant. Manage tenant quotas (within limits). View tenant-level metrics. |
| **system-admin** | All | All operations. Manage vClusters, nodes, tenants. View holistic metrics. |
| **claiming-user** | Claimed sensitive nodes | User role + claim/release sensitive nodes. Access sensitive storage pool. All actions audit-logged. |

Role assignment:
- `user` role derived from OIDC token (any authenticated user)
- `tenant-admin` assigned per-tenant in quorum state, or via `tenant-admin` role claim
- `system-admin` assigned via quorum configuration, or via `admin`/`system:admin` scope
- `claiming-user` assigned per-tenant by tenant-admin (sensitive tenants only)
- `operator` assigned via `operator` scope or role claim

**Cross-system role mapping (pact+lattice co-deployment):**

When pact delegates operations to lattice (e.g., drain, cordon), the pact admin's token carries a `pact_role` claim instead of lattice scopes. Lattice recognizes these cross-system role claims:

| Token claim | Value | Lattice role |
|-------------|-------|--------------|
| `pact_role` | `pact-platform-admin` | SystemAdmin |
| `pact_role` or `lattice_role` | `system-admin` | SystemAdmin |
| `pact_role` or `lattice_role` | `tenant-admin` | TenantAdmin |
| `pact_role` or `lattice_role` | `operator` | Operator |

Standard OIDC scopes take precedence over role claims. Both are checked by `derive_role()`.

## Network Security

| Traffic Class | Network | Isolation |
|---------------|---------|-----------|
| Management (mTLS, heartbeats) | Slingshot management traffic class | Dedicated bandwidth reservation |
| Compute (MPI, NCCL) | Slingshot compute VNIs | Hardware-isolated per network domain |
| Storage (NFS, S3) | Slingshot storage traffic class | QoS-enforced bandwidth |
| Telemetry (metrics) | Slingshot telemetry traffic class | Separate from compute, low priority |
| User access (API, SSH) | Out-of-band Ethernet | Firewalled, rate-limited |

Slingshot traffic classes provide hardware-enforced isolation — compute traffic cannot starve management traffic and vice versa.

## Certificate Rotation

### Quorum Members

1. Generate new certificate from site CA (same CN format)
2. Deploy new cert + key to the target member's TLS directory
3. Perform Raft membership change: remove old member, add "new" member (same node, new cert)
4. Verify: `lattice admin raft status` shows member healthy with new cert serial
5. Repeat for each member (one at a time, maintaining majority)

### Node Agents

Node agents receive certificates from OPAAL during boot. Rotation is automatic on reboot:

1. Drain the node: `lattice node drain <id>`
2. Reboot (or reimage) via OpenCHAMI
3. Node boots with new OPAAL-issued certificate
4. Undrain: `lattice node undrain <id>`

For batch rotation without reboot (if OPAAL supports renewal):
1. Node agent requests new cert from OPAAL
2. Node agent reloads TLS context (graceful, no connection drop)
3. New cert active on next heartbeat

### API Servers and Schedulers

1. Generate new certificate from site CA
2. Deploy new cert + key to the component's TLS directory
3. Restart the component (stateless — no data loss)
4. Load balancer health check confirms the component is back

### Federation (Sovra Certificates)

Sovra workspace keys are managed by the Sovra key rotation protocol. Lattice components use derived tokens, which are automatically refreshed. No Lattice-side action is required for routine Sovra key rotation.

For emergency revocation: revoke the Sovra shared workspace (see [federation.md](federation.md) — Removing a Federation Peer).

## Additional Security Considerations

### OIDC Token Refresh for Long-Lived Streams

Long-lived gRPC streams (Attach, StreamLogs, StreamMetrics) may outlive the OIDC access token's lifetime:

- **Token validation at stream open.** The API server validates the OIDC token when the stream is established.
- **Periodic re-validation.** For streams lasting longer than `token_revalidation_interval` (default: 5 minutes), the API server re-validates the token's claims against the OIDC provider. If the token has been revoked or the user's permissions have changed, the stream is terminated with an `UNAUTHENTICATED` error.
- **Client responsibility.** Clients should refresh their access token before it expires and present the new token on reconnection if the stream is terminated.

### Anti-Replay for API Requests

API requests are protected against replay attacks:

- **TLS as primary defense.** All external API communication uses TLS, which provides replay protection at the transport layer.
- **Request idempotency.** Mutating operations (Submit, Cancel, Update) use client-generated `request_id` fields for idempotency. Duplicate `request_id` values within a time window are rejected.
- **Raft proposal deduplication.** The quorum deduplicates proposals using the proposing scheduler's identity and a monotonic sequence number. Replayed proposals are ignored.

### RBAC for Node Management

Node management operations (drain, undrain, disable) require the `system-admin` role:

| Operation | Required Role | Notes |
|-----------|--------------|-------|
| `ListNodes`, `GetNode` | `user` | Read-only, filtered by tenant scope |
| `DrainNode`, `UndrainNode` | `system-admin` | Affects scheduling across all tenants |
| `DisableNode` | `system-admin` | Removes node from scheduling entirely |
| Sensitive node claim | `claiming-user` | Sensitive-specific role within tenant |

### Certificate CN vs NodeId Mapping

Node agent certificates use a deterministic CN format that maps to the node's xname identity:

- **Format:** `{xname}.{site}.lattice.internal` (e.g., `x1000c0s0b0n0.alps.lattice.internal`)
- **Validation:** On each heartbeat, the quorum verifies that the certificate CN matches the node ID reported in the heartbeat payload. A mismatch triggers an `UNAUTHENTICATED` error and an alert.
- **Prevents:** A compromised node agent from impersonating a different node.

### Sensitive Session Recording Storage

Attach session recordings for sensitive allocations are stored alongside the audit log:

- **Path:** `s3://sensitive-audit/{tenant}/{alloc_id}/sessions/{session_id}.recording`
- **Format:** Raw byte stream (input + output interleaved with timestamps), compressed with zstd
- **Encryption:** Encrypted at rest using the sensitive storage pool's encryption keys
- **Retention:** 7 years (matching sensitive audit log retention)
- **Access:** Only the claiming user and tenant-admin (compliance reviewer) can access recordings via the audit query API

### Audit Signing Key Persistence

The Ed25519 signing key for audit log entries is loaded from a persistent file configured via `QuorumConfig.audit_signing_key_path`. This ensures:

- **Chain continuity**: Archived audit entries (in S3) can be verified after quorum restart
- **Non-repudiation**: The same key signs all entries, forming a verifiable chain
- **Key rotation**: Replace the file and restart the quorum to rotate (old entries remain verifiable with the old public key)
- **Dev mode**: When `audit_signing_key_path` is not set, a random key is generated (suitable for testing only)

### REST API Authentication

REST and gRPC endpoints require authentication when OIDC or HMAC is configured:

- Bearer token required in `Authorization` header (validated on every request)
- Two validation modes: **JWKS** (production, via `oidc_issuer`) or **HMAC-SHA256** (dev/testing, via `LATTICE_OIDC_HMAC_SECRET`)
- REST middleware validates asynchronously (supports JWKS network fetch on cache miss)
- gRPC interceptor validates synchronously using cached JWKS keys (pre-fetched at startup) or HMAC
- Rate limiting applied per-user
- Public endpoints exempt: `/healthz`, `/api/v1/auth/discovery`
- OIDC discovery client disables HTTP redirects (JWKS cache poisoning prevention)
- Non-HTTPS issuer URLs produce a warning (MITM risk)
- Server logs a prominent warning on startup if no authentication is configured

### Service Discovery Isolation

Service discovery endpoints (`LookupService`, `ListServices`) are tenant-filtered:

- `x-lattice-tenant` header constrains results to the requesting tenant's services
- Without the header, all services are visible (admin/operator access)
- Prevents cross-tenant information disclosure of service topology

### Session Security

Interactive sessions are tracked globally in Raft state:

- `CreateSession` / `DeleteSession` are Raft-committed operations
- Sensitive allocations: at most one concurrent session globally (INV-C2)
- Sessions survive API server restart (persisted in quorum state)
- Ownership verified: only the allocation's user can create sessions

## Cross-References

- [sensitive-workloads.md](sensitive-workloads.md) — Sensitive-specific security requirements
- [failure-modes.md](failure-modes.md) — Security implications of failure scenarios
- [upgrades.md](upgrades.md) — Certificate rotation during upgrades
- [accounting.md](accounting.md) — Waldur API token management
