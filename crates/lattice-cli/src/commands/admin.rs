//! `lattice admin` — administrative subcommands.

use clap::{Args, Subcommand};

/// Arguments for admin commands.
#[derive(Args, Debug)]
pub struct AdminArgs {
    #[command(subcommand)]
    pub command: AdminCommand,
}

#[derive(Subcommand, Debug)]
pub enum AdminCommand {
    /// Drain a node (stop scheduling, allow running allocations to complete)
    Drain(DrainArgs),
    /// Undrain a node (resume scheduling)
    Undrain(UndrainArgs),
    /// Disable a node (force down)
    Disable(DisableArgs),
    /// Create a tenant
    TenantCreate(TenantCreateArgs),
    /// View Raft quorum status
    RaftStatus,
}

#[derive(Args, Debug)]
pub struct DrainArgs {
    /// Node ID to drain
    pub node_id: String,
}

#[derive(Args, Debug)]
pub struct UndrainArgs {
    /// Node ID to undrain
    pub node_id: String,
}

#[derive(Args, Debug)]
pub struct DisableArgs {
    /// Node ID to disable
    pub node_id: String,
    /// Reason for disabling
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Args, Debug)]
pub struct TenantCreateArgs {
    /// Tenant name
    pub name: String,
    /// Maximum nodes
    #[arg(long)]
    pub max_nodes: Option<u32>,
    /// Fair share target (0.0-1.0)
    #[arg(long)]
    pub fair_share: Option<f64>,
    /// Isolation level (standard/strict)
    #[arg(long, default_value = "standard")]
    pub isolation: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // Test that the CLI parses correctly by parsing the struct directly
    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: AdminCommand,
    }

    #[test]
    fn parse_drain() {
        let cli = TestCli::parse_from(["test", "drain", "node-0"]);
        assert!(matches!(cli.command, AdminCommand::Drain(ref args) if args.node_id == "node-0"));
    }

    #[test]
    fn parse_undrain() {
        let cli = TestCli::parse_from(["test", "undrain", "node-0"]);
        assert!(matches!(cli.command, AdminCommand::Undrain(_)));
    }

    #[test]
    fn parse_disable_with_reason() {
        let cli =
            TestCli::parse_from(["test", "disable", "node-0", "--reason", "hardware failure"]);
        if let AdminCommand::Disable(args) = cli.command {
            assert_eq!(args.node_id, "node-0");
            assert_eq!(args.reason.as_deref(), Some("hardware failure"));
        } else {
            panic!("expected Disable");
        }
    }

    #[test]
    fn parse_tenant_create() {
        let cli = TestCli::parse_from([
            "test",
            "tenant-create",
            "physics",
            "--max-nodes",
            "100",
            "--fair-share",
            "0.3",
        ]);
        if let AdminCommand::TenantCreate(args) = cli.command {
            assert_eq!(args.name, "physics");
            assert_eq!(args.max_nodes, Some(100));
            assert!((args.fair_share.unwrap() - 0.3).abs() < 0.001);
        } else {
            panic!("expected TenantCreate");
        }
    }

    #[test]
    fn parse_raft_status() {
        let cli = TestCli::parse_from(["test", "raft-status"]);
        assert!(matches!(cli.command, AdminCommand::RaftStatus));
    }
}
