"""Suite 3: Quota and Fairness — multi-tenant scheduling behavior."""
from __future__ import annotations

import asyncio
import pytest

from lattice_sdk import AllocationSpec
from .conftest import submit_and_get_id


@pytest.mark.sequential
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_fair_share_across_tenants(client, cluster, run_id, request):
    """2 tenants with equal quota each get a fair share of scheduling."""
    tenants = ["ov-fair-a", "ov-fair-b"]
    alloc_ids: dict[str, list[str]] = {t: [] for t in tenants}

    # Submit 5 short allocations per tenant
    for tenant in tenants:
        for i in range(5):
            spec = AllocationSpec(
                tenant=tenant,
                entrypoint="sleep 2",
                walltime_hours=1.0 / 3600.0 * 10,  # 10s
            )
            aid = await submit_and_get_id(
                client, spec, run_id=run_id, test_name=request.node.name,
            )
            alloc_ids[tenant].append(aid)

    # Wait for scheduler to process — give enough time for dispatch + startup
    await asyncio.sleep(30)

    scheduled = {t: 0 for t in tenants}
    for tenant in tenants:
        for aid in alloc_ids[tenant]:
            try:
                status = await client.status(aid)
                state = status.state.value if hasattr(status.state, "value") else str(status.state)
                if state in ("running", "completed"):
                    scheduled[tenant] += 1
            except Exception:
                pass

    total = sum(scheduled.values())
    # Cleanup
    for tenant in tenants:
        for aid in alloc_ids[tenant]:
            try:
                await client.cancel(aid)
            except Exception:
                pass

    # At least some should be scheduled
    assert total >= 2, f"Only {total} allocations scheduled out of 10"

    # Each tenant should get at least 1
    for tenant in tenants:
        assert scheduled[tenant] >= 1, (
            f"Tenant {tenant} got {scheduled[tenant]} scheduled, expected at least 1. "
            f"Distribution: {scheduled}"
        )


@pytest.mark.sequential
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_hard_quota_prevents_overscheduling(client, cluster, run_id, request):
    """Tenant at quota limit has additional allocations stay pending."""
    # Submit allocations to fill available nodes
    alloc_ids = []
    nodes = await client.list_nodes()
    node_count = len(nodes)

    for i in range(node_count + 1):
        spec = AllocationSpec(
            tenant="lattice-ov",
            entrypoint="sleep 30",
            walltime_hours=0.01,
        )
        aid = await submit_and_get_id(
            client, spec, run_id=run_id, test_name=request.node.name,
        )
        alloc_ids.append(aid)

    await asyncio.sleep(5)

    # At least one should be pending (more allocations than nodes)
    states = []
    for aid in alloc_ids:
        try:
            status = await client.status(aid)
            state = status.state.value if hasattr(status.state, "value") else str(status.state)
            states.append(state)
        except Exception:
            states.append("unknown")

    # Cleanup
    for aid in alloc_ids:
        try:
            await client.cancel(aid)
        except Exception:
            pass

    pending_count = states.count("pending")
    running_count = states.count("running")

    assert running_count <= node_count, (
        f"More running ({running_count}) than nodes ({node_count})"
    )
    assert pending_count >= 1, (
        f"Expected at least 1 pending allocation, got states: {states}"
    )
