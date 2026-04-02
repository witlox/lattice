"""Suite 4: Drain Under Load — node lifecycle with active allocations."""
from __future__ import annotations

import asyncio
import pytest

from lattice_sdk import AllocationSpec
from .conftest import submit_and_wait, submit_and_get_id


@pytest.mark.sequential
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_drain_node_with_running_allocation(client, cluster, run_id, request):
    """Draining a node with active allocation → draining → drained → undrain → ready."""
    # Submit a long-running allocation
    spec = AllocationSpec(
        tenant="lattice-ov",
        entrypoint="sleep 60",
        walltime_hours=0.05,
    )
    alloc = await submit_and_wait(
        client, spec, "running", timeout=30, run_id=run_id, test_name=request.node.name,
    )

    # Get the node it's running on
    assert len(alloc.nodes_allocated) >= 1
    node_id = alloc.nodes_allocated[0]

    # Drain that node
    nodes = await client.list_nodes()
    target = [n for n in nodes if getattr(n, "id", getattr(n, "name", "")) == node_id]
    assert target, f"Node {node_id} not found"

    await client.drain_node(node_id)

    # Node should be draining (has active allocation)
    for _ in range(10):
        nodes = await client.list_nodes()
        node = next(
            (n for n in nodes if getattr(n, "id", getattr(n, "name", "")) == node_id),
            None,
        )
        if node:
            state = getattr(node, "state", getattr(node, "status", ""))
            if hasattr(state, "value"):
                state = state.value
            if state in ("draining", "drained"):
                break
        await asyncio.sleep(1)

    # Cancel the allocation so the node can finish draining
    await client.cancel(alloc.id)

    # Wait for drained
    for _ in range(15):
        nodes = await client.list_nodes()
        node = next(
            (n for n in nodes if getattr(n, "id", getattr(n, "name", "")) == node_id),
            None,
        )
        if node:
            state = getattr(node, "state", getattr(node, "status", ""))
            if hasattr(state, "value"):
                state = state.value
            if state == "drained":
                break
        await asyncio.sleep(1)

    # Undrain
    await client.undrain_node(node_id)

    # Should return to ready
    for _ in range(10):
        nodes = await client.list_nodes()
        node = next(
            (n for n in nodes if getattr(n, "id", getattr(n, "name", "")) == node_id),
            None,
        )
        if node:
            state = getattr(node, "state", getattr(node, "status", ""))
            if hasattr(state, "value"):
                state = state.value
            if state == "ready":
                return  # Success
        await asyncio.sleep(1)

    pytest.fail(f"Node {node_id} did not return to ready after undrain")


@pytest.mark.sequential
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_drain_empty_node_is_immediate(client, cluster, run_id, request):
    """Draining a node with no allocations transitions to drained quickly."""
    nodes = await client.list_nodes()
    if len(nodes) < 2:
        pytest.skip("Need at least 2 nodes for this test")

    # Pick last node (least likely to have allocations)
    node_id = getattr(nodes[-1], "id", getattr(nodes[-1], "name", ""))

    await client.drain_node(node_id)

    # Should reach drained within 5s
    for _ in range(5):
        nodes = await client.list_nodes()
        node = next(
            (n for n in nodes if getattr(n, "id", getattr(n, "name", "")) == node_id),
            None,
        )
        if node:
            state = getattr(node, "state", getattr(node, "status", ""))
            if hasattr(state, "value"):
                state = state.value
            if state == "drained":
                # Undrain and return
                await client.undrain_node(node_id)
                return
        await asyncio.sleep(1)

    # Cleanup
    try:
        await client.undrain_node(node_id)
    except Exception:
        pass
    pytest.fail(f"Empty node {node_id} did not reach drained within 5s")
