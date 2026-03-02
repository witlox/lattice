//! Slurm compatibility — parsing `#SBATCH` directives from scripts.
//!
//! Maps Slurm directives to Lattice allocation parameters.

use std::collections::HashMap;
use std::time::Duration;

/// Parsed Slurm directives from a batch script.
#[derive(Debug, Clone, Default)]
pub struct SlurmDirectives {
    pub nodes: Option<u32>,
    pub ntasks: Option<u32>,
    pub ntasks_per_node: Option<u32>,
    pub walltime: Option<Duration>,
    pub partition: Option<String>,
    pub account: Option<String>,
    pub job_name: Option<String>,
    pub output_file: Option<String>,
    pub error_file: Option<String>,
    pub constraint: Option<String>,
    pub gres: Option<String>,
    pub exclusive: bool,
    pub array: Option<String>,
    pub dependency: Option<String>,
    pub qos: Option<String>,
    /// Lattice-specific extensions
    pub uenv: Option<String>,
    pub view: Option<String>,
    /// Directives that were recognized but not supported
    pub warnings: Vec<String>,
    /// The script body after directives
    pub entrypoint: String,
}

/// Parse `#SBATCH` directives from a batch script.
pub fn parse_sbatch_script(content: &str) -> SlurmDirectives {
    let mut directives = SlurmDirectives::default();
    let mut body_lines = Vec::new();
    let mut past_directives = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if !past_directives && trimmed.starts_with("#SBATCH") {
            let args = trimmed.trim_start_matches("#SBATCH").trim();
            parse_directive(args, &mut directives);
        } else if !past_directives
            && (trimmed.starts_with("#!") || trimmed.starts_with('#') || trimmed.is_empty())
        {
            // Skip shebang, comments, blank lines before body
        } else {
            past_directives = true;
            body_lines.push(line);
        }
    }

    directives.entrypoint = body_lines.join("\n").trim().to_string();
    directives
}

fn parse_directive(args: &str, directives: &mut SlurmDirectives) {
    // Handle both --key=value and --key value forms
    let parts: Vec<&str> = args.splitn(2, '=').collect();
    let (key, value) = if parts.len() == 2 {
        (parts[0].trim(), Some(parts[1].trim()))
    } else {
        // Handle --key value (space-separated)
        let space_parts: Vec<&str> = args.splitn(2, ' ').collect();
        if space_parts.len() == 2 {
            (space_parts[0].trim(), Some(space_parts[1].trim()))
        } else {
            (args.trim(), None)
        }
    };

    match key {
        "--nodes" | "-N" => {
            if let Some(v) = value {
                directives.nodes = v.parse().ok();
            }
        }
        "--ntasks" | "-n" => {
            if let Some(v) = value {
                directives.ntasks = v.parse().ok();
            }
        }
        "--ntasks-per-node" => {
            if let Some(v) = value {
                directives.ntasks_per_node = v.parse().ok();
            }
        }
        "--time" | "-t" => {
            if let Some(v) = value {
                directives.walltime = parse_slurm_time(v);
            }
        }
        "--partition" | "-p" => {
            directives.partition = value.map(|v| v.to_string());
        }
        "--account" | "-A" => {
            directives.account = value.map(|v| v.to_string());
        }
        "--job-name" | "-J" => {
            directives.job_name = value.map(|v| v.to_string());
        }
        "--output" | "-o" => {
            directives.output_file = value.map(|v| v.to_string());
        }
        "--error" | "-e" => {
            directives.error_file = value.map(|v| v.to_string());
        }
        "--constraint" | "-C" => {
            directives.constraint = value.map(|v| v.to_string());
        }
        "--gres" => {
            directives.gres = value.map(|v| v.to_string());
        }
        "--exclusive" => {
            directives.exclusive = true;
        }
        "--array" | "-a" => {
            directives.array = value.map(|v| v.to_string());
        }
        "--dependency" | "-d" => {
            directives.dependency = value.map(|v| v.to_string());
        }
        "--qos" | "-q" => {
            directives.qos = value.map(|v| v.to_string());
        }
        "--uenv" => {
            directives.uenv = value.map(|v| v.to_string());
        }
        "--view" => {
            directives.view = value.map(|v| v.to_string());
        }
        "--mem" | "--cpus-per-task" | "--mail-user" | "--mail-type" => {
            directives
                .warnings
                .push(format!("{key} is not supported by Lattice (ignored)"));
        }
        _ => {}
    }
}

/// Parse Slurm time format: `D-HH:MM:SS`, `HH:MM:SS`, `MM:SS`, or `minutes`.
pub fn parse_slurm_time(s: &str) -> Option<Duration> {
    // D-HH:MM:SS
    if let Some((days_str, rest)) = s.split_once('-') {
        let days: u64 = days_str.parse().ok()?;
        let hms = parse_hms(rest)?;
        return Some(Duration::from_secs(days * 86400 + hms));
    }

    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        3 => {
            // HH:MM:SS
            let h: u64 = parts[0].parse().ok()?;
            let m: u64 = parts[1].parse().ok()?;
            let sec: u64 = parts[2].parse().ok()?;
            Some(Duration::from_secs(h * 3600 + m * 60 + sec))
        }
        2 => {
            // MM:SS or HH:MM (ambiguous — treat as HH:MM for Slurm compat)
            let h: u64 = parts[0].parse().ok()?;
            let m: u64 = parts[1].parse().ok()?;
            Some(Duration::from_secs(h * 3600 + m * 60))
        }
        1 => {
            // Just minutes
            let m: u64 = parts[0].parse().ok()?;
            Some(Duration::from_secs(m * 60))
        }
        _ => None,
    }
}

fn parse_hms(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        3 => {
            let h: u64 = parts[0].parse().ok()?;
            let m: u64 = parts[1].parse().ok()?;
            let sec: u64 = parts[2].parse().ok()?;
            Some(h * 3600 + m * 60 + sec)
        }
        2 => {
            let m: u64 = parts[0].parse().ok()?;
            let sec: u64 = parts[1].parse().ok()?;
            Some(m * 60 + sec)
        }
        _ => None,
    }
}

/// Parse `--array=0-99%20` into (start, end, step, max_concurrent).
pub fn parse_array_spec(spec: &str) -> Option<(u32, u32, u32, u32)> {
    let (range_part, max_concurrent) = if let Some((r, m)) = spec.split_once('%') {
        (r, m.parse().unwrap_or(0))
    } else {
        (spec, 0u32)
    };

    let (start, end, step) = if let Some((range, step_str)) = range_part.split_once(':') {
        let s: u32 = step_str.parse().ok()?;
        let (a, b) = parse_range(range)?;
        (a, b, s)
    } else {
        let (a, b) = parse_range(range_part)?;
        (a, b, 1)
    };

    Some((start, end, step, max_concurrent))
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    if let Some((a, b)) = s.split_once('-') {
        Some((a.parse().ok()?, b.parse().ok()?))
    } else {
        let v: u32 = s.parse().ok()?;
        Some((v, v))
    }
}

/// Parse Slurm dependency string: `afterok:123,afternotok:456`.
pub fn parse_dependency(spec: &str) -> Vec<(String, String)> {
    let mut deps = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if let Some((condition, ref_id)) = part.split_once(':') {
            let lattice_condition = match condition {
                "afterok" | "after" => "success",
                "afternotok" => "failure",
                "afterany" => "any",
                "aftercorr" => "corresponding",
                _ => "any",
            };
            deps.push((ref_id.to_string(), lattice_condition.to_string()));
        }
    }
    deps
}

/// Parse `--gres=gpu:N` into gpu count.
pub fn parse_gres_gpu(spec: &str) -> Option<u32> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() >= 2 && parts[0] == "gpu" {
        parts.last()?.parse().ok()
    } else {
        None
    }
}

/// Map Slurm partition to Lattice vCluster.
pub fn partition_to_vcluster(partition: &str, mapping: &HashMap<String, String>) -> String {
    mapping
        .get(partition)
        .cloned()
        .unwrap_or_else(|| partition.to_string())
}

/// Map Slurm QoS to Lattice priority class.
pub fn qos_to_priority_class(qos: &str) -> u32 {
    match qos {
        "debug" | "interactive" => 1,
        "normal" | "default" => 5,
        "high" | "premium" => 8,
        "urgent" | "critical" => 10,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_sbatch_script() {
        let script = r#"#!/bin/bash
#SBATCH --nodes=4
#SBATCH --time=02:00:00
#SBATCH --account=physics
#SBATCH --job-name=training

python train.py
"#;
        let d = parse_sbatch_script(script);
        assert_eq!(d.nodes, Some(4));
        assert_eq!(d.walltime, Some(Duration::from_secs(7200)));
        assert_eq!(d.account.as_deref(), Some("physics"));
        assert_eq!(d.job_name.as_deref(), Some("training"));
        assert_eq!(d.entrypoint, "python train.py");
    }

    #[test]
    fn parse_gpu_and_constraint() {
        let script = r#"#!/bin/bash
#SBATCH --nodes=2
#SBATCH --gres=gpu:4
#SBATCH --constraint=nvme_scratch
#SBATCH --time=1:00:00

./run.sh
"#;
        let d = parse_sbatch_script(script);
        assert_eq!(d.nodes, Some(2));
        assert_eq!(d.gres.as_deref(), Some("gpu:4"));
        assert_eq!(d.constraint.as_deref(), Some("nvme_scratch"));
    }

    #[test]
    fn parse_array_and_dependency() {
        let script = r#"#!/bin/bash
#SBATCH --array=0-99%20
#SBATCH --dependency=afterok:12345

echo "Task $SLURM_ARRAY_TASK_ID"
"#;
        let d = parse_sbatch_script(script);
        assert_eq!(d.array.as_deref(), Some("0-99%20"));
        assert_eq!(d.dependency.as_deref(), Some("afterok:12345"));
    }

    #[test]
    fn parse_lattice_extensions() {
        let script = r#"#!/bin/bash
#SBATCH --uenv=pytorch:latest
#SBATCH --view=develop

python train.py
"#;
        let d = parse_sbatch_script(script);
        assert_eq!(d.uenv.as_deref(), Some("pytorch:latest"));
        assert_eq!(d.view.as_deref(), Some("develop"));
    }

    #[test]
    fn unsupported_directives_warned() {
        let script = r#"#!/bin/bash
#SBATCH --mem=64G
#SBATCH --cpus-per-task=8
#SBATCH --nodes=1

./run.sh
"#;
        let d = parse_sbatch_script(script);
        assert_eq!(d.warnings.len(), 2);
        assert!(d.warnings[0].contains("--mem"));
        assert!(d.warnings[1].contains("--cpus-per-task"));
    }

    #[test]
    fn parse_short_flags() {
        let script = r#"#!/bin/bash
#SBATCH -N 8
#SBATCH -t 72:00:00
#SBATCH -A ml-team
#SBATCH -J big-run

./train.sh
"#;
        let d = parse_sbatch_script(script);
        assert_eq!(d.nodes, Some(8));
        assert_eq!(d.walltime, Some(Duration::from_secs(72 * 3600)));
        assert_eq!(d.account.as_deref(), Some("ml-team"));
        assert_eq!(d.job_name.as_deref(), Some("big-run"));
    }

    #[test]
    fn slurm_time_formats() {
        // HH:MM:SS
        assert_eq!(
            parse_slurm_time("02:00:00"),
            Some(Duration::from_secs(7200))
        );
        // D-HH:MM:SS
        assert_eq!(
            parse_slurm_time("1-12:00:00"),
            Some(Duration::from_secs(129600))
        );
        // HH:MM
        assert_eq!(parse_slurm_time("72:00"), Some(Duration::from_secs(259200)));
        // Minutes
        assert_eq!(parse_slurm_time("30"), Some(Duration::from_secs(1800)));
    }

    #[test]
    fn array_spec_parsing() {
        assert_eq!(parse_array_spec("0-99%20"), Some((0, 99, 1, 20)));
        assert_eq!(parse_array_spec("0-99"), Some((0, 99, 1, 0)));
        assert_eq!(parse_array_spec("1-10:2"), Some((1, 10, 2, 0)));
        assert_eq!(parse_array_spec("5"), Some((5, 5, 1, 0)));
    }

    #[test]
    fn dependency_parsing() {
        let deps = parse_dependency("afterok:123,afternotok:456");
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0], ("123".to_string(), "success".to_string()));
        assert_eq!(deps[1], ("456".to_string(), "failure".to_string()));
    }

    #[test]
    fn dependency_any() {
        let deps = parse_dependency("afterany:789");
        assert_eq!(deps[0], ("789".to_string(), "any".to_string()));
    }

    #[test]
    fn gres_gpu_parsing() {
        assert_eq!(parse_gres_gpu("gpu:4"), Some(4));
        assert_eq!(parse_gres_gpu("gpu:A100:2"), Some(2));
        assert_eq!(parse_gres_gpu(""), None);
    }

    #[test]
    fn partition_mapping() {
        let mut mapping = HashMap::new();
        mapping.insert("batch".to_string(), "hpc-batch".to_string());
        mapping.insert("gpu".to_string(), "gpu-cluster".to_string());

        assert_eq!(partition_to_vcluster("batch", &mapping), "hpc-batch");
        assert_eq!(partition_to_vcluster("gpu", &mapping), "gpu-cluster");
        // Unknown partition → passthrough
        assert_eq!(partition_to_vcluster("custom", &mapping), "custom");
    }

    #[test]
    fn qos_priority_mapping() {
        assert_eq!(qos_to_priority_class("debug"), 1);
        assert_eq!(qos_to_priority_class("normal"), 5);
        assert_eq!(qos_to_priority_class("high"), 8);
        assert_eq!(qos_to_priority_class("urgent"), 10);
        assert_eq!(qos_to_priority_class("unknown"), 5);
    }

    #[test]
    fn empty_script() {
        let d = parse_sbatch_script("");
        assert!(d.nodes.is_none());
        assert!(d.entrypoint.is_empty());
    }

    #[test]
    fn script_with_only_body() {
        let script = "echo hello\necho world\n";
        let d = parse_sbatch_script(script);
        assert_eq!(d.entrypoint, "echo hello\necho world");
    }

    #[test]
    fn exclusive_flag() {
        let script = "#SBATCH --exclusive\n./run.sh\n";
        let d = parse_sbatch_script(script);
        assert!(d.exclusive);
    }
}
