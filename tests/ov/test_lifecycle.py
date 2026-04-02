"""Suite 1: Allocation Lifecycle — basic submit/complete/fail paths."""
from __future__ import annotations

import pytest
import pytest_asyncio

from lattice_sdk import AllocationSpec
from .conftest import submit_and_wait, submit_and_get_id


@pytest.mark.parallel
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_bare_process_completes(client, cluster, run_id, request):
    """Bare allocation running echo completes successfully."""
    spec = AllocationSpec(
        tenant="lattice-ov",
        entrypoint="/bin/echo hello-lattice",
    )
    alloc = await submit_and_wait(
        client, spec, "completed", timeout=30, run_id=run_id, test_name=request.node.name,
    )
    assert len(alloc.nodes_allocated) >= 1


@pytest.mark.parallel
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_container_numpy_bench(client, cluster, images, run_id, request):
    """NumPy benchmark container completes and reports GFLOPS."""
    spec = AllocationSpec(
        tenant="lattice-ov",
        entrypoint="python /app/bench.py",
        image=images["numpy-bench"],
    )
    alloc = await submit_and_wait(
        client, spec, "completed", timeout=60, run_id=run_id, test_name=request.node.name,
    )
    assert alloc is not None


@pytest.mark.parallel
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_container_pytorch_mnist(client, cluster, images, run_id, request):
    """PyTorch MNIST training completes."""
    spec = AllocationSpec(
        tenant="lattice-ov",
        entrypoint="python /app/train.py",
        image=images["pytorch-mnist"],
        walltime_hours=0.1,  # 6 minutes max
    )
    alloc = await submit_and_wait(
        client, spec, "completed", timeout=120, run_id=run_id, test_name=request.node.name,
    )
    assert alloc is not None


@pytest.mark.parallel
@pytest.mark.docker
@pytest.mark.gcp
@pytest.mark.asyncio
async def test_walltime_exceeded(client, cluster, run_id, request):
    """Allocation exceeding walltime is terminated."""
    spec = AllocationSpec(
        tenant="lattice-ov",
        entrypoint="sleep 300",
        walltime_hours=10.0 / 3600.0,  # 10 seconds
    )
    alloc = await submit_and_wait(
        client, spec, "failed", timeout=30, run_id=run_id, test_name=request.node.name,
    )
    assert alloc is not None
