# Federation Architecture

## Design Principle

Federation is **opt-in** and **sovereignty-first**. The system is fully functional without it. When enabled, each site retains full control over its resources. The federation broker *suggests*, the local scheduler *decides*.

## Feature Gate

Federation is compile-time optional via Rust feature flag:

```toml
# Cargo.toml
[features]
default = []
federation = ["sovra-client", "federation-broker"]
```

When `federation` feature is disabled:
- No Sovra dependency
- No federation broker binary
- No cross-site API endpoints
- System operates as a standalone site

## Trust Model: Sovra Integration

[Sovra](https://github.com/witlox/sovra) provides federated sovereign key management. Each site runs its own Sovra instance with its own root key.

```
Site A Sovra Instance              Site B Sovra Instance
├── Site A Root Key (sovereign)    ├── Site B Root Key (sovereign)
├── Workspace: "hpc-general"       ├── Workspace: "hpc-general"
│   (shared federation key)        │   (federated with Site A)
├── Workspace: "medical-ch"        └── Policy: Site B OPA rules
│   (hospital CRK, delegated)
└── Policy: Site A OPA rules

Sovra Federation Protocol (peer-to-peer, no central authority)
```

### Key Management Principles

1. **Site root keys never leave the site.** All cross-site authentication uses derived keys from shared workspaces.
2. **Federation is revocable.** Revoking a shared workspace invalidates all cross-site tokens. Instant defederation.
3. **Medical keys are tenant-controlled.** The hospital (data owner) holds the Customer Root Key. The operating site holds a delegated key. If the relationship ends, the hospital retains access.
4. **Audit logs are cryptographically signed.** Each site signs its audit entries with its own key. Cross-site audit trails are verifiable by any party in the trust chain.

## Federation Components

### Federation Broker

A Go service that runs alongside the scheduler (when federation feature is enabled).

**Responsibilities:**
- Advertises site capabilities to federated peers (available capacity, GPU types, energy prices, data locality)
- Receives federated allocation requests from peer sites
- Signs outbound requests with Sovra tokens
- Verifies inbound requests against Sovra trust chain + OPA policy
- Routes accepted requests into the local scheduling plane

**Communication:** gRPC over mTLS, with Sovra-signed metadata in request headers.

### Federation Catalog

A read-mostly, eventually consistent shared catalog across federated sites:

| Content | Update Frequency | Consistency |
|---|---|---|
| Site capabilities (GPU types, node counts) | Hourly | Eventual |
| uenv image registry (cross-site name resolution) | On publish | Eventual |
| Dataset catalog (where data physically resides) | On change | Eventual |
| Tenant identity mapping (OIDC trust) | On federation setup | Strong (Sovra) |
| Energy prices per site | Every 15 minutes | Eventual |

### Job Routing Logic

The federation broker's routing decision is advisory, not mandatory:

```
Input: Allocation request from remote site (or local user targeting remote)
Output: Recommendation (run locally, run at site X, reject)

Factors:
1. Data gravity: where does the input data physically reside?
   → Strong bias toward running where data is
2. Compute availability: does the target site have capacity?
   → Check advertised capacity (may be stale)
3. Energy cost: which site has cheaper power right now?
   → Time-varying electricity prices from catalog
4. Tenant authorization: is this user allowed at the target site?
   → OPA policy check via Sovra-delegated credentials
5. Data sovereignty: can the data legally transit to the target site?
   → Medical data: check jurisdiction constraints

Decision: route to site with best composite score, or reject if no site qualifies
```

## Federated Allocation Flow

```
1. User at Site A submits: lattice submit --site=B train.sh
2. Site A lattice-api receives request, passes to federation broker
3. Federation broker:
   a. Signs request with Sovra token (Site A workspace key)
   b. Resolves target: Site B (explicit) or best-fit (if --site=auto)
   c. Forwards to Site B's federation broker
4. Site B federation broker:
   a. Verifies Sovra token (Site A is trusted peer)
   b. Checks OPA policy (user authorized, resources available)
   c. Injects allocation into Site B's scheduling plane
5. Site B local quorum manages allocation entirely
6. Status/logs available to user at Site A via federation catalog query
7. On completion: Site B reports results, Site A's user notified
```

## Cross-Site Data Access

When a federated job runs at a remote site but needs data from the home site:

- **Small data (<1 GB):** Fetched on demand via S3 over WAN
- **Medium data (1 GB - 1 TB):** Pre-staged during queue wait via VAST DataSpace sync
- **Large data (>1 TB):** Strong recommendation to run job at data's home site
- **Medical data:** Never transferred. Job must run at data's home site. No exceptions.

## Operational Considerations

### Adding a Federation Peer

1. Exchange Sovra workspace keys (out-of-band, verified by site admins)
2. Configure federation broker with peer endpoint + workspace ID
3. Define OPA policies for cross-site access
4. Test with non-production allocations
5. Enable in production

### Removing a Federation Peer

1. Revoke Sovra shared workspace
2. All in-flight federated allocations continue to completion (or are cancelled by policy)
3. Remove peer from federation broker config
4. Immediate: no new federated requests accepted
