"""
Python dataclasses mirroring Lattice core domain types.

These types provide a Pythonic interface for the Lattice REST API.
Each type supports serialization via to_dict() and deserialization via from_dict().
"""

from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from typing import Any, Dict, List, Optional


def _parse_datetime(value: Any) -> Optional[datetime]:
    """Parse a datetime from ISO format string or return as-is if already a datetime."""
    if value is None:
        return None
    if isinstance(value, datetime):
        return value
    if isinstance(value, str):
        try:
            return datetime.fromisoformat(value)
        except ValueError:
            if value.endswith("Z"):
                return datetime.fromisoformat(value[:-1]).replace(tzinfo=timezone.utc)
            raise
    raise TypeError(f"Cannot parse datetime from {type(value)}: {value}")


def _format_datetime(value: Optional[datetime]) -> Optional[str]:
    """Format a datetime as ISO 8601 string."""
    if value is None:
        return None
    return value.isoformat()


class AllocationState(str, Enum):
    """Allocation lifecycle states."""
    PENDING = "pending"
    STAGING = "staging"
    RUNNING = "running"
    CHECKPOINTING = "checkpointing"
    SUSPENDED = "suspended"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


@dataclass
class ResourceRequest:
    """Resource request for an allocation."""
    cpus: float
    memory_gb: float
    gpus: int = 0
    gpu_memory_gb: float = 0.0
    storage_gb: float = 0.0
    metadata: Dict[str, str] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        d: Dict[str, Any] = {
            "cpus": self.cpus,
            "memory_gb": self.memory_gb,
            "gpus": self.gpus,
            "gpu_memory_gb": self.gpu_memory_gb,
            "storage_gb": self.storage_gb,
        }
        if self.metadata:
            d["metadata"] = dict(self.metadata)
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "ResourceRequest":
        return cls(
            cpus=float(data.get("cpus", 1.0)),
            memory_gb=float(data.get("memory_gb", 4.0)),
            gpus=int(data.get("gpus", 0)),
            gpu_memory_gb=float(data.get("gpu_memory_gb", 0.0)),
            storage_gb=float(data.get("storage_gb", 0.0)),
            metadata=data.get("metadata", {}),
        )


@dataclass
class Node:
    """Represents a compute node in the cluster."""
    name: str
    host: str
    status: str
    total_cpus: float
    total_memory_gb: float
    total_gpus: int = 0
    available_cpus: float = 0.0
    available_memory_gb: float = 0.0
    available_gpus: int = 0
    last_heartbeat: Optional[datetime] = None
    metadata: Dict[str, str] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        return {
            "name": self.name,
            "host": self.host,
            "status": self.status,
            "total_cpus": self.total_cpus,
            "total_memory_gb": self.total_memory_gb,
            "total_gpus": self.total_gpus,
            "available_cpus": self.available_cpus,
            "available_memory_gb": self.available_memory_gb,
            "available_gpus": self.available_gpus,
            "last_heartbeat": _format_datetime(self.last_heartbeat),
            "metadata": dict(self.metadata),
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "Node":
        return cls(
            name=data["name"],
            host=data["host"],
            status=data["status"],
            total_cpus=float(data["total_cpus"]),
            total_memory_gb=float(data["total_memory_gb"]),
            total_gpus=int(data.get("total_gpus", 0)),
            available_cpus=float(data.get("available_cpus", 0.0)),
            available_memory_gb=float(data.get("available_memory_gb", 0.0)),
            available_gpus=int(data.get("available_gpus", 0)),
            last_heartbeat=_parse_datetime(data.get("last_heartbeat")),
            metadata=data.get("metadata", {}),
        )


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
    exit_code: Optional[int] = None
    message: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        d: Dict[str, Any] = {
            "id": self.id,
            "entrypoint": self.entrypoint,
            "state": self.state.value if isinstance(self.state, AllocationState) else self.state,
            "requested_resources": self.requested_resources.to_dict(),
            "tenant_id": self.tenant_id,
            "user_id": self.user_id,
            "priority_class": self.priority_class,
        }
        if self.network_domain is not None:
            d["network_domain"] = self.network_domain
        if self.uenv is not None:
            d["uenv"] = self.uenv
        if self.nodes_allocated:
            d["nodes_allocated"] = list(self.nodes_allocated)
        if self.created_at is not None:
            d["created_at"] = _format_datetime(self.created_at)
        if self.started_at is not None:
            d["started_at"] = _format_datetime(self.started_at)
        if self.completed_at is not None:
            d["completed_at"] = _format_datetime(self.completed_at)
        if self.labels:
            d["labels"] = dict(self.labels)
        if self.annotations:
            d["annotations"] = dict(self.annotations)
        if self.exit_code is not None:
            d["exit_code"] = self.exit_code
        if self.message is not None:
            d["message"] = self.message
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "Allocation":
        resources_data = data.get("requested_resources", {})
        if resources_data:
            resources = ResourceRequest.from_dict(resources_data)
        else:
            resources = ResourceRequest(cpus=1.0, memory_gb=4.0)
        state_str = data.get("state", "pending")
        try:
            state = AllocationState(state_str)
        except ValueError:
            state = AllocationState.PENDING
        return cls(
            id=data["id"],
            entrypoint=data.get("entrypoint", ""),
            state=state,
            requested_resources=resources,
            tenant_id=data.get("tenant_id", ""),
            user_id=data.get("user_id", ""),
            priority_class=data.get("priority_class", "normal"),
            network_domain=data.get("network_domain"),
            uenv=data.get("uenv"),
            nodes_allocated=data.get("nodes_allocated", []),
            created_at=_parse_datetime(data.get("created_at")),
            started_at=_parse_datetime(data.get("started_at")),
            completed_at=_parse_datetime(data.get("completed_at")),
            labels=data.get("labels", {}),
            annotations=data.get("annotations", {}),
            exit_code=data.get("exit_code"),
            message=data.get("message"),
        )


@dataclass
class AllocationSpec:
    """Specification for submitting a new allocation."""
    entrypoint: str
    nodes: int = 1
    cpus: float = 1.0
    memory_gb: float = 4.0
    gpus: int = 0
    gpu_memory_gb: float = 0.0
    storage_gb: float = 0.0
    priority_class: str = "normal"
    tenant_id: Optional[str] = None
    user_id: Optional[str] = None
    network_domain: Optional[str] = None
    uenv: Optional[str] = None
    labels: Optional[Dict[str, str]] = None
    annotations: Optional[Dict[str, str]] = None

    def to_dict(self) -> Dict[str, Any]:
        d: Dict[str, Any] = {
            "entrypoint": self.entrypoint,
            "nodes": self.nodes,
            "resources": {
                "cpus": self.cpus,
                "memory_gb": self.memory_gb,
                "gpus": self.gpus,
                "gpu_memory_gb": self.gpu_memory_gb,
                "storage_gb": self.storage_gb,
            },
            "priority_class": self.priority_class,
        }
        if self.tenant_id is not None:
            d["tenant_id"] = self.tenant_id
        if self.user_id is not None:
            d["user_id"] = self.user_id
        if self.network_domain is not None:
            d["network_domain"] = self.network_domain
        if self.uenv is not None:
            d["uenv"] = self.uenv
        if self.labels:
            d["labels"] = dict(self.labels)
        if self.annotations:
            d["annotations"] = dict(self.annotations)
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "AllocationSpec":
        resources = data.get("resources", {})
        return cls(
            entrypoint=data["entrypoint"],
            nodes=data.get("nodes", 1),
            cpus=float(resources.get("cpus", data.get("cpus", 1.0))),
            memory_gb=float(resources.get("memory_gb", data.get("memory_gb", 4.0))),
            gpus=int(resources.get("gpus", data.get("gpus", 0))),
            gpu_memory_gb=float(resources.get("gpu_memory_gb", data.get("gpu_memory_gb", 0.0))),
            storage_gb=float(resources.get("storage_gb", data.get("storage_gb", 0.0))),
            priority_class=data.get("priority_class", "normal"),
            tenant_id=data.get("tenant_id"),
            user_id=data.get("user_id"),
            network_domain=data.get("network_domain"),
            uenv=data.get("uenv"),
            labels=data.get("labels"),
            annotations=data.get("annotations"),
        )


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

    def to_dict(self) -> Dict[str, Any]:
        return {
            "allocation_id": self.allocation_id,
            "timestamp": _format_datetime(self.timestamp),
            "cpu_utilization": self.cpu_utilization,
            "memory_utilization_gb": self.memory_utilization_gb,
            "gpu_utilization": self.gpu_utilization,
            "gpu_memory_utilization_gb": self.gpu_memory_utilization_gb,
            "network_in_mbps": self.network_in_mbps,
            "network_out_mbps": self.network_out_mbps,
            "io_read_mbps": self.io_read_mbps,
            "io_write_mbps": self.io_write_mbps,
            "metadata": dict(self.metadata),
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "AllocationMetrics":
        return cls(
            allocation_id=data["allocation_id"],
            timestamp=_parse_datetime(data["timestamp"]),
            cpu_utilization=float(data.get("cpu_utilization", 0.0)),
            memory_utilization_gb=float(data.get("memory_utilization_gb", 0.0)),
            gpu_utilization=float(data.get("gpu_utilization", 0.0)),
            gpu_memory_utilization_gb=float(data.get("gpu_memory_utilization_gb", 0.0)),
            network_in_mbps=float(data.get("network_in_mbps", 0.0)),
            network_out_mbps=float(data.get("network_out_mbps", 0.0)),
            io_read_mbps=float(data.get("io_read_mbps", 0.0)),
            io_write_mbps=float(data.get("io_write_mbps", 0.0)),
            metadata=data.get("metadata", {}),
        )


@dataclass
class WatchEvent:
    """Event yielded by watch() stream."""
    event_type: str
    allocation: Allocation
    timestamp: datetime = field(default_factory=datetime.now)

    def to_dict(self) -> Dict[str, Any]:
        return {
            "event_type": self.event_type,
            "allocation": self.allocation.to_dict(),
            "timestamp": _format_datetime(self.timestamp),
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "WatchEvent":
        return cls(
            event_type=data["event_type"],
            allocation=Allocation.from_dict(data["allocation"]),
            timestamp=_parse_datetime(data.get("timestamp")) or datetime.now(),
        )


@dataclass
class LogEntry:
    """Log entry from an allocation."""
    timestamp: datetime
    level: str
    message: str
    source: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        d: Dict[str, Any] = {
            "timestamp": _format_datetime(self.timestamp),
            "level": self.level,
            "message": self.message,
        }
        if self.source is not None:
            d["source"] = self.source
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "LogEntry":
        return cls(
            timestamp=_parse_datetime(data["timestamp"]),
            level=data["level"],
            message=data["message"],
            source=data.get("source"),
        )


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

    def to_dict(self) -> Dict[str, Any]:
        return {
            "id": self.id,
            "name": self.name,
            "quota_cpus": self.quota_cpus,
            "quota_memory_gb": self.quota_memory_gb,
            "quota_gpus": self.quota_gpus,
            "used_cpus": self.used_cpus,
            "used_memory_gb": self.used_memory_gb,
            "used_gpus": self.used_gpus,
            "metadata": dict(self.metadata),
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "Tenant":
        return cls(
            id=data["id"],
            name=data["name"],
            quota_cpus=float(data["quota_cpus"]),
            quota_memory_gb=float(data["quota_memory_gb"]),
            quota_gpus=int(data.get("quota_gpus", 0)),
            used_cpus=float(data.get("used_cpus", 0.0)),
            used_memory_gb=float(data.get("used_memory_gb", 0.0)),
            used_gpus=int(data.get("used_gpus", 0)),
            metadata=data.get("metadata", {}),
        )


@dataclass
class VCluster:
    """A view/projection of resources with its own scheduler policy."""
    name: str
    scheduler_type: str
    resource_filter: Dict[str, str] = field(default_factory=dict)
    scheduler_weights: Dict[str, float] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        return {
            "name": self.name,
            "scheduler_type": self.scheduler_type,
            "resource_filter": dict(self.resource_filter),
            "scheduler_weights": dict(self.scheduler_weights),
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "VCluster":
        return cls(
            name=data["name"],
            scheduler_type=data["scheduler_type"],
            resource_filter=data.get("resource_filter", {}),
            scheduler_weights=data.get("scheduler_weights", {}),
        )


@dataclass
class Session:
    """Interactive session attached to an allocation."""
    id: str
    allocation_id: str
    user_id: str
    created_at: Optional[datetime] = None
    expires_at: Optional[datetime] = None
    status: str = "active"
    metadata: Dict[str, str] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        d: Dict[str, Any] = {
            "id": self.id,
            "allocation_id": self.allocation_id,
            "user_id": self.user_id,
            "status": self.status,
        }
        if self.created_at is not None:
            d["created_at"] = _format_datetime(self.created_at)
        if self.expires_at is not None:
            d["expires_at"] = _format_datetime(self.expires_at)
        if self.metadata:
            d["metadata"] = dict(self.metadata)
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "Session":
        return cls(
            id=data["id"],
            allocation_id=data["allocation_id"],
            user_id=data["user_id"],
            created_at=_parse_datetime(data.get("created_at")),
            expires_at=_parse_datetime(data.get("expires_at")),
            status=data.get("status", "active"),
            metadata=data.get("metadata", {}),
        )


@dataclass
class DagSpec:
    """Specification for submitting a DAG of allocations."""
    name: str
    allocations: List[Dict[str, Any]]
    edges: List[Dict[str, str]] = field(default_factory=list)
    tenant_id: Optional[str] = None
    user_id: Optional[str] = None
    labels: Optional[Dict[str, str]] = None

    def to_dict(self) -> Dict[str, Any]:
        d: Dict[str, Any] = {
            "name": self.name,
            "allocations": list(self.allocations),
            "edges": list(self.edges),
        }
        if self.tenant_id is not None:
            d["tenant_id"] = self.tenant_id
        if self.user_id is not None:
            d["user_id"] = self.user_id
        if self.labels:
            d["labels"] = dict(self.labels)
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "DagSpec":
        return cls(
            name=data["name"],
            allocations=data.get("allocations", []),
            edges=data.get("edges", []),
            tenant_id=data.get("tenant_id"),
            user_id=data.get("user_id"),
            labels=data.get("labels"),
        )


@dataclass
class Dag:
    """Directed acyclic graph of allocations with dependency edges."""
    id: str
    name: str
    state: str
    allocations: List[str] = field(default_factory=list)
    edges: List[Dict[str, str]] = field(default_factory=list)
    tenant_id: str = ""
    user_id: str = ""
    created_at: Optional[datetime] = None
    completed_at: Optional[datetime] = None
    metadata: Dict[str, str] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        d: Dict[str, Any] = {
            "id": self.id,
            "name": self.name,
            "state": self.state,
            "allocations": list(self.allocations),
            "edges": list(self.edges),
            "tenant_id": self.tenant_id,
            "user_id": self.user_id,
        }
        if self.created_at is not None:
            d["created_at"] = _format_datetime(self.created_at)
        if self.completed_at is not None:
            d["completed_at"] = _format_datetime(self.completed_at)
        if self.metadata:
            d["metadata"] = dict(self.metadata)
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "Dag":
        return cls(
            id=data["id"],
            name=data["name"],
            state=data.get("state", "pending"),
            allocations=data.get("allocations", []),
            edges=data.get("edges", []),
            tenant_id=data.get("tenant_id", ""),
            user_id=data.get("user_id", ""),
            created_at=_parse_datetime(data.get("created_at")),
            completed_at=_parse_datetime(data.get("completed_at")),
            metadata=data.get("metadata", {}),
        )


@dataclass
class AuditEntry:
    """Audit log entry for sensitive workload operations."""
    id: str
    timestamp: datetime
    action: str
    user_id: str
    resource_type: str
    resource_id: str
    details: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        return {
            "id": self.id,
            "timestamp": _format_datetime(self.timestamp),
            "action": self.action,
            "user_id": self.user_id,
            "resource_type": self.resource_type,
            "resource_id": self.resource_id,
            "details": dict(self.details),
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "AuditEntry":
        return cls(
            id=data["id"],
            timestamp=_parse_datetime(data["timestamp"]),
            action=data["action"],
            user_id=data["user_id"],
            resource_type=data["resource_type"],
            resource_id=data["resource_id"],
            details=data.get("details", {}),
        )


@dataclass
class AccountingUsage:
    """Resource usage accounting data for a tenant."""
    tenant_id: str
    period_start: Optional[datetime] = None
    period_end: Optional[datetime] = None
    cpu_hours: float = 0.0
    gpu_hours: float = 0.0
    memory_gb_hours: float = 0.0
    storage_gb_hours: float = 0.0
    total_allocations: int = 0
    metadata: Dict[str, str] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        d: Dict[str, Any] = {
            "tenant_id": self.tenant_id,
            "cpu_hours": self.cpu_hours,
            "gpu_hours": self.gpu_hours,
            "memory_gb_hours": self.memory_gb_hours,
            "storage_gb_hours": self.storage_gb_hours,
            "total_allocations": self.total_allocations,
        }
        if self.period_start is not None:
            d["period_start"] = _format_datetime(self.period_start)
        if self.period_end is not None:
            d["period_end"] = _format_datetime(self.period_end)
        if self.metadata:
            d["metadata"] = dict(self.metadata)
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "AccountingUsage":
        return cls(
            tenant_id=data["tenant_id"],
            period_start=_parse_datetime(data.get("period_start")),
            period_end=_parse_datetime(data.get("period_end")),
            cpu_hours=float(data.get("cpu_hours", 0.0)),
            gpu_hours=float(data.get("gpu_hours", 0.0)),
            memory_gb_hours=float(data.get("memory_gb_hours", 0.0)),
            storage_gb_hours=float(data.get("storage_gb_hours", 0.0)),
            total_allocations=int(data.get("total_allocations", 0)),
            metadata=data.get("metadata", {}),
        )


@dataclass
class RaftStatus:
    """Raft consensus cluster status."""
    node_id: str
    role: str
    term: int
    leader_id: Optional[str] = None
    members: List[str] = field(default_factory=list)
    commit_index: int = 0
    last_applied: int = 0
    metadata: Dict[str, str] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        d: Dict[str, Any] = {
            "node_id": self.node_id,
            "role": self.role,
            "term": self.term,
            "commit_index": self.commit_index,
            "last_applied": self.last_applied,
        }
        if self.leader_id is not None:
            d["leader_id"] = self.leader_id
        if self.members:
            d["members"] = list(self.members)
        if self.metadata:
            d["metadata"] = dict(self.metadata)
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "RaftStatus":
        return cls(
            node_id=data["node_id"],
            role=data["role"],
            term=int(data["term"]),
            leader_id=data.get("leader_id"),
            members=data.get("members", []),
            commit_index=int(data.get("commit_index", 0)),
            last_applied=int(data.get("last_applied", 0)),
            metadata=data.get("metadata", {}),
        )


@dataclass
class BackupResult:
    """Result of a backup operation (create, verify, or restore)."""
    success: bool
    message: str
    backup_id: Optional[str] = None
    timestamp: Optional[datetime] = None
    metadata: Dict[str, str] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        d: Dict[str, Any] = {
            "success": self.success,
            "message": self.message,
        }
        if self.backup_id is not None:
            d["backup_id"] = self.backup_id
        if self.timestamp is not None:
            d["timestamp"] = _format_datetime(self.timestamp)
        if self.metadata:
            d["metadata"] = dict(self.metadata)
        return d

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "BackupResult":
        return cls(
            success=data.get("success", True),
            message=data.get("message", ""),
            backup_id=data.get("backup_id"),
            timestamp=_parse_datetime(data.get("timestamp")),
            metadata=data.get("metadata", {}),
        )


@dataclass
class DiagnosticsReport:
    """Diagnostics report for an allocation."""
    allocation_id: str
    state: str
    node_assignments: List[str] = field(default_factory=list)
    scheduler_decisions: List[Dict[str, Any]] = field(default_factory=list)
    resource_utilization: Dict[str, float] = field(default_factory=dict)
    warnings: List[str] = field(default_factory=list)
    metadata: Dict[str, str] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        return {
            "allocation_id": self.allocation_id,
            "state": self.state,
            "node_assignments": list(self.node_assignments),
            "scheduler_decisions": list(self.scheduler_decisions),
            "resource_utilization": dict(self.resource_utilization),
            "warnings": list(self.warnings),
            "metadata": dict(self.metadata),
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "DiagnosticsReport":
        return cls(
            allocation_id=data["allocation_id"],
            state=data.get("state", ""),
            node_assignments=data.get("node_assignments", []),
            scheduler_decisions=data.get("scheduler_decisions", []),
            resource_utilization=data.get("resource_utilization", {}),
            warnings=data.get("warnings", []),
            metadata=data.get("metadata", {}),
        )


@dataclass
class QueueInfo:
    """Queue information for a vCluster."""
    vcluster_name: str
    pending_count: int = 0
    running_count: int = 0
    allocations: List[Dict[str, Any]] = field(default_factory=list)
    metadata: Dict[str, str] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        return {
            "vcluster_name": self.vcluster_name,
            "pending_count": self.pending_count,
            "running_count": self.running_count,
            "allocations": list(self.allocations),
            "metadata": dict(self.metadata),
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "QueueInfo":
        return cls(
            vcluster_name=data["vcluster_name"],
            pending_count=int(data.get("pending_count", 0)),
            running_count=int(data.get("running_count", 0)),
            allocations=data.get("allocations", []),
            metadata=data.get("metadata", {}),
        )
