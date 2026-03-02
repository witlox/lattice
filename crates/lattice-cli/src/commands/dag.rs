//! `lattice dag` — DAG workflow management.
//!
//! Submit, monitor, and cancel DAG workflows (multi-stage pipelines).
//! See docs/architecture/dag-scheduling.md.

use clap::{Args, Subcommand};

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

/// Execute the dag command (stub).
pub async fn execute(args: &DagArgs) -> anyhow::Result<()> {
    match &args.command {
        DagCommand::Submit(submit_args) => {
            println!("Submitting DAG from: {}", submit_args.file);
            if let Some(ref t) = submit_args.tenant {
                println!("  Tenant: {t}");
            }
            println!("Would parse DAG file and submit to lattice-server");
        }
        DagCommand::Status(status_args) => {
            println!("DAG status for: {}", status_args.dag_id);
            if status_args.detail {
                println!("  Showing detailed per-stage status");
            }
            println!("Would query lattice-server for DAG status");
        }
        DagCommand::Cancel(cancel_args) => {
            println!("Cancelling DAG: {}", cancel_args.dag_id);
            if cancel_args.pending_only {
                println!("  Only cancelling pending stages");
            }
            println!("Would send cancel request to lattice-server");
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
