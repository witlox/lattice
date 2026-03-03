//! `lattice logs` — stream or tail allocation logs.
//!
//! Retrieves logs from the dual-path log system: ring buffer for live streaming,
//! S3 for persistent historical logs. See docs/architecture/observability.md.

use clap::Args;
use tokio_stream::StreamExt;

use crate::client::LatticeGrpcClient;
use crate::convert::build_log_stream_request;

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

/// Execute the logs command: stream logs from an allocation via gRPC.
pub async fn execute(args: &LogsArgs, client: &mut LatticeGrpcClient) -> anyhow::Result<()> {
    let req = build_log_stream_request(args);
    let mut stream = client.stream_logs(req).await?;

    while let Some(entry) = stream.next().await {
        let entry = entry?;
        let data = String::from_utf8_lossy(&entry.data);
        if args.timestamps {
            if let Some(ref ts) = entry.timestamp {
                print!("[{}] ", ts.seconds);
            }
        }
        if !entry.node_id.is_empty() {
            print!("[{}] ", entry.node_id);
        }
        print!("{data}");
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
