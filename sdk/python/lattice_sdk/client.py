"""
Lattice HTTP client with Pythonic async interface.

The LatticeClient class provides high-level methods for interacting with a Lattice
scheduler's REST API using httpx. All methods are async and support context manager usage.
"""

import json
from typing import Any, AsyncIterator, Dict, List, Optional

import httpx

from .types import (
    Allocation,
    AllocationMetrics,
    AllocationSpec,
    LogEntry,
    Node,
    Tenant,
    VCluster,
    WatchEvent,
)


class LatticeError(Exception):
    """Base exception for Lattice client errors."""

    def __init__(self, message: str, status_code: Optional[int] = None):
        super().__init__(message)
        self.status_code = status_code


class LatticeNotFoundError(LatticeError):
    """Raised when a requested resource is not found (404)."""
    pass


class LatticeAuthError(LatticeError):
    """Raised when authentication fails (401/403)."""
    pass


class LatticeClient:
    """
    Python client for Lattice distributed workload scheduler.

    Connects to a lattice-api REST server and provides methods for job submission,
    status queries, cancellation, and observability (logs, metrics, watch streams).

    Example:
        async with LatticeClient("lattice-api.example.com", 8080) as client:
            alloc = await client.submit(AllocationSpec(
                entrypoint="python train.py",
                cpus=8,
                memory_gb=32,
                gpus=1,
            ))
            status = await client.status(alloc.id)
            print(f"Job {alloc.id}: {status.state}")
    """

    def __init__(
        self,
        host: str = "localhost",
        port: int = 8080,
        token: Optional[str] = None,
        timeout: float = 30.0,
        scheme: str = "http",
    ):
        """
        Initialize a Lattice client.

        Args:
            host: API server hostname or IP address. Defaults to "localhost".
            port: API server port. Defaults to 8080.
            token: Optional Bearer token for authentication.
            timeout: Request timeout in seconds. Defaults to 30.0.
            scheme: URL scheme ("http" or "https"). Defaults to "http".
        """
        self.host = host
        self.port = port
        self.token = token
        self.timeout = timeout
        self.scheme = scheme
        self.base_url = f"{scheme}://{host}:{port}"
        self._client: Optional[httpx.AsyncClient] = None
        self.connected = False

    def _headers(self) -> Dict[str, str]:
        """Build request headers including auth token if set."""
        headers: Dict[str, str] = {"Content-Type": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        return headers

    async def connect(self) -> None:
        """
        Create the underlying httpx.AsyncClient.

        This is called automatically when using the async context manager.
        """
        self._client = httpx.AsyncClient(
            base_url=self.base_url,
            headers=self._headers(),
            timeout=self.timeout,
        )
        self.connected = True

    async def close(self) -> None:
        """Close the underlying HTTP client."""
        if self._client:
            await self._client.aclose()
            self._client = None
            self.connected = False

    async def __aenter__(self) -> "LatticeClient":
        """Async context manager entry."""
        await self.connect()
        return self

    async def __aexit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> None:
        """Async context manager exit."""
        await self.close()

    def _ensure_client(self) -> httpx.AsyncClient:
        """Return the httpx client, raising if not connected."""
        if self._client is None:
            raise LatticeError("Client is not connected. Use 'async with' or call connect().")
        return self._client

    def _raise_for_status(self, response: httpx.Response) -> None:
        """Check HTTP response status and raise appropriate LatticeError."""
        if response.status_code == 404:
            raise LatticeNotFoundError(
                f"Resource not found: {response.text}", status_code=404
            )
        if response.status_code in (401, 403):
            raise LatticeAuthError(
                f"Authentication failed: {response.text}", status_code=response.status_code
            )
        if response.status_code >= 400:
            raise LatticeError(
                f"Request failed ({response.status_code}): {response.text}",
                status_code=response.status_code,
            )

    # ── Allocation Operations ──

    async def submit(self, spec: AllocationSpec) -> Allocation:
        """
        Submit an allocation (job or service) to Lattice.

        Args:
            spec: AllocationSpec describing the workload.

        Returns:
            The created Allocation with its assigned ID and initial state.

        Raises:
            LatticeError: On submission failure.
            LatticeAuthError: On authentication failure.
        """
        client = self._ensure_client()
        response = await client.post("/v1/allocations", json=spec.to_dict())
        self._raise_for_status(response)
        return Allocation.from_dict(response.json())

    async def status(self, alloc_id: str) -> Allocation:
        """
        Get the status and details of an allocation.

        Args:
            alloc_id: Allocation ID returned from submit().

        Returns:
            Allocation object with current state, resource usage, and metadata.

        Raises:
            LatticeNotFoundError: If the allocation does not exist.
        """
        client = self._ensure_client()
        response = await client.get(f"/v1/allocations/{alloc_id}")
        self._raise_for_status(response)
        return Allocation.from_dict(response.json())

    async def cancel(self, alloc_id: str) -> bool:
        """
        Cancel a running or pending allocation.

        Args:
            alloc_id: Allocation ID to cancel.

        Returns:
            True if cancellation was accepted.

        Raises:
            LatticeNotFoundError: If the allocation does not exist.
        """
        client = self._ensure_client()
        response = await client.delete(f"/v1/allocations/{alloc_id}")
        self._raise_for_status(response)
        return True

    async def list_allocations(
        self,
        tenant_id: Optional[str] = None,
        state: Optional[str] = None,
        limit: int = 100,
        offset: int = 0,
    ) -> List[Allocation]:
        """
        List allocations with optional filtering.

        Args:
            tenant_id: Filter by tenant ID. Optional.
            state: Filter by allocation state. Optional.
            limit: Maximum number of results to return. Defaults to 100.
            offset: Offset for pagination. Defaults to 0.

        Returns:
            List of Allocation objects.
        """
        client = self._ensure_client()
        params: Dict[str, Any] = {"limit": limit, "offset": offset}
        if tenant_id:
            params["tenant_id"] = tenant_id
        if state:
            params["state"] = state
        response = await client.get("/v1/allocations", params=params)
        self._raise_for_status(response)
        data = response.json()
        items = data if isinstance(data, list) else data.get("allocations", [])
        return [Allocation.from_dict(item) for item in items]

    # ── Node Operations ──

    async def list_nodes(self) -> List[Node]:
        """
        List all compute nodes in the cluster.

        Returns:
            List of Node objects with availability status.
        """
        client = self._ensure_client()
        response = await client.get("/v1/nodes")
        self._raise_for_status(response)
        data = response.json()
        items = data if isinstance(data, list) else data.get("nodes", [])
        return [Node.from_dict(item) for item in items]

    async def get_node(self, node_name: str) -> Node:
        """
        Get details for a specific node.

        Args:
            node_name: Node name.

        Returns:
            Node object.

        Raises:
            LatticeNotFoundError: If the node does not exist.
        """
        client = self._ensure_client()
        response = await client.get(f"/v1/nodes/{node_name}")
        self._raise_for_status(response)
        return Node.from_dict(response.json())

    # ── Observability ──

    async def watch(self, alloc_id: str) -> AsyncIterator[WatchEvent]:
        """
        Stream allocation lifecycle events via server-sent events (SSE).

        Yields events as the allocation transitions through states.

        Args:
            alloc_id: Allocation ID to watch.

        Yields:
            WatchEvent objects with event type and updated allocation state.
        """
        client = self._ensure_client()
        async with client.stream(
            "GET",
            f"/v1/allocations/{alloc_id}/watch",
            headers={"Accept": "text/event-stream"},
        ) as response:
            self._raise_for_status(response)
            async for line in response.aiter_lines():
                line = line.strip()
                if line.startswith("data:"):
                    data_str = line[5:].strip()
                    if data_str:
                        try:
                            event_data = json.loads(data_str)
                            yield WatchEvent.from_dict(event_data)
                        except (json.JSONDecodeError, KeyError):
                            continue

    async def logs(
        self, alloc_id: str, follow: bool = False, lines: int = 100
    ) -> AsyncIterator[LogEntry]:
        """
        Stream logs from an allocation.

        Args:
            alloc_id: Allocation ID to fetch logs from.
            follow: If True, follow new logs as they arrive (like tail -f).
            lines: Number of previous lines to retrieve. Defaults to 100.

        Yields:
            LogEntry objects with timestamp, level, and message.
        """
        client = self._ensure_client()
        params: Dict[str, Any] = {"lines": lines}
        if follow:
            params["follow"] = "true"

        if follow:
            async with client.stream(
                "GET",
                f"/v1/allocations/{alloc_id}/logs",
                params=params,
                headers={"Accept": "text/event-stream"},
            ) as response:
                self._raise_for_status(response)
                async for line in response.aiter_lines():
                    line = line.strip()
                    if line.startswith("data:"):
                        data_str = line[5:].strip()
                        if data_str:
                            try:
                                entry_data = json.loads(data_str)
                                yield LogEntry.from_dict(entry_data)
                            except (json.JSONDecodeError, KeyError):
                                continue
        else:
            response = await client.get(
                f"/v1/allocations/{alloc_id}/logs", params=params
            )
            self._raise_for_status(response)
            data = response.json()
            items = data if isinstance(data, list) else data.get("entries", [])
            for item in items:
                yield LogEntry.from_dict(item)

    async def metrics(self, alloc_id: str) -> AllocationMetrics:
        """
        Get current metrics for an allocation.

        Args:
            alloc_id: Allocation ID to query.

        Returns:
            AllocationMetrics object with current utilization data.
        """
        client = self._ensure_client()
        response = await client.get(f"/v1/allocations/{alloc_id}/metrics")
        self._raise_for_status(response)
        return AllocationMetrics.from_dict(response.json())

    async def stream_metrics(self, alloc_id: str) -> AsyncIterator[AllocationMetrics]:
        """
        Stream metrics from an allocation via SSE.

        Args:
            alloc_id: Allocation ID to stream metrics from.

        Yields:
            AllocationMetrics objects at regular intervals.
        """
        client = self._ensure_client()
        async with client.stream(
            "GET",
            f"/v1/allocations/{alloc_id}/metrics/stream",
            headers={"Accept": "text/event-stream"},
        ) as response:
            self._raise_for_status(response)
            async for line in response.aiter_lines():
                line = line.strip()
                if line.startswith("data:"):
                    data_str = line[5:].strip()
                    if data_str:
                        try:
                            metrics_data = json.loads(data_str)
                            yield AllocationMetrics.from_dict(metrics_data)
                        except (json.JSONDecodeError, KeyError):
                            continue

    # ── Tenant Operations ──

    async def list_tenants(self) -> List[Tenant]:
        """
        List all tenants.

        Returns:
            List of Tenant objects.
        """
        client = self._ensure_client()
        response = await client.get("/v1/tenants")
        self._raise_for_status(response)
        data = response.json()
        items = data if isinstance(data, list) else data.get("tenants", [])
        return [Tenant.from_dict(item) for item in items]

    async def get_tenant(self, tenant_id: str) -> Tenant:
        """
        Get details for a specific tenant.

        Args:
            tenant_id: Tenant ID.

        Returns:
            Tenant object.

        Raises:
            LatticeNotFoundError: If the tenant does not exist.
        """
        client = self._ensure_client()
        response = await client.get(f"/v1/tenants/{tenant_id}")
        self._raise_for_status(response)
        return Tenant.from_dict(response.json())

    # ── VCluster Operations ──

    async def list_vclusters(self) -> List[VCluster]:
        """
        List all vClusters.

        Returns:
            List of VCluster objects.
        """
        client = self._ensure_client()
        response = await client.get("/v1/vclusters")
        self._raise_for_status(response)
        data = response.json()
        items = data if isinstance(data, list) else data.get("vclusters", [])
        return [VCluster.from_dict(item) for item in items]

    async def get_vcluster(self, name: str) -> VCluster:
        """
        Get details for a specific vCluster.

        Args:
            name: VCluster name.

        Returns:
            VCluster object.

        Raises:
            LatticeNotFoundError: If the vCluster does not exist.
        """
        client = self._ensure_client()
        response = await client.get(f"/v1/vclusters/{name}")
        self._raise_for_status(response)
        return VCluster.from_dict(response.json())

    # ── Health ──

    async def health(self) -> Dict[str, Any]:
        """
        Check the health of the Lattice API server.

        Returns:
            Dictionary with health status information.
        """
        client = self._ensure_client()
        response = await client.get("/healthz")
        self._raise_for_status(response)
        return response.json()
