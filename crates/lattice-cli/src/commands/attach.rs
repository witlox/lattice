//! `lattice attach` — attach a terminal to a running allocation.
//!
//! Uses nsenter to join the allocation's mount namespace on the target node.
//! See docs/architecture/observability.md for details.

use clap::Args;

/// Arguments for the attach command.
#[derive(Args, Debug)]
pub struct AttachArgs {
    /// Allocation ID to attach to
    pub alloc_id: String,

    /// Specific node within the allocation
    #[arg(long)]
    pub node: Option<String>,

    /// Command to execute in the allocation's namespace (default: shell)
    #[arg(long)]
    pub command: Option<String>,
}

/// Execute the attach command (stub — real gRPC call will come later).
pub async fn execute(args: &AttachArgs) -> anyhow::Result<()> {
    println!("Attaching to allocation {}...", args.alloc_id);
    if let Some(ref node) = args.node {
        println!("  Target node: {node}");
    }
    if let Some(ref cmd) = args.command {
        println!("  Command: {cmd}");
    } else {
        println!("  Opening interactive shell");
    }
    println!("Would connect to lattice-server and attach via nsenter");
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: AttachArgs,
    }

    #[test]
    fn parse_basic_attach() {
        let cli = TestCli::parse_from(["test", "alloc-123"]);
        assert_eq!(cli.args.alloc_id, "alloc-123");
        assert!(cli.args.node.is_none());
        assert!(cli.args.command.is_none());
    }

    #[test]
    fn parse_attach_with_node() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--node", "x1000c0s0b0n3"]);
        assert_eq!(cli.args.alloc_id, "alloc-123");
        assert_eq!(cli.args.node.as_deref(), Some("x1000c0s0b0n3"));
    }

    #[test]
    fn parse_attach_with_command() {
        let cli = TestCli::parse_from(["test", "alloc-123", "--command", "nvidia-smi -l 1"]);
        assert_eq!(cli.args.alloc_id, "alloc-123");
        assert_eq!(cli.args.command.as_deref(), Some("nvidia-smi -l 1"));
    }

    #[test]
    fn parse_attach_with_all_options() {
        let cli = TestCli::parse_from([
            "test",
            "alloc-123",
            "--node",
            "x1000c0s0b0n3",
            "--command",
            "htop",
        ]);
        assert_eq!(cli.args.alloc_id, "alloc-123");
        assert_eq!(cli.args.node.as_deref(), Some("x1000c0s0b0n3"));
        assert_eq!(cli.args.command.as_deref(), Some("htop"));
    }
}
