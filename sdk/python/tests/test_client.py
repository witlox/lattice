"""
Unit tests for LatticeClient (httpx-based) and types.

Tests cover client initialization, HTTP method behavior with mocked responses,
error handling, and type serialization roundtrips.
"""

import json
import pytest
from datetime import datetime, timezone

import sys
from pathlib import Path

# Add parent directory to path so we can import lattice_sdk
sys.path.insert(0, str(Path(__file__).parent.parent))

from lattice_sdk import (
    LatticeClient,
    LatticeError,
    LatticeNotFoundError,
    LatticeAuthError,
    Allocation,
    AllocationMetrics,
    AllocationSpec,
    AllocationState,
    LogEntry,
    Node,
    ResourceRequest,
    Tenant,
    VCluster,
    WatchEvent,
)


# ── Fixtures ──


def _sample_allocation_dict(alloc_id="alloc-001", state="pending"):
    return {
        "id": alloc_id,
        "entrypoint": "python train.py",
        "state": state,
        "requested_resources": {
            "cpus": 4.0,
            "memory_gb": 16.0,
            "gpus": 1,
            "gpu_memory_gb": 24.0,
            "storage_gb": 50.0,
        },
        "tenant_id": "tenant-001",
        "user_id": "user-001",
        "priority_class": "normal",
    }


def _sample_node_dict(name="node-001"):
    return {
        "name": name,
        "host": "192.168.1.1",
        "status": "available",
        "total_cpus": 64.0,
        "total_memory_gb": 256.0,
        "total_gpus": 4,
        "available_cpus": 32.0,
        "available_memory_gb": 128.0,
        "available_gpus": 2,
    }


def _sample_tenant_dict(tenant_id="tenant-001"):
    return {
        "id": tenant_id,
        "name": "team-ai",
        "quota_cpus": 1000.0,
        "quota_memory_gb": 10000.0,
        "quota_gpus": 100,
        "used_cpus": 200.0,
        "used_memory_gb": 2000.0,
        "used_gpus": 20,
    }


def _sample_vcluster_dict(name="hpc-backfill"):
    return {
        "name": name,
        "scheduler_type": "hpc_backfill",
        "resource_filter": {"gpu_type": "h100"},
        "scheduler_weights": {"priority_class": 1.0},
    }


def _sample_metrics_dict(alloc_id="alloc-001"):
    return {
        "allocation_id": alloc_id,
        "timestamp": "2025-01-15T10:30:00+00:00",
        "cpu_utilization": 75.5,
        "memory_utilization_gb": 12.3,
        "gpu_utilization": 95.0,
        "gpu_memory_utilization_gb": 20.0,
        "network_in_mbps": 500.0,
        "network_out_mbps": 300.0,
        "io_read_mbps": 100.0,
        "io_write_mbps": 50.0,
    }


# ── Client Initialization Tests ──


class TestLatticeClientInit:
    """Tests for LatticeClient initialization."""

    def test_client_default_init(self):
        """Test that LatticeClient can be constructed with default values."""
        client = LatticeClient()
        assert client.host == "localhost"
        assert client.port == 8080
        assert client.token is None
        assert client.connected is False
        assert client.base_url == "http://localhost:8080"

    def test_client_custom_init(self):
        """Test that LatticeClient can be constructed with custom values."""
        client = LatticeClient(
            host="lattice-api.example.com", port=9999, token="secret", scheme="https"
        )
        assert client.host == "lattice-api.example.com"
        assert client.port == 9999
        assert client.token == "secret"
        assert client.connected is False
        assert client.base_url == "https://lattice-api.example.com:9999"

    def test_client_partial_init(self):
        """Test that LatticeClient can be constructed with mixed defaults."""
        client = LatticeClient(host="192.168.1.100")
        assert client.host == "192.168.1.100"
        assert client.port == 8080
        assert client.token is None

    def test_client_headers_without_token(self):
        """Test headers without auth token."""
        client = LatticeClient()
        headers = client._headers()
        assert "Content-Type" in headers
        assert "Authorization" not in headers

    def test_client_headers_with_token(self):
        """Test headers with auth token."""
        client = LatticeClient(token="my-token")
        headers = client._headers()
        assert headers["Authorization"] == "Bearer my-token"

    def test_client_not_connected_raises(self):
        """Test that methods raise LatticeError when not connected."""
        client = LatticeClient()
        with pytest.raises(LatticeError, match="not connected"):
            client._ensure_client()

    @pytest.mark.asyncio
    async def test_connect_sets_connected(self):
        """Test that connect() sets connected flag."""
        client = LatticeClient()
        await client.connect()
        assert client.connected is True
        await client.close()
        assert client.connected is False

    @pytest.mark.asyncio
    async def test_context_manager(self):
        """Test async context manager connects and disconnects."""
        async with LatticeClient() as client:
            assert client.connected is True
        assert client.connected is False


# ── Submit Tests ──


class TestSubmit:
    """Tests for submit() method."""

    @pytest.mark.asyncio
    async def test_submit_success(self, httpx_mock):
        """Test successful allocation submission."""
        alloc_dict = _sample_allocation_dict()
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations",
            method="POST",
            json=alloc_dict,
            status_code=201,
        )
        async with LatticeClient() as client:
            spec = AllocationSpec(entrypoint="python train.py", cpus=4.0, memory_gb=16.0, gpus=1)
            alloc = await client.submit(spec)
            assert alloc.id == "alloc-001"
            assert alloc.state == AllocationState.PENDING

    @pytest.mark.asyncio
    async def test_submit_auth_error(self, httpx_mock):
        """Test submission with auth failure."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations",
            method="POST",
            status_code=401,
            text="Unauthorized",
        )
        async with LatticeClient() as client:
            spec = AllocationSpec(entrypoint="echo hello")
            with pytest.raises(LatticeAuthError):
                await client.submit(spec)

    @pytest.mark.asyncio
    async def test_submit_server_error(self, httpx_mock):
        """Test submission with server error."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations",
            method="POST",
            status_code=500,
            text="Internal Server Error",
        )
        async with LatticeClient() as client:
            spec = AllocationSpec(entrypoint="echo hello")
            with pytest.raises(LatticeError) as exc_info:
                await client.submit(spec)
            assert exc_info.value.status_code == 500


# ── Status Tests ──


class TestStatus:
    """Tests for status() method."""

    @pytest.mark.asyncio
    async def test_status_success(self, httpx_mock):
        """Test successful status query."""
        alloc_dict = _sample_allocation_dict("alloc-123", "running")
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations/alloc-123",
            method="GET",
            json=alloc_dict,
        )
        async with LatticeClient() as client:
            alloc = await client.status("alloc-123")
            assert alloc.id == "alloc-123"
            assert alloc.state == AllocationState.RUNNING

    @pytest.mark.asyncio
    async def test_status_not_found(self, httpx_mock):
        """Test status query for non-existent allocation."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations/nonexistent",
            method="GET",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.status("nonexistent")

    @pytest.mark.asyncio
    async def test_status_forbidden(self, httpx_mock):
        """Test status query with permission denied."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations/alloc-123",
            method="GET",
            status_code=403,
            text="Forbidden",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeAuthError):
                await client.status("alloc-123")


# ── Cancel Tests ──


class TestCancel:
    """Tests for cancel() method."""

    @pytest.mark.asyncio
    async def test_cancel_success(self, httpx_mock):
        """Test successful cancellation."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations/alloc-123",
            method="DELETE",
            status_code=200,
            json={"status": "cancelled"},
        )
        async with LatticeClient() as client:
            result = await client.cancel("alloc-123")
            assert result is True

    @pytest.mark.asyncio
    async def test_cancel_not_found(self, httpx_mock):
        """Test cancellation of non-existent allocation."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations/nonexistent",
            method="DELETE",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.cancel("nonexistent")


# ── List Allocations Tests ──


class TestListAllocations:
    """Tests for list_allocations() method."""

    @pytest.mark.asyncio
    async def test_list_allocations_array_response(self, httpx_mock):
        """Test listing allocations with array response."""
        allocs = [_sample_allocation_dict("a1"), _sample_allocation_dict("a2", "running")]
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations",
            method="GET",
            json=allocs,
        )
        async with LatticeClient() as client:
            result = await client.list_allocations()
            assert len(result) == 2
            assert result[0].id == "a1"
            assert result[1].id == "a2"

    @pytest.mark.asyncio
    async def test_list_allocations_object_response(self, httpx_mock):
        """Test listing allocations with wrapped object response."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations",
            method="GET",
            json={"allocations": [_sample_allocation_dict("a1")], "total": 1},
        )
        async with LatticeClient() as client:
            result = await client.list_allocations()
            assert len(result) == 1

    @pytest.mark.asyncio
    async def test_list_allocations_empty(self, httpx_mock):
        """Test listing allocations with empty result."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations",
            method="GET",
            json=[],
        )
        async with LatticeClient() as client:
            result = await client.list_allocations()
            assert len(result) == 0


# ── Node Tests ──


class TestNodes:
    """Tests for node-related methods."""

    @pytest.mark.asyncio
    async def test_list_nodes(self, httpx_mock):
        """Test listing nodes."""
        nodes = [_sample_node_dict("n1"), _sample_node_dict("n2")]
        httpx_mock.add_response(
            url="http://localhost:8080/v1/nodes",
            method="GET",
            json=nodes,
        )
        async with LatticeClient() as client:
            result = await client.list_nodes()
            assert len(result) == 2
            assert result[0].name == "n1"

    @pytest.mark.asyncio
    async def test_get_node(self, httpx_mock):
        """Test getting a specific node."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/nodes/node-001",
            method="GET",
            json=_sample_node_dict(),
        )
        async with LatticeClient() as client:
            node = await client.get_node("node-001")
            assert node.name == "node-001"
            assert node.total_gpus == 4


# ── Metrics Tests ──


class TestMetrics:
    """Tests for metrics methods."""

    @pytest.mark.asyncio
    async def test_metrics_success(self, httpx_mock):
        """Test getting allocation metrics."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations/alloc-001/metrics",
            method="GET",
            json=_sample_metrics_dict(),
        )
        async with LatticeClient() as client:
            metrics = await client.metrics("alloc-001")
            assert metrics.allocation_id == "alloc-001"
            assert metrics.cpu_utilization == 75.5
            assert metrics.gpu_utilization == 95.0


# ── Logs Tests ──


class TestLogs:
    """Tests for logs method (non-follow mode)."""

    @pytest.mark.asyncio
    async def test_logs_non_follow(self, httpx_mock):
        """Test getting logs without follow."""
        now_str = datetime.now(timezone.utc).isoformat()
        log_entries = [
            {"timestamp": now_str, "level": "INFO", "message": "Starting"},
            {"timestamp": now_str, "level": "INFO", "message": "Running"},
        ]
        httpx_mock.add_response(
            url="http://localhost:8080/v1/allocations/alloc-001/logs",
            method="GET",
            json=log_entries,
        )
        async with LatticeClient() as client:
            entries = []
            async for entry in client.logs("alloc-001"):
                entries.append(entry)
            assert len(entries) == 2
            assert entries[0].message == "Starting"
            assert entries[1].level == "INFO"


# ── Tenant Tests ──


class TestTenants:
    """Tests for tenant methods."""

    @pytest.mark.asyncio
    async def test_list_tenants(self, httpx_mock):
        """Test listing tenants."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/tenants",
            method="GET",
            json=[_sample_tenant_dict()],
        )
        async with LatticeClient() as client:
            tenants = await client.list_tenants()
            assert len(tenants) == 1
            assert tenants[0].name == "team-ai"

    @pytest.mark.asyncio
    async def test_get_tenant(self, httpx_mock):
        """Test getting a specific tenant."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/tenants/tenant-001",
            method="GET",
            json=_sample_tenant_dict(),
        )
        async with LatticeClient() as client:
            tenant = await client.get_tenant("tenant-001")
            assert tenant.id == "tenant-001"
            assert tenant.quota_gpus == 100


# ── VCluster Tests ──


class TestVClusters:
    """Tests for vCluster methods."""

    @pytest.mark.asyncio
    async def test_list_vclusters(self, httpx_mock):
        """Test listing vClusters."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/vclusters",
            method="GET",
            json=[_sample_vcluster_dict()],
        )
        async with LatticeClient() as client:
            vclusters = await client.list_vclusters()
            assert len(vclusters) == 1
            assert vclusters[0].name == "hpc-backfill"

    @pytest.mark.asyncio
    async def test_get_vcluster(self, httpx_mock):
        """Test getting a specific vCluster."""
        httpx_mock.add_response(
            url="http://localhost:8080/v1/vclusters/hpc-backfill",
            method="GET",
            json=_sample_vcluster_dict(),
        )
        async with LatticeClient() as client:
            vc = await client.get_vcluster("hpc-backfill")
            assert vc.scheduler_type == "hpc_backfill"


# ── Health Tests ──


class TestHealth:
    """Tests for health endpoint."""

    @pytest.mark.asyncio
    async def test_health(self, httpx_mock):
        """Test health check."""
        httpx_mock.add_response(
            url="http://localhost:8080/healthz",
            method="GET",
            json={"status": "healthy", "version": "0.1.0"},
        )
        async with LatticeClient() as client:
            health = await client.health()
            assert health["status"] == "healthy"


# ── Type Serialization Tests ──


class TestResourceRequestSerialization:
    """Tests for ResourceRequest to_dict/from_dict."""

    def test_roundtrip(self):
        req = ResourceRequest(cpus=4.0, memory_gb=16.0, gpus=2, gpu_memory_gb=40.0)
        d = req.to_dict()
        restored = ResourceRequest.from_dict(d)
        assert restored.cpus == 4.0
        assert restored.memory_gb == 16.0
        assert restored.gpus == 2
        assert restored.gpu_memory_gb == 40.0

    def test_from_dict_defaults(self):
        req = ResourceRequest.from_dict({})
        assert req.cpus == 1.0
        assert req.memory_gb == 4.0
        assert req.gpus == 0


class TestNodeSerialization:
    """Tests for Node to_dict/from_dict."""

    def test_roundtrip(self):
        node = Node(
            name="node-001",
            host="10.0.0.1",
            status="available",
            total_cpus=64.0,
            total_memory_gb=256.0,
            total_gpus=4,
        )
        d = node.to_dict()
        restored = Node.from_dict(d)
        assert restored.name == "node-001"
        assert restored.total_gpus == 4

    def test_with_heartbeat(self):
        now = datetime.now(timezone.utc)
        node = Node(
            name="n", host="h", status="s", total_cpus=1.0,
            total_memory_gb=1.0, last_heartbeat=now,
        )
        d = node.to_dict()
        restored = Node.from_dict(d)
        assert restored.last_heartbeat is not None


class TestAllocationSerialization:
    """Tests for Allocation to_dict/from_dict."""

    def test_roundtrip_minimal(self):
        alloc = Allocation(
            id="a1",
            entrypoint="echo hello",
            state=AllocationState.PENDING,
            requested_resources=ResourceRequest(cpus=1.0, memory_gb=2.0),
            tenant_id="t1",
            user_id="u1",
        )
        d = alloc.to_dict()
        assert d["state"] == "pending"
        restored = Allocation.from_dict(d)
        assert restored.id == "a1"
        assert restored.state == AllocationState.PENDING

    def test_roundtrip_full(self):
        now = datetime.now(timezone.utc)
        alloc = Allocation(
            id="a2",
            entrypoint="python train.py",
            state=AllocationState.RUNNING,
            requested_resources=ResourceRequest(cpus=8.0, memory_gb=32.0, gpus=2),
            tenant_id="t1",
            user_id="u1",
            priority_class="high",
            network_domain="vni-100",
            uenv="pytorch-2.0",
            nodes_allocated=["n1", "n2"],
            created_at=now,
            started_at=now,
            labels={"app": "training"},
            annotations={"note": "test"},
            exit_code=None,
            message="In progress",
        )
        d = alloc.to_dict()
        restored = Allocation.from_dict(d)
        assert restored.state == AllocationState.RUNNING
        assert restored.network_domain == "vni-100"
        assert restored.nodes_allocated == ["n1", "n2"]
        assert restored.message == "In progress"

    def test_from_dict_unknown_state_defaults(self):
        data = _sample_allocation_dict()
        data["state"] = "unknown_state"
        alloc = Allocation.from_dict(data)
        assert alloc.state == AllocationState.PENDING


class TestAllocationSpecSerialization:
    """Tests for AllocationSpec to_dict/from_dict."""

    def test_roundtrip(self):
        spec = AllocationSpec(
            entrypoint="python train.py",
            nodes=4,
            cpus=16.0,
            memory_gb=64.0,
            gpus=2,
            priority_class="high",
            tenant_id="t1",
            labels={"env": "prod"},
        )
        d = spec.to_dict()
        assert d["entrypoint"] == "python train.py"
        assert d["nodes"] == 4
        assert d["resources"]["cpus"] == 16.0
        assert d["resources"]["gpus"] == 2
        restored = AllocationSpec.from_dict(d)
        assert restored.entrypoint == "python train.py"
        assert restored.nodes == 4
        assert restored.cpus == 16.0
        assert restored.gpus == 2
        assert restored.tenant_id == "t1"


class TestAllocationMetricsSerialization:
    """Tests for AllocationMetrics to_dict/from_dict."""

    def test_roundtrip(self):
        now = datetime.now(timezone.utc)
        m = AllocationMetrics(
            allocation_id="a1",
            timestamp=now,
            cpu_utilization=80.0,
            gpu_utilization=95.0,
        )
        d = m.to_dict()
        restored = AllocationMetrics.from_dict(d)
        assert restored.allocation_id == "a1"
        assert restored.cpu_utilization == 80.0


class TestWatchEventSerialization:
    """Tests for WatchEvent to_dict/from_dict."""

    def test_roundtrip(self):
        alloc = Allocation(
            id="a1",
            entrypoint="echo",
            state=AllocationState.COMPLETED,
            requested_resources=ResourceRequest(cpus=1.0, memory_gb=1.0),
            tenant_id="t1",
            user_id="u1",
        )
        event = WatchEvent(event_type="completed", allocation=alloc)
        d = event.to_dict()
        restored = WatchEvent.from_dict(d)
        assert restored.event_type == "completed"
        assert restored.allocation.id == "a1"


class TestLogEntrySerialization:
    """Tests for LogEntry to_dict/from_dict."""

    def test_roundtrip(self):
        now = datetime.now(timezone.utc)
        entry = LogEntry(timestamp=now, level="INFO", message="hello", source="node-001")
        d = entry.to_dict()
        restored = LogEntry.from_dict(d)
        assert restored.level == "INFO"
        assert restored.source == "node-001"


class TestTenantSerialization:
    """Tests for Tenant to_dict/from_dict."""

    def test_roundtrip(self):
        t = Tenant(
            id="t1", name="team-ai", quota_cpus=1000.0, quota_memory_gb=10000.0, quota_gpus=50
        )
        d = t.to_dict()
        restored = Tenant.from_dict(d)
        assert restored.id == "t1"
        assert restored.quota_gpus == 50


class TestVClusterSerialization:
    """Tests for VCluster to_dict/from_dict."""

    def test_roundtrip(self):
        vc = VCluster(
            name="hpc",
            scheduler_type="hpc_backfill",
            resource_filter={"gpu": "h100"},
            scheduler_weights={"priority_class": 1.0},
        )
        d = vc.to_dict()
        restored = VCluster.from_dict(d)
        assert restored.name == "hpc"
        assert restored.scheduler_weights["priority_class"] == 1.0


# ── AllocationState Enum Tests ──


class TestAllocationState:
    """Tests for AllocationState enum."""

    def test_allocation_state_values(self):
        """Test that AllocationState enum has expected values."""
        assert AllocationState.PENDING == "pending"
        assert AllocationState.STAGING == "staging"
        assert AllocationState.RUNNING == "running"
        assert AllocationState.CHECKPOINTING == "checkpointing"
        assert AllocationState.SUSPENDED == "suspended"
        assert AllocationState.COMPLETED == "completed"
        assert AllocationState.FAILED == "failed"
        assert AllocationState.CANCELLED == "cancelled"

    def test_allocation_state_comparison(self):
        """Test that AllocationState values can be compared as strings."""
        state = AllocationState.RUNNING
        assert state == "running"
        assert state != "pending"

    def test_allocation_state_from_value(self):
        """Test creating AllocationState from string value."""
        state = AllocationState("completed")
        assert state == AllocationState.COMPLETED


# ── Error Class Tests ──


class TestErrors:
    """Tests for error classes."""

    def test_lattice_error(self):
        err = LatticeError("something failed", status_code=500)
        assert str(err) == "something failed"
        assert err.status_code == 500

    def test_not_found_error_is_lattice_error(self):
        err = LatticeNotFoundError("not found", status_code=404)
        assert isinstance(err, LatticeError)
        assert err.status_code == 404

    def test_auth_error_is_lattice_error(self):
        err = LatticeAuthError("unauthorized", status_code=401)
        assert isinstance(err, LatticeError)
        assert err.status_code == 401
