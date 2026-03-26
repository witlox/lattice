"""
Unit tests for LatticeClient (httpx-based) and types.

Tests cover client initialization, HTTP method behavior with mocked responses,
error handling, and type serialization roundtrips.
"""

import json
import re
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
    AccountingUsage,
    Allocation,
    AllocationMetrics,
    AllocationSpec,
    AllocationState,
    AuditEntry,
    BackupResult,
    Dag,
    DagSpec,
    DiagnosticsReport,
    LogEntry,
    Node,
    QueueInfo,
    RaftStatus,
    ResourceRequest,
    Session,
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
        "max_nodes": 50,
        "fair_share_target": 0.3,
        "gpu_hours_budget": 5000.0,
        "node_hours_budget": 10000.0,
        "max_concurrent_allocations": 10,
        "isolation_level": "standard",
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


def _sample_session_dict(session_id="session-001"):
    return {
        "id": session_id,
        "allocation_id": "alloc-001",
        "user_id": "user-001",
        "status": "active",
        "created_at": "2025-01-15T10:00:00+00:00",
    }


def _sample_dag_dict(dag_id="dag-001"):
    return {
        "id": dag_id,
        "name": "training-pipeline",
        "state": "pending",
        "allocations": ["alloc-001", "alloc-002"],
        "edges": [{"from": "alloc-001", "to": "alloc-002", "type": "afterok"}],
        "tenant_id": "tenant-001",
        "user_id": "user-001",
    }


def _sample_audit_entry_dict(entry_id="audit-001"):
    return {
        "id": entry_id,
        "timestamp": "2025-01-15T10:00:00+00:00",
        "action": "node_claim",
        "user_id": "user-001",
        "resource_type": "node",
        "resource_id": "node-001",
        "details": {"reason": "sensitive workload"},
    }


def _sample_accounting_usage_dict():
    return {
        "tenant_id": "tenant-001",
        "period_start": "2025-01-01T00:00:00+00:00",
        "period_end": "2025-01-31T23:59:59+00:00",
        "cpu_hours": 5000.0,
        "gpu_hours": 1200.0,
        "memory_gb_hours": 80000.0,
        "storage_gb_hours": 10000.0,
        "total_allocations": 150,
    }


def _sample_raft_status_dict():
    return {
        "node_id": "node-1",
        "role": "leader",
        "term": 5,
        "leader_id": "node-1",
        "members": ["node-1", "node-2", "node-3"],
        "commit_index": 42,
        "last_applied": 42,
    }


def _sample_backup_result_dict():
    return {
        "success": True,
        "message": "Backup created successfully",
        "backup_id": "backup-001",
        "timestamp": "2025-01-15T10:30:00+00:00",
    }


def _sample_diagnostics_dict(alloc_id="alloc-001"):
    return {
        "allocation_id": alloc_id,
        "state": "running",
        "node_assignments": ["node-001", "node-002"],
        "scheduler_decisions": [{"step": "placement", "result": "success"}],
        "resource_utilization": {"cpu": 0.75, "gpu": 0.95},
        "warnings": ["High GPU memory usage"],
    }


def _sample_queue_info_dict(name="hpc-backfill"):
    return {
        "vcluster_name": name,
        "pending_count": 5,
        "running_count": 10,
        "allocations": [
            {"id": "alloc-001", "state": "running"},
            {"id": "alloc-002", "state": "pending"},
        ],
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
            url="http://localhost:8080/api/v1/allocations",
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
            url="http://localhost:8080/api/v1/allocations",
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
            url="http://localhost:8080/api/v1/allocations",
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
            url="http://localhost:8080/api/v1/allocations/alloc-123",
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
            url="http://localhost:8080/api/v1/allocations/nonexistent",
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
            url="http://localhost:8080/api/v1/allocations/alloc-123",
            method="GET",
            status_code=403,
            text="Forbidden",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeAuthError):
                await client.status("alloc-123")


# ── Patch Allocation Tests ──


class TestPatchAllocation:
    """Tests for patch_allocation() method."""

    @pytest.mark.asyncio
    async def test_patch_allocation_success(self, httpx_mock):
        """Test successful allocation patch."""
        alloc_dict = _sample_allocation_dict("alloc-123", "running")
        alloc_dict["priority_class"] = "high"
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/allocations/alloc-123",
            method="PATCH",
            json=alloc_dict,
        )
        async with LatticeClient() as client:
            alloc = await client.patch_allocation("alloc-123", {"priority_class": "high"})
            assert alloc.priority_class == "high"

    @pytest.mark.asyncio
    async def test_patch_allocation_not_found(self, httpx_mock):
        """Test patching non-existent allocation."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/allocations/nonexistent",
            method="PATCH",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.patch_allocation("nonexistent", {"priority_class": "high"})


# ── Cancel Tests ──


class TestCancel:
    """Tests for cancel() method."""

    @pytest.mark.asyncio
    async def test_cancel_success(self, httpx_mock):
        """Test successful cancellation."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/allocations/alloc-123/cancel",
            method="POST",
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
            url="http://localhost:8080/api/v1/allocations/nonexistent/cancel",
            method="POST",
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
            url=re.compile(r".*/api/v1/allocations(\?.*)?$"),
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
            url=re.compile(r".*/api/v1/allocations(\?.*)?$"),
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
            url=re.compile(r".*/api/v1/allocations(\?.*)?$"),
            method="GET",
            json=[],
        )
        async with LatticeClient() as client:
            result = await client.list_allocations()
            assert len(result) == 0


# ── Diagnostics Tests ──


class TestDiagnostics:
    """Tests for get_diagnostics() method."""

    @pytest.mark.asyncio
    async def test_get_diagnostics_success(self, httpx_mock):
        """Test successful diagnostics retrieval."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/allocations/alloc-001/diagnostics",
            method="GET",
            json=_sample_diagnostics_dict(),
        )
        async with LatticeClient() as client:
            diag = await client.get_diagnostics("alloc-001")
            assert diag.allocation_id == "alloc-001"
            assert diag.state == "running"
            assert len(diag.node_assignments) == 2
            assert len(diag.warnings) == 1

    @pytest.mark.asyncio
    async def test_get_diagnostics_not_found(self, httpx_mock):
        """Test diagnostics for non-existent allocation."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/allocations/nonexistent/diagnostics",
            method="GET",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.get_diagnostics("nonexistent")


# ── Node Tests ──


class TestNodes:
    """Tests for node-related methods."""

    @pytest.mark.asyncio
    async def test_list_nodes(self, httpx_mock):
        """Test listing nodes."""
        nodes = [_sample_node_dict("n1"), _sample_node_dict("n2")]
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/nodes",
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
            url="http://localhost:8080/api/v1/nodes/node-001",
            method="GET",
            json=_sample_node_dict(),
        )
        async with LatticeClient() as client:
            node = await client.get_node("node-001")
            assert node.name == "node-001"
            assert node.total_gpus == 4

    @pytest.mark.asyncio
    async def test_drain_node_success(self, httpx_mock):
        """Test successful node drain."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/nodes/node-001/drain",
            method="POST",
            status_code=200,
            json={"status": "draining"},
        )
        async with LatticeClient() as client:
            result = await client.drain_node("node-001")
            assert result is True

    @pytest.mark.asyncio
    async def test_drain_node_not_found(self, httpx_mock):
        """Test draining non-existent node."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/nodes/nonexistent/drain",
            method="POST",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.drain_node("nonexistent")

    @pytest.mark.asyncio
    async def test_undrain_node_success(self, httpx_mock):
        """Test successful node undrain."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/nodes/node-001/undrain",
            method="POST",
            status_code=200,
            json={"status": "available"},
        )
        async with LatticeClient() as client:
            result = await client.undrain_node("node-001")
            assert result is True

    @pytest.mark.asyncio
    async def test_undrain_node_not_found(self, httpx_mock):
        """Test undraining non-existent node."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/nodes/nonexistent/undrain",
            method="POST",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.undrain_node("nonexistent")


# ── Metrics Tests ──


class TestMetrics:
    """Tests for metrics methods."""

    @pytest.mark.asyncio
    async def test_metrics_success(self, httpx_mock):
        """Test getting allocation metrics."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/allocations/alloc-001/metrics",
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
            url=re.compile(r".*/api/v1/allocations/alloc-001/logs(\?.*)?$"),
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


# ── Session Tests ──


class TestSessions:
    """Tests for session methods."""

    @pytest.mark.asyncio
    async def test_create_session_success(self, httpx_mock):
        """Test successful session creation."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/sessions",
            method="POST",
            json=_sample_session_dict(),
            status_code=201,
        )
        async with LatticeClient() as client:
            session = await client.create_session("alloc-001", user_id="user-001")
            assert session.id == "session-001"
            assert session.allocation_id == "alloc-001"
            assert session.status == "active"

    @pytest.mark.asyncio
    async def test_create_session_without_user(self, httpx_mock):
        """Test session creation without explicit user_id."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/sessions",
            method="POST",
            json=_sample_session_dict(),
            status_code=201,
        )
        async with LatticeClient() as client:
            session = await client.create_session("alloc-001")
            assert session.id == "session-001"

    @pytest.mark.asyncio
    async def test_get_session_success(self, httpx_mock):
        """Test successful session retrieval."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/sessions/session-001",
            method="GET",
            json=_sample_session_dict(),
        )
        async with LatticeClient() as client:
            session = await client.get_session("session-001")
            assert session.id == "session-001"
            assert session.user_id == "user-001"

    @pytest.mark.asyncio
    async def test_get_session_not_found(self, httpx_mock):
        """Test session retrieval for non-existent session."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/sessions/nonexistent",
            method="GET",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.get_session("nonexistent")

    @pytest.mark.asyncio
    async def test_delete_session_success(self, httpx_mock):
        """Test successful session deletion."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/sessions/session-001",
            method="DELETE",
            status_code=200,
            json={"status": "deleted"},
        )
        async with LatticeClient() as client:
            result = await client.delete_session("session-001")
            assert result is True

    @pytest.mark.asyncio
    async def test_delete_session_not_found(self, httpx_mock):
        """Test deleting non-existent session."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/sessions/nonexistent",
            method="DELETE",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.delete_session("nonexistent")


# ── DAG Tests ──


class TestDags:
    """Tests for DAG methods."""

    @pytest.mark.asyncio
    async def test_submit_dag_success(self, httpx_mock):
        """Test successful DAG submission."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/dags",
            method="POST",
            json=_sample_dag_dict(),
            status_code=201,
        )
        async with LatticeClient() as client:
            spec = DagSpec(
                name="training-pipeline",
                allocations=[{"entrypoint": "echo step1"}, {"entrypoint": "echo step2"}],
                edges=[{"from": "alloc-001", "to": "alloc-002", "type": "afterok"}],
                tenant_id="tenant-001",
            )
            dag = await client.submit_dag(spec)
            assert dag.id == "dag-001"
            assert dag.name == "training-pipeline"
            assert dag.state == "pending"

    @pytest.mark.asyncio
    async def test_submit_dag_error(self, httpx_mock):
        """Test DAG submission with server error."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/dags",
            method="POST",
            status_code=400,
            text="Invalid DAG: cycle detected",
        )
        async with LatticeClient() as client:
            spec = DagSpec(name="bad-dag", allocations=[])
            with pytest.raises(LatticeError) as exc_info:
                await client.submit_dag(spec)
            assert exc_info.value.status_code == 400

    @pytest.mark.asyncio
    async def test_list_dags_success(self, httpx_mock):
        """Test listing DAGs."""
        httpx_mock.add_response(
            url=re.compile(r".*/api/v1/dags(\?.*)?$"),
            method="GET",
            json=[_sample_dag_dict("dag-001"), _sample_dag_dict("dag-002")],
        )
        async with LatticeClient() as client:
            dags = await client.list_dags()
            assert len(dags) == 2
            assert dags[0].id == "dag-001"

    @pytest.mark.asyncio
    async def test_list_dags_empty(self, httpx_mock):
        """Test listing DAGs with empty result."""
        httpx_mock.add_response(
            url=re.compile(r".*/api/v1/dags(\?.*)?$"),
            method="GET",
            json=[],
        )
        async with LatticeClient() as client:
            dags = await client.list_dags()
            assert len(dags) == 0

    @pytest.mark.asyncio
    async def test_get_dag_success(self, httpx_mock):
        """Test getting a specific DAG."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/dags/dag-001",
            method="GET",
            json=_sample_dag_dict(),
        )
        async with LatticeClient() as client:
            dag = await client.get_dag("dag-001")
            assert dag.id == "dag-001"
            assert len(dag.allocations) == 2

    @pytest.mark.asyncio
    async def test_get_dag_not_found(self, httpx_mock):
        """Test getting non-existent DAG."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/dags/nonexistent",
            method="GET",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.get_dag("nonexistent")

    @pytest.mark.asyncio
    async def test_cancel_dag_success(self, httpx_mock):
        """Test successful DAG cancellation."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/dags/dag-001/cancel",
            method="POST",
            status_code=200,
            json={"status": "cancelled"},
        )
        async with LatticeClient() as client:
            result = await client.cancel_dag("dag-001")
            assert result is True

    @pytest.mark.asyncio
    async def test_cancel_dag_not_found(self, httpx_mock):
        """Test cancelling non-existent DAG."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/dags/nonexistent/cancel",
            method="POST",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.cancel_dag("nonexistent")


# ── Audit Tests ──


class TestAudit:
    """Tests for audit query methods."""

    @pytest.mark.asyncio
    async def test_query_audit_success(self, httpx_mock):
        """Test successful audit query."""
        httpx_mock.add_response(
            url=re.compile(r".*/api/v1/audit(\?.*)?$"),
            method="GET",
            json=[_sample_audit_entry_dict()],
        )
        async with LatticeClient() as client:
            entries = await client.query_audit()
            assert len(entries) == 1
            assert entries[0].id == "audit-001"
            assert entries[0].action == "node_claim"

    @pytest.mark.asyncio
    async def test_query_audit_with_filters(self, httpx_mock):
        """Test audit query with filters."""
        httpx_mock.add_response(
            url=re.compile(r".*/api/v1/audit(\?.*)?$"),
            method="GET",
            json=[_sample_audit_entry_dict()],
        )
        async with LatticeClient() as client:
            entries = await client.query_audit(
                tenant_id="tenant-001", user_id="user-001", action="node_claim"
            )
            assert len(entries) == 1

    @pytest.mark.asyncio
    async def test_query_audit_empty(self, httpx_mock):
        """Test audit query with no results."""
        httpx_mock.add_response(
            url=re.compile(r".*/api/v1/audit(\?.*)?$"),
            method="GET",
            json=[],
        )
        async with LatticeClient() as client:
            entries = await client.query_audit()
            assert len(entries) == 0


# ── Tenant Tests ──


class TestTenants:
    """Tests for tenant methods."""

    @pytest.mark.asyncio
    async def test_create_tenant_success(self, httpx_mock):
        """Test successful tenant creation."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/tenants",
            method="POST",
            json=_sample_tenant_dict(),
            status_code=201,
        )
        async with LatticeClient() as client:
            tenant = await client.create_tenant(
                name="team-ai",
                max_nodes=50,
                fair_share_target=0.3,
                gpu_hours_budget=5000.0,
                node_hours_budget=10000.0,
            )
            assert tenant.name == "team-ai"
            assert tenant.max_nodes == 50

    @pytest.mark.asyncio
    async def test_list_tenants(self, httpx_mock):
        """Test listing tenants."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/tenants",
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
            url="http://localhost:8080/api/v1/tenants/tenant-001",
            method="GET",
            json=_sample_tenant_dict(),
        )
        async with LatticeClient() as client:
            tenant = await client.get_tenant("tenant-001")
            assert tenant.id == "tenant-001"
            assert tenant.gpu_hours_budget == 5000.0

    @pytest.mark.asyncio
    async def test_update_tenant_success(self, httpx_mock):
        """Test successful tenant update."""
        updated = _sample_tenant_dict()
        updated["max_nodes"] = 100
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/tenants/tenant-001",
            method="PUT",
            json=updated,
        )
        async with LatticeClient() as client:
            tenant = await client.update_tenant("tenant-001", {"max_nodes": 100})
            assert tenant.max_nodes == 100

    @pytest.mark.asyncio
    async def test_update_tenant_not_found(self, httpx_mock):
        """Test updating non-existent tenant."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/tenants/nonexistent",
            method="PUT",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.update_tenant("nonexistent", {"name": "new"})


# ── VCluster Tests ──


class TestVClusters:
    """Tests for vCluster methods."""

    @pytest.mark.asyncio
    async def test_create_vcluster_success(self, httpx_mock):
        """Test successful vCluster creation."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/vclusters",
            method="POST",
            json=_sample_vcluster_dict(),
            status_code=201,
        )
        async with LatticeClient() as client:
            vc = await client.create_vcluster(
                name="hpc-backfill",
                scheduler_type="hpc_backfill",
                resource_filter={"gpu_type": "h100"},
            )
            assert vc.name == "hpc-backfill"
            assert vc.scheduler_type == "hpc_backfill"

    @pytest.mark.asyncio
    async def test_list_vclusters(self, httpx_mock):
        """Test listing vClusters."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/vclusters",
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
            url="http://localhost:8080/api/v1/vclusters/hpc-backfill",
            method="GET",
            json=_sample_vcluster_dict(),
        )
        async with LatticeClient() as client:
            vc = await client.get_vcluster("hpc-backfill")
            assert vc.scheduler_type == "hpc_backfill"

    @pytest.mark.asyncio
    async def test_vcluster_queue_success(self, httpx_mock):
        """Test getting vCluster queue info."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/vclusters/hpc-backfill/queue",
            method="GET",
            json=_sample_queue_info_dict(),
        )
        async with LatticeClient() as client:
            queue = await client.vcluster_queue("hpc-backfill")
            assert queue.vcluster_name == "hpc-backfill"
            assert queue.pending_count == 5
            assert queue.running_count == 10
            assert len(queue.allocations) == 2

    @pytest.mark.asyncio
    async def test_vcluster_queue_not_found(self, httpx_mock):
        """Test queue info for non-existent vCluster."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/vclusters/nonexistent/queue",
            method="GET",
            status_code=404,
            text="Not Found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.vcluster_queue("nonexistent")


# ── Accounting Tests ──


class TestAccounting:
    """Tests for accounting usage methods."""

    @pytest.mark.asyncio
    async def test_accounting_usage_success(self, httpx_mock):
        """Test successful accounting usage query."""
        httpx_mock.add_response(
            url=re.compile(r".*/api/v1/accounting/usage(\?.*)?$"),
            method="GET",
            json=_sample_accounting_usage_dict(),
        )
        async with LatticeClient() as client:
            usage = await client.accounting_usage(tenant_id="tenant-001")
            assert usage.tenant_id == "tenant-001"
            assert usage.cpu_hours == 5000.0
            assert usage.gpu_hours == 1200.0
            assert usage.total_allocations == 150

    @pytest.mark.asyncio
    async def test_accounting_usage_with_period(self, httpx_mock):
        """Test accounting usage with date range."""
        httpx_mock.add_response(
            url=re.compile(r".*/api/v1/accounting/usage(\?.*)?$"),
            method="GET",
            json=_sample_accounting_usage_dict(),
        )
        async with LatticeClient() as client:
            usage = await client.accounting_usage(
                tenant_id="tenant-001",
                start="2025-01-01T00:00:00Z",
                end="2025-01-31T23:59:59Z",
            )
            assert usage.tenant_id == "tenant-001"


# ── Budget Usage Tests ──


class TestBudgetUsage:
    """Tests for budget usage methods."""

    @pytest.mark.asyncio
    async def test_tenant_usage_success(self, httpx_mock):
        """Test successful tenant usage query."""
        httpx_mock.add_response(
            url=re.compile(r".*/api/v1/tenants/physics/usage(\?.*)?$"),
            method="GET",
            json={
                "tenant": "physics",
                "gpu_hours_used": 450.0,
                "gpu_hours_budget": 1000.0,
                "gpu_fraction_used": 0.45,
                "node_hours_used": 800.0,
                "node_hours_budget": 5000.0,
                "node_fraction_used": 0.16,
                "period_start": "2025-10-01T00:00:00Z",
                "period_end": "2025-12-30T00:00:00Z",
                "period_days": 90,
            },
        )
        async with LatticeClient() as client:
            usage = await client.tenant_usage("physics", days=90)
            assert usage.tenant == "physics"
            assert usage.gpu_hours_used == 450.0
            assert usage.gpu_hours_budget == 1000.0
            assert usage.gpu_fraction_used == 0.45
            assert usage.node_hours_used == 800.0
            assert usage.node_hours_budget == 5000.0
            assert usage.period_days == 90

    @pytest.mark.asyncio
    async def test_tenant_usage_not_found(self, httpx_mock):
        """Test tenant usage for non-existent tenant."""
        httpx_mock.add_response(
            url=re.compile(r".*/api/v1/tenants/nonexistent/usage(\?.*)?$"),
            method="GET",
            status_code=404,
            text="tenant 'nonexistent' not found",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeNotFoundError):
                await client.tenant_usage("nonexistent")

    @pytest.mark.asyncio
    async def test_user_usage_success(self, httpx_mock):
        """Test successful user usage query."""
        httpx_mock.add_response(
            url=re.compile(r".*/api/v1/usage(\?.*)?$"),
            method="GET",
            json={
                "user": "alice",
                "tenants": [
                    {
                        "tenant": "physics",
                        "gpu_hours_used": 200.0,
                        "gpu_hours_budget": 1000.0,
                        "node_hours_used": 400.0,
                        "node_hours_budget": 5000.0,
                    },
                    {
                        "tenant": "ml-team",
                        "gpu_hours_used": 50.0,
                        "gpu_hours_budget": None,
                        "node_hours_used": 100.0,
                        "node_hours_budget": None,
                    },
                ],
                "total_gpu_hours": 250.0,
                "period_start": "2025-10-01T00:00:00Z",
                "period_end": "2025-12-30T00:00:00Z",
            },
        )
        async with LatticeClient() as client:
            usage = await client.user_usage("alice", days=90)
            assert usage.user == "alice"
            assert len(usage.tenants) == 2
            assert usage.total_gpu_hours == 250.0
            assert usage.tenants[0].tenant == "physics"
            assert usage.tenants[0].gpu_hours_budget == 1000.0
            assert usage.tenants[1].gpu_hours_budget is None


# ── Node Enable/Disable Tests ──


class TestNodeEnableDisable:
    """Tests for node enable/disable methods."""

    @pytest.mark.asyncio
    async def test_disable_node_success(self, httpx_mock):
        """Test successful node disable."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/nodes/node-001/disable",
            method="POST",
            json={"status": "ok"},
        )
        async with LatticeClient() as client:
            result = await client.disable_node("node-001", reason="hardware issue")
            assert result is True

    @pytest.mark.asyncio
    async def test_enable_node_success(self, httpx_mock):
        """Test successful node enable."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/nodes/node-001/enable",
            method="POST",
            json={"status": "ok"},
        )
        async with LatticeClient() as client:
            result = await client.enable_node("node-001")
            assert result is True


# ── Raft Status Tests ──


class TestRaftStatus:
    """Tests for Raft status methods."""

    @pytest.mark.asyncio
    async def test_raft_status_success(self, httpx_mock):
        """Test successful Raft status query."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/raft/status",
            method="GET",
            json=_sample_raft_status_dict(),
        )
        async with LatticeClient() as client:
            status = await client.raft_status()
            assert status.node_id == "node-1"
            assert status.role == "leader"
            assert status.term == 5
            assert len(status.members) == 3
            assert status.commit_index == 42


# ── Admin Backup Tests ──


class TestAdminBackup:
    """Tests for admin backup operations."""

    @pytest.mark.asyncio
    async def test_create_backup_success(self, httpx_mock):
        """Test successful backup creation."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/admin/backup",
            method="POST",
            json=_sample_backup_result_dict(),
        )
        async with LatticeClient() as client:
            result = await client.create_backup()
            assert result.success is True
            assert result.backup_id == "backup-001"

    @pytest.mark.asyncio
    async def test_verify_backup_success(self, httpx_mock):
        """Test successful backup verification."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/admin/backup/verify",
            method="POST",
            json={"success": True, "message": "Backup verified"},
        )
        async with LatticeClient() as client:
            result = await client.verify_backup()
            assert result.success is True

    @pytest.mark.asyncio
    async def test_restore_backup_success(self, httpx_mock):
        """Test successful backup restore."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/admin/backup/restore",
            method="POST",
            json={"success": True, "message": "Restore complete"},
        )
        async with LatticeClient() as client:
            result = await client.restore_backup()
            assert result.success is True

    @pytest.mark.asyncio
    async def test_create_backup_server_error(self, httpx_mock):
        """Test backup creation with server error."""
        httpx_mock.add_response(
            url="http://localhost:8080/api/v1/admin/backup",
            method="POST",
            status_code=500,
            text="Internal Server Error",
        )
        async with LatticeClient() as client:
            with pytest.raises(LatticeError) as exc_info:
                await client.create_backup()
            assert exc_info.value.status_code == 500


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
            id="t1", name="team-ai", max_nodes=50, fair_share_target=0.3,
            gpu_hours_budget=5000.0, node_hours_budget=10000.0,
        )
        d = t.to_dict()
        restored = Tenant.from_dict(d)
        assert restored.id == "t1"
        assert restored.max_nodes == 50
        assert restored.gpu_hours_budget == 5000.0
        assert restored.node_hours_budget == 10000.0


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


class TestSessionSerialization:
    """Tests for Session to_dict/from_dict."""

    def test_roundtrip(self):
        now = datetime.now(timezone.utc)
        s = Session(
            id="s1",
            allocation_id="a1",
            user_id="u1",
            created_at=now,
            status="active",
        )
        d = s.to_dict()
        restored = Session.from_dict(d)
        assert restored.id == "s1"
        assert restored.allocation_id == "a1"
        assert restored.status == "active"

    def test_from_dict_defaults(self):
        data = {"id": "s1", "allocation_id": "a1", "user_id": "u1"}
        s = Session.from_dict(data)
        assert s.status == "active"
        assert s.metadata == {}


class TestDagSerialization:
    """Tests for Dag to_dict/from_dict."""

    def test_roundtrip(self):
        dag = Dag(
            id="d1",
            name="pipeline",
            state="running",
            allocations=["a1", "a2"],
            edges=[{"from": "a1", "to": "a2", "type": "afterok"}],
            tenant_id="t1",
            user_id="u1",
        )
        d = dag.to_dict()
        restored = Dag.from_dict(d)
        assert restored.id == "d1"
        assert restored.name == "pipeline"
        assert len(restored.allocations) == 2
        assert len(restored.edges) == 1


class TestDagSpecSerialization:
    """Tests for DagSpec to_dict/from_dict."""

    def test_roundtrip(self):
        spec = DagSpec(
            name="pipeline",
            allocations=[{"entrypoint": "step1"}, {"entrypoint": "step2"}],
            edges=[{"from": "step1", "to": "step2", "type": "afterok"}],
            tenant_id="t1",
        )
        d = spec.to_dict()
        restored = DagSpec.from_dict(d)
        assert restored.name == "pipeline"
        assert len(restored.allocations) == 2
        assert restored.tenant_id == "t1"


class TestAuditEntrySerialization:
    """Tests for AuditEntry to_dict/from_dict."""

    def test_roundtrip(self):
        now = datetime.now(timezone.utc)
        entry = AuditEntry(
            id="ae1",
            timestamp=now,
            action="node_claim",
            user_id="u1",
            resource_type="node",
            resource_id="n1",
            details={"reason": "sensitive"},
        )
        d = entry.to_dict()
        restored = AuditEntry.from_dict(d)
        assert restored.id == "ae1"
        assert restored.action == "node_claim"
        assert restored.details["reason"] == "sensitive"


class TestAccountingUsageSerialization:
    """Tests for AccountingUsage to_dict/from_dict."""

    def test_roundtrip(self):
        usage = AccountingUsage(
            tenant_id="t1",
            cpu_hours=5000.0,
            gpu_hours=1200.0,
            total_allocations=150,
        )
        d = usage.to_dict()
        restored = AccountingUsage.from_dict(d)
        assert restored.tenant_id == "t1"
        assert restored.cpu_hours == 5000.0
        assert restored.total_allocations == 150

    def test_from_dict_defaults(self):
        data = {"tenant_id": "t1"}
        usage = AccountingUsage.from_dict(data)
        assert usage.cpu_hours == 0.0
        assert usage.gpu_hours == 0.0
        assert usage.total_allocations == 0


class TestRaftStatusSerialization:
    """Tests for RaftStatus to_dict/from_dict."""

    def test_roundtrip(self):
        status = RaftStatus(
            node_id="n1",
            role="leader",
            term=5,
            leader_id="n1",
            members=["n1", "n2", "n3"],
            commit_index=42,
            last_applied=42,
        )
        d = status.to_dict()
        restored = RaftStatus.from_dict(d)
        assert restored.node_id == "n1"
        assert restored.role == "leader"
        assert restored.term == 5
        assert len(restored.members) == 3


class TestBackupResultSerialization:
    """Tests for BackupResult to_dict/from_dict."""

    def test_roundtrip(self):
        now = datetime.now(timezone.utc)
        result = BackupResult(
            success=True,
            message="Backup created",
            backup_id="b1",
            timestamp=now,
        )
        d = result.to_dict()
        restored = BackupResult.from_dict(d)
        assert restored.success is True
        assert restored.backup_id == "b1"

    def test_from_dict_defaults(self):
        data = {}
        result = BackupResult.from_dict(data)
        assert result.success is True
        assert result.message == ""
        assert result.backup_id is None


class TestDiagnosticsReportSerialization:
    """Tests for DiagnosticsReport to_dict/from_dict."""

    def test_roundtrip(self):
        diag = DiagnosticsReport(
            allocation_id="a1",
            state="running",
            node_assignments=["n1", "n2"],
            warnings=["High GPU usage"],
        )
        d = diag.to_dict()
        restored = DiagnosticsReport.from_dict(d)
        assert restored.allocation_id == "a1"
        assert len(restored.node_assignments) == 2
        assert restored.warnings[0] == "High GPU usage"


class TestQueueInfoSerialization:
    """Tests for QueueInfo to_dict/from_dict."""

    def test_roundtrip(self):
        qi = QueueInfo(
            vcluster_name="hpc",
            pending_count=5,
            running_count=10,
        )
        d = qi.to_dict()
        restored = QueueInfo.from_dict(d)
        assert restored.vcluster_name == "hpc"
        assert restored.pending_count == 5
        assert restored.running_count == 10


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
