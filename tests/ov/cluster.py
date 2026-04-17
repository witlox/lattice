"""TestCluster abstraction — environment-agnostic cluster interface."""
from __future__ import annotations

import asyncio
import json
import subprocess
from abc import ABC, abstractmethod
from pathlib import Path
from typing import Dict, List, Optional


class TestCluster(ABC):
    """Environment-agnostic interface to a deployed lattice cluster."""

    api_url: str
    token: str
    compute_node_count: int
    registry_url: Optional[str]
    has_ssh: bool
    has_gpu: bool

    @abstractmethod
    async def push_image(self, local_tag: str, remote_tag: str) -> bool:
        """Push a local Docker image to the cluster's registry."""

    @abstractmethod
    async def restart_agent(self, node_index: int) -> None:
        """Restart the lattice-agent on a compute node (graceful)."""

    @abstractmethod
    async def kill_agent(self, node_index: int) -> None:
        """Kill the lattice-agent on a compute node (SIGKILL)."""

    @abstractmethod
    async def health_check(self) -> bool:
        """Check if the cluster API is healthy."""

    async def build_and_push_images(self, image_dir: Path) -> Dict[str, str]:
        """Build all test images and push to registry.

        Returns mapping: image_name -> full registry tag.
        Raises RuntimeError if any build/push fails.
        """
        if not self.registry_url:
            raise RuntimeError("No registry available for image push")

        tags: Dict[str, str] = {}
        for dockerfile_dir in sorted(image_dir.iterdir()):
            if not dockerfile_dir.is_dir():
                continue
            if not (dockerfile_dir / "Dockerfile").exists():
                continue
            name = dockerfile_dir.name
            local_tag = f"lattice-test/{name}:latest"
            remote_tag = f"{self.registry_url}/lattice-test/{name}:latest"

            # Build
            proc = await asyncio.create_subprocess_exec(
                "docker", "build", "-t", local_tag, str(dockerfile_dir),
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
            _, stderr = await proc.communicate()
            if proc.returncode != 0:
                raise RuntimeError(
                    f"Failed to build {name}: {stderr.decode()}"
                )

            # Tag + push
            await asyncio.create_subprocess_exec(
                "docker", "tag", local_tag, remote_tag,
                stdout=asyncio.subprocess.DEVNULL,
            )
            proc = await asyncio.create_subprocess_exec(
                "docker", "push", remote_tag,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
            _, stderr = await proc.communicate()
            if proc.returncode != 0:
                raise RuntimeError(
                    f"Failed to push {name}: {stderr.decode()}"
                )

            tags[name] = remote_tag

        return tags


class DockerCluster(TestCluster):
    """docker-compose E2E stack with registry."""

    def __init__(
        self,
        api_url: str = "http://localhost:8080",
        token: str = "",
        registry_url: str = "localhost:5000",
        compute_node_count: int = 1,
    ):
        self.api_url = api_url
        self.token = token
        self.registry_url = registry_url
        self.compute_node_count = compute_node_count
        self.has_ssh = False
        self.has_gpu = False

    async def push_image(self, local_tag: str, remote_tag: str) -> bool:
        proc = await asyncio.create_subprocess_exec(
            "docker", "push", remote_tag,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        await proc.communicate()
        return proc.returncode == 0

    async def restart_agent(self, node_index: int) -> None:
        raise NotImplementedError("Docker cluster does not support agent restart")

    async def kill_agent(self, node_index: int) -> None:
        raise NotImplementedError("Docker cluster does not support agent kill")

    async def health_check(self) -> bool:
        import httpx
        try:
            async with httpx.AsyncClient() as client:
                r = await client.get(f"{self.api_url}/healthz", timeout=5.0)
                return r.status_code == 200
        except Exception:
            return False


class GcpCluster(TestCluster):
    """Terraform-provisioned GCP cluster."""

    def __init__(
        self,
        api_url: str,
        token: str,
        zone: str = "europe-west1-b",
        compute_node_count: int = 2,
        registry_url: Optional[str] = None,
        ssh_key: Optional[str] = None,
        compute_ips: Optional[List[str]] = None,
    ):
        self.api_url = api_url
        self.token = token
        self.zone = zone
        self.compute_node_count = compute_node_count
        self.registry_url = registry_url
        self.ssh_key = ssh_key
        self.compute_ips = compute_ips or []
        self.has_ssh = True
        self.has_gpu = False

    def _ssh(self, instance_or_ip: str, command: str) -> subprocess.CompletedProcess:
        """SSH to a node. Uses direct SSH if ssh_key is set, gcloud otherwise."""
        if self.ssh_key:
            return subprocess.run(
                ["ssh", "-o", "StrictHostKeyChecking=no",
                 "-o", "UserKnownHostsFile=/dev/null",
                 "-i", self.ssh_key,
                 f"lattice@{instance_or_ip}", command],
                capture_output=True, text=True, timeout=30,
            )
        return subprocess.run(
            ["gcloud", "compute", "ssh", instance_or_ip,
             f"--zone={self.zone}", f"--command={command}"],
            capture_output=True, text=True, timeout=30,
        )

    def _compute_target(self, node_index: int) -> str:
        """Resolve compute node target (IP if available, instance name otherwise)."""
        if self.compute_ips and node_index < len(self.compute_ips):
            return self.compute_ips[node_index]
        return f"lattice-test-compute-{node_index + 1}"

    async def push_image(self, local_tag: str, remote_tag: str) -> bool:
        proc = await asyncio.create_subprocess_exec(
            "docker", "push", remote_tag,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        await proc.communicate()
        return proc.returncode == 0

    async def restart_agent(self, node_index: int) -> None:
        target = self._compute_target(node_index)
        result = self._ssh(target, "sudo systemctl restart lattice-agent")
        if result.returncode != 0:
            raise RuntimeError(f"Failed to restart agent: {result.stderr}")

    async def kill_agent(self, node_index: int) -> None:
        target = self._compute_target(node_index)
        result = self._ssh(target, "sudo kill -9 $(pidof lattice-agent)")
        if result.returncode != 0:
            raise RuntimeError(f"Failed to kill agent: {result.stderr}")

    async def health_check(self) -> bool:
        import httpx
        try:
            async with httpx.AsyncClient() as client:
                r = await client.get(f"{self.api_url}/healthz", timeout=5.0)
                return r.status_code == 200
        except Exception:
            return False
