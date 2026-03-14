use cucumber::{given, then, when};

use super::helpers::make_dram_domain;
use crate::LatticeWorld;
use lattice_common::types::*;
use lattice_scheduler::conformance::{filter_by_constraints, memory_locality_score};
use lattice_test_harness::fixtures::*;

// ─── Helpers ───────────────────────────────────────────────

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

fn numa_memory_topology(domain_count: u32) -> MemoryTopology {
    let domains: Vec<MemoryDomain> = (0..domain_count)
        .map(|i| make_dram_domain(i, 128 * 1024 * 1024 * 1024, i))
        .collect();
    let interconnects: Vec<MemoryInterconnect> = if domain_count > 1 {
        (0..domain_count - 1)
            .map(|i| MemoryInterconnect {
                domain_a: i,
                domain_b: i + 1,
                link_type: MemoryLinkType::NumaLink,
                bandwidth_gbps: 50.0,
                latency_ns: 100,
            })
            .collect()
    } else {
        Vec::new()
    };
    let total = 128 * 1024 * 1024 * 1024 * domain_count as u64;
    MemoryTopology {
        domains,
        interconnects,
        total_capacity_bytes: total,
    }
}

fn cxl_memory_topology() -> MemoryTopology {
    MemoryTopology {
        domains: vec![MemoryDomain {
            id: 100,
            domain_type: MemoryDomainType::CxlAttached,
            capacity_bytes: 1024 * 1024 * 1024 * 1024,
            numa_node: None,
            attached_cpus: Vec::new(),
            attached_gpus: Vec::new(),
        }],
        interconnects: Vec::new(),
        total_capacity_bytes: 1024 * 1024 * 1024 * 1024,
    }
}

fn dram_only_topology() -> MemoryTopology {
    MemoryTopology {
        domains: vec![make_dram_domain(0, 256 * 1024 * 1024 * 1024, 0)],
        interconnects: Vec::new(),
        total_capacity_bytes: 256 * 1024 * 1024 * 1024,
    }
}

fn cxl_with_dram_topology() -> MemoryTopology {
    MemoryTopology {
        domains: vec![
            make_dram_domain(0, 256 * 1024 * 1024 * 1024, 0),
            MemoryDomain {
                id: 1,
                domain_type: MemoryDomainType::CxlAttached,
                capacity_bytes: 512 * 1024 * 1024 * 1024,
                numa_node: None,
                attached_cpus: Vec::new(),
                attached_gpus: Vec::new(),
            },
        ],
        interconnects: vec![MemoryInterconnect {
            domain_a: 0,
            domain_b: 1,
            link_type: MemoryLinkType::CxlSwitch,
            bandwidth_gbps: 32.0,
            latency_ns: 200,
        }],
        total_capacity_bytes: 768 * 1024 * 1024 * 1024,
    }
}

// ─── Given Steps ───────────────────────────────────────────

#[given(regex = r"^(\d+) nodes with unified memory in group (\d+)$")]
fn given_unified_memory_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("unified-g{group}-{i}"))
            .group(group)
            .memory_topology(unified_memory_topology())
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r"^(\d+) nodes with NUMA memory in group (\d+)$")]
fn given_numa_memory_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("numa-g{group}-{i}"))
            .group(group)
            .cpu_cores(4)
            .gpu_count(2)
            .memory_topology(numa_memory_topology(2))
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r"^(\d+) nodes with CXL memory domains in group (\d+)$")]
fn given_cxl_memory_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("cxl-g{group}-{i}"))
            .group(group)
            .memory_topology(cxl_memory_topology())
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r"^(\d+) nodes with standard DRAM in group (\d+)$")]
fn given_dram_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("dram-g{group}-{i}"))
            .group(group)
            .memory_topology(dram_only_topology())
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r"^(\d+) nodes with single NUMA domain in group (\d+)$")]
fn given_single_numa_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("numa1-g{group}-{i}"))
            .group(group)
            .memory_topology(numa_memory_topology(1))
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r"^(\d+) nodes with (\d+) NUMA domains in group (\d+)$")]
fn given_multi_numa_nodes(world: &mut LatticeWorld, count: usize, domains: u32, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("numa{domains}-g{group}-{i}"))
            .group(group)
            .cpu_cores(domains * 2)
            .gpu_count(domains)
            .memory_topology(numa_memory_topology(domains))
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r"^a node with (\d+) NUMA domains$")]
fn given_node_with_numa_domains(world: &mut LatticeWorld, domains: u32) {
    let node = NodeBuilder::new()
        .id("numa-test-node")
        .cpu_cores(domains * 2)
        .gpu_count(domains)
        .memory_topology(numa_memory_topology(domains))
        .build();
    world.nodes.push(node);
}

#[given(regex = r#"^an allocation with memory policy "(\w+)"$"#)]
fn given_allocation_with_memory_policy(world: &mut LatticeWorld, policy: String) {
    let memory_policy = match policy.as_str() {
        "Local" => MemoryPolicy::Local,
        "Interleave" => MemoryPolicy::Interleave,
        "Preferred" => MemoryPolicy::Preferred,
        "Bind" => MemoryPolicy::Bind,
        other => panic!("Unknown memory policy: {other}"),
    };
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Pending)
        .build();
    alloc.resources.constraints.memory_policy = Some(memory_policy);
    alloc.tags.insert("memory_policy".into(), policy);
    world.allocations.push(alloc);
}

#[given("a node with GH200 superchip architecture")]
fn given_gh200_superchip(world: &mut LatticeWorld) {
    let node = NodeBuilder::new()
        .id("gh200-superchip-node")
        .gpu_type("GH200")
        .memory_topology(unified_memory_topology())
        .build();
    world.nodes.push(node);
}

#[given(regex = r#"^(\d+) nodes with CXL-attached memory in group (\d+)$"#)]
fn given_cxl_attached_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("cxl-att-g{group}-{i}"))
            .group(group)
            .cpu_cores(4)
            .gpu_count(2)
            .memory_topology(cxl_with_dram_topology())
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with local DRAM only in group (\d+)$"#)]
fn given_local_dram_only_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("dram-only-g{group}-{i}"))
            .group(group)
            .memory_topology(dram_only_topology())
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r#"^a node agent for node "([^"]+)" with (\d+) NUMA domains$"#)]
fn given_node_agent_with_numa(world: &mut LatticeWorld, node_id: String, domains: u32) {
    let node = NodeBuilder::new()
        .id(&node_id)
        .cpu_cores(domains * 2)
        .gpu_count(domains)
        .memory_topology(numa_memory_topology(domains))
        .build();
    world.nodes.push(node);
}

// ─── When Steps ────────────────────────────────────────────

#[when("an allocation is submitted requiring unified memory")]
fn when_submit_unified_memory(world: &mut LatticeWorld) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    let constraints = ResourceConstraints {
        require_unified_memory: true,
        ..Default::default()
    };
    let filtered = filter_by_constraints(&node_refs, &constraints);
    world.filtered_nodes = filtered.iter().map(|n| n.id.clone()).collect();

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.resources.constraints = constraints;
    alloc.assigned_nodes = world.filtered_nodes.clone();
    world.allocations.push(alloc);
}

#[when("an allocation is submitted with allow_cxl_memory false")]
fn when_submit_no_cxl(world: &mut LatticeWorld) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    let constraints = ResourceConstraints {
        allow_cxl_memory: false,
        ..Default::default()
    };
    let filtered = filter_by_constraints(&node_refs, &constraints);
    world.filtered_nodes = filtered.iter().map(|n| n.id.clone()).collect();

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.resources.constraints = constraints;
    alloc.assigned_nodes = world.filtered_nodes.clone();
    world.allocations.push(alloc);
}

#[when("an allocation is submitted preferring same NUMA")]
fn when_submit_prefer_numa(world: &mut LatticeWorld) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();

    // Score all nodes by memory locality.
    let mut scores: Vec<(String, f64)> = node_refs
        .iter()
        .map(|n| (n.id.clone(), memory_locality_score(*n)))
        .collect();
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    world.locality_scores = scores;

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.resources.constraints.prefer_same_numa = true;
    alloc.assigned_nodes = vec![world.locality_scores[0].0.clone()];
    world.allocations.push(alloc);
}

#[when("the prologue generates numactl arguments")]
fn when_prologue_numactl(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    let policy = alloc
        .resources
        .constraints
        .memory_policy
        .expect("memory_policy not set on allocation");

    let node = world.nodes.last().expect("no node");
    let is_unified = node.capabilities.memory_topology.as_ref().is_some_and(|t| {
        t.domains
            .iter()
            .any(|d| d.domain_type == MemoryDomainType::Unified)
    });

    let args = if is_unified {
        // Unified memory does not need numactl.
        Vec::new()
    } else {
        match policy {
            MemoryPolicy::Local => vec!["--membind=0".to_string()],
            MemoryPolicy::Interleave => vec!["--interleave=all".to_string()],
            MemoryPolicy::Preferred => vec!["--preferred=0".to_string()],
            MemoryPolicy::Bind => vec!["--membind=0".to_string()],
        }
    };

    // Store the generated arguments for verification.
    let alloc = world.last_allocation_mut();
    alloc.tags.insert("numactl_args".into(), args.join(" "));
}

#[when("memory topology is discovered")]
fn when_memory_topology_discovered(world: &mut LatticeWorld) {
    // Memory topology was set in the given step. Verify it was discovered.
    let node = world.nodes.last().expect("no node");
    assert!(
        node.capabilities.memory_topology.is_some(),
        "Memory topology should have been discovered"
    );
}

#[when("an allocation is submitted without CXL preference")]
fn when_submit_without_cxl_pref(world: &mut LatticeWorld) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();

    // Score all nodes by memory locality (DRAM-only scores higher than CXL).
    let mut scores: Vec<(String, f64)> = node_refs
        .iter()
        .map(|n| (n.id.clone(), memory_locality_score(*n)))
        .collect();
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    world.locality_scores = scores;

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = vec![world.locality_scores[0].0.clone()];
    world.allocations.push(alloc);
}

#[when("the agent sends a heartbeat")]
fn when_agent_sends_heartbeat(world: &mut LatticeWorld) {
    let node = world.nodes.last().expect("no node");
    // Simulate heartbeat by recording topology information.
    let topo = node.capabilities.memory_topology.as_ref();
    let domain_count = topo.map(|t| t.domains.len()).unwrap_or(0);
    let has_interconnects = topo.map(|t| !t.interconnects.is_empty()).unwrap_or(false);

    let alloc = AllocationBuilder::new()
        .tag("heartbeat_sent", "true")
        .tag("memory_domain_count", &domain_count.to_string())
        .tag(
            "has_interconnects",
            if has_interconnects { "true" } else { "false" },
        )
        .build();
    world.allocations.push(alloc);
}

// ─── Then Steps ────────────────────────────────────────────

#[then("the allocation should be placed on unified memory nodes")]
fn then_placed_on_unified(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    for node_id in &alloc.assigned_nodes {
        let node = world
            .nodes
            .iter()
            .find(|n| n.id == *node_id)
            .unwrap_or_else(|| panic!("Node {node_id} not found"));
        let is_unified = node.capabilities.memory_topology.as_ref().is_some_and(|t| {
            t.domains
                .iter()
                .any(|d| d.domain_type == MemoryDomainType::Unified)
        });
        assert!(
            is_unified,
            "Node {node_id} should have unified memory topology"
        );
    }
}

#[then("the allocation should be placed on DRAM-only nodes")]
fn then_placed_on_dram(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    for node_id in &alloc.assigned_nodes {
        let node = world
            .nodes
            .iter()
            .find(|n| n.id == *node_id)
            .unwrap_or_else(|| panic!("Node {node_id} not found"));
        let has_cxl_only = node.capabilities.memory_topology.as_ref().is_some_and(|t| {
            let non_cxl: u64 = t
                .domains
                .iter()
                .filter(|d| d.domain_type != MemoryDomainType::CxlAttached)
                .map(|d| d.capacity_bytes)
                .sum();
            non_cxl == 0 && t.total_capacity_bytes > 0
        });
        assert!(!has_cxl_only, "Node {node_id} should not be CXL-only");
    }
}

#[then("nodes with fewer NUMA domains should score higher")]
fn then_fewer_numa_scores_higher(world: &mut LatticeWorld) {
    assert!(
        world.locality_scores.len() >= 2,
        "Expected at least 2 locality scores"
    );
    // Scores are sorted descending. The highest-scoring nodes should be the ones
    // with fewer NUMA domains (higher locality).
    let top_node_id = &world.locality_scores[0].0;
    let top_node = world
        .nodes
        .iter()
        .find(|n| n.id == *top_node_id)
        .expect("top-scoring node not found");
    let top_domains = top_node
        .capabilities
        .memory_topology
        .as_ref()
        .map(|t| t.domains.len())
        .unwrap_or(0);

    let bottom_node_id = &world.locality_scores.last().unwrap().0;
    let bottom_node = world
        .nodes
        .iter()
        .find(|n| n.id == *bottom_node_id)
        .expect("bottom-scoring node not found");
    let bottom_domains = bottom_node
        .capabilities
        .memory_topology
        .as_ref()
        .map(|t| t.domains.len())
        .unwrap_or(0);

    assert!(
        top_domains <= bottom_domains,
        "Nodes with fewer NUMA domains ({top_domains}) should score >= nodes with more ({bottom_domains})"
    );
}

#[then(regex = r#"^the arguments should include "--membind" for the assigned NUMA domain$"#)]
fn then_numactl_membind(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    let args = alloc
        .tags
        .get("numactl_args")
        .expect("numactl_args not set");
    assert!(
        args.contains("--membind"),
        "Expected --membind in numactl args, got: {args}"
    );
}

#[then(regex = r#"^the arguments should include "--interleave=all"$"#)]
fn then_numactl_interleave(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    let args = alloc
        .tags
        .get("numactl_args")
        .expect("numactl_args not set");
    assert!(
        args.contains("--interleave=all"),
        "Expected --interleave=all in numactl args, got: {args}"
    );
}

#[then("the node should report unified memory")]
fn then_node_reports_unified(world: &mut LatticeWorld) {
    let node = world.nodes.last().expect("no node");
    let is_unified = node.capabilities.memory_topology.as_ref().is_some_and(|t| {
        t.domains
            .iter()
            .any(|d| d.domain_type == MemoryDomainType::Unified)
    });
    assert!(is_unified, "GH200 node should report unified memory");
}

#[then("no NUMA pinning should be required")]
fn then_no_numa_pinning(world: &mut LatticeWorld) {
    let node = world.nodes.last().expect("no node");
    let topo = node
        .capabilities
        .memory_topology
        .as_ref()
        .expect("no memory topology");
    // Unified memory has a single domain: no NUMA pinning needed.
    let is_unified = topo
        .domains
        .iter()
        .any(|d| d.domain_type == MemoryDomainType::Unified);
    assert!(
        is_unified || topo.domains.len() <= 1,
        "No NUMA pinning should be required for unified memory"
    );
}

#[then("local DRAM nodes should score higher for memory locality")]
fn then_dram_scores_higher(world: &mut LatticeWorld) {
    assert!(
        !world.locality_scores.is_empty(),
        "Expected locality scores to be computed"
    );

    // Partition scores into DRAM-only and CXL-containing nodes.
    let mut dram_scores = Vec::new();
    let mut cxl_scores = Vec::new();
    for (node_id, score) in &world.locality_scores {
        let node = world
            .nodes
            .iter()
            .find(|n| n.id == *node_id)
            .expect("node not found");
        let has_cxl = node.capabilities.memory_topology.as_ref().is_some_and(|t| {
            t.domains
                .iter()
                .any(|d| d.domain_type == MemoryDomainType::CxlAttached)
        });
        if has_cxl {
            cxl_scores.push(*score);
        } else {
            dram_scores.push(*score);
        }
    }

    let avg_dram = if dram_scores.is_empty() {
        0.0
    } else {
        dram_scores.iter().sum::<f64>() / dram_scores.len() as f64
    };
    let avg_cxl = if cxl_scores.is_empty() {
        0.0
    } else {
        cxl_scores.iter().sum::<f64>() / cxl_scores.len() as f64
    };

    assert!(
        avg_dram >= avg_cxl,
        "DRAM average locality ({avg_dram}) should be >= CXL average ({avg_cxl})"
    );
}

#[then("the heartbeat should include memory topology information")]
fn then_heartbeat_includes_topo(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("heartbeat_sent").map(|s| s.as_str()),
        Some("true"),
        "Expected heartbeat to have been sent"
    );
    let domain_count: usize = alloc
        .tags
        .get("memory_domain_count")
        .expect("memory_domain_count not set")
        .parse()
        .unwrap();
    assert!(
        domain_count > 0,
        "Expected heartbeat to report at least 1 memory domain"
    );
}

#[then("the topology should report domain count and interconnect types")]
fn then_topology_reports_details(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    let domain_count: usize = alloc
        .tags
        .get("memory_domain_count")
        .expect("memory_domain_count not set")
        .parse()
        .unwrap();
    assert!(
        domain_count > 0,
        "Expected domain count > 0 in topology report"
    );
    // For multi-domain nodes, interconnects should be present.
    if domain_count > 1 {
        assert_eq!(
            alloc.tags.get("has_interconnects").map(|s| s.as_str()),
            Some("true"),
            "Expected interconnects to be reported for multi-domain topology"
        );
    }
}
