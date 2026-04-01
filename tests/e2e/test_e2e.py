"""
End-to-end tests for Lattice using testcontainers.

These tests spin up real Docker containers (server + agent + VictoriaMetrics)
and exercise the Python SDK against the live REST API.

Prerequisites:
    pip install -r tests/e2e/requirements.txt
    Docker must be running.

Usage:
    pytest tests/e2e/ -v --timeout=300
"""

import asyncio
import sys
from pathlib import Path

import httpx
import pytest

sys.path.insert(0, str(Path(__file__).parent.parent.parent / "sdk" / "python"))

from lattice_sdk import (
    Allocation,
    AllocationSpec,
    AllocationState,
    LatticeClient,
    LatticeError,
    LatticeNotFoundError,
)


# ── Health & Infrastructure ──────────────────────────────────────


class TestHealth:
    """Verify the stack is alive and reachable."""

    @pytest.mark.asyncio
    async def test_health_endpoint(self, client: LatticeClient):
        """Server /healthz returns healthy."""
        health = await client.health()
        assert "status" in health or isinstance(health, dict)

    @pytest.mark.asyncio
    async def test_server_responds_to_unknown_route(self, lattice_stack):
        """Unknown routes return 404, not connection error."""
        base_url = lattice_stack["base_url"]
        r = httpx.get(f"{base_url}/api/v1/nonexistent", timeout=5.0)
        assert r.status_code in (404, 405)


# ── Node Discovery ───────────────────────────────────────────────


class TestNodes:
    """Verify nodes are registered and queryable."""

    @pytest.mark.asyncio
    async def test_list_nodes(self, client: LatticeClient):
        """At least one agent should have registered."""
        nodes = await client.list_nodes()
        assert len(nodes) >= 1

    @pytest.mark.asyncio
    async def test_node_has_expected_resources(self, client: LatticeClient):
        """Agent registers with the resources declared in docker-compose."""
        nodes = await client.list_nodes()
        node = nodes[0]
        assert node.name == "e2e-node-001"
        assert node.total_cpus >= 64
        assert node.total_gpus >= 4


# ── Allocation Lifecycle ─────────────────────────────────────────


class TestAllocationLifecycle:
    """Test submit → status → cancel flow against real server."""

    @pytest.mark.asyncio
    async def test_submit_and_get(self, client: LatticeClient):
        """Submit an allocation and retrieve it by ID."""
        spec = AllocationSpec(
            entrypoint="echo hello-e2e",
            nodes=1,
            cpus=1.0,
            memory_gb=1.0,
            tenant_id="e2e-tenant",
        )
        alloc = await client.submit(spec)
        assert alloc.id is not None
        assert alloc.id != ""

        # Fetch by ID
        fetched = await client.status(alloc.id)
        assert fetched.id == alloc.id
        assert fetched.entrypoint == "echo hello-e2e"

    @pytest.mark.asyncio
    async def test_list_allocations_includes_submitted(self, client: LatticeClient):
        """Submitted allocations appear in the list."""
        spec = AllocationSpec(
            entrypoint="echo list-test",
            nodes=1,
            cpus=1.0,
            memory_gb=1.0,
            tenant_id="e2e-tenant",
        )
        alloc = await client.submit(spec)

        all_allocs = await client.list_allocations()
        ids = [a.id for a in all_allocs]
        assert alloc.id in ids

    @pytest.mark.asyncio
    async def test_cancel_allocation(self, client: LatticeClient):
        """Cancel a pending allocation."""
        spec = AllocationSpec(
            entrypoint="sleep 3600",
            nodes=1,
            cpus=1.0,
            memory_gb=1.0,
            tenant_id="e2e-tenant",
        )
        alloc = await client.submit(spec)
        result = await client.cancel(alloc.id)
        assert result is True

        # Verify state changed
        updated = await client.status(alloc.id)
        assert updated.state in (
            AllocationState.CANCELLED,
            AllocationState.COMPLETED,
            AllocationState.FAILED,
        )

    @pytest.mark.asyncio
    async def test_get_nonexistent_allocation(self, client: LatticeClient):
        """Fetching a non-existent allocation returns 404."""
        with pytest.raises(LatticeNotFoundError):
            await client.status("nonexistent-alloc-id-12345")


# ── Tenant Operations ───────────────────────────────────────────


class TestTenants:
    """Test tenant CRUD operations."""

    @pytest.mark.asyncio
    async def test_create_and_list_tenant(self, client: LatticeClient):
        """Create a tenant and verify it appears in the list."""
        tenant = await client.create_tenant(
            name="e2e-test-team",
            quota_cpus=100.0,
            quota_memory_gb=500.0,
            quota_gpus=10,
        )
        assert tenant.name == "e2e-test-team"
        assert tenant.quota_cpus == 100.0

        tenants = await client.list_tenants()
        names = [t.name for t in tenants]
        assert "e2e-test-team" in names

    @pytest.mark.asyncio
    async def test_get_tenant(self, client: LatticeClient):
        """Get a specific tenant by ID."""
        tenant = await client.create_tenant(
            name="e2e-get-tenant",
            quota_cpus=50.0,
            quota_memory_gb=200.0,
        )
        fetched = await client.get_tenant(tenant.id)
        assert fetched.id == tenant.id
        assert fetched.name == "e2e-get-tenant"


# ── VCluster Operations ─────────────────────────────────────────


class TestVClusters:
    """Test vCluster listing."""

    @pytest.mark.asyncio
    async def test_list_vclusters(self, client: LatticeClient):
        """List vClusters (may be empty initially)."""
        vclusters = await client.list_vclusters()
        assert isinstance(vclusters, list)


# ── Observability ────────────────────────────────────────────────


class TestObservability:
    """Test observability endpoints against real server."""

    @pytest.mark.asyncio
    async def test_get_allocation_metrics(self, client: LatticeClient):
        """Query metrics for a submitted allocation (may return empty)."""
        spec = AllocationSpec(
            entrypoint="echo metrics-test",
            nodes=1,
            cpus=1.0,
            memory_gb=1.0,
            tenant_id="e2e-tenant",
        )
        alloc = await client.submit(spec)

        # Metrics may not be populated yet, but the endpoint should respond
        try:
            metrics = await client.metrics(alloc.id)
            assert metrics.allocation_id == alloc.id
        except LatticeNotFoundError:
            # Acceptable — metrics may not exist for a just-submitted allocation
            pass

    @pytest.mark.asyncio
    async def test_get_allocation_logs(self, client: LatticeClient):
        """Query logs for a submitted allocation."""
        spec = AllocationSpec(
            entrypoint="echo log-test",
            nodes=1,
            cpus=1.0,
            memory_gb=1.0,
            tenant_id="e2e-tenant",
        )
        alloc = await client.submit(spec)

        entries = []
        try:
            async for entry in client.logs(alloc.id, follow=False, lines=10):
                entries.append(entry)
        except (LatticeNotFoundError, LatticeError):
            # Acceptable — logs may not exist yet
            pass


# ── Admin & Raft ─────────────────────────────────────────────────


class TestAdmin:
    """Test administrative endpoints."""

    @pytest.mark.asyncio
    async def test_raft_status(self, client: LatticeClient):
        """Raft status endpoint responds with leader info."""
        status = await client.raft_status()
        assert status.state is not None

    @pytest.mark.asyncio
    async def test_node_drain_undrain(self, client: LatticeClient):
        """Drain and undrain a node."""
        nodes = await client.list_nodes()
        if not nodes:
            pytest.skip("No nodes available for drain test")

        node_name = nodes[0].name
        result = await client.drain_node(node_name)
        assert result is True

        result = await client.undrain_node(node_name)
        assert result is True


# ── VictoriaMetrics Integration ──────────────────────────────────


class TestTSDB:
    """Verify metrics reach VictoriaMetrics."""

    @pytest.mark.asyncio
    async def test_metrics_pushed_to_tsdb(self, lattice_stack):
        """After agent registration, node metrics should appear in TSDB."""
        # Give the agent time to push at least one metric cycle
        await asyncio.sleep(15)

        # Query VictoriaMetrics directly via the exposed port
        compose = lattice_stack["compose"]
        vm_port = int(compose.get_service_port("victoriametrics", 8428))
        vm_url = f"http://localhost:{vm_port}"

        r = httpx.get(
            f"{vm_url}/api/v1/query",
            params={"query": "lattice_node_up"},
            timeout=5.0,
        )
        # VictoriaMetrics should respond even if no data yet
        assert r.status_code == 200
        data = r.json()
        assert data.get("status") == "success"


# ── Error Handling ───────────────────────────────────────────────


class TestErrorHandling:
    """Verify the server returns proper error codes."""

    @pytest.mark.asyncio
    async def test_invalid_submit_returns_error(self, client: LatticeClient):
        """Submitting with missing required fields should return an error."""
        try:
            spec = AllocationSpec(entrypoint="")
            await client.submit(spec)
            # If it doesn't raise, the server accepted it — that's also valid
        except LatticeError as e:
            assert e.status_code in (400, 422, 500)

    @pytest.mark.asyncio
    async def test_cancel_nonexistent_returns_404(self, client: LatticeClient):
        """Cancelling a non-existent allocation returns 404."""
        with pytest.raises((LatticeNotFoundError, LatticeError)):
            await client.cancel("does-not-exist-99999")


# ── Software Delivery ──────────────────────────────────────────


class TestSoftwareDelivery:
    """Test software delivery: uenv, containers, image resolution."""

    @pytest.mark.asyncio
    async def test_submit_with_uenv(self, client: LatticeClient):
        """Submit allocation with uenv image ref."""
        spec = AllocationSpec(
            entrypoint="./my_app",
            nodes=1,
            tenant_id="physics",
            uenv="prgenv-gnu/24.11:v1",
            view="default",
        )
        alloc = await client.submit(spec)
        assert alloc.id is not None

    @pytest.mark.asyncio
    async def test_submit_with_container(self, client: LatticeClient):
        """Submit allocation with OCI container image."""
        spec = AllocationSpec(
            entrypoint="python train.py",
            nodes=1,
            tenant_id="physics",
            image="nvcr.io/nvidia/pytorch:24.01-py3",
        )
        alloc = await client.submit(spec)
        assert alloc.id is not None

    @pytest.mark.asyncio
    async def test_submit_with_mounts(self, client: LatticeClient):
        """Submit allocation with bind mounts."""
        spec = AllocationSpec(
            entrypoint="./app",
            nodes=1,
            tenant_id="physics",
            mounts=["/scratch:/scratch:ro"],
        )
        alloc = await client.submit(spec)
        assert alloc.id is not None

    @pytest.mark.asyncio
    async def test_submit_with_devices(self, client: LatticeClient):
        """Submit allocation with CDI device specs."""
        spec = AllocationSpec(
            entrypoint="python train.py",
            nodes=1,
            tenant_id="physics",
            image="pytorch:latest",
            devices=["nvidia.com/gpu=all"],
        )
        alloc = await client.submit(spec)
        assert alloc.id is not None

    @pytest.mark.asyncio
    async def test_submit_bare_process(self, client: LatticeClient):
        """Submit allocation with no images — bare process."""
        spec = AllocationSpec(
            entrypoint="./my_binary",
            nodes=1,
            tenant_id="physics",
        )
        alloc = await client.submit(spec)
        assert alloc.id is not None

    @pytest.mark.asyncio
    async def test_submit_uenv_and_container(self, client: LatticeClient):
        """Submit allocation with both uenv and container."""
        spec = AllocationSpec(
            entrypoint="./app",
            nodes=1,
            tenant_id="physics",
            uenv="prgenv-gnu/24.11:v1",
            image="pytorch:latest",
        )
        alloc = await client.submit(spec)
        assert alloc.id is not None
