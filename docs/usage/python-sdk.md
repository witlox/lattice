# Python SDK

The Lattice Python SDK provides an async client for interacting with the REST API from notebooks, scripts, and autonomous agents.

## Installation

```bash
pip install lattice-sdk
```

## Quick Start

```python
import asyncio
from lattice_sdk import LatticeClient, AllocationSpec

async def main():
    async with LatticeClient("lattice-api.example.com", 8080) as client:
        # Submit an allocation
        alloc = await client.submit(AllocationSpec(
            entrypoint="python train.py",
            nodes=4,
            walltime="24h",
            tenant="ml-team",
        ))
        print(f"Submitted: {alloc.id}")

        # Check status
        status = await client.status(alloc.id)
        print(f"State: {status.state}")

        # Wait for completion
        async for event in client.watch(alloc.id):
            print(f"State changed: {event.state}")
            if event.state in ("Completed", "Failed", "Cancelled"):
                break

asyncio.run(main())
```

## Core Methods

### Submission

```python
# Basic submission
alloc = await client.submit(AllocationSpec(
    entrypoint="torchrun train.py",
    nodes=64,
    walltime="72h",
    uenv="prgenv-gnu/24.11:v1",
    constraints={"gpu_type": "GH200"},
))

# Submit DAG
dag = await client.submit_dag("workflow.yaml")
```

### Status & Listing

```python
# Get allocation
alloc = await client.status(alloc_id)

# List allocations
allocs = await client.list_allocations(state="running")

# List nodes
nodes = await client.list_nodes(state="ready")
```

### Monitoring

```python
# Stream logs
async for line in client.stream_logs(alloc_id):
    print(line.message)

# Query metrics
metrics = await client.query_metrics(alloc_id)
print(f"GPU util: {metrics.gpu_utilization}%")

# Stream metrics
async for snapshot in client.stream_metrics(alloc_id):
    print(f"GPU: {snapshot.gpu_utilization}%")

# Watch state changes
async for event in client.watch(alloc_id):
    print(f"State: {event.state}")
```

### Management

```python
# Cancel
await client.cancel(alloc_id)

# Checkpoint
await client.checkpoint(alloc_id)
```

### Tenants & vClusters

```python
tenants = await client.list_tenants()
vclusters = await client.list_vclusters()
```

## Error Handling

```python
from lattice_sdk import LatticeError, LatticeNotFoundError, LatticeAuthError

try:
    alloc = await client.status("nonexistent-id")
except LatticeNotFoundError:
    print("Allocation not found")
except LatticeAuthError:
    print("Authentication failed")
except LatticeError as e:
    print(f"API error ({e.status_code}): {e}")
```

## Authentication

```python
# Token-based (OIDC)
client = LatticeClient("api.example.com", 8080, token="eyJ...")

# Headers
client = LatticeClient("api.example.com", 8080, headers={"X-Tenant": "my-team"})
```
