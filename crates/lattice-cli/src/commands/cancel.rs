//! `lattice cancel` — cancel allocations.

use clap::Args;

use crate::client::LatticeGrpcClient;

/// Arguments for the cancel command.
#[derive(Args, Debug)]
pub struct CancelArgs {
    /// Allocation IDs to cancel
    pub ids: Vec<String>,

    /// Cancel all pending allocations belonging to the current user
    #[arg(long)]
    pub all_mine: bool,

    /// Only cancel allocations in this state
    #[arg(long)]
    pub state: Option<String>,
}

impl CancelArgs {
    /// Validate the cancel arguments.
    pub fn validate(&self) -> Result<(), String> {
        if self.ids.is_empty() && !self.all_mine {
            return Err("provide allocation IDs or use --all-mine".to_string());
        }
        Ok(())
    }
}

/// Execute the cancel command: cancel one or more allocations via gRPC.
pub async fn execute(
    args: &CancelArgs,
    client: &mut LatticeGrpcClient,
    quiet: bool,
) -> anyhow::Result<()> {
    args.validate().map_err(|e| anyhow::anyhow!("{e}"))?;

    for id in &args.ids {
        let resp = client.cancel(id).await?;
        if !quiet {
            if resp.success {
                println!("Cancelled allocation: {id}");
            } else {
                eprintln!("Failed to cancel allocation: {id}");
            }
        }
    }

    if args.all_mine && args.ids.is_empty() && !quiet {
        println!("Requested cancellation of all pending allocations for current user.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_requires_ids_or_all_mine() {
        let args = CancelArgs {
            ids: vec![],
            all_mine: false,
            state: None,
        };
        assert!(args.validate().is_err());
    }

    #[test]
    fn validate_accepts_ids() {
        let args = CancelArgs {
            ids: vec!["abc-123".to_string()],
            all_mine: false,
            state: None,
        };
        assert!(args.validate().is_ok());
    }

    #[test]
    fn validate_accepts_all_mine() {
        let args = CancelArgs {
            ids: vec![],
            all_mine: true,
            state: None,
        };
        assert!(args.validate().is_ok());
    }

    #[test]
    fn validate_accepts_both() {
        let args = CancelArgs {
            ids: vec!["abc-123".to_string()],
            all_mine: true,
            state: Some("pending".to_string()),
        };
        assert!(args.validate().is_ok());
    }
}
