//! Conformance group management.
//!
//! Delegates to hpc-scheduler-core for generic logic, with a convenience
//! wrapper for Lattice-specific `ResourceConstraints` → `NodeConstraints` conversion.

pub use hpc_scheduler_core::conformance::{
    conformance_fitness, group_by_conformance, memory_locality_score, select_conformant_nodes,
};

use hpc_scheduler_core::conformance as core_conformance;
use hpc_scheduler_core::types::NodeConstraints;

use lattice_common::types::*;

/// Filter nodes that satisfy an allocation's hardware constraints.
///
/// This is a thin wrapper that converts Lattice's `ResourceConstraints` to
/// hpc-scheduler-core's `NodeConstraints` before delegating.
pub fn filter_by_constraints<'a>(
    nodes: &[&'a Node],
    constraints: &ResourceConstraints,
) -> Vec<&'a Node> {
    let core_constraints = NodeConstraints {
        gpu_type: constraints.gpu_type.clone(),
        features: constraints.features.clone(),
        require_unified_memory: constraints.require_unified_memory,
        allow_cxl_memory: constraints.allow_cxl_memory,
    };
    core_conformance::filter_by_constraints(nodes, &core_constraints)
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
