use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use lattice_common::error::LatticeError;
use lattice_common::traits::*;
use lattice_common::types::*;

// ─── MockCall ───────────────────────────────────────────────

/// Records a method call for assertion.
#[derive(Debug, Clone)]
pub struct MockCall {
    pub method: String,
    pub args: Vec<String>,
}

// ─── MockStorageService ─────────────────────────────────────

#[derive(Debug, Default)]
pub struct MockStorageService {
    pub readiness_responses: Arc<Mutex<HashMap<String, f64>>>,
    pub calls: Arc<Mutex<Vec<MockCall>>>,
}

impl MockStorageService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_readiness(self, source: &str, readiness: f64) -> Self {
        self.readiness_responses
            .lock()
            .unwrap()
            .insert(source.into(), readiness);
        self
    }

    pub fn call_count(&self, method: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.method == method)
            .count()
    }
}

#[async_trait]
impl StorageService for MockStorageService {
    async fn data_readiness(&self, source: &str) -> Result<f64, LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "data_readiness".into(),
            args: vec![source.into()],
        });
        Ok(*self
            .readiness_responses
            .lock()
            .unwrap()
            .get(source)
            .unwrap_or(&1.0))
    }

    async fn stage_data(&self, source: &str, target: &str) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "stage_data".into(),
            args: vec![source.into(), target.into()],
        });
        Ok(())
    }

    async fn set_qos(&self, path: &str, floor_gbps: f64) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "set_qos".into(),
            args: vec![path.into(), floor_gbps.to_string()],
        });
        Ok(())
    }

    async fn wipe_data(&self, path: &str) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "wipe_data".into(),
            args: vec![path.into()],
        });
        Ok(())
    }
}

// ─── MockInfrastructureService ──────────────────────────────

#[derive(Debug, Default)]
pub struct MockInfrastructureService {
    pub calls: Arc<Mutex<Vec<MockCall>>>,
}

impl MockInfrastructureService {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl InfrastructureService for MockInfrastructureService {
    async fn boot_node(&self, node_id: &NodeId, image: &str) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "boot_node".into(),
            args: vec![node_id.clone(), image.into()],
        });
        Ok(())
    }

    async fn wipe_node(&self, node_id: &NodeId) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "wipe_node".into(),
            args: vec![node_id.clone()],
        });
        Ok(())
    }

    async fn query_node_health(&self, node_id: &NodeId) -> Result<NodeHealthReport, LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "query_node_health".into(),
            args: vec![node_id.clone()],
        });
        Ok(NodeHealthReport {
            healthy: true,
            issues: Vec::new(),
        })
    }
}

// ─── MockAccountingService ──────────────────────────────────

#[derive(Debug, Default)]
pub struct MockAccountingService {
    pub budgets: Arc<Mutex<HashMap<String, f64>>>,
    pub calls: Arc<Mutex<Vec<MockCall>>>,
}

impl MockAccountingService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_budget(self, tenant: &str, budget: f64) -> Self {
        self.budgets.lock().unwrap().insert(tenant.into(), budget);
        self
    }
}

#[async_trait]
impl AccountingService for MockAccountingService {
    async fn report_start(&self, allocation: &Allocation) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "report_start".into(),
            args: vec![allocation.id.to_string()],
        });
        Ok(())
    }

    async fn report_completion(&self, allocation: &Allocation) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "report_completion".into(),
            args: vec![allocation.id.to_string()],
        });
        Ok(())
    }

    async fn remaining_budget(&self, tenant: &TenantId) -> Result<Option<f64>, LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "remaining_budget".into(),
            args: vec![tenant.clone()],
        });
        Ok(self.budgets.lock().unwrap().get(tenant).copied())
    }
}

// ─── MockNodeRegistry ───────────────────────────────────────

#[derive(Debug, Default)]
pub struct MockNodeRegistry {
    pub nodes: Arc<Mutex<HashMap<NodeId, Node>>>,
    pub calls: Arc<Mutex<Vec<MockCall>>>,
}

impl MockNodeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_nodes(self, nodes: Vec<Node>) -> Self {
        let mut map = self.nodes.lock().unwrap();
        for node in nodes {
            map.insert(node.id.clone(), node);
        }
        drop(map);
        self
    }
}

#[async_trait]
impl NodeRegistry for MockNodeRegistry {
    async fn get_node(&self, id: &NodeId) -> Result<Node, LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "get_node".into(),
            args: vec![id.clone()],
        });
        self.nodes
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| LatticeError::NodeNotFound(id.clone()))
    }

    async fn list_nodes(&self, filter: &NodeFilter) -> Result<Vec<Node>, LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "list_nodes".into(),
            args: vec![format!("{filter:?}")],
        });
        let nodes = self.nodes.lock().unwrap();
        let result: Vec<Node> = nodes
            .values()
            .filter(|n| {
                if let Some(ref state) = filter.state {
                    if n.state != *state {
                        return false;
                    }
                }
                if let Some(group) = filter.group {
                    if n.group != group {
                        return false;
                    }
                }
                if let Some(ref tenant) = filter.tenant {
                    if let Some(ref owner) = n.owner {
                        if owner.tenant != *tenant {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();
        Ok(result)
    }

    async fn update_node_state(&self, id: &NodeId, state: NodeState) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "update_node_state".into(),
            args: vec![id.clone(), format!("{state:?}")],
        });
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get_mut(id) {
            Some(node) => {
                node.state = state;
                Ok(())
            }
            None => Err(LatticeError::NodeNotFound(id.clone())),
        }
    }

    async fn claim_node(&self, id: &NodeId, ownership: NodeOwnership) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "claim_node".into(),
            args: vec![id.clone(), format!("{ownership:?}")],
        });
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get_mut(id) {
            Some(node) => {
                if node.owner.is_some() {
                    return Err(LatticeError::OwnershipConflict {
                        node: id.clone(),
                        owner: format!("{:?}", node.owner),
                    });
                }
                node.owner = Some(ownership);
                Ok(())
            }
            None => Err(LatticeError::NodeNotFound(id.clone())),
        }
    }

    async fn release_node(&self, id: &NodeId) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "release_node".into(),
            args: vec![id.clone()],
        });
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get_mut(id) {
            Some(node) => {
                node.owner = None;
                Ok(())
            }
            None => Err(LatticeError::NodeNotFound(id.clone())),
        }
    }
}

// ─── MockAllocationStore ────────────────────────────────────

#[derive(Debug, Default)]
pub struct MockAllocationStore {
    pub allocations: Arc<Mutex<HashMap<AllocId, Allocation>>>,
    pub calls: Arc<Mutex<Vec<MockCall>>>,
}

impl MockAllocationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_allocations(self, allocs: Vec<Allocation>) -> Self {
        let mut map = self.allocations.lock().unwrap();
        for alloc in allocs {
            map.insert(alloc.id, alloc);
        }
        drop(map);
        self
    }
}

#[async_trait]
impl AllocationStore for MockAllocationStore {
    async fn insert(&self, allocation: Allocation) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "insert".into(),
            args: vec![allocation.id.to_string()],
        });
        self.allocations
            .lock()
            .unwrap()
            .insert(allocation.id, allocation);
        Ok(())
    }

    async fn get(&self, id: &AllocId) -> Result<Allocation, LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "get".into(),
            args: vec![id.to_string()],
        });
        self.allocations
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| LatticeError::AllocationNotFound(id.to_string()))
    }

    async fn update_state(&self, id: &AllocId, state: AllocationState) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "update_state".into(),
            args: vec![id.to_string(), format!("{state:?}")],
        });
        let mut allocs = self.allocations.lock().unwrap();
        match allocs.get_mut(id) {
            Some(alloc) => {
                alloc.state = state;
                Ok(())
            }
            None => Err(LatticeError::AllocationNotFound(id.to_string())),
        }
    }

    async fn list(&self, filter: &AllocationFilter) -> Result<Vec<Allocation>, LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "list".into(),
            args: vec![format!("{filter:?}")],
        });
        let allocs = self.allocations.lock().unwrap();
        let result: Vec<Allocation> = allocs
            .values()
            .filter(|a| {
                if let Some(ref user) = filter.user {
                    if a.user != *user {
                        return false;
                    }
                }
                if let Some(ref tenant) = filter.tenant {
                    if a.tenant != *tenant {
                        return false;
                    }
                }
                if let Some(ref state) = filter.state {
                    if a.state != *state {
                        return false;
                    }
                }
                if let Some(ref vcluster) = filter.vcluster {
                    if a.vcluster != *vcluster {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();
        Ok(result)
    }

    async fn count_running(&self, tenant: &TenantId) -> Result<u32, LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "count_running".into(),
            args: vec![tenant.clone()],
        });
        let allocs = self.allocations.lock().unwrap();
        let count = allocs
            .values()
            .filter(|a| a.tenant == *tenant && a.state == AllocationState::Running)
            .count() as u32;
        Ok(count)
    }
}

// ─── MockAuditLog ───────────────────────────────────────────

#[derive(Debug, Default)]
pub struct MockAuditLog {
    pub entries: Arc<Mutex<Vec<AuditEntry>>>,
    pub calls: Arc<Mutex<Vec<MockCall>>>,
}

impl MockAuditLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    pub fn entries_for_action(&self, action: &AuditAction) -> Vec<AuditEntry> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.action == *action)
            .cloned()
            .collect()
    }
}

#[async_trait]
impl AuditLog for MockAuditLog {
    async fn record(&self, entry: AuditEntry) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "record".into(),
            args: vec![format!("{:?}", entry.action)],
        });
        self.entries.lock().unwrap().push(entry);
        Ok(())
    }

    async fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditEntry>, LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "query".into(),
            args: vec![format!("{filter:?}")],
        });
        let entries = self.entries.lock().unwrap();
        let result: Vec<AuditEntry> = entries
            .iter()
            .filter(|e| {
                if let Some(ref user) = filter.user {
                    if e.user != *user {
                        return false;
                    }
                }
                if let Some(ref since) = filter.since {
                    if e.timestamp < *since {
                        return false;
                    }
                }
                if let Some(ref until) = filter.until {
                    if e.timestamp > *until {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();
        Ok(result)
    }
}

// ─── MockCheckpointBroker ───────────────────────────────────

#[derive(Debug, Default)]
pub struct MockCheckpointBroker {
    pub should_checkpoint: Arc<Mutex<bool>>,
    pub calls: Arc<Mutex<Vec<MockCall>>>,
}

impl MockCheckpointBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_should_checkpoint(self, value: bool) -> Self {
        *self.should_checkpoint.lock().unwrap() = value;
        self
    }
}

#[async_trait]
impl CheckpointBroker for MockCheckpointBroker {
    async fn should_checkpoint(&self, _allocation: &Allocation) -> Result<bool, LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "should_checkpoint".into(),
            args: vec![],
        });
        Ok(*self.should_checkpoint.lock().unwrap())
    }

    async fn initiate_checkpoint(&self, id: &AllocId) -> Result<(), LatticeError> {
        self.calls.lock().unwrap().push(MockCall {
            method: "initiate_checkpoint".into(),
            args: vec![id.to_string()],
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use chrono::Utc;
    use uuid::Uuid;

    #[tokio::test]
    async fn mock_node_registry_get_and_list() {
        let nodes = create_node_batch(5, 0);
        let registry = MockNodeRegistry::new().with_nodes(nodes);

        let node = registry.get_node(&"x1000c0s0b0n0".into()).await.unwrap();
        assert_eq!(node.id, "x1000c0s0b0n0");

        let all = registry.list_nodes(&NodeFilter::default()).await.unwrap();
        assert_eq!(all.len(), 5);
    }

    #[tokio::test]
    async fn mock_node_registry_claim_conflict() {
        let nodes = create_node_batch(1, 0);
        let registry = MockNodeRegistry::new().with_nodes(nodes);

        let ownership = NodeOwnership {
            tenant: "t1".into(),
            vcluster: "vc1".into(),
            allocation: Uuid::new_v4(),
            claimed_by: Some("user-a".into()),
            is_borrowed: false,
        };
        registry
            .claim_node(&"x1000c0s0b0n0".into(), ownership)
            .await
            .unwrap();

        let ownership2 = NodeOwnership {
            tenant: "t2".into(),
            vcluster: "vc2".into(),
            allocation: Uuid::new_v4(),
            claimed_by: Some("user-b".into()),
            is_borrowed: false,
        };
        let result = registry
            .claim_node(&"x1000c0s0b0n0".into(), ownership2)
            .await;
        assert!(matches!(
            result,
            Err(LatticeError::OwnershipConflict { .. })
        ));
    }

    #[tokio::test]
    async fn mock_allocation_store_insert_and_get() {
        let store = MockAllocationStore::new();
        let alloc = AllocationBuilder::new().build();
        let id = alloc.id;

        store.insert(alloc).await.unwrap();
        let retrieved = store.get(&id).await.unwrap();
        assert_eq!(retrieved.id, id);
    }

    #[tokio::test]
    async fn mock_audit_log_records_entries() {
        let log = MockAuditLog::new();
        let entry = AuditEntry {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            user: "test-user".into(),
            action: AuditAction::NodeClaim,
            details: serde_json::json!({"node": "x1000c0s0b0n0"}),
            previous_hash: String::new(),
            signature: String::new(),
        };
        log.record(entry).await.unwrap();

        assert_eq!(log.entry_count(), 1);
        let claims = log.entries_for_action(&AuditAction::NodeClaim);
        assert_eq!(claims.len(), 1);
    }

    #[tokio::test]
    async fn mock_storage_service_readiness() {
        let storage = MockStorageService::new().with_readiness("s3://data/input", 0.75);
        let readiness = storage.data_readiness("s3://data/input").await.unwrap();
        assert!((readiness - 0.75).abs() < f64::EPSILON);
        assert_eq!(storage.call_count("data_readiness"), 1);
    }
}
