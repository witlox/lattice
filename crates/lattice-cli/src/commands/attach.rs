//! `lattice attach` — attach a terminal to a running allocation.
//!
//! Uses nsenter to join the allocation's mount namespace on the target node.
//! See docs/architecture/observability.md for details.

use clap::Args;
use tokio_stream::StreamExt;

use crate::client::LatticeGrpcClient;
use lattice_common::proto::lattice::v1 as pb;

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

/// Execute the attach command via gRPC bidirectional streaming.
pub async fn execute(args: &AttachArgs, client: &mut LatticeGrpcClient) -> anyhow::Result<()> {
    eprintln!("Attaching to allocation {}...", args.alloc_id);
    if let Some(ref node) = args.node {
        eprintln!("  Target node: {node}");
    }
    if let Some(ref cmd) = args.command {
        eprintln!("  Command: {cmd}");
    } else {
        eprintln!("  Opening interactive shell");
    }

    let start = pb::AttachInput {
        input: Some(pb::attach_input::Input::Start(pb::AttachStart {
            allocation_id: args.alloc_id.clone(),
            node_id: args.node.clone().unwrap_or_default(),
            command: args.command.clone().unwrap_or_default(),
            rows: 24,
            cols: 80,
        })),
    };

    let input_stream = tokio_stream::once(start);

    let mut output_stream: tonic::Streaming<pb::AttachOutput> =
        client.allocations.attach(input_stream).await?.into_inner();

    while let Some(msg) = output_stream.next().await {
        let msg = msg?;
        match msg.output {
            Some(pb::attach_output::Output::Data(bytes)) => {
                use std::io::Write;
                std::io::stdout().write_all(&bytes)?;
                std::io::stdout().flush()?;
            }
            Some(pb::attach_output::Output::ExitCode(code)) => {
                eprintln!("\nProcess exited with code {code}");
                if code != 0 {
                    std::process::exit(code);
                }
                break;
            }
            Some(pb::attach_output::Output::Error(err)) => {
                eprintln!("Attach error: {err}");
                std::process::exit(1);
            }
            None => {}
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
