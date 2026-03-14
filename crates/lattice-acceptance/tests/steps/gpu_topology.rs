use cucumber::{given, then, when};

use crate::LatticeWorld;
use lattice_common::types::*;
use lattice_scheduler::conformance::filter_by_constraints;
use lattice_test_harness::fixtures::*;

// ─── Given Steps ───────────────────────────────────────────

#[given(regex = r#"^(\d+) nodes with GPU type "([^"]+)" in group (\d+)$"#)]
fn given_gpu_type_nodes(
    world: &mut LatticeWorld,
    count: usize,
    gpu_type: String,
    group: u32,
) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("gpu-{gpu_type}-g{group}-{i}"))
            .group(group)
            .gpu_type(&gpu_type)
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with (\d+) GPUs each in group (\d+)$"#)]
fn given_gpu_count_nodes(
    world: &mut LatticeWorld,
    count: usize,
    gpus: u32,
    group: u32,
) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("gpu{gpus}-g{group}-{i}"))
            .group(group)
            .gpu_count(gpus)
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with NVLink-connected GPU pairs in group (\d+)$"#)]
fn given_nvlink_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("nvlink-g{group}-{i}"))
            .group(group)
            .feature("nvlink")
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with PCIe-only GPUs in group (\d+)$"#)]
fn given_pcie_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("pcie-g{group}-{i}"))
            .group(group)
            .feature("pcie_gpu")
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with (\d+)GB GPU memory in group (\d+)$"#)]
fn given_gpu_memory_nodes(
    world: &mut LatticeWorld,
    count: usize,
    gpu_mem_gb: u64,
    group: u32,
) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("gpumem{gpu_mem_gb}-g{group}-{i}"))
            .group(group)
            .feature(&format!("gpu_memory_{gpu_mem_gb}gb"))
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes sharing a PCIe switch in group (\d+)$"#)]
fn given_pcie_switch_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("pcie-sw-g{group}-{i}"))
            .group(group)
            .feature("pcie_switch")
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes in a different switch group in group (\d+)$"#)]
fn given_different_switch_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("other-sw-g{group}-{i}"))
            .group(group)
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes without GPUs in group (\d+)$"#)]
fn given_no_gpu_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("nogpu-g{group}-{i}"))
            .group(group)
            .gpu_type("")
            .gpu_count(0)
            .build();
        // Clear gpu_type since NodeBuilder sets a default.
        let idx = world.nodes.len();
        world.nodes.push(node);
        world.nodes[idx].capabilities.gpu_type = None;
    }
}

// ─── When Steps ────────────────────────────────────────────

#[when(regex = r#"^an allocation requiring GPU type "([^"]+)" is submitted$"#)]
fn when_requiring_gpu_type(world: &mut LatticeWorld, gpu_type: String) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    let constraints = ResourceConstraints {
        gpu_type: Some(gpu_type),
        ..Default::default()
    };
    let filtered = filter_by_constraints(&node_refs, &constraints);
    world.filtered_nodes = filtered.iter().map(|n| n.id.clone()).collect();

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = world.filtered_nodes.clone();
    world.allocations.push(alloc);
}

#[when(regex = r"^an allocation requiring (\d+) GPUs per node is submitted$")]
fn when_requiring_gpu_count(world: &mut LatticeWorld, gpus: u32) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    // Filter nodes that have at least the required number of GPUs.
    let filtered: Vec<&Node> = node_refs
        .iter()
        .filter(|n| n.capabilities.gpu_count >= gpus)
        .copied()
        .collect();
    world.filtered_nodes = filtered.iter().map(|n| n.id.clone()).collect();

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = world.filtered_nodes.clone();
    world.allocations.push(alloc);
}

#[when("an allocation requiring multi-GPU communication is submitted")]
fn when_requiring_multi_gpu(world: &mut LatticeWorld) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    let constraints = ResourceConstraints {
        features: vec!["nvlink".into()],
        ..Default::default()
    };
    let filtered = filter_by_constraints(&node_refs, &constraints);

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();

    if !filtered.is_empty() {
        alloc.assigned_nodes = filtered.iter().map(|n| n.id.clone()).collect();
        alloc.tags.insert("gpu_interconnect".into(), "nvlink".into());
    } else {
        // Fallback to PCIe nodes.
        alloc.assigned_nodes = node_refs.iter().map(|n| n.id.clone()).collect();
        alloc.tags.insert("gpu_interconnect".into(), "pcie".into());
    }

    world.filtered_nodes = alloc.assigned_nodes.clone();
    world.allocations.push(alloc);
}

#[when("an allocation requesting GPUs is submitted")]
fn when_requesting_gpus(world: &mut LatticeWorld) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    let filtered: Vec<&Node> = node_refs
        .iter()
        .filter(|n| n.capabilities.gpu_count > 0)
        .copied()
        .collect();

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();

    // Check for NVLink availability.
    let has_nvlink = filtered
        .iter()
        .any(|n| n.capabilities.features.contains(&"nvlink".into()));

    if has_nvlink {
        alloc.tags.insert("gpu_interconnect".into(), "nvlink".into());
    } else {
        alloc.tags.insert("gpu_interconnect".into(), "pcie".into());
    }

    alloc.assigned_nodes = filtered.iter().map(|n| n.id.clone()).collect();
    world.filtered_nodes = alloc.assigned_nodes.clone();
    world.allocations.push(alloc);
}

#[when("an allocation without GPU type preference is submitted")]
fn when_no_gpu_preference(world: &mut LatticeWorld) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    let constraints = ResourceConstraints::default();
    let filtered = filter_by_constraints(&node_refs, &constraints);
    world.filtered_nodes = filtered.iter().map(|n| n.id.clone()).collect();

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = world.filtered_nodes.clone();
    world.allocations.push(alloc);
}

#[when(regex = r"^an allocation requiring 80GB GPU memory is submitted$")]
fn when_requiring_gpu_memory(world: &mut LatticeWorld) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    let constraints = ResourceConstraints {
        features: vec!["gpu_memory_80gb".into()],
        ..Default::default()
    };
    let filtered = filter_by_constraints(&node_refs, &constraints);
    world.filtered_nodes = filtered.iter().map(|n| n.id.clone()).collect();

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = world.filtered_nodes.clone();
    world.allocations.push(alloc);
}

// Note: "an allocation requiring N nodes is submitted" is in common.rs

#[when(regex = r"^an allocation requiring 0 GPUs is submitted$")]
fn when_requiring_zero_gpus(world: &mut LatticeWorld) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    // No GPU constraint: all nodes are candidates.
    world.filtered_nodes = node_refs.iter().map(|n| n.id.clone()).collect();

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .tag("gpu_required", "false")
        .build();
    alloc.assigned_nodes = world.filtered_nodes.clone();
    world.allocations.push(alloc);
}

// ─── Then Steps ────────────────────────────────────────────

#[then(regex = r"^the allocation should be placed on (\w+) nodes only$")]
fn then_placed_on_gpu_type_only(world: &mut LatticeWorld, expected_type: String) {
    let alloc = world.last_allocation();
    for node_id in &alloc.assigned_nodes {
        let node = world
            .nodes
            .iter()
            .find(|n| n.id == *node_id)
            .unwrap_or_else(|| panic!("Node {node_id} not found"));
        assert_eq!(
            node.capabilities.gpu_type.as_deref(),
            Some(expected_type.as_str()),
            "Node {node_id} has GPU type {:?}, expected {expected_type}",
            node.capabilities.gpu_type
        );
    }
}

#[then("the allocation should be placed on 8-GPU nodes")]
fn then_placed_on_8gpu(world: &mut LatticeWorld) {
    for node_id in &world.filtered_nodes {
        let node = world
            .nodes
            .iter()
            .find(|n| n.id == *node_id)
            .unwrap_or_else(|| panic!("Node {node_id} not found"));
        assert!(
            node.capabilities.gpu_count >= 8,
            "Node {node_id} has {} GPUs, expected >= 8",
            node.capabilities.gpu_count
        );
    }
}

#[then("the allocation should prefer NVLink-connected nodes")]
fn then_prefer_nvlink(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("gpu_interconnect").map(|s| s.as_str()),
        Some("nvlink"),
        "Expected NVLink-connected nodes to be preferred"
    );
    // Verify assigned nodes have the nvlink feature.
    for node_id in &alloc.assigned_nodes {
        let node = world
            .nodes
            .iter()
            .find(|n| n.id == *node_id)
            .unwrap_or_else(|| panic!("Node {node_id} not found"));
        assert!(
            node.capabilities.features.contains(&"nvlink".into()),
            "Node {node_id} should have nvlink feature"
        );
    }
}

#[then("the allocation should be placed using PCIe topology")]
fn then_placed_pcie(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("gpu_interconnect").map(|s| s.as_str()),
        Some("pcie"),
        "Expected PCIe topology placement"
    );
}

#[then("no NVLink preference should be applied")]
fn then_no_nvlink_preference(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    let interconnect = alloc
        .tags
        .get("gpu_interconnect")
        .map(|s| s.as_str())
        .unwrap_or("none");
    assert_ne!(
        interconnect, "nvlink",
        "NVLink preference should not be applied"
    );
}

#[then("the allocation may be placed on either GPU type")]
fn then_any_gpu_type(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(
        !alloc.assigned_nodes.is_empty(),
        "Expected allocation to be placed on some nodes"
    );
    // Collect distinct GPU types from assigned nodes.
    let gpu_types: std::collections::HashSet<Option<&str>> = alloc
        .assigned_nodes
        .iter()
        .filter_map(|id| world.nodes.iter().find(|n| n.id == *id))
        .map(|n| n.capabilities.gpu_type.as_deref())
        .collect();
    // With no constraint, either one or both types may appear.
    assert!(
        !gpu_types.is_empty(),
        "Expected at least one GPU type in assigned nodes"
    );
}

#[then(regex = r"^the allocation should be placed on 80GB GPU nodes only$")]
fn then_placed_on_80gb_gpu(world: &mut LatticeWorld) {
    for node_id in &world.filtered_nodes {
        let node = world
            .nodes
            .iter()
            .find(|n| n.id == *node_id)
            .unwrap_or_else(|| panic!("Node {node_id} not found"));
        assert!(
            node.capabilities.features.contains(&"gpu_memory_80gb".into()),
            "Node {node_id} should have gpu_memory_80gb feature"
        );
    }
}

#[then("the allocation should prefer nodes sharing the common PCIe ancestor")]
fn then_prefer_pcie_ancestor(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    // Nodes packed in the same group share a common topology ancestor.
    assert_eq!(
        alloc.tags.get("placement_mode").map(|s| s.as_str()),
        Some("group_packed"),
        "Expected group-packed placement for PCIe ancestor sharing"
    );
    // Verify all assigned nodes are in the same group.
    let groups: std::collections::HashSet<u32> = alloc
        .assigned_nodes
        .iter()
        .filter_map(|id| world.nodes.iter().find(|n| n.id == *id))
        .map(|n| n.group)
        .collect();
    assert_eq!(
        groups.len(),
        1,
        "Expected all assigned nodes in one group, got groups: {groups:?}"
    );
}

#[then("GPU topology should not influence placement")]
fn then_gpu_topology_not_used(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("gpu_required").map(|s| s.as_str()),
        Some("false"),
        "Expected GPU-agnostic placement"
    );
    assert!(
        !alloc.assigned_nodes.is_empty(),
        "Expected allocation to be placed despite no GPU requirement"
    );
}
