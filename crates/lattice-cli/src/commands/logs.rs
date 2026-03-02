//! `lattice logs` — stream or tail allocation logs.
//!
//! Retrieves logs from the dual-path log system: ring buffer for live streaming,
//! S3 for persistent historical logs. See docs/architecture/observability.md.

use clap::Args;

/// Arguments for the logs command.
#[derive(Args, Debug)]
pub struct LogsArgs {
    /// Allocation ID to retrieve logs for
    pub alloc_id: String,

    /// Follow log output (like `tail -f`)
    #[arg(short, long)]
    pub follow: bool,

    /// Show stderr instead of stdout
    #[arg(long)]
    pub stderr: bool,

    /// Specific node within the allocation
    #[arg(long)]
    pub node: Option<String>,

    /// Number of lines to show from the end
    #[arg(long)]
    pub tail: Option<u32>,

    /// Show timestamps on each log line
    #[arg(long)]
    pub timestamps: bool,
}

/// Execute the logs command (stub — real gRPC call will come later).
pub async fn execute(args: &LogsArgs) -> anyhow::Result<()> {
    let stream = if args.stderr { "stderr" } else { "stdout" };
    println!("Logs for allocation {} ({stream}):", args.alloc_id);
    if let Some(ref node) = args.node {
        println!("  Filtering to node: {node}");
    }
    if let Some(n) = args.tail {
        println!("  Showing last {n} lines");
    }
    if args.follow {
        println!("  Following output (Ctrl+C to stop)...");
    }
    if args.timestamps {
        println!("  Timestamps enabled");
    }
    println!("Would connect to lattice-server and stream logs");
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: LogsArgs,
    }

    #[test]
    fn parse_basic_logs() {
        let cli = TestCli::parse_from(["test", "alloc-123"]);
        assert_eq!(cli.args.alloc_id, "alloc-123");
        assert!(!cli.args.follow);
        assert!(!cli.args.stderr);
        assert!(cli.args.node.is_none());
        assert!(cli.args.tail.is_none());
    }

    #[test]
    fn parse_logs_follow() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--follow"]);
        assert!(cli.args.follow);
    }

    #[test]
    fn parse_logs_follow_short() {
        let cli = TestCli::parse_from(["test", "alloc-123", "-f"]);
        assert!(cli.args.follow);
    }

    #[test]
    fn parse_logs_stderr_and_node() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--stderr", "--node", "x1000c0s0b0n3"]);
        assert!(cli.args.stderr);
        assert_eq!(cli.args.node.as_deref(), Some("x1000c0s0b0n3"));
    }

    #[test]
    fn parse_logs_tail() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--tail", "100"]);
        assert_eq!(cli.args.tail, Some(100));
    }

    #[test]
    fn parse_logs_with_timestamps() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--follow", "--timestamps"]);
        assert!(cli.args.follow);
        assert!(cli.args.timestamps);
    }
}
