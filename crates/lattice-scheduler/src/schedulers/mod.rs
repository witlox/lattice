//! Per-vCluster scheduler implementations.
//!
//! Each scheduler type implements `VClusterScheduler` from lattice-common,
//! using the knapsack solver with type-specific weight profiles and policies.

pub mod hpc_backfill;
pub mod interactive;
pub mod medical;
pub mod service_binpack;

pub use hpc_backfill::HpcBackfillScheduler;
pub use interactive::InteractiveFifoScheduler;
pub use medical::MedicalReservationScheduler;
pub use service_binpack::ServiceBinPackScheduler;

use lattice_common::types::*;

/// Create a scheduler for the given vCluster type.
pub fn create_scheduler(vc: &VCluster) -> Box<dyn lattice_common::traits::VClusterScheduler> {
    match vc.scheduler_type {
        SchedulerType::HpcBackfill => Box::new(HpcBackfillScheduler::new(vc.cost_weights.clone())),
        SchedulerType::ServiceBinPack => {
            Box::new(ServiceBinPackScheduler::new(vc.cost_weights.clone()))
        }
        SchedulerType::MedicalReservation => Box::new(MedicalReservationScheduler::new()),
        SchedulerType::InteractiveFifo => {
            Box::new(InteractiveFifoScheduler::new(vc.cost_weights.clone()))
        }
    }
}
