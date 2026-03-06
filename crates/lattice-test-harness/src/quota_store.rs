//! A quota-enforcing wrapper around any `AllocationStore`.
//!
//! When the Raft quorum is absent (standalone mode, tests, etc.), hard quota
//! enforcement must still happen.  This wrapper checks tenant quotas before
//! delegating `insert()` to the inner store.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use lattice_common::error::LatticeError;
use lattice_common::traits::{AllocationFilter, AllocationStore};
use lattice_common::types::*;

/// Wraps an [`AllocationStore`] and enforces hard tenant quotas on insert.
///
/// Quotas checked:
/// - `max_nodes`: total assigned nodes across non-terminal allocations
/// - `max_concurrent_allocations`: count of non-terminal allocations
pub struct QuotaEnforcingAllocationStore {
    inner: Arc<dyn AllocationStore>,
    tenants: Arc<Mutex<HashMap<TenantId, Tenant>>>,
}

impl QuotaEnforcingAllocationStore {
    pub fn new(inner: Arc<dyn AllocationStore>, tenants: Vec<Tenant>) -> Self {
        let map: HashMap<TenantId, Tenant> =
            tenants.into_iter().map(|t| (t.id.clone(), t)).collect();
        Self {
            inner,
            tenants: Arc::new(Mutex::new(map)),
        }
    }

    /// Update or add a tenant's quota information.
    pub fn upsert_tenant(&self, tenant: Tenant) {
        self.tenants
            .lock()
            .unwrap()
            .insert(tenant.id.clone(), tenant);
    }

    /// Remove a tenant.
    pub fn remove_tenant(&self, id: &TenantId) {
        self.tenants.lock().unwrap().remove(id);
    }
}

impl std::fmt::Debug for QuotaEnforcingAllocationStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuotaEnforcingAllocationStore").finish()
    }
}

#[async_trait]
impl AllocationStore for QuotaEnforcingAllocationStore {
    async fn insert(&self, allocation: Allocation) -> Result<(), LatticeError> {
        // Extract quota info while holding the lock briefly
        let quota_info = {
            let tenants = self.tenants.lock().unwrap();
            tenants
                .get(&allocation.tenant)
                .map(|t| (t.quota.max_nodes, t.quota.max_concurrent_allocations))
        };

        if let Some((max_nodes, max_concurrent)) = quota_info {
            // Count existing non-terminal allocations for this tenant
            let existing = self
                .inner
                .list(&AllocationFilter {
                    tenant: Some(allocation.tenant.clone()),
                    ..Default::default()
                })
                .await?;

            let non_terminal: Vec<&Allocation> =
                existing.iter().filter(|a| !a.state.is_terminal()).collect();

            // Check max_concurrent_allocations
            if let Some(max_conc) = max_concurrent {
                if non_terminal.len() as u32 >= max_conc {
                    return Err(LatticeError::QuotaExceeded {
                        tenant: allocation.tenant.clone(),
                        detail: format!(
                            "max_concurrent_allocations: {} >= {}",
                            non_terminal.len(),
                            max_conc
                        ),
                    });
                }
            }

            // Check max_nodes
            let nodes_in_use: u32 = non_terminal
                .iter()
                .map(|a| match a.resources.nodes {
                    NodeCount::Exact(n) => n,
                    NodeCount::Range { max, .. } => max,
                })
                .sum();

            let requested_nodes = match allocation.resources.nodes {
                NodeCount::Exact(n) => n,
                NodeCount::Range { max, .. } => max,
            };

            if nodes_in_use + requested_nodes > max_nodes {
                return Err(LatticeError::QuotaExceeded {
                    tenant: allocation.tenant.clone(),
                    detail: format!(
                        "max_nodes: {} in use + {} requested > {} max",
                        nodes_in_use, requested_nodes, max_nodes
                    ),
                });
            }
        }

        self.inner.insert(allocation).await
    }

    async fn get(&self, id: &AllocId) -> Result<Allocation, LatticeError> {
        self.inner.get(id).await
    }

    async fn update_state(
        &self,
        id: &AllocId,
        state: AllocationState,
    ) -> Result<(), LatticeError> {
        self.inner.update_state(id, state).await
    }

    async fn list(&self, filter: &AllocationFilter) -> Result<Vec<Allocation>, LatticeError> {
        self.inner.list(filter).await
    }

    async fn count_running(&self, tenant: &TenantId) -> Result<u32, LatticeError> {
        self.inner.count_running(tenant).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{AllocationBuilder, TenantBuilder};
    use crate::mocks::MockAllocationStore;

    fn make_store(tenants: Vec<Tenant>) -> QuotaEnforcingAllocationStore {
        QuotaEnforcingAllocationStore::new(Arc::new(MockAllocationStore::new()), tenants)
    }

    #[tokio::test]
    async fn insert_within_quota_succeeds() {
        let tenant = TenantBuilder::new("t1").max_nodes(10).build();
        let store = make_store(vec![tenant]);

        let alloc = AllocationBuilder::new().tenant("t1").nodes(5).build();
        assert!(store.insert(alloc).await.is_ok());
    }

    #[tokio::test]
    async fn insert_exceeding_max_nodes_fails() {
        let tenant = TenantBuilder::new("t1").max_nodes(4).build();
        let store = make_store(vec![tenant]);

        let a1 = AllocationBuilder::new().tenant("t1").nodes(3).build();
        store.insert(a1).await.unwrap();

        // This should fail: 3 + 3 > 4
        let a2 = AllocationBuilder::new().tenant("t1").nodes(3).build();
        let result = store.insert(a2).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("max_nodes"));
    }

    #[tokio::test]
    async fn insert_exceeding_max_concurrent_fails() {
        let tenant = TenantBuilder::new("t1").max_concurrent(2).build();
        let store = make_store(vec![tenant]);

        let a1 = AllocationBuilder::new().tenant("t1").nodes(1).build();
        let a2 = AllocationBuilder::new().tenant("t1").nodes(1).build();
        store.insert(a1).await.unwrap();
        store.insert(a2).await.unwrap();

        // Third allocation should fail
        let a3 = AllocationBuilder::new().tenant("t1").nodes(1).build();
        let result = store.insert(a3).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("max_concurrent"));
    }

    #[tokio::test]
    async fn terminal_allocations_not_counted() {
        let tenant = TenantBuilder::new("t1").max_nodes(4).max_concurrent(2).build();
        let store = make_store(vec![tenant]);

        // Insert two allocations
        let a1 = AllocationBuilder::new().tenant("t1").nodes(3).build();
        let a1_id = a1.id;
        store.insert(a1).await.unwrap();

        // Cancel the first one
        store
            .update_state(&a1_id, AllocationState::Cancelled)
            .await
            .unwrap();

        // Now inserting another should succeed (cancelled doesn't count)
        let a2 = AllocationBuilder::new().tenant("t1").nodes(3).build();
        assert!(store.insert(a2).await.is_ok());
    }

    #[tokio::test]
    async fn unknown_tenant_bypasses_quota() {
        let store = make_store(vec![]);

        // Tenant not in the store → no quota check → insert succeeds
        let alloc = AllocationBuilder::new().tenant("unknown").nodes(100).build();
        assert!(store.insert(alloc).await.is_ok());
    }

    #[tokio::test]
    async fn range_nodes_checks_max() {
        let tenant = TenantBuilder::new("t1").max_nodes(5).build();
        let store = make_store(vec![tenant]);

        // Range { min: 2, max: 6 } should check against max (6) which exceeds quota (5)
        let alloc = AllocationBuilder::new()
            .tenant("t1")
            .node_range(2, 6)
            .build();
        let result = store.insert(alloc).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn upsert_tenant_updates_quota() {
        let tenant = TenantBuilder::new("t1").max_nodes(2).build();
        let store = make_store(vec![tenant]);

        let a1 = AllocationBuilder::new().tenant("t1").nodes(2).build();
        store.insert(a1).await.unwrap();

        // Should fail with original quota
        let a2 = AllocationBuilder::new().tenant("t1").nodes(1).build();
        assert!(store.insert(a2).await.is_err());

        // Update quota to allow more
        let updated_tenant = TenantBuilder::new("t1").max_nodes(10).build();
        store.upsert_tenant(updated_tenant);

        // Now should succeed
        let a3 = AllocationBuilder::new().tenant("t1").nodes(1).build();
        assert!(store.insert(a3).await.is_ok());
    }
}
