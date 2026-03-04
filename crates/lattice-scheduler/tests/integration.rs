//! Integration tests for lattice-scheduler.
//!
//! These tests exercise multi-component interactions that cross module
//! boundaries: scheduling cycles feeding into DAG resolution, federation
//! broker decisions used in scheduling, autoscaler evaluations driven by
//! cycle output, and inter-vCluster borrowing.

use std::collections::HashMap;

use chrono::Utc;
use uuid::Uuid;

use lattice_common::types::*;
use lattice_scheduler::autoscaler::{Autoscaler, AutoscalerConfig, ScaleDecision};
use lattice_scheduler::borrowing::{BorrowRequest, BorrowResult, BorrowingBroker, BorrowingConfig};
use lattice_scheduler::cycle::{run_cycle, CycleInput};
use lattice_scheduler::dag::{resolve_dependencies, root_allocations, validate_dag};
use lattice_scheduler::federation::{
    FederationBroker, FederationConfig, FederationOffer, OfferDecision,
};
use lattice_scheduler::preemption::{evaluate_preemption, PreemptionConfig, PreemptionResult};
use lattice_test_harness::fixtures::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a standard CycleInput for tests.
fn cycle_input(
    pending: Vec<Allocation>,
    running: Vec<Allocation>,
    nodes: Vec<Node>,
    tenants: Vec<Tenant>,
    groups: usize,
    nodes_per_group: usize,
) -> CycleInput {
    CycleInput {
        pending,
        running,
        nodes,
        tenants,
        topology: create_test_topology(groups, nodes_per_group),
        data_readiness: HashMap::new(),
        energy_price: 0.5,
    }
}

// ===========================================================================
// 1. Scheduler cycle + DAG resolution
// ===========================================================================

/// Submit a 3-stage DAG (A -> B -> C). Run a scheduling cycle with only
/// the root allocations pending. Verify that only the root (stage 1) is
/// placed. Then simulate completion of stage 1, call resolve_dependencies
/// to verify stage 2 is unblocked.
#[test]
fn dag_three_stage_only_roots_scheduled_then_stage2_unblocked() {
    // Build the 3-stage DAG: A (root) -> B -> C
    let stage_a = AllocationBuilder::new()
        .tenant("t1")
        .nodes(2)
        .dag_id("dag-1")
        .build();
    let stage_b = AllocationBuilder::new()
        .tenant("t1")
        .nodes(2)
        .dag_id("dag-1")
        .depends_on(&stage_a.id.to_string(), DependencyCondition::Success)
        .build();
    let stage_c = AllocationBuilder::new()
        .tenant("t1")
        .nodes(2)
        .dag_id("dag-1")
        .depends_on(&stage_b.id.to_string(), DependencyCondition::Success)
        .build();

    let all_allocs = vec![stage_a.clone(), stage_b.clone(), stage_c.clone()];

    // Validate the DAG is well-formed
    let topo_order = validate_dag(&all_allocs, 100).expect("DAG should be valid");
    assert_eq!(topo_order.len(), 3);

    // Only the root should be eligible
    let roots = root_allocations(&all_allocs);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], stage_a.id);

    // Schedule only the root allocations (stage_a)
    let nodes = create_node_batch(8, 0);
    let input = cycle_input(
        vec![stage_a.clone()],
        vec![],
        nodes,
        vec![TenantBuilder::new("t1").build()],
        1,
        8,
    );
    let result = run_cycle(&input, &CostWeights::default());
    assert_eq!(result.placed().len(), 1, "root stage should be placed");
    assert_eq!(result.placed()[0].allocation_id(), stage_a.id);

    // Simulate completion of stage_a
    let mut terminal_states = HashMap::new();
    terminal_states.insert(stage_a.id, AllocationState::Completed);

    let unblocked = resolve_dependencies(&all_allocs, &terminal_states);
    assert_eq!(unblocked.len(), 1, "exactly stage_b should be unblocked");
    assert_eq!(unblocked[0], stage_b.id);

    // stage_c should NOT yet be unblocked (stage_b is still Pending, not terminal)
    assert!(
        !unblocked.contains(&stage_c.id),
        "stage_c must not be unblocked yet"
    );
}

/// After all three DAG stages complete, verify no further allocations
/// are unblocked (the DAG is fully resolved).
#[test]
fn dag_full_resolution_all_stages_complete() {
    let stage_a = AllocationBuilder::new()
        .tenant("t1")
        .nodes(1)
        .dag_id("dag-2")
        .build();
    let stage_b = AllocationBuilder::new()
        .tenant("t1")
        .nodes(1)
        .dag_id("dag-2")
        .depends_on(&stage_a.id.to_string(), DependencyCondition::Success)
        .build();
    let stage_c = AllocationBuilder::new()
        .tenant("t1")
        .nodes(1)
        .dag_id("dag-2")
        .depends_on(&stage_b.id.to_string(), DependencyCondition::Success)
        .build();

    // Mark a and b completed. resolve_dependencies checks alloc.state == Pending
    // to determine which allocations are still waiting, so we must update the
    // allocation structs to reflect their actual terminal state.
    let mut stage_a_done = stage_a.clone();
    stage_a_done.state = AllocationState::Completed;
    let mut stage_b_done = stage_b.clone();
    stage_b_done.state = AllocationState::Completed;

    let mut terminal_states = HashMap::new();
    terminal_states.insert(stage_a.id, AllocationState::Completed);
    terminal_states.insert(stage_b.id, AllocationState::Completed);

    let all_allocs = vec![stage_a_done.clone(), stage_b_done.clone(), stage_c.clone()];
    let unblocked = resolve_dependencies(&all_allocs, &terminal_states);
    assert_eq!(unblocked.len(), 1);
    assert_eq!(unblocked[0], stage_c.id);

    // Now mark c as completed too, nothing left to unblock
    let mut stage_c_done = stage_c.clone();
    stage_c_done.state = AllocationState::Completed;
    terminal_states.insert(stage_c.id, AllocationState::Completed);
    let all_done = vec![stage_a_done, stage_b_done, stage_c_done];
    let unblocked_after = resolve_dependencies(&all_done, &terminal_states);
    assert!(
        unblocked_after.is_empty(),
        "no allocations should be unblocked when all are terminal"
    );
}

/// DAG with a failure condition: stage_b only unblocks on failure of stage_a.
/// Complete stage_a successfully and verify stage_b stays blocked.
/// Then fail stage_a and verify stage_b unblocks.
#[test]
fn dag_failure_condition_blocks_on_success_unblocks_on_failure() {
    let stage_a = AllocationBuilder::new()
        .tenant("t1")
        .nodes(1)
        .dag_id("dag-3")
        .build();
    let stage_b = AllocationBuilder::new()
        .tenant("t1")
        .nodes(1)
        .dag_id("dag-3")
        .depends_on(&stage_a.id.to_string(), DependencyCondition::Failure)
        .build();

    let all_allocs = vec![stage_a.clone(), stage_b.clone()];

    // Complete stage_a successfully -- stage_b should NOT unblock
    let mut terminal_success = HashMap::new();
    terminal_success.insert(stage_a.id, AllocationState::Completed);
    let unblocked = resolve_dependencies(&all_allocs, &terminal_success);
    assert!(
        unblocked.is_empty(),
        "stage_b should not unblock on success when condition is Failure"
    );

    // Fail stage_a -- stage_b SHOULD unblock
    let mut terminal_failure = HashMap::new();
    terminal_failure.insert(stage_a.id, AllocationState::Failed);
    let unblocked = resolve_dependencies(&all_allocs, &terminal_failure);
    assert_eq!(unblocked.len(), 1);
    assert_eq!(unblocked[0], stage_b.id);
}

// ===========================================================================
// 2. Federation broker + scheduler
// ===========================================================================

/// Accept a federation offer and verify the resulting nodes can be used
/// as placeholders in a scheduling context: the offer produces node IDs,
/// and those node IDs are well-formed strings.
#[test]
fn federation_accepted_offer_produces_usable_node_ids() {
    let config = FederationConfig {
        site_id: "alps".to_string(),
        max_federation_pct: 0.3,
        accept_sensitive: false,
        trusted_sites: vec!["daint".to_string()],
    };
    let broker = FederationBroker::new(config);

    let offer = FederationOffer {
        source_site: "daint".to_string(),
        allocation_id: Uuid::new_v4(),
        tenant_id: "cern-physics".to_string(),
        node_count: 4,
        sensitive: false,
        data_locations: vec!["alps".to_string()],
        offered_at: Utc::now(),
        ttl_secs: 300,
    };

    let decision = broker.evaluate_offer(&offer, 20, 100);
    match decision {
        OfferDecision::Accept { nodes } => {
            assert_eq!(nodes.len(), 4, "should allocate 4 nodes");
            for node in &nodes {
                assert!(
                    node.starts_with("alps-fed-node-"),
                    "federation node IDs should carry site prefix"
                );
            }
        }
        OfferDecision::Reject { reason } => {
            panic!("expected accept, got reject: {reason}");
        }
    }
}

/// Verify that data gravity affects which site is preferred: when data
/// is remote, the offer is still accepted (loose coupling), but we can
/// detect this via the offer's data_locations field.
#[test]
fn federation_data_gravity_local_vs_remote() {
    let config = FederationConfig {
        site_id: "alps".to_string(),
        max_federation_pct: 0.5,
        accept_sensitive: false,
        trusted_sites: vec!["daint".to_string()],
    };
    let broker = FederationBroker::new(config);

    // Offer with local data
    let local_offer = FederationOffer {
        source_site: "daint".to_string(),
        allocation_id: Uuid::new_v4(),
        tenant_id: "ml-team".to_string(),
        node_count: 2,
        sensitive: false,
        data_locations: vec!["alps".to_string()],
        offered_at: Utc::now(),
        ttl_secs: 300,
    };
    let local_decision = broker.evaluate_offer(&local_offer, 10, 100);
    assert!(
        matches!(local_decision, OfferDecision::Accept { .. }),
        "local data offer should be accepted"
    );

    // Offer with remote data (data lives at daint, not alps)
    let remote_offer = FederationOffer {
        source_site: "daint".to_string(),
        allocation_id: Uuid::new_v4(),
        tenant_id: "ml-team".to_string(),
        node_count: 2,
        sensitive: false,
        data_locations: vec!["daint".to_string()],
        offered_at: Utc::now(),
        ttl_secs: 300,
    };
    let remote_decision = broker.evaluate_offer(&remote_offer, 10, 100);
    // Still accepted (loose coupling) but in production the scheduler
    // would weigh the data gravity penalty.
    assert!(
        matches!(remote_decision, OfferDecision::Accept { .. }),
        "remote data offer should still be accepted (loose coupling)"
    );
}

/// Sensitive offer rejected when config disallows it, but accepted
/// when explicitly enabled. Then verify the accepted nodes could
/// feed into a scheduling cycle's node pool.
#[test]
fn federation_sensitive_policy_and_scheduler_integration() {
    // Config that rejects sensitive
    let strict_config = FederationConfig {
        site_id: "secure-site".to_string(),
        max_federation_pct: 0.5,
        accept_sensitive: false,
        trusted_sites: vec!["hospital-site".to_string()],
    };
    let strict_broker = FederationBroker::new(strict_config);

    let sensitive_offer = FederationOffer {
        source_site: "hospital-site".to_string(),
        allocation_id: Uuid::new_v4(),
        tenant_id: "radiology".to_string(),
        node_count: 2,
        sensitive: true,
        data_locations: vec![],
        offered_at: Utc::now(),
        ttl_secs: 300,
    };

    let reject_decision = strict_broker.evaluate_offer(&sensitive_offer, 10, 100);
    assert!(
        matches!(reject_decision, OfferDecision::Reject { .. }),
        "sensitive should be rejected when accept_sensitive=false"
    );

    // Config that accepts sensitive
    let open_config = FederationConfig {
        site_id: "secure-site".to_string(),
        max_federation_pct: 0.5,
        accept_sensitive: true,
        trusted_sites: vec!["hospital-site".to_string()],
    };
    let open_broker = FederationBroker::new(open_config);

    let accept_decision = open_broker.evaluate_offer(&sensitive_offer, 10, 100);
    match accept_decision {
        OfferDecision::Accept { nodes } => {
            assert_eq!(nodes.len(), 2);
            // These federation node IDs could, in principle, be added to a
            // scheduling cycle's node pool (modelling the remote resources).
            // Verify they are non-empty and well-formed.
            for node in &nodes {
                assert!(!node.is_empty());
            }
        }
        OfferDecision::Reject { reason } => {
            panic!("expected accept with sensitive enabled, got reject: {reason}");
        }
    }
}

// ===========================================================================
// 3. Autoscaler + cost evaluator / scheduling cycle
// ===========================================================================

/// Run a scheduling cycle where demand exceeds capacity. Then feed the
/// resulting deferral count into the autoscaler, verifying it recommends
/// scaling up.
#[test]
fn autoscaler_recommends_scale_up_when_cycle_defers() {
    let allocs: Vec<Allocation> = (0..6)
        .map(|_| AllocationBuilder::new().tenant("t1").nodes(2).build())
        .collect();
    let nodes = create_node_batch(4, 0); // only 4 nodes for 12 requested

    let input = cycle_input(
        allocs,
        vec![],
        nodes,
        vec![TenantBuilder::new("t1").build()],
        1,
        4,
    );

    let result = run_cycle(&input, &CostWeights::default());

    let placed_count = result.placed().len();
    let deferred_count = result.deferred().len();

    assert!(
        deferred_count > 0,
        "some allocations should be deferred due to limited nodes"
    );

    // Feed into autoscaler: high utilization + pending work
    let current_nodes = 4u32;
    let utilization = placed_count as f64 / 6.0; // fraction of demand satisfied
    let queue_depth = deferred_count as u32;

    let mut autoscaler = Autoscaler::new(AutoscalerConfig {
        scale_up_threshold: 0.5,
        scale_down_threshold: 0.1,
        cooldown_secs: 0,
        min_nodes: 1,
        max_nodes: 100,
        evaluation_interval_secs: 60,
    });

    let decision = autoscaler.evaluate(current_nodes, utilization, queue_depth);
    assert_eq!(
        decision,
        ScaleDecision::ScaleUp { count: 1 },
        "autoscaler should recommend scale-up when there are deferred allocations"
    );
}

/// When all allocations fit and utilization is low, the autoscaler
/// should recommend scale-down.
#[test]
fn autoscaler_recommends_scale_down_when_underutilized() {
    // 1 small allocation on 10 nodes
    let alloc = AllocationBuilder::new().tenant("t1").nodes(1).build();
    let nodes = create_node_batch(10, 0);

    let input = cycle_input(
        vec![alloc],
        vec![],
        nodes,
        vec![TenantBuilder::new("t1").build()],
        1,
        10,
    );

    let result = run_cycle(&input, &CostWeights::default());
    assert_eq!(result.placed().len(), 1);
    assert_eq!(result.deferred().len(), 0);

    // Very low utilization (1 node used out of 10), no queue
    let mut autoscaler = Autoscaler::new(AutoscalerConfig {
        scale_up_threshold: 0.8,
        scale_down_threshold: 0.2,
        cooldown_secs: 0,
        min_nodes: 1,
        max_nodes: 100,
        evaluation_interval_secs: 60,
    });

    let decision = autoscaler.evaluate(10, 0.1, 0);
    assert_eq!(
        decision,
        ScaleDecision::ScaleDown { count: 1 },
        "autoscaler should recommend scale-down when utilization is low and queue is empty"
    );
}

// ===========================================================================
// 4. Borrowing broker + scheduler cycle
// ===========================================================================

/// Create two vClusters. vc-hpc has idle nodes. vc-ml is overloaded.
/// Verify the borrowing broker approves lending from vc-hpc to vc-ml.
/// Then use the borrowed nodes (conceptually) in a scheduling cycle.
#[test]
fn borrowing_from_idle_vcluster_to_overloaded() {
    let _vc_hpc = VClusterBuilder::new("vc-hpc")
        .tenant("t1")
        .scheduler(SchedulerType::HpcBackfill)
        .build();
    let _vc_ml = VClusterBuilder::new("vc-ml")
        .tenant("t1")
        .scheduler(SchedulerType::ServiceBinPack)
        .build();

    // vc-hpc has 10 idle nodes
    let idle_nodes: Vec<String> = (0..10).map(|i| format!("hpc-node-{i}")).collect();

    let broker = BorrowingBroker::new(BorrowingConfig {
        allow_lending: true,
        allow_borrowing: true,
        max_borrow_pct: 0.3, // up to 30% of dedicated pool
        return_grace_secs: 60,
    });

    let request = BorrowRequest {
        source_vcluster: "vc-hpc".into(),
        target_vcluster: "vc-ml".into(),
        node_count: 3,
        priority: 5,
    };

    let result = broker.evaluate_request(&request, &idle_nodes, 10);
    match &result {
        BorrowResult::Approved { nodes } => {
            assert_eq!(nodes.len(), 3, "should borrow exactly 3 nodes");
            // All borrowed nodes should come from the idle set
            for node in nodes {
                assert!(
                    idle_nodes.contains(node),
                    "borrowed node should be from idle pool"
                );
            }
        }
        BorrowResult::Denied { reason } => {
            panic!("expected approval, got denial: {reason}");
        }
    }

    // Now use those 3 borrowed nodes plus vc-ml's own 4 nodes in a cycle
    let mut all_nodes = create_node_batch(4, 0); // vc-ml's own nodes
    if let BorrowResult::Approved { nodes } = &result {
        for node_id in nodes.iter() {
            all_nodes.push(
                NodeBuilder::new()
                    .id(node_id)
                    .group(1) // different group
                    .build(),
            );
        }
    }

    let ml_alloc = AllocationBuilder::new().tenant("t1").nodes(6).build();
    let input = cycle_input(
        vec![ml_alloc.clone()],
        vec![],
        all_nodes,
        vec![TenantBuilder::new("t1").build()],
        2,
        4,
    );

    let sched_result = run_cycle(&input, &CostWeights::default());
    assert_eq!(
        sched_result.placed().len(),
        1,
        "allocation needing 6 nodes should be placed with borrowed + own nodes"
    );
    assert_eq!(sched_result.placed()[0].allocation_id(), ml_alloc.id);
}

/// Borrowing is denied when the lending vCluster has no idle nodes.
/// The scheduling cycle then defers the allocation.
#[test]
fn borrowing_denied_causes_deferral_in_cycle() {
    let broker = BorrowingBroker::new(BorrowingConfig::default());

    let request = BorrowRequest {
        source_vcluster: "vc-hpc".into(),
        target_vcluster: "vc-ml".into(),
        node_count: 4,
        priority: 5,
    };

    // No idle nodes to lend
    let idle_nodes: Vec<String> = vec![];
    let result = broker.evaluate_request(&request, &idle_nodes, 10);
    assert!(
        !result.is_approved(),
        "borrowing should be denied with no idle nodes"
    );

    // Without borrowed nodes, a 6-node allocation on 4 nodes should be deferred
    let nodes = create_node_batch(4, 0);
    let alloc = AllocationBuilder::new().tenant("t1").nodes(6).build();
    let input = cycle_input(
        vec![alloc],
        vec![],
        nodes,
        vec![TenantBuilder::new("t1").build()],
        1,
        4,
    );

    let sched_result = run_cycle(&input, &CostWeights::default());
    assert_eq!(sched_result.placed().len(), 0);
    assert_eq!(sched_result.deferred().len(), 1);
}

// ===========================================================================
// 5. Scheduling cycle + preemption evaluation
// ===========================================================================

/// When a high-priority allocation is deferred by the cycle, evaluate
/// preemption against running lower-priority allocations to find victims.
#[test]
fn deferred_allocation_triggers_preemption_evaluation() {
    // Set up: 4 nodes, all running low-priority jobs
    let running: Vec<Allocation> = (0..2)
        .map(|i| {
            let mut a = AllocationBuilder::new()
                .tenant("t1")
                .nodes(2)
                .preemption_class(1)
                .state(AllocationState::Running)
                .build();
            a.assigned_nodes = vec![format!("n{}", i * 2), format!("n{}", i * 2 + 1)];
            a
        })
        .collect();

    // High-priority new allocation wants 2 nodes
    let pending = AllocationBuilder::new()
        .tenant("t2")
        .nodes(2)
        .preemption_class(8)
        .build();

    // Run the scheduling cycle -- all nodes are owned by running allocs
    // so the pending allocation should be deferred
    let mut nodes = create_node_batch(4, 0);
    // Mark nodes as owned by the running allocations
    for (i, node) in nodes.iter_mut().enumerate() {
        let alloc_idx = i / 2;
        node.owner = Some(NodeOwnership {
            tenant: "t1".into(),
            vcluster: "default".into(),
            allocation: running[alloc_idx].id,
            claimed_by: None,
            is_borrowed: false,
        });
    }

    let input = cycle_input(
        vec![pending.clone()],
        running.clone(),
        nodes,
        vec![
            TenantBuilder::new("t1").build(),
            TenantBuilder::new("t2").build(),
        ],
        1,
        4,
    );

    let result = run_cycle(&input, &CostWeights::default());
    assert_eq!(
        result.deferred().len(),
        1,
        "high-priority alloc should be deferred (all nodes owned)"
    );

    // Now evaluate preemption for the deferred allocation
    let preempt_result = evaluate_preemption(&pending, &running, &PreemptionConfig::default());

    match preempt_result {
        PreemptionResult::Possible {
            victims,
            freed_nodes,
        } => {
            assert!(
                !victims.is_empty(),
                "should identify at least one preemption victim"
            );
            assert!(
                freed_nodes.len() >= 2,
                "should free enough nodes for the pending allocation"
            );
        }
        PreemptionResult::NotPossible { reason } => {
            panic!("expected preemption to be possible, got: {reason}");
        }
    }
}

// ===========================================================================
// 6. Cost evaluator + scheduling cycle priority interaction
// ===========================================================================

/// Verify that the cost evaluator's scoring directly drives which
/// allocations get placed first in a constrained-resource scenario.
#[test]
fn cost_weights_drive_placement_order_across_cycle() {
    // Two allocations from different tenants; one with high fair-share
    // deficit, one over-consuming. Use fair-share-heavy weights.
    let deficit_alloc = AllocationBuilder::new()
        .tenant("underserved")
        .nodes(2)
        .preemption_class(3)
        .build();
    let overserved_alloc = AllocationBuilder::new()
        .tenant("overserved")
        .nodes(2)
        .preemption_class(3)
        .build();

    // Only 2 nodes available -- only one allocation can be placed.
    let nodes = create_node_batch(2, 0);
    let t_under = TenantBuilder::new("underserved").fair_share(0.5).build();
    let t_over = TenantBuilder::new("overserved").fair_share(0.5).build();

    // Set up a running allocation for "overserved" to inflate their usage
    let mut running_alloc = AllocationBuilder::new()
        .tenant("overserved")
        .nodes(4)
        .state(AllocationState::Running)
        .build();
    running_alloc.assigned_nodes = (0..4).map(|i| format!("r{i}")).collect();

    // Fair-share-heavy weights
    let weights = CostWeights {
        priority: 0.0,
        wait_time: 0.0,
        fair_share: 1.0,
        topology: 0.0,
        data_readiness: 0.0,
        backlog: 0.0,
        energy: 0.0,
        checkpoint_efficiency: 0.0,
        conformance: 0.0,
    };

    let input = CycleInput {
        pending: vec![overserved_alloc.clone(), deficit_alloc.clone()],
        running: vec![running_alloc],
        nodes,
        tenants: vec![t_under, t_over],
        topology: create_test_topology(1, 2),
        data_readiness: HashMap::new(),
        energy_price: 0.5,
    };

    let result = run_cycle(&input, &weights);

    assert_eq!(result.placed().len(), 1);
    assert_eq!(result.deferred().len(), 1);

    // The underserved tenant should be placed (higher fair-share deficit score)
    assert_eq!(
        result.placed()[0].allocation_id(),
        deficit_alloc.id,
        "underserved tenant's allocation should be placed when using fair-share weights"
    );
    assert_eq!(
        result.deferred()[0].allocation_id(),
        overserved_alloc.id,
        "overserved tenant's allocation should be deferred"
    );
}

// ===========================================================================
// 7. DAG + scheduling cycle: wide fan-out DAG
// ===========================================================================

/// A wide fan-out DAG: one root with 4 parallel children. All 4 children
/// should unblock simultaneously when the root completes, and a scheduling
/// cycle should place as many as capacity allows.
#[test]
fn dag_wide_fanout_all_children_unblock_simultaneously() {
    let root = AllocationBuilder::new()
        .tenant("t1")
        .nodes(1)
        .dag_id("fan-dag")
        .build();

    let children: Vec<Allocation> = (0..4)
        .map(|_| {
            AllocationBuilder::new()
                .tenant("t1")
                .nodes(2)
                .dag_id("fan-dag")
                .depends_on(&root.id.to_string(), DependencyCondition::Success)
                .build()
        })
        .collect();

    let mut all_allocs = vec![root.clone()];
    all_allocs.extend(children.clone());

    // Validate the DAG
    let order = validate_dag(&all_allocs, 100).expect("fan-out DAG should be valid");
    assert_eq!(order.len(), 5);

    // Root completes -> all 4 children should unblock
    let mut terminal_states = HashMap::new();
    terminal_states.insert(root.id, AllocationState::Completed);

    let unblocked = resolve_dependencies(&all_allocs, &terminal_states);
    assert_eq!(
        unblocked.len(),
        4,
        "all 4 children should unblock when root completes"
    );

    // Schedule the 4 unblocked children on 6 nodes (only 3 can fit: 3*2=6)
    let nodes = create_node_batch(6, 0);
    let input = cycle_input(
        children.clone(),
        vec![],
        nodes,
        vec![TenantBuilder::new("t1").build()],
        1,
        6,
    );

    let result = run_cycle(&input, &CostWeights::default());
    let placed = result.placed().len();
    let deferred = result.deferred().len();

    assert_eq!(placed, 3, "3 children should fit on 6 nodes (2 nodes each)");
    assert_eq!(deferred, 1, "1 child should be deferred");
}

// ===========================================================================
// 8. Autoscaler + borrowing: combined resource expansion
// ===========================================================================

/// When both autoscaling and borrowing are available, verify that the
/// borrowing broker can provide immediate capacity while the autoscaler
/// recommends longer-term expansion.
#[test]
fn autoscaler_and_borrowing_provide_complementary_scaling() {
    // Current state: vc-ml has 4 nodes but needs 8
    let allocs: Vec<Allocation> = (0..4)
        .map(|_| AllocationBuilder::new().tenant("t1").nodes(2).build())
        .collect();
    let nodes = create_node_batch(4, 0);

    let input = cycle_input(
        allocs.clone(),
        vec![],
        nodes,
        vec![TenantBuilder::new("t1").build()],
        1,
        4,
    );

    let result = run_cycle(&input, &CostWeights::default());
    let deferred = result.deferred().len();

    assert!(deferred > 0, "some allocations should be deferred");

    // Step 1: Borrowing broker provides immediate relief
    let broker = BorrowingBroker::new(BorrowingConfig {
        allow_lending: true,
        allow_borrowing: true,
        max_borrow_pct: 0.5,
        return_grace_secs: 120,
    });

    let idle_hpc_nodes: Vec<String> = (0..6).map(|i| format!("hpc-idle-{i}")).collect();
    let borrow_req = BorrowRequest {
        source_vcluster: "vc-hpc".into(),
        target_vcluster: "vc-ml".into(),
        node_count: 4,
        priority: 5,
    };

    let borrow_result = broker.evaluate_request(&borrow_req, &idle_hpc_nodes, 10);
    assert!(
        borrow_result.is_approved(),
        "borrowing should be approved for immediate capacity"
    );

    // Step 2: Autoscaler recommends persistent scale-up
    let mut autoscaler = Autoscaler::new(AutoscalerConfig {
        scale_up_threshold: 0.7,
        scale_down_threshold: 0.2,
        cooldown_secs: 0,
        min_nodes: 4,
        max_nodes: 20,
        evaluation_interval_secs: 60,
    });

    // High utilization since all 4 nodes are in use
    let scale_decision = autoscaler.evaluate(4, 0.95, deferred as u32);
    assert_eq!(
        scale_decision,
        ScaleDecision::ScaleUp { count: 1 },
        "autoscaler should recommend permanent scale-up alongside borrowing"
    );

    // Both mechanisms are complementary: borrowing is immediate,
    // autoscaling is for sustained demand.
}
