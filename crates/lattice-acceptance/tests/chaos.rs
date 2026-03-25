//! Chaos tests: verify scheduling invariants under fault conditions.
//!
//! These tests exercise the scheduler under adversarial conditions:
//! concurrent submissions, node failures mid-cycle, cancellations,
//! zero-resource scenarios, and fair-share contention.

use std::collections::{HashMap, HashSet};

use lattice_common::types::*;
use lattice_scheduler::cycle::{run_cycle, CycleInput};
use lattice_scheduler::knapsack::KnapsackSolver;
use lattice_scheduler::placement::PlacementDecision;
use lattice_test_harness::fixtures::*;

// ─── Helpers ───────────────────────────────────────────────

fn make_cycle_input(
    pending: Vec<Allocation>,
    running: Vec<Allocation>,
    nodes: Vec<Node>,
    tenants: Vec<Tenant>,
    topology: TopologyModel,
) -> CycleInput {
    CycleInput {
        pending,
        running,
        nodes,
        tenants,
        topology,
        data_readiness: HashMap::new(),
        energy_price: 0.5,
        timeline_config: lattice_scheduler::resource_timeline::TimelineConfig::default(),
        budget_utilization: HashMap::new(),
    }
}

// ─── Test: No double placement after concurrent submits ────

/// Submit many allocations concurrently (in a single cycle) and verify
/// no node is assigned to more than one allocation.
#[tokio::test]
async fn no_double_placement_after_concurrent_submits() {
    let num_allocs = 20;
    let num_nodes = 10;
    let nodes = create_node_batch(num_nodes, 0);
    let topology = create_test_topology(1, num_nodes);
    let tenant = TenantBuilder::new("t1").build();

    let allocs: Vec<Allocation> = (0..num_allocs)
        .map(|_| AllocationBuilder::new().tenant("t1").nodes(1).build())
        .collect();

    let input = make_cycle_input(allocs, vec![], nodes, vec![tenant], topology);

    let result = run_cycle(&input, &CostWeights::default());

    // Collect all assigned node IDs
    let mut assigned_nodes: HashSet<String> = HashSet::new();
    for decision in &result.decisions {
        if let PlacementDecision::Place { nodes, .. } = decision {
            for nid in nodes {
                assert!(
                    assigned_nodes.insert(nid.clone()),
                    "Node {} was assigned to multiple allocations",
                    nid
                );
            }
        }
    }

    // Some allocations should be placed, some deferred
    let placed_count = result.placed().len();
    let deferred_count = result.deferred().len();
    assert!(placed_count > 0, "Expected at least one placement");
    assert_eq!(
        placed_count + deferred_count,
        num_allocs,
        "All allocations should be either placed or deferred"
    );
    assert!(
        placed_count <= num_nodes,
        "Cannot place more allocations than nodes"
    );
}

// ─── Test: Node removal mid-cycle maintains consistency ────

/// Start with nodes, mark some as Down (simulating failure), and verify
/// no allocation is placed on a non-operational node.
#[tokio::test]
async fn node_removal_mid_cycle_maintains_consistency() {
    let mut nodes = create_node_batch(8, 0);
    // Simulate 3 nodes going down before the scheduling cycle
    nodes[2].state = NodeState::Down {
        reason: "hardware failure".into(),
    };
    nodes[5].state = NodeState::Down {
        reason: "network partition".into(),
    };
    nodes[7].state = NodeState::Down {
        reason: "kernel panic".into(),
    };

    let down_node_ids: HashSet<String> = nodes
        .iter()
        .filter(|n| !n.state.is_operational())
        .map(|n| n.id.clone())
        .collect();

    let topology = create_test_topology(1, 8);
    let tenant = TenantBuilder::new("t1").build();
    let allocs: Vec<Allocation> = (0..5)
        .map(|_| AllocationBuilder::new().tenant("t1").nodes(1).build())
        .collect();

    let input = make_cycle_input(allocs, vec![], nodes, vec![tenant], topology);

    let result = run_cycle(&input, &CostWeights::default());

    // Verify no allocation is placed on a down node
    for decision in &result.decisions {
        if let PlacementDecision::Place { nodes, .. } = decision {
            for nid in nodes {
                assert!(
                    !down_node_ids.contains(nid),
                    "Allocation placed on down node {}",
                    nid
                );
            }
        }
    }

    // At most 5 operational nodes available (8 - 3 down)
    assert!(result.placed().len() <= 5);
}

// ─── Test: Concurrent cancel (no orphans) ──────────────────

/// Submit allocations where some are already cancelled, and verify
/// the scheduler still produces valid output without crashing or
/// double-assigning nodes.
#[tokio::test]
async fn concurrent_cancel_no_orphans() {
    let nodes = create_node_batch(8, 0);
    let topology = create_test_topology(1, 8);

    let mut allocs = Vec::new();
    // 3 normal pending allocations
    for _ in 0..3 {
        allocs.push(AllocationBuilder::new().tenant("t1").nodes(1).build());
    }
    // 2 already-cancelled allocations (simulating concurrent cancel)
    for _ in 0..2 {
        allocs.push(
            AllocationBuilder::new()
                .tenant("t1")
                .nodes(1)
                .state(AllocationState::Cancelled)
                .build(),
        );
    }

    // The knapsack solver does not filter by state itself -- it processes
    // whatever is passed to it. This tests that it handles mixed states
    // without panicking or producing invalid output.
    let solver = KnapsackSolver::new(CostWeights::default());
    let ctx = lattice_scheduler::CostContext::default();
    let result = solver.solve(
        &allocs,
        &nodes,
        &topology,
        &ctx,
        &lattice_scheduler::ResourceTimeline { events: vec![] },
    );

    // All decisions should reference known allocation IDs
    let known_ids: HashSet<_> = allocs.iter().map(|a| a.id).collect();
    for decision in &result.decisions {
        assert!(
            known_ids.contains(&decision.allocation_id()),
            "Decision references unknown allocation"
        );
    }

    // No node double-assignment
    let mut used: HashSet<String> = HashSet::new();
    for decision in &result.decisions {
        if let PlacementDecision::Place { nodes, .. } = decision {
            for nid in nodes {
                assert!(used.insert(nid.clone()), "Double assignment of {}", nid);
            }
        }
    }
}

// ─── Test: State machine consistency under chaos ──────────

/// Validate that all defined transitions are self-consistent:
/// every transition that can_transition_to allows leads to a different
/// state; and terminal states truly block everything.
#[tokio::test]
async fn state_machine_consistency_under_chaos() {
    let all_states = [
        AllocationState::Pending,
        AllocationState::Staging,
        AllocationState::Running,
        AllocationState::Checkpointing,
        AllocationState::Suspended,
        AllocationState::Completed,
        AllocationState::Failed,
        AllocationState::Cancelled,
    ];

    let terminal_states = [
        AllocationState::Completed,
        AllocationState::Failed,
        AllocationState::Cancelled,
    ];

    // Verify terminal states block all transitions
    // Exception: Failed → Pending is allowed (service reconciliation)
    for terminal in &terminal_states {
        for target in &all_states {
            if *terminal == AllocationState::Failed && *target == AllocationState::Pending {
                assert!(
                    terminal.can_transition_to(target),
                    "Failed should be allowed to transition to Pending (service reconciliation)"
                );
            } else {
                assert!(
                    !terminal.can_transition_to(target),
                    "Terminal state {:?} should not transition to {:?}",
                    terminal,
                    target
                );
            }
        }
    }

    // Verify no self-transitions
    for state in &all_states {
        assert!(
            !state.can_transition_to(state),
            "State {:?} should not self-transition",
            state
        );
    }

    // Verify that every valid transition goes to a different state
    for source in &all_states {
        for target in &all_states {
            if source.can_transition_to(target) {
                assert_ne!(
                    source, target,
                    "Valid transition {:?} -> {:?} is a self-transition",
                    source, target
                );
            }
        }
    }

    // Verify reachability: Pending can reach all terminal states through
    // valid transition chains
    assert!(AllocationState::Pending.can_transition_to(&AllocationState::Running));
    assert!(AllocationState::Running.can_transition_to(&AllocationState::Completed));
    assert!(AllocationState::Running.can_transition_to(&AllocationState::Failed));
    assert!(AllocationState::Pending.can_transition_to(&AllocationState::Cancelled));
}

// ─── Test: Scheduling with zero available nodes ───────────

/// All nodes are down or draining. All allocations should be deferred.
#[tokio::test]
async fn scheduling_with_no_available_nodes_defers_all() {
    let mut nodes = create_node_batch(6, 0);
    for node in &mut nodes {
        node.state = NodeState::Down {
            reason: "total outage".into(),
        };
    }

    let topology = create_test_topology(1, 6);
    let tenant = TenantBuilder::new("t1").build();
    let allocs: Vec<Allocation> = (0..4)
        .map(|_| AllocationBuilder::new().tenant("t1").nodes(1).build())
        .collect();

    let input = make_cycle_input(allocs, vec![], nodes, vec![tenant], topology);

    let result = run_cycle(&input, &CostWeights::default());

    assert_eq!(
        result.placed().len(),
        0,
        "No allocations should be placed when all nodes are down"
    );
    assert_eq!(
        result.deferred().len(),
        4,
        "All allocations should be deferred"
    );
}

// ─── Test: Scheduling with draining nodes ─────────────────

/// Draining nodes should not receive new allocations.
#[tokio::test]
async fn draining_nodes_not_assigned() {
    let mut nodes = create_node_batch(4, 0);
    nodes[0].state = NodeState::Draining;
    nodes[1].state = NodeState::Draining;

    let draining_ids: HashSet<String> = nodes
        .iter()
        .filter(|n| matches!(n.state, NodeState::Draining))
        .map(|n| n.id.clone())
        .collect();

    let topology = create_test_topology(1, 4);
    let tenant = TenantBuilder::new("t1").build();
    let allocs: Vec<Allocation> = (0..3)
        .map(|_| AllocationBuilder::new().tenant("t1").nodes(1).build())
        .collect();

    let input = make_cycle_input(allocs, vec![], nodes, vec![tenant], topology);

    let result = run_cycle(&input, &CostWeights::default());

    for decision in &result.decisions {
        if let PlacementDecision::Place { nodes, .. } = decision {
            for nid in nodes {
                assert!(
                    !draining_ids.contains(nid),
                    "Allocation placed on draining node {}",
                    nid
                );
            }
        }
    }

    // Only 2 operational nodes, so at most 2 placements
    assert!(result.placed().len() <= 2);
}

// ─── Test: Fair share under contention ────────────────────

/// Two tenants, both requesting many allocations on limited nodes.
/// The higher-priority tenant should get scheduled first.
#[tokio::test]
async fn fair_share_under_contention() {
    let nodes = create_node_batch(4, 0);
    let topology = create_test_topology(1, 4);
    let t1 = TenantBuilder::new("physics").fair_share(0.6).build();
    let t2 = TenantBuilder::new("biology").fair_share(0.4).build();

    let mut allocs = Vec::new();
    // Physics team: 5 allocations at high priority
    for _ in 0..5 {
        allocs.push(
            AllocationBuilder::new()
                .tenant("physics")
                .nodes(1)
                .preemption_class(8)
                .build(),
        );
    }
    // Biology team: 5 allocations at lower priority
    for _ in 0..5 {
        allocs.push(
            AllocationBuilder::new()
                .tenant("biology")
                .nodes(1)
                .preemption_class(2)
                .build(),
        );
    }

    // Use priority-only weights to make ordering deterministic
    let weights = CostWeights {
        priority: 1.0,
        wait_time: 0.0,
        fair_share: 0.0,
        topology: 0.0,
        data_readiness: 0.0,
        backlog: 0.0,
        energy: 0.0,
        checkpoint_efficiency: 0.0,
        conformance: 0.0,
    };

    let input = make_cycle_input(allocs.clone(), vec![], nodes, vec![t1, t2], topology);

    let result = run_cycle(&input, &weights);

    assert_eq!(result.placed().len(), 4, "Expected 4 placements");

    let physics_ids: HashSet<_> = allocs
        .iter()
        .filter(|a| a.tenant == "physics")
        .map(|a| a.id)
        .collect();

    let placed_physics = result
        .placed()
        .iter()
        .filter(|d| physics_ids.contains(&d.allocation_id()))
        .count();

    // All 4 placements should go to physics (higher priority)
    assert_eq!(
        placed_physics, 4,
        "Higher priority tenant should be scheduled first"
    );
}

// ─── Test: Owned nodes are not reassigned ─────────────────

/// Nodes already owned by another allocation should not be assigned
/// to new allocations.
#[tokio::test]
async fn owned_nodes_not_reassigned() {
    let mut nodes = create_node_batch(6, 0);
    for node in nodes.iter_mut().take(3) {
        node.owner = Some(NodeOwnership {
            tenant: "existing-tenant".into(),
            vcluster: "vc1".into(),
            allocation: uuid::Uuid::new_v4(),
            claimed_by: None,
            is_borrowed: false,
        });
    }

    let owned_ids: HashSet<String> = nodes
        .iter()
        .filter(|n| n.owner.is_some())
        .map(|n| n.id.clone())
        .collect();

    let topology = create_test_topology(1, 6);
    let tenant = TenantBuilder::new("t1").build();
    let allocs: Vec<Allocation> = (0..5)
        .map(|_| AllocationBuilder::new().tenant("t1").nodes(1).build())
        .collect();

    let input = make_cycle_input(allocs, vec![], nodes, vec![tenant], topology);

    let result = run_cycle(&input, &CostWeights::default());

    for decision in &result.decisions {
        if let PlacementDecision::Place { nodes, .. } = decision {
            for nid in nodes {
                assert!(
                    !owned_ids.contains(nid),
                    "Allocation placed on owned node {}",
                    nid
                );
            }
        }
    }

    // Only 3 free nodes available
    assert!(result.placed().len() <= 3);
}

// ─── Test: Large allocation on small cluster ──────────────

/// An allocation requesting more nodes than available should be deferred.
#[tokio::test]
async fn large_allocation_deferred_on_small_cluster() {
    let nodes = create_node_batch(4, 0);
    let topology = create_test_topology(1, 4);
    let tenant = TenantBuilder::new("t1").build();

    let allocs = vec![
        AllocationBuilder::new().tenant("t1").nodes(10).build(),
        AllocationBuilder::new().tenant("t1").nodes(1).build(),
    ];

    let input = make_cycle_input(allocs.clone(), vec![], nodes, vec![tenant], topology);

    let result = run_cycle(&input, &CostWeights::default());

    let deferred_ids: HashSet<_> = result
        .deferred()
        .iter()
        .map(|d| d.allocation_id())
        .collect();
    assert!(
        deferred_ids.contains(&allocs[0].id),
        "10-node allocation should be deferred"
    );

    let placed_ids: HashSet<_> = result.placed().iter().map(|d| d.allocation_id()).collect();
    assert!(
        placed_ids.contains(&allocs[1].id),
        "1-node allocation should be placed"
    );
}

// ─── Test: Mixed node states ──────────────────────────────

/// A cluster with mixed node states: some Ready, some Degraded, some Down.
/// Only Ready and Degraded nodes should receive allocations.
#[tokio::test]
async fn mixed_node_states_only_operational_assigned() {
    let nodes = vec![
        NodeBuilder::new().id("n0").group(0).build(),
        NodeBuilder::new().id("n1").group(0).build(),
        NodeBuilder::new()
            .id("n2")
            .group(0)
            .state(NodeState::Degraded {
                reason: "high temp".into(),
            })
            .build(),
        NodeBuilder::new()
            .id("n3")
            .group(0)
            .state(NodeState::Down {
                reason: "disk failure".into(),
            })
            .build(),
        NodeBuilder::new()
            .id("n4")
            .group(0)
            .state(NodeState::Draining)
            .build(),
        NodeBuilder::new()
            .id("n5")
            .group(0)
            .state(NodeState::Booting)
            .build(),
    ];

    let non_operational: HashSet<String> = nodes
        .iter()
        .filter(|n| !n.state.is_operational())
        .map(|n| n.id.clone())
        .collect();

    let topology = create_test_topology(1, 6);
    let tenant = TenantBuilder::new("t1").build();
    let allocs: Vec<Allocation> = (0..4)
        .map(|_| AllocationBuilder::new().tenant("t1").nodes(1).build())
        .collect();

    let input = make_cycle_input(allocs, vec![], nodes, vec![tenant], topology);

    let result = run_cycle(&input, &CostWeights::default());

    for decision in &result.decisions {
        if let PlacementDecision::Place { nodes, .. } = decision {
            for nid in nodes {
                assert!(
                    !non_operational.contains(nid),
                    "Allocation placed on non-operational node {}",
                    nid
                );
            }
        }
    }

    // 3 operational nodes (n0, n1, n2), so at most 3 placements
    assert!(
        result.placed().len() <= 3,
        "At most 3 placements on 3 operational nodes, got {}",
        result.placed().len()
    );
}

// ─── Test: Idempotent scheduling cycles ───────────────────

/// Running the same scheduling cycle twice should produce the same results.
#[tokio::test]
async fn scheduling_is_idempotent() {
    let nodes = create_node_batch(6, 0);
    let topology = create_test_topology(1, 6);
    let tenant = TenantBuilder::new("t1").build();
    let allocs: Vec<Allocation> = (0..3)
        .map(|_| AllocationBuilder::new().tenant("t1").nodes(1).build())
        .collect();

    let input = make_cycle_input(allocs, vec![], nodes, vec![tenant], topology);

    let result1 = run_cycle(&input, &CostWeights::default());
    let result2 = run_cycle(&input, &CostWeights::default());

    assert_eq!(
        result1.decisions.len(),
        result2.decisions.len(),
        "Decision count should be identical across cycles"
    );
    assert_eq!(
        result1.placed().len(),
        result2.placed().len(),
        "Placed count should be identical across cycles"
    );
    assert_eq!(
        result1.deferred().len(),
        result2.deferred().len(),
        "Deferred count should be identical across cycles"
    );

    let placed1: HashSet<_> = result1.placed().iter().map(|d| d.allocation_id()).collect();
    let placed2: HashSet<_> = result2.placed().iter().map(|d| d.allocation_id()).collect();
    assert_eq!(placed1, placed2, "Same allocations should be placed");
}
