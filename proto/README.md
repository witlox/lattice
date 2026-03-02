# Protobuf Definitions

This directory contains the protobuf definitions that serve as the **API contract** for Lattice. All inter-component communication uses these definitions.

## Files

- `lattice/v1/allocations.proto` — Allocation CRUD, DAG submission, task groups
- `lattice/v1/nodes.proto` — Node state queries
- `lattice/v1/sessions.proto` — Interactive session management
- `lattice/v1/tenants.proto` — Tenant and vCluster management
- `lattice/v1/accounting.proto` — Usage history queries
- `lattice/v1/telemetry.proto` — Telemetry mode switching
- `lattice/v1/internal.proto` — Quorum ↔ scheduler ↔ node agent (not user-facing)

## Generation

```bash
# Install buf: https://buf.build/docs/installation
buf generate

# Outputs:
# - Rust (tonic/prost): crates/lattice-common/src/proto/
# - Python (grpcio): sdk/python/lattice_sdk/proto/
```

## Design Rules

1. The Intent API is the primary interface. The Compatibility API is a translation layer.
2. All user-facing RPCs require OIDC bearer token in metadata.
3. Internal RPCs (quorum ↔ components) use mTLS, not OIDC.
4. Streaming RPCs for: job status watching, telemetry streams, terminal sessions.
5. Federation RPCs are gated behind the `federation` feature flag.
