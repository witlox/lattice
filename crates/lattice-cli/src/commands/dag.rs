//! `lattice dag` — DAG workflow management.
//!
//! Submit, monitor, and cancel DAG workflows (multi-stage pipelines).
//! See docs/architecture/dag-scheduling.md.

use std::collections::{HashMap, HashSet, VecDeque};

use clap::{Args, Subcommand};
use serde::Deserialize;

use crate::client::{ClientConfig, LatticeGrpcClient};
use crate::convert::allocation_status_to_info;
use crate::output::OutputFormat;

use lattice_common::proto::lattice::v1 as pb;

/// Arguments for the dag command.
#[derive(Args, Debug)]
pub struct DagArgs {
    #[command(subcommand)]
    pub command: DagCommand,
}

#[derive(Subcommand, Debug)]
pub enum DagCommand {
    /// Submit a DAG workflow from a YAML file
    Submit(DagSubmitArgs),
    /// Show DAG status and stage progress
    Status(DagStatusArgs),
    /// Cancel a DAG workflow
    Cancel(DagCancelArgs),
}

/// Arguments for submitting a DAG.
#[derive(Args, Debug)]
pub struct DagSubmitArgs {
    /// Path to DAG definition file (YAML)
    pub file: String,

    /// Override tenant
    #[arg(long)]
    pub tenant: Option<String>,
}

/// Arguments for checking DAG status.
#[derive(Args, Debug)]
pub struct DagStatusArgs {
    /// DAG workflow ID
    pub dag_id: String,

    /// Show detailed per-stage status
    #[arg(long)]
    pub detail: bool,
}

/// Arguments for cancelling a DAG.
#[derive(Args, Debug)]
pub struct DagCancelArgs {
    /// DAG workflow ID
    pub dag_id: String,

    /// Cancel only pending stages (let running stages finish)
    #[arg(long)]
    pub pending_only: bool,
}

// ---------------------------------------------------------------------------
// YAML schema types
// ---------------------------------------------------------------------------

/// Top-level DAG YAML specification.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DagYamlSpec {
    pub dag_id: String,
    pub allocations: Vec<DagAllocationYaml>,
    #[serde(default)]
    pub edges: Vec<DagEdgeYaml>,
}

/// A single allocation within a DAG YAML file.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DagAllocationYaml {
    pub name: String,
    pub tenant: String,
    #[serde(default)]
    pub project: Option<String>,
    pub entrypoint: String,
    #[serde(default)]
    pub nodes: Option<u32>,
    #[serde(default)]
    pub walltime_hours: Option<f64>,
    #[serde(default)]
    pub depends_on: Vec<DagInlineDep>,
}

/// An inline dependency reference on a DAG allocation.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DagInlineDep {
    pub name: String,
    #[serde(default = "default_condition")]
    pub condition: String,
}

/// A top-level edge in a DAG YAML file.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DagEdgeYaml {
    pub from: String,
    pub to: String,
    #[serde(default = "default_condition")]
    pub condition: String,
}

fn default_condition() -> String {
    "success".to_string()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a parsed DAG YAML spec:
/// - No duplicate allocation names
/// - All edge/depends_on references point to existing names
/// - No cycles (via Kahn's topological sort)
pub(crate) fn validate_dag_yaml(spec: &DagYamlSpec) -> anyhow::Result<()> {
    // Check for duplicate names
    let mut seen = HashSet::new();
    for alloc in &spec.allocations {
        if !seen.insert(&alloc.name) {
            anyhow::bail!("duplicate allocation name: {}", alloc.name);
        }
    }

    // Build the set of valid allocation names
    let valid_names: HashSet<&str> = spec.allocations.iter().map(|a| a.name.as_str()).collect();

    // Validate top-level edge references
    for edge in &spec.edges {
        if !valid_names.contains(edge.from.as_str()) {
            anyhow::bail!(
                "edge references unknown allocation '{}' in 'from'",
                edge.from
            );
        }
        if !valid_names.contains(edge.to.as_str()) {
            anyhow::bail!("edge references unknown allocation '{}' in 'to'", edge.to);
        }
    }

    // Validate inline depends_on references
    for alloc in &spec.allocations {
        for dep in &alloc.depends_on {
            if !valid_names.contains(dep.name.as_str()) {
                anyhow::bail!(
                    "allocation '{}' depends on unknown allocation '{}'",
                    alloc.name,
                    dep.name
                );
            }
        }
    }

    // Cycle detection via Kahn's topological sort
    detect_cycles(spec)?;

    Ok(())
}

/// Detect cycles using Kahn's algorithm. Returns an error if a cycle exists.
fn detect_cycles(spec: &DagYamlSpec) -> anyhow::Result<()> {
    let names: Vec<&str> = spec.allocations.iter().map(|a| a.name.as_str()).collect();
    let name_to_idx: HashMap<&str, usize> =
        names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let n = names.len();

    let mut in_degree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];

    // Build adjacency from top-level edges
    for edge in &spec.edges {
        let from = name_to_idx[edge.from.as_str()];
        let to = name_to_idx[edge.to.as_str()];
        adj[from].push(to);
        in_degree[to] += 1;
    }

    // Build adjacency from inline depends_on (dependency -> allocation)
    for alloc in &spec.allocations {
        let to = name_to_idx[alloc.name.as_str()];
        for dep in &alloc.depends_on {
            let from = name_to_idx[dep.name.as_str()];
            adj[from].push(to);
            in_degree[to] += 1;
        }
    }

    // Kahn's algorithm
    let mut queue: VecDeque<usize> = VecDeque::new();
    for (i, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            queue.push_back(i);
        }
    }

    let mut visited = 0;
    while let Some(node) = queue.pop_front() {
        visited += 1;
        for &neighbor in &adj[node] {
            in_degree[neighbor] -= 1;
            if in_degree[neighbor] == 0 {
                queue.push_back(neighbor);
            }
        }
    }

    if visited != n {
        anyhow::bail!("DAG contains a cycle");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Conversion to proto
// ---------------------------------------------------------------------------

/// Convert a validated DAG YAML spec into a proto DagSpec.
pub(crate) fn yaml_to_proto_dag(spec: &DagYamlSpec, tenant_override: Option<&str>) -> pb::DagSpec {
    // Build a map from allocation name to its collected dependencies
    // (merged from both top-level edges and inline depends_on).
    let mut deps_map: HashMap<&str, Vec<pb::DependencySpec>> = HashMap::new();

    // Inline depends_on
    for alloc in &spec.allocations {
        let entry = deps_map.entry(alloc.name.as_str()).or_default();
        for dep in &alloc.depends_on {
            entry.push(pb::DependencySpec {
                ref_id: dep.name.clone(),
                condition: dep.condition.clone(),
            });
        }
    }

    // Top-level edges
    for edge in &spec.edges {
        deps_map
            .entry(edge.to.as_str())
            .or_default()
            .push(pb::DependencySpec {
                ref_id: edge.from.clone(),
                condition: edge.condition.clone(),
            });
    }

    let allocations = spec
        .allocations
        .iter()
        .map(|alloc| {
            let tenant = tenant_override
                .map(|t| t.to_string())
                .unwrap_or_else(|| alloc.tenant.clone());

            let depends_on = deps_map.remove(alloc.name.as_str()).unwrap_or_default();

            let walltime = alloc.walltime_hours.map(|h| prost_types::Duration {
                seconds: (h * 3600.0) as i64,
                nanos: 0,
            });

            pb::AllocationSpec {
                id: alloc.name.clone(),
                tenant,
                project: alloc.project.clone().unwrap_or_default(),
                entrypoint: alloc.entrypoint.clone(),
                resources: Some(pb::ResourceSpec {
                    min_nodes: alloc.nodes.unwrap_or(1),
                    max_nodes: alloc.nodes.unwrap_or(1),
                    ..Default::default()
                }),
                lifecycle: walltime.map(|w| pb::LifecycleSpec {
                    r#type: 0, // BOUNDED
                    walltime: Some(w),
                    ..Default::default()
                }),
                depends_on,
                ..Default::default()
            }
        })
        .collect();

    pb::DagSpec { allocations }
}

// ---------------------------------------------------------------------------
// Execute
// ---------------------------------------------------------------------------

/// Execute the dag command via gRPC.
pub async fn execute(
    args: &DagArgs,
    client: &mut LatticeGrpcClient,
    _config: &ClientConfig,
    format: OutputFormat,
    quiet: bool,
) -> anyhow::Result<()> {
    match &args.command {
        DagCommand::Submit(submit_args) => {
            let content = std::fs::read_to_string(&submit_args.file).map_err(|e| {
                anyhow::anyhow!("failed to read DAG file {}: {e}", submit_args.file)
            })?;

            let spec: DagYamlSpec = serde_yaml::from_str(&content)
                .map_err(|e| anyhow::anyhow!("failed to parse DAG YAML: {e}"))?;

            validate_dag_yaml(&spec)?;

            let dag = yaml_to_proto_dag(&spec, submit_args.tenant.as_deref());

            let req = pb::SubmitRequest {
                submission: Some(pb::submit_request::Submission::Dag(dag)),
            };
            let resp = client.submit(req).await?;

            if !quiet {
                let dag_name = &spec.dag_id;
                match format {
                    OutputFormat::Json => {
                        let json = serde_json::json!({
                            "dag_id": resp.dag_id,
                            "dag_name": dag_name,
                            "allocation_ids": resp.allocation_ids,
                        });
                        println!("{}", serde_json::to_string_pretty(&json)?);
                    }
                    _ => {
                        println!("Submitted DAG '{}': {}", dag_name, resp.dag_id);
                        for id in &resp.allocation_ids {
                            println!("  allocation: {id}");
                        }
                    }
                }
            }
        }
        DagCommand::Status(status_args) => {
            let dag = client.get_dag(&status_args.dag_id).await?;
            match format {
                OutputFormat::Json => {
                    let allocs: Vec<serde_json::Value> = dag
                        .allocations
                        .iter()
                        .map(|a| {
                            let info = allocation_status_to_info(a);
                            serde_json::json!({
                                "id": info.id,
                                "state": info.state,
                                "nodes": info.nodes,
                            })
                        })
                        .collect();
                    let json = serde_json::json!({
                        "dag_id": dag.dag_id,
                        "state": dag.state,
                        "allocations": allocs,
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
                _ => {
                    println!("DAG {} [{}]", dag.dag_id, dag.state);
                    for a in &dag.allocations {
                        let info = allocation_status_to_info(a);
                        println!("  {} - {} ({} nodes)", info.id, info.state, info.nodes);
                    }
                }
            }
        }
        DagCommand::Cancel(cancel_args) => {
            let resp = client.cancel_dag(&cancel_args.dag_id).await?;
            if !quiet {
                if resp.success {
                    println!(
                        "Cancelled DAG {}: {} allocation(s) cancelled",
                        cancel_args.dag_id, resp.allocations_cancelled
                    );
                } else {
                    eprintln!("Failed to cancel DAG: {}", cancel_args.dag_id);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: DagCommand,
    }

    #[test]
    fn parse_dag_submit() {
        let cli = TestCli::parse_from(["test", "submit", "pipeline.yaml"]);
        if let DagCommand::Submit(args) = cli.command {
            assert_eq!(args.file, "pipeline.yaml");
        } else {
            panic!("expected Submit");
        }
    }

    #[test]
    fn parse_dag_status() {
        let cli = TestCli::parse_from(["test", "status", "dag-123", "--detail"]);
        if let DagCommand::Status(args) = cli.command {
            assert_eq!(args.dag_id, "dag-123");
            assert!(args.detail);
        } else {
            panic!("expected Status");
        }
    }

    #[test]
    fn parse_dag_cancel() {
        let cli = TestCli::parse_from(["test", "cancel", "dag-123", "--pending-only"]);
        if let DagCommand::Cancel(args) = cli.command {
            assert_eq!(args.dag_id, "dag-123");
            assert!(args.pending_only);
        } else {
            panic!("expected Cancel");
        }
    }

    // ── YAML parsing tests ──────────────────────────────────────

    const SAMPLE_DAG_YAML: &str = r#"
dag_id: training-pipeline
allocations:
  - name: preprocess
    tenant: ml-team
    entrypoint: /bin/preprocess.sh
    nodes: 2
    walltime_hours: 1.0
  - name: train
    tenant: ml-team
    entrypoint: /bin/train.sh
    nodes: 4
    walltime_hours: 8.0
    depends_on:
      - name: preprocess
        condition: success
  - name: evaluate
    tenant: ml-team
    entrypoint: /bin/evaluate.sh
    nodes: 1
    depends_on:
      - name: train
edges:
  - from: preprocess
    to: evaluate
    condition: any
"#;

    #[test]
    fn parse_valid_dag_yaml() {
        let spec: DagYamlSpec = serde_yaml::from_str(SAMPLE_DAG_YAML).unwrap();
        assert_eq!(spec.dag_id, "training-pipeline");
        assert_eq!(spec.allocations.len(), 3);
        assert_eq!(spec.edges.len(), 1);

        assert_eq!(spec.allocations[0].name, "preprocess");
        assert_eq!(spec.allocations[0].nodes, Some(2));
        assert!(spec.allocations[0].depends_on.is_empty());

        assert_eq!(spec.allocations[1].name, "train");
        assert_eq!(spec.allocations[1].depends_on.len(), 1);
        assert_eq!(spec.allocations[1].depends_on[0].name, "preprocess");
        assert_eq!(spec.allocations[1].depends_on[0].condition, "success");

        assert_eq!(spec.allocations[2].depends_on.len(), 1);
        assert_eq!(spec.allocations[2].depends_on[0].condition, "success"); // default
    }

    #[test]
    fn parse_minimal_dag_yaml() {
        let yaml = r#"
dag_id: simple
allocations:
  - name: step1
    tenant: t
    entrypoint: /run.sh
"#;
        let spec: DagYamlSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.allocations.len(), 1);
        assert!(spec.edges.is_empty());
    }

    // ── Validation tests ────────────────────────────────────────

    #[test]
    fn validate_valid_dag() {
        let spec: DagYamlSpec = serde_yaml::from_str(SAMPLE_DAG_YAML).unwrap();
        validate_dag_yaml(&spec).unwrap();
    }

    #[test]
    fn validate_duplicate_names() {
        let yaml = r#"
dag_id: dup
allocations:
  - name: step1
    tenant: t
    entrypoint: /a.sh
  - name: step1
    tenant: t
    entrypoint: /b.sh
"#;
        let spec: DagYamlSpec = serde_yaml::from_str(yaml).unwrap();
        let err = validate_dag_yaml(&spec).unwrap_err();
        assert!(err.to_string().contains("duplicate allocation name"));
    }

    #[test]
    fn validate_missing_edge_reference() {
        let yaml = r#"
dag_id: bad-edge
allocations:
  - name: step1
    tenant: t
    entrypoint: /a.sh
edges:
  - from: step1
    to: nonexistent
"#;
        let spec: DagYamlSpec = serde_yaml::from_str(yaml).unwrap();
        let err = validate_dag_yaml(&spec).unwrap_err();
        assert!(err.to_string().contains("unknown allocation 'nonexistent'"));
    }

    #[test]
    fn validate_missing_inline_dep_reference() {
        let yaml = r#"
dag_id: bad-dep
allocations:
  - name: step1
    tenant: t
    entrypoint: /a.sh
    depends_on:
      - name: ghost
"#;
        let spec: DagYamlSpec = serde_yaml::from_str(yaml).unwrap();
        let err = validate_dag_yaml(&spec).unwrap_err();
        assert!(err.to_string().contains("unknown allocation 'ghost'"));
    }

    #[test]
    fn validate_cycle_detection() {
        let yaml = r#"
dag_id: cycle
allocations:
  - name: a
    tenant: t
    entrypoint: /a.sh
    depends_on:
      - name: c
  - name: b
    tenant: t
    entrypoint: /b.sh
    depends_on:
      - name: a
  - name: c
    tenant: t
    entrypoint: /c.sh
    depends_on:
      - name: b
"#;
        let spec: DagYamlSpec = serde_yaml::from_str(yaml).unwrap();
        let err = validate_dag_yaml(&spec).unwrap_err();
        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn validate_self_loop_detected() {
        let yaml = r#"
dag_id: self-loop
allocations:
  - name: a
    tenant: t
    entrypoint: /a.sh
    depends_on:
      - name: a
"#;
        let spec: DagYamlSpec = serde_yaml::from_str(yaml).unwrap();
        let err = validate_dag_yaml(&spec).unwrap_err();
        assert!(err.to_string().contains("cycle"));
    }

    // ── Proto conversion tests ──────────────────────────────────

    #[test]
    fn yaml_to_proto_basic() {
        let spec: DagYamlSpec = serde_yaml::from_str(SAMPLE_DAG_YAML).unwrap();
        let dag = yaml_to_proto_dag(&spec, None);

        assert_eq!(dag.allocations.len(), 3);

        // preprocess: no deps from inline, but has edge (preprocess -> evaluate)
        let preprocess = &dag.allocations[0];
        assert_eq!(preprocess.id, "preprocess");
        assert_eq!(preprocess.tenant, "ml-team");
        assert!(preprocess.depends_on.is_empty());
        assert_eq!(preprocess.resources.as_ref().unwrap().min_nodes, 2);
        assert!(preprocess.lifecycle.is_some());
        assert_eq!(
            preprocess
                .lifecycle
                .as_ref()
                .unwrap()
                .walltime
                .as_ref()
                .unwrap()
                .seconds,
            3600
        );

        // train: depends on preprocess (inline)
        let train = &dag.allocations[1];
        assert_eq!(train.id, "train");
        assert_eq!(train.depends_on.len(), 1);
        assert_eq!(train.depends_on[0].ref_id, "preprocess");
        assert_eq!(train.depends_on[0].condition, "success");

        // evaluate: depends on train (inline) + preprocess (top-level edge)
        let evaluate = &dag.allocations[2];
        assert_eq!(evaluate.id, "evaluate");
        assert_eq!(evaluate.depends_on.len(), 2);
        let dep_names: HashSet<&str> = evaluate
            .depends_on
            .iter()
            .map(|d| d.ref_id.as_str())
            .collect();
        assert!(dep_names.contains("train"));
        assert!(dep_names.contains("preprocess"));
    }

    #[test]
    fn yaml_to_proto_tenant_override() {
        let spec: DagYamlSpec = serde_yaml::from_str(SAMPLE_DAG_YAML).unwrap();
        let dag = yaml_to_proto_dag(&spec, Some("overridden"));

        for alloc in &dag.allocations {
            assert_eq!(alloc.tenant, "overridden");
        }
    }

    #[test]
    fn yaml_to_proto_defaults() {
        let yaml = r#"
dag_id: minimal
allocations:
  - name: step1
    tenant: t
    entrypoint: /run.sh
"#;
        let spec: DagYamlSpec = serde_yaml::from_str(yaml).unwrap();
        let dag = yaml_to_proto_dag(&spec, None);

        assert_eq!(dag.allocations.len(), 1);
        let alloc = &dag.allocations[0];
        assert_eq!(alloc.resources.as_ref().unwrap().min_nodes, 1); // default
        assert!(alloc.lifecycle.is_none()); // no walltime
        assert!(alloc.depends_on.is_empty());
    }

    #[test]
    fn malformed_yaml_returns_error() {
        let bad_yaml = "not: [valid: yaml: {{{}}}";
        let result: Result<DagYamlSpec, _> = serde_yaml::from_str(bad_yaml);
        assert!(result.is_err());
    }
}
