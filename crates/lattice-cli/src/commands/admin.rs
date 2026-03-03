//! `lattice admin` — administrative subcommands.

use clap::{Args, Subcommand};

use crate::client::LatticeGrpcClient;
use crate::convert::build_create_tenant_request;
use crate::output::OutputFormat;

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

/// Execute the admin command via gRPC.
pub async fn execute(
    args: &AdminArgs,
    client: &mut LatticeGrpcClient,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match &args.command {
        AdminCommand::Drain(drain_args) => {
            let resp = client
                .drain_node(&drain_args.node_id, "operator-initiated drain")
                .await?;
            if resp.success {
                println!("Draining node: {}", drain_args.node_id);
                if resp.active_allocations > 0 {
                    println!(
                        "  {} active allocation(s) will complete before drain finishes",
                        resp.active_allocations
                    );
                }
            } else {
                eprintln!("Failed to drain node: {}", drain_args.node_id);
            }
        }
        AdminCommand::Undrain(undrain_args) => {
            let resp = client.undrain_node(&undrain_args.node_id).await?;
            if resp.success {
                println!("Undrained node: {}", undrain_args.node_id);
            } else {
                eprintln!("Failed to undrain node: {}", undrain_args.node_id);
            }
        }
        AdminCommand::Disable(disable_args) => {
            let reason = disable_args
                .reason
                .as_deref()
                .unwrap_or("operator-disabled");
            let resp = client.disable_node(&disable_args.node_id, reason).await?;
            if resp.success {
                println!("Disabled node: {}", disable_args.node_id);
            } else {
                eprintln!("Failed to disable node: {}", disable_args.node_id);
            }
        }
        AdminCommand::TenantCreate(tenant_args) => {
            let req = build_create_tenant_request(tenant_args);
            let resp = client.create_tenant(req).await?;
            match format {
                OutputFormat::Json => {
                    let json = serde_json::json!({
                        "tenant_id": resp.tenant_id,
                        "name": resp.name,
                        "isolation_level": resp.isolation_level,
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
                _ => {
                    println!("Created tenant: {} (id={})", resp.name, resp.tenant_id);
                }
            }
        }
        AdminCommand::RaftStatus => {
            let resp = client.get_raft_status().await?;
            match format {
                OutputFormat::Json => {
                    let members: Vec<serde_json::Value> = resp
                        .members
                        .iter()
                        .map(|m| {
                            serde_json::json!({
                                "node_id": m.node_id,
                                "address": m.address,
                                "role": m.role,
                                "match_index": m.match_index,
                            })
                        })
                        .collect();
                    let json = serde_json::json!({
                        "leader_id": resp.leader_id,
                        "current_term": resp.current_term,
                        "last_applied": resp.last_applied,
                        "commit_index": resp.commit_index,
                        "members": members,
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
                _ => {
                    println!("Raft Cluster Status");
                    println!("  Leader:       {}", resp.leader_id);
                    println!("  Term:         {}", resp.current_term);
                    println!("  Last Applied: {}", resp.last_applied);
                    println!("  Commit Index: {}", resp.commit_index);
                    println!("  Members:");
                    for m in &resp.members {
                        println!(
                            "    {} ({}) - {} [match_index={}]",
                            m.node_id, m.address, m.role, m.match_index
                        );
                    }
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
