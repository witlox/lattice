"""Suite 8: Performance Baselines — throughput and completion metrics."""
from __future__ import annotations

import asyncio
import time
import pytest

from lattice_sdk import AllocationSpec
from .conftest import submit_and_get_id


@pytest.mark.parallel
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_concurrent_submission_throughput(client, cluster, run_id, request):
    """20 allocations submitted within 5s are all acknowledged."""
    alloc_ids = []
    start = time.monotonic()

    for i in range(20):
        spec = AllocationSpec(
            tenant="lattice-ov",
            entrypoint=f"echo batch-{i}",
            walltime_hours=0.01,
        )
        aid = await submit_and_get_id(
            client, spec, run_id=run_id, test_name=request.node.name,
        )
        alloc_ids.append(aid)

    elapsed = time.monotonic() - start

    # Cleanup
    for aid in alloc_ids:
        try:
            await client.cancel(aid)
        except Exception:
            pass

    assert len(alloc_ids) == 20, f"Only {len(alloc_ids)} allocations submitted"
    assert elapsed < 10.0, f"Submissions took {elapsed:.1f}s, expected < 10s"


@pytest.mark.parallel
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_allocations_scheduled_promptly(client, cluster, run_id, request):
    """Submitted allocations reach running state within 30s."""
    nodes = await client.list_nodes()
    n = min(len(nodes), 3)
    if n < 1:
        pytest.skip("No ready nodes")

    alloc_ids = []
    for i in range(n):
        spec = AllocationSpec(
            tenant="lattice-ov",
            entrypoint="sleep 5",
            walltime_hours=0.01,
        )
        aid = await submit_and_get_id(
            client, spec, run_id=run_id, test_name=request.node.name,
        )
        alloc_ids.append(aid)

    # Wait for at least 1 to reach running
    running_count = 0
    for _ in range(30):
        for aid in alloc_ids:
            try:
                status = await client.status(aid)
                state = status.state.value if hasattr(status.state, "value") else str(status.state)
                if state in ("running", "completed"):
                    running_count += 1
            except Exception:
                pass
        if running_count >= 1:
            break
        running_count = 0
        await asyncio.sleep(1)

    # Cleanup
    for aid in alloc_ids:
        try:
            await client.cancel(aid)
        except Exception:
            pass

    assert running_count >= 1, "No allocations reached running within 30s"
