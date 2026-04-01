//! `lattice submit` — submit allocations or batch scripts.

use std::collections::{HashMap, HashSet};

use clap::Args;
use serde::Deserialize;

use crate::client::{ClientConfig, LatticeGrpcClient};
use crate::convert::build_submit_request;
use crate::output::OutputFormat;

/// Arguments for the submit command.
#[derive(Args, Debug)]
pub struct SubmitArgs {
    /// Script file to submit (with #SBATCH directives)
    pub script: Option<String>,

    /// Number of nodes
    #[arg(long)]
    pub nodes: Option<u32>,

    /// Walltime (Slurm format: HH:MM:SS, D-HH:MM:SS, or minutes)
    #[arg(long)]
    pub walltime: Option<String>,

    /// uenv image spec (repeatable, format: name/version:tag)
    #[arg(long = "uenv", value_name = "SPEC")]
    pub uenvs: Vec<String>,

    /// uenv view to activate (repeatable, validated after resolution)
    #[arg(long = "view", value_name = "NAME")]
    pub views: Vec<String>,

    /// OCI container image reference
    #[arg(long)]
    pub image: Option<String>,

    /// EDF TOML file (parsed as ContainerSpec fields)
    #[arg(long, value_name = "PATH")]
    pub edf: Option<String>,

    /// Additional bind mount (repeatable, format: src:dst[:opts])
    #[arg(long = "mount", value_name = "SRC:DST[:OPTS]")]
    pub mounts: Vec<String>,

    /// CDI device spec (repeatable, e.g., nvidia.com/gpu=all)
    #[arg(long = "device", value_name = "SPEC")]
    pub devices: Vec<String>,

    /// Defer image resolution to scheduling time
    #[arg(long)]
    pub resolve_on_schedule: bool,

    /// Task group spec (e.g., 0-99%20)
    #[arg(long)]
    pub task_group: Option<String>,

    /// Dependency spec (e.g., afterok:123)
    #[arg(long)]
    pub depends_on: Option<String>,

    /// Priority class (0-10)
    #[arg(long)]
    pub priority: Option<u32>,

    /// Project name
    #[arg(long)]
    pub project: Option<String>,

    /// Inline command (after --)
    #[arg(last = true)]
    pub command: Vec<String>,
}

impl SubmitArgs {
    /// Build a submission description from CLI args + optional script parsing.
    pub fn to_submission(&self, tenant: Option<&str>) -> SubmitDescription {
        let mut desc = SubmitDescription::default();

        // If a script file is specified, the caller should read it and
        // call `merge_sbatch_directives` to fold in the directives.
        if let Some(ref script) = self.script {
            desc.script_path = Some(script.clone());
        }

        // CLI args override script directives
        if let Some(n) = self.nodes {
            desc.nodes = Some(n);
        }
        if let Some(ref w) = self.walltime {
            desc.walltime = crate::compat::parse_slurm_time(w);
        }
        // Backwards-compat: single uenv → first element; multiple → repeatable
        if !self.uenvs.is_empty() {
            desc.uenvs = self.uenvs.clone();
        }
        if !self.views.is_empty() {
            desc.views = self.views.clone();
        }
        if let Some(ref img) = self.image {
            desc.image = Some(img.clone());
        }
        if let Some(ref edf) = self.edf {
            desc.edf = Some(edf.clone());
        }
        if !self.mounts.is_empty() {
            desc.mounts = self.mounts.clone();
        }
        if !self.devices.is_empty() {
            desc.devices = self.devices.clone();
        }
        desc.resolve_on_schedule = self.resolve_on_schedule;
        if let Some(ref tg) = self.task_group {
            desc.task_group = crate::compat::parse_array_spec(tg);
        }
        if let Some(ref dep) = self.depends_on {
            desc.dependencies = crate::compat::parse_dependency(dep);
        }
        if let Some(p) = self.priority {
            desc.priority_class = Some(p);
        }
        if let Some(ref p) = self.project {
            desc.project = Some(p.clone());
        }
        if !self.command.is_empty() {
            desc.entrypoint = Some(self.command.join(" "));
        }
        if let Some(t) = tenant {
            desc.tenant = Some(t.to_string());
        }

        desc
    }
}

// ─── EDF Parsing ───────────────────────────────────────────

/// Maximum depth for EDF base_environment resolution.
const EDF_MAX_DEPTH: usize = 10;

/// Parsed EDF (Environment Definition File) specification.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EdfSpec {
    pub image: Option<String>,
    #[serde(default)]
    pub base_environment: Vec<String>,
    #[serde(default)]
    pub mounts: Vec<String>,
    #[serde(default)]
    pub devices: Vec<String>,
    pub workdir: Option<String>,
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub entrypoint: bool,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub annotations: HashMap<String, String>,
}

/// Parse an EDF TOML file from disk.
pub fn parse_edf_file(path: &str) -> Result<EdfSpec, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read EDF file {path}: {e}"))?;
    parse_edf_toml(&content)
}

/// Parse EDF TOML content.
pub fn parse_edf_toml(content: &str) -> Result<EdfSpec, String> {
    toml::from_str(content).map_err(|e| format!("invalid EDF TOML: {e}"))
}

/// Resolve EDF base_environment chain by searching known paths.
/// Returns a merged EdfSpec with all base environments applied (additive).
/// Missing base files produce warnings but do not fail.
pub fn resolve_edf_chain(spec: &EdfSpec, quiet: bool) -> EdfSpec {
    let search_paths = edf_search_paths();
    let mut visited = HashSet::new();
    let mut merged = spec.clone();
    resolve_edf_bases(&mut merged, &search_paths, &mut visited, 0, quiet);
    merged
}

/// Search paths for EDF files (in priority order).
fn edf_search_paths() -> Vec<String> {
    let mut paths = Vec::new();

    // 1. EDF_PATH env var (colon-separated)
    if let Ok(edf_path) = std::env::var("EDF_PATH") {
        for p in edf_path.split(':') {
            if !p.is_empty() {
                paths.push(p.to_string());
            }
        }
    }

    // 2. $HOME/.edf/
    if let Some(home) = dirs::home_dir() {
        paths.push(format!("{}/.edf", home.display()));
    }

    // 3. System paths
    paths.push("/etc/lattice/edf".to_string());
    paths.push("/etc/sarus-suite/edf".to_string());

    paths
}

/// Recursively resolve base environments, merging additively.
fn resolve_edf_bases(
    spec: &mut EdfSpec,
    search_paths: &[String],
    visited: &mut HashSet<String>,
    depth: usize,
    quiet: bool,
) {
    if depth >= EDF_MAX_DEPTH {
        if !quiet {
            eprintln!(
                "Warning: EDF base_environment chain exceeded depth limit ({EDF_MAX_DEPTH}), stopping resolution"
            );
        }
        return;
    }

    // Drain base_environment names so we don't re-process them
    let bases: Vec<String> = spec.base_environment.drain(..).collect();

    for base_name in &bases {
        if visited.contains(base_name) {
            if !quiet {
                eprintln!("Warning: EDF cycle detected for '{base_name}', skipping");
            }
            continue;
        }
        visited.insert(base_name.clone());

        match find_edf_file(base_name, search_paths) {
            Some(path) => match parse_edf_file(&path) {
                Ok(mut base_spec) => {
                    // Recurse first (base of base)
                    resolve_edf_bases(&mut base_spec, search_paths, visited, depth + 1, quiet);
                    // Merge base into current (additive: mounts, devices, env are unioned)
                    merge_edf_base(spec, &base_spec);
                }
                Err(e) => {
                    if !quiet {
                        eprintln!("Warning: failed to parse base EDF '{base_name}' at {path}: {e}");
                    }
                }
            },
            None => {
                if !quiet {
                    eprintln!("Warning: base environment '{base_name}' not found, skipping");
                }
            }
        }
    }

    // Restore original base names (for proto serialization)
    spec.base_environment = bases;
}

/// Search for a named EDF file in the search paths.
fn find_edf_file(name: &str, search_paths: &[String]) -> Option<String> {
    for dir in search_paths {
        let path = format!("{dir}/{name}.toml");
        if std::path::Path::new(&path).exists() {
            return Some(path);
        }
    }
    None
}

/// Merge a base EDF into the current spec (additive).
/// User values override base for scalar fields; collections are unioned.
fn merge_edf_base(current: &mut EdfSpec, base: &EdfSpec) {
    // image: user overrides base
    if current.image.is_none() {
        current.image = base.image.clone();
    }

    // Additive collections: union (base first, then current)
    let mut merged_mounts: Vec<String> = base.mounts.clone();
    for m in &current.mounts {
        if !merged_mounts.contains(m) {
            merged_mounts.push(m.clone());
        }
    }
    current.mounts = merged_mounts;

    let mut merged_devices: Vec<String> = base.devices.clone();
    for d in &current.devices {
        if !merged_devices.contains(d) {
            merged_devices.push(d.clone());
        }
    }
    current.devices = merged_devices;

    // Env: base provides defaults, current overrides
    let mut merged_env = base.env.clone();
    merged_env.extend(current.env.clone());
    current.env = merged_env;

    // Annotations: same as env
    let mut merged_ann = base.annotations.clone();
    merged_ann.extend(current.annotations.clone());
    current.annotations = merged_ann;

    // workdir: user overrides base
    if current.workdir.is_none() {
        current.workdir = base.workdir.clone();
    }

    // writable: user true overrides base
    if base.writable && !current.writable {
        current.writable = base.writable;
    }
}

/// Fully-resolved submission description ready for API call.
#[derive(Debug, Clone, Default)]
pub struct SubmitDescription {
    pub script_path: Option<String>,
    pub tenant: Option<String>,
    pub project: Option<String>,
    pub entrypoint: Option<String>,
    pub nodes: Option<u32>,
    pub walltime: Option<std::time::Duration>,
    pub uenvs: Vec<String>,
    pub views: Vec<String>,
    pub image: Option<String>,
    pub edf: Option<String>,
    pub mounts: Vec<String>,
    pub devices: Vec<String>,
    pub resolve_on_schedule: bool,
    pub task_group: Option<(u32, u32, u32, u32)>,
    pub dependencies: Vec<(String, String)>,
    pub priority_class: Option<u32>,
    /// EDF-derived fields (populated by apply_edf_to_description).
    pub edf_base_environments: Vec<String>,
    pub edf_env: HashMap<String, String>,
    pub edf_annotations: HashMap<String, String>,
    pub edf_workdir: Option<String>,
    pub edf_writable: bool,
}

impl SubmitDescription {
    /// Merge Slurm directives from a parsed script (script values used
    /// only when the corresponding CLI arg was not explicitly provided).
    pub fn merge_sbatch_directives(&mut self, directives: &crate::compat::SlurmDirectives) {
        if self.nodes.is_none() {
            self.nodes = directives.nodes;
        }
        if self.walltime.is_none() {
            self.walltime = directives.walltime;
        }
        if self.tenant.is_none() {
            self.tenant = directives.account.clone();
        }
        if self.entrypoint.is_none() && !directives.entrypoint.is_empty() {
            self.entrypoint = Some(directives.entrypoint.clone());
        }
        if self.uenvs.is_empty() {
            if let Some(ref u) = directives.uenv {
                self.uenvs = vec![u.clone()];
            }
        }
        if self.views.is_empty() {
            if let Some(ref v) = directives.view {
                self.views = vec![v.clone()];
            }
        }
        if self.task_group.is_none() {
            if let Some(ref a) = directives.array {
                self.task_group = crate::compat::parse_array_spec(a);
            }
        }
        if self.dependencies.is_empty() {
            if let Some(ref d) = directives.dependency {
                self.dependencies = crate::compat::parse_dependency(d);
            }
        }
        if self.priority_class.is_none() {
            if let Some(ref q) = directives.qos {
                self.priority_class = Some(crate::compat::qos_to_priority_class(q));
            }
        }
        if self.project.is_none() {
            self.project = directives.job_name.clone();
        }
    }
}

/// Apply parsed EDF fields into the submit description.
/// EDF values supplement CLI args (CLI args take precedence where applicable).
fn apply_edf_to_description(desc: &mut SubmitDescription, edf: &EdfSpec) {
    // Image from EDF → OCI image (if not already set by CLI)
    if desc.image.is_none() {
        desc.image = edf.image.clone();
    }

    // Mounts: merge EDF mounts with CLI mounts
    for m in &edf.mounts {
        if !desc.mounts.contains(m) {
            desc.mounts.push(m.clone());
        }
    }

    // Devices: merge
    for d in &edf.devices {
        if !desc.devices.contains(d) {
            desc.devices.push(d.clone());
        }
    }

    // base_environments: store for proto
    desc.edf_base_environments = edf.base_environment.clone();

    // env: store for proto (as Set operations)
    desc.edf_env = edf.env.clone();

    // annotations: store for proto
    desc.edf_annotations = edf.annotations.clone();

    // workdir
    desc.edf_workdir = edf.workdir.clone();

    // writable
    desc.edf_writable = edf.writable;
}

/// Execute the submit command: build a SubmitRequest and send it via gRPC.
pub async fn execute(
    args: &SubmitArgs,
    client: &mut LatticeGrpcClient,
    config: &ClientConfig,
    format: OutputFormat,
    quiet: bool,
) -> anyhow::Result<()> {
    let mut desc = args.to_submission(config.tenant.as_deref());

    if let Some(ref path) = desc.script_path {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read script {path}: {e}"))?;
        let directives = crate::compat::parse_sbatch_script(&content);
        for warn in &directives.warnings {
            if !quiet {
                eprintln!("Warning: {warn}");
            }
        }
        desc.merge_sbatch_directives(&directives);
    }

    // Parse EDF if specified
    if let Some(ref edf_path) = desc.edf {
        match parse_edf_file(edf_path) {
            Ok(raw_edf) => {
                let edf = resolve_edf_chain(&raw_edf, quiet);
                apply_edf_to_description(&mut desc, &edf);
            }
            Err(e) => {
                return Err(anyhow::anyhow!("{e}"));
            }
        }
    }

    let req = build_submit_request(&desc, &config.user, config.vcluster.as_deref());
    let resp = client.submit(req).await?;

    match format {
        OutputFormat::Json => {
            let json = serde_json::json!({
                "allocation_ids": resp.allocation_ids,
                "dag_id": if resp.dag_id.is_empty() { None } else { Some(&resp.dag_id) },
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        _ => {
            if !resp.dag_id.is_empty() {
                println!("Submitted DAG: {}", resp.dag_id);
            }
            for id in &resp.allocation_ids {
                println!("Submitted allocation: {id}");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat;

    #[test]
    fn submit_args_to_description() {
        let args = SubmitArgs {
            script: None,
            nodes: Some(4),
            walltime: Some("2:00:00".to_string()),
            uenvs: vec!["pytorch:latest".to_string()],
            views: vec![],
            image: None,
            edf: None,
            mounts: vec![],
            devices: vec![],
            resolve_on_schedule: false,
            task_group: None,
            depends_on: None,
            priority: Some(8),
            project: Some("training".to_string()),
            command: vec!["python".to_string(), "train.py".to_string()],
        };

        let desc = args.to_submission(Some("physics"));
        assert_eq!(desc.nodes, Some(4));
        assert_eq!(desc.walltime, Some(std::time::Duration::from_secs(7200)));
        assert_eq!(desc.uenvs, vec!["pytorch:latest"]);
        assert_eq!(desc.tenant.as_deref(), Some("physics"));
        assert_eq!(desc.entrypoint.as_deref(), Some("python train.py"));
        assert_eq!(desc.priority_class, Some(8));
    }

    #[test]
    fn merge_sbatch_directives_fills_gaps() {
        let args = SubmitArgs {
            script: Some("train.sh".to_string()),
            nodes: None,    // Will come from script
            walltime: None, // Will come from script
            uenvs: vec![],
            views: vec![],
            image: None,
            edf: None,
            mounts: vec![],
            devices: vec![],
            resolve_on_schedule: false,
            task_group: None,
            depends_on: None,
            priority: None,
            project: None,
            command: vec![],
        };

        let mut desc = args.to_submission(None);

        let script = r#"#!/bin/bash
#SBATCH --nodes=8
#SBATCH --time=72:00:00
#SBATCH --account=ml-team
#SBATCH --job-name=big-training

torchrun --nproc_per_node=4 train.py
"#;
        let directives = compat::parse_sbatch_script(script);
        desc.merge_sbatch_directives(&directives);

        assert_eq!(desc.nodes, Some(8));
        assert_eq!(
            desc.walltime,
            Some(std::time::Duration::from_secs(72 * 3600))
        );
        assert_eq!(desc.tenant.as_deref(), Some("ml-team"));
        assert_eq!(desc.project.as_deref(), Some("big-training"));
        assert!(desc.entrypoint.as_deref().unwrap().contains("torchrun"));
    }

    #[test]
    fn cli_args_override_script() {
        let args = SubmitArgs {
            script: Some("train.sh".to_string()),
            nodes: Some(16), // CLI override
            walltime: None,
            uenvs: vec![],
            views: vec![],
            image: None,
            edf: None,
            mounts: vec![],
            devices: vec![],
            resolve_on_schedule: false,
            task_group: None,
            depends_on: None,
            priority: None,
            project: None,
            command: vec![],
        };

        let mut desc = args.to_submission(None);

        let script = r#"#!/bin/bash
#SBATCH --nodes=8
#SBATCH --time=72:00:00

./run.sh
"#;
        let directives = compat::parse_sbatch_script(script);
        desc.merge_sbatch_directives(&directives);

        // CLI --nodes=16 wins over script #SBATCH --nodes=8
        assert_eq!(desc.nodes, Some(16));
        // Walltime comes from script since CLI didn't specify
        assert_eq!(
            desc.walltime,
            Some(std::time::Duration::from_secs(72 * 3600))
        );
    }

    #[test]
    fn submit_with_task_group() {
        let args = SubmitArgs {
            script: None,
            nodes: Some(1),
            walltime: None,
            uenvs: vec![],
            views: vec![],
            image: None,
            edf: None,
            mounts: vec![],
            devices: vec![],
            resolve_on_schedule: false,
            task_group: Some("0-99%20".to_string()),
            depends_on: None,
            priority: None,
            project: None,
            command: vec!["./run.sh".to_string()],
        };

        let desc = args.to_submission(Some("physics"));
        assert_eq!(desc.task_group, Some((0, 99, 1, 20)));
    }

    #[test]
    fn submit_with_dependencies() {
        let args = SubmitArgs {
            script: None,
            nodes: Some(1),
            walltime: None,
            uenvs: vec![],
            views: vec![],
            image: None,
            edf: None,
            mounts: vec![],
            devices: vec![],
            resolve_on_schedule: false,
            task_group: None,
            depends_on: Some("afterok:123,afternotok:456".to_string()),
            priority: None,
            project: None,
            command: vec!["./eval.sh".to_string()],
        };

        let desc = args.to_submission(None);
        assert_eq!(desc.dependencies.len(), 2);
        assert_eq!(desc.dependencies[0].1, "success");
        assert_eq!(desc.dependencies[1].1, "failure");
    }

    // ─── EDF Parsing Tests ─────────────────────────────────────

    #[test]
    fn parse_edf_toml_valid() {
        let toml = r#"
image = "nvcr.io/nvidia/pytorch:24.01"
base_environment = ["hpc-base"]
mounts = ["/data/input:/mnt/input:ro", "/scratch:/scratch:rw"]
devices = ["nvidia.com/gpu=all"]
workdir = "/workspace"
writable = true

[env]
CUDA_VISIBLE_DEVICES = "0,1,2,3"
OMP_NUM_THREADS = "8"

[annotations]
purpose = "training"
"#;
        let edf = parse_edf_toml(toml).unwrap();
        assert_eq!(edf.image.as_deref(), Some("nvcr.io/nvidia/pytorch:24.01"));
        assert_eq!(edf.base_environment, vec!["hpc-base"]);
        assert_eq!(edf.mounts.len(), 2);
        assert_eq!(edf.devices, vec!["nvidia.com/gpu=all"]);
        assert_eq!(edf.workdir.as_deref(), Some("/workspace"));
        assert!(edf.writable);
        assert_eq!(edf.env.get("CUDA_VISIBLE_DEVICES").unwrap(), "0,1,2,3");
        assert_eq!(edf.annotations.get("purpose").unwrap(), "training");
    }

    #[test]
    fn parse_edf_toml_minimal() {
        let toml = "";
        let edf = parse_edf_toml(toml).unwrap();
        assert!(edf.image.is_none());
        assert!(edf.base_environment.is_empty());
        assert!(edf.mounts.is_empty());
        assert!(edf.devices.is_empty());
        assert!(!edf.writable);
        assert!(edf.env.is_empty());
    }

    #[test]
    fn parse_edf_toml_invalid() {
        let toml = "this is not [[ valid toml {{{";
        let result = parse_edf_toml(toml);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid EDF TOML"));
    }

    #[test]
    fn parse_edf_file_missing() {
        let result = parse_edf_file("/nonexistent/path/edf.toml");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to read EDF file"));
    }

    #[test]
    fn parse_edf_file_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(
            &path,
            r#"
image = "pytorch:latest"
devices = ["nvidia.com/gpu=all"]
"#,
        )
        .unwrap();

        let edf = parse_edf_file(path.to_str().unwrap()).unwrap();
        assert_eq!(edf.image.as_deref(), Some("pytorch:latest"));
        assert_eq!(edf.devices, vec!["nvidia.com/gpu=all"]);
    }

    #[test]
    fn resolve_edf_chain_missing_base_warns_but_continues() {
        let edf = EdfSpec {
            image: Some("pytorch:latest".to_string()),
            base_environment: vec!["nonexistent-base".to_string()],
            devices: vec!["nvidia.com/gpu=all".to_string()],
            ..Default::default()
        };

        // Should not panic; base_environment remains in the list
        let resolved = resolve_edf_chain(&edf, true);
        assert_eq!(resolved.image.as_deref(), Some("pytorch:latest"));
        assert_eq!(resolved.devices, vec!["nvidia.com/gpu=all"]);
        assert_eq!(resolved.base_environment, vec!["nonexistent-base"]);
    }

    #[test]
    fn resolve_edf_chain_with_base_file() {
        let dir = tempfile::tempdir().unwrap();

        // Write a base EDF
        std::fs::write(
            dir.path().join("hpc-base.toml"),
            r#"
mounts = ["/opt/hpc:/opt/hpc:ro"]
devices = ["nvidia.com/gpu=all"]

[env]
HPC_BASE = "true"
"#,
        )
        .unwrap();

        // Set EDF_PATH to our temp dir
        std::env::set_var("EDF_PATH", dir.path().to_str().unwrap());

        let edf = EdfSpec {
            image: Some("pytorch:latest".to_string()),
            base_environment: vec!["hpc-base".to_string()],
            mounts: vec!["/data:/data:rw".to_string()],
            ..Default::default()
        };

        let resolved = resolve_edf_chain(&edf, true);
        assert_eq!(resolved.image.as_deref(), Some("pytorch:latest"));
        // Should have merged mounts from base + current
        assert!(resolved
            .mounts
            .contains(&"/opt/hpc:/opt/hpc:ro".to_string()));
        assert!(resolved.mounts.contains(&"/data:/data:rw".to_string()));
        // Should have base env vars
        assert_eq!(resolved.env.get("HPC_BASE").unwrap(), "true");
        // Devices from base
        assert!(resolved.devices.contains(&"nvidia.com/gpu=all".to_string()));

        std::env::remove_var("EDF_PATH");
    }

    #[test]
    fn apply_edf_to_description_wires_fields() {
        let mut desc = SubmitDescription::default();
        let edf = EdfSpec {
            image: Some("pytorch:latest".to_string()),
            base_environment: vec!["hpc-base".to_string()],
            mounts: vec!["/data:/mnt:ro".to_string()],
            devices: vec!["nvidia.com/gpu=all".to_string()],
            workdir: Some("/workspace".to_string()),
            writable: true,
            env: HashMap::from([("FOO".to_string(), "bar".to_string())]),
            annotations: HashMap::from([("key".to_string(), "value".to_string())]),
            entrypoint: false,
        };

        apply_edf_to_description(&mut desc, &edf);

        assert_eq!(desc.image.as_deref(), Some("pytorch:latest"));
        assert_eq!(desc.mounts, vec!["/data:/mnt:ro"]);
        assert_eq!(desc.devices, vec!["nvidia.com/gpu=all"]);
        assert_eq!(desc.edf_base_environments, vec!["hpc-base"]);
        assert_eq!(desc.edf_env.get("FOO").unwrap(), "bar");
        assert_eq!(desc.edf_annotations.get("key").unwrap(), "value");
        assert_eq!(desc.edf_workdir.as_deref(), Some("/workspace"));
        assert!(desc.edf_writable);
    }

    #[test]
    fn apply_edf_cli_image_overrides_edf() {
        let mut desc = SubmitDescription {
            image: Some("cli-image:v1".to_string()),
            ..Default::default()
        };
        let edf = EdfSpec {
            image: Some("edf-image:v2".to_string()),
            ..Default::default()
        };

        apply_edf_to_description(&mut desc, &edf);
        // CLI image should win
        assert_eq!(desc.image.as_deref(), Some("cli-image:v1"));
    }

    #[test]
    fn merge_edf_base_additive_collections() {
        let mut current = EdfSpec {
            mounts: vec!["/user:/user:rw".to_string()],
            devices: vec!["nvidia.com/gpu=0".to_string()],
            env: HashMap::from([("USER_VAR".to_string(), "user_val".to_string())]),
            ..Default::default()
        };
        let base = EdfSpec {
            mounts: vec!["/base:/base:ro".to_string()],
            devices: vec!["nvidia.com/gpu=all".to_string()],
            env: HashMap::from([
                ("BASE_VAR".to_string(), "base_val".to_string()),
                ("USER_VAR".to_string(), "base_override".to_string()),
            ]),
            workdir: Some("/base/work".to_string()),
            ..Default::default()
        };

        merge_edf_base(&mut current, &base);

        // Mounts: base first, then current
        assert_eq!(current.mounts[0], "/base:/base:ro");
        assert_eq!(current.mounts[1], "/user:/user:rw");

        // Devices: unioned
        assert!(current.devices.contains(&"nvidia.com/gpu=all".to_string()));
        assert!(current.devices.contains(&"nvidia.com/gpu=0".to_string()));

        // Env: user overrides base for same key
        assert_eq!(current.env.get("USER_VAR").unwrap(), "user_val");
        assert_eq!(current.env.get("BASE_VAR").unwrap(), "base_val");

        // workdir: user None → inherits from base
        assert_eq!(current.workdir.as_deref(), Some("/base/work"));
    }
}
