//! `lattice diag` — show diagnostics for an allocation.
//!
//! Displays network topology, storage I/O patterns, and health diagnostics.
//! See docs/architecture/observability.md.

use clap::Args;

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

/// Execute the diag command (stub — real gRPC call will come later).
pub async fn execute(args: &DiagArgs) -> anyhow::Result<()> {
    println!("Diagnostics for allocation {}:", args.alloc_id);

    let show_all = args.show_all();

    if show_all || args.network {
        println!("  [Network]");
        println!("    Would show Slingshot/UE topology and inter-node latency");
    }
    if show_all || args.storage {
        println!("  [Storage]");
        println!("    Would show VAST I/O throughput and latency");
    }
    if show_all || args.gpu {
        println!("  [GPU]");
        println!("    Would show GPU health, ECC errors, thermal state");
    }
    println!("Would connect to lattice-server and retrieve diagnostics");
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
