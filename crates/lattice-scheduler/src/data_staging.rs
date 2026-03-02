//! Data staging and prefetch planner.
//!
//! Computes a prioritised staging plan for pending allocations so the data
//! mover can pre-stage input datasets while jobs wait in the queue — keeping
//! data movement invisible to users.
//!
//! See `docs/architecture/data-staging.md` for the design.

use lattice_common::types::{AllocId, Allocation, AllocationState};

/// A single data staging request for one mount inside one allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagingRequest {
    /// The allocation that requires this data.
    pub allocation_id: AllocId,
    /// Source path (e.g. `s3://bucket/path` or `nfs://server/path`).
    pub source: String,
    /// Destination mount point inside the allocation's namespace.
    pub destination: String,
    /// Estimated size of the dataset in bytes.
    pub size_bytes: u64,
    /// Priority: higher value = stage earlier (inherits from allocation class).
    pub priority: u8,
}

/// An ordered plan of staging requests for the data mover to execute.
#[derive(Debug, Clone, Default)]
pub struct StagingPlan {
    /// Requests sorted by priority descending (highest first).
    pub requests: Vec<StagingRequest>,
    /// Total bytes to be moved across all requests.
    pub total_bytes: u64,
    /// Rough estimated time in seconds, given a fixed throughput assumption.
    pub estimated_time_secs: u64,
}

impl StagingPlan {
    /// Assumed staging throughput used for time estimation (bytes/sec).
    /// Corresponds to ~1 GB/s, typical for VAST NFS hot-tier.
    const ASSUMED_THROUGHPUT_BPS: u64 = 1_000_000_000;

    /// Build a `StagingPlan` from an unordered list of requests.
    pub fn from_requests(mut requests: Vec<StagingRequest>) -> Self {
        // Sort descending by priority, then ascending by size for tie-breaking.
        requests.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.size_bytes.cmp(&b.size_bytes))
        });

        let total_bytes: u64 = requests.iter().map(|r| r.size_bytes).sum();
        let estimated_time_secs = if Self::ASSUMED_THROUGHPUT_BPS == 0 {
            0
        } else {
            total_bytes / Self::ASSUMED_THROUGHPUT_BPS
        };

        Self {
            requests,
            total_bytes,
            estimated_time_secs,
        }
    }
}

/// Plans data staging and prefetching for pending allocations.
pub struct DataStager;

impl DataStager {
    /// Create a new `DataStager`.
    pub fn new() -> Self {
        Self
    }

    /// Build a staging plan from a slice of pending allocations.
    ///
    /// Only allocations in the `Pending` or `Staging` state with at least one
    /// data mount are included. The `priority` of each request is taken from
    /// `allocation.lifecycle.preemption_class` as a proxy for urgency.
    pub fn plan_staging(&self, allocations: &[Allocation]) -> StagingPlan {
        let requests: Vec<StagingRequest> = allocations
            .iter()
            .filter(|a| self.should_prefetch(a))
            .flat_map(|a| {
                a.data.mounts.iter().map(move |mount| StagingRequest {
                    allocation_id: a.id,
                    source: mount.source.clone(),
                    destination: mount.target.clone(),
                    // Size not stored in DataMount; default to 0 when unknown.
                    size_bytes: 0,
                    priority: a.lifecycle.preemption_class,
                })
            })
            .collect();

        StagingPlan::from_requests(requests)
    }

    /// Return `true` if the allocation should have its data pre-staged.
    ///
    /// Criteria:
    /// - State is `Pending` or `Staging`
    /// - Has at least one data mount
    pub fn should_prefetch(&self, allocation: &Allocation) -> bool {
        let is_queued = matches!(
            allocation.state,
            AllocationState::Pending | AllocationState::Staging
        );
        let has_data = !allocation.data.mounts.is_empty();
        is_queued && has_data
    }
}

impl Default for DataStager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_common::types::{DataAccess, DataMount};
    use lattice_test_harness::fixtures::AllocationBuilder;

    // ── Helpers ──────────────────────────────────────────────

    fn add_mount(alloc: &mut Allocation, source: &str, target: &str) {
        alloc.data.mounts.push(DataMount {
            source: source.into(),
            target: target.into(),
            access: DataAccess::ReadOnly,
            tier_hint: None,
        });
    }

    // ── StagingPlan ordering tests ───────────────────────────

    #[test]
    fn staging_plan_ordered_by_priority_descending() {
        let req_low = StagingRequest {
            allocation_id: uuid::Uuid::new_v4(),
            source: "s3://bucket/low".into(),
            destination: "/data/low".into(),
            size_bytes: 100,
            priority: 1,
        };
        let req_high = StagingRequest {
            allocation_id: uuid::Uuid::new_v4(),
            source: "s3://bucket/high".into(),
            destination: "/data/high".into(),
            size_bytes: 200,
            priority: 9,
        };
        let req_mid = StagingRequest {
            allocation_id: uuid::Uuid::new_v4(),
            source: "s3://bucket/mid".into(),
            destination: "/data/mid".into(),
            size_bytes: 150,
            priority: 5,
        };

        let plan = StagingPlan::from_requests(vec![req_low, req_high, req_mid]);
        assert_eq!(plan.requests[0].priority, 9);
        assert_eq!(plan.requests[1].priority, 5);
        assert_eq!(plan.requests[2].priority, 1);
    }

    #[test]
    fn staging_plan_tie_broken_by_size_ascending() {
        // Two requests with same priority; smaller one should come first.
        let req_big = StagingRequest {
            allocation_id: uuid::Uuid::new_v4(),
            source: "s3://bucket/big".into(),
            destination: "/data/big".into(),
            size_bytes: 1000,
            priority: 5,
        };
        let req_small = StagingRequest {
            allocation_id: uuid::Uuid::new_v4(),
            source: "s3://bucket/small".into(),
            destination: "/data/small".into(),
            size_bytes: 100,
            priority: 5,
        };

        let plan = StagingPlan::from_requests(vec![req_big, req_small]);
        assert_eq!(plan.requests[0].size_bytes, 100);
        assert_eq!(plan.requests[1].size_bytes, 1000);
    }

    #[test]
    fn staging_plan_total_bytes_summed_correctly() {
        let requests = vec![
            StagingRequest {
                allocation_id: uuid::Uuid::new_v4(),
                source: "s3://a".into(),
                destination: "/a".into(),
                size_bytes: 500,
                priority: 3,
            },
            StagingRequest {
                allocation_id: uuid::Uuid::new_v4(),
                source: "s3://b".into(),
                destination: "/b".into(),
                size_bytes: 1500,
                priority: 7,
            },
        ];
        let plan = StagingPlan::from_requests(requests);
        assert_eq!(plan.total_bytes, 2000);
    }

    #[test]
    fn staging_plan_estimated_time_calculated() {
        let requests = vec![StagingRequest {
            allocation_id: uuid::Uuid::new_v4(),
            source: "s3://big".into(),
            destination: "/big".into(),
            // 10 GB → at 1 GB/s → 10 seconds
            size_bytes: 10 * 1_000_000_000,
            priority: 5,
        }];
        let plan = StagingPlan::from_requests(requests);
        assert_eq!(plan.estimated_time_secs, 10);
    }

    #[test]
    fn empty_requests_gives_empty_plan() {
        let plan = StagingPlan::from_requests(vec![]);
        assert!(plan.requests.is_empty());
        assert_eq!(plan.total_bytes, 0);
        assert_eq!(plan.estimated_time_secs, 0);
    }

    // ── DataStager::plan_staging tests ───────────────────────

    #[test]
    fn plan_staging_includes_pending_allocations_with_mounts() {
        let stager = DataStager::new();
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Pending)
            .preemption_class(5)
            .build();
        add_mount(&mut alloc, "s3://bucket/input", "/data/input");

        let plan = stager.plan_staging(&[alloc]);
        assert_eq!(plan.requests.len(), 1);
        assert_eq!(plan.requests[0].source, "s3://bucket/input");
        assert_eq!(plan.requests[0].destination, "/data/input");
        assert_eq!(plan.requests[0].priority, 5);
    }

    #[test]
    fn plan_staging_includes_staging_state_allocations() {
        let stager = DataStager::new();
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Staging)
            .build();
        add_mount(&mut alloc, "nfs://server/data", "/mnt/data");

        let plan = stager.plan_staging(&[alloc]);
        assert_eq!(plan.requests.len(), 1);
    }

    #[test]
    fn plan_staging_skips_running_allocations() {
        let stager = DataStager::new();
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        add_mount(&mut alloc, "s3://bucket/data", "/data");

        let plan = stager.plan_staging(&[alloc]);
        assert!(plan.requests.is_empty());
    }

    #[test]
    fn plan_staging_skips_allocations_with_no_mounts() {
        let stager = DataStager::new();
        let alloc = AllocationBuilder::new()
            .state(AllocationState::Pending)
            .build();
        // No mounts added.

        let plan = stager.plan_staging(&[alloc]);
        assert!(plan.requests.is_empty());
    }

    #[test]
    fn plan_staging_skips_terminal_allocations() {
        let stager = DataStager::new();
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        add_mount(&mut alloc, "s3://bucket/data", "/data");

        let plan = stager.plan_staging(&[alloc]);
        assert!(plan.requests.is_empty());
    }

    #[test]
    fn plan_staging_multiple_mounts_per_allocation() {
        let stager = DataStager::new();
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Pending)
            .build();
        add_mount(&mut alloc, "s3://bucket/input1", "/data/input1");
        add_mount(&mut alloc, "s3://bucket/input2", "/data/input2");

        let plan = stager.plan_staging(&[alloc]);
        assert_eq!(plan.requests.len(), 2);
    }

    #[test]
    fn plan_staging_multiple_allocations() {
        let stager = DataStager::new();
        let mut a1 = AllocationBuilder::new()
            .state(AllocationState::Pending)
            .preemption_class(8)
            .build();
        add_mount(&mut a1, "s3://bucket/a1", "/a1");

        let mut a2 = AllocationBuilder::new()
            .state(AllocationState::Pending)
            .preemption_class(2)
            .build();
        add_mount(&mut a2, "s3://bucket/a2", "/a2");

        let plan = stager.plan_staging(&[a1, a2]);
        assert_eq!(plan.requests.len(), 2);
        // High priority first.
        assert_eq!(plan.requests[0].priority, 8);
        assert_eq!(plan.requests[1].priority, 2);
    }

    // ── DataStager::should_prefetch tests ────────────────────

    #[test]
    fn should_prefetch_pending_with_mounts() {
        let stager = DataStager::new();
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Pending)
            .build();
        add_mount(&mut alloc, "s3://x", "/x");
        assert!(stager.should_prefetch(&alloc));
    }

    #[test]
    fn should_prefetch_staging_with_mounts() {
        let stager = DataStager::new();
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Staging)
            .build();
        add_mount(&mut alloc, "s3://x", "/x");
        assert!(stager.should_prefetch(&alloc));
    }

    #[test]
    fn should_not_prefetch_running() {
        let stager = DataStager::new();
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        add_mount(&mut alloc, "s3://x", "/x");
        assert!(!stager.should_prefetch(&alloc));
    }

    #[test]
    fn should_not_prefetch_no_mounts() {
        let stager = DataStager::new();
        let alloc = AllocationBuilder::new()
            .state(AllocationState::Pending)
            .build();
        assert!(!stager.should_prefetch(&alloc));
    }

    #[test]
    fn should_not_prefetch_cancelled() {
        let stager = DataStager::new();
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Cancelled)
            .build();
        add_mount(&mut alloc, "s3://x", "/x");
        assert!(!stager.should_prefetch(&alloc));
    }

    #[test]
    fn should_not_prefetch_failed() {
        let stager = DataStager::new();
        let mut alloc = AllocationBuilder::new()
            .state(AllocationState::Failed)
            .build();
        add_mount(&mut alloc, "s3://x", "/x");
        assert!(!stager.should_prefetch(&alloc));
    }
}
