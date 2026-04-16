// Acceptance tests prioritize readability over clippy perfection
#![allow(
    clippy::single_match,
    clippy::match_like_matches_macro,
    clippy::get_unwrap,
    clippy::bool_comparison,
    clippy::overly_complex_bool_expr,
    clippy::unnecessary_get_then_check,
    clippy::type_complexity,
    clippy::absurd_extreme_comparisons,
    clippy::redundant_pattern_matching,
    clippy::async_yields_async,
    unused_comparisons
)]

mod steps;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use tokio::sync::RwLock;

use cucumber::World;
use uuid::Uuid;

use lattice_common::error::LatticeError;
use lattice_common::types::*;
use lattice_test_harness::mocks::*;

use hpc_identity::WorkloadIdentity;
use hpc_node::{CgroupHandle, CgroupManager, CgroupMetrics};
use lattice_api::events::{AllocationEvent, EventBus};
use lattice_node_agent::cgroup::StubCgroupManager;
use lattice_node_agent::identity::LatticeRotator;

/// Debug wrapper for `LatticeRotator` (which does not derive Debug).
pub struct IdentityRotatorWrapper(LatticeRotator);

impl std::fmt::Debug for IdentityRotatorWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LatticeRotator").finish_non_exhaustive()
    }
}

impl std::ops::Deref for IdentityRotatorWrapper {
    type Target = LatticeRotator;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<LatticeRotator> for IdentityRotatorWrapper {
    fn from(r: LatticeRotator) -> Self {
        Self(r)
    }
}

/// Debug wrapper for `StubCgroupManager` (which does not derive Debug).
pub struct CgroupManagerWrapper(StubCgroupManager);

impl std::fmt::Debug for CgroupManagerWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StubCgroupManager").finish_non_exhaustive()
    }
}

impl std::ops::Deref for CgroupManagerWrapper {
    type Target = StubCgroupManager;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl CgroupManager for CgroupManagerWrapper {
    fn create_hierarchy(&self) -> Result<(), hpc_node::CgroupError> {
        self.0.create_hierarchy()
    }
    fn create_scope(
        &self,
        parent_slice: &str,
        name: &str,
        limits: &hpc_node::ResourceLimits,
    ) -> Result<CgroupHandle, hpc_node::CgroupError> {
        self.0.create_scope(parent_slice, name, limits)
    }
    fn destroy_scope(&self, handle: &CgroupHandle) -> Result<(), hpc_node::CgroupError> {
        self.0.destroy_scope(handle)
    }
    fn read_metrics(&self, path: &str) -> Result<CgroupMetrics, hpc_node::CgroupError> {
        self.0.read_metrics(path)
    }
    fn is_scope_empty(&self, handle: &CgroupHandle) -> Result<bool, hpc_node::CgroupError> {
        self.0.is_scope_empty(handle)
    }
}

impl From<StubCgroupManager> for CgroupManagerWrapper {
    fn from(m: StubCgroupManager) -> Self {
        Self(m)
    }
}
use lattice_api::middleware::rbac::Role;
use lattice_node_agent::agent::NodeAgent;
use lattice_node_agent::epilogue::EpilogueResult;
use lattice_node_agent::heartbeat::Heartbeat;
use lattice_node_agent::image_cache::ImageCache;
use lattice_node_agent::pmi2::fence::FenceCoordinator;
use lattice_node_agent::pmi2::protocol::Pmi2Command;
use lattice_node_agent::pmi2::server::Pmi2Server;
use lattice_node_agent::process_launcher::ProcessLauncher;
use lattice_node_agent::prologue::PrologueResult;
use lattice_quorum::backup::BackupMetadata;
use lattice_quorum::global_state::GlobalState;
use lattice_scheduler::autoscaler::ScaleDecision;
use lattice_scheduler::data_staging::StagingPlan;
use lattice_scheduler::federation::FederationBroker;
use lattice_scheduler::federation::OfferDecision;
use lattice_scheduler::preemption::PreemptionResult;
use lattice_scheduler::walltime::{WalltimeEnforcer, WalltimeExpiry};

use lattice_api::mpi::MpiLaunchOrchestrator;

/// A persistent PMI-2 connection to a server, used across multiple cucumber steps.
#[cfg(unix)]
pub struct PmiConnection {
    pub reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    pub writer: tokio::net::unix::OwnedWriteHalf,
}

#[cfg(unix)]
impl std::fmt::Debug for PmiConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PmiConnection").finish_non_exhaustive()
    }
}

#[derive(Debug, World)]
#[world(init = Self::new)]
pub struct LatticeWorld {
    pub nodes: Vec<Node>,
    pub allocations: Vec<Allocation>,
    pub tenants: Vec<Tenant>,
    pub vclusters: Vec<VCluster>,
    pub last_error: Option<LatticeError>,
    pub store: MockAllocationStore,
    pub registry: MockNodeRegistry,
    pub audit: MockAuditLog,
    /// Named allocations for DAG scenarios
    pub named_allocations: HashMap<String, Allocation>,
    pub dag_id: Option<String>,
    // Phase 8: end-to-end fields
    pub last_heartbeat: Option<Heartbeat>,
    pub agent: Option<NodeAgent>,
    pub agent_alloc_id: Option<Uuid>,
    pub should_checkpoint: Option<bool>,
    // Prologue/epilogue fields
    pub prologue_result: Option<PrologueResult>,
    pub epilogue_result: Option<EpilogueResult>,
    pub image_cache: Option<ImageCache>,
    // Federation fields
    pub federation_broker: Option<FederationBroker>,
    pub federation_total_nodes: u32,
    pub federation_idle_nodes: u32,
    pub federation_decision: Option<OfferDecision>,
    // Session management
    pub session_alloc_idx: Option<usize>,
    pub session_indices: Vec<usize>,
    pub session_user: Option<String>,
    pub session_max_per_user: Option<u32>,
    pub active_sessions_per_user: HashMap<String, u32>,
    // RBAC
    pub last_rbac_result: Option<bool>,
    pub current_role: Option<Role>,
    pub current_user: Option<String>,
    pub requesting_tenant: Option<String>,
    // Streaming / EventBus
    pub event_bus: Option<Arc<EventBus>>,
    pub named_alloc_ids: HashMap<String, Uuid>,
    pub received_events: HashMap<String, Vec<AllocationEvent>>,
    pub named_received_events: HashMap<String, Vec<AllocationEvent>>,
    pub slow_subscriber_name: Option<String>,
    pub event_bus_blocked: Option<bool>,
    pub dropped_event_count: u64,
    pub disconnected_subscribers: Vec<String>,
    // Data staging
    pub staging_plan: Option<StagingPlan>,
    pub data_readiness: HashMap<String, f64>,
    // Preemption
    pub preemption_result: Option<PreemptionResult>,
    // Network domains
    pub network_domains: Vec<NetworkDomain>,
    // Observability
    pub log_buffer_data: Vec<u8>,
    pub attach_owner: Option<String>,
    pub attach_user: Option<String>,
    pub attach_allowed: Option<bool>,
    pub metrics_streaming: bool,
    pub diagnostics_data: Option<String>,
    pub resolution_mode: Option<String>,
    // Autoscaling
    pub scale_decision: Option<ScaleDecision>,
    pub last_scale_time: Option<Instant>,
    pub tsdb_available: bool,
    pub alloc_min_nodes: HashMap<String, u32>,
    pub alloc_current_nodes: HashMap<String, u32>,
    // Per-node tags (Node struct has no tags field, tracked here for BDD scenarios)
    pub node_tags: HashMap<String, HashMap<String, String>>,
    // GPU topology / conformance filtering
    pub filtered_nodes: Vec<String>,
    pub locality_scores: Vec<(String, f64)>,
    // Backup & restore
    pub backup_state: Option<Arc<RwLock<GlobalState>>>,
    pub backup_path: Option<PathBuf>,
    pub backup_metadata: Option<BackupMetadata>,
    pub verified_metadata: Option<BackupMetadata>,
    pub backup_error: Option<String>,
    pub restore_data_dir: Option<PathBuf>,
    pub _backup_tempdir: Option<tempfile::TempDir>,
    // Walltime enforcement
    pub walltime_enforcer: Option<WalltimeEnforcer>,
    pub walltime_alloc_id: Option<Uuid>,
    pub walltime_alloc_ids: Vec<Uuid>,
    pub walltime_start: Option<chrono::DateTime<Utc>>,
    pub walltime_expired: Vec<WalltimeExpiry>,
    // Failure modes
    pub failed_node_alloc_state: Option<AllocationState>,
    pub quorum_nodes: Vec<String>,
    pub quorum_leader: Option<String>,
    pub quorum_available: bool,
    pub vcluster_crashed: Option<String>,
    pub api_crashed: bool,
    pub checkpoint_broker_crashed: bool,
    pub network_partitioned: bool,
    pub storage_available: bool,
    pub openchamj_available: bool,
    pub prologue_retry_count: u32,
    pub prologue_max_retries: u32,
    pub requeue_policy: Option<String>,
    pub requeue_count: u32,
    pub requeue_limit: u32,
    // MPI / PMI-2
    #[cfg(unix)]
    pub pmi_server: Option<Arc<Pmi2Server>>,
    #[cfg(unix)]
    pub pmi_socket_path: Option<PathBuf>,
    #[cfg(unix)]
    pub pmi_responses: Vec<String>,
    #[cfg(unix)]
    pub pmi_connections: Vec<PmiConnection>,
    pub rank_layout: Option<RankLayout>,
    pub launch_result: Option<Result<LaunchId, String>>,
    pub parsed_command: Option<Result<Pmi2Command, String>>,
    pub rank_env: Option<HashMap<String, String>>,
    pub process_launcher: Option<ProcessLauncher>,
    pub fence_merged: Option<HashMap<String, String>>,
    pub mpi_temp_dir: Option<tempfile::TempDir>,
    #[cfg(unix)]
    pub pmi_server_handle: Option<tokio::task::JoinHandle<()>>,
    pub mpi_orchestrator: Option<MpiLaunchOrchestrator>,
    pub mpi_node_ids: Vec<NodeId>,
    pub mpi_node_addresses: HashMap<NodeId, String>,
    pub mpi_tasks_per_node_cfg: u32,
    pub mpi_num_nodes_cfg: u32,
    pub fence_coordinator: Option<FenceCoordinator>,
    pub fence_contributions: Vec<(u32, HashMap<String, String>)>,
    // Identity cascade
    pub identity_spire_available: bool,
    pub identity_self_signed_cached: bool,
    pub identity_bootstrap_exists: bool,
    pub identity_cascade_empty: bool,
    pub identity_result: Option<Result<WorkloadIdentity, String>>,
    pub identity_issued_at: Option<chrono::DateTime<Utc>>,
    pub identity_tls_config_ok: Option<bool>,
    #[allow(dead_code)]
    pub identity_rotator: Option<IdentityRotatorWrapper>,
    pub identity_rotation_failed: bool,
    pub _identity_tempdir: Option<tempfile::TempDir>,
    // Cgroup isolation
    #[allow(dead_code)]
    pub cgroup_manager: Option<CgroupManagerWrapper>,
    pub cgroup_handle: Option<CgroupHandle>,
    pub cgroup_metrics: Option<CgroupMetrics>,
    pub cgroup_result_ok: Option<bool>,
    pub cgroup_is_empty: Option<bool>,
    // Software delivery
    pub sd_image_registry: HashMap<String, (ImageRef, ImageMetadata)>,
    pub sd_env_patches: Vec<EnvPatch>,
    pub sd_process_env: HashMap<String, String>,
    pub sd_images: Vec<ImageRef>,
    pub sd_mounts: Vec<MountSpec>,
    pub sd_container_spec: Option<ContainerSpec>,
    pub sd_edf_search_dir: Option<tempfile::TempDir>,
    pub sd_resolved_devices: Vec<String>,
    pub sd_resolved_mounts: Vec<String>,
    pub sd_submit_error: Option<String>,
    pub sd_podman_container_id: Option<String>,
    pub sd_podman_container_pid: Option<u32>,
    pub sd_podman_devices: Vec<String>,
    pub sd_is_sensitive: bool,
    pub sd_container_writable: bool,
    pub sd_f5_scores: HashMap<String, f64>,
    pub sd_cli_result: Option<String>,
    pub sd_persisted_container_ids: Vec<String>,
    pub sd_reattach_result: Option<String>,
    // ── Dispatch (allocation_dispatch.feature) ───────────────────
    /// A scenario-local GlobalState used by dispatch step defs to drive
    /// Raft commands directly (applies are synchronous on a fresh state).
    pub dispatch_state: Option<lattice_quorum::global_state::GlobalState>,
    /// Per-scenario context: current node id, last response, named allocs.
    pub dispatch_ctx: DispatchCtx,
}

/// Per-scenario scratch state for dispatch step definitions.
#[derive(Default, Debug)]
pub struct DispatchCtx {
    pub node_id: String,
    pub pending_address: String,
    pub last_response: Option<lattice_quorum::commands::CommandResponse>,
    pub last_visible_nodes: Vec<String>,
    pub alloc_name_to_id: std::collections::HashMap<String, uuid::Uuid>,
    pub state_version_before: u64,
    pub completion_buffer: lattice_node_agent::allocation_runner::CompletionBuffer,
    pub cert_sans: Vec<String>,
    pub san_validation_result: Option<Result<(), String>>,
    pub ambiguous_alloc: Option<lattice_common::types::Allocation>,
    pub user_env_vars: Vec<(String, String)>,
    pub prologue_panic: bool,
    pub multi_node_ids: Vec<String>,
}

impl LatticeWorld {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            allocations: Vec::new(),
            tenants: Vec::new(),
            vclusters: Vec::new(),
            last_error: None,
            store: MockAllocationStore::new(),
            registry: MockNodeRegistry::new(),
            audit: MockAuditLog::new(),
            named_allocations: HashMap::new(),
            dag_id: None,
            last_heartbeat: None,
            agent: None,
            agent_alloc_id: None,
            should_checkpoint: None,
            prologue_result: None,
            epilogue_result: None,
            image_cache: None,
            federation_broker: None,
            federation_total_nodes: 0,
            federation_idle_nodes: 0,
            federation_decision: None,
            session_alloc_idx: None,
            session_indices: Vec::new(),
            session_user: None,
            session_max_per_user: None,
            active_sessions_per_user: HashMap::new(),
            last_rbac_result: None,
            current_role: None,
            current_user: None,
            requesting_tenant: None,
            event_bus: None,
            named_alloc_ids: HashMap::new(),
            received_events: HashMap::new(),
            named_received_events: HashMap::new(),
            slow_subscriber_name: None,
            event_bus_blocked: None,
            dropped_event_count: 0,
            disconnected_subscribers: Vec::new(),
            staging_plan: None,
            data_readiness: HashMap::new(),
            preemption_result: None,
            network_domains: Vec::new(),
            log_buffer_data: Vec::new(),
            attach_owner: None,
            attach_user: None,
            attach_allowed: None,
            metrics_streaming: false,
            diagnostics_data: None,
            resolution_mode: None,
            scale_decision: None,
            last_scale_time: None,
            tsdb_available: true,
            alloc_min_nodes: HashMap::new(),
            alloc_current_nodes: HashMap::new(),
            node_tags: HashMap::new(),
            filtered_nodes: Vec::new(),
            locality_scores: Vec::new(),
            backup_state: None,
            backup_path: None,
            backup_metadata: None,
            verified_metadata: None,
            backup_error: None,
            restore_data_dir: None,
            _backup_tempdir: None,
            walltime_enforcer: None,
            walltime_alloc_id: None,
            walltime_alloc_ids: Vec::new(),
            walltime_start: None,
            walltime_expired: Vec::new(),
            failed_node_alloc_state: None,
            quorum_nodes: Vec::new(),
            quorum_leader: None,
            quorum_available: true,
            vcluster_crashed: None,
            api_crashed: false,
            checkpoint_broker_crashed: false,
            network_partitioned: false,
            storage_available: true,
            openchamj_available: true,
            prologue_retry_count: 0,
            prologue_max_retries: 3,
            requeue_policy: None,
            requeue_count: 0,
            requeue_limit: 3,
            // MPI / PMI-2
            #[cfg(unix)]
            pmi_server: None,
            #[cfg(unix)]
            pmi_socket_path: None,
            #[cfg(unix)]
            pmi_responses: Vec::new(),
            #[cfg(unix)]
            pmi_connections: Vec::new(),
            rank_layout: None,
            launch_result: None,
            parsed_command: None,
            rank_env: None,
            process_launcher: None,
            fence_merged: None,
            mpi_temp_dir: None,
            #[cfg(unix)]
            pmi_server_handle: None,
            mpi_orchestrator: None,
            mpi_node_ids: Vec::new(),
            mpi_node_addresses: HashMap::new(),
            mpi_tasks_per_node_cfg: 0,
            mpi_num_nodes_cfg: 0,
            fence_coordinator: None,
            fence_contributions: Vec::new(),
            // Identity cascade
            identity_spire_available: false,
            identity_self_signed_cached: false,
            identity_bootstrap_exists: false,
            identity_cascade_empty: false,
            identity_result: None,
            identity_issued_at: None,
            identity_tls_config_ok: None,
            identity_rotator: None,
            identity_rotation_failed: false,
            _identity_tempdir: None,
            // Cgroup isolation
            cgroup_manager: None,
            cgroup_handle: None,
            cgroup_metrics: None,
            cgroup_result_ok: None,
            cgroup_is_empty: None,
            // Software delivery
            sd_image_registry: HashMap::new(),
            sd_env_patches: Vec::new(),
            sd_process_env: HashMap::new(),
            sd_images: Vec::new(),
            sd_mounts: Vec::new(),
            sd_container_spec: None,
            sd_edf_search_dir: None,
            sd_resolved_devices: Vec::new(),
            sd_resolved_mounts: Vec::new(),
            sd_submit_error: None,
            sd_podman_container_id: None,
            sd_podman_container_pid: None,
            sd_podman_devices: Vec::new(),
            sd_is_sensitive: false,
            sd_container_writable: false,
            sd_f5_scores: HashMap::new(),
            sd_cli_result: None,
            sd_persisted_container_ids: Vec::new(),
            sd_reattach_result: None,
            dispatch_state: None,
            dispatch_ctx: DispatchCtx::default(),
        }
    }

    pub fn last_allocation(&self) -> &Allocation {
        self.allocations.last().expect("no allocations submitted")
    }

    pub fn last_allocation_mut(&mut self) -> &mut Allocation {
        self.allocations
            .last_mut()
            .expect("no allocations submitted")
    }
}

#[tokio::main]
async fn main() {
    LatticeWorld::cucumber().run("features/").await;
}
