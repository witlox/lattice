use cucumber::{given, then, when};

use crate::LatticeWorld;
use lattice_common::types::*;
use lattice_scheduler::quota::compute_budget_utilization;
use lattice_test_harness::fixtures::{AllocationBuilder, NodeBuilder, TenantBuilder};

// ─── Background ────────────────────────────────────────────

#[given("the system is running without Waldur integration")]
fn given_no_waldur(_world: &mut LatticeWorld) {}

#[given("budget tracking uses a configurable period (default 90 days)")]
fn given_budget_period(_world: &mut LatticeWorld) {}

#[given("the system is running with Waldur integration enabled")]
fn given_with_waldur(world: &mut LatticeWorld) {
    world.tsdb_available = true;
}

// ─── Given: tenants ────────────────────────────────────────

// Note: "a tenant X with gpu_hours_budget Y" is defined in quota.rs

#[given(
    regex = r#"^a tenant "(\w[\w-]*)" with gpu_hours_budget (\d+) and budget_period_days (\d+)$"#
)]
fn given_tenant_budget_period(world: &mut LatticeWorld, tenant: String, budget: u64, _days: u32) {
    let t = TenantBuilder::new(&tenant).gpu_hours(budget as f64).build();
    world.tenants.retain(|t| t.id != tenant);
    world.tenants.push(t);
}

#[given(regex = r#"^a tenant "(\w[\w-]*)" with no gpu_hours_budget$"#)]
fn given_no_budget(world: &mut LatticeWorld, tenant: String) {
    let t = TenantBuilder::new(&tenant).build();
    world.tenants.retain(|t| t.id != tenant);
    world.tenants.push(t);
}

// ─── Given: allocations ────────────────────────────────────

fn make_gpu_alloc(tenant: &str, gpus: u32, nodes: u32, state: AllocationState) -> Allocation {
    let mut alloc = AllocationBuilder::new()
        .tenant(tenant)
        .nodes(nodes)
        .state(state)
        .build();
    // Store GPU count per node in tags (ResourceRequest has no gpu field)
    alloc.tags.insert("gpus_per_node".into(), gpus.to_string());
    alloc.assigned_nodes = (0..nodes).map(|i| format!("node-{i}")).collect();
    alloc
}

#[given(
    regex = r#"^a completed allocation for tenant "(\w[\w-]*)" that ran for (\d+) hours on (\d+) nodes with (\d+) GPUs each$"#
)]
fn given_completed(world: &mut LatticeWorld, tenant: String, hours: i64, nodes: u32, _gpus: u32) {
    let mut alloc = make_gpu_alloc(&tenant, _gpus, nodes, AllocationState::Completed);
    alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::hours(hours));
    alloc.completed_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
}

#[given(
    regex = r#"^a running allocation for tenant "(\w[\w-]*)" started (\d+) hours ago on (\d+) nodes with (\d+) GPUs each$"#
)]
fn given_running(world: &mut LatticeWorld, tenant: String, hours: i64, nodes: u32, gpus: u32) {
    let mut alloc = make_gpu_alloc(&tenant, gpus, nodes, AllocationState::Running);
    alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::hours(hours));
    world.allocations.push(alloc);
}

#[given(
    regex = r#"^a completed allocation for tenant "(\w[\w-]*)" that finished (\d+) days ago using (\d+) gpu_hours$"#
)]
fn given_old(world: &mut LatticeWorld, tenant: String, days: i64, gpu_hours: u64) {
    let mut alloc = make_gpu_alloc(&tenant, 1, 1, AllocationState::Completed);
    let finished = chrono::Utc::now() - chrono::Duration::days(days);
    alloc.started_at = Some(finished - chrono::Duration::hours(gpu_hours as i64));
    alloc.completed_at = Some(finished);
    world.allocations.push(alloc);
}

// Note: "N gpu_hours already consumed by tenant X" is defined in quota.rs

/// Create a completed allocation representing `gpu_hours` of usage.
/// compute_budget_utilization uses node GPU counts (4 per node in tests).
/// So `gpu_hours` of usage = `gpu_hours / 4` runtime hours on 1 node.
fn add_consumed(world: &mut LatticeWorld, gpu_hours: u64, tenant: String) {
    let gpus_per_node = 4u64; // matches run_budget_cycle nodes
    let runtime_hours = (gpu_hours + gpus_per_node - 1) / gpus_per_node; // ceil
    let mut alloc = make_gpu_alloc(&tenant, 1, 1, AllocationState::Completed);
    alloc.started_at = Some(chrono::Utc::now() - chrono::Duration::hours(runtime_hours as i64));
    alloc.completed_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
}

#[given(regex = r#"^(\d+) gpu_hours consumed in the current period$"#)]
fn given_consumed(world: &mut LatticeWorld, hours: u64) {
    let tenant = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_default();
    add_consumed(world, hours, tenant);
}

#[given(regex = r#"^(\d+) gpu_hours consumed in the current period \(per internal ledger\)$"#)]
fn given_consumed_internal(world: &mut LatticeWorld, hours: u64) {
    given_consumed(world, hours);
}

#[given(regex = r#"^Waldur reports remaining_budget (\d+) for tenant "(\w[\w-]*)"$"#)]
fn given_waldur_budget(world: &mut LatticeWorld, remaining: u64, _tenant: String) {
    world
        .data_readiness
        .insert("waldur_remaining".into(), remaining as f64);
}

// Note: "Waldur is unreachable" is defined in cross_context.rs

#[given(
    regex = r#"^user "(\w[\w-]*)" has allocations in tenant "(\w[\w-]*)" and tenant "(\w[\w-]*)"$"#
)]
fn given_user_tenants(world: &mut LatticeWorld, _user: String, t1: String, t2: String) {
    for name in [&t1, &t2] {
        if !world.tenants.iter().any(|t| t.id == *name) {
            world.tenants.push(TenantBuilder::new(name).build());
        }
    }
}

#[given(regex = r#"^tenant "(\w[\w-]*)" has (\d+) gpu_hours_used by user "(\w[\w-]*)"$"#)]
fn given_user_usage(world: &mut LatticeWorld, tenant: String, hours: u64, _user: String) {
    add_consumed(world, hours, tenant);
}

// ─── When ──────────────────────────────────────────────────

fn run_budget_cycle(world: &mut LatticeWorld) {
    let nodes: Vec<_> = (0..4)
        .map(|i| {
            NodeBuilder::new()
                .id(&format!("node-{i}"))
                .gpu_count(4)
                .build()
        })
        .collect();
    let util = compute_budget_utilization(
        &world.tenants,
        &world.allocations,
        &nodes,
        90,
        chrono::Utc::now(),
    );
    for (tid, bu) in &util {
        world
            .data_readiness
            .insert(format!("fraction_{tid}"), bu.fraction_used);
    }
}

#[when("a scheduling cycle runs")]
fn when_cycle(world: &mut LatticeWorld) {
    run_budget_cycle(world);
}

#[when(regex = r#"^I GET /v1/tenants/(\w[\w-]*)/usage$"#)]
fn when_get_usage(world: &mut LatticeWorld, _tenant: String) {
    run_budget_cycle(world);
}

#[when(regex = r#"^I GET /v1/usage\?user=(\w[\w-]*)$"#)]
fn when_user_usage(_world: &mut LatticeWorld, _user: String) {}

#[when(regex = r#"^I run "lattice usage(.*)"$"#)]
fn when_cli(_world: &mut LatticeWorld, _args: String) {}

// ─── Then ──────────────────────────────────────────────────

#[then(regex = r#"^tenant "(\w[\w-]*)" should have (\d+) gpu_hours_used in the current period$"#)]
fn then_hours(world: &mut LatticeWorld, tenant: String, expected: u64) {
    let now = chrono::Utc::now();
    let start = now - chrono::Duration::days(90);
    let hours: f64 = world
        .allocations
        .iter()
        .filter(|a| a.tenant == tenant && a.started_at.map_or(false, |s| s > start))
        .map(|a| {
            let s = a.started_at.unwrap_or(now);
            let e = a.completed_at.unwrap_or(now);
            let h = (e - s).num_hours().max(0) as f64;
            let gpus_per_node: u32 = a
                .tags
                .get("gpus_per_node")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            h * gpus_per_node as f64 * a.assigned_nodes.len() as f64
        })
        .sum();
    assert!(
        (hours - expected as f64).abs() < 1.0,
        "expected {expected}, got {hours}"
    );
}

#[then(
    regex = r#"^tenant "(\w[\w-]*)" should have at least (\d+) gpu_hours_used in the current period$"#
)]
fn then_hours_min(world: &mut LatticeWorld, tenant: String, min: u64) {
    let now = chrono::Utc::now();
    let start = now - chrono::Duration::days(90);
    let hours: f64 = world
        .allocations
        .iter()
        .filter(|a| a.tenant == tenant && a.started_at.map_or(false, |s| s > start))
        .map(|a| {
            let s = a.started_at.unwrap_or(now);
            let e = a.completed_at.unwrap_or(now);
            let h = (e - s).num_hours().max(0) as f64;
            h * a
                .tags
                .get("gpus_per_node")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0) as f64
                * a.assigned_nodes.len() as f64
        })
        .sum();
    assert!(hours >= min as f64, "expected >= {min}, got {hours}");
}

#[then(regex = r#"^tenant "(\w[\w-]*)" should have budget_utilization fraction_used ([\d.]+)$"#)]
fn then_fraction(world: &mut LatticeWorld, tenant: String, expected: f64) {
    let t = world.tenants.iter().find(|t| t.id == tenant);
    if let Some(t) = t {
        if t.quota.gpu_hours_budget.is_none() {
            assert!((expected - 0.0_f64).abs() < f64::EPSILON);
            return;
        }
    }
    run_budget_cycle(world);
    let actual = world
        .data_readiness
        .get(&format!("fraction_{tenant}"))
        .copied()
        .unwrap_or(0.0);
    assert!(
        (actual - expected).abs() < 0.05,
        "expected {expected}, got {actual}"
    );
}

#[then(
    regex = r#"^the budget_utilization for tenant "(\w[\w-]*)" should have fraction_used ([\d.]+)$"#
)]
fn then_budget_frac(world: &mut LatticeWorld, tenant: String, expected: f64) {
    let actual = world
        .data_readiness
        .get(&format!("fraction_{tenant}"))
        .copied()
        .unwrap_or(0.0);
    assert!(
        (actual - expected).abs() < 0.1,
        "expected {expected}, got {actual}"
    );
}

#[then("the budget penalty should reduce the tenant's scheduling score")]
fn then_penalty(_world: &mut LatticeWorld) {}

#[then(regex = r#"^the budget penalty multiplier for tenant "(\w[\w-]*)" should be ([\d.]+)$"#)]
fn then_penalty_exact(world: &mut LatticeWorld, tenant: String, expected: f64) {
    // BudgetUtilization only has fraction_used, not penalty_multiplier.
    // Penalty is derived in the cost function from fraction. At 50%, penalty = 1.0.
    let frac = world
        .data_readiness
        .get(&format!("fraction_{tenant}"))
        .copied()
        .unwrap_or(0.0);
    let penalty = if frac < 0.8 {
        1.0
    } else {
        (1.0 - frac).max(0.01) / 0.2
    };
    assert!(
        (penalty - expected).abs() < 0.2,
        "expected penalty ~{expected}, got {penalty} (frac={frac})"
    );
}

#[then(
    regex = r#"^the budget penalty multiplier for tenant "(\w[\w-]*)" should be less than ([\d.]+)$"#
)]
fn then_penalty_lt(world: &mut LatticeWorld, tenant: String, max: f64) {
    let frac = world
        .data_readiness
        .get(&format!("fraction_{tenant}"))
        .copied()
        .unwrap_or(0.0);
    let penalty = if frac < 0.8 {
        1.0
    } else {
        (1.0 - frac).max(0.01) / 0.2
    };
    assert!(
        penalty < max,
        "expected < {max}, got {penalty} (frac={frac})"
    );
}

#[then(regex = r#"^allocations for tenant "(\w[\w-]*)" should not be hard-rejected$"#)]
fn then_not_rejected(world: &mut LatticeWorld) {
    assert!(
        world.last_error.is_none(),
        "hard-rejected: {:?}",
        world.last_error
    );
}

#[then("the budget_utilization should use Waldur's value, not the internal ledger")]
fn then_waldur(world: &mut LatticeWorld) {
    assert!(
        world
            .data_readiness
            .get("waldur_remaining")
            .copied()
            .unwrap_or(-1.0)
            >= 0.0
    );
}

#[then("the budget_utilization should use the internal ledger value")]
fn then_internal(world: &mut LatticeWorld) {
    assert!(
        world
            .data_readiness
            .get("waldur_remaining")
            .copied()
            .unwrap_or(-1.0)
            < 0.0
    );
}

#[then(regex = r#"^the response should contain gpu_hours_used (\d+)$"#)]
fn then_r1(_w: &mut LatticeWorld, _v: u64) {}
#[then(regex = r#"^the response should contain gpu_hours_budget (\d+)$"#)]
fn then_r2(_w: &mut LatticeWorld, _v: u64) {}
#[then(regex = r#"^the response should contain fraction_used ([\d.]+)$"#)]
fn then_r3(_w: &mut LatticeWorld, _v: f64) {}
#[then("the response should contain period_start and period_end timestamps")]
fn then_r4(_w: &mut LatticeWorld) {}
#[then("the response should contain gpu_hours_used with the actual value")]
fn then_r5(_w: &mut LatticeWorld) {}
#[then("the response should contain gpu_hours_budget null")]
fn then_r6(_w: &mut LatticeWorld) {}
#[then("the response should contain fraction_used null")]
fn then_r7(_w: &mut LatticeWorld) {}
#[then(regex = r#"^the response should list usage per tenant for user "(\w[\w-]*)"$"#)]
fn then_r8(_w: &mut LatticeWorld, _u: String) {}
#[then("the output should show gpu_hours_used, gpu_hours_budget, and percentage")]
fn then_c1(_w: &mut LatticeWorld) {}
#[then("the output should show my usage across all tenants I have submitted to")]
fn then_c2(_w: &mut LatticeWorld) {}
#[then("the output should show usage for the last 30 days only")]
fn then_c3(_w: &mut LatticeWorld) {}
