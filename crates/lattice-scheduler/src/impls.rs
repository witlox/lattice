//! Re-exports of hpc-scheduler-core trait implementations and conversions.
//!
//! The actual trait implementations ([`Job`] for [`Allocation`], [`ComputeNode`]
//! for [`Node`]) live in `lattice-common` behind the `scheduler-core` feature flag.
//! This module re-exports the conversion helpers for convenience.

pub use lattice_common::scheduler_core_impls::{to_core_topology, to_core_weights};

#[cfg(test)]
mod tests {
    use hpc_scheduler_core::traits::{ComputeNode as _, Job as _};
    use lattice_common::types::*;
    use lattice_test_harness::fixtures::{AllocationBuilder, NodeBuilder};

    use super::*;

    #[test]
    fn allocation_implements_job() {
        let alloc = AllocationBuilder::new()
            .tenant("t1")
            .nodes(4)
            .preemption_class(7)
            .lifecycle_bounded(2)
            .build();

        assert_eq!(alloc.tenant_id(), "t1");
        assert_eq!(alloc.node_count_min(), 4);
        assert_eq!(alloc.preemption_class(), 7);
        assert_eq!(alloc.walltime(), Some(chrono::Duration::hours(2)));
        assert!(!alloc.is_running());
    }

    #[test]
    fn node_implements_compute_node() {
        let node = NodeBuilder::new()
            .id("n1")
            .group(3)
            .gpu_type("GH200")
            .feature("nvme_scratch")
            .build();

        assert_eq!(node.id(), "n1");
        assert_eq!(node.group(), 3);
        assert!(node.is_available());
        assert_eq!(node.gpu_type(), Some("GH200"));
        assert!(node.features().contains(&"nvme_scratch".to_string()));
    }

    #[test]
    fn owned_node_not_available() {
        let node = NodeBuilder::new()
            .id("n1")
            .owner(NodeOwnership {
                tenant: "t1".into(),
                vcluster: "vc".into(),
                allocation: uuid::Uuid::new_v4(),
                claimed_by: None,
                is_borrowed: false,
            })
            .build();

        assert!(!node.is_available());
    }

    #[test]
    fn cost_weights_conversion() {
        let lattice_weights = CostWeights::default();
        let core_weights = to_core_weights(&lattice_weights);
        assert!((core_weights.priority - lattice_weights.priority).abs() < f64::EPSILON);
        assert!((core_weights.topology - lattice_weights.topology).abs() < f64::EPSILON);
    }

    #[test]
    fn topology_conversion() {
        let lattice_topo = TopologyModel {
            groups: vec![TopologyGroup {
                id: 0,
                nodes: vec!["n1".into(), "n2".into()],
                adjacent_groups: vec![1],
            }],
        };
        let core_topo = to_core_topology(&lattice_topo);
        assert_eq!(core_topo.groups.len(), 1);
        assert_eq!(core_topo.groups[0].nodes.len(), 2);
    }

    #[test]
    fn unbounded_allocation_has_no_walltime() {
        let alloc = AllocationBuilder::new().lifecycle_unbounded().build();
        assert_eq!(alloc.walltime(), None);
    }

    #[test]
    fn sensitive_allocation_detected() {
        let alloc = AllocationBuilder::new().sensitive().build();
        assert!(alloc.is_sensitive());
    }
}
