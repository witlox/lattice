//! CLI subcommands — each module implements one top-level command.

pub mod admin;
pub mod attach;
pub mod auth;
pub mod cancel;
pub mod dag;
pub mod diag;
pub mod logs;
pub mod nodes;
pub mod session;
pub mod status;
pub mod submit;
pub mod top;
pub mod watch;

use clap::{Parser, Subcommand};
use clap_complete::Shell;

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

    /// Attach a terminal to a running allocation
    Attach(attach::AttachArgs),

    /// Stream or tail allocation logs
    Logs(logs::LogsArgs),

    /// Live resource usage dashboard
    Top(top::TopArgs),

    /// Watch allocation state changes
    Watch(watch::WatchArgs),

    /// Show allocation diagnostics
    Diag(diag::DiagArgs),

    /// Interactive session management
    Session(session::SessionArgs),

    /// DAG workflow management
    Dag(dag::DagArgs),

    /// Login to lattice server
    Login(auth::LoginArgs),

    /// Logout from lattice server
    Logout(auth::LogoutArgs),

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },
}
