//! Integration tests for lattice-common crate.
//!
//! Tests cover config, error, client stubs, and type validation
//! using only public API surface.

use lattice_common::clients::sovra::{SovraClient, SovraCredential, StubSovraClient};
use lattice_common::clients::waldur::{
    AccountingClient, AccountingEvent, InMemoryAccountingClient,
};
use lattice_common::config::*;
use lattice_common::error::LatticeError;
use lattice_common::types::*;

use chrono::{Duration, Utc};

// ─── Config Tests ───────────────────────────────────────────────

#[test]
fn default_lattice_config_has_sane_values() {
    let cfg = LatticeConfig::default();

    assert_eq!(cfg.api.grpc_address, "0.0.0.0:50051");
    assert_eq!(cfg.quorum.node_id, 1);
    assert_eq!(cfg.telemetry.default_mode, "prod");
    assert!(matches!(cfg.role, NodeRole::Combined));
}

#[test]
fn config_yaml_roundtrip() {
    let original = LatticeConfig::default();
    let yaml = serde_yaml::to_string(&original).expect("serialize to YAML");
    let restored: LatticeConfig = serde_yaml::from_str(&yaml).expect("deserialize from YAML");

    assert_eq!(restored.api.grpc_address, original.api.grpc_address);
    assert_eq!(restored.quorum.node_id, original.quorum.node_id);
    assert_eq!(
        restored.telemetry.prod_interval_seconds,
        original.telemetry.prod_interval_seconds
    );
    assert_eq!(
        restored.storage.nfs_home_path,
        original.storage.nfs_home_path
    );
    assert_eq!(
        restored.quorum.raft_listen_address,
        original.quorum.raft_listen_address
    );
}

#[test]
fn quorum_config_default_raft_listen_address() {
    let cfg = QuorumConfig::default();
    assert_eq!(cfg.raft_listen_address, "0.0.0.0:9000");
    assert_eq!(cfg.election_timeout_ms, 500);
    assert_eq!(cfg.heartbeat_interval_ms, 100);
}

#[test]
fn node_agent_config_default_sensitive_grace_period() {
    let cfg = NodeAgentConfig::default();
    assert_eq!(cfg.sensitive_grace_period_seconds, 600);
    assert_eq!(cfg.heartbeat_interval_seconds, 10);
    assert!(cfg.sensitive_grace_period_seconds > cfg.grace_period_seconds);
}

#[test]
fn optional_config_sections_default_to_none() {
    let cfg = LatticeConfig::default();
    assert!(cfg.federation.is_none());
    assert!(cfg.node_agent.is_none());
    assert!(cfg.network.is_none());
    assert!(cfg.checkpoint.is_none());
    assert!(cfg.scheduling.is_none());
    assert!(cfg.accounting.is_none());
    assert!(cfg.rate_limit.is_none());
    assert!(cfg.compat.is_none());
}

// ─── Error Tests ────────────────────────────────────────────────

#[test]
fn error_display_allocation_not_found() {
    let err = LatticeError::AllocationNotFound("abc-123".to_string());
    let msg = err.to_string();
    assert!(msg.contains("Allocation not found"));
    assert!(msg.contains("abc-123"));
}

#[test]
fn error_display_sensitive_isolation() {
    let err =
        LatticeError::SensitiveIsolation("node shared with non-sensitive workload".to_string());
    let msg = err.to_string();
    assert!(msg.contains("Sensitive isolation violation"));
    assert!(msg.contains("non-sensitive workload"));
}

#[test]
fn error_display_quota_exceeded_includes_tenant_and_detail() {
    let err = LatticeError::QuotaExceeded {
        tenant: "physics-dept".to_string(),
        detail: "GPU hours limit reached".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("physics-dept"), "missing tenant in: {msg}");
    assert!(
        msg.contains("GPU hours limit reached"),
        "missing detail in: {msg}"
    );
}

#[test]
fn error_display_ownership_conflict_includes_node_and_owner() {
    let err = LatticeError::OwnershipConflict {
        node: "x1000c0s0b0n0".to_string(),
        owner: "dr.smith".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("x1000c0s0b0n0"), "missing node in: {msg}");
    assert!(msg.contains("dr.smith"), "missing owner in: {msg}");
}

// ─── Client Stub Tests ─────────────────────────────────────────

#[tokio::test]
async fn stub_sovra_exchange_credentials_returns_valid_credential() {
    let client = StubSovraClient::new("site-alpha".to_string());
    let cred = client.exchange_credentials("site-beta").await.unwrap();

    assert_eq!(cred.site_id, "site-alpha");
    assert!(cred.token.contains("site-beta"));
    assert!(!cred.scopes.is_empty());
}

#[tokio::test]
async fn stub_sovra_verify_credential_always_succeeds() {
    let client = StubSovraClient::new("site-alpha".to_string());
    let cred = SovraCredential {
        site_id: "untrusted-site".to_string(),
        token: "arbitrary-token".to_string(),
        expires_at: 0,
        scopes: vec![],
    };
    let result = client.verify_credential(&cred).await.unwrap();
    assert!(result, "stub should always verify as true");
}

#[tokio::test]
async fn in_memory_accounting_submit_and_query_usage() {
    let client = InMemoryAccountingClient::new();
    let now = Utc::now();

    let event1 = AccountingEvent {
        allocation_id: uuid::Uuid::new_v4(),
        tenant_id: "physics".to_string(),
        resource_type: "gpu_hours".to_string(),
        amount: 10.0,
        period_start: now - Duration::hours(1),
        period_end: now,
    };
    let event2 = AccountingEvent {
        allocation_id: uuid::Uuid::new_v4(),
        tenant_id: "physics".to_string(),
        resource_type: "gpu_hours".to_string(),
        amount: 5.5,
        period_start: now - Duration::hours(1),
        period_end: now,
    };

    client.submit_event(event1).await.unwrap();
    client.submit_event(event2).await.unwrap();

    let total = client
        .query_usage(
            "physics",
            "gpu_hours",
            now - Duration::hours(2),
            now + Duration::hours(1),
        )
        .await
        .unwrap();
    assert!((total - 15.5).abs() < 0.001, "expected 15.5, got {total}");
}

#[tokio::test]
async fn in_memory_accounting_flush_and_events_tracking() {
    let client = InMemoryAccountingClient::new();
    let now = Utc::now();

    let event = AccountingEvent {
        allocation_id: uuid::Uuid::new_v4(),
        tenant_id: "biology".to_string(),
        resource_type: "node_hours".to_string(),
        amount: 24.0,
        period_start: now - Duration::hours(1),
        period_end: now,
    };

    client.submit_event(event).await.unwrap();

    // flush reports the count of buffered events
    let count = client.flush().await.unwrap();
    assert_eq!(count, 1);

    // events() accessor returns all stored events
    let events = client.events().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].tenant_id, "biology");
    assert!((events[0].amount - 24.0).abs() < 0.001);
}

// ─── Type Validation Tests ──────────────────────────────────────

#[test]
fn allocation_state_valid_transitions() {
    // Pending -> Running is valid
    assert!(AllocationState::Pending.can_transition_to(&AllocationState::Running));
    // Pending -> Staging is valid
    assert!(AllocationState::Pending.can_transition_to(&AllocationState::Staging));
    // Running -> Completed is valid
    assert!(AllocationState::Running.can_transition_to(&AllocationState::Completed));
    // Running -> Failed is valid
    assert!(AllocationState::Running.can_transition_to(&AllocationState::Failed));
}

#[test]
fn allocation_state_invalid_transitions() {
    // Completed is terminal, cannot transition to anything
    assert!(!AllocationState::Completed.can_transition_to(&AllocationState::Running));
    assert!(!AllocationState::Completed.can_transition_to(&AllocationState::Pending));
    // Failed is terminal
    assert!(!AllocationState::Failed.can_transition_to(&AllocationState::Running));
    // Cancelled is terminal
    assert!(!AllocationState::Cancelled.can_transition_to(&AllocationState::Pending));
    // Terminal states report correctly
    assert!(AllocationState::Completed.is_terminal());
    assert!(AllocationState::Failed.is_terminal());
    assert!(AllocationState::Cancelled.is_terminal());
    assert!(!AllocationState::Running.is_terminal());
}

#[test]
fn node_state_transitions_valid_and_invalid() {
    // Valid transitions
    assert!(NodeState::Unknown.can_transition_to(&NodeState::Booting));
    assert!(NodeState::Booting.can_transition_to(&NodeState::Ready));
    assert!(NodeState::Ready.can_transition_to(&NodeState::Draining));
    assert!(NodeState::Draining.can_transition_to(&NodeState::Drained));
    assert!(NodeState::Drained.can_transition_to(&NodeState::Ready));

    // Invalid transitions
    assert!(!NodeState::Draining.can_transition_to(&NodeState::Ready));
    assert!(!NodeState::Ready.can_transition_to(&NodeState::Unknown));
    assert!(!NodeState::Booting.can_transition_to(&NodeState::Draining));

    // Operational checks
    assert!(NodeState::Ready.is_operational());
    assert!(NodeState::Degraded {
        reason: "slow disk".into()
    }
    .is_operational());
    assert!(!NodeState::Draining.is_operational());
    assert!(!NodeState::Down {
        reason: "offline".into()
    }
    .is_operational());
}

#[test]
fn cost_weights_default_sums_to_one() {
    let w = CostWeights::default();
    let sum = w.priority
        + w.wait_time
        + w.fair_share
        + w.topology
        + w.data_readiness
        + w.backlog
        + w.energy
        + w.checkpoint_efficiency
        + w.conformance;
    assert!(
        (sum - 1.0).abs() < 1e-10,
        "default weights should sum to 1.0, got {sum}"
    );
}
