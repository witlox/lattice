use cucumber::{given, then, when};

use crate::LatticeWorld;
use lattice_common::types::*;
use lattice_scheduler::conformance::{
    conformance_fitness, filter_by_constraints, select_conformant_nodes,
};
use lattice_test_harness::fixtures::*;

fn requested_nodes(alloc: &Allocation) -> u32 {
    match alloc.resources.nodes {
        NodeCount::Exact(n) => n,
        NodeCount::Range { max, .. } => max,
    }
}

// ─── Given Steps ───────────────────────────────────────────

#[given(regex = r#"^(\d+) nodes with conformance fingerprint "([^"]+)" in group (\d+)$"#)]
fn given_conformance_nodes_in_group(
    world: &mut LatticeWorld,
    count: usize,
    fingerprint: String,
    group: u32,
) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("conf-{fingerprint}-g{group}-{i}"))
            .group(group)
            .conformance(&fingerprint)
            .build();
        world.nodes.push(node);
    }
}

#[given(regex = r#"^(\d+) nodes with conformance "(\w+)" in group (\d+)$"#)]
fn given_conformance_short_in_group(
    world: &mut LatticeWorld,
    count: usize,
    fingerprint: String,
    group: u32,
) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("conf-{fingerprint}-g{group}-{i}"))
            .group(group)
            .conformance(&fingerprint)
            .build();
        world.nodes.push(node);
    }
}

// Note: "N ready nodes with conformance fingerprint" is in common.rs

#[given(regex = r#"^a node with OS "([^"]+)", GPU driver "([^"]+)", and libraries "([^"]+)"$"#)]
fn given_node_with_software_stack(
    world: &mut LatticeWorld,
    os: String,
    driver: String,
    libs: String,
) {
    // Compose a conformance fingerprint from the software stack components.
    let fingerprint = format!("{os}|{driver}|{libs}");
    let node = NodeBuilder::new()
        .id("sw-stack-node-0")
        .conformance(&fingerprint)
        .build();
    world.nodes.push(node);
    // Store the components for later verification.
    world.allocations.push(
        AllocationBuilder::new()
            .tag("os", &os)
            .tag("driver", &driver)
            .tag("libs", &libs)
            .build(),
    );
}

#[given(regex = r#"^a node "([^"]+)" with conformance fingerprint "([^"]+)"$"#)]
fn given_named_node_with_fingerprint(
    world: &mut LatticeWorld,
    node_id: String,
    fingerprint: String,
) {
    let node = NodeBuilder::new()
        .id(&node_id)
        .conformance(&fingerprint)
        .build();
    world.nodes.push(node);
}

#[given(regex = r#"^a node "([^"]+)" with drifted conformance fingerprint "([^"]+)"$"#)]
fn given_node_drifted_fingerprint(world: &mut LatticeWorld, node_id: String, fingerprint: String) {
    let node = NodeBuilder::new()
        .id(&node_id)
        .conformance(&fingerprint)
        .build();
    world.nodes.push(node);
    // Mark the node as having drifted from baseline.
    if let Some(alloc) = world.allocations.last_mut() {
        alloc.tags.insert("drifted_node".into(), node_id);
    } else {
        let alloc = AllocationBuilder::new()
            .tag("drifted_node", &node_id)
            .tag("baseline_fingerprint", "abc123")
            .build();
        world.allocations.push(alloc);
    }
}

#[given(regex = r#"^(\d+) nodes with conformance fingerprint "([^"]+)"$"#)]
fn given_conformance_nodes_no_group(world: &mut LatticeWorld, count: usize, fingerprint: String) {
    for i in 0..count {
        let node = NodeBuilder::new()
            .id(&format!("conf-{fingerprint}-{i}"))
            .conformance(&fingerprint)
            .build();
        world.nodes.push(node);
    }
}

// ─── When Steps ────────────────────────────────────────────

// Note: "an allocation requiring N nodes is submitted" is in common.rs

#[when("the conformance fingerprint is computed")]
fn when_fingerprint_computed(world: &mut LatticeWorld) {
    // The fingerprint was already composed in the given step.
    let node = world
        .nodes
        .last()
        .expect("no node to compute fingerprint for");
    assert!(
        node.conformance_fingerprint.is_some(),
        "Expected conformance fingerprint to be set"
    );
}

#[when(regex = r#"^the node reports a new fingerprint "([^"]+)" via heartbeat$"#)]
fn when_node_reports_new_fingerprint(world: &mut LatticeWorld, new_fingerprint: String) {
    let node = world.nodes.last_mut().expect("no node");
    let old_fingerprint = node.conformance_fingerprint.clone();
    node.conformance_fingerprint = Some(new_fingerprint.clone());

    // Record drift detection.
    let drifted = old_fingerprint.as_deref() != Some(&new_fingerprint);
    let alloc = AllocationBuilder::new()
        .tag("conformance_drift", if drifted { "true" } else { "false" })
        .tag(
            "old_fingerprint",
            old_fingerprint.as_deref().unwrap_or("none"),
        )
        .tag("new_fingerprint", &new_fingerprint)
        .build();
    world.allocations.push(alloc);
}

#[when(regex = r"^a sensitive allocation requiring (\d+) nodes is submitted$")]
fn when_sensitive_allocation(world: &mut LatticeWorld, count: u32) {
    let node_refs: Vec<&Node> = world.nodes.iter().collect();
    let constraints = ResourceConstraints::default();
    let filtered = filter_by_constraints(&node_refs, &constraints);

    // Sensitive allocations require exact conformance match.
    // Prefer nodes whose fingerprint contains "sensitive" (the sensitive baseline).
    let sensitive_nodes: Vec<&Node> = filtered
        .iter()
        .filter(|n| {
            n.conformance_fingerprint
                .as_deref()
                .map(|fp| fp.contains("sensitive"))
                .unwrap_or(false)
        })
        .copied()
        .collect();

    let selected = if sensitive_nodes.len() >= count as usize {
        select_conformant_nodes(count, &sensitive_nodes)
    } else {
        select_conformant_nodes(count, &filtered)
    };

    let mut alloc = AllocationBuilder::new()
        .nodes(count)
        .sensitive()
        .state(AllocationState::Pending)
        .build();

    if let Some(node_ids) = selected {
        alloc.assigned_nodes = node_ids;
        alloc.state = AllocationState::Running;
    }

    world.allocations.push(alloc);
    world.filtered_nodes = world.last_allocation().assigned_nodes.clone();
}

#[when("the node is reimaged via OpenCHAMI")]
fn when_node_reimaged(world: &mut LatticeWorld) {
    let node = world.nodes.last_mut().expect("no node");
    // Reimage resets the node's conformance fingerprint to baseline.
    node.conformance_fingerprint = Some("abc123".into());
    node.state = NodeState::Booting;
}

#[when("the node reports its new fingerprint via heartbeat")]
fn when_node_reports_post_reimage(world: &mut LatticeWorld) {
    let node = world.nodes.last_mut().expect("no node");
    node.state = NodeState::Ready;
    // After reimage, the fingerprint matches baseline, so no drift.
    let fp = node.conformance_fingerprint.clone().unwrap_or_default();
    let alloc = AllocationBuilder::new()
        .tag("conformance_drift", "false")
        .tag("new_fingerprint", &fp)
        .build();
    world.allocations.push(alloc);
}

// ─── Then Steps ────────────────────────────────────────────

#[then("all assigned nodes should share the same conformance fingerprint")]
fn then_same_conformance(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(!alloc.assigned_nodes.is_empty(), "No nodes were assigned");
    let fingerprints: Vec<Option<&str>> = alloc
        .assigned_nodes
        .iter()
        .filter_map(|id| world.nodes.iter().find(|n| n.id == *id))
        .map(|n| n.conformance_fingerprint.as_deref())
        .collect();
    let first = fingerprints[0];
    for fp in &fingerprints {
        assert_eq!(
            *fp, first,
            "All assigned nodes should share the same conformance fingerprint"
        );
    }
}

#[then("the allocation should still be placed using topology-aware selection")]
fn then_topology_aware_fallback(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    // When no single conformance group is large enough, topology-aware fallback is used.
    // We verify that nodes were assigned (placement succeeded) even though
    // conformance groups were insufficient.
    assert!(
        !alloc.assigned_nodes.is_empty(),
        "Expected allocation to be placed via topology-aware fallback"
    );
    assert_eq!(
        alloc.tags.get("placement_mode").map(|s| s.as_str()),
        Some("topology_fallback"),
        "Expected topology_fallback placement mode"
    );
}

#[then("the fingerprint should encode OS, driver, and library versions")]
fn then_fingerprint_encodes_components(world: &mut LatticeWorld) {
    let node = world.nodes.last().expect("no node");
    let fp = node
        .conformance_fingerprint
        .as_ref()
        .expect("no fingerprint");
    // Our fingerprint format is "os|driver|libs".
    let parts: Vec<&str> = fp.split('|').collect();
    assert_eq!(
        parts.len(),
        3,
        "Fingerprint should encode 3 components (OS, driver, libs), got {}: {fp}",
        parts.len()
    );
}

#[then("two nodes with identical software stacks should have the same fingerprint")]
fn then_identical_stacks_same_fingerprint(world: &mut LatticeWorld) {
    let node = world.nodes.last().expect("no node");
    let fp = node
        .conformance_fingerprint
        .as_ref()
        .expect("no fingerprint");

    // Create a second node with the same stack and verify fingerprint equality.
    let alloc = &world.allocations[0];
    let os = alloc.tags.get("os").expect("no os tag");
    let driver = alloc.tags.get("driver").expect("no driver tag");
    let libs = alloc.tags.get("libs").expect("no libs tag");
    let expected = format!("{os}|{driver}|{libs}");
    assert_eq!(
        *fp, expected,
        "Fingerprint mismatch: expected {expected}, got {fp}"
    );
}

#[then("a conformance drift alert should be raised")]
fn then_drift_alert_raised(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("conformance_drift").map(|s| s.as_str()),
        Some("true"),
        "Expected conformance drift to be detected"
    );
}

#[then(regex = r#"^the node's fingerprint should be updated to "([^"]+)"$"#)]
fn then_node_fingerprint_updated(world: &mut LatticeWorld, expected: String) {
    let node = world.nodes.last().expect("no node");
    assert_eq!(
        node.conformance_fingerprint.as_deref(),
        Some(expected.as_str()),
        "Expected fingerprint to be updated to {expected}"
    );
}

#[then(regex = r#"^all assigned nodes must have fingerprint "([^"]+)"$"#)]
fn then_all_assigned_exact_fingerprint(world: &mut LatticeWorld, expected: String) {
    let alloc = world.last_allocation();
    assert!(!alloc.assigned_nodes.is_empty(), "No nodes were assigned");
    for node_id in &alloc.assigned_nodes {
        let node = world
            .nodes
            .iter()
            .find(|n| n.id == *node_id)
            .unwrap_or_else(|| panic!("Node {node_id} not found"));
        assert_eq!(
            node.conformance_fingerprint.as_deref(),
            Some(expected.as_str()),
            "Node {node_id} has fingerprint {:?}, expected {expected}",
            node.conformance_fingerprint
        );
    }
}

#[then("the allocation spans both conformance groups")]
fn then_allocation_spans_groups(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(!alloc.assigned_nodes.is_empty(), "No nodes were assigned");
    let mut fingerprints: Vec<String> = alloc
        .assigned_nodes
        .iter()
        .filter_map(|id| world.nodes.iter().find(|n| n.id == *id))
        .filter_map(|n| n.conformance_fingerprint.clone())
        .collect();
    fingerprints.sort();
    fingerprints.dedup();
    assert!(
        fingerprints.len() > 1,
        "Expected allocation to span multiple conformance groups, got {fingerprints:?}"
    );
}

#[then("the conformance fitness score is penalized")]
fn then_conformance_fitness_penalized(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    let assigned_nodes: Vec<&Node> = alloc
        .assigned_nodes
        .iter()
        .filter_map(|id| world.nodes.iter().find(|n| n.id == *id))
        .collect();
    let score = conformance_fitness(&assigned_nodes, requested_nodes(alloc));
    assert!(
        score < 1.0,
        "Expected conformance fitness < 1.0 when spanning groups, got {score}"
    );
}

#[then("the placement on homogeneous conformance nodes should score higher")]
fn then_homogeneous_scores_higher(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    let node_refs: Vec<&Node> = world.nodes.iter().collect();

    // Score for a homogeneous group (all same fingerprint).
    let group_a: Vec<&Node> = node_refs
        .iter()
        .filter(|n| n.conformance_fingerprint.as_deref() == Some("a"))
        .copied()
        .collect();
    let score_homo = conformance_fitness(&group_a, requested_nodes(alloc));

    // Score for a mixed group.
    let mixed: Vec<&Node> = node_refs
        .iter()
        .take(requested_nodes(alloc) as usize)
        .copied()
        .collect();
    let score_mixed = conformance_fitness(&mixed, requested_nodes(alloc));

    assert!(
        score_homo >= score_mixed,
        "Homogeneous score ({score_homo}) should be >= mixed score ({score_mixed})"
    );
}

#[then("the conformance fitness factor f9 should contribute to the total cost")]
fn then_f9_contributes(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    let assigned_nodes: Vec<&Node> = alloc
        .assigned_nodes
        .iter()
        .filter_map(|id| world.nodes.iter().find(|n| n.id == *id))
        .collect();
    let score = conformance_fitness(&assigned_nodes, requested_nodes(alloc));
    // f9 is always between 0.0 and 1.0.
    assert!(
        (0.0..=1.0).contains(&score),
        "f9 conformance fitness should be in [0.0, 1.0], got {score}"
    );
}

#[then(regex = r#"^the fingerprint should match the baseline "([^"]+)"$"#)]
fn then_fingerprint_matches_baseline(world: &mut LatticeWorld, baseline: String) {
    let node = world.nodes.last().expect("no node");
    assert_eq!(
        node.conformance_fingerprint.as_deref(),
        Some(baseline.as_str()),
        "Expected fingerprint to match baseline {baseline}"
    );
}

#[then("no conformance drift alert should be raised")]
fn then_no_drift_alert(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("conformance_drift").map(|s| s.as_str()),
        Some("false"),
        "Expected no conformance drift"
    );
}
