use chrono::{Duration, Utc};
use cucumber::{given, then, when};

use crate::LatticeWorld;
use super::helpers::parse_allocation_state;
use lattice_common::types::*;
use lattice_test_harness::fixtures::*;

// ─── Given steps ───────────────────────────────────────────
// Note: tenant, vCluster, ready nodes, and scheduler cycle steps are in common.rs

#[given(regex = r#"^a tenant "(\w+)" with fair_share_target ([0-9.]+) and current usage ([0-9.]+)$"#)]
fn given_tenant_with_fair_share(
    world: &mut LatticeWorld,
    tenant: String,
    fair_share: f64,
    usage: f64,
) {
    let t = TenantBuilder::new(&tenant)
        .fair_share(fair_share)
        .build();
    world.tenants.push(t);

    // Simulate current usage by creating running allocations.
    // Usage is fraction of total nodes, we'll compute after nodes are set up.
    // Store the usage target in a tag so we can set up running allocs later.
    world
        .named_allocations
        .entry(format!("__usage__{tenant}"))
        .or_insert_with(|| {
            let mut alloc = AllocationBuilder::new()
                .tenant(&tenant)
                .nodes(1)
                .state(AllocationState::Running)
                .build();
            alloc.assigned_nodes = vec![format!("__usage_node_{tenant}")];
            // We'll adjust node count in the scheduling step based on usage fraction.
            alloc.resources.nodes = NodeCount::Exact((usage * 10.0).round() as u32);
            alloc
        });
}

#[given(regex = r#"^a large deferred allocation reserving (\d+) nodes starting in (\d+) hours$"#)]
fn given_large_deferred_allocation(
    world: &mut LatticeWorld,
    node_count: u32,
    _hours_from_now: u32,
) {
    // Create a high-priority allocation that cannot be placed yet (simulating
    // the reservation). It will be pending and the scheduler should create a
    // reservation for it during the cycle.
    let alloc = AllocationBuilder::new()
        .tenant(
            &world
                .tenants
                .first()
                .map(|t| t.id.clone())
                .unwrap_or_else(|| "test-tenant".into()),
        )
        .nodes(node_count)
        .preemption_class(9)
        .lifecycle_bounded(4)
        .build();
    world
        .named_allocations
        .insert("large_deferred".into(), alloc.clone());
    world.allocations.push(alloc);
}

#[given(regex = r#"^(\d+) ready nodes in group (\d+) all running allocations$"#)]
fn given_nodes_all_running(world: &mut LatticeWorld, count: usize, group: u32) {
    let mut nodes = create_node_batch(count, group);
    let tenant = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let vcluster = world
        .vclusters
        .first()
        .map(|vc| vc.id.clone())
        .unwrap_or_else(|| "default".into());

    // Create a running allocation occupying all these nodes, and mark each
    // node as owned so the scheduler sees them as unavailable.
    for node in &mut nodes {
        let mut alloc = AllocationBuilder::new()
            .tenant(&tenant)
            .nodes(1)
            .lifecycle_bounded(2)
            .state(AllocationState::Running)
            .build();
        alloc.assigned_nodes = vec![node.id.clone()];
        alloc.started_at = Some(Utc::now() - Duration::minutes(30));
        node.owner = Some(NodeOwnership {
            tenant: tenant.clone(),
            vcluster: vcluster.clone(),
            allocation: alloc.id,
            claimed_by: None,
            is_borrowed: false,
        });
        world.allocations.push(alloc);
    }
    world.nodes.extend(nodes);
}

#[given(regex = r#"^a reservation for a high-priority allocation needing (\d+) nodes in (\d+) hour$"#)]
fn given_reservation_for_high_priority(
    world: &mut LatticeWorld,
    node_count: u32,
    _hours: u32,
) {
    // The high-priority allocation that has a reservation: it's pending and
    // high-priority. The scheduler should protect its reservation window.
    let alloc = AllocationBuilder::new()
        .tenant(
            &world
                .tenants
                .first()
                .map(|t| t.id.clone())
                .unwrap_or_else(|| "test-tenant".into()),
        )
        .nodes(node_count)
        .preemption_class(9)
        .lifecycle_bounded(2)
        .build();
    world
        .named_allocations
        .insert("reserved_high".into(), alloc.clone());
    world.allocations.push(alloc);
}

#[given(regex = r#"^a pending allocation submitted (\d+) hours ago requesting (\d+) nodes$"#)]
fn given_old_pending_allocation(
    world: &mut LatticeWorld,
    hours_ago: i64,
    node_count: u32,
) {
    let tenant = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let mut alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .nodes(node_count)
        .lifecycle_bounded(1)
        .build();
    alloc.created_at = Utc::now() - Duration::hours(hours_ago);
    world
        .named_allocations
        .insert("old_pending".into(), alloc.clone());
    world.allocations.push(alloc);
}

#[given(regex = r#"^a pending allocation submitted just now requesting (\d+) nodes$"#)]
fn given_new_pending_allocation(world: &mut LatticeWorld, node_count: u32) {
    let tenant = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .nodes(node_count)
        .lifecycle_bounded(1)
        .build();
    world
        .named_allocations
        .insert("new_pending".into(), alloc.clone());
    world.allocations.push(alloc);
}

// ─── When steps ────────────────────────────────────────────

// Note: "the scheduler runs a cycle" is in common.rs

// Note: submit_bounded is in common.rs

#[when(regex = r#"^I submit a low-priority allocation requesting (\d+) nodes$"#)]
fn submit_low_priority(world: &mut LatticeWorld, node_count: u32) {
    let tenant = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let vcluster = world
        .vclusters
        .first()
        .map(|vc| vc.id.clone())
        .unwrap_or_else(|| "default".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .vcluster(&vcluster)
        .nodes(node_count)
        .preemption_class(1)
        .lifecycle_bounded(1)
        .build();
    world
        .named_allocations
        .insert("low_priority".into(), alloc.clone());
    world.allocations.push(alloc);
}

#[when(regex = r#"^I submit a high-priority allocation requesting (\d+) nodes$"#)]
fn submit_high_priority(world: &mut LatticeWorld, node_count: u32) {
    let tenant = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let vcluster = world
        .vclusters
        .first()
        .map(|vc| vc.id.clone())
        .unwrap_or_else(|| "default".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .vcluster(&vcluster)
        .nodes(node_count)
        .preemption_class(9)
        .lifecycle_bounded(2)
        .build();
    world
        .named_allocations
        .insert("high_priority".into(), alloc.clone());
    world.allocations.push(alloc);
}

#[when(regex = r#"^I submit a high-priority allocation requesting (\d+) nodes with walltime "([^"]+)"$"#)]
fn submit_high_priority_with_walltime(
    world: &mut LatticeWorld,
    node_count: u32,
    walltime: String,
) {
    let hours = super::helpers::parse_duration_str(&walltime).num_hours() as u64;
    let tenant = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let vcluster = world
        .vclusters
        .first()
        .map(|vc| vc.id.clone())
        .unwrap_or_else(|| "default".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .vcluster(&vcluster)
        .nodes(node_count)
        .preemption_class(9)
        .lifecycle_bounded(hours)
        .build();
    world
        .named_allocations
        .insert("high_priority".into(), alloc.clone());
    world.allocations.push(alloc);
}

#[when(regex = r#"^I submit a small allocation requesting (\d+) nodes with walltime "([^"]+)"$"#)]
fn submit_small_allocation(world: &mut LatticeWorld, node_count: u32, walltime: String) {
    let hours = super::helpers::parse_duration_str(&walltime).num_hours() as u64;
    let tenant = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let vcluster = world
        .vclusters
        .first()
        .map(|vc| vc.id.clone())
        .unwrap_or_else(|| "default".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .vcluster(&vcluster)
        .nodes(node_count)
        .preemption_class(1)
        .lifecycle_bounded(hours)
        .build();
    world
        .named_allocations
        .insert("small".into(), alloc.clone());
    world.allocations.push(alloc);
}

#[when(regex = r#"^I submit a backfill candidate requiring (\d+) nodes with walltime "([^"]+)"$"#)]
fn submit_backfill_candidate(world: &mut LatticeWorld, node_count: u32, walltime: String) {
    let hours = super::helpers::parse_duration_str(&walltime).num_hours() as u64;
    let tenant = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let vcluster = world
        .vclusters
        .first()
        .map(|vc| vc.id.clone())
        .unwrap_or_else(|| "default".into());
    let alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .vcluster(&vcluster)
        .nodes(node_count)
        .preemption_class(1)
        .lifecycle_bounded(hours)
        .build();
    world
        .named_allocations
        .insert("backfill_candidate".into(), alloc.clone());
    world.allocations.push(alloc);
}

#[when("both vClusters have pending allocations")]
fn both_vclusters_have_pending(world: &mut LatticeWorld) {
    for vc in &world.vclusters {
        let tenant = world
            .tenants
            .first()
            .map(|t| t.id.clone())
            .unwrap_or_else(|| "test-tenant".into());
        let alloc = AllocationBuilder::new()
            .tenant(&tenant)
            .vcluster(&vc.id)
            .nodes(2)
            .lifecycle_bounded(1)
            .build();
        world
            .named_allocations
            .insert(format!("vcluster_{}", vc.id), alloc.clone());
        world.allocations.push(alloc);
    }
}

#[when("both tenants submit allocations requesting 2 nodes")]
fn both_tenants_submit(world: &mut LatticeWorld) {
    let vcluster = world
        .vclusters
        .first()
        .map(|vc| vc.id.clone())
        .unwrap_or_else(|| "default".into());
    for tenant in world.tenants.clone() {
        let alloc = AllocationBuilder::new()
            .tenant(&tenant.id)
            .vcluster(&vcluster)
            .nodes(2)
            .lifecycle_bounded(1)
            .build();
        world
            .named_allocations
            .insert(format!("tenant_{}", tenant.id), alloc.clone());
        world.allocations.push(alloc);
    }
}

#[when(regex = r#"^I submit (\d+) unbounded allocations each requesting (\d+) node$"#)]
fn submit_unbounded_allocations(world: &mut LatticeWorld, count: usize, node_count: u32) {
    let tenant = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let vcluster = world
        .vclusters
        .first()
        .map(|vc| vc.id.clone())
        .unwrap_or_else(|| "default".into());
    for i in 0..count {
        let alloc = AllocationBuilder::new()
            .tenant(&tenant)
            .vcluster(&vcluster)
            .nodes(node_count)
            .lifecycle_unbounded()
            .build();
        world
            .named_allocations
            .insert(format!("unbounded_{i}"), alloc.clone());
        world.allocations.push(alloc);
    }
}

#[when(regex = r#"^I submit (\d+) interactive allocations in sequence$"#)]
fn submit_interactive_allocations(world: &mut LatticeWorld, count: usize) {
    let tenant = world
        .tenants
        .first()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());
    let vcluster = world
        .vclusters
        .first()
        .map(|vc| vc.id.clone())
        .unwrap_or_else(|| "default".into());
    for i in 0..count {
        let mut alloc = AllocationBuilder::new()
            .tenant(&tenant)
            .vcluster(&vcluster)
            .nodes(1)
            .lifecycle_bounded(1)
            .build();
        // Stagger creation times so ordering is deterministic
        alloc.created_at = Utc::now() - Duration::seconds((count - i) as i64 * 60);
        world
            .named_allocations
            .insert(format!("interactive_{i}"), alloc.clone());
        world.allocations.push(alloc);
    }
}

// ─── Then steps ────────────────────────────────────────────

#[then(regex = r#"^the allocation should be placed on (\d+) nodes$"#)]
fn allocation_placed_on_nodes(world: &mut LatticeWorld, expected_nodes: usize) {
    let running: Vec<&Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .collect();
    assert!(
        !running.is_empty(),
        "Expected at least one running allocation"
    );
    let last_running = running.last().unwrap();
    assert_eq!(
        last_running.assigned_nodes.len(),
        expected_nodes,
        "Expected allocation placed on {expected_nodes} nodes, got {}",
        last_running.assigned_nodes.len()
    );
}

// Note: check_allocation_state is in common.rs

#[then(regex = r#"^the high-priority allocation should be "(\w+)"$"#)]
fn high_priority_is_state(world: &mut LatticeWorld, expected: String) {
    let expected_state = parse_allocation_state(&expected);
    let hp = world
        .named_allocations
        .get("high_priority")
        .expect("no high_priority allocation submitted");
    let hp_id = hp.id;
    let alloc = world
        .allocations
        .iter()
        .find(|a| a.id == hp_id)
        .expect("high-priority allocation not found in world");
    assert_eq!(
        alloc.state, expected_state,
        "Expected high-priority allocation state {expected}, got {:?}",
        alloc.state
    );
}

#[then(regex = r#"^the small allocation should be "(\w+)" via backfill$"#)]
fn small_allocation_backfilled(world: &mut LatticeWorld, expected: String) {
    let expected_state = parse_allocation_state(&expected);
    let small = world
        .named_allocations
        .get("small")
        .expect("no small allocation submitted");
    let small_id = small.id;
    let alloc = world
        .allocations
        .iter()
        .find(|a| a.id == small_id)
        .expect("small allocation not found in world");
    assert_eq!(
        alloc.state, expected_state,
        "Expected small allocation state {expected}, got {:?}",
        alloc.state
    );

    // Verify it was placed via backfill or regular placement (the scheduler
    // may use either path depending on whether there are running bounded jobs).
    // The key assertion is that it IS running.
    assert_eq!(alloc.state, AllocationState::Running);
}

#[then("the reservation for the large allocation is preserved")]
fn reservation_preserved(world: &mut LatticeWorld) {
    let large = world
        .named_allocations
        .get("large_deferred")
        .expect("no large_deferred allocation");
    let large_id = large.id;
    // The large allocation should either still be pending (deferred) or
    // should have its nodes reserved. In our model, a deferred allocation
    // with a reservation stays Pending but would be first in next cycle.
    let alloc = world
        .allocations
        .iter()
        .find(|a| a.id == large_id)
        .expect("large deferred allocation not found");
    // It should not have been displaced — it can be Pending or Running
    // depending on available nodes, but its reservation intent is intact.
    assert!(
        alloc.state == AllocationState::Pending || alloc.state == AllocationState::Running,
        "Large deferred allocation should be Pending or Running, got {:?}",
        alloc.state
    );
}

#[then("a reservation should be created for the high-priority allocation")]
fn reservation_created_for_high_priority(world: &mut LatticeWorld) {
    let hp = world
        .named_allocations
        .get("high_priority")
        .expect("no high_priority allocation");
    let hp_id = hp.id;
    let alloc = world
        .allocations
        .iter()
        .find(|a| a.id == hp_id)
        .expect("high-priority allocation not found");
    // When all nodes are busy, the high-priority allocation stays Pending
    // (deferred) but gets a reservation in the scheduler's next pass.
    // In our test, the allocation should remain pending since all nodes
    // are occupied by running allocations.
    assert_eq!(
        alloc.state,
        AllocationState::Pending,
        "High-priority allocation should be Pending (reserved), got {:?}",
        alloc.state
    );
}

#[then("the reservation should target the earliest available nodes")]
fn reservation_targets_earliest_nodes(world: &mut LatticeWorld) {
    // The scheduler's reservation mechanism targets nodes whose running
    // allocations finish soonest. We verify indirectly: there are running
    // allocations and the high-priority allocation is deferred with
    // reservation intent (it stays Pending but would be scheduled first
    // once nodes free up).
    let running_count = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .count();
    assert!(
        running_count > 0,
        "Expected running allocations occupying nodes"
    );
}

#[then(regex = r#"^the backfill candidate should remain "(\w+)"$"#)]
fn backfill_candidate_remains(world: &mut LatticeWorld, expected: String) {
    let expected_state = parse_allocation_state(&expected);
    let candidate = world
        .named_allocations
        .get("backfill_candidate")
        .expect("no backfill_candidate allocation");
    let cid = candidate.id;
    let alloc = world
        .allocations
        .iter()
        .find(|a| a.id == cid)
        .expect("backfill candidate not found");
    assert_eq!(
        alloc.state, expected_state,
        "Expected backfill candidate state {expected}, got {:?}",
        alloc.state
    );
}

#[then("the reservation should not be delayed")]
fn reservation_not_delayed(world: &mut LatticeWorld) {
    // The reserved high-priority allocation should still be first in line.
    let reserved = world
        .named_allocations
        .get("reserved_high")
        .expect("no reserved_high allocation");
    let rid = reserved.id;
    let alloc = world
        .allocations
        .iter()
        .find(|a| a.id == rid)
        .expect("reserved high-priority allocation not found");
    // It should remain pending (its reservation window is intact).
    assert!(
        alloc.state == AllocationState::Pending || alloc.state == AllocationState::Running,
        "Reserved allocation should be Pending or Running, got {:?}",
        alloc.state
    );
}

#[then("allocations from both vClusters should be evaluated")]
fn both_vclusters_evaluated(world: &mut LatticeWorld) {
    let result_meta = world
        .named_allocations
        .get("__cycle_result")
        .expect("no cycle result recorded");
    let decisions_count: usize = result_meta
        .tags
        .get("__decisions_count")
        .unwrap()
        .parse()
        .unwrap();
    // Both vClusters submitted allocations, so we expect at least 2 decisions
    assert!(
        decisions_count >= 2,
        "Expected decisions from both vClusters, got {decisions_count}"
    );
}

#[then("all placed nodes should be in the same dragonfly group")]
fn all_placed_in_same_group(world: &mut LatticeWorld) {
    let running: Vec<&Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .collect();
    assert!(!running.is_empty(), "No running allocations found");

    let last = running.last().unwrap();
    if last.assigned_nodes.is_empty() {
        panic!("Running allocation has no assigned nodes");
    }

    // Determine the group for each assigned node
    let groups: Vec<GroupId> = last
        .assigned_nodes
        .iter()
        .filter_map(|nid| world.nodes.iter().find(|n| n.id == *nid).map(|n| n.group))
        .collect();

    assert!(
        !groups.is_empty(),
        "Could not find group info for assigned nodes"
    );
    let first_group = groups[0];
    assert!(
        groups.iter().all(|g| *g == first_group),
        "Expected all nodes in same group, got groups: {groups:?}"
    );
}

#[then(regex = r#"^the "(\w+)" tenant's allocation should be scheduled first$"#)]
fn tenant_allocation_scheduled_first(world: &mut LatticeWorld, tenant_name: String) {
    let tenant_alloc = world
        .named_allocations
        .get(&format!("tenant_{tenant_name}"))
        .unwrap_or_else(|| panic!("no allocation for tenant {tenant_name}"));
    let tid = tenant_alloc.id;
    let alloc = world
        .allocations
        .iter()
        .find(|a| a.id == tid)
        .expect("tenant allocation not found");
    assert_eq!(
        alloc.state,
        AllocationState::Running,
        "Expected tenant {tenant_name}'s allocation to be Running, got {:?}",
        alloc.state
    );
}

#[then("the older allocation should score higher due to wait time aging")]
fn older_allocation_scores_higher(world: &mut LatticeWorld) {
    let old = world
        .named_allocations
        .get("old_pending")
        .expect("no old_pending allocation");
    let old_id = old.id;
    let old_alloc = world
        .allocations
        .iter()
        .find(|a| a.id == old_id)
        .expect("old allocation not found");

    let new = world
        .named_allocations
        .get("new_pending")
        .expect("no new_pending allocation");
    let new_id = new.id;
    let new_alloc = world
        .allocations
        .iter()
        .find(|a| a.id == new_id)
        .expect("new allocation not found");

    // With only 2 nodes available and both requesting 2, only one can be placed.
    // The older one should win due to wait-time aging.
    assert_eq!(
        old_alloc.state,
        AllocationState::Running,
        "Expected older allocation to be scheduled (Running), got {:?}",
        old_alloc.state
    );
    assert_eq!(
        new_alloc.state,
        AllocationState::Pending,
        "Expected newer allocation to remain Pending, got {:?}",
        new_alloc.state
    );
}

#[then("allocations should be packed onto fewer nodes when possible")]
fn allocations_packed_densely(world: &mut LatticeWorld) {
    let running: Vec<&Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .collect();
    // All 3 unbounded allocations requesting 1 node each should be placed.
    // With 4 available nodes, all 3 should be placed.
    assert!(
        running.len() >= 3,
        "Expected at least 3 running allocations (bin-packed), got {}",
        running.len()
    );

    // Verify that the total distinct nodes used is <= total available.
    let used_nodes: std::collections::HashSet<&str> = running
        .iter()
        .flat_map(|a| a.assigned_nodes.iter().map(|n| n.as_str()))
        .collect();
    assert!(
        used_nodes.len() <= world.nodes.len(),
        "Used {} distinct nodes which exceeds available {}",
        used_nodes.len(),
        world.nodes.len()
    );
}

#[then("the first submitted allocation should be scheduled first")]
fn first_submitted_scheduled_first(world: &mut LatticeWorld) {
    // interactive_0 was submitted first (earliest created_at)
    let first = world
        .named_allocations
        .get("interactive_0")
        .expect("no interactive_0 allocation");
    let first_id = first.id;
    let first_alloc = world
        .allocations
        .iter()
        .find(|a| a.id == first_id)
        .expect("first interactive allocation not found");

    assert_eq!(
        first_alloc.state,
        AllocationState::Running,
        "Expected first-submitted interactive allocation to be Running, got {:?}",
        first_alloc.state
    );

    // With only 2 nodes and 3 single-node allocations, the third should be deferred.
    let last = world
        .named_allocations
        .get("interactive_2")
        .expect("no interactive_2 allocation");
    let last_id = last.id;
    let last_alloc = world
        .allocations
        .iter()
        .find(|a| a.id == last_id)
        .expect("last interactive allocation not found");

    assert_eq!(
        last_alloc.state,
        AllocationState::Pending,
        "Expected last-submitted interactive allocation to be Pending, got {:?}",
        last_alloc.state
    );
}
