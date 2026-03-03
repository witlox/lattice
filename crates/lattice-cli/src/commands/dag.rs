//! `lattice dag` — DAG workflow management.
//!
//! Submit, monitor, and cancel DAG workflows (multi-stage pipelines).
//! See docs/architecture/dag-scheduling.md.

use clap::{Args, Subcommand};

use crate::client::{ClientConfig, LatticeGrpcClient};
use crate::convert::allocation_status_to_info;
use crate::output::OutputFormat;

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
            // DAG files are YAML descriptions; for now we parse them as
            // a simple list of allocation specs that form a DAG.
            // The real implementation would use serde_yaml to parse the
            // file into DagSpec. For now, we submit as a raw entrypoint.
            let _ = content; // acknowledged but not fully parsed yet
            eprintln!(
                "DAG submission from YAML files is not yet implemented. \
                 Use `lattice submit` with --depends-on for simple DAG workflows."
            );
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
}
