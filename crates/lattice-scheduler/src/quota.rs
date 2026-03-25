//! Soft quota checking for the cost function.
//!
//! Hard quota enforcement is done by the quorum (lattice-quorum).
//! This module provides soft quota signals that feed into the
//! fair-share factor (f₃) of the cost function, and budget
//! utilization from the internal budget ledger.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use lattice_common::types::*;

use crate::cost::{BudgetUtilization, TenantUsage};

/// Compute per-tenant usage from current allocations.
pub fn compute_tenant_usage(
    tenants: &[Tenant],
    allocations: &[Allocation],
    total_nodes: u32,
) -> HashMap<TenantId, TenantUsage> {
    let mut usage = HashMap::new();

    // Compute system-wide utilization once
    let total_nodes_in_use: u32 = allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .map(|a| a.assigned_nodes.len() as u32)
        .sum();
    let system_utilization = if total_nodes > 0 {
        total_nodes_in_use as f64 / total_nodes as f64
    } else {
        0.0
    };

    for tenant in tenants {
        let nodes_in_use: u32 = allocations
            .iter()
            .filter(|a| a.tenant == tenant.id && a.state == AllocationState::Running)
            .map(|a| a.assigned_nodes.len() as u32)
            .sum();

        let actual_usage = if total_nodes > 0 {
            nodes_in_use as f64 / total_nodes as f64
        } else {
            0.0
        };

        usage.insert(
            tenant.id.clone(),
            TenantUsage {
                target_share: tenant.quota.fair_share_target,
                actual_usage,
                burst_allowance: tenant.quota.burst_allowance,
                system_utilization,
            },
        );
    }

    usage
}

/// Check if a tenant has reached their hard quota limits.
///
/// Returns `Some(reason)` if the limit is exceeded, `None` if okay.
pub fn check_hard_quota(tenant: &Tenant, running_count: u32, nodes_in_use: u32) -> Option<String> {
    // Check max concurrent allocations
    if let Some(max_concurrent) = tenant.quota.max_concurrent_allocations {
        if running_count >= max_concurrent {
            return Some(format!(
                "tenant {} exceeds max_concurrent_allocations: {} >= {}",
                tenant.id, running_count, max_concurrent
            ));
        }
    }

    // Check max nodes
    if nodes_in_use >= tenant.quota.max_nodes {
        return Some(format!(
            "tenant {} exceeds max_nodes: {} >= {}",
            tenant.id, nodes_in_use, tenant.quota.max_nodes
        ));
    }

    None
}

/// Hours elapsed for an allocation (0 if not started).
fn allocation_elapsed_hours(alloc: &Allocation, now: DateTime<Utc>) -> f64 {
    let started = match alloc.started_at {
        Some(t) => t,
        None => return 0.0,
    };
    let end = alloc.completed_at.unwrap_or(now);
    (end - started).num_seconds().max(0) as f64 / 3600.0
}

/// Node-hours consumed by an allocation: nodes × hours.
fn allocation_node_hours(alloc: &Allocation, now: DateTime<Utc>) -> f64 {
    let hours = allocation_elapsed_hours(alloc, now);
    hours * alloc.assigned_nodes.len() as f64
}

/// GPU-hours consumed by an allocation: Σ(gpu_count per node) × hours.
/// For nodes not found in the map, falls back to 1 GPU (conservative).
fn allocation_gpu_hours(
    alloc: &Allocation,
    now: DateTime<Utc>,
    node_gpu_counts: &HashMap<NodeId, u32>,
) -> f64 {
    let hours = allocation_elapsed_hours(alloc, now);
    let gpus: u32 = alloc
        .assigned_nodes
        .iter()
        .map(|nid| node_gpu_counts.get(nid).copied().unwrap_or(1))
        .sum();
    hours * gpus as f64
}

/// Usage metrics for a tenant within a budget period.
#[derive(Debug, Clone, Default)]
pub struct TenantUsageMetrics {
    pub gpu_hours_used: f64,
    pub node_hours_used: f64,
}

/// Compute per-tenant budget utilization from allocation history.
///
/// Tracks both GPU-hours and node-hours. When both budgets are set,
/// the worse (higher) utilization fraction drives the penalty.
pub fn compute_budget_utilization(
    tenants: &[Tenant],
    allocations: &[Allocation],
    nodes: &[Node],
    budget_period_days: u32,
    now: DateTime<Utc>,
) -> HashMap<TenantId, BudgetUtilization> {
    let period_start = now - chrono::Duration::days(budget_period_days as i64);

    let node_gpu_counts: HashMap<NodeId, u32> = nodes
        .iter()
        .map(|n| (n.id.clone(), n.capabilities.gpu_count))
        .collect();

    let mut result = HashMap::new();

    for tenant in tenants {
        let has_gpu_budget = tenant.quota.gpu_hours_budget.is_some_and(|b| b > 0.0);
        let has_node_budget = tenant.quota.node_hours_budget.is_some_and(|b| b > 0.0);

        if !has_gpu_budget && !has_node_budget {
            continue;
        }

        let tenant_allocs: Vec<&Allocation> = allocations
            .iter()
            .filter(|a| {
                a.tenant == tenant.id
                    && a.started_at.is_some()
                    && a.started_at.unwrap() >= period_start
            })
            .collect();

        let mut fraction_used = 0.0_f64;

        if let Some(gpu_budget) = tenant.quota.gpu_hours_budget.filter(|b| *b > 0.0) {
            let gpu_hours: f64 = tenant_allocs
                .iter()
                .map(|a| allocation_gpu_hours(a, now, &node_gpu_counts))
                .sum();
            fraction_used = fraction_used.max(gpu_hours / gpu_budget);
        }

        if let Some(node_budget) = tenant.quota.node_hours_budget.filter(|b| *b > 0.0) {
            let node_hours: f64 = tenant_allocs
                .iter()
                .map(|a| allocation_node_hours(a, now))
                .sum();
            fraction_used = fraction_used.max(node_hours / node_budget);
        }

        result.insert(tenant.id.clone(), BudgetUtilization { fraction_used });
    }

    result
}

/// Compute per-tenant usage metrics (for REST/CLI queries).
pub fn compute_tenant_usage_metrics(
    tenant: &Tenant,
    allocations: &[Allocation],
    nodes: &[Node],
    period_start: DateTime<Utc>,
    now: DateTime<Utc>,
) -> TenantUsageMetrics {
    let node_gpu_counts: HashMap<NodeId, u32> = nodes
        .iter()
        .map(|n| (n.id.clone(), n.capabilities.gpu_count))
        .collect();

    let tenant_allocs: Vec<&Allocation> = allocations
        .iter()
        .filter(|a| {
            a.tenant == tenant.id && a.started_at.is_some() && a.started_at.unwrap() >= period_start
        })
        .collect();

    TenantUsageMetrics {
        gpu_hours_used: tenant_allocs
            .iter()
            .map(|a| allocation_gpu_hours(a, now, &node_gpu_counts))
            .sum(),
        node_hours_used: tenant_allocs
            .iter()
            .map(|a| allocation_node_hours(a, now))
            .sum(),
    }
}

/// Per-user usage across tenants (for CLI query).
#[derive(Debug, Clone, Default)]
pub struct UserTenantMetrics {
    pub gpu_hours_used: f64,
    pub node_hours_used: f64,
}

/// Compute per-user usage across tenants (for CLI query).
pub fn compute_user_usage_metrics(
    user: &str,
    allocations: &[Allocation],
    nodes: &[Node],
    period_start: DateTime<Utc>,
    now: DateTime<Utc>,
) -> HashMap<TenantId, UserTenantMetrics> {
    let node_gpu_counts: HashMap<NodeId, u32> = nodes
        .iter()
        .map(|n| (n.id.clone(), n.capabilities.gpu_count))
        .collect();

    let mut by_tenant: HashMap<TenantId, UserTenantMetrics> = HashMap::new();
    for alloc in allocations {
        if alloc.user == user
            && alloc.started_at.is_some()
            && alloc.started_at.unwrap() >= period_start
        {
            let entry = by_tenant.entry(alloc.tenant.clone()).or_default();
            entry.gpu_hours_used += allocation_gpu_hours(alloc, now, &node_gpu_counts);
            entry.node_hours_used += allocation_node_hours(alloc, now);
        }
    }
    by_tenant
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::{AllocationBuilder, TenantBuilder};

    #[test]
    fn tenant_with_no_running_allocs_has_zero_usage() {
        let tenant = TenantBuilder::new("t1").fair_share(0.3).build();
        let allocs = vec![AllocationBuilder::new()
            .tenant("t1")
            .state(AllocationState::Pending)
            .build()];

        let usage = compute_tenant_usage(&[tenant], &allocs, 100);
        assert!((usage["t1"].actual_usage - 0.0).abs() < f64::EPSILON);
        assert!((usage["t1"].target_share - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn tenant_usage_computed_correctly() {
        let tenant = TenantBuilder::new("t1").fair_share(0.5).build();
        let mut alloc = AllocationBuilder::new()
            .tenant("t1")
            .state(AllocationState::Running)
            .nodes(10)
            .build();
        alloc.assigned_nodes = (0..10).map(|i| format!("n{i}")).collect();

        let usage = compute_tenant_usage(&[tenant], &[alloc], 100);
        assert!((usage["t1"].actual_usage - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn multiple_tenants() {
        let t1 = TenantBuilder::new("t1").fair_share(0.6).build();
        let t2 = TenantBuilder::new("t2").fair_share(0.4).build();

        let mut a1 = AllocationBuilder::new()
            .tenant("t1")
            .state(AllocationState::Running)
            .build();
        a1.assigned_nodes = vec!["n1".into(), "n2".into()];

        let mut a2 = AllocationBuilder::new()
            .tenant("t2")
            .state(AllocationState::Running)
            .build();
        a2.assigned_nodes = vec!["n3".into()];

        let usage = compute_tenant_usage(&[t1, t2], &[a1, a2], 10);
        assert!((usage["t1"].actual_usage - 0.2).abs() < f64::EPSILON);
        assert!((usage["t2"].actual_usage - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn hard_quota_within_limits() {
        let tenant = TenantBuilder::new("t1")
            .max_nodes(10)
            .max_concurrent(5)
            .build();
        assert!(check_hard_quota(&tenant, 3, 5).is_none());
    }

    #[test]
    fn hard_quota_max_concurrent_exceeded() {
        let tenant = TenantBuilder::new("t1").max_concurrent(5).build();
        let result = check_hard_quota(&tenant, 5, 0);
        assert!(result.is_some());
        assert!(result.unwrap().contains("max_concurrent"));
    }

    #[test]
    fn hard_quota_max_nodes_exceeded() {
        let tenant = TenantBuilder::new("t1").max_nodes(10).build();
        let result = check_hard_quota(&tenant, 0, 10);
        assert!(result.is_some());
        assert!(result.unwrap().contains("max_nodes"));
    }

    #[test]
    fn hard_quota_no_limits() {
        let tenant = TenantBuilder::new("t1").max_nodes(1000).build();
        assert!(check_hard_quota(&tenant, 100, 500).is_none());
    }

    // ── Budget utilization tests ──

    use lattice_test_harness::fixtures::NodeBuilder;

    fn test_nodes() -> Vec<Node> {
        vec![
            NodeBuilder::new().id("n0").gpu_count(4).build(),
            NodeBuilder::new().id("n1").gpu_count(4).build(),
            NodeBuilder::new().id("n2").gpu_count(4).build(),
            NodeBuilder::new().id("n3").gpu_count(4).build(),
        ]
    }

    fn make_alloc(tenant: &str, nodes: Vec<&str>, hours_ago: i64, completed: bool) -> Allocation {
        let now = Utc::now();
        let mut alloc = AllocationBuilder::new()
            .tenant(tenant)
            .state(if completed {
                AllocationState::Completed
            } else {
                AllocationState::Running
            })
            .build();
        alloc.assigned_nodes = nodes.into_iter().map(String::from).collect();
        alloc.started_at = Some(now - chrono::Duration::hours(hours_ago));
        if completed {
            alloc.completed_at = Some(now);
        }
        alloc
    }

    #[test]
    fn budget_gpu_hours_from_completed() {
        let now = Utc::now();
        let tenant = TenantBuilder::new("t1").gpu_hours(1000.0).build();
        let alloc = make_alloc("t1", vec!["n0", "n1"], 10, true);
        let result = compute_budget_utilization(&[tenant], &[alloc], &test_nodes(), 90, now);
        // 10h × 2 nodes × 4 GPUs = 80 → 80/1000 = 0.08
        assert!((result["t1"].fraction_used - 0.08).abs() < 1e-6);
    }

    #[test]
    fn budget_node_hours_only() {
        let now = Utc::now();
        let tenant = TenantBuilder::new("t1").node_hours(100.0).build();
        let alloc = make_alloc("t1", vec!["n0", "n1"], 10, true);
        let result = compute_budget_utilization(&[tenant], &[alloc], &test_nodes(), 90, now);
        // 10h × 2 nodes = 20 node-hours → 20/100 = 0.2
        assert!((result["t1"].fraction_used - 0.2).abs() < 1e-6);
    }

    #[test]
    fn budget_worst_of_gpu_and_node() {
        let now = Utc::now();
        // GPU budget: loose. Node budget: tight.
        let mut tenant = TenantBuilder::new("t1")
            .gpu_hours(10000.0)
            .node_hours(25.0)
            .build();
        let alloc = make_alloc("t1", vec!["n0", "n1"], 10, true);
        let result = compute_budget_utilization(
            std::slice::from_ref(&tenant),
            std::slice::from_ref(&alloc),
            &test_nodes(),
            90,
            now,
        );
        // GPU: 80/10000 = 0.008, Node: 20/25 = 0.8 → worst = 0.8
        assert!((result["t1"].fraction_used - 0.8).abs() < 1e-6);

        // Now flip: GPU budget tight, node budget loose.
        tenant.quota.gpu_hours_budget = Some(100.0);
        tenant.quota.node_hours_budget = Some(10000.0);
        let result = compute_budget_utilization(&[tenant], &[alloc], &test_nodes(), 90, now);
        // GPU: 80/100 = 0.8, Node: 20/10000 = 0.002 → worst = 0.8
        assert!((result["t1"].fraction_used - 0.8).abs() < 1e-6);
    }

    #[test]
    fn budget_includes_running() {
        let now = Utc::now();
        let tenant = TenantBuilder::new("t1").node_hours(1000.0).build();
        let alloc = make_alloc("t1", vec!["n0"], 5, false); // still running
        let result = compute_budget_utilization(&[tenant], &[alloc], &test_nodes(), 90, now);
        // 5h × 1 node = 5 node-hours → 5/1000 = 0.005
        assert!((result["t1"].fraction_used - 0.005).abs() < 1e-6);
    }

    #[test]
    fn budget_excludes_old_allocations() {
        let now = Utc::now();
        let tenant = TenantBuilder::new("t1").node_hours(1000.0).build();
        let mut old = AllocationBuilder::new()
            .tenant("t1")
            .state(AllocationState::Completed)
            .build();
        old.assigned_nodes = vec!["n0".into()];
        old.started_at = Some(now - chrono::Duration::days(100));
        old.completed_at = Some(now - chrono::Duration::days(99));
        let result = compute_budget_utilization(&[tenant], &[old], &test_nodes(), 90, now);
        assert!(!result.contains_key("t1") || result["t1"].fraction_used.abs() < 1e-10);
    }

    #[test]
    fn budget_no_budget_skipped() {
        let now = Utc::now();
        let tenant = TenantBuilder::new("t1").build(); // no budgets
        let result = compute_budget_utilization(&[tenant], &[], &test_nodes(), 90, now);
        assert!(!result.contains_key("t1"));
    }

    #[test]
    fn budget_unknown_node_gpu_fallback() {
        let now = Utc::now();
        let tenant = TenantBuilder::new("t1").gpu_hours(1000.0).build();
        let mut alloc = AllocationBuilder::new()
            .tenant("t1")
            .state(AllocationState::Completed)
            .build();
        alloc.assigned_nodes = vec!["unknown".into()];
        alloc.started_at = Some(now - chrono::Duration::hours(10));
        alloc.completed_at = Some(now);
        let result = compute_budget_utilization(&[tenant], &[alloc], &[], 90, now);
        // 10h × 1 GPU (fallback) = 10 → 10/1000 = 0.01
        assert!((result["t1"].fraction_used - 0.01).abs() < 1e-6);
    }

    #[test]
    fn usage_metrics_both_tracked() {
        let now = Utc::now();
        let tenant = TenantBuilder::new("t1")
            .gpu_hours(1000.0)
            .node_hours(500.0)
            .build();
        let alloc = make_alloc("t1", vec!["n0", "n1"], 10, true);
        let period_start = now - chrono::Duration::days(90);
        let metrics =
            compute_tenant_usage_metrics(&tenant, &[alloc], &test_nodes(), period_start, now);
        assert!((metrics.gpu_hours_used - 80.0).abs() < 1e-6); // 10h × 2 × 4
        assert!((metrics.node_hours_used - 20.0).abs() < 1e-6); // 10h × 2
    }
}
