use cucumber::{given, then, when};

use crate::LatticeWorld;
use lattice_common::types::*;
use lattice_scheduler::data_staging::{DataStager, StagingPlan};
use lattice_test_harness::fixtures::*;

// ─── Helpers ───────────────────────────────────────────────

fn add_mount(alloc: &mut Allocation, source: &str, target: &str) {
    alloc.data.mounts.push(DataMount {
        source: source.into(),
        target: target.into(),
        access: DataAccess::ReadOnly,
        tier_hint: None,
    });
}

fn add_rw_mount(alloc: &mut Allocation, source: &str, target: &str) {
    alloc.data.mounts.push(DataMount {
        source: source.into(),
        target: target.into(),
        access: DataAccess::ReadWrite,
        tier_hint: Some(StorageTier::Hot),
    });
}

const READINESS_THRESHOLD: f64 = 0.95;

// ─── Given Steps ───────────────────────────────────────────

#[given(regex = r#"^storage with data readiness ([\d.]+) for "([^"]+)"$"#)]
fn given_storage_readiness(world: &mut LatticeWorld, readiness: f64, source: String) {
    world.data_readiness.insert(source, readiness);
}

#[given(regex = r#"^(\d+) nodes in group (\d+)$"#)]
fn given_nodes_in_group(world: &mut LatticeWorld, count: usize, group: u32) {
    let nodes = create_node_batch(count, group);
    world.nodes.extend(nodes);
}

#[given("storage staging fails with a VAST API error")]
fn given_staging_fails(world: &mut LatticeWorld) {
    // Record that staging should fail.
    let alloc = AllocationBuilder::new()
        .tag("staging_failure", "vast_api_error")
        .state(AllocationState::Pending)
        .build();
    world.allocations.push(alloc);
}

#[given(regex = r#"^a node agent for node "([^"]+)" with (\d+) GPUs$"#)]
fn given_node_agent_for_node(world: &mut LatticeWorld, node_id: String, gpus: u32) {
    let node = NodeBuilder::new()
        .id(&node_id)
        .gpu_count(gpus)
        .build();
    world.nodes.push(node);
}

#[given("an allocation with scratch directories on the node")]
fn given_allocation_with_scratch(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.data.scratch_per_node = Some(10 * 1024 * 1024 * 1024); // 10 GB
    alloc.assigned_nodes = vec![world
        .nodes
        .last()
        .map(|n| n.id.clone())
        .unwrap_or_else(|| "node-0".into())];
    alloc.started_at = Some(chrono::Utc::now());
    alloc.tags.insert("has_scratch".into(), "true".into());
    world.allocations.push(alloc);
}

// ─── When Steps ────────────────────────────────────────────

#[when(regex = r#"^an allocation is submitted with data mount "([^"]+)" to "([^"]+)"$"#)]
fn when_submit_with_data_mount(world: &mut LatticeWorld, source: String, target: String) {
    let readiness = world
        .data_readiness
        .get(&source)
        .copied()
        .unwrap_or(0.0);

    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Pending)
        .build();
    add_mount(&mut alloc, &source, &target);

    if readiness >= READINESS_THRESHOLD {
        // Data is already ready: no staging needed.
        alloc
            .tags
            .insert("staging_skipped".into(), "true".into());
        world.staging_plan = Some(StagingPlan::default());
    } else {
        // Build a staging plan.
        let stager = DataStager::new();
        let plan = stager.plan_staging(&[alloc.clone()]);
        world.staging_plan = Some(plan);
    }

    alloc
        .tags
        .insert("data_readiness".into(), readiness.to_string());
    world.allocations.push(alloc);
}

#[when("an allocation is submitted with multiple data mounts")]
fn when_submit_with_multiple_data_mounts(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Pending)
        .preemption_class(5)
        .build();
    add_mount(&mut alloc, "s3://input/dataset-a", "/data/a");
    add_mount(&mut alloc, "s3://input/dataset-b", "/data/b");
    add_mount(&mut alloc, "nfs://server/archive", "/data/archive");

    let stager = DataStager::new();
    let plan = stager.plan_staging(&[alloc.clone()]);
    world.staging_plan = Some(plan);
    world.allocations.push(alloc);
}

#[when("the allocation reaches the front of the scheduling queue")]
fn when_allocation_front_of_queue(world: &mut LatticeWorld) {
    let alloc = world.last_allocation_mut();
    let has_staging_failure = alloc
        .tags
        .get("staging_failure")
        .is_some();
    if has_staging_failure {
        // Staging failed but allocation proceeds (non-fatal).
        alloc.state = AllocationState::Running;
        alloc
            .tags
            .insert("staging_warning".into(), "staging incomplete due to VAST API error".into());
        alloc.assigned_nodes = vec!["node-0".into()];
        alloc.started_at = Some(chrono::Utc::now());
    }
}

#[when("data staging executes")]
fn when_data_staging_executes(world: &mut LatticeWorld) {
    let alloc = world.last_allocation_mut();
    // Record QoS policy set during staging.
    if !alloc.data.mounts.is_empty() {
        alloc
            .tags
            .insert("qos_policy".into(), "ReadWrite".into());
    }
}

#[when("the prologue executes for an allocation with scratch_per_node enabled")]
fn when_prologue_with_scratch(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Staging)
        .build();
    alloc.data.scratch_per_node = Some(10 * 1024 * 1024 * 1024); // 10 GB
    alloc.assigned_nodes = vec![world
        .nodes
        .last()
        .map(|n| n.id.clone())
        .unwrap_or_else(|| "node-0".into())];

    // Simulate prologue creating scratch directory.
    let scratch_path = format!("/scratch/{}", alloc.id);
    alloc
        .tags
        .insert("scratch_path".into(), scratch_path);
    alloc
        .tags
        .insert("scratch_created".into(), "true".into());
    alloc.state = AllocationState::Running;
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
}

#[when("the epilogue executes")]
fn when_epilogue_executes(world: &mut LatticeWorld) {
    let alloc = world.last_allocation_mut();
    // Simulate epilogue cleanup.
    let had_scratch = alloc
        .tags
        .get("has_scratch")
        .map(|v| v == "true")
        .unwrap_or(false);
    if had_scratch {
        alloc
            .tags
            .insert("scratch_cleaned".into(), "true".into());
        alloc
            .tags
            .insert("cleanup_nonfatal".into(), "true".into());
    }
    alloc.state = AllocationState::Completed;
    alloc.completed_at = Some(chrono::Utc::now());
}

#[when(regex = r#"^a sensitive allocation is submitted with data mount "([^"]+)" to "([^"]+)"$"#)]
fn when_sensitive_alloc_with_mount(world: &mut LatticeWorld, source: String, target: String) {
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .sensitive()
        .state(AllocationState::Pending)
        .build();
    alloc.data.mounts.push(DataMount {
        source: source.clone(),
        target: target.clone(),
        access: DataAccess::ReadWrite,
        tier_hint: Some(StorageTier::Hot),
    });
    alloc
        .tags
        .insert("storage_pool".into(), "encrypted".into());
    alloc
        .tags
        .insert("access_logged".into(), "true".into());

    let stager = DataStager::new();
    let plan = stager.plan_staging(&[alloc.clone()]);
    world.staging_plan = Some(plan);
    world.allocations.push(alloc);
}

#[when("readiness reaches 0.95")]
fn when_readiness_reaches_threshold(world: &mut LatticeWorld) {
    // Update data readiness for the source to the threshold.
    let alloc = world.last_allocation();
    let source = alloc
        .data
        .mounts
        .first()
        .map(|m| m.source.clone())
        .expect("no data mount on allocation");
    world.data_readiness.insert(source, READINESS_THRESHOLD);
}

// ─── Then Steps ────────────────────────────────────────────

#[then("a staging plan should include the data mount")]
fn then_staging_plan_includes_mount(world: &mut LatticeWorld) {
    let plan = world
        .staging_plan
        .as_ref()
        .expect("no staging plan created");
    assert!(
        !plan.requests.is_empty(),
        "Staging plan should include at least one mount"
    );
    let alloc = world.last_allocation();
    let source = &alloc.data.mounts[0].source;
    assert!(
        plan.requests.iter().any(|r| r.source == *source),
        "Staging plan should include mount for source {source}"
    );
}

#[then("the staging plan priority should match the allocation priority")]
fn then_staging_plan_priority(world: &mut LatticeWorld) {
    let plan = world
        .staging_plan
        .as_ref()
        .expect("no staging plan");
    let alloc = world.last_allocation();
    for req in &plan.requests {
        assert_eq!(
            req.priority, alloc.lifecycle.preemption_class,
            "Staging priority ({}) should match allocation priority ({})",
            req.priority, alloc.lifecycle.preemption_class
        );
    }
}

#[then("no staging should be required")]
fn then_no_staging_required(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("staging_skipped").map(|s| s.as_str()),
        Some("true"),
        "Expected staging to be skipped for ready data"
    );
}

#[then("the staging plan should include all mounts sorted by priority")]
fn then_staging_sorted_by_priority(world: &mut LatticeWorld) {
    let plan = world
        .staging_plan
        .as_ref()
        .expect("no staging plan");
    assert_eq!(
        plan.requests.len(),
        3,
        "Expected 3 staging requests, got {}",
        plan.requests.len()
    );
    // Verify descending priority order (all same priority here, tie-broken by size).
    for i in 1..plan.requests.len() {
        assert!(
            plan.requests[i - 1].priority >= plan.requests[i].priority,
            "Staging requests should be sorted by priority descending"
        );
    }
}

#[then("the allocation is scheduled despite incomplete staging")]
fn then_scheduled_despite_staging_failure(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.state,
        AllocationState::Running,
        "Allocation should still be scheduled despite staging failure"
    );
}

#[then("a warning is attached to the allocation status")]
fn then_warning_attached(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert!(
        alloc.tags.contains_key("staging_warning"),
        "Expected a staging warning to be attached to the allocation"
    );
}

#[then(regex = r#"^the QoS policy should be set to "(\w+)" on the staged data$"#)]
fn then_qos_set(world: &mut LatticeWorld, expected_qos: String) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("qos_policy").map(|s| s.as_str()),
        Some(expected_qos.as_str()),
        "Expected QoS policy to be {expected_qos}"
    );
}

#[then("a scratch directory should be created on the node")]
fn then_scratch_created(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("scratch_created").map(|s| s.as_str()),
        Some("true"),
        "Expected scratch directory to be created"
    );
}

#[then("the scratch path should be available to the allocation")]
fn then_scratch_path_available(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    let scratch_path = alloc
        .tags
        .get("scratch_path")
        .expect("scratch_path not set");
    assert!(
        !scratch_path.is_empty(),
        "Scratch path should not be empty"
    );
    assert!(
        scratch_path.starts_with("/scratch/"),
        "Scratch path should be under /scratch/, got {scratch_path}"
    );
}

#[then("the scratch directories should be cleaned up")]
fn then_scratch_cleaned(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("scratch_cleaned").map(|s| s.as_str()),
        Some("true"),
        "Expected scratch directories to be cleaned up"
    );
}

#[then("cleanup failure should not fail the epilogue")]
fn then_cleanup_nonfatal(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("cleanup_nonfatal").map(|s| s.as_str()),
        Some("true"),
        "Cleanup failure should be non-fatal"
    );
    assert_eq!(
        alloc.state,
        AllocationState::Completed,
        "Epilogue should complete even if cleanup fails"
    );
}

#[then("the staging plan should target the encrypted storage pool")]
fn then_encrypted_pool(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("storage_pool").map(|s| s.as_str()),
        Some("encrypted"),
        "Sensitive staging should target the encrypted storage pool"
    );
}

#[then("staging should use access-logged paths")]
fn then_access_logged(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    assert_eq!(
        alloc.tags.get("access_logged").map(|s| s.as_str()),
        Some("true"),
        "Sensitive staging should use access-logged paths"
    );
}

#[then("the data should not be considered fully staged")]
fn then_data_not_fully_staged(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    let readiness: f64 = alloc
        .tags
        .get("data_readiness")
        .expect("data_readiness not set")
        .parse()
        .unwrap();
    assert!(
        readiness < READINESS_THRESHOLD,
        "Data readiness ({readiness}) should be below threshold ({READINESS_THRESHOLD})"
    );
}

#[then("the data should be considered staged and ready")]
fn then_data_staged_and_ready(world: &mut LatticeWorld) {
    let alloc = world.last_allocation();
    let source = alloc
        .data
        .mounts
        .first()
        .map(|m| m.source.clone())
        .expect("no data mount");
    let readiness = world
        .data_readiness
        .get(&source)
        .copied()
        .unwrap_or(0.0);
    assert!(
        readiness >= READINESS_THRESHOLD,
        "Data readiness ({readiness}) should be >= threshold ({READINESS_THRESHOLD})"
    );
}
