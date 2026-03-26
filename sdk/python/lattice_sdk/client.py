"""
Lattice HTTP client with Pythonic async interface.

The LatticeClient class provides high-level methods for interacting with a Lattice
scheduler's REST API using httpx. All methods are async and support context manager usage.
"""

import json
from typing import Any, AsyncIterator, Dict, List, Optional

import httpx

from .types import (
    AccountingUsage,
    Allocation,
    AllocationMetrics,
    AllocationSpec,
    AuditEntry,
    BackupResult,
    Dag,
    DagSpec,
    DiagnosticsReport,
    LogEntry,
    Node,
    QueueInfo,
    RaftStatus,
    Session,
    Tenant,
    TenantUsage,
    UserUsage,
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
        response = await client.post("/api/v1/allocations", json=spec.to_dict())
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
        response = await client.get(f"/api/v1/allocations/{alloc_id}")
        self._raise_for_status(response)
        return Allocation.from_dict(response.json())

    async def patch_allocation(
        self, alloc_id: str, patch: Dict[str, Any]
    ) -> Allocation:
        """
        Patch an existing allocation with partial updates.

        Args:
            alloc_id: Allocation ID to patch.
            patch: Dictionary of fields to update.

        Returns:
            The updated Allocation.

        Raises:
            LatticeNotFoundError: If the allocation does not exist.
        """
        client = self._ensure_client()
        response = await client.patch(
            f"/api/v1/allocations/{alloc_id}", json=patch
        )
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
        response = await client.post(f"/api/v1/allocations/{alloc_id}/cancel")
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
        response = await client.get("/api/v1/allocations", params=params)
        self._raise_for_status(response)
        data = response.json()
        items = data if isinstance(data, list) else data.get("allocations", [])
        return [Allocation.from_dict(item) for item in items]

    async def get_diagnostics(self, alloc_id: str) -> DiagnosticsReport:
        """
        Get diagnostics report for an allocation.

        Args:
            alloc_id: Allocation ID to get diagnostics for.

        Returns:
            DiagnosticsReport with scheduler decisions, resource usage, and warnings.

        Raises:
            LatticeNotFoundError: If the allocation does not exist.
        """
        client = self._ensure_client()
        response = await client.get(
            f"/api/v1/allocations/{alloc_id}/diagnostics"
        )
        self._raise_for_status(response)
        return DiagnosticsReport.from_dict(response.json())

    # ── Node Operations ──

    async def list_nodes(self) -> List[Node]:
        """
        List all compute nodes in the cluster.

        Returns:
            List of Node objects with availability status.
        """
        client = self._ensure_client()
        response = await client.get("/api/v1/nodes")
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
        response = await client.get(f"/api/v1/nodes/{node_name}")
        self._raise_for_status(response)
        return Node.from_dict(response.json())

    async def drain_node(self, node_id: str) -> bool:
        """
        Drain a node, preventing new allocations from being scheduled on it.

        Args:
            node_id: Node ID to drain.

        Returns:
            True if the drain request was accepted.

        Raises:
            LatticeNotFoundError: If the node does not exist.
        """
        client = self._ensure_client()
        response = await client.post(f"/api/v1/nodes/{node_id}/drain")
        self._raise_for_status(response)
        return True

    async def undrain_node(self, node_id: str) -> bool:
        """
        Undrain a node, allowing new allocations to be scheduled on it.

        Args:
            node_id: Node ID to undrain.

        Returns:
            True if the undrain request was accepted.

        Raises:
            LatticeNotFoundError: If the node does not exist.
        """
        client = self._ensure_client()
        response = await client.post(f"/api/v1/nodes/{node_id}/undrain")
        self._raise_for_status(response)
        return True

    async def disable_node(self, node_id: str, reason: str = "") -> bool:
        """
        Disable a node (mark as unavailable until re-enabled).

        Args:
            node_id: Node ID to disable.
            reason: Reason for disabling.

        Returns:
            True if the disable request was accepted.

        Raises:
            LatticeNotFoundError: If the node does not exist.
        """
        client = self._ensure_client()
        body: Dict[str, Any] = {"reason": reason}
        response = await client.post(f"/api/v1/nodes/{node_id}/disable", json=body)
        self._raise_for_status(response)
        return True

    async def enable_node(self, node_id: str) -> bool:
        """
        Re-enable a previously disabled node.

        Args:
            node_id: Node ID to enable.

        Returns:
            True if the enable request was accepted.

        Raises:
            LatticeNotFoundError: If the node does not exist.
        """
        client = self._ensure_client()
        response = await client.post(f"/api/v1/nodes/{node_id}/enable")
        self._raise_for_status(response)
        return True

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
            f"/api/v1/allocations/{alloc_id}/watch",
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
                f"/api/v1/allocations/{alloc_id}/logs",
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
                f"/api/v1/allocations/{alloc_id}/logs", params=params
            )
            self._raise_for_status(response)
            data = response.json()
            items = data if isinstance(data, list) else data.get("entries", [])
            for item in items:
                yield LogEntry.from_dict(item)

    async def stream_logs(self, alloc_id: str) -> AsyncIterator[LogEntry]:
        """
        Stream logs from an allocation via SSE (dedicated streaming endpoint).

        Unlike logs(follow=True), this uses the dedicated /logs/stream endpoint
        which is optimized for real-time log streaming.

        Args:
            alloc_id: Allocation ID to stream logs from.

        Yields:
            LogEntry objects with timestamp, level, and message.
        """
        client = self._ensure_client()
        async with client.stream(
            "GET",
            f"/api/v1/allocations/{alloc_id}/logs/stream",
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

    async def metrics(self, alloc_id: str) -> AllocationMetrics:
        """
        Get current metrics for an allocation.

        Args:
            alloc_id: Allocation ID to query.

        Returns:
            AllocationMetrics object with current utilization data.
        """
        client = self._ensure_client()
        response = await client.get(f"/api/v1/allocations/{alloc_id}/metrics")
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
            f"/api/v1/allocations/{alloc_id}/metrics/stream",
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

    # ── Session Operations ──

    async def create_session(
        self, allocation_id: str, user_id: Optional[str] = None
    ) -> Session:
        """
        Create an interactive session attached to an allocation.

        Args:
            allocation_id: Allocation ID to attach the session to.
            user_id: Optional user ID for the session.

        Returns:
            The created Session.

        Raises:
            LatticeNotFoundError: If the allocation does not exist.
        """
        client = self._ensure_client()
        body: Dict[str, Any] = {"allocation_id": allocation_id}
        if user_id is not None:
            body["user_id"] = user_id
        response = await client.post("/api/v1/sessions", json=body)
        self._raise_for_status(response)
        return Session.from_dict(response.json())

    async def get_session(self, session_id: str) -> Session:
        """
        Get details for a specific session.

        Args:
            session_id: Session ID.

        Returns:
            Session object.

        Raises:
            LatticeNotFoundError: If the session does not exist.
        """
        client = self._ensure_client()
        response = await client.get(f"/api/v1/sessions/{session_id}")
        self._raise_for_status(response)
        return Session.from_dict(response.json())

    async def delete_session(self, session_id: str) -> bool:
        """
        Delete (terminate) a session.

        Args:
            session_id: Session ID to delete.

        Returns:
            True if the session was deleted.

        Raises:
            LatticeNotFoundError: If the session does not exist.
        """
        client = self._ensure_client()
        response = await client.delete(f"/api/v1/sessions/{session_id}")
        self._raise_for_status(response)
        return True

    # ── DAG Operations ──

    async def submit_dag(self, spec: DagSpec) -> Dag:
        """
        Submit a DAG (directed acyclic graph) of allocations.

        Args:
            spec: DagSpec describing the DAG structure and allocations.

        Returns:
            The created Dag with its assigned ID and initial state.

        Raises:
            LatticeError: On submission failure.
        """
        client = self._ensure_client()
        response = await client.post("/api/v1/dags", json=spec.to_dict())
        self._raise_for_status(response)
        return Dag.from_dict(response.json())

    async def list_dags(
        self,
        tenant_id: Optional[str] = None,
        limit: int = 100,
        offset: int = 0,
    ) -> List[Dag]:
        """
        List DAGs with optional filtering.

        Args:
            tenant_id: Filter by tenant ID. Optional.
            limit: Maximum number of results to return. Defaults to 100.
            offset: Offset for pagination. Defaults to 0.

        Returns:
            List of Dag objects.
        """
        client = self._ensure_client()
        params: Dict[str, Any] = {"limit": limit, "offset": offset}
        if tenant_id:
            params["tenant_id"] = tenant_id
        response = await client.get("/api/v1/dags", params=params)
        self._raise_for_status(response)
        data = response.json()
        items = data if isinstance(data, list) else data.get("dags", [])
        return [Dag.from_dict(item) for item in items]

    async def get_dag(self, dag_id: str) -> Dag:
        """
        Get details for a specific DAG.

        Args:
            dag_id: DAG ID.

        Returns:
            Dag object.

        Raises:
            LatticeNotFoundError: If the DAG does not exist.
        """
        client = self._ensure_client()
        response = await client.get(f"/api/v1/dags/{dag_id}")
        self._raise_for_status(response)
        return Dag.from_dict(response.json())

    async def cancel_dag(self, dag_id: str) -> bool:
        """
        Cancel a DAG and all its pending allocations.

        Args:
            dag_id: DAG ID to cancel.

        Returns:
            True if cancellation was accepted.

        Raises:
            LatticeNotFoundError: If the DAG does not exist.
        """
        client = self._ensure_client()
        response = await client.post(f"/api/v1/dags/{dag_id}/cancel")
        self._raise_for_status(response)
        return True

    # ── Audit Operations ──

    async def query_audit(
        self,
        tenant_id: Optional[str] = None,
        user_id: Optional[str] = None,
        action: Optional[str] = None,
        limit: int = 100,
        offset: int = 0,
    ) -> List[AuditEntry]:
        """
        Query audit log entries with optional filtering.

        Args:
            tenant_id: Filter by tenant ID. Optional.
            user_id: Filter by user ID. Optional.
            action: Filter by action type. Optional.
            limit: Maximum number of results to return. Defaults to 100.
            offset: Offset for pagination. Defaults to 0.

        Returns:
            List of AuditEntry objects.
        """
        client = self._ensure_client()
        params: Dict[str, Any] = {"limit": limit, "offset": offset}
        if tenant_id:
            params["tenant_id"] = tenant_id
        if user_id:
            params["user_id"] = user_id
        if action:
            params["action"] = action
        response = await client.get("/api/v1/audit", params=params)
        self._raise_for_status(response)
        data = response.json()
        items = data if isinstance(data, list) else data.get("entries", [])
        return [AuditEntry.from_dict(item) for item in items]

    # ── Tenant Operations ──

    async def create_tenant(
        self,
        name: str,
        max_nodes: int = 100,
        fair_share_target: float = 0.0,
        gpu_hours_budget: Optional[float] = None,
        node_hours_budget: Optional[float] = None,
        max_concurrent_allocations: Optional[int] = None,
        burst_allowance: Optional[float] = None,
        isolation_level: str = "standard",
    ) -> Tenant:
        """
        Create a new tenant.

        Args:
            name: Tenant name (also used as ID).
            max_nodes: Maximum nodes this tenant can use simultaneously.
            fair_share_target: Target fair-share fraction (0.0 - 1.0).
            gpu_hours_budget: GPU-hours budget (None = unlimited).
            node_hours_budget: Node-hours budget (None = unlimited).
            max_concurrent_allocations: Hard limit on concurrent allocations.
            burst_allowance: Burst multiplier (e.g. 1.5 = 150% of fair share when idle).
            isolation_level: "standard" or "strict".

        Returns:
            The created Tenant.
        """
        client = self._ensure_client()
        body: Dict[str, Any] = {
            "name": name,
            "max_nodes": max_nodes,
            "fair_share_target": fair_share_target,
            "isolation_level": isolation_level,
        }
        if gpu_hours_budget is not None:
            body["gpu_hours_budget"] = gpu_hours_budget
        if node_hours_budget is not None:
            body["node_hours_budget"] = node_hours_budget
        if max_concurrent_allocations is not None:
            body["max_concurrent_allocations"] = max_concurrent_allocations
        if burst_allowance is not None:
            body["burst_allowance"] = burst_allowance
        response = await client.post("/api/v1/tenants", json=body)
        self._raise_for_status(response)
        return Tenant.from_dict(response.json())

    async def list_tenants(self) -> List[Tenant]:
        """
        List all tenants.

        Returns:
            List of Tenant objects.
        """
        client = self._ensure_client()
        response = await client.get("/api/v1/tenants")
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
        response = await client.get(f"/api/v1/tenants/{tenant_id}")
        self._raise_for_status(response)
        return Tenant.from_dict(response.json())

    async def update_tenant(
        self, tenant_id: str, updates: Dict[str, Any]
    ) -> Tenant:
        """
        Update a tenant's configuration.

        Args:
            tenant_id: Tenant ID to update.
            updates: Dictionary of fields to update (e.g., quota_cpus, name).

        Returns:
            The updated Tenant.

        Raises:
            LatticeNotFoundError: If the tenant does not exist.
        """
        client = self._ensure_client()
        response = await client.put(
            f"/api/v1/tenants/{tenant_id}", json=updates
        )
        self._raise_for_status(response)
        return Tenant.from_dict(response.json())

    # ── VCluster Operations ──

    async def create_vcluster(
        self,
        name: str,
        scheduler_type: str,
        resource_filter: Optional[Dict[str, str]] = None,
        scheduler_weights: Optional[Dict[str, float]] = None,
    ) -> VCluster:
        """
        Create a new vCluster.

        Args:
            name: VCluster name.
            scheduler_type: Scheduler type (e.g., "hpc_backfill", "service_bin_pack").
            resource_filter: Optional resource filter for node selection.
            scheduler_weights: Optional cost function weight overrides.

        Returns:
            The created VCluster.
        """
        client = self._ensure_client()
        body: Dict[str, Any] = {
            "name": name,
            "scheduler_type": scheduler_type,
        }
        if resource_filter:
            body["resource_filter"] = resource_filter
        if scheduler_weights:
            body["scheduler_weights"] = scheduler_weights
        response = await client.post("/api/v1/vclusters", json=body)
        self._raise_for_status(response)
        return VCluster.from_dict(response.json())

    async def list_vclusters(self) -> List[VCluster]:
        """
        List all vClusters.

        Returns:
            List of VCluster objects.
        """
        client = self._ensure_client()
        response = await client.get("/api/v1/vclusters")
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
        response = await client.get(f"/api/v1/vclusters/{name}")
        self._raise_for_status(response)
        return VCluster.from_dict(response.json())

    async def vcluster_queue(self, name: str) -> QueueInfo:
        """
        Get queue information for a specific vCluster.

        Args:
            name: VCluster name.

        Returns:
            QueueInfo with pending/running counts and allocation details.

        Raises:
            LatticeNotFoundError: If the vCluster does not exist.
        """
        client = self._ensure_client()
        response = await client.get(f"/api/v1/vclusters/{name}/queue")
        self._raise_for_status(response)
        return QueueInfo.from_dict(response.json())

    # ── Accounting Operations ──

    async def accounting_usage(
        self,
        tenant_id: Optional[str] = None,
        start: Optional[str] = None,
        end: Optional[str] = None,
    ) -> AccountingUsage:
        """
        Get resource usage accounting data.

        Args:
            tenant_id: Filter by tenant ID. Optional.
            start: Start of reporting period (ISO 8601). Optional.
            end: End of reporting period (ISO 8601). Optional.

        Returns:
            AccountingUsage with resource consumption totals.
        """
        client = self._ensure_client()
        params: Dict[str, Any] = {}
        if tenant_id:
            params["tenant_id"] = tenant_id
        if start:
            params["start"] = start
        if end:
            params["end"] = end
        response = await client.get("/api/v1/accounting/usage", params=params)
        self._raise_for_status(response)
        return AccountingUsage.from_dict(response.json())

    # ── Budget Usage ──

    async def tenant_usage(
        self, tenant_id: str, days: int = 90
    ) -> TenantUsage:
        """
        Get budget usage (GPU-hours, node-hours) for a tenant.

        Args:
            tenant_id: Tenant ID to query.
            days: Number of days to look back (rolling window). Defaults to 90.

        Returns:
            TenantUsage with GPU-hours and node-hours consumption.

        Raises:
            LatticeNotFoundError: If the tenant does not exist.
        """
        client = self._ensure_client()
        params: Dict[str, Any] = {"days": days}
        response = await client.get(
            f"/api/v1/tenants/{tenant_id}/usage", params=params
        )
        self._raise_for_status(response)
        return TenantUsage.from_dict(response.json())

    async def user_usage(
        self, user: str, days: int = 90
    ) -> UserUsage:
        """
        Get budget usage for a user across all tenants.

        Args:
            user: User ID to query.
            days: Number of days to look back (rolling window). Defaults to 90.

        Returns:
            UserUsage with per-tenant breakdown.
        """
        client = self._ensure_client()
        params: Dict[str, Any] = {"user": user, "days": days}
        response = await client.get("/api/v1/usage", params=params)
        self._raise_for_status(response)
        return UserUsage.from_dict(response.json())

    # ── Raft Operations ──

    async def raft_status(self) -> RaftStatus:
        """
        Get the Raft consensus cluster status.

        Returns:
            RaftStatus with node role, term, leader, and member information.
        """
        client = self._ensure_client()
        response = await client.get("/api/v1/raft/status")
        self._raise_for_status(response)
        return RaftStatus.from_dict(response.json())

    # ── Admin Operations ──

    async def create_backup(self) -> BackupResult:
        """
        Create a backup of the Raft state.

        Returns:
            BackupResult with backup ID and status.
        """
        client = self._ensure_client()
        response = await client.post("/api/v1/admin/backup")
        self._raise_for_status(response)
        return BackupResult.from_dict(response.json())

    async def verify_backup(self) -> BackupResult:
        """
        Verify the integrity of the most recent backup.

        Returns:
            BackupResult indicating verification status.
        """
        client = self._ensure_client()
        response = await client.post("/api/v1/admin/backup/verify")
        self._raise_for_status(response)
        return BackupResult.from_dict(response.json())

    async def restore_backup(self) -> BackupResult:
        """
        Restore from the most recent backup.

        Returns:
            BackupResult indicating restore status.
        """
        client = self._ensure_client()
        response = await client.post("/api/v1/admin/backup/restore")
        self._raise_for_status(response)
        return BackupResult.from_dict(response.json())

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

    # ── Service Discovery ────────────────────────────────────

    async def list_services(self) -> List[str]:
        """
        List all registered service names from the service registry.

        Returns:
            List of service name strings.
        """
        client = self._ensure_client()
        response = await client.get("/api/v1/services")
        self._raise_for_status(response)
        data = response.json()
        return data.get("services", [])

    async def lookup_service(self, name: str) -> Dict[str, Any]:
        """
        Look up endpoints for a named service.

        Args:
            name: Service name to look up.

        Returns:
            Dictionary with service name and list of endpoints, each containing
            allocation_id, tenant, nodes, port, and protocol.
        """
        client = self._ensure_client()
        response = await client.get(f"/api/v1/services/{name}")
        self._raise_for_status(response)
        return response.json()
