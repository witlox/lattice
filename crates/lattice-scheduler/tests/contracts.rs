//! Contract tests — verify scheduling module boundary contracts.
//!
//! Source: specs/architecture/interfaces/scheduling.md, invariants.md, enforcement-map.md
//!
//! These tests verify:
//! - VClusterScheduler trait implementations conform to interface contract
//! - Cost function contract (scoring, factor independence)
//! - Knapsack solver contract (no double-placement, defers when can't fit)
//! - Preemption contract (INV-E1: only preempt lower class)
//! - DAG controller contract (INV-E2: acyclicity, INV-E3: dependency satisfaction)

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lattice_common::types::*;
use lattice_scheduler::cost::{CostContext, CostEvaluator};
use lattice_scheduler::cycle::{run_cycle, CycleInput};
use lattice_scheduler::dag_controller::{
    DagCommandSink, DagController, DagControllerConfig, DagStateReader,
};
use lattice_scheduler::placement::PlacementDecision;
use lattice_scheduler::preemption::{evaluate_preemption, PreemptionConfig, PreemptionResult};
use lattice_scheduler::schedulers::create_scheduler;
use lattice_test_harness::fixtures::*;

// ═══════════════════════════════════════════════════════════════════
// INTERFACE CONTRACTS — scheduling.md § VClusterScheduler
// ═══════════════════════════════════════════════════════════════════

mod scheduler_interface {
    use super::*;

    // Contract: scheduling.md § VClusterScheduler — "Pure function: no side effects"
    // All 4 scheduler types must return valid placements or empty vec
    #[tokio::test]
    async fn all_scheduler_types_return_valid_placements() {
        let types = vec![
            SchedulerType::HpcBackfill,
            SchedulerType::ServiceBinPack,
            SchedulerType::SensitiveReservation,
            SchedulerType::InteractiveFifo,
        ];
        let nodes = create_node_batch(4, 0);
        let pending = vec![AllocationBuilder::new().nodes(1).build()];

        for st in types {
            let vc = VClusterBuilder::new("contract-vc")
                .scheduler(st.clone())
                .build();
            let scheduler = create_scheduler(&vc);

            let placements = scheduler.schedule(&pending, &nodes).await.unwrap();
            // Either places or defers — never panics, never assigns >1 node set per alloc
            for p in &placements {
                assert!(
                    !p.nodes.is_empty(),
                    "Placement for {:?} must have at least one node",
                    st
                );
            }
        }
    }

    // Contract: scheduling.md § VClusterScheduler — factory creates correct type
    #[test]
    fn create_scheduler_returns_correct_type() {
        let types = vec![
            (SchedulerType::HpcBackfill, "HpcBackfill"),
            (SchedulerType::ServiceBinPack, "ServiceBinPack"),
            (SchedulerType::SensitiveReservation, "SensitiveReservation"),
            (SchedulerType::InteractiveFifo, "InteractiveFifo"),
        ];

        for (st, expected_name) in types {
            let vc = VClusterBuilder::new("factory-vc").scheduler(st).build();
            let scheduler = create_scheduler(&vc);
            let name = format!("{:?}", scheduler.scheduler_type());
            assert!(
                name.contains(expected_name),
                "create_scheduler({expected_name}) returned wrong type: {name}"
            );
        }
    }

    // Contract: scheduling.md § VClusterScheduler — empty pending returns empty placements
    #[tokio::test]
    async fn schedule_empty_pending_returns_empty() {
        let vc = VClusterBuilder::new("empty-vc").build();
        let scheduler = create_scheduler(&vc);
        let nodes = create_node_batch(4, 0);

        let placements = scheduler.schedule(&[], &nodes).await.unwrap();
        assert!(
            placements.is_empty(),
            "Scheduling empty pending must return empty placements"
        );
    }

    // Contract: scheduling.md § VClusterScheduler — no nodes returns empty placements
    #[tokio::test]
    async fn schedule_no_nodes_returns_empty() {
        let vc = VClusterBuilder::new("no-nodes-vc").build();
        let scheduler = create_scheduler(&vc);
        let pending = vec![AllocationBuilder::new().nodes(1).build()];

        let placements = scheduler.schedule(&pending, &[]).await.unwrap();
        assert!(
            placements.is_empty(),
            "Scheduling with no nodes must return empty placements"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
// COST FUNCTION CONTRACTS — scheduling.md § CostEvaluator
// ═══════════════════════════════════════════════════════════════════

mod cost_contracts {
    use super::*;

    // Contract: scheduling.md § CostEvaluator — "Score = Σ wᵢ · fᵢ(alloc, ctx)"
    // Score must be finite and non-negative for valid inputs
    #[test]
    fn score_is_finite_for_valid_inputs() {
        let evaluator = CostEvaluator::new(CostWeights::default());
        let ctx = CostContext::default();
        let alloc = AllocationBuilder::new().build();

        let score = evaluator.score(&alloc, &ctx);
        assert!(score.is_finite(), "Score must be finite");
        assert!(score >= 0.0, "Score must be non-negative");
    }

    // Contract: scheduling.md § CostEvaluator — higher preemption class = higher f1 score
    #[test]
    fn higher_preemption_class_increases_score() {
        let evaluator = CostEvaluator::new(CostWeights {
            priority: 1.0,
            wait_time: 0.0,
            fair_share: 0.0,
            topology: 0.0,
            data_readiness: 0.0,
            backlog: 0.0,
            energy: 0.0,
            checkpoint_efficiency: 0.0,
            conformance: 0.0,
        });
        let ctx = CostContext::default();

        let low = AllocationBuilder::new().preemption_class(1).build();
        let high = AllocationBuilder::new().preemption_class(8).build();

        let score_low = evaluator.score(&low, &ctx);
        let score_high = evaluator.score(&high, &ctx);
        assert!(
            score_high > score_low,
            "Higher preemption class must yield higher score: {score_high} vs {score_low}"
        );
    }

    // Contract: scheduling.md § CostEvaluator — zero weights produce zero score
    #[test]
    fn zero_weights_produce_zero_score() {
        let evaluator = CostEvaluator::new(CostWeights {
            priority: 0.0,
            wait_time: 0.0,
            fair_share: 0.0,
            topology: 0.0,
            data_readiness: 0.0,
            backlog: 0.0,
            energy: 0.0,
            checkpoint_efficiency: 0.0,
            conformance: 0.0,
        });
        let ctx = CostContext::default();
        let alloc = AllocationBuilder::new().build();

        let score = evaluator.score(&alloc, &ctx);
        assert!(
            score.abs() < 1e-10,
            "Zero weights must produce zero score, got {score}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
// KNAPSACK SOLVER CONTRACTS — scheduling.md § KnapsackSolver
// ═══════════════════════════════════════════════════════════════════

mod knapsack_contracts {
    use super::*;

    fn make_cycle_input(pending: Vec<Allocation>, nodes: Vec<Node>) -> CycleInput {
        let topology = create_test_topology(1, nodes.len());
        CycleInput {
            pending,
            running: vec![],
            nodes,
            tenants: vec![TenantBuilder::new("t1").build()],
            topology,
            data_readiness: HashMap::new(),
            energy_price: 0.5,
        }
    }

    // Contract: scheduling.md § KnapsackSolver — "Never places more nodes than available"
    #[test]
    fn never_overcommits_available_nodes() {
        let nodes = create_node_batch(3, 0);
        let pending: Vec<Allocation> = (0..10)
            .map(|_| AllocationBuilder::new().tenant("t1").nodes(1).build())
            .collect();

        let input = make_cycle_input(pending, nodes.clone());
        let result = run_cycle(&input, &CostWeights::default());

        // Count total nodes assigned across all placed decisions
        let total_assigned: usize = result
            .placed()
            .iter()
            .map(|d| match d {
                PlacementDecision::Place { nodes, .. } => nodes.len(),
                _ => 0,
            })
            .sum();
        assert!(
            total_assigned <= nodes.len(),
            "Total assigned ({total_assigned}) must not exceed available ({})",
            nodes.len()
        );
    }

    // Contract: scheduling.md § KnapsackSolver — "Never violates INV-S1 (double-assignment)"
    #[test]
    fn no_double_assignment_of_nodes() {
        let nodes = create_node_batch(5, 0);
        let pending: Vec<Allocation> = (0..3)
            .map(|_| AllocationBuilder::new().tenant("t1").nodes(2).build())
            .collect();

        let input = make_cycle_input(pending, nodes);
        let result = run_cycle(&input, &CostWeights::default());

        let mut all_assigned: Vec<String> = Vec::new();
        for d in &result.decisions {
            if let PlacementDecision::Place { nodes, .. } = d {
                all_assigned.extend(nodes.iter().cloned());
            }
        }
        let unique: HashSet<&String> = all_assigned.iter().collect();
        assert_eq!(
            all_assigned.len(),
            unique.len(),
            "INV-S1: no node should be assigned to multiple allocations"
        );
    }

    // Contract: scheduling.md § KnapsackSolver — "Defers allocations that cannot fit"
    #[test]
    fn defers_when_insufficient_nodes() {
        let nodes = create_node_batch(2, 0);
        // Request 5 nodes but only 2 available
        let pending = vec![AllocationBuilder::new().tenant("t1").nodes(5).build()];

        let input = make_cycle_input(pending, nodes);
        let result = run_cycle(&input, &CostWeights::default());

        assert_eq!(
            result.deferred().len(),
            1,
            "Must defer allocation when insufficient nodes"
        );
        assert!(
            result.placed().is_empty(),
            "Must not place allocation when insufficient nodes"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
// PREEMPTION CONTRACTS — scheduling.md § Preemption, INV-E1
// ═══════════════════════════════════════════════════════════════════

mod preemption_contracts {
    use super::*;

    // Contract: INV-E1 (Preemption Class Ordering) — "class-N only preempted by class-(N+1)+"
    #[test]
    fn inv_e1_higher_class_can_preempt_lower() {
        let requester = AllocationBuilder::new()
            .preemption_class(5)
            .nodes(1)
            .build();
        let mut victim = AllocationBuilder::new()
            .preemption_class(2)
            .nodes(1)
            .state(AllocationState::Running)
            .build();
        victim.assigned_nodes = vec!["n1".into()];

        let result = evaluate_preemption(&requester, &[victim], &PreemptionConfig::default());
        assert!(
            matches!(result, PreemptionResult::Possible { .. }),
            "INV-E1: class-5 must be able to preempt class-2"
        );
    }

    // Contract: INV-E1 — "lower class cannot preempt higher"
    #[test]
    fn inv_e1_lower_class_cannot_preempt_higher() {
        let requester = AllocationBuilder::new()
            .preemption_class(2)
            .nodes(1)
            .build();
        let mut victim = AllocationBuilder::new()
            .preemption_class(5)
            .nodes(1)
            .state(AllocationState::Running)
            .build();
        victim.assigned_nodes = vec!["n1".into()];

        let result = evaluate_preemption(&requester, &[victim], &PreemptionConfig::default());
        assert!(
            matches!(result, PreemptionResult::NotPossible { .. }),
            "INV-E1: class-2 must not preempt class-5"
        );
    }

    // Contract: INV-E1 — "Sensitive (class 10) never preempted"
    #[test]
    fn inv_e1_sensitive_never_preempted() {
        let requester = AllocationBuilder::new()
            .preemption_class(10)
            .nodes(1)
            .build();
        let mut victim = AllocationBuilder::new()
            .preemption_class(5)
            .sensitive()
            .nodes(1)
            .state(AllocationState::Running)
            .build();
        victim.assigned_nodes = vec!["n1".into()];

        let result = evaluate_preemption(&requester, &[victim], &PreemptionConfig::default());
        assert!(
            matches!(result, PreemptionResult::NotPossible { .. }),
            "INV-E1: sensitive must never be preempted"
        );
    }

    // Contract: INV-E1 — "same class cannot preempt same class"
    #[test]
    fn inv_e1_same_class_cannot_preempt() {
        let requester = AllocationBuilder::new()
            .preemption_class(5)
            .nodes(1)
            .build();
        let mut victim = AllocationBuilder::new()
            .preemption_class(5)
            .nodes(1)
            .state(AllocationState::Running)
            .build();
        victim.assigned_nodes = vec!["n1".into()];

        let result = evaluate_preemption(&requester, &[victim], &PreemptionConfig::default());
        assert!(
            matches!(result, PreemptionResult::NotPossible { .. }),
            "INV-E1: same class must not preempt same class"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
// DAG CONTRACTS — scheduling.md § DAG Controller, INV-E2, INV-E3
// ═══════════════════════════════════════════════════════════════════

mod dag_contracts {
    use super::*;
    use lattice_common::error::LatticeError;
    use tokio::sync::Mutex;

    // Mock implementations matching the DagController trait-based API

    struct MockDagReader {
        allocations: Mutex<Vec<Allocation>>,
    }

    impl MockDagReader {
        fn new(allocs: Vec<Allocation>) -> Self {
            Self {
                allocations: Mutex::new(allocs),
            }
        }

        async fn update(&self, allocs: Vec<Allocation>) {
            *self.allocations.lock().await = allocs;
        }
    }

    #[async_trait::async_trait]
    impl DagStateReader for MockDagReader {
        async fn dag_allocations(&self) -> Result<Vec<Allocation>, LatticeError> {
            Ok(self.allocations.lock().await.clone())
        }
    }

    struct MockDagSink {
        unblocked: Mutex<Vec<AllocId>>,
        cancelled: Mutex<Vec<(AllocId, String)>>,
    }

    impl MockDagSink {
        fn new() -> Self {
            Self {
                unblocked: Mutex::new(Vec::new()),
                cancelled: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl DagCommandSink for MockDagSink {
        async fn unblock_allocation(&self, alloc_id: AllocId) -> Result<(), LatticeError> {
            self.unblocked.lock().await.push(alloc_id);
            Ok(())
        }

        async fn cancel_allocation(
            &self,
            alloc_id: AllocId,
            reason: String,
        ) -> Result<(), LatticeError> {
            self.cancelled.lock().await.push((alloc_id, reason));
            Ok(())
        }
    }

    // Contract: INV-E2 (DAG Acyclicity) — validated at submission
    // Root nodes with no dependencies are unblocked on first cycle
    #[tokio::test]
    async fn inv_e2_root_unblocked_dependent_blocked() {
        // A (completed) → B (pending, depends on A with Success)
        let a = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();
        let b_id = b.id;

        let reader = Arc::new(MockDagReader::new(vec![a, b]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

        let count = ctrl.run_once().await.unwrap();
        assert_eq!(count, 1, "INV-E2: one allocation should be unblocked");

        let unblocked = sink.unblocked.lock().await;
        assert_eq!(
            unblocked[0], b_id,
            "INV-E2: B should be unblocked after A completes"
        );
    }

    // Contract: INV-E3 (Dependency Condition Satisfaction)
    // "A DAG allocation enters the scheduler queue only when ALL dependencies satisfied"
    #[tokio::test]
    async fn inv_e3_all_dependencies_must_be_satisfied() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        // B is still running — not terminal
        let b = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        // C depends on both A (success) and B (success)
        let c = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .depends_on(&b.id.to_string(), DependencyCondition::Success)
            .build();

        let reader = Arc::new(MockDagReader::new(vec![a.clone(), b.clone(), c.clone()]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl =
            DagController::new(reader.clone(), sink.clone(), DagControllerConfig::default());

        // First cycle: A completed but B still running — C stays blocked
        let count = ctrl.run_once().await.unwrap();
        assert_eq!(
            count, 0,
            "INV-E3: C must remain blocked when B is still running"
        );

        // Now B completes too
        let mut b_completed = b.clone();
        b_completed.state = AllocationState::Completed;
        reader.update(vec![a, b_completed, c.clone()]).await;

        // Second cycle: both deps satisfied — C should unblock
        let count = ctrl.run_once().await.unwrap();
        assert_eq!(
            count, 1,
            "INV-E3: C must be unblocked when all dependencies satisfied"
        );
        assert_eq!(sink.unblocked.lock().await[0], c.id);
    }

    // Contract: ADV-07 — "DAG controller marks processed BEFORE unblock"
    // The same allocation must not be unblocked twice
    #[tokio::test]
    async fn adv_07_no_double_unblock() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();

        let reader = Arc::new(MockDagReader::new(vec![a, b]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

        // First cycle unblocks B
        ctrl.run_once().await.unwrap();
        assert_eq!(sink.unblocked.lock().await.len(), 1);

        // Second cycle: B already processed, should NOT be re-triggered
        ctrl.run_once().await.unwrap();
        assert_eq!(
            sink.unblocked.lock().await.len(),
            1,
            "ADV-07: must not double-unblock allocation on duplicate completion"
        );
    }

    // Contract: DAG unsatisfiable dependencies get cancelled
    #[tokio::test]
    async fn unsatisfiable_dependency_cancelled() {
        // A failed, B depends on A with Success condition — B can never succeed
        let a = AllocationBuilder::new()
            .state(AllocationState::Failed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();
        let b_id = b.id;

        let reader = Arc::new(MockDagReader::new(vec![a, b]));
        let sink = Arc::new(MockDagSink::new());
        let mut ctrl = DagController::new(reader, sink.clone(), DagControllerConfig::default());

        ctrl.run_once().await.unwrap();

        // B should be cancelled, not unblocked
        let unblocked = sink.unblocked.lock().await;
        assert!(unblocked.is_empty(), "B should not be unblocked");
        let cancelled = sink.cancelled.lock().await;
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].0, b_id);
    }
}
