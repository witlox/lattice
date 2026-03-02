//! Soft quota checking for the cost function.
//!
//! Hard quota enforcement is done by the quorum (lattice-quorum).
//! This module provides soft quota signals that feed into the
//! fair-share factor (f₃) of the cost function.

use std::collections::HashMap;

use lattice_common::types::*;

use crate::cost::TenantUsage;

/// Compute per-tenant usage from current allocations.
pub fn compute_tenant_usage(
    tenants: &[Tenant],
    allocations: &[Allocation],
    total_nodes: u32,
) -> HashMap<TenantId, TenantUsage> {
    let mut usage = HashMap::new();

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
}
