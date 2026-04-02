"""Suite 6: DAG Workflows — dependency resolution and failure propagation."""
from __future__ import annotations

import asyncio
import pytest

from lattice_sdk import DagSpec


@pytest.mark.parallel
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_linear_dag_executes_in_order(client, cluster, run_id, request):
    """3-stage linear DAG completes with stages running sequentially."""
    dag_spec = DagSpec(
        dag_id=f"ov-linear-{run_id}",
        allocations=[
            {"name": "preprocess", "tenant": "lattice-ov", "entrypoint": "echo step1", "nodes": 1},
            {"name": "train", "tenant": "lattice-ov", "entrypoint": "echo step2", "nodes": 1},
            {"name": "postprocess", "tenant": "lattice-ov", "entrypoint": "echo step3", "nodes": 1},
        ],
        edges=[
            {"from": "preprocess", "to": "train"},
            {"from": "train", "to": "postprocess"},
        ],
    )
    dag = await client.submit_dag(dag_spec)
    dag_id = dag.id if hasattr(dag, "id") else dag.get("dag_id", dag.get("id", ""))

    # Poll until completed or timeout
    for _ in range(60):
        status = await client.get_dag(dag_id)
        state = getattr(status, "state", status.get("state", "")) if isinstance(status, dict) else getattr(status, "state", "")
        if hasattr(state, "value"):
            state = state.value
        state = str(state)
        if state in ("completed", "failed"):
            break
        await asyncio.sleep(2)

    assert state == "completed", f"DAG ended in state '{state}', expected 'completed'"


@pytest.mark.parallel
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_dag_failure_stops_dependents(client, cluster, run_id, request):
    """DAG where stage-1 fails → stage-2 never runs → DAG fails."""
    dag_spec = DagSpec(
        dag_id=f"ov-fail-{run_id}",
        allocations=[
            {"name": "failing", "tenant": "lattice-ov", "entrypoint": "sh -c 'exit 1'", "nodes": 1},
            {"name": "dependent", "tenant": "lattice-ov", "entrypoint": "echo should-not-run", "nodes": 1},
        ],
        edges=[
            {"from": "failing", "to": "dependent"},
        ],
    )
    dag = await client.submit_dag(dag_spec)
    dag_id = dag.id if hasattr(dag, "id") else dag.get("dag_id", dag.get("id", ""))

    for _ in range(30):
        status = await client.get_dag(dag_id)
        state = getattr(status, "state", status.get("state", "")) if isinstance(status, dict) else getattr(status, "state", "")
        if hasattr(state, "value"):
            state = state.value
        state = str(state)
        if state in ("completed", "failed"):
            break
        await asyncio.sleep(1)

    assert state == "failed", f"DAG ended in state '{state}', expected 'failed'"
