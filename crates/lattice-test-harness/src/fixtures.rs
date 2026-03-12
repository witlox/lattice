use std::collections::HashMap;

use chrono::Utc;
use uuid::Uuid;

use lattice_common::types::*;

// ─── AllocationBuilder ──────────────────────────────────────

pub struct AllocationBuilder {
    tenant: String,
    project: String,
    vcluster: String,
    user: String,
    state: AllocationState,
    lifecycle: LifecycleType,
    preemption_class: u8,
    node_count: NodeCount,
    entrypoint: String,
    sign_required: bool,
    scan_required: bool,
    isolation_level: Option<IsolationLevel>,
    tags: HashMap<String, String>,
    depends_on: Vec<Dependency>,
    dag_id: Option<String>,
    sensitive: bool,
}

impl AllocationBuilder {
    pub fn new() -> Self {
        Self {
            tenant: "test-tenant".into(),
            project: "test-project".into(),
            vcluster: "default".into(),
            user: "test-user".into(),
            state: AllocationState::Pending,
            lifecycle: LifecycleType::Bounded {
                walltime: chrono::Duration::hours(1),
            },
            preemption_class: 0,
            node_count: NodeCount::Exact(1),
            entrypoint: "/bin/true".into(),
            sign_required: false,
            scan_required: false,
            isolation_level: None,
            tags: HashMap::new(),
            depends_on: Vec::new(),
            dag_id: None,
            sensitive: false,
        }
    }

    pub fn tenant(mut self, t: &str) -> Self {
        self.tenant = t.into();
        self
    }

    pub fn project(mut self, p: &str) -> Self {
        self.project = p.into();
        self
    }

    pub fn vcluster(mut self, v: &str) -> Self {
        self.vcluster = v.into();
        self
    }

    pub fn user(mut self, u: &str) -> Self {
        self.user = u.into();
        self
    }

    pub fn state(mut self, s: AllocationState) -> Self {
        self.state = s;
        self
    }

    pub fn nodes(mut self, n: u32) -> Self {
        self.node_count = NodeCount::Exact(n);
        self
    }

    pub fn node_range(mut self, min: u32, max: u32) -> Self {
        self.node_count = NodeCount::Range { min, max };
        self
    }

    pub fn lifecycle_bounded(mut self, walltime_hours: u64) -> Self {
        self.lifecycle = LifecycleType::Bounded {
            walltime: chrono::Duration::hours(walltime_hours as i64),
        };
        self
    }

    pub fn lifecycle_unbounded(mut self) -> Self {
        self.lifecycle = LifecycleType::Unbounded;
        self
    }

    pub fn preemption_class(mut self, class: u8) -> Self {
        self.preemption_class = class;
        self
    }

    pub fn entrypoint(mut self, e: &str) -> Self {
        self.entrypoint = e.into();
        self
    }

    pub fn sensitive(mut self) -> Self {
        self.sign_required = true;
        self.scan_required = true;
        self.sensitive = true;
        self.isolation_level = Some(IsolationLevel::Strict);
        self.tags
            .insert("workload_class".into(), "sensitive".into());
        self
    }

    pub fn depends_on(mut self, ref_id: &str, condition: DependencyCondition) -> Self {
        self.depends_on.push(Dependency {
            ref_id: ref_id.into(),
            condition,
        });
        self
    }

    pub fn dag_id(mut self, id: &str) -> Self {
        self.dag_id = Some(id.into());
        self
    }

    pub fn tag(mut self, key: &str, value: &str) -> Self {
        self.tags.insert(key.into(), value.into());
        self
    }

    pub fn build(self) -> Allocation {
        Allocation {
            id: Uuid::new_v4(),
            tenant: self.tenant,
            project: self.project,
            vcluster: self.vcluster,
            user: self.user,
            tags: self.tags,
            allocation_type: AllocationType::Single,
            environment: Environment {
                uenv: Some("prgenv-gnu/24.11:v1".into()),
                view: Some("default".into()),
                image: None,
                tools_uenv: None,
                sign_required: self.sign_required,
                scan_required: self.scan_required,
                approved_bases_only: self.sign_required,
            },
            entrypoint: self.entrypoint,
            resources: ResourceRequest {
                nodes: self.node_count,
                constraints: ResourceConstraints::default(),
            },
            lifecycle: Lifecycle {
                lifecycle_type: self.lifecycle,
                preemption_class: self.preemption_class,
            },
            requeue_policy: RequeuePolicy::OnNodeFailure,
            max_requeue: 3,
            data: DataRequirements::default(),
            connectivity: Connectivity::default(),
            depends_on: self.depends_on,
            checkpoint: CheckpointStrategy::Auto,
            telemetry_mode: TelemetryMode::Prod,
            state: self.state,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            assigned_nodes: Vec::new(),
            dag_id: self.dag_id,
            exit_code: None,
            message: None,
            requeue_count: 0,
            preempted_count: 0,
            resume_from_checkpoint: false,
            sensitive: self.sensitive,
        }
    }
}

impl Default for AllocationBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ─── NodeBuilder ────────────────────────────────────────────

pub struct NodeBuilder {
    id: Option<String>,
    group: GroupId,
    state: NodeState,
    gpu_type: Option<String>,
    gpu_count: u32,
    cpu_cores: u32,
    memory_gb: u64,
    features: Vec<String>,
    owner: Option<NodeOwnership>,
    conformance_fingerprint: Option<String>,
    memory_topology: Option<MemoryTopology>,
}

impl NodeBuilder {
    pub fn new() -> Self {
        Self {
            id: None,
            group: 0,
            state: NodeState::Ready,
            gpu_type: Some("GH200".into()),
            gpu_count: 4,
            cpu_cores: 72,
            memory_gb: 512,
            features: Vec::new(),
            owner: None,
            conformance_fingerprint: None,
            memory_topology: None,
        }
    }

    pub fn id(mut self, id: &str) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn group(mut self, g: GroupId) -> Self {
        self.group = g;
        self
    }

    pub fn state(mut self, s: NodeState) -> Self {
        self.state = s;
        self
    }

    pub fn gpu_type(mut self, t: &str) -> Self {
        self.gpu_type = Some(t.into());
        self
    }

    pub fn gpu_count(mut self, c: u32) -> Self {
        self.gpu_count = c;
        self
    }

    pub fn cpu_cores(mut self, c: u32) -> Self {
        self.cpu_cores = c;
        self
    }

    pub fn memory_gb(mut self, m: u64) -> Self {
        self.memory_gb = m;
        self
    }

    pub fn feature(mut self, f: &str) -> Self {
        self.features.push(f.into());
        self
    }

    pub fn owner(mut self, ownership: NodeOwnership) -> Self {
        self.owner = Some(ownership);
        self
    }

    pub fn conformance(mut self, fingerprint: &str) -> Self {
        self.conformance_fingerprint = Some(fingerprint.into());
        self
    }

    pub fn memory_topology(mut self, topo: MemoryTopology) -> Self {
        self.memory_topology = Some(topo);
        self
    }

    pub fn build(self) -> Node {
        let id = self
            .id
            .unwrap_or_else(|| format!("x1000c0s0b0n{}", rand_node_suffix()));
        Node {
            id,
            group: self.group,
            capabilities: NodeCapabilities {
                gpu_type: self.gpu_type,
                gpu_count: self.gpu_count,
                cpu_cores: self.cpu_cores,
                memory_gb: self.memory_gb,
                features: self.features,
                gpu_topology: None,
                memory_topology: self.memory_topology,
            },
            state: self.state,
            owner: self.owner,
            conformance_fingerprint: self.conformance_fingerprint,
            last_heartbeat: Some(Utc::now()),
            owner_version: 0,
        }
    }
}

impl Default for NodeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn rand_node_suffix() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

// ─── TenantBuilder ──────────────────────────────────────────

pub struct TenantBuilder {
    id: String,
    name: String,
    max_nodes: u32,
    fair_share_target: f64,
    gpu_hours_budget: Option<f64>,
    max_concurrent_allocations: Option<u32>,
    isolation_level: IsolationLevel,
}

impl TenantBuilder {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            name: id.into(),
            max_nodes: 100,
            fair_share_target: 0.1,
            gpu_hours_budget: None,
            max_concurrent_allocations: None,
            isolation_level: IsolationLevel::Standard,
        }
    }

    pub fn name(mut self, n: &str) -> Self {
        self.name = n.into();
        self
    }

    pub fn max_nodes(mut self, n: u32) -> Self {
        self.max_nodes = n;
        self
    }

    pub fn fair_share(mut self, f: f64) -> Self {
        self.fair_share_target = f;
        self
    }

    pub fn gpu_hours(mut self, h: f64) -> Self {
        self.gpu_hours_budget = Some(h);
        self
    }

    pub fn max_concurrent(mut self, n: u32) -> Self {
        self.max_concurrent_allocations = Some(n);
        self
    }

    pub fn strict_isolation(mut self) -> Self {
        self.isolation_level = IsolationLevel::Strict;
        self
    }

    pub fn build(self) -> Tenant {
        Tenant {
            id: self.id,
            name: self.name,
            quota: TenantQuota {
                max_nodes: self.max_nodes,
                fair_share_target: self.fair_share_target,
                gpu_hours_budget: self.gpu_hours_budget,
                max_concurrent_allocations: self.max_concurrent_allocations,
                burst_allowance: None,
            },
            isolation_level: self.isolation_level,
        }
    }
}

// ─── VClusterBuilder ────────────────────────────────────────

pub struct VClusterBuilder {
    id: String,
    name: String,
    tenant: String,
    scheduler_type: SchedulerType,
    cost_weights: CostWeights,
    allow_borrowing: bool,
    allow_lending: bool,
}

impl VClusterBuilder {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            name: id.into(),
            tenant: "test-tenant".into(),
            scheduler_type: SchedulerType::HpcBackfill,
            cost_weights: CostWeights::default(),
            allow_borrowing: true,
            allow_lending: true,
        }
    }

    pub fn tenant(mut self, t: &str) -> Self {
        self.tenant = t.into();
        self
    }

    pub fn scheduler(mut self, s: SchedulerType) -> Self {
        self.scheduler_type = s;
        self
    }

    pub fn cost_weights(mut self, w: CostWeights) -> Self {
        self.cost_weights = w;
        self
    }

    pub fn no_borrowing(mut self) -> Self {
        self.allow_borrowing = false;
        self
    }

    pub fn no_lending(mut self) -> Self {
        self.allow_lending = false;
        self
    }

    pub fn build(self) -> VCluster {
        VCluster {
            id: self.id,
            name: self.name,
            tenant: self.tenant,
            scheduler_type: self.scheduler_type,
            cost_weights: self.cost_weights,
            dedicated_nodes: Vec::new(),
            allow_borrowing: self.allow_borrowing,
            allow_lending: self.allow_lending,
        }
    }
}

// ─── Batch Helpers ──────────────────────────────────────────

/// Create a batch of nodes in the same dragonfly group.
pub fn create_node_batch(count: usize, group: GroupId) -> Vec<Node> {
    (0..count)
        .map(|i| {
            NodeBuilder::new()
                .id(&format!("x1000c0s{group}b0n{i}"))
                .group(group)
                .build()
        })
        .collect()
}

/// Create a test topology with multiple groups.
pub fn create_test_topology(groups: usize, nodes_per_group: usize) -> TopologyModel {
    let mut topo_groups = Vec::with_capacity(groups);
    for g in 0..groups {
        let group_id = g as GroupId;
        let nodes: Vec<NodeId> = (0..nodes_per_group)
            .map(|n| format!("x1000c0s{g}b0n{n}"))
            .collect();
        let adjacent: Vec<GroupId> = (0..groups)
            .filter(|&other| other != g)
            .map(|other| other as GroupId)
            .collect();
        topo_groups.push(TopologyGroup {
            id: group_id,
            nodes,
            adjacent_groups: adjacent,
        });
    }
    TopologyModel {
        groups: topo_groups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_builder_defaults() {
        let alloc = AllocationBuilder::new().build();
        assert_eq!(alloc.state, AllocationState::Pending);
        assert_eq!(alloc.tenant, "test-tenant");
        assert!(alloc.assigned_nodes.is_empty());
    }

    #[test]
    fn allocation_builder_sensitive() {
        let alloc = AllocationBuilder::new().sensitive().build();
        assert!(alloc.environment.sign_required);
        assert!(alloc.environment.scan_required);
        assert!(alloc.environment.approved_bases_only);
        assert_eq!(alloc.tags.get("workload_class").unwrap(), "sensitive");
    }

    #[test]
    fn node_builder_defaults() {
        let node = NodeBuilder::new().build();
        assert_eq!(node.state, NodeState::Ready);
        assert_eq!(node.capabilities.gpu_count, 4);
        assert!(node.owner.is_none());
    }

    #[test]
    fn create_node_batch_creates_correct_count() {
        let nodes = create_node_batch(10, 0);
        assert_eq!(nodes.len(), 10);
        for node in &nodes {
            assert_eq!(node.group, 0);
            assert_eq!(node.state, NodeState::Ready);
        }
    }

    #[test]
    fn create_test_topology_structure() {
        let topo = create_test_topology(3, 8);
        assert_eq!(topo.groups.len(), 3);
        for group in &topo.groups {
            assert_eq!(group.nodes.len(), 8);
            assert_eq!(group.adjacent_groups.len(), 2);
        }
    }

    #[test]
    fn tenant_builder_defaults() {
        let tenant = TenantBuilder::new("physics").build();
        assert_eq!(tenant.id, "physics");
        assert_eq!(tenant.quota.max_nodes, 100);
    }

    #[test]
    fn vcluster_builder_defaults() {
        let vc = VClusterBuilder::new("hpc-batch").build();
        assert_eq!(vc.scheduler_type, SchedulerType::HpcBackfill);
        assert!(vc.allow_borrowing);
        assert!(vc.allow_lending);
    }
}
