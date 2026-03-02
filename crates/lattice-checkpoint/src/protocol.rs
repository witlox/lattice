//! Checkpoint communication protocol.
//!
//! Defines the three modes an application can use to receive
//! checkpoint hints: signal, shared memory, or gRPC callback.

use lattice_common::types::*;

/// Communication mode for checkpoint coordination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointProtocol {
    /// SIGUSR1 sent to process group, completion via sentinel file.
    Signal,
    /// Shared memory flag at a well-known path.
    SharedMemory { shm_path: String },
    /// gRPC callback to application-registered endpoint.
    GrpcCallback { endpoint: String },
}

/// A checkpoint request issued by the broker to the node agent.
#[derive(Debug, Clone)]
pub struct CheckpointRequest {
    pub allocation_id: AllocId,
    pub node_id: NodeId,
    pub protocol: CheckpointProtocol,
    pub timeout_seconds: u64,
    pub checkpoint_id: String,
    pub destination: CheckpointDestination,
}

/// Where the checkpoint should be written.
#[derive(Debug, Clone)]
pub enum CheckpointDestination {
    S3 { path: String },
    Nfs { path: String },
}

/// Response from the node agent after a checkpoint attempt.
#[derive(Debug, Clone)]
pub enum CheckpointResponse {
    /// Checkpoint completed successfully.
    Completed {
        checkpoint_id: String,
        size_bytes: u64,
        duration_seconds: f64,
    },
    /// Application requested deferral (gRPC callback mode only).
    Deferred {
        reason: String,
        retry_after_seconds: u64,
    },
    /// Checkpoint timed out.
    TimedOut,
    /// Checkpoint failed.
    Failed { reason: String },
}

/// Determine the protocol for an allocation based on its configuration.
pub fn resolve_protocol(alloc: &Allocation) -> Option<CheckpointProtocol> {
    match alloc.checkpoint {
        CheckpointStrategy::Auto => Some(CheckpointProtocol::Signal),
        CheckpointStrategy::Manual => {
            // Manual: application registered its own protocol
            // Default to signal for now
            Some(CheckpointProtocol::Signal)
        }
        CheckpointStrategy::None => None,
    }
}

/// Generate the checkpoint destination path.
pub fn checkpoint_destination(
    alloc: &Allocation,
    checkpoint_id: &str,
    use_nfs: bool,
) -> CheckpointDestination {
    if use_nfs {
        CheckpointDestination::Nfs {
            path: format!(
                "/scratch/{}/{}/{}/checkpoints/{}/",
                alloc.tenant, alloc.project, alloc.id, checkpoint_id
            ),
        }
    } else {
        CheckpointDestination::S3 {
            path: format!(
                "s3://{}/{}/{}/checkpoints/{}/",
                alloc.tenant, alloc.project, alloc.id, checkpoint_id
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::AllocationBuilder;

    #[test]
    fn auto_checkpoint_uses_signal() {
        let mut alloc = AllocationBuilder::new().build();
        alloc.checkpoint = CheckpointStrategy::Auto;
        let protocol = resolve_protocol(&alloc);
        assert_eq!(protocol, Some(CheckpointProtocol::Signal));
    }

    #[test]
    fn none_checkpoint_returns_none() {
        let mut alloc = AllocationBuilder::new().build();
        alloc.checkpoint = CheckpointStrategy::None;
        let protocol = resolve_protocol(&alloc);
        assert!(protocol.is_none());
    }

    #[test]
    fn s3_destination_format() {
        let alloc = AllocationBuilder::new()
            .tenant("hospital-a")
            .project("research")
            .build();
        let dest = checkpoint_destination(&alloc, "ckpt-001", false);
        if let CheckpointDestination::S3 { path } = dest {
            assert!(path.starts_with("s3://hospital-a/research/"));
            assert!(path.contains("ckpt-001"));
        } else {
            panic!("Expected S3 destination");
        }
    }

    #[test]
    fn nfs_destination_format() {
        let alloc = AllocationBuilder::new()
            .tenant("physics")
            .project("sim")
            .build();
        let dest = checkpoint_destination(&alloc, "ckpt-002", true);
        if let CheckpointDestination::Nfs { path } = dest {
            assert!(path.starts_with("/scratch/physics/sim/"));
            assert!(path.contains("ckpt-002"));
        } else {
            panic!("Expected NFS destination");
        }
    }
}
