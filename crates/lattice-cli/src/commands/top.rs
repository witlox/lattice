//! `lattice top` — live resource usage dashboard.
//!
//! Displays a snapshot of resource usage across the cluster or for a specific
//! allocation. Similar to `htop` but for Lattice allocations.
//! See docs/architecture/observability.md.

use clap::Args;

use crate::client::LatticeGrpcClient;
use crate::convert::build_query_metrics_request;
use crate::output::OutputFormat;

/// Arguments for the top command.
#[derive(Args, Debug)]
pub struct TopArgs {
    /// Allocation ID (omit for cluster-wide view)
    pub alloc_id: Option<String>,

    /// Filter by tenant
    #[arg(short, long)]
    pub tenant: Option<String>,

    /// Filter by vCluster
    #[arg(long)]
    pub vcluster: Option<String>,

    /// Show per-GPU breakdown
    #[arg(long)]
    pub per_gpu: bool,

    /// Refresh interval in seconds (0 for one-shot)
    #[arg(long, default_value = "5")]
    pub interval: u64,

    /// Sort by field: gpu_util, mem_util, network, name
    #[arg(long, default_value = "gpu_util")]
    pub sort: String,
}

/// Execute the top command: query metrics snapshot via gRPC.
pub async fn execute(
    args: &TopArgs,
    client: &mut LatticeGrpcClient,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let alloc_id = match &args.alloc_id {
        Some(id) => id.clone(),
        None => {
            eprintln!("Cluster-wide top requires an allocation ID for now.");
            return Ok(());
        }
    };

    let req = build_query_metrics_request(&alloc_id);
    let snapshot = client.query_metrics(req).await?;

    match format {
        OutputFormat::Json => {
            let json = serde_json::json!({
                "allocation_id": snapshot.allocation_id,
                "summary": snapshot.summary.as_ref().map(|s| serde_json::json!({
                    "gpu_utilization_mean": s.gpu_utilization_mean,
                    "cpu_utilization_mean": s.cpu_utilization_mean,
                    "memory_used_bytes": s.memory_used_bytes,
                    "memory_total_bytes": s.memory_total_bytes,
                })),
                "nodes": snapshot.nodes.len(),
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        _ => {
            println!("Resource usage for allocation {}:", snapshot.allocation_id);
            if let Some(ref summary) = snapshot.summary {
                println!("  GPU Utilization: {:.1}%", summary.gpu_utilization_mean);
                println!("  CPU Utilization: {:.1}%", summary.cpu_utilization_mean);
                if summary.memory_total_bytes > 0 {
                    let pct = (summary.memory_used_bytes as f64
                        / summary.memory_total_bytes as f64)
                        * 100.0;
                    println!("  Memory: {pct:.1}% used");
                }
            }
            for node in &snapshot.nodes {
                println!("  Node {}: CPU {:.1}%", node.node_id, node.cpu_utilization);
                for gpu in &node.gpus {
                    println!(
                        "    GPU {}: {:.1}% util, {:.0}W, {:.0}C",
                        gpu.index, gpu.utilization, gpu.power_draw_watts, gpu.temperature_celsius
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
        args: TopArgs,
    }

    #[test]
    fn parse_cluster_wide_top() {
        let cli = TestCli::parse_from(["test"]);
        assert!(cli.args.alloc_id.is_none());
        assert!(cli.args.tenant.is_none());
        assert_eq!(cli.args.interval, 5);
        assert_eq!(cli.args.sort, "gpu_util");
    }

    #[test]
    fn parse_top_with_alloc_id() {
        let cli = TestCli::parse_from(["test", "alloc-123"]);
        assert_eq!(cli.args.alloc_id.as_deref(), Some("alloc-123"));
    }

    #[test]
    fn parse_top_with_tenant() {
        let cli = TestCli::parse_from(["test", "--tenant", "physics"]);
        assert_eq!(cli.args.tenant.as_deref(), Some("physics"));
    }

    #[test]
    fn parse_top_with_tenant_short() {
        let cli = TestCli::parse_from(["test", "-t", "physics"]);
        assert_eq!(cli.args.tenant.as_deref(), Some("physics"));
    }

    #[test]
    fn parse_top_per_gpu() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--per-gpu"]);
        assert!(cli.args.per_gpu);
    }

    #[test]
    fn parse_top_custom_interval() {
        let cli = TestCli::parse_from(["test", "--interval", "10"]);
        assert_eq!(cli.args.interval, 10);
    }

    #[test]
    fn parse_top_sort_by_mem() {
        let cli = TestCli::parse_from(["test", "--sort", "mem_util"]);
        assert_eq!(cli.args.sort, "mem_util");
    }
}
