# Network Domains

## Design Principle

Network domains provide L3 reachability between allocations that need to communicate. They map to Slingshot VNIs (Virtual Network Identifiers) which provide hardware-enforced network isolation. Domains are created on demand, scoped to tenants, and cleaned up automatically.

## What is a Network Domain

A network domain is a named group of allocations that share network reachability:

```yaml
# Two allocations sharing a domain:
allocation_a:
  connectivity:
    network_domain: "ml-workspace"

allocation_b:
  connectivity:
    network_domain: "ml-workspace"
```

Allocations in the same domain can communicate over the Slingshot fabric. Allocations in different domains (or with no domain) are network-isolated at the hardware level.

## VNI Lifecycle

### Allocation

```
1. User submits allocation with network_domain: "ml-workspace"
2. lattice-api checks if domain "ml-workspace" exists for this tenant:
   a. If exists: allocation joins the existing domain
   b. If not: create new domain, allocate VNI from pool
3. VNI assignment is stored in quorum state (eventually consistent)
4. Node agents configure Slingshot NIC with the VNI for the allocation's traffic
```

### VNI Pool

VNIs are allocated from a configured pool:

```yaml
network:
  vni_pool_start: 1000
  vni_pool_end: 4095
  # Reserved VNIs:
  # 1 = management
  # 2 = telemetry
  # 3-999 = reserved for future use
```

VNIs are allocated sequentially from the pool. When freed, they return to the available set.

### Release

```
1. Last allocation in the domain completes (or is cancelled)
2. Domain enters "draining" state for grace_period (default: 5 minutes)
   - Allows brief gaps between allocations in a long-running workflow
3. After grace period with no new allocations: domain is released
4. VNI returns to the available pool
5. Domain name can be reused by the same tenant
```

The grace period prevents VNI churn in DAG workflows where allocations start and stop in sequence but share a domain.

### DAG Domain Persistence

DAG workflows often have sequential stages that share a network domain but have gaps between stages (one allocation completes before the next starts). The grace period (default: 5 minutes) covers these gaps:

- If the next DAG stage starts within the grace period: it joins the existing domain (same VNI, no churn)
- If the gap exceeds the grace period: the domain is released and a new VNI is allocated when the next stage starts
- For long-running DAGs with predictable inter-stage gaps, increase the grace period per-domain: `lattice admin network set-grace --domain=<name> --grace=15m`
- The grace period timer resets each time a new allocation joins the domain

## Scoping Rules

| Rule | Enforcement |
|------|-------------|
| Domain names are scoped to a tenant | Two tenants can use the same domain name without conflict |
| Only allocations from the same tenant can share a domain | Cross-tenant domains are not allowed (isolation requirement) |
| Sensitive domains are per-allocation | Each sensitive allocation gets a unique domain (no sharing, even within tenant) |
| Domain names are user-chosen strings | No system-generated names; users pick meaningful names |

## Capacity

| Parameter | Default | Notes |
|-----------|---------|-------|
| VNI pool size | 3095 (1000-4095) | Sufficient for typical HPC deployments |
| Max domains per tenant | 50 | Configurable per tenant |
| Max allocations per domain | Unlimited | Practical limit: node count |

### VNI Exhaustion

If the VNI pool is exhausted:

1. New domain creation fails with a clear error:
   ```
   Error: cannot create network domain — VNI pool exhausted (3095/3095 in use)
   Hint: Wait for running allocations to complete, or contact your system admin.
   ```
2. Allocations without `network_domain` are unaffected (they don't need a VNI)
3. Allocations joining an existing domain are unaffected (domain already has a VNI)
4. Alert raised for operators

### VNI Exhaustion Mid-DAG

If the VNI pool is exhausted while a DAG has pending allocations that require a new network domain:

- The allocation that needs the new domain enters `Pending` state with reason `vni_pool_exhausted`.
- The DAG stalls at this allocation — downstream dependencies remain blocked.
- Already-running DAG allocations with existing domains are unaffected.
- **Mitigation:** Use a shared network domain across DAG stages where possible. This avoids new VNI allocation for each stage and reduces pool pressure.
- **Recovery:** When other allocations complete and release VNIs, the pending allocation is re-evaluated on the next scheduling cycle.

## Default Behavior

If an allocation does not specify `network_domain`:

- Single-node allocations: no VNI needed, no network isolation beyond the default
- Multi-node allocations: automatically assigned a domain named `alloc-{id}` (private to this allocation)
- Services with `expose` ports: automatically assigned a domain if not specified

## Service Exposure

For allocations exposing service endpoints:

```yaml
connectivity:
  network_domain: "inference-cluster"
  expose:
    - name: "api"
      port: 8080
      protocol: "http"
```

Exposed ports are reachable from:
1. Other allocations in the same network domain (always)
2. The FirecREST API gateway (for external access, if configured)
3. Not directly reachable from outside the fabric (Slingshot is not routable from Ethernet)

## Sensitive Network Domains

Sensitive allocations get strict network isolation:

```yaml
connectivity:
  network_domain: "sensitive-{user}-{alloc_id}"  # auto-generated, unique
  policy:
    ingress: deny-all-except:
      - same_domain          # only processes in this allocation
      - data_gateway         # controlled data ingress
    egress: deny-all-except:
      - data_gateway         # controlled data egress
```

- Each sensitive allocation gets its own domain (no sharing)
- Ingress/egress restricted to a data gateway endpoint
- With Ultra Ethernet: network-level encryption enabled for the VNI
- VNI released immediately on allocation completion (no grace period)

## VNI Pool Expansion

To expand the VNI pool when approaching exhaustion:

1. **Update the configuration** to extend `vni_pool_end`:
   ```yaml
   network:
     vni_pool_start: 1000
     vni_pool_end: 8191   # expanded from 4095
   ```

2. **Restart the API server** to pick up the new pool range. Existing domains and their VNI assignments are not affected.

3. **Verify:** The `lattice_network_vni_pool_total` metric should reflect the new pool size.

**Note:** The expanded range must not overlap with reserved VNIs (1-999) or VNIs used by other systems on the Slingshot fabric. Coordinate with network administrators before expanding.

## Cross-References

- [system-architecture.md](system-architecture.md) — Network fabric layer, VNI-based isolation
- [sensitive-workloads.md](sensitive-workloads.md) — Sensitive network isolation policy
- [security.md](security.md) — Network security, traffic classes
- [api-design.md](api-design.md) — Connectivity field in allocation request
