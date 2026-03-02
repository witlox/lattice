//! `lattice top` — live resource usage dashboard.
//!
//! Displays a snapshot of resource usage across the cluster or for a specific
//! allocation. Similar to `htop` but for Lattice allocations.
//! See docs/architecture/observability.md.

use clap::Args;

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

/// Execute the top command (stub — real gRPC call will come later).
pub async fn execute(args: &TopArgs) -> anyhow::Result<()> {
    if let Some(ref alloc_id) = args.alloc_id {
        println!("Resource usage for allocation {alloc_id}:");
    } else {
        println!("Cluster-wide resource usage:");
    }
    if let Some(ref tenant) = args.tenant {
        println!("  Tenant filter: {tenant}");
    }
    if let Some(ref vcluster) = args.vcluster {
        println!("  vCluster filter: {vcluster}");
    }
    if args.per_gpu {
        println!("  Per-GPU breakdown enabled");
    }
    println!("  Refresh interval: {}s", args.interval);
    println!("  Sort by: {}", args.sort);
    println!("Would connect to lattice-server and stream metrics");
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
