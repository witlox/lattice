//! # hpc-scheduler-core
//!
//! Core scheduling algorithms for HPC job schedulers.
//!
//! This crate provides generic, reusable components for building HPC schedulers:
//!
//! - **Composite cost function** (`cost`): 9-factor scoring with tunable weights
//! - **Knapsack solver** (`knapsack`): Greedy topology-aware placement with reservation-based backfill
//! - **Topology-aware placement** (`topology`): Dragonfly group packing (tight/spread/any)
//! - **Conformance grouping** (`conformance`): Driver/firmware homogeneity enforcement
//! - **Resource timeline** (`resource_timeline`): Future node release tracking for backfill
//! - **Preemption** (`preemption`): Burst-aware victim selection
//! - **Walltime enforcement** (`walltime`): Two-phase SIGTERM/SIGKILL protocol
//! - **Placement types** (`placement`): Decision and result types
//!
//! ## Getting started
//!
//! Implement the [`Job`] and [`ComputeNode`] traits for your workload and node types,
//! then use [`KnapsackSolver`] to run scheduling cycles:
//!
//! ```ignore
//! use hpc_scheduler_core::*;
//!
//! let solver = KnapsackSolver::new(CostWeights::default());
//! let result = solver.solve(&pending_jobs, &nodes, &topology, &ctx, &timeline);
//! for decision in &result.decisions {
//!     match decision {
//!         PlacementDecision::Place { allocation_id, nodes } => { /* assign */ }
//!         PlacementDecision::Defer { allocation_id, reason } => { /* requeue */ }
//!         _ => {}
//!     }
//! }
//! ```

pub mod conformance;
pub mod cost;
pub mod knapsack;
pub mod placement;
pub mod preemption;
pub mod resource_timeline;
pub mod topology;
pub mod traits;
pub mod types;
pub mod walltime;

// Re-export key types at crate root for convenience.
pub use conformance::{
    conformance_fitness, filter_by_constraints, group_by_conformance, memory_locality_score,
    select_conformant_nodes,
};
pub use cost::{BacklogMetrics, BudgetUtilization, CostContext, CostEvaluator, TenantUsage};
pub use knapsack::KnapsackSolver;
pub use placement::{PlacementDecision, SchedulingResult};
pub use preemption::{
    evaluate_preemption, PreemptionCandidate, PreemptionConfig, PreemptionResult,
};
pub use resource_timeline::{ReleaseEvent, ResourceTimeline, TimelineConfig};
pub use topology::{group_span, select_nodes_topology_aware};
pub use traits::{ComputeNode, Job};
pub use types::*;
pub use walltime::{ExpiryPhase, WalltimeEnforcer, WalltimeExpiry};
