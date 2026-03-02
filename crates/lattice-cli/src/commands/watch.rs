//! `lattice watch` — watch allocation state changes.
//!
//! Streams live state transitions and metric updates for an allocation.
//! See docs/architecture/observability.md.

use clap::Args;

/// Arguments for the watch command.
#[derive(Args, Debug)]
pub struct WatchArgs {
    /// Allocation ID to watch
    pub alloc_id: String,

    /// Show only alert events (anomalies, threshold breaches)
    #[arg(long)]
    pub alerts_only: bool,

    /// Metric to watch (e.g., gpu_util, mem_util, network_bw)
    #[arg(long)]
    pub metric: Option<String>,

    /// Exit after the allocation reaches this state
    #[arg(long)]
    pub until: Option<String>,
}

/// Execute the watch command (stub — real gRPC call will come later).
pub async fn execute(args: &WatchArgs) -> anyhow::Result<()> {
    println!("Watching allocation {}...", args.alloc_id);
    if args.alerts_only {
        println!("  Showing alerts only");
    }
    if let Some(ref metric) = args.metric {
        println!("  Tracking metric: {metric}");
    }
    if let Some(ref until) = args.until {
        println!("  Will stop when state reaches: {until}");
    }
    println!("Would connect to lattice-server and stream state changes (Ctrl+C to stop)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: WatchArgs,
    }

    #[test]
    fn parse_basic_watch() {
        let cli = TestCli::parse_from(["test", "alloc-123"]);
        assert_eq!(cli.args.alloc_id, "alloc-123");
        assert!(!cli.args.alerts_only);
        assert!(cli.args.metric.is_none());
        assert!(cli.args.until.is_none());
    }

    #[test]
    fn parse_watch_alerts_only() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--alerts-only"]);
        assert!(cli.args.alerts_only);
    }

    #[test]
    fn parse_watch_with_metric() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--metric", "gpu_util"]);
        assert_eq!(cli.args.metric.as_deref(), Some("gpu_util"));
    }

    #[test]
    fn parse_watch_with_until() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--until", "completed"]);
        assert_eq!(cli.args.until.as_deref(), Some("completed"));
    }

    #[test]
    fn parse_watch_all_options() {
        let cli = TestCli::parse_from([
            "test",
            "alloc-123",
            "--alerts-only",
            "--metric",
            "mem_util",
            "--until",
            "failed",
        ]);
        assert!(cli.args.alerts_only);
        assert_eq!(cli.args.metric.as_deref(), Some("mem_util"));
        assert_eq!(cli.args.until.as_deref(), Some("failed"));
    }
}
