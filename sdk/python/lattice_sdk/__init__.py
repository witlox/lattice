"""
Lattice Python SDK - Pythonic wrapper for Lattice REST API.

Lattice is a distributed workload scheduler that sits between Slurm (HPC batch)
and Kubernetes (cloud services). This SDK provides a high-level interface for
submitting jobs, monitoring status, and observing metrics and logs.

Example:
    async with LatticeClient("lattice-api.example.com", 8080) as client:
        alloc = await client.submit(AllocationSpec(
            entrypoint="python train.py",
            nodes=2,
            cpus=16,
            memory_gb=64,
            gpus=2,
        ))
        async for event in client.watch(alloc.id):
            print(f"Job {alloc.id}: {event.allocation.state}")
"""

__version__ = "0.1.0"
__author__ = "Lattice Contributors"

from .client import LatticeClient, LatticeError, LatticeNotFoundError, LatticeAuthError
from .types import (
    AccountingUsage,
    Allocation,
    AllocationMetrics,
    AllocationSpec,
    AllocationState,
    AuditEntry,
    BackupResult,
    Dag,
    DagSpec,
    DiagnosticsReport,
    LogEntry,
    Node,
    QueueInfo,
    RaftStatus,
    ResourceRequest,
    Session,
    Tenant,
    VCluster,
    WatchEvent,
)

__all__ = [
    "LatticeClient",
    "LatticeError",
    "LatticeNotFoundError",
    "LatticeAuthError",
    "AccountingUsage",
    "Allocation",
    "AllocationMetrics",
    "AllocationSpec",
    "AllocationState",
    "AuditEntry",
    "BackupResult",
    "Dag",
    "DagSpec",
    "DiagnosticsReport",
    "LogEntry",
    "Node",
    "QueueInfo",
    "RaftStatus",
    "ResourceRequest",
    "Session",
    "Tenant",
    "VCluster",
    "WatchEvent",
]
