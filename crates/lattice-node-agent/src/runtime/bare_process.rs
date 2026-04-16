//! Bare-Process Runtime: spawns the allocation entrypoint directly on the
//! host under a cgroup scope, with no mount namespace and no container.
//!
//! Designed for Slurm-compatible bare-binary jobs where the user expects the
//! entrypoint to run against the host filesystem. Selected when the
//! allocation has neither a `uenv` nor an `image` set (see `select_runtime`).
//!
//! Environment scrubbing rules (DEC-DISP-05 + D-ADV-ARCH-11 resolution):
//!
//! The workload inherits a FILTERED subset of the agent's process
//! environment. Variables matching the block-list are stripped; variables
//! explicitly in the allow-list are passed through. User-provided
//! `env_vars` from the AllocationSpec are applied LAST (with precedence
//! over inherited values) but are still subject to the block-list — a user
//! key matching any block-list pattern is a `MALFORMED_REQUEST` at prologue
//! time.
//!
//! See `specs/architecture/interfaces/allocation-dispatch.md` § BareProcess.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use lattice_common::types::AllocId;

use super::{ExitStatus, PrepareConfig, ProcessHandle, Runtime, RuntimeError};

/// Configuration for the Bare-Process runtime's environment scrubbing.
#[derive(Debug, Clone)]
pub struct BareProcessConfig {
    /// Additional env-var patterns to strip beyond the built-in block-list.
    /// Matched case-insensitively as substrings.
    pub extra_block_list: Vec<String>,
    /// Additional env-var names to allow beyond the built-in allow-list.
    pub extra_allow_list: Vec<String>,
    /// Include SLURM_* variables (opt-in via `compat.set_slurm_env`).
    pub include_slurm_vars: bool,
}

impl Default for BareProcessConfig {
    fn default() -> Self {
        Self {
            extra_block_list: Vec::new(),
            extra_allow_list: Vec::new(),
            include_slurm_vars: false,
        }
    }
}

/// Bare-Process Runtime. Spawns workloads directly via `tokio::process::Command`.
pub struct BareProcessRuntime {
    config: BareProcessConfig,
    /// Tracks running children by allocation for signal/stop/wait.
    children: Arc<Mutex<HashMap<AllocId, tokio::process::Child>>>,
    /// Prepared environment per allocation (scrubbed env + cwd).
    prepared: Arc<Mutex<HashMap<AllocId, PreparedEnv>>>,
}

#[derive(Debug, Clone)]
struct PreparedEnv {
    env: Vec<(String, String)>,
    workdir: Option<String>,
}

impl BareProcessRuntime {
    pub fn new() -> Self {
        Self {
            config: BareProcessConfig::default(),
            children: Arc::new(Mutex::new(HashMap::new())),
            prepared: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_config(config: BareProcessConfig) -> Self {
        Self {
            config,
            children: Arc::new(Mutex::new(HashMap::new())),
            prepared: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Apply block/allow-list scrubbing to the agent's environment, then
    /// merge in user-provided `env_vars`. User keys in the block-list are
    /// rejected up-front (returned as `Err`), per D-ADV-ARCH-11 resolution.
    fn build_workload_env(
        &self,
        user_env: &[(String, String)],
    ) -> Result<Vec<(String, String)>, String> {
        // Validate user-provided keys against block-list first so we can
        // return MALFORMED_REQUEST before doing any prologue work.
        for (key, _) in user_env {
            if matches_block_list(key, &self.config.extra_block_list) {
                return Err(format!("env_var_in_block_list: {key}"));
            }
        }

        let mut result: HashMap<String, String> = HashMap::new();
        // Step 1: filter agent env through allow-list and block-list.
        for (k, v) in std::env::vars() {
            if matches_block_list(&k, &self.config.extra_block_list) {
                continue;
            }
            if allow_list_matches(&k, &self.config) {
                result.insert(k, v);
            }
        }
        // Step 2: apply user overrides. Block-list was already checked above.
        for (k, v) in user_env {
            result.insert(k.clone(), v.clone());
        }
        Ok(result.into_iter().collect())
    }
}

impl Default for BareProcessRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns true if `name` matches any block-list pattern. Block-list is
/// exact-name + site-config + built-in substrings (case-insensitive).
fn matches_block_list(name: &str, extra: &[String]) -> bool {
    let upper = name.to_ascii_uppercase();
    // Built-in patterns — substring match, case-insensitive.
    const BLOCKED_SUBSTRINGS: &[&str] =
        &["SECRET", "TOKEN", "KEY", "PASSWORD", "PASSWD", "CREDENTIAL"];
    for pat in BLOCKED_SUBSTRINGS {
        if upper.contains(pat) {
            return true;
        }
    }
    // Built-in prefixes — namespace-level blocks.
    const BLOCKED_PREFIXES: &[&str] = &["LATTICE_AGENT_", "VAULT_"];
    for pre in BLOCKED_PREFIXES {
        if upper.starts_with(pre) {
            return true;
        }
    }
    // Site-configured extras (case-insensitive substring).
    for extra_pat in extra {
        if upper.contains(&extra_pat.to_ascii_uppercase()) {
            return true;
        }
    }
    false
}

/// Returns true if `name` is in the allow-list. Allow-list is explicit
/// names + namespace prefixes appropriate for HPC workloads.
fn allow_list_matches(name: &str, config: &BareProcessConfig) -> bool {
    // Shell basics
    const EXACT_NAMES: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LANG",
        "TZ",
        "SHELL",
        "TMPDIR",
        "XDG_RUNTIME_DIR",
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
    ];
    if EXACT_NAMES.iter().any(|&e| e == name) {
        return true;
    }
    // Locale
    if name.starts_with("LC_") {
        return true;
    }
    // HPC compute
    if name.starts_with("OMP_") || name == "MKL_NUM_THREADS" || name.starts_with("MKL_") {
        return true;
    }
    // GPU selection
    if matches!(
        name,
        "CUDA_VISIBLE_DEVICES"
            | "CUDA_DEVICE_ORDER"
            | "ROCR_VISIBLE_DEVICES"
            | "HIP_VISIBLE_DEVICES"
            | "GPU_DEVICE_ORDINAL"
    ) {
        return true;
    }
    // MPI / networking (but NOT OMPI_MCA_pmix which may contain tokens —
    // that's OMPI_, not a secret). Our block-list substring rule "TOKEN"
    // already filters misuse.
    if name.starts_with("PMI_")
        || name.starts_with("MPI_")
        || name.starts_with("OMPI_")
        || name.starts_with("MPICH_")
        || name.starts_with("FI_")
        || name.starts_with("NCCL_")
        || name.starts_with("CXI_")
    {
        return true;
    }
    // Lattice allocation context — user-facing LATTICE_* that is NOT
    // LATTICE_AGENT_* (already blocked by prefix).
    if name.starts_with("LATTICE_") {
        return true;
    }
    // Slurm compat (opt-in)
    if config.include_slurm_vars && name.starts_with("SLURM_") {
        return true;
    }
    // Site-configured extras
    if config.extra_allow_list.iter().any(|e| e == name) {
        return true;
    }
    false
}

#[async_trait]
impl Runtime for BareProcessRuntime {
    async fn prepare(&self, config: &PrepareConfig) -> Result<(), RuntimeError> {
        // Bare-process has no image to pull or mount to create — its
        // "prologue" is purely environment scrubbing + cwd selection.
        let env = self
            .build_workload_env(&config.env_vars)
            .map_err(|reason| RuntimeError::PrepareFailed {
                alloc_id: config.alloc_id,
                reason,
            })?;
        self.prepared.lock().await.insert(
            config.alloc_id,
            PreparedEnv {
                env,
                workdir: config.workdir.clone(),
            },
        );
        info!(
            alloc_id = %config.alloc_id,
            "bare-process runtime prepared (no image, no namespace)"
        );
        Ok(())
    }

    async fn spawn(
        &self,
        alloc_id: AllocId,
        entrypoint: &str,
        args: &[String],
    ) -> Result<ProcessHandle, RuntimeError> {
        let prepared = self
            .prepared
            .lock()
            .await
            .get(&alloc_id)
            .cloned()
            .ok_or_else(|| RuntimeError::SpawnFailed {
                alloc_id,
                reason: "prepare() was not called; no scrubbed env available".into(),
            })?;

        let mut cmd = Command::new(entrypoint);
        cmd.args(args);
        cmd.env_clear();
        for (k, v) in prepared.env {
            cmd.env(k, v);
        }
        if let Some(ref dir) = prepared.workdir {
            cmd.current_dir(dir);
        }
        // NOTE: cgroup scope creation happens at the agent layer via
        // CgroupManager trait; the spawned child is moved into the scope
        // by the agent's supervisor loop before exec runs in most kernels.
        // For now, the child runs as a direct subprocess of the agent.

        let child = cmd.spawn().map_err(|e| RuntimeError::SpawnFailed {
            alloc_id,
            reason: format!("tokio::process::Command::spawn failed: {e}"),
        })?;
        let pid = child.id();
        debug!(
            alloc_id = %alloc_id,
            pid = ?pid,
            entrypoint = entrypoint,
            "bare-process workload spawned"
        );
        self.children.lock().await.insert(alloc_id, child);
        Ok(ProcessHandle {
            alloc_id,
            pid,
            container_id: None,
        })
    }

    async fn signal(&self, handle: &ProcessHandle, signal: i32) -> Result<(), RuntimeError> {
        let mut children = self.children.lock().await;
        let child = children
            .get_mut(&handle.alloc_id)
            .ok_or_else(|| RuntimeError::NotFound {
                alloc_id: handle.alloc_id,
            })?;
        let Some(pid) = child.id() else {
            return Err(RuntimeError::SignalFailed {
                alloc_id: handle.alloc_id,
                reason: "child has no PID (already exited)".into(),
            });
        };

        #[cfg(unix)]
        {
            // SAFETY: libc::kill has well-defined behavior for PID 0 or
            // negative PIDs, but we already checked that `pid` came from a
            // live tokio child handle, so it is a positive process ID.
            let rc = unsafe { libc::kill(pid as libc::pid_t, signal as libc::c_int) };
            if rc != 0 {
                let errno = std::io::Error::last_os_error();
                return Err(RuntimeError::SignalFailed {
                    alloc_id: handle.alloc_id,
                    reason: format!("kill({pid}, {signal}) failed: {errno}"),
                });
            }
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            return Err(RuntimeError::SignalFailed {
                alloc_id: handle.alloc_id,
                reason: "signals not supported on this platform".into(),
            });
        }
        Ok(())
    }

    async fn stop(
        &self,
        handle: &ProcessHandle,
        grace_secs: u32,
    ) -> Result<ExitStatus, RuntimeError> {
        // SIGTERM → grace → SIGKILL, matching the other runtimes' contract.
        #[cfg(unix)]
        let _ = self.signal(handle, 15).await; // SIGTERM best-effort

        let grace = std::time::Duration::from_secs(grace_secs as u64);
        let wait_fut = async {
            let mut children = self.children.lock().await;
            if let Some(mut child) = children.remove(&handle.alloc_id) {
                match child.wait().await {
                    Ok(status) => {
                        if let Some(code) = status.code() {
                            return Ok::<ExitStatus, RuntimeError>(ExitStatus::Code(code));
                        }
                        #[cfg(unix)]
                        {
                            use std::os::unix::process::ExitStatusExt;
                            if let Some(sig) = status.signal() {
                                return Ok(ExitStatus::Signal(sig));
                            }
                        }
                        Ok(ExitStatus::Unknown)
                    }
                    Err(e) => Err(RuntimeError::StopFailed {
                        alloc_id: handle.alloc_id,
                        reason: format!("wait() failed: {e}"),
                    }),
                }
            } else {
                Err(RuntimeError::NotFound {
                    alloc_id: handle.alloc_id,
                })
            }
        };

        match tokio::time::timeout(grace, wait_fut).await {
            Ok(r) => r,
            Err(_) => {
                warn!(
                    alloc_id = %handle.alloc_id,
                    grace_secs = grace_secs,
                    "SIGTERM grace period expired; sending SIGKILL"
                );
                let _ = self.signal(handle, 9).await;
                // Try one more short wait.
                let mut children = self.children.lock().await;
                if let Some(mut child) = children.remove(&handle.alloc_id) {
                    match tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
                        .await
                    {
                        Ok(Ok(status)) => {
                            if let Some(code) = status.code() {
                                Ok(ExitStatus::Code(code))
                            } else {
                                Ok(ExitStatus::Unknown)
                            }
                        }
                        _ => Ok(ExitStatus::Unknown),
                    }
                } else {
                    Err(RuntimeError::NotFound {
                        alloc_id: handle.alloc_id,
                    })
                }
            }
        }
    }

    async fn wait(&self, handle: &ProcessHandle) -> Result<ExitStatus, RuntimeError> {
        let mut children = self.children.lock().await;
        let Some(mut child) = children.remove(&handle.alloc_id) else {
            return Err(RuntimeError::NotFound {
                alloc_id: handle.alloc_id,
            });
        };
        match child.wait().await {
            Ok(status) => {
                if let Some(code) = status.code() {
                    Ok(ExitStatus::Code(code))
                } else {
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        if let Some(sig) = status.signal() {
                            return Ok(ExitStatus::Signal(sig));
                        }
                    }
                    Ok(ExitStatus::Unknown)
                }
            }
            Err(e) => Err(RuntimeError::StopFailed {
                alloc_id: handle.alloc_id,
                reason: format!("wait() failed: {e}"),
            }),
        }
    }

    async fn cleanup(&self, alloc_id: AllocId) -> Result<(), RuntimeError> {
        // No mount / image / namespace to tear down. Just drop prepared env.
        self.prepared.lock().await.remove(&alloc_id);
        self.children.lock().await.remove(&alloc_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_list_strips_obvious_secret_vars() {
        assert!(matches_block_list("LATTICE_AGENT_TOKEN", &[]));
        assert!(matches_block_list("VAULT_SECRET_ID", &[]));
        assert!(matches_block_list("MY_API_TOKEN", &[]));
        assert!(matches_block_list("some_secret_value", &[]));
        assert!(matches_block_list("USER_PASSWORD", &[]));
        assert!(matches_block_list("AWS_CREDENTIAL", &[]));

        assert!(!matches_block_list("PATH", &[]));
        assert!(!matches_block_list("HOME", &[]));
        assert!(!matches_block_list("OMP_NUM_THREADS", &[]));
    }

    #[test]
    fn allow_list_covers_hpc_staples() {
        let cfg = BareProcessConfig::default();
        assert!(allow_list_matches("PATH", &cfg));
        assert!(allow_list_matches("HOME", &cfg));
        assert!(allow_list_matches("LD_LIBRARY_PATH", &cfg));
        assert!(allow_list_matches("TMPDIR", &cfg));
        assert!(allow_list_matches("OMP_NUM_THREADS", &cfg));
        assert!(allow_list_matches("OMP_PROC_BIND", &cfg));
        assert!(allow_list_matches("CUDA_VISIBLE_DEVICES", &cfg));
        assert!(allow_list_matches("ROCR_VISIBLE_DEVICES", &cfg));
        assert!(allow_list_matches("PMI_RANK", &cfg));
        assert!(allow_list_matches("MPICH_CXI_AR", &cfg));
        assert!(allow_list_matches("NCCL_DEBUG", &cfg));
        assert!(allow_list_matches("FI_PROVIDER", &cfg));
        assert!(allow_list_matches("CXI_FORK_SAFE", &cfg));
        assert!(allow_list_matches("LC_ALL", &cfg));
        assert!(allow_list_matches("LATTICE_ALLOC_ID", &cfg));

        // Not in allow-list by default
        assert!(!allow_list_matches("RANDOM_VAR", &cfg));
        // SLURM_* opt-in
        assert!(!allow_list_matches("SLURM_JOB_ID", &cfg));

        let mut cfg_slurm = BareProcessConfig::default();
        cfg_slurm.include_slurm_vars = true;
        assert!(allow_list_matches("SLURM_JOB_ID", &cfg_slurm));
    }

    #[tokio::test]
    async fn user_env_var_matching_block_list_is_rejected_at_prologue() {
        let rt = BareProcessRuntime::new();
        let result = rt.build_workload_env(&[("SECRET_KEY".into(), "shhh".into())]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("env_var_in_block_list"));
    }

    #[tokio::test]
    async fn user_env_var_merged_over_agent_env() {
        let rt = BareProcessRuntime::new();
        let result = rt
            .build_workload_env(&[("OMP_NUM_THREADS".into(), "4".into())])
            .unwrap();
        let threads = result.iter().find(|(k, _)| k == "OMP_NUM_THREADS");
        assert!(threads.is_some());
        assert_eq!(threads.unwrap().1, "4");
    }
}
