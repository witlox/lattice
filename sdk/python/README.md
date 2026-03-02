# Lattice Python SDK

A Python SDK for interacting with [Lattice](https://github.com/witlox/lattice), a distributed workload scheduler that sits between Slurm (HPC batch) and Kubernetes (cloud services).

## Quick Start

Install the SDK:

```bash
pip install -e .
```

Or with development dependencies:

```bash
pip install -e ".[dev]"
```

## Usage

```python
import asyncio
from lattice_sdk import LatticeClient

async def main():
    # Connect to Lattice API server
    async with LatticeClient(
        host="lattice-api.example.com",
        port=50051,
        token="your-auth-token"  # optional
    ) as client:
        # Submit an allocation (job or service)
        alloc_id = await client.submit(
            entrypoint="python train.py",
            nodes=2,
            cpus=16,
            memory_gb=64,
            gpus=2,
            priority_class="high",
            tenant_id="team-ai",
            user_id="alice",
            labels={"app": "training", "experiment": "exp-001"}
        )
        print(f"Submitted allocation: {alloc_id}")

        # Watch allocation lifecycle events
        async for event in client.watch(alloc_id):
            print(f"Event: {event.event_type}")
            print(f"  State: {event.allocation.state}")
            if event.allocation.state == AllocationState.COMPLETED:
                break

        # Stream logs
        async for log in client.logs(alloc_id, follow=True):
            print(f"[{log.level}] {log.message}")

        # Query current metrics
        metrics = await client.metrics(alloc_id)
        print(f"GPU utilization: {metrics.gpu_utilization}%")

        # Stream metrics in real-time
        async for metric in client.stream_metrics(alloc_id):
            print(f"CPU: {metric.cpu_utilization}%, GPU: {metric.gpu_utilization}%")

if __name__ == "__main__":
    asyncio.run(main())
```

## API Reference

### LatticeClient

Main client class for interacting with Lattice.

#### Constructor

```python
LatticeClient(
    host: str = "localhost",
    port: int = 50051,
    token: Optional[str] = None
)
```

- `host`: Lattice API server hostname or IP (default: `localhost`)
- `port`: gRPC server port (default: `50051`)
- `token`: Optional authentication token for secure channels

#### Methods

All methods are async and require use of `await` or async context manager.

##### submit(...)

Submit an allocation (job or service) to Lattice.

```python
alloc_id = await client.submit(
    entrypoint="python train.py",
    nodes=1,                          # number of nodes
    cpus=8.0,                         # CPUs per node
    memory_gb=32.0,                   # Memory per node (GB)
    gpus=2,                           # GPUs per node
    gpu_memory_gb=40.0,               # GPU memory per node (GB)
    storage_gb=100.0,                 # Storage requirement (GB)
    priority_class="normal",          # "low", "normal", "high"
    tenant_id=None,                   # Tenant for quota enforcement
    user_id=None,                     # User ID for audit logging
    network_domain=None,              # VNI for L3 reachability
    uenv=None,                        # uenv image for software delivery
    labels=None,                      # Metadata labels
    annotations=None                  # User annotations
)
```

Returns the allocation ID as a string.

##### status(alloc_id)

Get the status and details of an allocation.

```python
allocation = await client.status(alloc_id)
print(f"State: {allocation.state}")
print(f"Nodes: {allocation.nodes_allocated}")
```

Returns an `Allocation` object.

##### cancel(alloc_id)

Cancel a running or pending allocation.

```python
success = await client.cancel(alloc_id)
```

Returns `True` if cancellation was successful.

##### list_nodes()

List all compute nodes in the cluster.

```python
nodes = await client.list_nodes()
for node in nodes:
    print(f"{node.name}: {node.available_cpus}/{node.total_cpus} CPUs available")
```

Returns a list of `Node` objects.

##### watch(alloc_id)

Stream allocation lifecycle events.

```python
async for event in client.watch(alloc_id):
    print(f"Event: {event.event_type}")
    print(f"State: {event.allocation.state}")
```

Yields `WatchEvent` objects. Events include: `created`, `updated`, `completed`, `failed`, `cancelled`.

##### logs(alloc_id, follow=False, lines=100)

Stream logs from an allocation.

```python
async for log in client.logs(alloc_id, follow=True):
    print(f"[{log.timestamp}] [{log.level}] {log.message}")
```

- `follow`: Follow new logs as they arrive (like `tail -f`). Default: `False`
- `lines`: Number of previous lines to retrieve. Default: `100`

Yields `LogEntry` objects.

##### metrics(alloc_id)

Get current metrics for an allocation.

```python
metrics = await client.metrics(alloc_id)
print(f"CPU: {metrics.cpu_utilization}%")
print(f"GPU: {metrics.gpu_utilization}%")
print(f"Memory: {metrics.memory_utilization_gb} GB")
```

Returns an `AllocationMetrics` object.

##### stream_metrics(alloc_id)

Stream real-time metrics from an allocation.

```python
async for metrics in client.stream_metrics(alloc_id):
    print(f"[{metrics.timestamp}] CPU: {metrics.cpu_utilization}%")
```

Yields `AllocationMetrics` objects at regular intervals.

## Types

### Allocation

Represents a job or service submitted to Lattice.

```python
@dataclass
class Allocation:
    id: str                                    # Allocation ID
    entrypoint: str                            # Command to execute
    state: AllocationState                     # Current state
    requested_resources: ResourceRequest       # Resource requirements
    tenant_id: str                             # Tenant identifier
    user_id: str                               # User identifier
    priority_class: str                        # "low", "normal", "high"
    network_domain: Optional[str]              # VNI for L3 reachability
    uenv: Optional[str]                        # Software image
    nodes_allocated: List[str]                 # Allocated node names
    created_at: Optional[datetime]             # Creation timestamp
    started_at: Optional[datetime]             # Start timestamp
    completed_at: Optional[datetime]           # Completion timestamp
    labels: Dict[str, str]                     # Metadata labels
    annotations: Dict[str, str]                # User annotations
```

### AllocationState

Enum for allocation lifecycle states:

- `PENDING` — Waiting for resources
- `RUNNING` — Executing on nodes
- `COMPLETED` — Successfully finished
- `FAILED` — Execution failed
- `CANCELLED` — User cancelled
- `CHECKPOINTED` — Checkpointed for later resumption

### ResourceRequest

Represents resource requirements.

```python
@dataclass
class ResourceRequest:
    cpus: float                    # Number of CPUs
    memory_gb: float               # Memory in GB
    gpus: int                      # Number of GPUs
    gpu_memory_gb: float           # GPU memory in GB
    storage_gb: float              # Storage in GB
    metadata: Dict[str, str]       # Additional metadata
```

### Node

Represents a compute node.

```python
@dataclass
class Node:
    name: str                      # Node identifier
    host: str                      # Hostname or IP
    status: str                    # "available", "allocated", "maintenance"
    total_cpus: float              # Total CPUs on node
    total_memory_gb: float         # Total memory on node
    total_gpus: int                # Total GPUs on node
    available_cpus: float          # Available CPUs
    available_memory_gb: float     # Available memory
    available_gpus: int            # Available GPUs
    last_heartbeat: Optional[datetime]  # Last health check
    metadata: Dict[str, str]       # Node metadata
```

### WatchEvent

Represents an event in the watch stream.

```python
@dataclass
class WatchEvent:
    event_type: str                # "created", "updated", "completed", etc.
    allocation: Allocation         # Updated allocation object
    timestamp: datetime            # Event timestamp
```

### LogEntry

Represents a single log message.

```python
@dataclass
class LogEntry:
    timestamp: datetime            # Log timestamp
    level: str                     # "DEBUG", "INFO", "WARN", "ERROR"
    message: str                   # Log message
    source: Optional[str]          # Node name or component
```

### AllocationMetrics

Represents resource utilization metrics.

```python
@dataclass
class AllocationMetrics:
    allocation_id: str             # Allocation ID
    timestamp: datetime            # Metric timestamp
    cpu_utilization: float         # CPU usage percentage
    memory_utilization_gb: float   # Memory usage in GB
    gpu_utilization: float         # GPU usage percentage
    gpu_memory_utilization_gb: float  # GPU memory usage in GB
    network_in_mbps: float         # Network inbound throughput
    network_out_mbps: float        # Network outbound throughput
    io_read_mbps: float            # Disk read throughput
    io_write_mbps: float           # Disk write throughput
    metadata: Dict[str, str]       # Additional metadata
```

## Testing

Run the test suite:

```bash
pytest tests/
```

Or with verbose output:

```bash
pytest -v tests/
```

## Development

This SDK is a scaffold. Full functionality requires:

1. **Generate protobuf stubs**: Run `buf generate` in the root Lattice repository to generate Python bindings from `proto/lattice/v1/*.proto`

2. **Implement gRPC client methods**: Replace stub implementations in `client.py` with actual gRPC calls

3. **Add integration tests**: Use the generated stubs to test against a running Lattice server

See the main [Lattice documentation](https://github.com/witlox/lattice/tree/main/docs) for architecture and design details.

## License

Apache License 2.0 - See LICENSE file in the Lattice repository.
