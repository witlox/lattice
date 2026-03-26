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

    /// Identity cascade configuration for workload mTLS
    #[serde(default)]
    pub identity: Option<IdentityConfig>,

    /// HashiCorp Vault integration for secret resolution.
    /// When present, all secret fields are resolved from Vault KV v2.
    /// When absent, secrets fall back to env vars → config literals.
    #[serde(default)]
    pub vault: Option<VaultConfig>,
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
                oidc_client_id: None,
                tls_cert: None,
                tls_key: None,
                tls_ca: None,
                bind_network: BindNetwork::Any,
            },
            storage: StorageConfig {
                vast_api_url: None,
                vast_username: None,
                vast_password: None,
                vast_timeout_secs: default_vast_timeout(),
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
            identity: None,
            vault: None,
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
    /// Directory for persistent Raft storage (WAL, snapshots).
    /// When None, uses in-memory storage (test/dev mode).
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    /// Network to bind to: "hsn" (high-speed, default for production),
    /// "management" (1G admin), or "any" (0.0.0.0, for dev/standalone).
    /// See PACT ADR-017: lattice traffic runs on HSN, PACT on management.
    #[serde(default = "default_bind_network")]
    pub bind_network: BindNetwork,
    /// Path to Ed25519 signing key for audit log entries (F10).
    /// When None, a random key is generated (dev/test mode only).
    /// Production deployments MUST set this to a persistent key file.
    #[serde(default)]
    pub audit_signing_key_path: Option<PathBuf>,
}

fn default_raft_listen_address() -> String {
    "0.0.0.0:9000".to_string()
}

fn default_bind_network() -> BindNetwork {
    BindNetwork::Any
}

/// Which network interface lattice services bind to.
///
/// HPC infrastructure has two networks (PACT ADR-017):
/// - Management (1G Ethernet): PXE boot, BMC, admin — PACT uses this
/// - HSN (Slingshot/UE 200G+): workload traffic, MPI, storage — Lattice uses this
///
/// In production, lattice should bind to HSN. In dev/standalone mode, "any" is fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BindNetwork {
    /// Bind to high-speed network interface (Slingshot/Ultra Ethernet).
    /// Production default when co-deployed with PACT.
    Hsn,
    /// Bind to management network interface (1G Ethernet).
    /// Not recommended for lattice — use for diagnostics only.
    Management,
    /// Bind to all interfaces (0.0.0.0). Default for dev/standalone mode.
    Any,
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
            data_dir: None,
            bind_network: BindNetwork::Any,
            audit_signing_key_path: None,
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
    /// OIDC client ID for auth discovery
    #[serde(default)]
    pub oidc_client_id: Option<String>,
    /// TLS certificate path (PEM)
    pub tls_cert: Option<PathBuf>,
    /// TLS private key path (PEM)
    pub tls_key: Option<PathBuf>,
    /// CA certificate path (PEM) for mTLS client verification.
    /// When set, the server requires clients to present a certificate
    /// signed by this CA.
    #[serde(default)]
    pub tls_ca: Option<PathBuf>,
    /// Network to bind to (see [`BindNetwork`]).
    #[serde(default = "default_bind_network")]
    pub bind_network: BindNetwork,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// VAST API endpoint (for QoS, catalog, staging)
    pub vast_api_url: Option<String>,
    /// VAST API username
    #[serde(default)]
    pub vast_username: Option<String>,
    /// VAST API password
    #[serde(default)]
    pub vast_password: Option<String>,
    /// VAST API request timeout in seconds
    #[serde(default = "default_vast_timeout")]
    pub vast_timeout_secs: u64,
    /// S3 endpoint for object storage
    pub s3_endpoint: String,
    /// Default NFS mount point for home directories
    pub nfs_home_path: String,
    /// Node-local scratch path
    pub local_scratch_path: String,
}

fn default_vast_timeout() -> u64 {
    30
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
    /// Network to bind to (see [`BindNetwork`]).
    /// Production: "hsn" (connects to quorum on HSN).
    /// Standalone: "any" (default).
    #[serde(default = "default_bind_network")]
    pub bind_network: BindNetwork,
}

impl Default for NodeAgentConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_seconds: 10,
            heartbeat_timeout_seconds: 30,
            grace_period_seconds: 120,
            sensitive_grace_period_seconds: 600,
            bind_network: BindNetwork::Any,
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
    /// Rolling window for GPU-hours budget tracking (days).
    /// Allocations older than this are excluded from budget utilization.
    #[serde(default = "default_budget_period_days")]
    pub budget_period_days: u32,
}

fn default_budget_period_days() -> u32 {
    90
}

impl Default for SchedulingConfig {
    fn default() -> Self {
        Self {
            cycle_interval_seconds: 5,
            backfill_depth: 100,
            budget_period_days: default_budget_period_days(),
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
    pub waldur_token: String,
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
            waldur_token: String::new(),
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

// ─── Identity ──────────────────────────────────────────────

/// Identity cascade configuration for workload mTLS.
///
/// Configures the three identity providers tried in order:
/// SPIRE -> SelfSigned (quorum CA) -> Bootstrap (filesystem).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityConfig {
    /// SPIRE agent socket path. Default: /run/spire/agent.sock
    #[serde(default = "default_spire_socket")]
    pub spire_socket: String,
    /// Lattice-quorum endpoint for CSR signing (SelfSignedProvider fallback).
    #[serde(default)]
    pub signing_endpoint: Option<String>,
    /// Bootstrap certificate path.
    #[serde(default)]
    pub bootstrap_cert: Option<String>,
    /// Bootstrap private key path.
    #[serde(default)]
    pub bootstrap_key: Option<String>,
    /// Bootstrap CA trust bundle path.
    #[serde(default)]
    pub bootstrap_ca: Option<String>,
    /// Certificate lifetime for self-signed certs (seconds). Default: 259200 (3 days).
    #[serde(default = "default_cert_lifetime")]
    pub cert_lifetime_seconds: u64,
}

fn default_spire_socket() -> String {
    "/run/spire/agent.sock".to_string()
}

fn default_cert_lifetime() -> u64 {
    259_200 // 3 days
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            spire_socket: default_spire_socket(),
            signing_endpoint: None,
            bootstrap_cert: None,
            bootstrap_key: None,
            bootstrap_ca: None,
            cert_lifetime_seconds: default_cert_lifetime(),
        }
    }
}

// ─── Vault ─────────────────────────────────────────────────

/// HashiCorp Vault integration for secret resolution.
///
/// When this section is present in configuration, ALL secret fields
/// (VAST credentials, Waldur token, audit signing key, Sovra key) are
/// resolved from Vault KV v2. Config file literals and environment
/// variables for those fields are ignored (INV-SEC4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultConfig {
    /// Vault server URL (e.g., "https://vault.example.com:8200").
    pub address: String,

    /// KV v2 mount + path prefix. Convention-based paths: field
    /// `{section}.{field}` maps to `GET {prefix}/{section}` key `{field}`.
    #[serde(default = "default_vault_prefix")]
    pub prefix: String,

    /// AppRole role ID. Not secret — safe in config files.
    pub role_id: String,

    /// Name of environment variable containing the AppRole secret ID.
    /// The secret ID value itself is NEVER stored in config files.
    #[serde(default = "default_vault_secret_id_env")]
    pub secret_id_env: String,

    /// CA certificate bundle for verifying Vault's TLS certificate.
    /// When None, the system trust store is used (may include SPIRE-provisioned CAs).
    /// TLS verification is never disabled.
    #[serde(default)]
    pub tls_ca_path: Option<PathBuf>,

    /// HTTP client timeout for Vault requests in seconds.
    #[serde(default = "default_vault_timeout")]
    pub timeout_secs: u64,
}

fn default_vault_prefix() -> String {
    "secret/data/lattice".to_string()
}

fn default_vault_secret_id_env() -> String {
    "VAULT_SECRET_ID".to_string()
}

fn default_vault_timeout() -> u64 {
    10
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            address: String::new(),
            prefix: default_vault_prefix(),
            role_id: String::new(),
            secret_id_env: default_vault_secret_id_env(),
            tls_ca_path: None,
            timeout_secs: default_vault_timeout(),
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
        assert_eq!(cfg.budget_period_days, 90);
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
    fn identity_config_defaults() {
        let cfg = IdentityConfig::default();
        assert_eq!(cfg.spire_socket, "/run/spire/agent.sock");
        assert!(cfg.signing_endpoint.is_none());
        assert!(cfg.bootstrap_cert.is_none());
        assert!(cfg.bootstrap_key.is_none());
        assert!(cfg.bootstrap_ca.is_none());
        assert_eq!(cfg.cert_lifetime_seconds, 259_200);
    }

    #[test]
    fn identity_config_deserializes() {
        let yaml = r#"
spire_socket: /custom/spire.sock
signing_endpoint: https://quorum:9443
bootstrap_cert: /etc/lattice/cert.pem
bootstrap_key: /etc/lattice/key.pem
bootstrap_ca: /etc/lattice/ca.pem
cert_lifetime_seconds: 86400
"#;
        let cfg: IdentityConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.spire_socket, "/custom/spire.sock");
        assert_eq!(cfg.signing_endpoint.as_deref(), Some("https://quorum:9443"));
        assert_eq!(cfg.cert_lifetime_seconds, 86400);
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
