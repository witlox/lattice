//! Topology-aware node selection.
//!
//! Delegates to hpc-scheduler-core, with a convenience wrapper for
//! Lattice-specific `TopologyHint` → `TopologyPreference` conversion.

use hpc_scheduler_core::topology as core_topology;
use hpc_scheduler_core::types::TopologyPreference;

use lattice_common::types::*;

/// Select nodes from available set, minimizing topology group span.
///
/// This is a thin wrapper that converts Lattice's `TopologyHint` to
/// hpc-scheduler-core's `TopologyPreference` before delegating.
pub fn select_nodes_topology_aware(
    requested: u32,
    hint: Option<&TopologyHint>,
    available: &[&Node],
    topology: &TopologyModel,
) -> Option<Vec<NodeId>> {
    let core_hint = hint.map(|h| match h {
        TopologyHint::Tight => TopologyPreference::Tight,
        TopologyHint::Spread => TopologyPreference::Spread,
        TopologyHint::Any => TopologyPreference::Any,
    });
    let core_topology = lattice_common::scheduler_core_impls::to_core_topology(topology);
    core_topology::select_nodes_topology_aware(
        requested,
        core_hint.as_ref(),
        available,
        &core_topology,
    )
}

/// Compute the group span for a set of node IDs.
///
/// Wraps hpc-scheduler-core's `group_span` with topology conversion.
pub fn group_span(nodes: &[NodeId], topology: &TopologyModel) -> u32 {
    let core_topology = lattice_common::scheduler_core_impls::to_core_topology(topology);
    core_topology::group_span(nodes, &core_topology)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::{create_node_batch, create_test_topology, NodeBuilder};

    #[test]
    fn tight_prefers_single_group() {
        let topology = create_test_topology(3, 8);
        let nodes_g0 = create_node_batch(8, 0);
        let nodes_g1 = create_node_batch(8, 1);
        let all: Vec<&Node> = nodes_g0.iter().chain(nodes_g1.iter()).collect();

        let selected =
            select_nodes_topology_aware(4, Some(&TopologyHint::Tight), &all, &topology).unwrap();
        assert_eq!(selected.len(), 4);
        let span = group_span(&selected, &topology);
        assert_eq!(span, 1);
    }

    #[test]
    fn spread_uses_multiple_groups() {
        let topology = create_test_topology(3, 8);
        let nodes_g0 = create_node_batch(8, 0);
        let nodes_g1 = create_node_batch(8, 1);
        let nodes_g2 = create_node_batch(8, 2);
        let all: Vec<&Node> = nodes_g0
            .iter()
            .chain(nodes_g1.iter())
            .chain(nodes_g2.iter())
            .collect();

        let selected =
            select_nodes_topology_aware(6, Some(&TopologyHint::Spread), &all, &topology).unwrap();
        assert_eq!(selected.len(), 6);
        let span = group_span(&selected, &topology);
        assert!(span >= 2);
    }

    #[test]
    fn any_selects_requested_count() {
        let topology = create_test_topology(2, 4);
        let nodes = create_node_batch(8, 0);
        let refs: Vec<&Node> = nodes.iter().collect();

        let selected =
            select_nodes_topology_aware(3, Some(&TopologyHint::Any), &refs, &topology).unwrap();
        assert_eq!(selected.len(), 3);
    }

    #[test]
    fn returns_none_when_insufficient_nodes() {
        let topology = create_test_topology(1, 4);
        let nodes = create_node_batch(4, 0);
        let refs: Vec<&Node> = nodes.iter().collect();

        let result = select_nodes_topology_aware(10, None, &refs, &topology);
        assert!(result.is_none());
    }

    #[test]
    fn tight_with_unequal_groups() {
        let topology = create_test_topology(2, 8);
        let g0: Vec<Node> = (0..3)
            .map(|i| {
                NodeBuilder::new()
                    .id(&format!("x1000c0s0b0n{i}"))
                    .group(0)
                    .build()
            })
            .collect();
        let g1 = create_node_batch(8, 1);
        let all: Vec<&Node> = g0.iter().chain(g1.iter()).collect();

        let selected =
            select_nodes_topology_aware(5, Some(&TopologyHint::Tight), &all, &topology).unwrap();
        assert_eq!(selected.len(), 5);
        let span = group_span(&selected, &topology);
        assert_eq!(span, 1);
    }
}
