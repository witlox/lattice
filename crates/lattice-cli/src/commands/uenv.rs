//! `lattice uenv` — uenv image management subcommands.

use clap::{Args, Subcommand};

/// Arguments for the uenv command.
#[derive(Args, Debug)]
pub struct UenvArgs {
    #[command(subcommand)]
    pub command: UenvCommand,
}

#[derive(Subcommand, Debug)]
pub enum UenvCommand {
    /// Query available views for a uenv image spec
    Views(UenvViewsArgs),
    /// Manage the uenv image cache
    Cache(UenvCacheArgs),
}

/// Arguments for `lattice uenv views <spec>`.
#[derive(Args, Debug)]
pub struct UenvViewsArgs {
    /// uenv image spec (e.g., prgenv-gnu/24.11:v1)
    pub spec: String,
}

/// Arguments for `lattice uenv cache`.
#[derive(Args, Debug)]
pub struct UenvCacheArgs {
    #[command(subcommand)]
    pub command: UenvCacheCommand,
}

#[derive(Subcommand, Debug)]
pub enum UenvCacheCommand {
    /// List cached images on a node
    List(UenvCacheListArgs),
}

/// Arguments for `lattice uenv cache list`.
#[derive(Args, Debug)]
pub struct UenvCacheListArgs {
    /// Node ID to query
    #[arg(long)]
    pub node: String,
}

/// Execute the uenv command.
pub async fn execute(args: &UenvArgs) -> anyhow::Result<()> {
    match &args.command {
        UenvCommand::Views(v) => {
            println!(
                "view query not yet connected to registry (spec: {})",
                v.spec
            );
        }
        UenvCommand::Cache(c) => match &c.command {
            UenvCacheCommand::List(l) => {
                println!("cache query not yet connected (node: {})", l.node);
            }
        },
    }
    Ok(())
}
