"""
Lattice Python SDK - Pythonic wrapper for Lattice gRPC API.

Lattice is a distributed workload scheduler that sits between Slurm (HPC batch)
and Kubernetes (cloud services). This SDK provides a high-level interface for
submitting jobs, monitoring status, and observing metrics and logs.

Example:
    async with LatticeClient("lattice-api.example.com", 50051) as client:
        alloc_id = await client.submit(
            entrypoint="python train.py",
            nodes=2,
            cpus=16,
            memory_gb=64,
            gpus=2
        )
        async for event in client.watch(alloc_id):
            print(f"Job {alloc_id}: {event.allocation.state}")
"""

__version__ = "0.1.0"
__author__ = "Lattice Contributors"

from .client import LatticeClient
from .types import (
    Allocation,
    AllocationMetrics,
    AllocationState,
    LogEntry,
    Node,
    ResourceRequest,
    Tenant,
    VCluster,
    WatchEvent,
)

__all__ = [
    "LatticeClient",
    "Allocation",
    "AllocationMetrics",
    "AllocationState",
    "LogEntry",
    "Node",
    "ResourceRequest",
    "Tenant",
    "VCluster",
    "WatchEvent",
]
