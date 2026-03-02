//! AllocationService gRPC implementation.
//!
//! Implements all 18 RPCs defined in allocations.proto.

use std::pin::Pin;
use std::sync::Arc;

use tokio_stream::Stream;
use tonic::{Request, Response, Status};

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::proto::lattice::v1::allocation_service_server::AllocationService;

use crate::convert;
use crate::state::ApiState;

/// AllocationService backed by trait-object stores.
pub struct LatticeAllocationService {
    state: Arc<ApiState>,
}

impl LatticeAllocationService {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }

    fn extract_user(req: &tonic::metadata::MetadataMap) -> String {
        req.get("x-lattice-user")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("anonymous")
            .to_string()
    }
}

type StreamPin<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl AllocationService for LatticeAllocationService {
    async fn submit(
        &self,
        request: Request<pb::SubmitRequest>,
    ) -> Result<Response<pb::SubmitResponse>, Status> {
        let user = Self::extract_user(request.metadata());
        let req = request.into_inner();

        match req.submission {
            Some(pb::submit_request::Submission::Single(spec)) => {
                let alloc = convert::allocation_from_proto(&spec, &user)
                    .map_err(Status::invalid_argument)?;
                let id = alloc.id.to_string();

                self.state
                    .allocations
                    .insert(alloc)
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?;

                Ok(Response::new(pb::SubmitResponse {
                    allocation_ids: vec![id],
                    dag_id: String::new(),
                }))
            }
            Some(pb::submit_request::Submission::Dag(dag)) => {
                let dag_id = uuid::Uuid::new_v4().to_string();
                let mut ids = Vec::new();

                for spec in &dag.allocations {
                    let mut alloc = convert::allocation_from_proto(spec, &user)
                        .map_err(Status::invalid_argument)?;
                    alloc.dag_id = Some(dag_id.clone());
                    ids.push(alloc.id.to_string());

                    self.state
                        .allocations
                        .insert(alloc)
                        .await
                        .map_err(|e| Status::internal(e.to_string()))?;
                }

                Ok(Response::new(pb::SubmitResponse {
                    allocation_ids: ids,
                    dag_id,
                }))
            }
            Some(pb::submit_request::Submission::TaskGroup(tg)) => {
                let template = tg
                    .template
                    .as_ref()
                    .ok_or_else(|| Status::invalid_argument("task group requires a template"))?;
                let mut ids = Vec::new();
                let step = tg.step.max(1);

                let mut i = tg.range_start;
                while i <= tg.range_end {
                    let mut alloc = convert::allocation_from_proto(template, &user)
                        .map_err(Status::invalid_argument)?;
                    alloc.allocation_type = lattice_common::types::AllocationType::TaskGroup {
                        range_start: tg.range_start,
                        range_end: tg.range_end,
                        step,
                        max_concurrent: tg.max_concurrent,
                    };
                    ids.push(alloc.id.to_string());

                    self.state
                        .allocations
                        .insert(alloc)
                        .await
                        .map_err(|e| Status::internal(e.to_string()))?;
                    i += step;
                }

                Ok(Response::new(pb::SubmitResponse {
                    allocation_ids: ids,
                    dag_id: String::new(),
                }))
            }
            None => Err(Status::invalid_argument("submission is required")),
        }
    }

    async fn get(
        &self,
        request: Request<pb::GetAllocationRequest>,
    ) -> Result<Response<pb::AllocationStatus>, Status> {
        let req = request.into_inner();
        let id = uuid::Uuid::parse_str(&req.allocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid allocation id: {e}")))?;

        let alloc = self
            .state
            .allocations
            .get(&id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(convert::allocation_to_status(&alloc)))
    }

    async fn list(
        &self,
        request: Request<pb::ListAllocationsRequest>,
    ) -> Result<Response<pb::ListAllocationsResponse>, Status> {
        let req = request.into_inner();
        let filter = convert::filter_from_list_request(&req);

        let allocations = self
            .state
            .allocations
            .list(&filter)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let statuses: Vec<pb::AllocationStatus> = allocations
            .iter()
            .map(convert::allocation_to_status)
            .collect();

        Ok(Response::new(pb::ListAllocationsResponse {
            allocations: statuses,
            next_cursor: String::new(),
        }))
    }

    async fn cancel(
        &self,
        request: Request<pb::CancelRequest>,
    ) -> Result<Response<pb::CancelResponse>, Status> {
        let req = request.into_inner();
        let id = uuid::Uuid::parse_str(&req.allocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid allocation id: {e}")))?;

        self.state
            .allocations
            .update_state(&id, lattice_common::types::AllocationState::Cancelled)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::CancelResponse { success: true }))
    }

    async fn update(
        &self,
        request: Request<pb::UpdateAllocationRequest>,
    ) -> Result<Response<pb::AllocationStatus>, Status> {
        let req = request.into_inner();
        let id = uuid::Uuid::parse_str(&req.allocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid allocation id: {e}")))?;

        // Fetch current allocation, apply updates
        let alloc = self
            .state
            .allocations
            .get(&id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        // For now, we only support telemetry mode updates
        // (walltime extension etc. would require quorum proposal)
        Ok(Response::new(convert::allocation_to_status(&alloc)))
    }

    async fn launch_tasks(
        &self,
        request: Request<pb::LaunchTasksRequest>,
    ) -> Result<Response<pb::LaunchTasksResponse>, Status> {
        let req = request.into_inner();
        let _id = uuid::Uuid::parse_str(&req.allocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid allocation id: {e}")))?;

        // Task launching is delegated to the node agent.
        // Here we just acknowledge the request.
        Ok(Response::new(pb::LaunchTasksResponse {
            task_launch_id: uuid::Uuid::new_v4().to_string(),
        }))
    }

    // ── Streaming RPCs ──────────────────────────────────────

    type WatchStream = StreamPin<pb::AllocationEvent>;

    async fn watch(
        &self,
        _request: Request<pb::WatchRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        // Return an empty stream — event publishing requires the
        // scheduling loop to push events, which will be wired in Phase 8.
        let stream = tokio_stream::empty();
        Ok(Response::new(Box::pin(stream)))
    }

    async fn checkpoint(
        &self,
        request: Request<pb::CheckpointRequest>,
    ) -> Result<Response<pb::CheckpointResponse>, Status> {
        let req = request.into_inner();
        let id = uuid::Uuid::parse_str(&req.allocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid allocation id: {e}")))?;

        self.state
            .checkpoint
            .initiate_checkpoint(&id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::CheckpointResponse {
            accepted: true,
            message: String::new(),
        }))
    }

    type AttachStream = StreamPin<pb::AttachOutput>;

    async fn attach(
        &self,
        _request: Request<tonic::Streaming<pb::AttachInput>>,
    ) -> Result<Response<Self::AttachStream>, Status> {
        // Attach is forwarded to the node agent — not yet implemented.
        Err(Status::unimplemented(
            "attach requires node agent integration",
        ))
    }

    type StreamLogsStream = StreamPin<pb::LogEntry>;

    async fn stream_logs(
        &self,
        _request: Request<pb::LogStreamRequest>,
    ) -> Result<Response<Self::StreamLogsStream>, Status> {
        let stream = tokio_stream::empty();
        Ok(Response::new(Box::pin(stream)))
    }

    async fn query_metrics(
        &self,
        _request: Request<pb::QueryMetricsRequest>,
    ) -> Result<Response<pb::MetricsSnapshot>, Status> {
        // Metrics are queried from TSDB — not yet integrated.
        Err(Status::unimplemented(
            "metrics query requires TSDB integration",
        ))
    }

    type StreamMetricsStream = StreamPin<pb::MetricsEvent>;

    async fn stream_metrics(
        &self,
        _request: Request<pb::StreamMetricsRequest>,
    ) -> Result<Response<Self::StreamMetricsStream>, Status> {
        let stream = tokio_stream::empty();
        Ok(Response::new(Box::pin(stream)))
    }

    async fn get_diagnostics(
        &self,
        _request: Request<pb::DiagnosticsRequest>,
    ) -> Result<Response<pb::DiagnosticsResponse>, Status> {
        Err(Status::unimplemented(
            "diagnostics requires node agent integration",
        ))
    }

    async fn compare_metrics(
        &self,
        _request: Request<pb::CompareMetricsRequest>,
    ) -> Result<Response<pb::CompareMetricsResponse>, Status> {
        Err(Status::unimplemented(
            "compare metrics requires TSDB integration",
        ))
    }

    // ── DAG RPCs ──────────────────────────────────────────────

    async fn get_dag(
        &self,
        request: Request<pb::GetDagRequest>,
    ) -> Result<Response<pb::DagStatus>, Status> {
        let req = request.into_inner();
        let filter = lattice_common::traits::AllocationFilter::default();
        let all = self
            .state
            .allocations
            .list(&filter)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let dag_allocs: Vec<_> = all
            .iter()
            .filter(|a| a.dag_id.as_deref() == Some(&req.dag_id))
            .collect();

        if dag_allocs.is_empty() {
            return Err(Status::not_found(format!("DAG {} not found", req.dag_id)));
        }

        let overall_state = if dag_allocs.iter().all(|a| a.state.is_terminal()) {
            "completed"
        } else if dag_allocs
            .iter()
            .any(|a| a.state == lattice_common::types::AllocationState::Running)
        {
            "running"
        } else {
            "pending"
        };

        let created_at = dag_allocs.first().map(|a| prost_types::Timestamp {
            seconds: a.created_at.timestamp(),
            nanos: 0,
        });
        let completed_at = dag_allocs
            .iter()
            .filter_map(|a| a.completed_at)
            .max()
            .map(|dt| prost_types::Timestamp {
                seconds: dt.timestamp(),
                nanos: 0,
            });

        Ok(Response::new(pb::DagStatus {
            dag_id: req.dag_id,
            state: overall_state.to_string(),
            allocations: dag_allocs
                .iter()
                .map(|a| convert::allocation_to_status(a))
                .collect(),
            created_at,
            completed_at,
        }))
    }

    async fn list_dags(
        &self,
        request: Request<pb::ListDagsRequest>,
    ) -> Result<Response<pb::ListDagsResponse>, Status> {
        let req = request.into_inner();
        let filter = lattice_common::traits::AllocationFilter {
            tenant: if req.tenant.is_empty() {
                None
            } else {
                Some(req.tenant)
            },
            ..Default::default()
        };

        let all = self
            .state
            .allocations
            .list(&filter)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        // Group by dag_id
        let mut dags: std::collections::HashMap<String, Vec<&lattice_common::types::Allocation>> =
            std::collections::HashMap::new();
        for alloc in &all {
            if let Some(dag_id) = &alloc.dag_id {
                dags.entry(dag_id.clone()).or_default().push(alloc);
            }
        }

        let dag_statuses: Vec<pb::DagStatus> = dags
            .into_iter()
            .map(|(dag_id, allocs)| {
                let overall_state = if allocs.iter().all(|a| a.state.is_terminal()) {
                    "completed"
                } else if allocs
                    .iter()
                    .any(|a| a.state == lattice_common::types::AllocationState::Running)
                {
                    "running"
                } else {
                    "pending"
                };
                let created_at = allocs.first().map(|a| prost_types::Timestamp {
                    seconds: a.created_at.timestamp(),
                    nanos: 0,
                });
                let completed_at = allocs
                    .iter()
                    .filter_map(|a| a.completed_at)
                    .max()
                    .map(|dt| prost_types::Timestamp {
                        seconds: dt.timestamp(),
                        nanos: 0,
                    });
                pb::DagStatus {
                    dag_id,
                    state: overall_state.to_string(),
                    allocations: allocs
                        .iter()
                        .map(|a| convert::allocation_to_status(a))
                        .collect(),
                    created_at,
                    completed_at,
                }
            })
            .collect();

        Ok(Response::new(pb::ListDagsResponse {
            dags: dag_statuses,
            next_cursor: String::new(),
        }))
    }

    async fn cancel_dag(
        &self,
        request: Request<pb::CancelDagRequest>,
    ) -> Result<Response<pb::CancelDagResponse>, Status> {
        let req = request.into_inner();
        let filter = lattice_common::traits::AllocationFilter::default();

        let all = self
            .state
            .allocations
            .list(&filter)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let mut cancelled = 0u32;
        for alloc in &all {
            if alloc.dag_id.as_deref() == Some(&req.dag_id) && !alloc.state.is_terminal() {
                if let Ok(()) = self
                    .state
                    .allocations
                    .update_state(&alloc.id, lattice_common::types::AllocationState::Cancelled)
                    .await
                {
                    cancelled += 1;
                }
            }
        }

        Ok(Response::new(pb::CancelDagResponse {
            success: cancelled > 0,
            allocations_cancelled: cancelled,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_common::types::AllocationState;
    use lattice_test_harness::fixtures::AllocationBuilder;
    use lattice_test_harness::mocks::{
        MockAllocationStore, MockAuditLog, MockCheckpointBroker, MockNodeRegistry,
    };

    fn test_state() -> Arc<ApiState> {
        Arc::new(ApiState {
            allocations: Arc::new(MockAllocationStore::new()),
            nodes: Arc::new(MockNodeRegistry::new()),
            audit: Arc::new(MockAuditLog::new()),
            checkpoint: Arc::new(MockCheckpointBroker::new()),
        })
    }

    fn single_submit_request(tenant: &str) -> pb::SubmitRequest {
        pb::SubmitRequest {
            submission: Some(pb::submit_request::Submission::Single(pb::AllocationSpec {
                tenant: tenant.to_string(),
                project: "test".to_string(),
                entrypoint: "python train.py".to_string(),
                ..Default::default()
            })),
        }
    }

    #[tokio::test]
    async fn submit_single_allocation() {
        let state = test_state();
        let svc = LatticeAllocationService::new(state);

        let resp = svc
            .submit(Request::new(single_submit_request("physics")))
            .await
            .unwrap();
        assert_eq!(resp.get_ref().allocation_ids.len(), 1);
        assert!(resp.get_ref().dag_id.is_empty());
    }

    #[tokio::test]
    async fn submit_dag() {
        let state = test_state();
        let svc = LatticeAllocationService::new(state);

        let req = pb::SubmitRequest {
            submission: Some(pb::submit_request::Submission::Dag(pb::DagSpec {
                allocations: vec![
                    pb::AllocationSpec {
                        id: "step1".to_string(),
                        tenant: "ml".to_string(),
                        ..Default::default()
                    },
                    pb::AllocationSpec {
                        id: "step2".to_string(),
                        tenant: "ml".to_string(),
                        depends_on: vec![pb::DependencySpec {
                            ref_id: "step1".to_string(),
                            condition: "success".to_string(),
                        }],
                        ..Default::default()
                    },
                ],
            })),
        };

        let resp = svc.submit(Request::new(req)).await.unwrap();
        assert_eq!(resp.get_ref().allocation_ids.len(), 2);
        assert!(!resp.get_ref().dag_id.is_empty());
    }

    #[tokio::test]
    async fn get_allocation() {
        let state = test_state();

        // Insert an allocation directly
        let alloc = AllocationBuilder::new()
            .tenant("physics")
            .state(AllocationState::Running)
            .build();
        let id = alloc.id;
        state.allocations.insert(alloc).await.unwrap();

        let svc = LatticeAllocationService::new(state);
        let resp = svc
            .get(Request::new(pb::GetAllocationRequest {
                allocation_id: id.to_string(),
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().state, "running");
    }

    #[tokio::test]
    async fn get_allocation_not_found() {
        let state = test_state();
        let svc = LatticeAllocationService::new(state);

        let result = svc
            .get(Request::new(pb::GetAllocationRequest {
                allocation_id: uuid::Uuid::new_v4().to_string(),
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn list_allocations() {
        let state = test_state();

        let alloc = AllocationBuilder::new().tenant("physics").build();
        state.allocations.insert(alloc).await.unwrap();

        let svc = LatticeAllocationService::new(state);
        let resp = svc
            .list(Request::new(pb::ListAllocationsRequest {
                tenant: "physics".to_string(),
                ..Default::default()
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().allocations.len(), 1);
    }

    #[tokio::test]
    async fn cancel_allocation() {
        let state = test_state();

        let alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let id = alloc.id;
        state.allocations.insert(alloc).await.unwrap();

        let svc = LatticeAllocationService::new(state);
        let resp = svc
            .cancel(Request::new(pb::CancelRequest {
                allocation_id: id.to_string(),
            }))
            .await
            .unwrap();

        assert!(resp.get_ref().success);
    }

    #[tokio::test]
    async fn checkpoint_allocation() {
        let state = test_state();
        let svc = LatticeAllocationService::new(state);

        let resp = svc
            .checkpoint(Request::new(pb::CheckpointRequest {
                allocation_id: uuid::Uuid::new_v4().to_string(),
            }))
            .await
            .unwrap();

        assert!(resp.get_ref().accepted);
    }

    #[tokio::test]
    async fn submit_empty_rejected() {
        let state = test_state();
        let svc = LatticeAllocationService::new(state);

        let result = svc
            .submit(Request::new(pb::SubmitRequest { submission: None }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }
}
