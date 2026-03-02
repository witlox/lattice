"""
Lattice gRPC client wrapper with Pythonic interface.

The LatticeClient class provides high-level methods for interacting with a Lattice
scheduler. This is a stub implementation; full functionality requires generated
protobuf stubs from proto/lattice/v1/*.proto.
"""

from typing import AsyncIterator, Iterator, Optional
import grpc

from .types import Allocation, AllocationState, Node, AllocationMetrics, WatchEvent, LogEntry


class LatticeClient:
    """
    Python client for Lattice distributed workload scheduler.

    Connects to a lattice-api gRPC server and provides methods for job submission,
    status queries, cancellation, and observability (logs, metrics, watch streams).

    Example:
        async with LatticeClient("lattice-api.example.com", 50051) as client:
            alloc_id = await client.submit(
                entrypoint="python train.py",
                nodes=1,
                cpus=8,
                memory_gb=32,
                gpus=1
            )
            status = await client.status(alloc_id)
            print(f"Job {alloc_id}: {status.state}")
    """

    def __init__(
        self,
        host: str = "localhost",
        port: int = 50051,
        token: Optional[str] = None,
    ):
        """
        Initialize a Lattice client.

        Args:
            host: gRPC server hostname or IP address. Defaults to "localhost".
            port: gRPC server port. Defaults to 50051.
            token: Optional authentication token for secure channels (not yet implemented).
        """
        self.host = host
        self.port = port
        self.token = token
        self.channel: Optional[grpc.aio.Channel] = None
        self.connected = False

    async def __aenter__(self):
        """Async context manager entry."""
        await self.connect()
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        """Async context manager exit."""
        await self.disconnect()

    async def connect(self):
        """
        Establish connection to Lattice gRPC server.

        Raises:
            NotImplementedError: gRPC channel setup requires protobuf stubs.
        """
        raise NotImplementedError(
            "gRPC channel initialization requires generated protobuf stubs from "
            "proto/lattice/v1/*.proto. Run 'buf generate' to generate Python bindings."
        )

    async def disconnect(self):
        """Close the gRPC channel."""
        if self.channel:
            await self.channel.close()
            self.connected = False

    async def submit(
        self,
        entrypoint: str,
        nodes: int = 1,
        cpus: float = 1.0,
        memory_gb: float = 4.0,
        gpus: int = 0,
        gpu_memory_gb: float = 0.0,
        storage_gb: float = 0.0,
        priority_class: str = "normal",
        tenant_id: Optional[str] = None,
        user_id: Optional[str] = None,
        network_domain: Optional[str] = None,
        uenv: Optional[str] = None,
        labels: Optional[dict] = None,
        annotations: Optional[dict] = None,
    ) -> str:
        """
        Submit an allocation (job or service) to Lattice.

        Args:
            entrypoint: Command to execute (e.g., "python train.py").
            nodes: Number of nodes required. Defaults to 1.
            cpus: CPUs per node. Defaults to 1.0.
            memory_gb: Memory per node in GB. Defaults to 4.0.
            gpus: Number of GPUs per node. Defaults to 0.
            gpu_memory_gb: GPU memory per node in GB. Defaults to 0.0.
            storage_gb: Storage requirement in GB. Defaults to 0.0.
            priority_class: Priority class ("low", "normal", "high"). Defaults to "normal".
            tenant_id: Tenant identifier for quota enforcement. Optional.
            user_id: User identifier for audit logging. Optional.
            network_domain: Network domain for L3 reachability. Optional.
            uenv: uenv image for software delivery. Optional.
            labels: Metadata labels (key-value pairs). Optional.
            annotations: Annotations for user metadata. Optional.

        Returns:
            Allocation ID as a string.

        Raises:
            NotImplementedError: Requires generated protobuf stubs.
        """
        raise NotImplementedError(
            "submit() requires generated protobuf stubs. "
            "Run 'buf generate' in the root of the Lattice repository to generate "
            "proto/gen/python/ bindings and import them here."
        )

    async def status(self, alloc_id: str) -> Allocation:
        """
        Get the status and details of an allocation.

        Args:
            alloc_id: Allocation ID returned from submit().

        Returns:
            Allocation object with current state, resource usage, and metadata.

        Raises:
            NotImplementedError: Requires generated protobuf stubs.
        """
        raise NotImplementedError(
            "status() requires generated protobuf stubs. "
            "Run 'buf generate' to generate proto/gen/python/ bindings."
        )

    async def cancel(self, alloc_id: str) -> bool:
        """
        Cancel a running or pending allocation.

        Args:
            alloc_id: Allocation ID to cancel.

        Returns:
            True if cancellation was successful.

        Raises:
            NotImplementedError: Requires generated protobuf stubs.
        """
        raise NotImplementedError(
            "cancel() requires generated protobuf stubs. "
            "Run 'buf generate' to generate proto/gen/python/ bindings."
        )

    async def list_nodes(self) -> list[Node]:
        """
        List all compute nodes in the cluster.

        Returns:
            List of Node objects with availability status.

        Raises:
            NotImplementedError: Requires generated protobuf stubs.
        """
        raise NotImplementedError(
            "list_nodes() requires generated protobuf stubs. "
            "Run 'buf generate' to generate proto/gen/python/ bindings."
        )

    async def watch(self, alloc_id: str) -> AsyncIterator[WatchEvent]:
        """
        Stream allocation lifecycle events (watch pattern).

        Yields events as the allocation transitions through states:
        created -> running -> completed (or failed/cancelled).

        Args:
            alloc_id: Allocation ID to watch.

        Yields:
            WatchEvent objects with event type and updated allocation state.

        Raises:
            NotImplementedError: Requires generated protobuf stubs.

        Example:
            async for event in client.watch(alloc_id):
                print(f"Event: {event.event_type}, State: {event.allocation.state}")
                if event.allocation.state == AllocationState.COMPLETED:
                    break
        """
        raise NotImplementedError(
            "watch() requires generated protobuf stubs. "
            "Run 'buf generate' to generate proto/gen/python/ bindings."
        )

    async def logs(
        self, alloc_id: str, follow: bool = False, lines: int = 100
    ) -> Iterator[LogEntry]:
        """
        Stream logs from an allocation.

        Logs are dual-path (ring buffer live + S3 persistent) per ADR-004.

        Args:
            alloc_id: Allocation ID to fetch logs from.
            follow: If True, follow new logs as they arrive (like `tail -f`).
            lines: Number of previous lines to retrieve. Defaults to 100.

        Yields:
            LogEntry objects with timestamp, level, and message.

        Raises:
            NotImplementedError: Requires generated protobuf stubs.

        Example:
            async for entry in client.logs(alloc_id, follow=True):
                print(f"[{entry.level}] {entry.message}")
        """
        raise NotImplementedError(
            "logs() requires generated protobuf stubs. "
            "Run 'buf generate' to generate proto/gen/python/ bindings."
        )

    async def metrics(self, alloc_id: str) -> AllocationMetrics:
        """
        Get current metrics for an allocation (GPU util, CPU, memory, I/O, etc).

        Metrics are queried from TSDB with configurable resolution per ADR-004.

        Args:
            alloc_id: Allocation ID to query.

        Returns:
            AllocationMetrics object with current utilization data.

        Raises:
            NotImplementedError: Requires generated protobuf stubs.
        """
        raise NotImplementedError(
            "metrics() requires generated protobuf stubs. "
            "Run 'buf generate' to generate proto/gen/python/ bindings."
        )

    async def stream_metrics(self, alloc_id: str) -> AsyncIterator[AllocationMetrics]:
        """
        Stream metrics from an allocation (real-time feed).

        Per ADR-004, StreamMetrics fans out to node agents with configurable resolution.

        Args:
            alloc_id: Allocation ID to stream metrics from.

        Yields:
            AllocationMetrics objects at regular intervals.

        Raises:
            NotImplementedError: Requires generated protobuf stubs.
        """
        raise NotImplementedError(
            "stream_metrics() requires generated protobuf stubs. "
            "Run 'buf generate' to generate proto/gen/python/ bindings."
        )
