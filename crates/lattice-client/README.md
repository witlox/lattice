# lattice-client

[![CI](https://github.com/witlox/lattice/actions/workflows/ci.yml/badge.svg)](https://github.com/witlox/lattice/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)

Rust gRPC client SDK for the [Lattice](https://github.com/witlox/lattice) distributed workload scheduler. Type-safe access to all Lattice API operations — allocations, DAGs, sessions, nodes, tenants, vClusters, audit, accounting, and cluster administration.

This crate is the Rust counterpart to the Python SDK (`sdk/python/`). It is the recommended way for Rust applications (like [Pact](https://github.com/witlox/pact)) to interact with a Lattice cluster.

## Features

- **Full API Parity**: 44 methods covering all 11 API domains, matching the Python SDK surface
- **Type-Safe**: Generated tonic/prost types — compile-time guarantees on request/response shapes
- **Pure gRPC**: No REST fallback needed; all operations have proto definitions
- **Streaming**: Server-streaming (watch, logs, metrics) and bidirectional (attach) support
- **Typed Errors**: `LatticeClientError` maps gRPC status codes to semantic variants (NotFound, Auth, PermissionDenied, InvalidArgument)
- **Bearer Auth**: Automatic token injection via interceptor on every request

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
lattice-client = { git = "https://github.com/witlox/lattice" }
```

## Usage

### 1. Connect to a Lattice Cluster

```rust
use lattice_client::{LatticeClient, ClientConfig};

let config = ClientConfig {
    endpoint: "http://lattice-api:50051".to_string(),
    token: Some("my-oidc-token".to_string()),
    ..Default::default()
};

let mut client = LatticeClient::connect(config).await?;
```

### 2. Node Operations

```rust
use lattice_client::proto;

// List all nodes
let nodes = client.list_nodes(proto::ListNodesRequest::default()).await?;
for node in &nodes.nodes {
    println!("{}: {}", node.node_id, node.state);
}

// Drain → maintenance → enable
client.drain_node("x1000c0s0b0n0", "scheduled maintenance").await?;
// ... perform maintenance ...
client.undrain_node("x1000c0s0b0n0").await?;

// Disable and re-enable
client.disable_node("x1000c0s0b0n0", "hardware replacement").await?;
client.enable_node("x1000c0s0b0n0").await?;
```

### 3. Submit and Monitor Workloads

```rust
use lattice_client::proto;

// Submit an allocation
let resp = client.submit(proto::SubmitRequest {
    submission: Some(proto::submit_request::Submission::Single(proto::AllocationSpec {
        tenant: "physics".to_string(),
        entrypoint: "python train.py".to_string(),
        resources: Some(proto::ResourceSpec {
            min_nodes: 4,
            gpu_type: "GH200".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    })),
}).await?;

let alloc_id = &resp.allocation_ids[0];

// Watch for state changes
let mut stream = client.watch(proto::WatchRequest {
    allocation_id: alloc_id.clone(),
    ..Default::default()
}).await?;

use tokio_stream::StreamExt;
while let Some(event) = stream.next().await {
    let event = event?;
    println!("{}: {}", event.event_type, event.message);
}
```

### 4. Cluster Administration

```rust
// Create a tenant
client.create_tenant(proto::CreateTenantRequest {
    name: "physics".to_string(),
    quota: Some(proto::TenantQuotaSpec {
        max_nodes: 200,
        fair_share_target: 0.3,
        ..Default::default()
    }),
    isolation_level: "standard".to_string(),
}).await?;

// Check budget usage
let usage = client.tenant_usage("physics", 90).await?;
println!("GPU-hours: {:.1}/{}", usage.gpu_hours_used,
    usage.gpu_hours_budget.map(|b| format!("{:.0}", b)).unwrap_or("unlimited".into()));

// Check cluster health
let status = client.raft_status().await?;
println!("Leader: {}, Term: {}", status.leader_id, status.current_term);

let health = client.health().await?;
println!("Status: {}", health.status);
```

### 5. Error Handling

```rust
use lattice_client::LatticeClientError;

match client.get_node("nonexistent").await {
    Ok(node) => println!("Found: {}", node.node_id),
    Err(LatticeClientError::NotFound(msg)) => eprintln!("Node not found: {msg}"),
    Err(LatticeClientError::Auth(msg)) => eprintln!("Auth failed: {msg}"),
    Err(LatticeClientError::PermissionDenied(msg)) => eprintln!("Forbidden: {msg}"),
    Err(e) => eprintln!("Error: {e}"),
}
```

## Architecture

### What's Provided

| Component | Description |
|-----------|-------------|
| `LatticeClient` | Main entry point — 44 async methods across all API domains |
| `ClientConfig` | Endpoint, timeout, and bearer token configuration |
| `LatticeClientError` | Typed error enum mapped from gRPC status codes |
| `AuthInterceptor` | Bearer token injection on every outgoing request |
| `proto` module | Re-exported protobuf types (request/response messages) |

### What You Provide

| Component | Description |
|-----------|-------------|
| `endpoint` | Lattice API server address (e.g., `http://lattice-api:50051`) |
| `token` | OIDC bearer token (obtain via `hpc-auth` or your IdP) |
| `timeout_secs` | Request timeout (default: 30s) |

### Method Coverage

| Domain | Methods | Protocol |
|--------|---------|----------|
| Allocations | submit, get, list, cancel, update, launch_tasks, checkpoint | gRPC |
| Observability | watch, stream_logs, query_metrics, stream_metrics, get_diagnostics, compare_metrics, attach | gRPC (streaming) |
| DAGs | get_dag, list_dags, cancel_dag | gRPC |
| Sessions | create_session, get_session, delete_session | gRPC |
| Nodes | list_nodes, get_node, drain_node, undrain_node, disable_node, enable_node, health | gRPC |
| Tenants | create_tenant, update_tenant, list_tenants, get_tenant | gRPC |
| vClusters | create_vcluster, update_vcluster, list_vclusters, get_vcluster, vcluster_queue | gRPC |
| Audit | query_audit | gRPC |
| Accounting | accounting_usage | gRPC |
| Budget Usage | tenant_usage, user_usage | gRPC |
| Raft/Admin | raft_status, create_backup, verify_backup, restore_backup | gRPC |
