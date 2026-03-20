//! Contract tests — verify integration-surface contracts from specs/architecture/.
//!
//! These tests verify that module boundaries enforce the contracts defined in the
//! architecture specs. Each test cites its source invariant and interface specification.
//!
//! Organization:
//! - Interface contracts: consensus.md (QuorumClient, NodeRegistry, AllocationStore, AuditLog)
//! - Data contracts: shared-kernel.md, global-state.md (round-trip, validation)
//! - Invariant contracts: invariants.md × enforcement-map.md
//! - Event contracts: event-catalog.md (command schema, response types)
//! - Failure contracts: failure-modes.md (degradation behavior)

use chrono::Utc;
use uuid::Uuid;

use lattice_common::error::LatticeError;
use lattice_common::traits::{
    audit_actions, lattice_audit_event, AllocationStore, AuditEntry, AuditFilter, AuditLog,
    NodeRegistry,
};
use lattice_common::types::*;
use lattice_quorum::commands::{Command, CommandResponse};
use lattice_quorum::global_state::GlobalState;
use lattice_test_harness::fixtures::*;

// ═══════════════════════════════════════════════════════════════════
// INTERFACE CONTRACTS — consensus.md
// ═══════════════════════════════════════════════════════════════════

mod consensus_interface {
    use super::*;

    // Contract: consensus.md § QuorumClient — propose returns committed state
    #[tokio::test]
    async fn propose_returns_committed_response() {
        let client = lattice_quorum::create_test_quorum().await.unwrap();
        let alloc = AllocationBuilder::new().build();
        let id = alloc.id;

        let resp = client
            .propose(Command::SubmitAllocation(alloc))
            .await
            .unwrap();
        assert!(
            matches!(resp, CommandResponse::AllocationId(aid) if aid == id),
            "propose must return committed AllocationId"
        );
    }

    // Contract: consensus.md § AllocationStore — insert + get round-trip
    #[tokio::test]
    async fn allocation_store_insert_get_roundtrip() {
        let client = lattice_quorum::create_test_quorum().await.unwrap();
        let alloc = AllocationBuilder::new().tenant("roundtrip-t").build();
        let id = alloc.id;

        client.insert(alloc.clone()).await.unwrap();
        let retrieved = client.get(&id).await.unwrap();
        assert_eq!(retrieved.id, id);
        assert_eq!(retrieved.tenant, "roundtrip-t");
    }

    // Contract: consensus.md § AllocationStore — get returns AllocationNotFound for unknown ID
    #[tokio::test]
    async fn allocation_store_get_not_found() {
        let client = lattice_quorum::create_test_quorum().await.unwrap();
        let result = client.get(&Uuid::new_v4()).await;
        assert!(
            matches!(result, Err(LatticeError::AllocationNotFound(_))),
            "get on unknown ID must return AllocationNotFound"
        );
    }

    // Contract: consensus.md § NodeRegistry — register + get round-trip via propose
    #[tokio::test]
    async fn node_registry_register_get_roundtrip() {
        let client = lattice_quorum::create_test_quorum().await.unwrap();
        let node = NodeBuilder::new().id("contract-node-0").build();

        client.propose(Command::RegisterNode(node)).await.unwrap();
        let retrieved = client
            .get_node(&"contract-node-0".to_string())
            .await
            .unwrap();
        assert_eq!(retrieved.id, "contract-node-0");
    }

    // Contract: consensus.md § NodeRegistry — get_node returns NodeNotFound
    #[tokio::test]
    async fn node_registry_get_not_found() {
        let client = lattice_quorum::create_test_quorum().await.unwrap();
        let result = client.get_node(&"nonexistent-node".to_string()).await;
        assert!(
            matches!(result, Err(LatticeError::NodeNotFound(_))),
            "get_node on unknown ID must return NodeNotFound"
        );
    }

    // Contract: consensus.md § AuditLog — record is append-only, query returns recorded entries
    #[tokio::test]
    async fn audit_log_record_and_query() {
        let client = lattice_quorum::create_test_quorum().await.unwrap();
        let entry = AuditEntry::new(lattice_audit_event(
            audit_actions::NODE_CLAIM,
            "contract-user",
            hpc_audit::AuditScope::default(),
            hpc_audit::AuditOutcome::Success,
            "contract test",
            serde_json::json!({"test": "contract"}),
            hpc_audit::AuditSource::LatticeQuorum,
        ));

        client.record(entry).await.unwrap();
        let results = client
            .query(&AuditFilter {
                principal: Some("contract-user".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event.principal.identity, "contract-user");
    }

    // Contract: consensus.md § NodeRegistry — list_nodes returns registered nodes
    #[tokio::test]
    async fn node_registry_list_nodes() {
        use lattice_common::traits::NodeFilter;
        let client = lattice_quorum::create_test_quorum().await.unwrap();
        let node = NodeBuilder::new().id("list-node").build();
        client.propose(Command::RegisterNode(node)).await.unwrap();

        let nodes = client.list_nodes(&NodeFilter::default()).await.unwrap();
        assert!(!nodes.is_empty());
    }
}

// ═══════════════════════════════════════════════════════════════════
// DATA CONTRACTS — shared-kernel.md, global-state.md
// ═══════════════════════════════════════════════════════════════════

mod data_contracts {
    use super::*;

    // Contract: shared-kernel.md § Allocation — serialization round-trip
    #[test]
    fn allocation_serde_roundtrip() {
        let alloc = AllocationBuilder::new()
            .tenant("serde-t")
            .preemption_class(5)
            .sensitive()
            .build();
        let json = serde_json::to_string(&alloc).unwrap();
        let deserialized: Allocation = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, alloc.id);
        assert_eq!(deserialized.tenant, "serde-t");
        assert_eq!(deserialized.lifecycle.preemption_class, 5);
        assert!(deserialized.sensitive);
    }

    // Contract: shared-kernel.md § Node — serialization round-trip
    #[test]
    fn node_serde_roundtrip() {
        let node = NodeBuilder::new()
            .id("serde-node")
            .gpu_type("MI300X")
            .gpu_count(8)
            .build();
        let json = serde_json::to_string(&node).unwrap();
        let deserialized: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "serde-node");
        assert_eq!(
            deserialized.capabilities.gpu_type.as_deref(),
            Some("MI300X")
        );
        assert_eq!(deserialized.capabilities.gpu_count, 8);
    }

    // Contract: shared-kernel.md § Tenant — serialization round-trip
    #[test]
    fn tenant_serde_roundtrip() {
        let tenant = TenantBuilder::new("serde-tenant")
            .max_nodes(50)
            .gpu_hours(1000.0)
            .build();
        let json = serde_json::to_string(&tenant).unwrap();
        let deserialized: Tenant = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "serde-tenant");
        assert_eq!(deserialized.quota.max_nodes, 50);
    }

    // Contract: shared-kernel.md § Command — serde round-trip for all command variants
    #[test]
    fn command_serde_roundtrip() {
        let commands = vec![
            Command::RegisterNode(NodeBuilder::new().id("cmd-node").build()),
            Command::UpdateNodeState {
                id: "n1".into(),
                state: NodeState::Draining,
                reason: Some("test".into()),
            },
            Command::ClaimNode {
                id: "n1".into(),
                ownership: NodeOwnership {
                    tenant: "t1".into(),
                    vcluster: "vc1".into(),
                    allocation: Uuid::new_v4(),
                    claimed_by: Some("user".into()),
                    is_borrowed: false,
                },
            },
            Command::ReleaseNode { id: "n1".into() },
            Command::RecordHeartbeat {
                id: "n1".into(),
                timestamp: Utc::now(),
                owner_version: 42,
            },
            Command::CreateNetworkDomain {
                tenant: "t1".into(),
                name: "domain".into(),
            },
            Command::ReleaseNetworkDomain {
                tenant: "t1".into(),
                name: "domain".into(),
            },
            Command::SetSensitivePoolSize(Some(10)),
        ];

        for cmd in commands {
            let json = serde_json::to_string(&cmd).unwrap();
            let deserialized: Command = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(json, json2, "Command serde round-trip failed for: {cmd}");
        }
    }

    // Contract: shared-kernel.md § CommandResponse — serde round-trip
    #[test]
    fn command_response_serde_roundtrip() {
        let responses = vec![
            CommandResponse::Ok,
            CommandResponse::AllocationId(Uuid::new_v4()),
            CommandResponse::NodeId("n1".into()),
            CommandResponse::TenantId("t1".into()),
            CommandResponse::VClusterId("vc1".into()),
            CommandResponse::Error("test error".into()),
        ];

        for resp in responses {
            let json = serde_json::to_string(&resp).unwrap();
            let deserialized: CommandResponse = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(
                json, json2,
                "CommandResponse serde round-trip failed for: {resp}"
            );
        }
    }

    // Contract: global-state.md § GlobalState — serde round-trip preserves all fields
    #[test]
    fn global_state_serde_roundtrip() {
        let mut state = GlobalState::new();
        let node = NodeBuilder::new().id("gs-node").build();
        state.apply(Command::RegisterNode(node));
        let alloc = AllocationBuilder::new().tenant("gs-tenant").build();
        state.apply(Command::SubmitAllocation(alloc));
        let tenant = TenantBuilder::new("gs-tenant").build();
        state.apply(Command::CreateTenant(tenant));
        state.apply(Command::CreateNetworkDomain {
            tenant: "gs-tenant".into(),
            name: "gs-domain".into(),
        });

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: GlobalState = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.nodes.len(), state.nodes.len());
        assert_eq!(deserialized.allocations.len(), state.allocations.len());
        assert_eq!(deserialized.tenants.len(), state.tenants.len());
        assert_eq!(
            deserialized.network_domains.len(),
            state.network_domains.len()
        );
        assert_eq!(deserialized.state_version, state.state_version);
    }

    // Contract: global-state.md § Command Processing — deterministic application
    #[test]
    fn command_application_is_deterministic() {
        let node = NodeBuilder::new().id("det-node").build();
        let alloc = AllocationBuilder::new().tenant("det-t").build();
        let alloc_clone = alloc.clone();

        let mut state1 = GlobalState::new();
        state1.apply(Command::RegisterNode(node.clone()));
        state1.apply(Command::SubmitAllocation(alloc));

        let mut state2 = GlobalState::new();
        state2.apply(Command::RegisterNode(node));
        state2.apply(Command::SubmitAllocation(alloc_clone));

        assert_eq!(state1.nodes.len(), state2.nodes.len());
        assert_eq!(state1.allocations.len(), state2.allocations.len());
        assert_eq!(state1.state_version, state2.state_version);
    }

    // Contract: shared-kernel.md § Allocation State Machine — valid transitions
    #[test]
    fn allocation_state_machine_valid_transitions() {
        let mut state = GlobalState::new();
        let alloc = AllocationBuilder::new().build();
        let id = alloc.id;
        state.apply(Command::SubmitAllocation(alloc));

        assert!(matches!(
            state.apply(Command::UpdateAllocationState {
                id,
                state: AllocationState::Running,
                message: None,
                exit_code: None,
            }),
            CommandResponse::Ok
        ));
        assert!(matches!(
            state.apply(Command::UpdateAllocationState {
                id,
                state: AllocationState::Completed,
                message: None,
                exit_code: Some(0),
            }),
            CommandResponse::Ok
        ));
    }

    // Contract: shared-kernel.md § Allocation State Machine — invalid transitions rejected
    #[test]
    fn allocation_state_machine_invalid_transitions() {
        let mut state = GlobalState::new();
        let alloc = AllocationBuilder::new().build();
        let id = alloc.id;
        state.apply(Command::SubmitAllocation(alloc));

        // Pending → Completed (must go through Running)
        let resp = state.apply(Command::UpdateAllocationState {
            id,
            state: AllocationState::Completed,
            message: None,
            exit_code: None,
        });
        assert!(
            matches!(resp, CommandResponse::Error(_)),
            "Pending → Completed should be rejected"
        );
    }

    // Contract: shared-kernel.md § Node State Machine — valid transitions
    #[test]
    fn node_state_machine_valid_transitions() {
        let mut state = GlobalState::new();
        state.apply(Command::RegisterNode(NodeBuilder::new().id("nsm").build()));

        assert!(matches!(
            state.apply(Command::UpdateNodeState {
                id: "nsm".into(),
                state: NodeState::Draining,
                reason: None,
            }),
            CommandResponse::Ok
        ));
        assert!(matches!(
            state.apply(Command::UpdateNodeState {
                id: "nsm".into(),
                state: NodeState::Drained,
                reason: None,
            }),
            CommandResponse::Ok
        ));
    }

    // Contract: shared-kernel.md § Node State Machine — invalid transitions rejected
    #[test]
    fn node_state_machine_invalid_transitions() {
        let mut state = GlobalState::new();
        state.apply(Command::RegisterNode(
            NodeBuilder::new().id("nsm-inv").build(),
        ));

        let resp = state.apply(Command::UpdateNodeState {
            id: "nsm-inv".into(),
            state: NodeState::Drained,
            reason: None,
        });
        assert!(
            matches!(resp, CommandResponse::Error(_)),
            "Ready → Drained should be rejected"
        );
    }

    // Contract: shared-kernel.md § AuditEntry — hash chain fields populated by state machine
    #[test]
    fn audit_entry_hash_chain_populated_on_apply() {
        let mut state = GlobalState::new();
        let entry = AuditEntry::new(lattice_audit_event(
            audit_actions::NODE_CLAIM,
            "hash-test",
            hpc_audit::AuditScope::default(),
            hpc_audit::AuditOutcome::Success,
            "hash test",
            serde_json::json!({}),
            hpc_audit::AuditSource::LatticeQuorum,
        ));
        state.apply(Command::RecordAudit(entry));

        assert!(
            !state.audit_log[0].signature.is_empty(),
            "RecordAudit must populate signature field"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
// INVARIANT CONTRACTS — invariants.md × enforcement-map.md
// ═══════════════════════════════════════════════════════════════════

mod invariant_contracts {
    use super::*;

    // Contract: INV-S1 (Exclusive Node Ownership)
    #[test]
    fn inv_s1_exclusive_node_ownership() {
        let mut state = GlobalState::new();
        state.apply(Command::RegisterNode(
            NodeBuilder::new().id("inv-s1").build(),
        ));

        let own1 = NodeOwnership {
            tenant: "t1".into(),
            vcluster: "vc1".into(),
            allocation: Uuid::new_v4(),
            claimed_by: Some("user-1".into()),
            is_borrowed: false,
        };
        state.apply(Command::ClaimNode {
            id: "inv-s1".into(),
            ownership: own1,
        });

        let own2 = NodeOwnership {
            tenant: "t2".into(),
            vcluster: "vc2".into(),
            allocation: Uuid::new_v4(),
            claimed_by: Some("user-2".into()),
            is_borrowed: false,
        };
        let resp = state.apply(Command::ClaimNode {
            id: "inv-s1".into(),
            ownership: own2,
        });
        assert!(
            matches!(resp, CommandResponse::Error(e) if e.contains("already claimed")),
            "INV-S1: must reject second claim on already-claimed node"
        );
    }

    // Contract: INV-S2 (Hard Quota Non-Violation)
    #[test]
    fn inv_s2_hard_quota_on_submit() {
        let mut state = GlobalState::new();
        let tenant = TenantBuilder::new("inv-s2-t").max_nodes(1).build();
        state.apply(Command::CreateTenant(tenant));

        let mut alloc = AllocationBuilder::new().tenant("inv-s2-t").build();
        alloc.state = AllocationState::Running;
        alloc.assigned_nodes = vec!["n1".into()];
        state.allocations.insert(alloc.id, alloc);

        let alloc2 = AllocationBuilder::new().tenant("inv-s2-t").build();
        let resp = state.apply(Command::SubmitAllocation(alloc2));
        assert!(
            matches!(resp, CommandResponse::Error(e) if e.contains("max_nodes")),
            "INV-S2: must reject submission that would exceed max_nodes quota"
        );
    }

    // Contract: INV-S2 — max_concurrent_allocations
    #[test]
    fn inv_s2_concurrent_allocation_limit() {
        let mut state = GlobalState::new();
        let tenant = TenantBuilder::new("inv-s2-conc")
            .max_nodes(100)
            .max_concurrent(1)
            .build();
        state.apply(Command::CreateTenant(tenant));
        state.apply(Command::SubmitAllocation(
            AllocationBuilder::new().tenant("inv-s2-conc").build(),
        ));

        let resp = state.apply(Command::SubmitAllocation(
            AllocationBuilder::new().tenant("inv-s2-conc").build(),
        ));
        assert!(
            matches!(resp, CommandResponse::Error(e) if e.contains("max_concurrent")),
            "INV-S2: must enforce max_concurrent_allocations"
        );
    }

    // Contract: INV-S4 (Sensitive Audit Immutability) — append-only, hash chain integrity
    #[test]
    fn inv_s4_audit_log_append_only_with_hash_chain() {
        let mut state = GlobalState::new();
        for i in 0..5 {
            let entry = AuditEntry::new(lattice_audit_event(
                audit_actions::NODE_CLAIM,
                &format!("user-{i}"),
                hpc_audit::AuditScope::default(),
                hpc_audit::AuditOutcome::Success,
                "hash chain test",
                serde_json::json!({"i": i}),
                hpc_audit::AuditSource::LatticeQuorum,
            ));
            state.apply(Command::RecordAudit(entry));
        }
        assert_eq!(state.audit_log.len(), 5);

        // Verify hash chain integrity — each entry after the first has a non-empty previous_hash
        assert!(state.audit_log[0].previous_hash.is_empty());
        for i in 1..5 {
            assert!(
                !state.audit_log[i].previous_hash.is_empty(),
                "Entry {i} must have a previous_hash"
            );
            assert!(
                !state.audit_log[i].signature.is_empty(),
                "Entry {i} must have a signature"
            );
        }
    }

    // Contract: INV-S6 (Sensitive Wipe Before Reuse) — release clears ownership, bumps version
    #[test]
    fn inv_s6_release_clears_ownership_and_bumps_version() {
        let mut state = GlobalState::new();
        state.apply(Command::RegisterNode(
            NodeBuilder::new().id("inv-s6").build(),
        ));

        let ownership = NodeOwnership {
            tenant: "t1".into(),
            vcluster: "vc1".into(),
            allocation: Uuid::new_v4(),
            claimed_by: Some("user".into()),
            is_borrowed: false,
        };
        state.apply(Command::ClaimNode {
            id: "inv-s6".into(),
            ownership,
        });
        assert_eq!(state.nodes["inv-s6"].owner_version, 1);

        state.apply(Command::ReleaseNode {
            id: "inv-s6".into(),
        });
        assert!(state.nodes["inv-s6"].owner.is_none());
        assert_eq!(state.nodes["inv-s6"].owner_version, 2);
    }

    // Contract: INV-C3 (VNI Uniqueness)
    #[test]
    fn inv_c3_vni_uniqueness() {
        let mut state = GlobalState::new();

        state.apply(Command::CreateNetworkDomain {
            tenant: "t1".into(),
            name: "dom-a".into(),
        });
        state.apply(Command::CreateNetworkDomain {
            tenant: "t1".into(),
            name: "dom-b".into(),
        });
        state.apply(Command::CreateNetworkDomain {
            tenant: "t2".into(),
            name: "dom-a".into(),
        });

        let vnis: Vec<u32> = state.network_domains.values().map(|d| d.vni).collect();
        let unique: std::collections::HashSet<u32> = vnis.iter().copied().collect();
        assert_eq!(
            vnis.len(),
            unique.len(),
            "INV-C3: all active domains must have unique VNIs"
        );
    }

    // Contract: INV-C3 — VNI returned to pool on release
    #[test]
    fn inv_c3_vni_released_on_domain_teardown() {
        let mut state = GlobalState::new();
        state.apply(Command::CreateNetworkDomain {
            tenant: "t1".into(),
            name: "reuse-dom".into(),
        });
        let vni = state.network_domains["t1/reuse-dom"].vni;
        assert!(state.vni_pool.allocated.contains_key(&vni));

        state.apply(Command::ReleaseNetworkDomain {
            tenant: "t1".into(),
            name: "reuse-dom".into(),
        });
        assert!(!state.vni_pool.allocated.contains_key(&vni));
    }

    // Contract: INV-E5 (Network Domain Tenant Scoping)
    #[test]
    fn inv_e5_network_domain_tenant_scoped() {
        let mut state = GlobalState::new();

        state.apply(Command::CreateNetworkDomain {
            tenant: "tenant-a".into(),
            name: "shared-name".into(),
        });
        state.apply(Command::CreateNetworkDomain {
            tenant: "tenant-b".into(),
            name: "shared-name".into(),
        });

        assert!(state.network_domains.contains_key("tenant-a/shared-name"));
        assert!(state.network_domains.contains_key("tenant-b/shared-name"));

        let vni_a = state.network_domains["tenant-a/shared-name"].vni;
        let vni_b = state.network_domains["tenant-b/shared-name"].vni;
        assert_ne!(vni_a, vni_b);
    }

    // Contract: ADV-04 (Stale-Read Race)
    #[test]
    fn adv_04_stale_proposal_rejected() {
        let mut state = GlobalState::new();
        let alloc = AllocationBuilder::new().build();
        let id = alloc.id;
        state.apply(Command::SubmitAllocation(alloc));
        let version_at_read = state.state_version;

        state.apply(Command::RegisterNode(
            NodeBuilder::new().id("interloper").build(),
        ));

        let resp = state.apply(Command::AssignNodes {
            id,
            nodes: vec!["interloper".into()],
            expected_version: Some(version_at_read),
        });
        assert!(matches!(resp, CommandResponse::Error(e) if e.contains("Stale")));
    }

    // Contract: ADV-04 — None expected_version always passes
    #[test]
    fn adv_04_no_version_check_when_none() {
        let mut state = GlobalState::new();
        let alloc = AllocationBuilder::new().build();
        let id = alloc.id;
        state.apply(Command::SubmitAllocation(alloc));
        state.apply(Command::RegisterNode(
            NodeBuilder::new().id("no-ver").build(),
        ));

        let resp = state.apply(Command::AssignNodes {
            id,
            nodes: vec!["no-ver".into()],
            expected_version: None,
        });
        assert!(matches!(resp, CommandResponse::Ok));
    }

    // Contract: ADV-06 (Heartbeat/Claim Race)
    #[test]
    fn adv_06_heartbeat_owner_version_enforced() {
        let mut state = GlobalState::new();
        state.apply(Command::RegisterNode(
            NodeBuilder::new().id("adv06").build(),
        ));

        // Initial heartbeat at version 0 works
        assert!(matches!(
            state.apply(Command::RecordHeartbeat {
                id: "adv06".into(),
                timestamp: Utc::now(),
                owner_version: 0,
            }),
            CommandResponse::Ok
        ));

        // Claim bumps to version 1
        state.apply(Command::ClaimNode {
            id: "adv06".into(),
            ownership: NodeOwnership {
                tenant: "t".into(),
                vcluster: "vc".into(),
                allocation: Uuid::new_v4(),
                claimed_by: Some("u".into()),
                is_borrowed: false,
            },
        });

        // Heartbeat with old version 0 rejected
        assert!(matches!(
            state.apply(Command::RecordHeartbeat {
                id: "adv06".into(),
                timestamp: Utc::now(),
                owner_version: 0,
            }),
            CommandResponse::Error(e) if e.contains("Stale heartbeat")
        ));

        // Heartbeat with current version 1 accepted
        assert!(matches!(
            state.apply(Command::RecordHeartbeat {
                id: "adv06".into(),
                timestamp: Utc::now(),
                owner_version: 1,
            }),
            CommandResponse::Ok
        ));
    }

    // Contract: ADV-08 — VNI pool exhaustion
    #[test]
    fn adv_08_vni_pool_exhaustion() {
        let mut state = GlobalState::new();
        state.vni_pool = lattice_quorum::global_state::VniPool {
            start: 100,
            end: 101,
            allocated: std::collections::HashMap::new(),
            next_candidate: 100,
        };

        state.apply(Command::CreateNetworkDomain {
            tenant: "t1".into(),
            name: "d1".into(),
        });
        state.apply(Command::CreateNetworkDomain {
            tenant: "t1".into(),
            name: "d2".into(),
        });

        let resp = state.apply(Command::CreateNetworkDomain {
            tenant: "t1".into(),
            name: "d3".into(),
        });
        assert!(matches!(resp, CommandResponse::Error(e) if e.contains("VNI pool exhausted")));
    }

    // Contract: ADV-09 — duplicate domain rejected
    #[test]
    fn adv_09_duplicate_domain_rejected() {
        let mut state = GlobalState::new();
        state.apply(Command::CreateNetworkDomain {
            tenant: "t1".into(),
            name: "dup".into(),
        });

        let resp = state.apply(Command::CreateNetworkDomain {
            tenant: "t1".into(),
            name: "dup".into(),
        });
        assert!(matches!(resp, CommandResponse::Error(e) if e.contains("already exists")));
    }
}

// ═══════════════════════════════════════════════════════════════════
// FAILURE CONTRACTS — failure-modes.md
// ═══════════════════════════════════════════════════════════════════

mod failure_contracts {
    use super::*;

    // Contract: failed commands don't corrupt state
    #[test]
    fn failed_command_does_not_corrupt_state() {
        let mut state = GlobalState::new();
        let initial_version = state.state_version;

        let resp = state.apply(Command::UpdateNodeState {
            id: "nonexistent".into(),
            state: NodeState::Degraded {
                reason: "test".into(),
            },
            reason: None,
        });
        assert!(matches!(resp, CommandResponse::Error(_)));
        assert_eq!(state.state_version, initial_version);
        assert!(state.nodes.is_empty());
    }

    // Contract: error-taxonomy.md — operations on nonexistent entities return errors
    #[test]
    fn operations_on_nonexistent_entities_return_errors() {
        let mut state = GlobalState::new();

        assert!(matches!(
            state.apply(Command::ClaimNode {
                id: "missing".into(),
                ownership: NodeOwnership {
                    tenant: "t".into(),
                    vcluster: "vc".into(),
                    allocation: Uuid::new_v4(),
                    claimed_by: None,
                    is_borrowed: false,
                },
            }),
            CommandResponse::Error(e) if e.contains("not found")
        ));

        assert!(matches!(
            state.apply(Command::ReleaseNode {
                id: "missing".into()
            }),
            CommandResponse::Error(e) if e.contains("not found")
        ));

        assert!(matches!(
            state.apply(Command::RecordHeartbeat {
                id: "missing".into(),
                timestamp: Utc::now(),
                owner_version: 0,
            }),
            CommandResponse::Error(e) if e.contains("not found")
        ));

        assert!(matches!(
            state.apply(Command::UpdateAllocationState {
                id: Uuid::new_v4(),
                state: AllocationState::Running,
                message: None,
                exit_code: None,
            }),
            CommandResponse::Error(e) if e.contains("not found")
        ));

        assert!(matches!(
            state.apply(Command::ReleaseNetworkDomain {
                tenant: "t".into(),
                name: "missing".into(),
            }),
            CommandResponse::Error(e) if e.contains("not found")
        ));

        assert!(matches!(
            state.apply(Command::UpdateTenant {
                id: "missing".into(),
                quota: None,
                isolation_level: None,
            }),
            CommandResponse::Error(e) if e.contains("not found")
        ));

        assert!(matches!(
            state.apply(Command::UpdateVCluster {
                id: "missing".into(),
                cost_weights: None,
                allow_borrowing: None,
                allow_lending: None,
            }),
            CommandResponse::Error(e) if e.contains("not found")
        ));
    }

    // Contract: FM-06 — state is queryable after mutations (stateless restart reads from quorum)
    #[tokio::test]
    async fn state_queryable_after_mutations() {
        use lattice_common::traits::NodeFilter;
        let client = lattice_quorum::create_test_quorum().await.unwrap();

        let alloc = AllocationBuilder::new().tenant("query-t").build();
        let id = alloc.id;
        client.insert(alloc).await.unwrap();
        client
            .propose(Command::RegisterNode(
                NodeBuilder::new().id("query-node").build(),
            ))
            .await
            .unwrap();

        let alloc = client.get(&id).await.unwrap();
        assert_eq!(alloc.tenant, "query-t");

        let node = client.get_node(&"query-node".to_string()).await.unwrap();
        assert_eq!(node.id, "query-node");

        let nodes = client.list_nodes(&NodeFilter::default()).await.unwrap();
        assert!(!nodes.is_empty());
    }

    // Contract: INV-N4 — all invalid operations produce Error, never Ok
    #[test]
    fn no_silent_failure_on_invalid_operations() {
        let mut state = GlobalState::new();

        let invalid_ops = vec![
            Command::UpdateNodeState {
                id: "x".into(),
                state: NodeState::Degraded {
                    reason: "test".into(),
                },
                reason: None,
            },
            Command::ReleaseNode { id: "x".into() },
            Command::RecordHeartbeat {
                id: "x".into(),
                timestamp: Utc::now(),
                owner_version: 0,
            },
            Command::UpdateAllocationState {
                id: Uuid::new_v4(),
                state: AllocationState::Running,
                message: None,
                exit_code: None,
            },
            Command::ReleaseNetworkDomain {
                tenant: "x".into(),
                name: "x".into(),
            },
            Command::UpdateTenant {
                id: "x".into(),
                quota: None,
                isolation_level: None,
            },
            Command::UpdateVCluster {
                id: "x".into(),
                cost_weights: None,
                allow_borrowing: None,
                allow_lending: None,
            },
        ];

        for cmd in invalid_ops {
            let resp = state.apply(cmd.clone());
            assert!(
                matches!(resp, CommandResponse::Error(_)),
                "INV-N4: invalid command {cmd} must return Error"
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// EVENT CONTRACTS — event-catalog.md
// ═══════════════════════════════════════════════════════════════════

mod event_contracts {
    use super::*;

    // Contract: every command variant produces a response (no panic)
    #[test]
    fn every_command_variant_produces_response() {
        let mut state = GlobalState::new();

        state.apply(Command::RegisterNode(
            NodeBuilder::new().id("evt-n").build(),
        ));
        let alloc = AllocationBuilder::new().build();
        let alloc_id = alloc.id;
        state.apply(Command::SubmitAllocation(alloc));
        let tenant = TenantBuilder::new("evt-t").build();
        state.apply(Command::CreateTenant(tenant));
        let vc = VClusterBuilder::new("evt-vc").build();
        state.apply(Command::CreateVCluster(vc));

        let commands: Vec<Command> = vec![
            Command::SubmitAllocation(AllocationBuilder::new().build()),
            Command::UpdateAllocationState {
                id: alloc_id,
                state: AllocationState::Running,
                message: None,
                exit_code: None,
            },
            Command::AssignNodes {
                id: alloc_id,
                nodes: vec!["evt-n".into()],
                expected_version: None,
            },
            Command::RegisterNode(NodeBuilder::new().id("evt-n2").build()),
            Command::UpdateNodeState {
                id: "evt-n".into(),
                state: NodeState::Draining,
                reason: None,
            },
            Command::ClaimNode {
                id: "evt-n2".into(),
                ownership: NodeOwnership {
                    tenant: "evt-t".into(),
                    vcluster: "evt-vc".into(),
                    allocation: Uuid::new_v4(),
                    claimed_by: None,
                    is_borrowed: false,
                },
            },
            Command::RecordHeartbeat {
                id: "evt-n".into(),
                timestamp: Utc::now(),
                owner_version: 0,
            },
            Command::CreateNetworkDomain {
                tenant: "evt-t".into(),
                name: "evt-dom".into(),
            },
            Command::SetSensitivePoolSize(Some(5)),
            Command::UpdateTopology(TopologyModel { groups: vec![] }),
            Command::RecordAudit(AuditEntry::new(lattice_audit_event(
                audit_actions::NODE_CLAIM,
                "evt-user",
                hpc_audit::AuditScope::default(),
                hpc_audit::AuditOutcome::Success,
                "event test",
                serde_json::json!({}),
                hpc_audit::AuditSource::LatticeQuorum,
            ))),
            Command::UpdateTenant {
                id: "evt-t".into(),
                quota: None,
                isolation_level: None,
            },
            Command::UpdateVCluster {
                id: "evt-vc".into(),
                cost_weights: None,
                allow_borrowing: Some(false),
                allow_lending: None,
            },
        ];

        for cmd in commands {
            let _resp = state.apply(cmd);
            // Verifying no panic
        }
    }

    // Contract: event-catalog.md § AuditEntry — serialization round-trip with new structure
    #[test]
    fn audit_entry_serde_roundtrip() {
        let actions = [
            audit_actions::NODE_CLAIM,
            audit_actions::NODE_RELEASE,
            audit_actions::ALLOCATION_START,
            audit_actions::ALLOCATION_COMPLETE,
            audit_actions::DATA_ACCESS,
            audit_actions::ATTACH_SESSION,
            audit_actions::LOG_ACCESS,
            audit_actions::METRICS_QUERY,
            audit_actions::CHECKPOINT_TRIGGERED,
            audit_actions::WIPE_STARTED,
            audit_actions::WIPE_COMPLETED,
            audit_actions::WIPE_FAILED,
        ];

        for action in actions {
            let entry = AuditEntry::new(lattice_audit_event(
                action,
                "serde-user",
                hpc_audit::AuditScope::default(),
                hpc_audit::AuditOutcome::Success,
                "serde test",
                serde_json::json!({}),
                hpc_audit::AuditSource::LatticeQuorum,
            ));
            let json = serde_json::to_string(&entry).unwrap();
            let deserialized: AuditEntry = serde_json::from_str(&json).unwrap();
            assert_eq!(
                entry.event.action, deserialized.event.action,
                "AuditEntry action must round-trip for {action}"
            );
        }
    }
}
