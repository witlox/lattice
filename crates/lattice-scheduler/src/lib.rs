//! # lattice-scheduler
//!
//! Multi-dimensional knapsack scheduler with composite cost function.
//!
//! Implements the scheduling algorithm from `docs/architecture/scheduling-algorithm.md`:
//! - 9-factor composite cost function (`Score(j) = Σ wᵢ · fᵢ(j)`)
//! - Greedy topology-aware backfill solver
//! - 4 vCluster scheduler types (HPC backfill, service bin-pack, medical, interactive)
//! - Preemption candidate selection
//! - DAG validation and dependency resolution
//! - Soft quota signaling for fair-share

pub mod conformance;
pub mod cost;
pub mod cycle;
pub mod dag;
pub mod knapsack;
pub mod placement;
pub mod preemption;
pub mod quota;
pub mod schedulers;
pub mod topology;

pub use conformance::{conformance_fitness, filter_by_constraints};
pub use cost::{CostContext, CostEvaluator};
pub use cycle::{run_cycle, CycleInput};
pub use dag::{validate_dag, Dag, DagError};
pub use knapsack::KnapsackSolver;
pub use placement::{PlacementDecision, SchedulingResult};
pub use preemption::{evaluate_preemption, PreemptionConfig, PreemptionResult};
pub use schedulers::create_scheduler;
