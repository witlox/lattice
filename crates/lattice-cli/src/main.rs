//! `lattice` — the Lattice CLI.
//!
//! Parses command-line arguments and dispatches to the appropriate subcommand.
//! Each subcommand connects to the lattice-server via gRPC.

use anyhow::Result;
use clap::Parser;
use tracing::debug;

use lattice_cli::client::{ClientConfig, LatticeGrpcClient};
use lattice_cli::commands::{self, Cli, Command};
use lattice_cli::config_file::CliConfig;
use lattice_cli::output::OutputFormat;

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

    // Shell completions don't need a gRPC connection.
    if let Command::Completions { shell } = cli.command {
        lattice_cli::completions::generate_completions(shell);
        return Ok(());
    }

    // Load config file and merge CLI overrides.
    let file_config = CliConfig::load();
    let merged = file_config.merge_cli_overrides(
        cli.tenant.as_deref(),
        cli.vcluster.as_deref(),
        Some(&cli.output),
    );

    let output_format = OutputFormat::parse(merged.output_format.as_deref().unwrap_or(&cli.output));

    // Build client config from merged configuration.
    let client_config = ClientConfig {
        api_endpoint: format!("http://{}", merged.server_addr()),
        timeout_secs: 30,
        user: std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "anonymous".to_string()),
        tenant: merged.default_tenant.clone(),
        vcluster: merged.default_vcluster.clone(),
    };

    // Connect to the lattice-server.
    let mut client = match LatticeGrpcClient::connect(&client_config).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "Error: failed to connect to lattice-server at {}: {}",
                client_config.api_endpoint, e
            );
            std::process::exit(1);
        }
    };

    let quiet = cli.quiet;

    match cli.command {
        Command::Submit(args) => {
            commands::submit::execute(&args, &mut client, &client_config, output_format, quiet)
                .await?;
        }
        Command::Status(args) => {
            commands::status::execute(&args, &mut client, &client_config, output_format).await?;
        }
        Command::Cancel(args) => {
            commands::cancel::execute(&args, &mut client, quiet).await?;
        }
        Command::Nodes(args) => {
            commands::nodes::execute(&args, &mut client, output_format).await?;
        }
        Command::Admin(args) => {
            commands::admin::execute(&args, &mut client, output_format).await?;
        }
        Command::Attach(args) => {
            commands::attach::execute(&args, &mut client).await?;
        }
        Command::Logs(args) => {
            commands::logs::execute(&args, &mut client).await?;
        }
        Command::Top(args) => {
            commands::top::execute(&args, &mut client, output_format).await?;
        }
        Command::Watch(args) => {
            commands::watch::execute(&args, &mut client, &client_config).await?;
        }
        Command::Diag(args) => {
            commands::diag::execute(&args, &mut client, output_format).await?;
        }
        Command::Session(args) => {
            commands::session::execute(&args, &mut client, &client_config, output_format, quiet)
                .await?;
        }
        Command::Dag(args) => {
            commands::dag::execute(&args, &mut client, &client_config, output_format, quiet)
                .await?;
        }
        Command::Completions { .. } => unreachable!(),
    }

    Ok(())
}
