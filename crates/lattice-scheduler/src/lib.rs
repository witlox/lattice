//! # lattice-scheduler
//!
//! Multi-dimensional knapsack scheduler with composite cost function.
//!
//! Implements the scheduling algorithm from `docs/architecture/scheduling-algorithm.md`:
//! - 9-factor composite cost function (`Score(j) = Σ wᵢ · fᵢ(j)`)
//! - Greedy topology-aware backfill solver
//! - 4 vCluster scheduler types (HPC backfill, service bin-pack, sensitive, interactive)
//! - Preemption candidate selection
//! - DAG validation and dependency resolution
//! - Soft quota signaling for fair-share

pub mod autoscaler;
pub mod borrowing;
pub mod conformance;
pub mod cost;
pub mod cycle;
pub mod dag;
pub mod dag_controller;
pub mod data_staging;
pub mod federation;
pub mod impls;
pub mod knapsack;
pub mod loop_runner;
pub mod placement;
pub mod preemption;
pub mod quota;
pub mod resource_timeline;
pub mod schedulers;
pub mod topology;
pub mod walltime;

pub use autoscaler::{Autoscaler, AutoscalerConfig, ScaleDecision};
pub use borrowing::{BorrowRequest, BorrowResult, BorrowingBroker, BorrowingConfig};
pub use conformance::{conformance_fitness, filter_by_constraints};
pub use cost::{CostContext, CostEvaluator};
pub use cycle::{run_cycle, CycleInput};
pub use dag::{validate_dag, Dag, DagError};
pub use dag_controller::{DagCommandSink, DagController, DagControllerConfig, DagStateReader};
pub use data_staging::{DataStager, StagingPlan, StagingRequest};
pub use federation::{FederationBroker, FederationConfig, FederationOffer, OfferDecision};
pub use knapsack::KnapsackSolver;
pub use loop_runner::{
    SchedulerCommandSink, SchedulerLoop, SchedulerLoopConfig, SchedulerStateReader,
    TraitCommandSink, TraitStateReader,
};
pub use placement::{PlacementDecision, SchedulingResult};
pub use preemption::{evaluate_preemption, PreemptionConfig, PreemptionResult};
pub use resource_timeline::{ResourceTimeline, TimelineConfig};
pub use schedulers::create_scheduler;
pub use walltime::{ExpiryPhase, WalltimeEnforcer, WalltimeExpiry};
