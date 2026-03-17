use cucumber::{given, then, when};

use crate::LatticeWorld;
use hpc_node::{CgroupManager, ResourceLimits};
use lattice_node_agent::cgroup::StubCgroupManager;

// ─── Given Steps ────────────────────────────────────────────

#[given("a stub cgroup manager")]
fn given_stub_cgroup_manager(world: &mut LatticeWorld) {
    world.cgroup_manager = Some(StubCgroupManager.into());
    world.cgroup_handle = None;
    world.cgroup_metrics = None;
    world.cgroup_result_ok = None;
    world.cgroup_is_empty = None;
}

#[given(regex = r#"^a scope "([^"]+)" exists under workload.slice$"#)]
fn given_scope_exists(world: &mut LatticeWorld, name: String) {
    let mgr = world.cgroup_manager.as_ref().expect("no cgroup manager");
    let handle = mgr
        .create_scope(
            hpc_node::cgroup::slices::WORKLOAD_ROOT,
            &name,
            &ResourceLimits::default(),
        )
        .unwrap();
    world.cgroup_handle = Some(handle);
}

// ─── When Steps ─────────────────────────────────────────────

#[when("the hierarchy is created")]
fn when_hierarchy_created(world: &mut LatticeWorld) {
    let mgr = world.cgroup_manager.as_ref().expect("no cgroup manager");
    let result = mgr.create_hierarchy();
    world.cgroup_result_ok = Some(result.is_ok());
}

#[when("the hierarchy is created again")]
fn when_hierarchy_created_again(world: &mut LatticeWorld) {
    let mgr = world.cgroup_manager.as_ref().expect("no cgroup manager");
    let result = mgr.create_hierarchy();
    world.cgroup_result_ok = Some(result.is_ok());
}

#[when(regex = r#"^a scope "([^"]+)" is created under workload.slice$"#)]
fn when_scope_created(world: &mut LatticeWorld, name: String) {
    let mgr = world.cgroup_manager.as_ref().expect("no cgroup manager");
    let handle = mgr
        .create_scope(
            hpc_node::cgroup::slices::WORKLOAD_ROOT,
            &name,
            &ResourceLimits::default(),
        )
        .unwrap();
    world.cgroup_handle = Some(handle);
    world.cgroup_result_ok = Some(true);
}

#[when(regex = r#"^a scope "([^"]+)" is created with memory limit (\d+)$"#)]
fn when_scope_with_memory(world: &mut LatticeWorld, name: String, memory_max: u64) {
    let mgr = world.cgroup_manager.as_ref().expect("no cgroup manager");
    let limits = ResourceLimits {
        memory_max: Some(memory_max),
        cpu_weight: None,
        io_max: None,
    };
    let handle = mgr
        .create_scope(hpc_node::cgroup::slices::WORKLOAD_ROOT, &name, &limits)
        .unwrap();
    world.cgroup_handle = Some(handle);
    world.cgroup_result_ok = Some(true);
}

#[when(regex = r#"^a scope "([^"]+)" is created with CPU weight (\d+)$"#)]
fn when_scope_with_cpu(world: &mut LatticeWorld, name: String, cpu_weight: u16) {
    let mgr = world.cgroup_manager.as_ref().expect("no cgroup manager");
    let limits = ResourceLimits {
        memory_max: None,
        cpu_weight: Some(cpu_weight),
        io_max: None,
    };
    let handle = mgr
        .create_scope(hpc_node::cgroup::slices::WORKLOAD_ROOT, &name, &limits)
        .unwrap();
    world.cgroup_handle = Some(handle);
    world.cgroup_result_ok = Some(true);
}

#[when(regex = r#"^a scope "([^"]+)" is created with no resource limits$"#)]
fn when_scope_no_limits(world: &mut LatticeWorld, name: String) {
    let mgr = world.cgroup_manager.as_ref().expect("no cgroup manager");
    let handle = mgr
        .create_scope(
            hpc_node::cgroup::slices::WORKLOAD_ROOT,
            &name,
            &ResourceLimits::default(),
        )
        .unwrap();
    world.cgroup_handle = Some(handle);
    world.cgroup_result_ok = Some(true);
}

#[when("the scope is destroyed")]
fn when_scope_destroyed(world: &mut LatticeWorld) {
    let mgr = world.cgroup_manager.as_ref().expect("no cgroup manager");
    let handle = world.cgroup_handle.as_ref().expect("no scope handle");
    let result = mgr.destroy_scope(handle);
    world.cgroup_result_ok = Some(result.is_ok());
}

#[when("metrics are read from a scope path")]
fn when_metrics_read(world: &mut LatticeWorld) {
    let mgr = world.cgroup_manager.as_ref().expect("no cgroup manager");
    let metrics = mgr.read_metrics("/any/scope/path").unwrap();
    world.cgroup_metrics = Some(metrics);
}

#[when("the scope emptiness is checked")]
fn when_scope_emptiness_checked(world: &mut LatticeWorld) {
    let mgr = world.cgroup_manager.as_ref().expect("no cgroup manager");
    let handle = world.cgroup_handle.as_ref().expect("no scope handle");
    let empty = mgr.is_scope_empty(handle).unwrap();
    world.cgroup_is_empty = Some(empty);
}

// ─── Then Steps ─────────────────────────────────────────────

#[then("hierarchy creation should succeed")]
fn then_hierarchy_ok(world: &mut LatticeWorld) {
    assert!(
        world.cgroup_result_ok.unwrap_or(false),
        "expected hierarchy creation to succeed"
    );
}

#[then(regex = r#"^the scope path should contain "([^"]+)"$"#)]
fn then_scope_path_contains(world: &mut LatticeWorld, expected: String) {
    let handle = world.cgroup_handle.as_ref().expect("no scope handle");
    assert!(
        handle.path.contains(&expected),
        "expected scope path to contain '{expected}', got '{}'",
        handle.path
    );
}

#[then("the scope should be created successfully")]
fn then_scope_created(world: &mut LatticeWorld) {
    assert!(
        world.cgroup_result_ok.unwrap_or(false),
        "expected scope creation to succeed"
    );
}

#[then("scope destruction should succeed")]
fn then_scope_destroyed(world: &mut LatticeWorld) {
    assert!(
        world.cgroup_result_ok.unwrap_or(false),
        "expected scope destruction to succeed"
    );
}

#[then("the metrics should have zero memory usage")]
fn then_metrics_zero_memory(world: &mut LatticeWorld) {
    let metrics = world.cgroup_metrics.as_ref().expect("no metrics");
    assert_eq!(
        metrics.memory_current, 0,
        "expected zero memory usage, got {}",
        metrics.memory_current
    );
}

#[then("the metrics should have zero CPU usage")]
fn then_metrics_zero_cpu(world: &mut LatticeWorld) {
    let metrics = world.cgroup_metrics.as_ref().expect("no metrics");
    assert_eq!(
        metrics.cpu_usage_usec, 0,
        "expected zero CPU usage, got {}",
        metrics.cpu_usage_usec
    );
}

#[then("the metrics should have zero processes")]
fn then_metrics_zero_procs(world: &mut LatticeWorld) {
    let metrics = world.cgroup_metrics.as_ref().expect("no metrics");
    assert_eq!(
        metrics.nr_processes, 0,
        "expected zero processes, got {}",
        metrics.nr_processes
    );
}

#[then("the scope should be empty")]
fn then_scope_empty(world: &mut LatticeWorld) {
    assert!(
        world.cgroup_is_empty.unwrap_or(false),
        "expected scope to be empty"
    );
}
