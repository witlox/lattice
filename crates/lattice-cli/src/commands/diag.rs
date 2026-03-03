//! `lattice diag` — show diagnostics for an allocation.
//!
//! Displays network topology, storage I/O patterns, and health diagnostics.
//! See docs/architecture/observability.md.

use clap::Args;

use crate::client::LatticeGrpcClient;
use crate::convert::build_diagnostics_request;
use crate::output::OutputFormat;

/// Arguments for the diag command.
#[derive(Args, Debug)]
pub struct DiagArgs {
    /// Allocation ID to diagnose
    pub alloc_id: String,

    /// Show network diagnostics (Slingshot/Ultra Ethernet topology, latency)
    #[arg(long)]
    pub network: bool,

    /// Show storage I/O diagnostics (VAST throughput, latency, QoS)
    #[arg(long)]
    pub storage: bool,

    /// Show GPU health diagnostics (ECC errors, thermal throttling)
    #[arg(long)]
    pub gpu: bool,

    /// Show all diagnostics (default if no specific flag is set)
    #[arg(long)]
    pub all: bool,
}

impl DiagArgs {
    /// Returns true if all diagnostic categories should be shown.
    /// This is the case when `--all` is set, or when no specific category flag is given.
    pub fn show_all(&self) -> bool {
        self.all || !(self.network || self.storage || self.gpu)
    }
}

/// Execute the diag command: get diagnostics for an allocation via gRPC.
pub async fn execute(
    args: &DiagArgs,
    client: &mut LatticeGrpcClient,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let req = build_diagnostics_request(args);
    let resp = client.get_diagnostics(req).await?;

    match format {
        OutputFormat::Json => {
            let json = serde_json::json!({
                "allocation_id": resp.allocation_id,
                "network": resp.network.as_ref().map(|n| serde_json::json!({
                    "group_span": n.group_span,
                    "congestion_avg": n.csig_congestion_avg,
                    "inter_node_bw_gbps": n.inter_node_bandwidth_gbps,
                    "target_bw_gbps": n.target_bandwidth_gbps,
                    "network_errors": n.network_errors,
                })),
                "storage": resp.storage.as_ref().map(|s| {
                    let mounts: Vec<serde_json::Value> = s.mounts.iter().map(|m| serde_json::json!({
                        "path": m.mount_path,
                        "type": m.mount_type,
                        "read_gbps": m.read_throughput_gbps,
                        "write_gbps": m.write_throughput_gbps,
                        "health": m.health,
                    })).collect();
                    serde_json::json!({ "mounts": mounts })
                }),
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        _ => {
            println!("Diagnostics for allocation {}:", resp.allocation_id);

            if let Some(ref net) = resp.network {
                println!("  [Network]");
                println!("    Dragonfly groups spanned: {}", net.group_span);
                println!(
                    "    Inter-node bandwidth: {:.2} / {:.2} Gbps",
                    net.inter_node_bandwidth_gbps, net.target_bandwidth_gbps
                );
                println!("    Congestion (CSIG avg): {:.3}", net.csig_congestion_avg);
                if net.network_errors > 0 {
                    println!("    Network errors: {}", net.network_errors);
                }
            }

            if let Some(ref storage) = resp.storage {
                println!("  [Storage]");
                for m in &storage.mounts {
                    println!(
                        "    {} ({}) - R: {:.2} Gbps, W: {:.2} Gbps [{}]",
                        m.mount_path,
                        m.mount_type,
                        m.read_throughput_gbps,
                        m.write_throughput_gbps,
                        m.health,
                    );
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
        #[command(flatten)]
        args: DiagArgs,
    }

    #[test]
    fn parse_basic_diag() {
        let cli = TestCli::parse_from(["test", "alloc-123"]);
        assert_eq!(cli.args.alloc_id, "alloc-123");
        assert!(!cli.args.network);
        assert!(!cli.args.storage);
        assert!(!cli.args.gpu);
        assert!(!cli.args.all);
    }

    #[test]
    fn show_all_when_no_flags() {
        let cli = TestCli::parse_from(["test", "alloc-123"]);
        assert!(cli.args.show_all());
    }

    #[test]
    fn show_all_explicit() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--all"]);
        assert!(cli.args.show_all());
    }

    #[test]
    fn specific_category_disables_implicit_all() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--network"]);
        assert!(!cli.args.show_all());
        assert!(cli.args.network);
    }

    #[test]
    fn parse_diag_network_only() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--network"]);
        assert!(cli.args.network);
        assert!(!cli.args.storage);
        assert!(!cli.args.gpu);
    }

    #[test]
    fn parse_diag_multiple_categories() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--network", "--gpu"]);
        assert!(cli.args.network);
        assert!(!cli.args.storage);
        assert!(cli.args.gpu);
    }
}
