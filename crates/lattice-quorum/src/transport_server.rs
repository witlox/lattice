//! Tonic server handler for Raft RPCs.
//!
//! Receives gRPC requests, deserializes them, forwards to the local Raft
//! instance, and serializes responses back.

use std::io::Cursor;

use openraft::raft::{AppendEntriesRequest, VoteRequest};
use openraft::storage::Snapshot;
use openraft::SnapshotMeta;
use tonic::{Request, Response, Status};

use crate::proto::raft_service_server::RaftService;
use crate::proto::{RaftPayload, SnapshotRequest};
use crate::{LatticeRaft, TypeConfig};

type VoteOf = openraft::vote::Vote<TypeConfig>;

/// Tonic service that forwards Raft RPCs to a local Raft instance.
pub struct RaftTransportServer {
    raft: LatticeRaft,
}

impl RaftTransportServer {
    pub fn new(raft: LatticeRaft) -> Self {
        Self { raft }
    }
}

#[tonic::async_trait]
impl RaftService for RaftTransportServer {
    async fn append_entries(
        &self,
        request: Request<RaftPayload>,
    ) -> Result<Response<RaftPayload>, Status> {
        let req: AppendEntriesRequest<TypeConfig> =
            serde_json::from_slice(&request.into_inner().data)
                .map_err(|e| Status::invalid_argument(format!("Deserialize error: {e}")))?;

        let resp = self
            .raft
            .append_entries(req)
            .await
            .map_err(|e| Status::internal(format!("Raft append_entries error: {e}")))?;

        let data = serde_json::to_vec(&resp)
            .map_err(|e| Status::internal(format!("Serialize error: {e}")))?;

        Ok(Response::new(RaftPayload { data }))
    }

    async fn vote(&self, request: Request<RaftPayload>) -> Result<Response<RaftPayload>, Status> {
        let req: VoteRequest<TypeConfig> = serde_json::from_slice(&request.into_inner().data)
            .map_err(|e| Status::invalid_argument(format!("Deserialize error: {e}")))?;

        let resp = self
            .raft
            .vote(req)
            .await
            .map_err(|e| Status::internal(format!("Raft vote error: {e}")))?;

        let data = serde_json::to_vec(&resp)
            .map_err(|e| Status::internal(format!("Serialize error: {e}")))?;

        Ok(Response::new(RaftPayload { data }))
    }

    async fn install_snapshot(
        &self,
        request: Request<SnapshotRequest>,
    ) -> Result<Response<RaftPayload>, Status> {
        let req = request.into_inner();

        let vote: VoteOf = serde_json::from_slice(&req.vote)
            .map_err(|e| Status::invalid_argument(format!("Deserialize vote error: {e}")))?;

        let meta: SnapshotMeta<TypeConfig> = serde_json::from_slice(&req.meta)
            .map_err(|e| Status::invalid_argument(format!("Deserialize meta error: {e}")))?;

        let snapshot = Snapshot {
            meta,
            snapshot: Cursor::new(req.snapshot_data),
        };

        let resp = self
            .raft
            .install_full_snapshot(vote, snapshot)
            .await
            .map_err(|e| Status::internal(format!("Raft install_snapshot error: {e}")))?;

        let data = serde_json::to_vec(&resp)
            .map_err(|e| Status::internal(format!("Serialize error: {e}")))?;

        Ok(Response::new(RaftPayload { data }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_meta_roundtrip() {
        use openraft::vote::leader_id_adv::CommittedLeaderId;
        use openraft::vote::RaftLeaderId;

        let meta: SnapshotMeta<TypeConfig> = SnapshotMeta {
            last_log_id: Some(openraft::LogId::new(CommittedLeaderId::new(1, 1), 5)),
            last_membership: openraft::StoredMembership::default(),
            snapshot_id: "test-snap-1".to_string(),
        };
        let data = serde_json::to_vec(&meta).unwrap();
        let decoded: SnapshotMeta<TypeConfig> = serde_json::from_slice(&data).unwrap();
        assert_eq!(decoded.snapshot_id, "test-snap-1");
        assert_eq!(decoded.last_log_id, meta.last_log_id);
    }

    #[test]
    fn vote_roundtrip() {
        let vote = openraft::vote::Vote::<TypeConfig>::new(3, 7);
        let data = serde_json::to_vec(&vote).unwrap();
        let decoded: VoteOf = serde_json::from_slice(&data).unwrap();
        assert_eq!(decoded, vote);
    }
}
