use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level Lattice configuration.
/// Loaded from YAML/TOML config files + environment variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatticeConfig {
    /// This node's role
    pub role: NodeRole,

    /// Quorum configuration
    pub quorum: QuorumConfig,

    /// API server configuration
    pub api: ApiConfig,

    /// Storage integration
    pub storage: StorageConfig,

    /// Telemetry configuration
    pub telemetry: TelemetryConfig,

    /// Federation (optional)
    #[serde(default)]
    pub federation: Option<FederationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeRole {
    /// Quorum member (runs Raft + API server)
    QuorumMember,
    /// Compute node (runs node agent)
    ComputeNode,
    /// Both (for small deployments / testing)
    Combined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuorumConfig {
    /// This node's ID in the Raft cluster
    pub node_id: u64,
    /// Peer addresses for Raft cluster
    pub peers: Vec<PeerConfig>,
    /// Raft election timeout (ms)
    pub election_timeout_ms: u64,
    /// Raft heartbeat interval (ms)
    pub heartbeat_interval_ms: u64,
    /// Log compaction threshold
    pub snapshot_threshold: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConfig {
    pub id: u64,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// gRPC listen address
    pub grpc_address: String,
    /// REST gateway address (optional)
    pub rest_address: Option<String>,
    /// OIDC provider URL for token validation
    pub oidc_issuer: String,
    /// TLS certificate path
    pub tls_cert: Option<PathBuf>,
    /// TLS key path
    pub tls_key: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// VAST API endpoint (for QoS, catalog, staging)
    pub vast_api_url: Option<String>,
    /// S3 endpoint for object storage
    pub s3_endpoint: String,
    /// Default NFS mount point for home directories
    pub nfs_home_path: String,
    /// Node-local scratch path
    pub local_scratch_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// Default telemetry mode
    pub default_mode: String, // "prod", "debug", "audit"
    /// Time-series store endpoint
    pub tsdb_endpoint: String,
    /// Aggregation interval for prod mode (seconds)
    pub prod_interval_seconds: u64,
    /// eBPF program directory
    pub ebpf_programs_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationConfig {
    /// Sovra API endpoint
    pub sovra_endpoint: String,
    /// Sovra workspace ID for this site
    pub workspace_id: String,
    /// Federation broker listen address
    pub broker_address: String,
    /// Peer sites
    pub peers: Vec<FederationPeer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationPeer {
    pub name: String,
    pub broker_address: String,
    pub sovra_workspace_id: String,
}
