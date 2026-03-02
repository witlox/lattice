//! Topology-aware node selection.
//!
//! Implements dragonfly group packing: prefers placing allocations
//! within the fewest possible topology groups for optimal network performance.

use std::collections::HashMap;

use lattice_common::types::*;

/// Select nodes from available set, minimizing topology group span.
///
/// Returns the selected node IDs, or `None` if not enough nodes are available.
pub fn select_nodes_topology_aware(
    requested: u32,
    hint: Option<&TopologyHint>,
    available: &[&Node],
    topology: &TopologyModel,
) -> Option<Vec<NodeId>> {
    if available.len() < requested as usize {
        return None;
    }

    match hint {
        Some(TopologyHint::Spread) => select_spread(requested, available, topology),
        Some(TopologyHint::Tight) | None => select_tight(requested, available, topology),
        Some(TopologyHint::Any) => select_any(requested, available),
    }
}

/// Tight packing: minimize group span (default for HPC).
fn select_tight(
    requested: u32,
    available: &[&Node],
    topology: &TopologyModel,
) -> Option<Vec<NodeId>> {
    // Group available nodes by their topology group
    let mut by_group: HashMap<GroupId, Vec<&Node>> = HashMap::new();
    for node in available {
        by_group.entry(node.group).or_default().push(node);
    }

    // Sort groups by size descending (largest group first)
    let mut groups: Vec<(GroupId, Vec<&Node>)> = by_group.into_iter().collect();
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    // Try single-group placement first
    for (_, nodes) in &groups {
        if nodes.len() >= requested as usize {
            return Some(
                nodes
                    .iter()
                    .take(requested as usize)
                    .map(|n| n.id.clone())
                    .collect(),
            );
        }
    }

    // Multi-group: greedily pick from largest groups, preferring adjacent groups
    let mut selected = Vec::new();
    let mut remaining = requested as usize;
    let mut used_groups: Vec<GroupId> = Vec::new();

    // Sort by adjacency to already-selected groups (initially by size)
    while remaining > 0 && !groups.is_empty() {
        // If we have used groups, prefer adjacent ones
        if !used_groups.is_empty() {
            let adjacent: Vec<GroupId> = topology
                .groups
                .iter()
                .filter(|g| used_groups.contains(&g.id))
                .flat_map(|g| g.adjacent_groups.iter().copied())
                .collect();

            groups.sort_by(|a, b| {
                let a_adj = adjacent.contains(&a.0);
                let b_adj = adjacent.contains(&b.0);
                match (a_adj, b_adj) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => b.1.len().cmp(&a.1.len()),
                }
            });
        }

        let (gid, nodes) = groups.remove(0);
        let take = remaining.min(nodes.len());
        for node in nodes.iter().take(take) {
            selected.push(node.id.clone());
        }
        remaining -= take;
        used_groups.push(gid);
    }

    if selected.len() >= requested as usize {
        Some(selected)
    } else {
        None
    }
}

/// Spread: maximize group span for bisection bandwidth.
fn select_spread(
    requested: u32,
    available: &[&Node],
    _topology: &TopologyModel,
) -> Option<Vec<NodeId>> {
    // Group nodes by topology group
    let mut by_group: HashMap<GroupId, Vec<&Node>> = HashMap::new();
    for node in available {
        by_group.entry(node.group).or_default().push(node);
    }

    let num_groups = by_group.len();
    if num_groups == 0 {
        return None;
    }

    // Round-robin across groups
    let mut selected = Vec::new();
    let per_group = (requested as usize).div_ceil(num_groups);

    let mut groups: Vec<(GroupId, Vec<&Node>)> = by_group.into_iter().collect();
    groups.sort_by_key(|(gid, _)| *gid);

    for (_, nodes) in &groups {
        let take = per_group.min(nodes.len());
        for node in nodes.iter().take(take) {
            selected.push(node.id.clone());
        }
    }

    selected.truncate(requested as usize);
    if selected.len() >= requested as usize {
        Some(selected)
    } else {
        None
    }
}

/// Any: no topology preference, just pick available nodes.
fn select_any(requested: u32, available: &[&Node]) -> Option<Vec<NodeId>> {
    if available.len() < requested as usize {
        return None;
    }
    Some(
        available
            .iter()
            .take(requested as usize)
            .map(|n| n.id.clone())
            .collect(),
    )
}

/// Compute the group span for a set of nodes.
pub fn group_span(nodes: &[NodeId], topology: &TopologyModel) -> u32 {
    let mut groups = std::collections::HashSet::new();
    for node_id in nodes {
        for group in &topology.groups {
            if group.nodes.contains(node_id) {
                groups.insert(group.id);
            }
        }
    }
    groups.len() as u32
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
        // All should be from the same group
        let span = group_span(&selected, &topology);
        assert_eq!(span, 1);
    }

    #[test]
    fn tight_spills_to_adjacent_groups() {
        let topology = create_test_topology(3, 4);
        let nodes_g0 = create_node_batch(4, 0);
        let nodes_g1 = create_node_batch(4, 1);
        let nodes_g2 = create_node_batch(4, 2);
        let all: Vec<&Node> = nodes_g0
            .iter()
            .chain(nodes_g1.iter())
            .chain(nodes_g2.iter())
            .collect();

        let selected =
            select_nodes_topology_aware(6, Some(&TopologyHint::Tight), &all, &topology).unwrap();
        assert_eq!(selected.len(), 6);
        let span = group_span(&selected, &topology);
        assert_eq!(span, 2);
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
        // Spread should use at least 2 groups, ideally 3
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
    fn group_span_single_group() {
        let topology = create_test_topology(3, 4);
        let node_ids = vec!["x1000c0s0b0n0".into(), "x1000c0s0b0n1".into()];
        assert_eq!(group_span(&node_ids, &topology), 1);
    }

    #[test]
    fn group_span_multi_group() {
        let topology = create_test_topology(3, 4);
        let node_ids = vec![
            "x1000c0s0b0n0".into(),
            "x1000c0s1b0n0".into(),
            "x1000c0s2b0n0".into(),
        ];
        assert_eq!(group_span(&node_ids, &topology), 3);
    }

    #[test]
    fn tight_with_unequal_groups() {
        let topology = create_test_topology(2, 8);
        // Group 0 has 3 available nodes, group 1 has 8
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
        // Should pick from group 1 (has 8, can fit 5 in one group)
        let span = group_span(&selected, &topology);
        assert_eq!(span, 1);
    }
}
