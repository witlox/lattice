"""
Shared fixtures for Lattice e2e tests using testcontainers.

Spins up a single-node Lattice server + agent + VictoriaMetrics via
docker-compose, waits for health, and provides a connected LatticeClient.
"""

import os
import sys
import time
from pathlib import Path

import httpx
import pytest

# Add SDK to path
sys.path.insert(0, str(Path(__file__).parent.parent.parent / "sdk" / "python"))

from testcontainers.compose import DockerCompose

from lattice_sdk import LatticeClient

E2E_DIR = Path(__file__).parent
COMPOSE_FILE = E2E_DIR / "docker-compose.e2e.yml"
STARTUP_TIMEOUT = 180  # seconds — includes Docker build time


@pytest.fixture(scope="session")
def lattice_stack():
    """Start the Lattice stack via docker-compose and yield connection info."""
    compose = DockerCompose(
        str(E2E_DIR),
        compose_file_name="docker-compose.e2e.yml",
        build=True,
    )
    compose.start()

    # Wait for server to be healthy
    rest_host = "localhost"
    rest_port = int(compose.get_service_port("lattice-server", 8080))

    base_url = f"http://{rest_host}:{rest_port}"
    _wait_for_healthy(base_url, timeout=STARTUP_TIMEOUT)

    # Wait for agent to register (poll /api/v1/nodes)
    _wait_for_nodes(base_url, expected=1, timeout=60)

    yield {
        "host": rest_host,
        "port": rest_port,
        "base_url": base_url,
        "compose": compose,
    }

    compose.stop()


@pytest.fixture
async def client(lattice_stack):
    """Provide a connected LatticeClient for each test."""
    async with LatticeClient(
        host=lattice_stack["host"],
        port=lattice_stack["port"],
        timeout=30.0,
    ) as c:
        yield c


def _wait_for_healthy(base_url: str, timeout: int) -> None:
    """Poll /healthz until the server responds 200."""
    deadline = time.time() + timeout
    last_error = None
    while time.time() < deadline:
        try:
            r = httpx.get(f"{base_url}/healthz", timeout=5.0)
            if r.status_code == 200:
                return
        except (httpx.ConnectError, httpx.ReadError, httpx.TimeoutException) as e:
            last_error = e
        time.sleep(2)
    raise TimeoutError(
        f"Server at {base_url} not healthy after {timeout}s. Last error: {last_error}"
    )


def _wait_for_nodes(base_url: str, expected: int, timeout: int) -> None:
    """Poll /api/v1/nodes until at least `expected` nodes are registered."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            r = httpx.get(f"{base_url}/api/v1/nodes", timeout=5.0)
            if r.status_code == 200:
                data = r.json()
                nodes = data if isinstance(data, list) else data.get("nodes", [])
                if len(nodes) >= expected:
                    return
        except (httpx.ConnectError, httpx.ReadError, httpx.TimeoutException):
            pass
        time.sleep(3)
    raise TimeoutError(f"Expected {expected} nodes within {timeout}s")
