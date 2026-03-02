//! `lattice session` — interactive session management.
//!
//! Create, list, and destroy interactive sessions (salloc equivalent).
//! See docs/architecture/sessions.md.

use clap::{Args, Subcommand};

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

/// Execute the session command (stub — real gRPC calls will come later).
pub async fn execute(args: &SessionArgs) -> anyhow::Result<()> {
    match &args.command {
        SessionCommand::List(list_args) => {
            println!("Listing sessions:");
            if list_args.mine {
                println!("  Filtering to own sessions");
            }
            if let Some(ref state) = list_args.state {
                println!("  State filter: {state}");
            }
            println!("Would connect to lattice-server and list sessions");
        }
        SessionCommand::Create(create_args) => {
            println!("Creating interactive session:");
            if let Some(n) = create_args.nodes {
                println!("  Nodes: {n}");
            }
            if let Some(ref w) = create_args.walltime {
                println!("  Walltime: {w}");
            }
            if let Some(ref u) = create_args.uenv {
                println!("  uenv: {u}");
            }
            if let Some(ref v) = create_args.view {
                println!("  View: {v}");
            }
            if let Some(ref c) = create_args.constraint {
                println!("  Constraint: {c}");
            }
            if let Some(ref name) = create_args.name {
                println!("  Name: {name}");
            }
            println!("Would connect to lattice-server and create session");
        }
        SessionCommand::Destroy(destroy_args) => {
            println!("Destroying session {}:", destroy_args.session_id);
            if destroy_args.force {
                println!("  Force mode enabled");
            }
            println!("Would connect to lattice-server and destroy session");
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
