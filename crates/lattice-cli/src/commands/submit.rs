//! `lattice submit` — submit allocations or batch scripts.

use clap::Args;

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
}
