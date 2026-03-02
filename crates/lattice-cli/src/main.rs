//! `lattice` — the Lattice CLI.
//!
//! Parses command-line arguments and dispatches to the appropriate subcommand.
//! Each subcommand connects to the lattice-server via gRPC.

use anyhow::Result;
use clap::Parser;
use tracing::debug;

use lattice_cli::commands::{self, Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    debug!("Parsed CLI: output={}, quiet={}", cli.output, cli.quiet);

    match cli.command {
        Command::Submit(args) => {
            println!(
                "Submit: script={:?} nodes={:?}",
                args.script.as_deref().unwrap_or("(none)"),
                args.nodes
            );
            println!("Would connect to lattice-server and submit allocation");
        }
        Command::Status(args) => {
            println!("Status: {:?}", args);
            println!("Would connect to lattice-server and query status");
        }
        Command::Cancel(args) => {
            println!("Cancel: {:?}", args);
            println!("Would connect to lattice-server and cancel allocation");
        }
        Command::Nodes(args) => {
            println!("Nodes: {:?}", args);
            println!("Would connect to lattice-server and list/inspect nodes");
        }
        Command::Admin(args) => {
            println!("Admin: {:?}", args);
            println!("Would connect to lattice-server and run admin command");
        }
        Command::Attach(args) => {
            commands::attach::execute(&args).await?;
        }
        Command::Logs(args) => {
            commands::logs::execute(&args).await?;
        }
        Command::Top(args) => {
            commands::top::execute(&args).await?;
        }
        Command::Watch(args) => {
            commands::watch::execute(&args).await?;
        }
        Command::Diag(args) => {
            commands::diag::execute(&args).await?;
        }
        Command::Session(args) => {
            commands::session::execute(&args).await?;
        }
        Command::Dag(args) => {
            commands::dag::execute(&args).await?;
        }
        Command::Completions { shell } => {
            lattice_cli::completions::generate_completions(shell);
        }
    }

    Ok(())
}
