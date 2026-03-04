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

            // Unified memory constraint
            if constraints.require_unified_memory {
                let has_unified = node
                    .capabilities
                    .memory_topology
                    .as_ref()
                    .is_some_and(|topo| {
                        topo.domains
                            .iter()
                            .any(|d| d.domain_type == MemoryDomainType::Unified)
                    });
                if !has_unified {
                    return false;
                }
            }

            // CXL memory constraint: when disallowed, skip nodes that only have CXL memory
            // (nodes without memory topology are always allowed)
            if !constraints.allow_cxl_memory {
                if let Some(ref topo) = node.capabilities.memory_topology {
                    let non_cxl_capacity: u64 = topo
                        .domains
                        .iter()
                        .filter(|d| d.domain_type != MemoryDomainType::CxlAttached)
                        .map(|d| d.capacity_bytes)
                        .sum();
                    if non_cxl_capacity == 0 && topo.total_capacity_bytes > 0 {
                        return false;
                    }
                }
            }

            true
        })
        .copied()
        .collect()
}

/// Compute memory locality score for a node: fraction of resources sharing a memory domain.
///
/// Returns 1.0 if all requested resources fit in one domain, 0.5 if no topology info.
pub fn memory_locality_score(node: &Node) -> f64 {
    let topo = match &node.capabilities.memory_topology {
        Some(t) => t,
        None => return 0.5, // neutral when unknown
    };

    if topo.domains.len() <= 1 {
        return 1.0; // single domain = perfect locality
    }

    // For each domain, count how many of the node's resources it covers
    let total_cpus = node.capabilities.cpu_cores;
    let total_gpus = node.capabilities.gpu_count;
    if total_cpus == 0 && total_gpus == 0 {
        return 1.0;
    }

    let total_resources = total_cpus as f64 + total_gpus as f64;

    // Find the domain with the most attached resources
    let best_domain_resources = topo
        .domains
        .iter()
        .map(|d| d.attached_cpus.len() as f64 + d.attached_gpus.len() as f64)
        .fold(0.0_f64, f64::max);

    (best_domain_resources / total_resources).min(1.0)
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

    // ── Memory constraint tests ──

    fn unified_memory_topology() -> MemoryTopology {
        MemoryTopology {
            domains: vec![MemoryDomain {
                id: 0,
                domain_type: MemoryDomainType::Unified,
                capacity_bytes: 512 * 1024 * 1024 * 1024,
                numa_node: Some(0),
                attached_cpus: vec![0, 1, 2, 3],
                attached_gpus: vec![0],
            }],
            interconnects: Vec::new(),
            total_capacity_bytes: 512 * 1024 * 1024 * 1024,
        }
    }

    fn dual_numa_topology() -> MemoryTopology {
        MemoryTopology {
            domains: vec![
                MemoryDomain {
                    id: 0,
                    domain_type: MemoryDomainType::Dram,
                    capacity_bytes: 256 * 1024 * 1024 * 1024,
                    numa_node: Some(0),
                    attached_cpus: vec![0, 1],
                    attached_gpus: vec![0],
                },
                MemoryDomain {
                    id: 1,
                    domain_type: MemoryDomainType::Dram,
                    capacity_bytes: 256 * 1024 * 1024 * 1024,
                    numa_node: Some(1),
                    attached_cpus: vec![2, 3],
                    attached_gpus: vec![1],
                },
            ],
            interconnects: vec![MemoryInterconnect {
                domain_a: 0,
                domain_b: 1,
                link_type: MemoryLinkType::NumaLink,
                bandwidth_gbps: 50.0,
                latency_ns: 100,
            }],
            total_capacity_bytes: 512 * 1024 * 1024 * 1024,
        }
    }

    fn cxl_only_topology() -> MemoryTopology {
        MemoryTopology {
            domains: vec![MemoryDomain {
                id: 100,
                domain_type: MemoryDomainType::CxlAttached,
                capacity_bytes: 1024 * 1024 * 1024 * 1024,
                numa_node: None,
                attached_cpus: vec![],
                attached_gpus: vec![],
            }],
            interconnects: Vec::new(),
            total_capacity_bytes: 1024 * 1024 * 1024 * 1024,
        }
    }

    #[test]
    fn filter_require_unified_memory_passes() {
        let n1 = NodeBuilder::new()
            .id("n1")
            .memory_topology(unified_memory_topology())
            .build();
        let n2 = NodeBuilder::new()
            .id("n2")
            .memory_topology(dual_numa_topology())
            .build();
        let nodes: Vec<&Node> = vec![&n1, &n2];

        let constraints = ResourceConstraints {
            require_unified_memory: true,
            ..Default::default()
        };
        let filtered = filter_by_constraints(&nodes, &constraints);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "n1");
    }

    #[test]
    fn filter_require_unified_memory_rejects_none() {
        let n1 = NodeBuilder::new().id("n1").build(); // no memory topology
        let nodes: Vec<&Node> = vec![&n1];

        let constraints = ResourceConstraints {
            require_unified_memory: true,
            ..Default::default()
        };
        let filtered = filter_by_constraints(&nodes, &constraints);
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_disallow_cxl_rejects_cxl_only() {
        let n1 = NodeBuilder::new()
            .id("n1")
            .memory_topology(cxl_only_topology())
            .build();
        let n2 = NodeBuilder::new()
            .id("n2")
            .memory_topology(dual_numa_topology())
            .build();
        let nodes: Vec<&Node> = vec![&n1, &n2];

        let constraints = ResourceConstraints {
            allow_cxl_memory: false,
            ..Default::default()
        };
        let filtered = filter_by_constraints(&nodes, &constraints);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "n2");
    }

    #[test]
    fn filter_allow_cxl_keeps_all() {
        let n1 = NodeBuilder::new()
            .id("n1")
            .memory_topology(cxl_only_topology())
            .build();
        let nodes: Vec<&Node> = vec![&n1];

        let constraints = ResourceConstraints {
            allow_cxl_memory: true,
            ..Default::default()
        };
        let filtered = filter_by_constraints(&nodes, &constraints);
        assert_eq!(filtered.len(), 1);
    }

    // ── Memory locality score tests ──

    #[test]
    fn locality_score_no_topology_returns_neutral() {
        let n = NodeBuilder::new().id("n1").build();
        assert!((memory_locality_score(&n) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn locality_score_single_domain_returns_one() {
        let n = NodeBuilder::new()
            .id("n1")
            .memory_topology(unified_memory_topology())
            .build();
        assert!((memory_locality_score(&n) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn locality_score_dual_numa_below_one() {
        let n = NodeBuilder::new()
            .id("n1")
            .cpu_cores(4)
            .gpu_count(2)
            .memory_topology(dual_numa_topology())
            .build();
        let score = memory_locality_score(&n);
        // Best domain has 2 CPUs + 1 GPU = 3 resources out of 6 total = 0.5
        assert!(score > 0.0 && score <= 1.0);
        assert!((score - 0.5).abs() < f64::EPSILON);
    }
}
