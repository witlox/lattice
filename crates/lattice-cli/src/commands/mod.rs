//! CLI subcommands — each module implements one top-level command.

pub mod admin;
pub mod cancel;
pub mod nodes;
pub mod status;
pub mod submit;

use clap::{Parser, Subcommand};

/// Lattice — distributed workload scheduler CLI.
#[derive(Parser)]
#[command(name = "lattice", version, about = "Distributed workload scheduler")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Output format: table, json, wide
    #[arg(short, long, default_value = "table", global = true)]
    pub output: String,

    /// Suppress non-essential output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Override tenant
    #[arg(short, long, global = true)]
    pub tenant: Option<String>,

    /// Override vCluster
    #[arg(long, global = true)]
    pub vcluster: Option<String>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Submit an allocation or batch script
    Submit(submit::SubmitArgs),

    /// Query allocation status
    Status(status::StatusArgs),

    /// Cancel allocations
    Cancel(cancel::CancelArgs),

    /// List or inspect nodes
    Nodes(nodes::NodesArgs),

    /// Administrative commands
    Admin(admin::AdminArgs),
}
