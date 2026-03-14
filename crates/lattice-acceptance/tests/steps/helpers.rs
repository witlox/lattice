use chrono::Duration;
use lattice_api::middleware::rbac::{Operation, Role};
use lattice_common::traits::AuditAction;
use lattice_common::types::*;

pub fn parse_allocation_state(s: &str) -> AllocationState {
    match s {
        "Pending" => AllocationState::Pending,
        "Staging" => AllocationState::Staging,
        "Running" => AllocationState::Running,
        "Checkpointing" => AllocationState::Checkpointing,
        "Suspended" => AllocationState::Suspended,
        "Completed" => AllocationState::Completed,
        "Failed" => AllocationState::Failed,
        "Cancelled" => AllocationState::Cancelled,
        other => panic!("Unknown allocation state: {other}"),
    }
}

pub fn parse_audit_action(s: &str) -> AuditAction {
    match s {
        "NodeClaim" => AuditAction::NodeClaim,
        "NodeRelease" => AuditAction::NodeRelease,
        "AllocationStart" => AuditAction::AllocationStart,
        "AllocationComplete" => AuditAction::AllocationComplete,
        "DataAccess" => AuditAction::DataAccess,
        "AttachSession" => AuditAction::AttachSession,
        "LogAccess" => AuditAction::LogAccess,
        "MetricsQuery" => AuditAction::MetricsQuery,
        "CheckpointEvent" => AuditAction::CheckpointEvent,
        other => panic!("Unknown audit action: {other}"),
    }
}

pub fn parse_role(s: &str) -> Role {
    match s {
        "User" => Role::User,
        "TenantAdmin" => Role::TenantAdmin,
        "SystemAdmin" => Role::SystemAdmin,
        "ClaimingUser" => Role::ClaimingUser,
        "Operator" => Role::Operator,
        "ReadOnly" => Role::ReadOnly,
        other => panic!("Unknown role: {other}"),
    }
}

pub fn parse_operation(s: &str) -> Operation {
    match s {
        "SubmitAllocation" => Operation::SubmitAllocation,
        "GetAllocation" => Operation::GetAllocation,
        "ListAllocations" => Operation::ListAllocations,
        "CancelAllocation" => Operation::CancelAllocation,
        "UpdateAllocation" => Operation::UpdateAllocation,
        "WatchAllocation" => Operation::WatchAllocation,
        "StreamLogs" => Operation::StreamLogs,
        "QueryMetrics" => Operation::QueryMetrics,
        "StreamMetrics" => Operation::StreamMetrics,
        "GetDiagnostics" => Operation::GetDiagnostics,
        "CreateTenant" => Operation::CreateTenant,
        "UpdateTenant" => Operation::UpdateTenant,
        "CreateVCluster" => Operation::CreateVCluster,
        "UpdateVCluster" => Operation::UpdateVCluster,
        "DrainNode" => Operation::DrainNode,
        "UndrainNode" => Operation::UndrainNode,
        "DisableNode" => Operation::DisableNode,
        "ListNodes" => Operation::ListNodes,
        "GetNode" => Operation::GetNode,
        "ClaimNode" => Operation::ClaimNode,
        "ReleaseNode" => Operation::ReleaseNode,
        "GetRaftStatus" => Operation::GetRaftStatus,
        "BackupVerify" => Operation::BackupVerify,
        "BackupExport" => Operation::BackupExport,
        "QueryAudit" => Operation::QueryAudit,
        "FederationManage" => Operation::FederationManage,
        other => panic!("Unknown operation: {other}"),
    }
}

pub fn parse_scheduler_type(s: &str) -> SchedulerType {
    match s {
        "hpc_backfill" | "HpcBackfill" => SchedulerType::HpcBackfill,
        "service_bin_pack" | "ServiceBinPack" => SchedulerType::ServiceBinPack,
        "sensitive_reservation" | "SensitiveReservation" => SchedulerType::SensitiveReservation,
        "interactive_fifo" | "InteractiveFifo" => SchedulerType::InteractiveFifo,
        other => panic!("Unknown scheduler type: {other}"),
    }
}

pub fn make_dram_domain(id: u32, capacity_bytes: u64, numa: u32) -> MemoryDomain {
    MemoryDomain {
        id,
        domain_type: MemoryDomainType::Dram,
        capacity_bytes,
        numa_node: Some(numa),
        attached_cpus: vec![numa * 2, numa * 2 + 1],
        attached_gpus: Vec::new(),
    }
}

/// Parse simple duration strings like "1h", "2h", "10m", "30s"
pub fn parse_duration_str(s: &str) -> Duration {
    if let Some(h) = s.strip_suffix('h') {
        Duration::hours(h.parse::<i64>().unwrap())
    } else if let Some(m) = s.strip_suffix('m') {
        Duration::minutes(m.parse::<i64>().unwrap())
    } else if let Some(sec) = s.strip_suffix('s') {
        Duration::seconds(sec.parse::<i64>().unwrap())
    } else {
        panic!("cannot parse duration: {s}")
    }
}
