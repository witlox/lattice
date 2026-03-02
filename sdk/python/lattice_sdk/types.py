"""
Python dataclasses mirroring Lattice core domain types.

These types provide a Pythonic interface before protobuf stubs are fully generated.
Eventually these will be replaced by generated protobuf messages.
"""

from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Dict, List, Optional


class AllocationState(str, Enum):
    """Allocation lifecycle states."""
    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"
    CHECKPOINTED = "checkpointed"


@dataclass
class ResourceRequest:
    """Resource request for an allocation."""
    cpus: float
    memory_gb: float
    gpus: int = 0
    gpu_memory_gb: float = 0.0
    storage_gb: float = 0.0
    metadata: Dict[str, str] = field(default_factory=dict)


@dataclass
class Node:
    """Represents a compute node in the cluster."""
    name: str
    host: str
    status: str  # "available", "allocated", "maintenance", etc.
    total_cpus: float
    total_memory_gb: float
    total_gpus: int = 0
    available_cpus: float = 0.0
    available_memory_gb: float = 0.0
    available_gpus: int = 0
    last_heartbeat: Optional[datetime] = None
    metadata: Dict[str, str] = field(default_factory=dict)


@dataclass
class Allocation:
    """Universal work unit in Lattice (job or service)."""
    id: str
    entrypoint: str
    state: AllocationState
    requested_resources: ResourceRequest
    tenant_id: str
    user_id: str
    priority_class: str = "normal"
    network_domain: Optional[str] = None
    uenv: Optional[str] = None
    nodes_allocated: List[str] = field(default_factory=list)
    created_at: Optional[datetime] = None
    started_at: Optional[datetime] = None
    completed_at: Optional[datetime] = None
    labels: Dict[str, str] = field(default_factory=dict)
    annotations: Dict[str, str] = field(default_factory=dict)


@dataclass
class AllocationMetrics:
    """Metrics for an allocation (GPU util, CPU, memory, I/O, etc)."""
    allocation_id: str
    timestamp: datetime
    cpu_utilization: float = 0.0
    memory_utilization_gb: float = 0.0
    gpu_utilization: float = 0.0
    gpu_memory_utilization_gb: float = 0.0
    network_in_mbps: float = 0.0
    network_out_mbps: float = 0.0
    io_read_mbps: float = 0.0
    io_write_mbps: float = 0.0
    metadata: Dict[str, str] = field(default_factory=dict)


@dataclass
class WatchEvent:
    """Event yielded by watch() stream."""
    event_type: str  # "created", "updated", "completed", "failed", "cancelled"
    allocation: Allocation
    timestamp: datetime = field(default_factory=datetime.now)


@dataclass
class LogEntry:
    """Log entry from an allocation."""
    timestamp: datetime
    level: str  # "DEBUG", "INFO", "WARN", "ERROR"
    message: str
    source: Optional[str] = None  # node name or component


@dataclass
class Tenant:
    """Organizational boundary with quotas and isolation."""
    id: str
    name: str
    quota_cpus: float
    quota_memory_gb: float
    quota_gpus: int = 0
    used_cpus: float = 0.0
    used_memory_gb: float = 0.0
    used_gpus: int = 0
    metadata: Dict[str, str] = field(default_factory=dict)


@dataclass
class VCluster:
    """A view/projection of resources with its own scheduler policy."""
    name: str
    scheduler_type: str  # "hpc_backfill", "service_binpack", "medical", "interactive_fifo"
    resource_filter: Dict[str, str] = field(default_factory=dict)
    scheduler_weights: Dict[str, float] = field(default_factory=dict)
