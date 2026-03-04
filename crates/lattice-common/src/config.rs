use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

    /// Node agent configuration
    #[serde(default)]
    pub node_agent: Option<NodeAgentConfig>,

    /// Network (VNI pool) configuration
    #[serde(default)]
    pub network: Option<NetworkConfig>,

    /// Checkpoint broker configuration
    #[serde(default)]
    pub checkpoint: Option<CheckpointConfig>,

    /// Scheduling cycle configuration
    #[serde(default)]
    pub scheduling: Option<SchedulingConfig>,

    /// External accounting (Waldur) integration
    #[serde(default)]
    pub accounting: Option<AccountingConfig>,

    /// API rate limiting
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,

    /// Slurm compatibility layer configuration
    #[serde(default)]
    pub compat: Option<CompatConfig>,
}

impl Default for LatticeConfig {
    fn default() -> Self {
        Self {
            role: NodeRole::Combined,
            quorum: QuorumConfig::default(),
            api: ApiConfig {
                grpc_address: "0.0.0.0:50051".to_string(),
                rest_address: Some("0.0.0.0:8080".to_string()),
                oidc_issuer: String::new(),
                tls_cert: None,
                tls_key: None,
                tls_ca: None,
            },
            storage: StorageConfig {
                vast_api_url: None,
                s3_endpoint: String::new(),
                nfs_home_path: "/home".to_string(),
                local_scratch_path: "/scratch".to_string(),
            },
            telemetry: TelemetryConfig {
                default_mode: "prod".to_string(),
                tsdb_endpoint: String::new(),
                prod_interval_seconds: 30,
                ebpf_programs_path: PathBuf::from("/opt/lattice/ebpf"),
            },
            federation: None,
            node_agent: None,
            network: None,
            checkpoint: None,
            scheduling: None,
            accounting: None,
            rate_limit: None,
            compat: None,
        }
    }
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
    /// Address for the Raft transport to listen on
    #[serde(default = "default_raft_listen_address")]
    pub raft_listen_address: String,
}

fn default_raft_listen_address() -> String {
    "0.0.0.0:9000".to_string()
}

impl Default for QuorumConfig {
    fn default() -> Self {
        Self {
            node_id: 1,
            peers: Vec::new(),
            election_timeout_ms: 500,
            heartbeat_interval_ms: 100,
            snapshot_threshold: 10000,
            raft_listen_address: default_raft_listen_address(),
        }
    }
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
    /// TLS certificate path (PEM)
    pub tls_cert: Option<PathBuf>,
    /// TLS private key path (PEM)
    pub tls_key: Option<PathBuf>,
    /// CA certificate path (PEM) for mTLS client verification.
    /// When set, the server requires clients to present a certificate
    /// signed by this CA.
    #[serde(default)]
    pub tls_ca: Option<PathBuf>,
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

// ─── Node Agent ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAgentConfig {
    /// Heartbeat interval from node agent to quorum (seconds)
    pub heartbeat_interval_seconds: u64,
    /// Heartbeat timeout before marking node Degraded (seconds)
    pub heartbeat_timeout_seconds: u64,
    /// Grace period before marking node Down (seconds)
    pub grace_period_seconds: u64,
    /// Extended grace period for sensitive nodes (seconds)
    pub sensitive_grace_period_seconds: u64,
}

impl Default for NodeAgentConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_seconds: 10,
            heartbeat_timeout_seconds: 30,
            grace_period_seconds: 120,
            sensitive_grace_period_seconds: 600,
        }
    }
}

// ─── Network ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Start of the VNI pool range (inclusive)
    pub vni_pool_start: u32,
    /// End of the VNI pool range (inclusive)
    pub vni_pool_end: u32,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            vni_pool_start: 100,
            vni_pool_end: 4095,
        }
    }
}

// ─── Checkpoint ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointConfig {
    /// How often the checkpoint broker evaluates the cost function (seconds)
    pub evaluation_interval_seconds: u64,
    /// Timeout for a checkpoint operation to complete (seconds)
    pub checkpoint_timeout_seconds: u64,
    /// Maximum time an application can defer a checkpoint request (seconds)
    pub max_deferral_seconds: u64,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            evaluation_interval_seconds: 30,
            checkpoint_timeout_seconds: 300,
            max_deferral_seconds: 60,
        }
    }
}

// ─── Scheduling ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulingConfig {
    /// Main scheduling cycle interval (seconds)
    pub cycle_interval_seconds: u64,
    /// Maximum number of jobs to consider during backfill pass
    pub backfill_depth: u32,
}

impl Default for SchedulingConfig {
    fn default() -> Self {
        Self {
            cycle_interval_seconds: 5,
            backfill_depth: 100,
        }
    }
}

// ─── Accounting ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountingConfig {
    /// Enable external accounting integration
    pub enabled: bool,
    /// Waldur API URL
    pub waldur_api_url: String,
    /// Kubernetes secret reference for Waldur API token
    pub waldur_token_secret_ref: String,
    /// Interval for pushing accounting events to Waldur (seconds)
    pub push_interval_seconds: u64,
    /// Local buffer size for accounting events before push
    pub buffer_size: u32,
}

impl Default for AccountingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            waldur_api_url: String::new(),
            waldur_token_secret_ref: String::new(),
            push_interval_seconds: 60,
            buffer_size: 1000,
        }
    }
}

// ─── Rate Limiting ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Maximum concurrent Attach sessions per user
    pub attach_max_concurrent: u32,
    /// Maximum concurrent StreamLogs sessions per user
    pub stream_logs_max_concurrent: u32,
    /// Maximum QueryMetrics requests per minute per user
    pub query_metrics_per_minute: u32,
    /// Maximum concurrent StreamMetrics sessions per user
    pub stream_metrics_max_concurrent: u32,
    /// Maximum Diagnostics requests per minute per user
    pub diagnostics_per_minute: u32,
    /// Maximum Compare requests per minute per user
    pub compare_per_minute: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            attach_max_concurrent: 5,
            stream_logs_max_concurrent: 10,
            query_metrics_per_minute: 60,
            stream_metrics_max_concurrent: 5,
            diagnostics_per_minute: 10,
            compare_per_minute: 10,
        }
    }
}

// ─── Compatibility ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatConfig {
    /// Set SLURM_* environment variables in allocation environment
    pub set_slurm_env: bool,
    /// Slurm partition name → vCluster ID mapping
    pub partition_mapping: HashMap<String, String>,
    /// Slurm QOS name → preemption class mapping
    pub qos_mapping: HashMap<String, u32>,
}

impl Default for CompatConfig {
    fn default() -> Self {
        Self {
            set_slurm_env: true,
            partition_mapping: HashMap::new(),
            qos_mapping: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_agent_config_defaults() {
        let cfg = NodeAgentConfig::default();
        assert_eq!(cfg.heartbeat_interval_seconds, 10);
        assert_eq!(cfg.heartbeat_timeout_seconds, 30);
        assert_eq!(cfg.grace_period_seconds, 120);
        assert_eq!(cfg.sensitive_grace_period_seconds, 600);
    }

    #[test]
    fn network_config_defaults() {
        let cfg = NetworkConfig::default();
        assert_eq!(cfg.vni_pool_start, 100);
        assert_eq!(cfg.vni_pool_end, 4095);
        assert!(cfg.vni_pool_start < cfg.vni_pool_end);
    }

    #[test]
    fn checkpoint_config_defaults() {
        let cfg = CheckpointConfig::default();
        assert_eq!(cfg.evaluation_interval_seconds, 30);
        assert_eq!(cfg.checkpoint_timeout_seconds, 300);
        assert_eq!(cfg.max_deferral_seconds, 60);
        assert!(cfg.max_deferral_seconds < cfg.checkpoint_timeout_seconds);
    }

    #[test]
    fn scheduling_config_defaults() {
        let cfg = SchedulingConfig::default();
        assert_eq!(cfg.cycle_interval_seconds, 5);
        assert_eq!(cfg.backfill_depth, 100);
    }

    #[test]
    fn accounting_config_defaults_disabled() {
        let cfg = AccountingConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.push_interval_seconds, 60);
        assert_eq!(cfg.buffer_size, 1000);
    }

    #[test]
    fn rate_limit_config_defaults() {
        let cfg = RateLimitConfig::default();
        assert_eq!(cfg.attach_max_concurrent, 5);
        assert_eq!(cfg.stream_logs_max_concurrent, 10);
        assert_eq!(cfg.query_metrics_per_minute, 60);
        assert_eq!(cfg.stream_metrics_max_concurrent, 5);
        assert_eq!(cfg.diagnostics_per_minute, 10);
        assert_eq!(cfg.compare_per_minute, 10);
    }

    #[test]
    fn compat_config_defaults() {
        let cfg = CompatConfig::default();
        assert!(cfg.set_slurm_env);
        assert!(cfg.partition_mapping.is_empty());
        assert!(cfg.qos_mapping.is_empty());
    }

    #[test]
    fn lattice_config_deserializes_minimal_yaml() {
        let yaml = r#"
role: QuorumMember
quorum:
  node_id: 1
  peers:
    - id: 2
      address: "10.0.0.2:9000"
  election_timeout_ms: 500
  heartbeat_interval_ms: 100
  snapshot_threshold: 10000
api:
  grpc_address: "0.0.0.0:50051"
  oidc_issuer: "https://auth.example.com"
storage:
  s3_endpoint: "https://s3.example.com"
  nfs_home_path: "/home"
  local_scratch_path: "/scratch"
telemetry:
  default_mode: "prod"
  tsdb_endpoint: "https://tsdb.example.com"
  prod_interval_seconds: 30
  ebpf_programs_path: "/opt/lattice/ebpf"
"#;
        let cfg: LatticeConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.quorum.node_id, 1);
        assert_eq!(cfg.quorum.peers.len(), 1);
        assert_eq!(cfg.api.grpc_address, "0.0.0.0:50051");
        assert!(cfg.federation.is_none());
        assert!(cfg.node_agent.is_none());
        assert!(cfg.network.is_none());
        // raft_listen_address defaults when omitted
        assert_eq!(cfg.quorum.raft_listen_address, "0.0.0.0:9000");
    }

    #[test]
    fn quorum_config_defaults() {
        let cfg = QuorumConfig::default();
        assert_eq!(cfg.node_id, 1);
        assert!(cfg.peers.is_empty());
        assert_eq!(cfg.raft_listen_address, "0.0.0.0:9000");
    }

    #[test]
    fn lattice_config_defaults() {
        let cfg = LatticeConfig::default();
        assert_eq!(cfg.quorum.node_id, 1);
        assert!(cfg.quorum.peers.is_empty());
        assert_eq!(cfg.api.grpc_address, "0.0.0.0:50051");
        assert!(cfg.telemetry.tsdb_endpoint.is_empty());
        assert!(cfg.federation.is_none());
    }
}
