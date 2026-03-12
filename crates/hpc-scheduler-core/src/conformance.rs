//! Conformance group management.
//!
//! Groups nodes by their conformance fingerprint (hash of driver/firmware/kernel)
//! and selects the largest group that can satisfy a job's constraints.

use std::collections::HashMap;

use crate::traits::ComputeNode;
use crate::types::{MemoryDomainKind, NodeConstraints};

/// Group nodes by conformance fingerprint.
///
/// Nodes without a fingerprint are placed in a special "unknown" group.
pub fn group_by_conformance<'a, N: ComputeNode>(nodes: &[&'a N]) -> HashMap<String, Vec<&'a N>> {
    let mut groups: HashMap<String, Vec<&'a N>> = HashMap::new();
    for node in nodes {
        let key = node
            .conformance_fingerprint()
            .unwrap_or("unknown")
            .to_string();
        groups.entry(key).or_default().push(node);
    }
    groups
}

/// Select the largest conformance group that has enough nodes.
///
/// Returns node IDs from the largest conformant group that can satisfy
/// the request, or `None` if no single group is large enough.
pub fn select_conformant_nodes<N: ComputeNode>(
    requested: u32,
    nodes: &[&N],
) -> Option<Vec<String>> {
    let groups = group_by_conformance(nodes);

    let mut sorted: Vec<(String, Vec<&N>)> = groups.into_iter().collect();
    sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    for (_fingerprint, group_nodes) in &sorted {
        if group_nodes.len() >= requested as usize {
            return Some(
                group_nodes
                    .iter()
                    .take(requested as usize)
                    .map(|n| n.id().to_string())
                    .collect(),
            );
        }
    }

    None
}

/// Compute f₉ conformance fitness score for a set of candidate nodes.
///
/// Score = largest_conformance_group_size / requested_nodes.
/// Returns 1.0 when all candidates share the same fingerprint.
pub fn conformance_fitness<N: ComputeNode>(candidates: &[&N], requested_nodes: u32) -> f64 {
    if requested_nodes == 0 {
        return 1.0;
    }
    let groups = group_by_conformance(candidates);
    let max_group_size = groups.values().map(|g| g.len()).max().unwrap_or(0);
    max_group_size as f64 / requested_nodes as f64
}

/// Filter nodes that satisfy hardware constraints.
pub fn filter_by_constraints<'a, N: ComputeNode>(
    nodes: &[&'a N],
    constraints: &NodeConstraints,
) -> Vec<&'a N> {
    nodes
        .iter()
        .filter(|node| {
            // GPU type constraint
            if let Some(ref required_gpu) = constraints.gpu_type {
                match node.gpu_type() {
                    Some(node_gpu) if node_gpu == required_gpu => {}
                    _ => return false,
                }
            }

            // Feature constraints
            let node_features = node.features();
            for feature in &constraints.features {
                if !node_features.contains(feature) {
                    return false;
                }
            }

            // Unified memory constraint
            if constraints.require_unified_memory {
                let has_unified = node.memory_topology().is_some_and(|topo| {
                    topo.domains
                        .iter()
                        .any(|d| d.domain_type == MemoryDomainKind::Unified)
                });
                if !has_unified {
                    return false;
                }
            }

            // CXL memory constraint
            if !constraints.allow_cxl_memory {
                if let Some(topo) = node.memory_topology() {
                    let non_cxl_capacity: u64 = topo
                        .domains
                        .iter()
                        .filter(|d| d.domain_type != MemoryDomainKind::CxlAttached)
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
/// Returns 1.0 if all resources fit in one domain, 0.5 if no topology info.
pub fn memory_locality_score<N: ComputeNode>(node: &N) -> f64 {
    let topo = match node.memory_topology() {
        Some(t) => t,
        None => return 0.5,
    };

    if topo.domains.len() <= 1 {
        return 1.0;
    }

    let total_cpus = node.cpu_cores();
    let total_gpus = node.gpu_count();
    if total_cpus == 0 && total_gpus == 0 {
        return 1.0;
    }

    let total_resources = total_cpus as f64 + total_gpus as f64;

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
    use crate::types::{
        MemoryDomainInfo, MemoryDomainKind, MemoryInterconnectInfo, MemoryLinkKind,
        MemoryTopologyInfo,
    };

    struct TestNode {
        id: String,
        group: u32,
        conformance: Option<String>,
        gpu_type: Option<String>,
        features: Vec<String>,
        cpu_cores: u32,
        gpu_count: u32,
        memory_topology: Option<MemoryTopologyInfo>,
    }

    impl TestNode {
        fn new(id: &str) -> Self {
            Self {
                id: id.to_string(),
                group: 0,
                conformance: None,
                gpu_type: None,
                features: vec![],
                cpu_cores: 4,
                gpu_count: 2,
                memory_topology: None,
            }
        }

        fn with_conformance(mut self, fp: &str) -> Self {
            self.conformance = Some(fp.to_string());
            self
        }

        fn with_gpu_type(mut self, gpu: &str) -> Self {
            self.gpu_type = Some(gpu.to_string());
            self
        }

        fn with_feature(mut self, f: &str) -> Self {
            self.features.push(f.to_string());
            self
        }

        fn with_memory_topology(mut self, topo: MemoryTopologyInfo) -> Self {
            self.memory_topology = Some(topo);
            self
        }
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
            self.conformance.as_deref()
        }
        fn gpu_type(&self) -> Option<&str> {
            self.gpu_type.as_deref()
        }
        fn features(&self) -> &[String] {
            &self.features
        }
        fn cpu_cores(&self) -> u32 {
            self.cpu_cores
        }
        fn gpu_count(&self) -> u32 {
            self.gpu_count
        }
        fn memory_topology(&self) -> Option<MemoryTopologyInfo> {
            self.memory_topology.clone()
        }
    }

    #[test]
    fn group_by_conformance_separates_fingerprints() {
        let n1 = TestNode::new("n1").with_conformance("fp-a");
        let n2 = TestNode::new("n2").with_conformance("fp-a");
        let n3 = TestNode::new("n3").with_conformance("fp-b");
        let nodes: Vec<&TestNode> = vec![&n1, &n2, &n3];

        let groups = group_by_conformance(&nodes);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups["fp-a"].len(), 2);
        assert_eq!(groups["fp-b"].len(), 1);
    }

    #[test]
    fn select_conformant_picks_largest_group() {
        let n1 = TestNode::new("n1").with_conformance("fp-a");
        let n2 = TestNode::new("n2").with_conformance("fp-a");
        let n3 = TestNode::new("n3").with_conformance("fp-a");
        let n4 = TestNode::new("n4").with_conformance("fp-b");
        let nodes: Vec<&TestNode> = vec![&n1, &n2, &n3, &n4];

        let selected = select_conformant_nodes(2, &nodes).unwrap();
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn filter_by_gpu_type() {
        let n1 = TestNode::new("n1").with_gpu_type("GH200");
        let n2 = TestNode::new("n2").with_gpu_type("MI300X");
        let n3 = TestNode::new("n3").with_gpu_type("GH200");
        let nodes: Vec<&TestNode> = vec![&n1, &n2, &n3];

        let constraints = NodeConstraints {
            gpu_type: Some("GH200".into()),
            ..Default::default()
        };
        let filtered = filter_by_constraints(&nodes, &constraints);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_by_features() {
        let n1 = TestNode::new("n1").with_feature("nvme_scratch");
        let n2 = TestNode::new("n2");
        let nodes: Vec<&TestNode> = vec![&n1, &n2];

        let constraints = NodeConstraints {
            features: vec!["nvme_scratch".into()],
            ..Default::default()
        };
        let filtered = filter_by_constraints(&nodes, &constraints);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_require_unified_memory() {
        let n1 = TestNode::new("n1").with_memory_topology(MemoryTopologyInfo {
            domains: vec![MemoryDomainInfo {
                id: 0,
                domain_type: MemoryDomainKind::Unified,
                capacity_bytes: 512 * 1024 * 1024 * 1024,
                numa_node: Some(0),
                attached_cpus: vec![0, 1],
                attached_gpus: vec![0],
            }],
            interconnects: Vec::new(),
            total_capacity_bytes: 512 * 1024 * 1024 * 1024,
        });
        let n2 = TestNode::new("n2").with_memory_topology(MemoryTopologyInfo {
            domains: vec![MemoryDomainInfo {
                id: 0,
                domain_type: MemoryDomainKind::Dram,
                capacity_bytes: 256 * 1024 * 1024 * 1024,
                numa_node: Some(0),
                attached_cpus: vec![0, 1],
                attached_gpus: vec![0],
            }],
            interconnects: Vec::new(),
            total_capacity_bytes: 256 * 1024 * 1024 * 1024,
        });
        let nodes: Vec<&TestNode> = vec![&n1, &n2];

        let constraints = NodeConstraints {
            require_unified_memory: true,
            ..Default::default()
        };
        let filtered = filter_by_constraints(&nodes, &constraints);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id(), "n1");
    }

    #[test]
    fn locality_score_no_topology_returns_neutral() {
        let n = TestNode::new("n1");
        assert!((memory_locality_score(&n) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn locality_score_single_domain_returns_one() {
        let n = TestNode::new("n1").with_memory_topology(MemoryTopologyInfo {
            domains: vec![MemoryDomainInfo {
                id: 0,
                domain_type: MemoryDomainKind::Unified,
                capacity_bytes: 512 * 1024 * 1024 * 1024,
                numa_node: Some(0),
                attached_cpus: vec![0, 1, 2, 3],
                attached_gpus: vec![0],
            }],
            interconnects: Vec::new(),
            total_capacity_bytes: 512 * 1024 * 1024 * 1024,
        });
        assert!((memory_locality_score(&n) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn locality_score_dual_numa() {
        let mut n = TestNode::new("n1");
        n.cpu_cores = 4;
        n.gpu_count = 2;
        n = n.with_memory_topology(MemoryTopologyInfo {
            domains: vec![
                MemoryDomainInfo {
                    id: 0,
                    domain_type: MemoryDomainKind::Dram,
                    capacity_bytes: 256 * 1024 * 1024 * 1024,
                    numa_node: Some(0),
                    attached_cpus: vec![0, 1],
                    attached_gpus: vec![0],
                },
                MemoryDomainInfo {
                    id: 1,
                    domain_type: MemoryDomainKind::Dram,
                    capacity_bytes: 256 * 1024 * 1024 * 1024,
                    numa_node: Some(1),
                    attached_cpus: vec![2, 3],
                    attached_gpus: vec![1],
                },
            ],
            interconnects: vec![MemoryInterconnectInfo {
                domain_a: 0,
                domain_b: 1,
                link_type: MemoryLinkKind::NumaLink,
                bandwidth_gbps: 50.0,
                latency_ns: 100,
            }],
            total_capacity_bytes: 512 * 1024 * 1024 * 1024,
        });
        let score = memory_locality_score(&n);
        // Best domain has 2 CPUs + 1 GPU = 3 resources out of 6 total = 0.5
        assert!((score - 0.5).abs() < f64::EPSILON);
    }
}
