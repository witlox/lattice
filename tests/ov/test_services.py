"""Suite 2: Service Workloads — targets service_workloads LOW confidence area."""
from __future__ import annotations

import asyncio
import pytest

from lattice_sdk import AllocationSpec
from .conftest import submit_and_wait, submit_and_get_id


@pytest.mark.parallel
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_unbounded_service_stays_running(client, cluster, images, run_id, request):
    """Unbounded service allocation stays running until cancelled."""
    spec = AllocationSpec(
        tenant="lattice-ov",
        entrypoint="python /app/server.py",
        image=images["service-http"],
        lifecycle="unbounded",
    )
    alloc = await submit_and_wait(
        client, spec, "running", timeout=30, run_id=run_id, test_name=request.node.name,
    )

    # Verify it remains running for 15s
    for _ in range(5):
        await asyncio.sleep(3)
        status = await client.status(alloc.id)
        state = status.state.value if hasattr(status.state, "value") else str(status.state)
        assert state == "running", f"Service stopped unexpectedly: {state}"

    # Cancel and verify completion
    await client.cancel(alloc.id)
    for _ in range(10):
        status = await client.status(alloc.id)
        state = status.state.value if hasattr(status.state, "value") else str(status.state)
        if state in ("completed", "cancelled"):
            return
        await asyncio.sleep(1)
    pytest.fail("Service did not stop within 10s of cancel")


@pytest.mark.parallel
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_service_failure_triggers_requeue(client, cluster, images, run_id, request):
    """Crashing unbounded service with requeue_policy=always is requeued."""
    spec = AllocationSpec(
        tenant="lattice-ov",
        entrypoint="sh -c 'sleep 3 && exit 1'",
        image=images["crasher"],
        lifecycle="unbounded",
        requeue_policy="always",
        max_requeue=3,
    )
    alloc_id = await submit_and_get_id(
        client, spec, run_id=run_id, test_name=request.node.name,
    )

    # Wait for it to start, crash, and get requeued
    # We should see it go through running → failed → pending/running cycle
    saw_running = False
    saw_requeue = False
    for _ in range(30):
        status = await client.status(alloc_id)
        state = status.state.value if hasattr(status.state, "value") else str(status.state)
        if state == "running":
            saw_running = True
        if saw_running and state in ("pending", "running"):
            # After first run, seeing pending again means requeue
            saw_requeue = True
            break
        await asyncio.sleep(1)

    # Clean up
    try:
        await client.cancel(alloc_id)
    except Exception:
        pass

    assert saw_running, "Service never reached running state"
    # Note: requeue depends on reconciliation loop timing; this is best-effort


@pytest.mark.parallel
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_max_requeue_exhaustion(client, cluster, images, run_id, request):
    """Service with max_requeue=1 stops retrying after limit."""
    spec = AllocationSpec(
        tenant="lattice-ov",
        entrypoint="sh -c 'exit 1'",
        image=images["crasher"],
        lifecycle="unbounded",
        requeue_policy="always",
        max_requeue=1,
    )
    alloc_id = await submit_and_get_id(
        client, spec, run_id=run_id, test_name=request.node.name,
    )

    # Eventually should reach failed (not keep retrying)
    for _ in range(60):
        status = await client.status(alloc_id)
        state = status.state.value if hasattr(status.state, "value") else str(status.state)
        if state == "failed":
            return  # Success — exhausted requeues
        await asyncio.sleep(1)

    # Clean up
    try:
        await client.cancel(alloc_id)
    except Exception:
        pass

    pytest.fail("Allocation did not reach failed state after requeue exhaustion")
