//! Topology-aware node selection.
//!
//! Implements dragonfly group packing: prefers placing jobs
//! within the fewest possible topology groups for optimal network performance.

use std::collections::HashMap;

use crate::traits::ComputeNode;
use crate::types::{TopologyModel, TopologyPreference};

/// Select nodes from available set, minimizing topology group span.
///
/// Returns the selected node IDs, or `None` if not enough nodes are available.
pub fn select_nodes_topology_aware<N: ComputeNode>(
    requested: u32,
    hint: Option<&TopologyPreference>,
    available: &[&N],
    topology: &TopologyModel,
) -> Option<Vec<String>> {
    if available.len() < requested as usize {
        return None;
    }

    match hint {
        Some(TopologyPreference::Spread) => select_spread(requested, available, topology),
        Some(TopologyPreference::Tight) | None => select_tight(requested, available, topology),
        Some(TopologyPreference::Any) => select_any(requested, available),
    }
}

/// Tight packing: minimize group span (default for HPC).
fn select_tight<N: ComputeNode>(
    requested: u32,
    available: &[&N],
    topology: &TopologyModel,
) -> Option<Vec<String>> {
    let mut by_group: HashMap<u32, Vec<&N>> = HashMap::new();
    for node in available {
        by_group.entry(node.group()).or_default().push(node);
    }

    let mut groups: Vec<(u32, Vec<&N>)> = by_group.into_iter().collect();
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    // Try single-group placement first
    for (_, nodes) in &groups {
        if nodes.len() >= requested as usize {
            return Some(
                nodes
                    .iter()
                    .take(requested as usize)
                    .map(|n| n.id().to_string())
                    .collect(),
            );
        }
    }

    // Multi-group: greedily pick from largest groups, preferring adjacent
    let mut selected = Vec::new();
    let mut remaining = requested as usize;
    let mut used_groups: Vec<u32> = Vec::new();

    while remaining > 0 && !groups.is_empty() {
        if !used_groups.is_empty() {
            let adjacent: Vec<u32> = topology
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
            selected.push(node.id().to_string());
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
fn select_spread<N: ComputeNode>(
    requested: u32,
    available: &[&N],
    _topology: &TopologyModel,
) -> Option<Vec<String>> {
    let mut by_group: HashMap<u32, Vec<&N>> = HashMap::new();
    for node in available {
        by_group.entry(node.group()).or_default().push(node);
    }

    let num_groups = by_group.len();
    if num_groups == 0 {
        return None;
    }

    let mut selected = Vec::new();
    let per_group = (requested as usize).div_ceil(num_groups);

    let mut groups: Vec<(u32, Vec<&N>)> = by_group.into_iter().collect();
    groups.sort_by_key(|(gid, _)| *gid);

    for (_, nodes) in &groups {
        let take = per_group.min(nodes.len());
        for node in nodes.iter().take(take) {
            selected.push(node.id().to_string());
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
fn select_any<N: ComputeNode>(requested: u32, available: &[&N]) -> Option<Vec<String>> {
    if available.len() < requested as usize {
        return None;
    }
    Some(
        available
            .iter()
            .take(requested as usize)
            .map(|n| n.id().to_string())
            .collect(),
    )
}

/// Compute the group span for a set of node IDs.
pub fn group_span(nodes: &[String], topology: &TopologyModel) -> u32 {
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
    use crate::types::TopologyGroup;

    struct TestNode {
        id: String,
        group: u32,
    }

    impl ComputeNode for TestNode {
        fn id(&self) -> &str {
            &self.id
        }
        fn group(&self) -> u32 {
            self.group
        }
        fn is_available(&self) -> bool {
            true
        }
        fn conformance_fingerprint(&self) -> Option<&str> {
            None
        }
        fn gpu_type(&self) -> Option<&str> {
            None
        }
        fn features(&self) -> &[String] {
            &[]
        }
        fn cpu_cores(&self) -> u32 {
            4
        }
        fn gpu_count(&self) -> u32 {
            0
        }
        fn memory_topology(&self) -> Option<crate::types::MemoryTopologyInfo> {
            None
        }
    }

    fn make_nodes(count: usize, group: u32) -> Vec<TestNode> {
        (0..count)
            .map(|i| TestNode {
                id: format!("g{group}n{i}"),
                group,
            })
            .collect()
    }

    fn make_topology(num_groups: u32, nodes_per_group: usize) -> TopologyModel {
        let groups = (0..num_groups)
            .map(|g| {
                let adj: Vec<u32> = (0..num_groups).filter(|&x| x != g).collect();
                TopologyGroup {
                    id: g,
                    nodes: (0..nodes_per_group).map(|i| format!("g{g}n{i}")).collect(),
                    adjacent_groups: adj,
                }
            })
            .collect();
        TopologyModel { groups }
    }

    #[test]
    fn tight_prefers_single_group() {
        let topology = make_topology(3, 8);
        let g0 = make_nodes(8, 0);
        let g1 = make_nodes(8, 1);
        let all: Vec<&TestNode> = g0.iter().chain(g1.iter()).collect();

        let selected =
            select_nodes_topology_aware(4, Some(&TopologyPreference::Tight), &all, &topology)
                .unwrap();
        assert_eq!(selected.len(), 4);
        let span = group_span(&selected, &topology);
        assert_eq!(span, 1);
    }

    #[test]
    fn any_selects_requested_count() {
        let topology = make_topology(2, 4);
        let nodes = make_nodes(8, 0);
        let refs: Vec<&TestNode> = nodes.iter().collect();

        let selected =
            select_nodes_topology_aware(3, Some(&TopologyPreference::Any), &refs, &topology)
                .unwrap();
        assert_eq!(selected.len(), 3);
    }

    #[test]
    fn returns_none_when_insufficient_nodes() {
        let topology = make_topology(1, 4);
        let nodes = make_nodes(4, 0);
        let refs: Vec<&TestNode> = nodes.iter().collect();

        let result = select_nodes_topology_aware(10, None, &refs, &topology);
        assert!(result.is_none());
    }
}
