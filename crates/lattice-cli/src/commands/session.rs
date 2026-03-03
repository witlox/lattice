//! `lattice session` — interactive session management.
//!
//! Create, list, and destroy interactive sessions (salloc equivalent).
//! See docs/architecture/sessions.md.

use clap::{Args, Subcommand};

use crate::client::{ClientConfig, LatticeGrpcClient};
use crate::convert::{allocation_status_to_info, build_list_request, build_submit_request};
use crate::output::OutputFormat;

/// Arguments for the session command.
#[derive(Args, Debug)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

#[derive(Subcommand, Debug)]
pub enum SessionCommand {
    /// List active sessions
    List(SessionListArgs),
    /// Create a new interactive session
    Create(SessionCreateArgs),
    /// Destroy an active session
    Destroy(SessionDestroyArgs),
}

/// Arguments for listing sessions.
#[derive(Args, Debug)]
pub struct SessionListArgs {
    /// Show only own sessions
    #[arg(long)]
    pub mine: bool,

    /// Filter by state
    #[arg(long)]
    pub state: Option<String>,
}

/// Arguments for creating a session.
#[derive(Args, Debug)]
pub struct SessionCreateArgs {
    /// Number of nodes
    #[arg(long)]
    pub nodes: Option<u32>,

    /// Walltime (Slurm format: HH:MM:SS or minutes)
    #[arg(long)]
    pub walltime: Option<String>,

    /// uenv image to use
    #[arg(long)]
    pub uenv: Option<String>,

    /// uenv view to mount
    #[arg(long)]
    pub view: Option<String>,

    /// GPU type constraint (e.g., GH200, MI300X)
    #[arg(long)]
    pub constraint: Option<String>,

    /// Session name
    #[arg(long)]
    pub name: Option<String>,
}

/// Arguments for destroying a session.
#[derive(Args, Debug)]
pub struct SessionDestroyArgs {
    /// Session ID to destroy
    pub session_id: String,

    /// Force destroy without confirmation
    #[arg(long)]
    pub force: bool,
}

/// Execute the session command via gRPC.
pub async fn execute(
    args: &SessionArgs,
    client: &mut LatticeGrpcClient,
    config: &ClientConfig,
    format: OutputFormat,
    quiet: bool,
) -> anyhow::Result<()> {
    match &args.command {
        SessionCommand::List(list_args) => {
            let user_filter = if list_args.mine {
                Some(config.user.as_str())
            } else {
                None
            };
            let req = build_list_request(
                user_filter,
                config.tenant.as_deref(),
                config.vcluster.as_deref(),
                list_args.state.as_deref(),
            );
            let resp = client.list_allocations(req).await?;
            let infos: Vec<_> = resp
                .allocations
                .iter()
                .map(allocation_status_to_info)
                .collect();
            let output = crate::commands::status::format_allocations(&infos, format);
            print!("{output}");
        }
        SessionCommand::Create(create_args) => {
            let desc = crate::commands::submit::SubmitDescription {
                tenant: config.tenant.clone(),
                project: create_args.name.clone(),
                nodes: create_args.nodes,
                walltime: create_args
                    .walltime
                    .as_deref()
                    .and_then(crate::compat::parse_slurm_time),
                uenv: create_args.uenv.clone(),
                view: create_args.view.clone(),
                ..Default::default()
            };
            let req = build_submit_request(&desc, &config.user, config.vcluster.as_deref());
            let resp = client.submit(req).await?;

            if !quiet {
                for id in &resp.allocation_ids {
                    println!("Session created: {id}");
                }
            }
        }
        SessionCommand::Destroy(destroy_args) => {
            let resp = client.cancel(&destroy_args.session_id).await?;
            if !quiet {
                if resp.success {
                    println!("Session destroyed: {}", destroy_args.session_id);
                } else {
                    eprintln!("Failed to destroy session: {}", destroy_args.session_id);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: SessionCommand,
    }

    #[test]
    fn parse_session_list() {
        let cli = TestCli::parse_from(["test", "list"]);
        assert!(matches!(cli.command, SessionCommand::List(_)));
    }

    #[test]
    fn parse_session_list_mine() {
        let cli = TestCli::parse_from(["test", "list", "--mine"]);
        if let SessionCommand::List(args) = cli.command {
            assert!(args.mine);
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn parse_session_list_with_state() {
        let cli = TestCli::parse_from(["test", "list", "--state", "active"]);
        if let SessionCommand::List(args) = cli.command {
            assert_eq!(args.state.as_deref(), Some("active"));
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn parse_session_create_basic() {
        let cli = TestCli::parse_from(["test", "create", "--walltime", "4:00:00"]);
        if let SessionCommand::Create(args) = cli.command {
            assert_eq!(args.walltime.as_deref(), Some("4:00:00"));
            assert!(args.nodes.is_none());
        } else {
            panic!("expected Create");
        }
    }

    #[test]
    fn parse_session_create_full() {
        let cli = TestCli::parse_from([
            "test",
            "create",
            "--nodes",
            "2",
            "--walltime",
            "8:00:00",
            "--uenv",
            "pytorch:latest",
            "--view",
            "develop",
            "--constraint",
            "gpu_type:GH200",
            "--name",
            "my-session",
        ]);
        if let SessionCommand::Create(args) = cli.command {
            assert_eq!(args.nodes, Some(2));
            assert_eq!(args.walltime.as_deref(), Some("8:00:00"));
            assert_eq!(args.uenv.as_deref(), Some("pytorch:latest"));
            assert_eq!(args.view.as_deref(), Some("develop"));
            assert_eq!(args.constraint.as_deref(), Some("gpu_type:GH200"));
            assert_eq!(args.name.as_deref(), Some("my-session"));
        } else {
            panic!("expected Create");
        }
    }

    #[test]
    fn parse_session_destroy() {
        let cli = TestCli::parse_from(["test", "destroy", "session-123"]);
        if let SessionCommand::Destroy(args) = cli.command {
            assert_eq!(args.session_id, "session-123");
            assert!(!args.force);
        } else {
            panic!("expected Destroy");
        }
    }

    #[test]
    fn parse_session_destroy_force() {
        let cli = TestCli::parse_from(["test", "destroy", "session-123", "--force"]);
        if let SessionCommand::Destroy(args) = cli.command {
            assert_eq!(args.session_id, "session-123");
            assert!(args.force);
        } else {
            panic!("expected Destroy");
        }
    }
}
