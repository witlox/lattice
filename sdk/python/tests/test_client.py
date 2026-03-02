"""
Unit tests for LatticeClient and types.

Tests cover client initialization, configuration, and stub method behavior.
Full integration tests will be added once protobuf stubs are generated.
"""

import pytest
from datetime import datetime

import sys
from pathlib import Path

# Add parent directory to path so we can import lattice_sdk
sys.path.insert(0, str(Path(__file__).parent.parent))

from lattice_sdk import (
    LatticeClient,
    Allocation,
    AllocationMetrics,
    AllocationState,
    LogEntry,
    Node,
    ResourceRequest,
    Tenant,
    VCluster,
    WatchEvent,
)


class TestLatticeClientInit:
    """Tests for LatticeClient initialization."""

    def test_client_default_init(self):
        """Test that LatticeClient can be constructed with default values."""
        client = LatticeClient()
        assert client.host == "localhost"
        assert client.port == 50051
        assert client.token is None
        assert client.connected is False

    def test_client_custom_init(self):
        """Test that LatticeClient can be constructed with custom host and port."""
        client = LatticeClient(host="lattice-api.example.com", port=9999, token="secret")
        assert client.host == "lattice-api.example.com"
        assert client.port == 9999
        assert client.token == "secret"
        assert client.connected is False

    def test_client_partial_init(self):
        """Test that LatticeClient can be constructed with mixed defaults and custom values."""
        client = LatticeClient(host="192.168.1.100")
        assert client.host == "192.168.1.100"
        assert client.port == 50051
        assert client.token is None


class TestLatticeClientStubMethods:
    """Tests for stub method NotImplementedError behavior."""

    @pytest.mark.asyncio
    async def test_submit_not_implemented(self):
        """Test that submit() raises NotImplementedError with helpful message."""
        client = LatticeClient()
        with pytest.raises(NotImplementedError) as exc_info:
            await client.submit(entrypoint="echo hello", cpus=1.0, memory_gb=4.0)
        assert "buf generate" in str(exc_info.value).lower()

    @pytest.mark.asyncio
    async def test_status_not_implemented(self):
        """Test that status() raises NotImplementedError."""
        client = LatticeClient()
        with pytest.raises(NotImplementedError) as exc_info:
            await client.status("alloc-123")
        assert "buf generate" in str(exc_info.value).lower()

    @pytest.mark.asyncio
    async def test_cancel_not_implemented(self):
        """Test that cancel() raises NotImplementedError."""
        client = LatticeClient()
        with pytest.raises(NotImplementedError) as exc_info:
            await client.cancel("alloc-123")
        assert "buf generate" in str(exc_info.value).lower()

    @pytest.mark.asyncio
    async def test_list_nodes_not_implemented(self):
        """Test that list_nodes() raises NotImplementedError."""
        client = LatticeClient()
        with pytest.raises(NotImplementedError) as exc_info:
            await client.list_nodes()
        assert "buf generate" in str(exc_info.value).lower()

    @pytest.mark.asyncio
    async def test_watch_not_implemented(self):
        """Test that watch() raises NotImplementedError."""
        client = LatticeClient()
        with pytest.raises(NotImplementedError) as exc_info:
            async for _ in client.watch("alloc-123"):
                pass
        assert "buf generate" in str(exc_info.value).lower()

    @pytest.mark.asyncio
    async def test_logs_not_implemented(self):
        """Test that logs() raises NotImplementedError."""
        client = LatticeClient()
        with pytest.raises(NotImplementedError) as exc_info:
            async for _ in client.logs("alloc-123"):
                pass
        assert "buf generate" in str(exc_info.value).lower()

    @pytest.mark.asyncio
    async def test_metrics_not_implemented(self):
        """Test that metrics() raises NotImplementedError."""
        client = LatticeClient()
        with pytest.raises(NotImplementedError) as exc_info:
            await client.metrics("alloc-123")
        assert "buf generate" in str(exc_info.value).lower()

    @pytest.mark.asyncio
    async def test_stream_metrics_not_implemented(self):
        """Test that stream_metrics() raises NotImplementedError."""
        client = LatticeClient()
        with pytest.raises(NotImplementedError) as exc_info:
            async for _ in client.stream_metrics("alloc-123"):
                pass
        assert "buf generate" in str(exc_info.value).lower()

    @pytest.mark.asyncio
    async def test_connect_not_implemented(self):
        """Test that connect() raises NotImplementedError."""
        client = LatticeClient()
        with pytest.raises(NotImplementedError) as exc_info:
            await client.connect()
        assert "protobuf stubs" in str(exc_info.value).lower()


class TestResourceRequest:
    """Tests for ResourceRequest type."""

    def test_resource_request_defaults(self):
        """Test that ResourceRequest can be instantiated with defaults."""
        req = ResourceRequest(cpus=2.0, memory_gb=8.0)
        assert req.cpus == 2.0
        assert req.memory_gb == 8.0
        assert req.gpus == 0
        assert req.gpu_memory_gb == 0.0
        assert req.storage_gb == 0.0
        assert req.metadata == {}

    def test_resource_request_full(self):
        """Test that ResourceRequest can be instantiated with all fields."""
        metadata = {"accelerator": "h100"}
        req = ResourceRequest(
            cpus=4.0,
            memory_gb=16.0,
            gpus=2,
            gpu_memory_gb=40.0,
            storage_gb=100.0,
            metadata=metadata,
        )
        assert req.cpus == 4.0
        assert req.memory_gb == 16.0
        assert req.gpus == 2
        assert req.gpu_memory_gb == 40.0
        assert req.storage_gb == 100.0
        assert req.metadata == metadata


class TestNode:
    """Tests for Node type."""

    def test_node_defaults(self):
        """Test that Node can be instantiated with minimal fields."""
        node = Node(
            name="node-001",
            host="192.168.1.1",
            status="available",
            total_cpus=16.0,
            total_memory_gb=64.0,
        )
        assert node.name == "node-001"
        assert node.host == "192.168.1.1"
        assert node.status == "available"
        assert node.total_cpus == 16.0
        assert node.total_memory_gb == 64.0
        assert node.total_gpus == 0
        assert node.available_cpus == 0.0
        assert node.available_memory_gb == 0.0
        assert node.available_gpus == 0
        assert node.last_heartbeat is None
        assert node.metadata == {}

    def test_node_full(self):
        """Test that Node can be instantiated with all fields."""
        now = datetime.now()
        metadata = {"gpu_type": "h100"}
        node = Node(
            name="node-002",
            host="192.168.1.2",
            status="allocated",
            total_cpus=32.0,
            total_memory_gb=128.0,
            total_gpus=4,
            available_cpus=8.0,
            available_memory_gb=32.0,
            available_gpus=1,
            last_heartbeat=now,
            metadata=metadata,
        )
        assert node.name == "node-002"
        assert node.host == "192.168.1.2"
        assert node.status == "allocated"
        assert node.total_gpus == 4
        assert node.available_cpus == 8.0
        assert node.last_heartbeat == now
        assert node.metadata == metadata


class TestAllocation:
    """Tests for Allocation type."""

    def test_allocation_defaults(self):
        """Test that Allocation can be instantiated with minimal fields."""
        req = ResourceRequest(cpus=1.0, memory_gb=4.0)
        alloc = Allocation(
            id="alloc-001",
            entrypoint="python train.py",
            state=AllocationState.PENDING,
            requested_resources=req,
            tenant_id="tenant-001",
            user_id="user-001",
        )
        assert alloc.id == "alloc-001"
        assert alloc.entrypoint == "python train.py"
        assert alloc.state == AllocationState.PENDING
        assert alloc.requested_resources == req
        assert alloc.tenant_id == "tenant-001"
        assert alloc.user_id == "user-001"
        assert alloc.priority_class == "normal"
        assert alloc.network_domain is None
        assert alloc.uenv is None
        assert alloc.nodes_allocated == []
        assert alloc.created_at is None
        assert alloc.labels == {}
        assert alloc.annotations == {}

    def test_allocation_full(self):
        """Test that Allocation can be instantiated with all fields."""
        now = datetime.now()
        req = ResourceRequest(cpus=8.0, memory_gb=32.0, gpus=1)
        labels = {"app": "training"}
        annotations = {"note": "experimental"}
        alloc = Allocation(
            id="alloc-002",
            entrypoint="python train.py --distributed",
            state=AllocationState.RUNNING,
            requested_resources=req,
            tenant_id="tenant-001",
            user_id="user-002",
            priority_class="high",
            network_domain="vni-100",
            uenv="pytorch-2.0",
            nodes_allocated=["node-001", "node-002"],
            created_at=now,
            started_at=now,
            labels=labels,
            annotations=annotations,
        )
        assert alloc.id == "alloc-002"
        assert alloc.state == AllocationState.RUNNING
        assert alloc.priority_class == "high"
        assert alloc.network_domain == "vni-100"
        assert alloc.uenv == "pytorch-2.0"
        assert alloc.nodes_allocated == ["node-001", "node-002"]
        assert alloc.created_at == now
        assert alloc.labels == labels


class TestAllocationMetrics:
    """Tests for AllocationMetrics type."""

    def test_allocation_metrics_defaults(self):
        """Test that AllocationMetrics can be instantiated with defaults."""
        now = datetime.now()
        metrics = AllocationMetrics(allocation_id="alloc-001", timestamp=now)
        assert metrics.allocation_id == "alloc-001"
        assert metrics.timestamp == now
        assert metrics.cpu_utilization == 0.0
        assert metrics.memory_utilization_gb == 0.0
        assert metrics.gpu_utilization == 0.0
        assert metrics.metadata == {}

    def test_allocation_metrics_full(self):
        """Test that AllocationMetrics can be instantiated with all fields."""
        now = datetime.now()
        metadata = {"source": "prometheus"}
        metrics = AllocationMetrics(
            allocation_id="alloc-001",
            timestamp=now,
            cpu_utilization=75.5,
            memory_utilization_gb=12.3,
            gpu_utilization=95.0,
            gpu_memory_utilization_gb=24.5,
            network_in_mbps=500.0,
            network_out_mbps=300.0,
            io_read_mbps=150.0,
            io_write_mbps=100.0,
            metadata=metadata,
        )
        assert metrics.cpu_utilization == 75.5
        assert metrics.gpu_utilization == 95.0
        assert metrics.network_in_mbps == 500.0
        assert metrics.metadata == metadata


class TestWatchEvent:
    """Tests for WatchEvent type."""

    def test_watch_event_defaults(self):
        """Test that WatchEvent can be instantiated with default timestamp."""
        req = ResourceRequest(cpus=1.0, memory_gb=4.0)
        alloc = Allocation(
            id="alloc-001",
            entrypoint="echo hello",
            state=AllocationState.COMPLETED,
            requested_resources=req,
            tenant_id="tenant-001",
            user_id="user-001",
        )
        event = WatchEvent(event_type="completed", allocation=alloc)
        assert event.event_type == "completed"
        assert event.allocation == alloc
        assert isinstance(event.timestamp, datetime)

    def test_watch_event_custom_timestamp(self):
        """Test that WatchEvent can be instantiated with custom timestamp."""
        now = datetime.now()
        req = ResourceRequest(cpus=1.0, memory_gb=4.0)
        alloc = Allocation(
            id="alloc-001",
            entrypoint="echo hello",
            state=AllocationState.RUNNING,
            requested_resources=req,
            tenant_id="tenant-001",
            user_id="user-001",
        )
        event = WatchEvent(event_type="updated", allocation=alloc, timestamp=now)
        assert event.timestamp == now


class TestLogEntry:
    """Tests for LogEntry type."""

    def test_log_entry_minimal(self):
        """Test that LogEntry can be instantiated with minimal fields."""
        now = datetime.now()
        entry = LogEntry(timestamp=now, level="INFO", message="Started training")
        assert entry.timestamp == now
        assert entry.level == "INFO"
        assert entry.message == "Started training"
        assert entry.source is None

    def test_log_entry_with_source(self):
        """Test that LogEntry can be instantiated with source."""
        now = datetime.now()
        entry = LogEntry(
            timestamp=now, level="ERROR", message="GPU out of memory", source="node-001"
        )
        assert entry.level == "ERROR"
        assert entry.source == "node-001"


class TestTenant:
    """Tests for Tenant type."""

    def test_tenant_defaults(self):
        """Test that Tenant can be instantiated with defaults."""
        tenant = Tenant(
            id="tenant-001",
            name="team-ai",
            quota_cpus=1000.0,
            quota_memory_gb=10000.0,
        )
        assert tenant.id == "tenant-001"
        assert tenant.name == "team-ai"
        assert tenant.quota_cpus == 1000.0
        assert tenant.quota_memory_gb == 10000.0
        assert tenant.quota_gpus == 0
        assert tenant.used_cpus == 0.0
        assert tenant.used_memory_gb == 0.0
        assert tenant.used_gpus == 0


class TestVCluster:
    """Tests for VCluster type."""

    def test_vcluster_defaults(self):
        """Test that VCluster can be instantiated with defaults."""
        vcluster = VCluster(
            name="hpc-backfill", scheduler_type="hpc_backfill"
        )
        assert vcluster.name == "hpc-backfill"
        assert vcluster.scheduler_type == "hpc_backfill"
        assert vcluster.resource_filter == {}
        assert vcluster.scheduler_weights == {}

    def test_vcluster_full(self):
        """Test that VCluster can be instantiated with all fields."""
        resource_filter = {"gpu_type": "h100"}
        scheduler_weights = {
            "priority_class": 1.0,
            "wait_time_factor": 0.5,
            "fair_share_deficit": 0.3,
        }
        vcluster = VCluster(
            name="service-binpack",
            scheduler_type="service_binpack",
            resource_filter=resource_filter,
            scheduler_weights=scheduler_weights,
        )
        assert vcluster.name == "service-binpack"
        assert vcluster.scheduler_type == "service_binpack"
        assert vcluster.resource_filter == resource_filter
        assert vcluster.scheduler_weights == scheduler_weights


class TestAllocationState:
    """Tests for AllocationState enum."""

    def test_allocation_state_values(self):
        """Test that AllocationState enum has expected values."""
        assert AllocationState.PENDING == "pending"
        assert AllocationState.RUNNING == "running"
        assert AllocationState.COMPLETED == "completed"
        assert AllocationState.FAILED == "failed"
        assert AllocationState.CANCELLED == "cancelled"
        assert AllocationState.CHECKPOINTED == "checkpointed"

    def test_allocation_state_comparison(self):
        """Test that AllocationState values can be compared as strings."""
        state = AllocationState.RUNNING
        assert state == "running"
        assert state != "pending"
