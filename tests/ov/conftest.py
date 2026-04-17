"""Operational Validation test harness — fixtures and cluster setup."""
from __future__ import annotations

import asyncio
import uuid
from pathlib import Path
from typing import AsyncIterator, Dict, Optional

import pytest
import pytest_asyncio
import sys

# Add SDK to path
sys.path.insert(0, str(Path(__file__).parent.parent.parent / "sdk" / "python"))

from lattice_sdk import LatticeClient, AllocationSpec
from .cluster import TestCluster, DockerCluster, GcpCluster

IMAGE_DIR = Path(__file__).parent / "images"


def pytest_addoption(parser):
    parser.addoption("--cluster", default="docker", choices=["docker", "gcp"])
    parser.addoption("--api-url", default="http://localhost:8080")
    parser.addoption("--token", default="")
    parser.addoption("--registry-url", default=None)
    parser.addoption("--zone", default="europe-west1-b")
    parser.addoption("--ssh-key", default=None, help="Path to SSH private key for direct SSH (bypasses gcloud)")
    parser.addoption("--compute-ips", default=None, help="Comma-separated external IPs of compute nodes")
    parser.addoption("--registry-external-url", default=None, help="External URL for registry catalog check (if different from --registry-url)")


def pytest_configure(config):
    config.addinivalue_line("markers", "parallel: safe to run concurrently")
    config.addinivalue_line("markers", "sequential: must run alone")
    config.addinivalue_line("markers", "docker: runs on docker-compose")
    config.addinivalue_line("markers", "gcp: runs on GCP cluster")


def pytest_collection_modifyitems(config, items):
    """Auto-skip tests not matching the current cluster type."""
    cluster_type = config.getoption("--cluster")
    skip_docker = pytest.mark.skip(reason="requires docker cluster")
    skip_gcp = pytest.mark.skip(reason="requires GCP cluster")
    skip_ssh = pytest.mark.skip(reason="requires SSH (GCP only)")

    for item in items:
        markers = {m.name for m in item.iter_markers()}
        # Skip docker-only tests on GCP and vice versa
        if "docker" in markers and "gcp" not in markers and cluster_type != "docker":
            item.add_marker(skip_docker)
        if "gcp" in markers and "docker" not in markers and cluster_type != "gcp":
            item.add_marker(skip_gcp)


@pytest.fixture(scope="session")
def cluster(request) -> TestCluster:
    """Provide cluster abstraction based on --cluster flag."""
    cluster_type = request.config.getoption("--cluster")
    api_url = request.config.getoption("--api-url")
    token = request.config.getoption("--token")
    registry_url = request.config.getoption("--registry-url")
    zone = request.config.getoption("--zone")

    if cluster_type == "docker":
        return DockerCluster(
            api_url=api_url,
            token=token,
            registry_url=registry_url or "localhost:5000",
        )
    else:
        ssh_key = request.config.getoption("--ssh-key")
        compute_ips_raw = request.config.getoption("--compute-ips")
        compute_ips = compute_ips_raw.split(",") if compute_ips_raw else None
        registry_external = request.config.getoption("--registry-external-url")
        return GcpCluster(
            api_url=api_url,
            token=token,
            zone=zone,
            registry_url=registry_url,
            ssh_key=ssh_key,
            compute_ips=compute_ips,
            registry_external_url=registry_external,
        )


@pytest.fixture(scope="session")
def run_id() -> str:
    """Unique ID for this test run (for allocation labels)."""
    return f"ov-{uuid.uuid4().hex[:8]}"


@pytest_asyncio.fixture(scope="session", loop_scope="session")
async def client(cluster: TestCluster) -> AsyncIterator[LatticeClient]:
    """Authenticated SDK client for the cluster."""
    # Parse host/port from api_url
    url = cluster.api_url
    scheme = "https" if url.startswith("https") else "http"
    host_port = url.replace("http://", "").replace("https://", "")
    parts = host_port.split(":")
    host = parts[0]
    port = int(parts[1]) if len(parts) > 1 else (443 if scheme == "https" else 80)

    async with LatticeClient(
        host=host, port=port, token=cluster.token, scheme=scheme
    ) as c:
        yield c


@pytest_asyncio.fixture(scope="session", loop_scope="session")
async def images(cluster: TestCluster) -> Dict[str, str]:
    """Resolve test images. Use pre-pushed if available, else build+push."""
    if not cluster.registry_url:
        pytest.skip("No registry available")

    # Check if images are already in the registry (pre-pushed)
    import httpx
    registry = cluster.registry_url
    expected = [d.name for d in IMAGE_DIR.iterdir()
                if d.is_dir() and (d / "Dockerfile").exists()]

    # Try direct HTTP catalog check (works if registry is reachable)
    for check_url in filter(None, [
        cluster.registry_external_url,
        registry,
    ]):
        try:
            async with httpx.AsyncClient() as http:
                r = await http.get(f"http://{check_url}/v2/_catalog", timeout=5.0)
                if r.status_code == 200:
                    repos = r.json().get("repositories", [])
                    tags = {}
                    for name in expected:
                        full_repo = f"lattice-test/{name}"
                        if full_repo in repos:
                            tags[name] = f"{registry}/{full_repo}:latest"
                    if len(tags) == len(expected):
                        return tags
        except Exception:
            continue

    # Try SSH-based catalog check (for GCP where registry isn't externally exposed)
    if hasattr(cluster, "ssh_key") and cluster.ssh_key:
        import subprocess
        api_host = cluster.api_url.replace("http://", "").replace("https://", "").split(":")[0]
        try:
            result = subprocess.run(
                ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                 "-i", cluster.ssh_key, f"lattice@{api_host}",
                 f"curl -s http://{registry}/v2/_catalog"],
                capture_output=True, text=True, timeout=10,
            )
            if result.returncode == 0:
                import json
                repos = json.loads(result.stdout).get("repositories", [])
                tags = {}
                for name in expected:
                    full_repo = f"lattice-test/{name}"
                    if full_repo in repos:
                        tags[name] = f"{registry}/{full_repo}:latest"
                if len(tags) == len(expected):
                    return tags
        except Exception:
            pass

    # Fall back to building locally
    try:
        return await cluster.build_and_push_images(IMAGE_DIR)
    except RuntimeError as e:
        pytest.skip(f"Image build/push failed: {e}")


@pytest_asyncio.fixture(autouse=True)
async def cleanup_allocations(client: LatticeClient, run_id: str, request):
    """Cancel this test's allocations on teardown (ADV-OV-2)."""
    yield
    test_name = request.node.name
    try:
        allocs = await client.list_allocations()
        for alloc in allocs:
            if hasattr(alloc, "labels") and alloc.labels.get("ov-test") == test_name:
                try:
                    await client.cancel(alloc.id)
                except Exception:
                    pass
    except Exception:
        pass  # Best-effort cleanup


@pytest_asyncio.fixture(scope="session", loop_scope="session", autouse=True)
async def session_cleanup(client: LatticeClient, run_id: str):
    """Session-level safety net: cancel all run allocations (ADV-OV-2)."""
    yield
    try:
        allocs = await client.list_allocations()
        for alloc in allocs:
            if hasattr(alloc, "labels") and alloc.labels.get("ov-run") == run_id:
                try:
                    await client.cancel(alloc.id)
                except Exception:
                    pass
    except Exception:
        pass


# ── Helpers ──────────────────────────────────────────────────


async def submit_and_wait(
    client: LatticeClient,
    spec: AllocationSpec,
    target_state: str,
    timeout: float,
    run_id: str,
    test_name: str,
) -> object:
    """Submit allocation with labels, poll until target state or timeout.

    Returns the final allocation object.
    Raises TimeoutError if state not reached.
    """
    # Add tracking labels
    labels = dict(spec.labels or {})
    labels["ov-run"] = run_id
    labels["ov-test"] = test_name
    spec.labels = labels

    alloc_id = await client.submit(spec)

    deadline = asyncio.get_event_loop().time() + timeout
    while asyncio.get_event_loop().time() < deadline:
        alloc = await client.status(alloc_id)
        state = alloc.state.value if hasattr(alloc.state, "value") else str(alloc.state)
        if state == target_state:
            return alloc
        # Terminal states short-circuit
        if state in ("completed", "failed", "cancelled") and state != target_state:
            raise AssertionError(
                f"Allocation {alloc_id} reached terminal state '{state}' "
                f"instead of expected '{target_state}'"
            )
        await asyncio.sleep(1)

    raise TimeoutError(
        f"Allocation {alloc_id} did not reach '{target_state}' within {timeout}s"
    )


async def submit_and_get_id(
    client: LatticeClient,
    spec: AllocationSpec,
    run_id: str,
    test_name: str,
) -> str:
    """Submit allocation with labels, return allocation ID."""
    labels = dict(spec.labels or {})
    labels["ov-run"] = run_id
    labels["ov-test"] = test_name
    spec.labels = labels
    return await client.submit(spec)
