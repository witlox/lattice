//! Conformance group management.
//!
//! Groups nodes by their conformance fingerprint (SHA-256 of driver/firmware/kernel)
//! and selects the largest group that can satisfy an allocation's constraints.

use std::collections::HashMap;

use lattice_common::types::*;

/// Group nodes by conformance fingerprint.
///
/// Nodes without a fingerprint are placed in a special "unknown" group.
pub fn group_by_conformance<'a>(nodes: &[&'a Node]) -> HashMap<String, Vec<&'a Node>> {
    let mut groups: HashMap<String, Vec<&'a Node>> = HashMap::new();
    for node in nodes {
        let key = node
            .conformance_fingerprint
            .clone()
            .unwrap_or_else(|| "unknown".into());
        groups.entry(key).or_default().push(node);
    }
    groups
}

/// Select the largest conformance group that has enough nodes.
///
/// Returns node IDs from the largest conformant group that can satisfy
/// the request, or `None` if no single group is large enough.
pub fn select_conformant_nodes(requested: u32, nodes: &[&Node]) -> Option<Vec<NodeId>> {
    let groups = group_by_conformance(nodes);

    // Sort groups by size descending
    let mut sorted: Vec<(String, Vec<&Node>)> = groups.into_iter().collect();
    sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    // Return the first group large enough
    for (_fingerprint, group_nodes) in &sorted {
        if group_nodes.len() >= requested as usize {
            return Some(
                group_nodes
                    .iter()
                    .take(requested as usize)
                    .map(|n| n.id.clone())
                    .collect(),
            );
        }
    }

    None
}

/// Compute f₉ conformance fitness score for a set of candidate nodes.
///
/// Score = largest_conformance_group_size / requested_nodes
/// Returns 1.0 when all candidates share the same fingerprint.
pub fn conformance_fitness(candidates: &[&Node], requested_nodes: u32) -> f64 {
    if requested_nodes == 0 {
        return 1.0;
    }
    let groups = group_by_conformance(candidates);
    let max_group_size = groups
        .values()
        .map(|g: &Vec<&Node>| g.len())
        .max()
        .unwrap_or(0);
    max_group_size as f64 / requested_nodes as f64
}

/// Filter nodes that satisfy an allocation's hardware constraints.
pub fn filter_by_constraints<'a>(
    nodes: &[&'a Node],
    constraints: &ResourceConstraints,
) -> Vec<&'a Node> {
    nodes
        .iter()
        .filter(|node| {
            // GPU type constraint
            if let Some(ref required_gpu) = constraints.gpu_type {
                if let Some(ref node_gpu) = node.capabilities.gpu_type {
                    if node_gpu != required_gpu {
                        return false;
                    }
                } else {
                    return false;
                }
            }

            // Feature constraints
            for feature in &constraints.features {
                if !node.capabilities.features.contains(feature) {
                    return false;
                }
            }

            true
        })
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::NodeBuilder;

    #[test]
    fn group_by_conformance_separates_fingerprints() {
        let n1 = NodeBuilder::new().id("n1").conformance("fp-a").build();
        let n2 = NodeBuilder::new().id("n2").conformance("fp-a").build();
        let n3 = NodeBuilder::new().id("n3").conformance("fp-b").build();
        let nodes: Vec<&Node> = vec![&n1, &n2, &n3];

        let groups = group_by_conformance(&nodes);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups["fp-a"].len(), 2);
        assert_eq!(groups["fp-b"].len(), 1);
    }

    #[test]
    fn group_by_conformance_unknown_fingerprint() {
        let n1 = NodeBuilder::new().id("n1").build(); // no fingerprint
        let n2 = NodeBuilder::new().id("n2").conformance("fp-a").build();
        let nodes: Vec<&Node> = vec![&n1, &n2];

        let groups = group_by_conformance(&nodes);
        assert_eq!(groups["unknown"].len(), 1);
        assert_eq!(groups["fp-a"].len(), 1);
    }

    #[test]
    fn select_conformant_picks_largest_group() {
        let n1 = NodeBuilder::new().id("n1").conformance("fp-a").build();
        let n2 = NodeBuilder::new().id("n2").conformance("fp-a").build();
        let n3 = NodeBuilder::new().id("n3").conformance("fp-a").build();
        let n4 = NodeBuilder::new().id("n4").conformance("fp-b").build();
        let n5 = NodeBuilder::new().id("n5").conformance("fp-b").build();
        let nodes: Vec<&Node> = vec![&n1, &n2, &n3, &n4, &n5];

        let selected = select_conformant_nodes(2, &nodes).unwrap();
        assert_eq!(selected.len(), 2);
        // Should come from fp-a (the larger group)
    }

    #[test]
    fn select_conformant_returns_none_if_no_group_big_enough() {
        let n1 = NodeBuilder::new().id("n1").conformance("fp-a").build();
        let n2 = NodeBuilder::new().id("n2").conformance("fp-b").build();
        let nodes: Vec<&Node> = vec![&n1, &n2];

        let result = select_conformant_nodes(2, &nodes);
        assert!(result.is_none());
    }

    #[test]
    fn conformance_fitness_all_same() {
        let n1 = NodeBuilder::new().id("n1").conformance("fp-a").build();
        let n2 = NodeBuilder::new().id("n2").conformance("fp-a").build();
        let nodes: Vec<&Node> = vec![&n1, &n2];

        let score = conformance_fitness(&nodes, 2);
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn conformance_fitness_mixed() {
        let n1 = NodeBuilder::new().id("n1").conformance("fp-a").build();
        let n2 = NodeBuilder::new().id("n2").conformance("fp-a").build();
        let n3 = NodeBuilder::new().id("n3").conformance("fp-b").build();
        let nodes: Vec<&Node> = vec![&n1, &n2, &n3];

        // Largest group = 2, requested = 3
        let score = conformance_fitness(&nodes, 3);
        assert!((score - 2.0 / 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn filter_by_gpu_type() {
        let n1 = NodeBuilder::new().id("n1").gpu_type("GH200").build();
        let n2 = NodeBuilder::new().id("n2").gpu_type("MI300X").build();
        let n3 = NodeBuilder::new().id("n3").gpu_type("GH200").build();
        let nodes: Vec<&Node> = vec![&n1, &n2, &n3];

        let constraints = ResourceConstraints {
            gpu_type: Some("GH200".into()),
            ..Default::default()
        };
        let filtered = filter_by_constraints(&nodes, &constraints);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_by_features() {
        let n1 = NodeBuilder::new().id("n1").feature("nvme_scratch").build();
        let n2 = NodeBuilder::new().id("n2").build();
        let n3 = NodeBuilder::new()
            .id("n3")
            .feature("nvme_scratch")
            .feature("slingshot")
            .build();
        let nodes: Vec<&Node> = vec![&n1, &n2, &n3];

        let constraints = ResourceConstraints {
            features: vec!["nvme_scratch".into()],
            ..Default::default()
        };
        let filtered = filter_by_constraints(&nodes, &constraints);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().any(|n| n.id == "n1"));
        assert!(filtered.iter().any(|n| n.id == "n3"));
    }

    #[test]
    fn filter_no_constraints_returns_all() {
        let n1 = NodeBuilder::new().id("n1").build();
        let n2 = NodeBuilder::new().id("n2").build();
        let nodes: Vec<&Node> = vec![&n1, &n2];

        let constraints = ResourceConstraints::default();
        let filtered = filter_by_constraints(&nodes, &constraints);
        assert_eq!(filtered.len(), 2);
    }
}
