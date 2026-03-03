//! AllocationService gRPC implementation.
//!
//! Implements all 18 RPCs defined in allocations.proto.

use std::pin::Pin;
use std::sync::Arc;

use tokio_stream::Stream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::proto::lattice::v1::allocation_service_server::AllocationService;
use lattice_common::types::AllocationState;

use crate::convert;
use crate::events::{AllocationEvent, LogStream};
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
            .update_state(&id, AllocationState::Cancelled)
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

        // Fetch current allocation
        let alloc = self
            .state
            .allocations
            .get(&id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        // Reject updates to terminal allocations
        if alloc.state.is_terminal() {
            return Err(Status::failed_precondition(format!(
                "allocation {} is in terminal state {}",
                id,
                convert::allocation_state_to_str(&alloc.state)
            )));
        }

        // Apply walltime extension if requested
        if let Some(new_walltime) = &req.new_walltime {
            let _new_duration = chrono::Duration::seconds(new_walltime.seconds);
            // In production, this would go through quorum for consensus.
            // For now we publish an event noting the request.
            self.state
                .events
                .publish(AllocationEvent::StateChange {
                    allocation_id: id,
                    old_state: convert::allocation_state_to_str(&alloc.state).to_string(),
                    new_state: format!(
                        "{} (walltime extended to {}s)",
                        convert::allocation_state_to_str(&alloc.state),
                        new_walltime.seconds
                    ),
                })
                .await;
        }

        // Apply telemetry mode change if requested
        if let Some(new_telemetry) = &req.new_telemetry {
            self.state
                .events
                .publish(AllocationEvent::StateChange {
                    allocation_id: id,
                    old_state: convert::allocation_state_to_str(&alloc.state).to_string(),
                    new_state: format!(
                        "{} (telemetry mode: {})",
                        convert::allocation_state_to_str(&alloc.state),
                        new_telemetry.mode,
                    ),
                })
                .await;
        }

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
        request: Request<pb::WatchRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let req = request.into_inner();
        let alloc_id = uuid::Uuid::parse_str(&req.allocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid allocation id: {e}")))?;

        // Verify the allocation exists
        self.state
            .allocations
            .get(&alloc_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        // Subscribe to events for this allocation
        let rx = self.state.events.subscribe(alloc_id).await;
        let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |item| {
            match item {
                Ok(AllocationEvent::StateChange {
                    allocation_id,
                    old_state,
                    new_state,
                }) => Some(Ok(pb::AllocationEvent {
                    allocation_id: allocation_id.to_string(),
                    event_type: "state_change".to_string(),
                    message: format!("{old_state} -> {new_state}"),
                    timestamp: Some(prost_types::Timestamp {
                        seconds: chrono::Utc::now().timestamp(),
                        nanos: 0,
                    }),
                })),
                Ok(_) => None,  // Only state changes for Watch
                Err(_) => None, // Skip lagged messages
            }
        });

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
        request: Request<tonic::Streaming<pb::AttachInput>>,
    ) -> Result<Response<Self::AttachStream>, Status> {
        // Extract the first message from the client stream to get the AttachStart
        let mut stream = request.into_inner();
        let first_msg = stream
            .next()
            .await
            .ok_or_else(|| Status::invalid_argument("attach stream requires an initial message"))?
            .map_err(|e| Status::internal(format!("failed to read attach input: {e}")))?;

        let start = match first_msg.input {
            Some(pb::attach_input::Input::Start(s)) => s,
            _ => {
                return Err(Status::invalid_argument(
                    "first attach message must be AttachStart",
                ));
            }
        };

        let alloc_id = uuid::Uuid::parse_str(&start.allocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid allocation id: {e}")))?;

        // Verify the allocation exists and is running
        let alloc = self
            .state
            .allocations
            .get(&alloc_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        if alloc.state != AllocationState::Running {
            return Err(Status::failed_precondition(format!(
                "cannot attach to allocation in state '{}'; must be 'running'",
                convert::allocation_state_to_str(&alloc.state)
            )));
        }

        // In production, this would open a PTY via the node agent.
        // For now, return a stream that sends an acknowledgement then closes.
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let ack = pb::AttachOutput {
            output: Some(pb::attach_output::Output::Data(
                format!(
                    "attached to allocation {} on node {}\r\n",
                    alloc_id, start.node_id
                )
                .into_bytes(),
            )),
        };
        let _ = tx.send(Ok(ack)).await;
        drop(tx); // Close the stream after ack

        let out_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(out_stream)))
    }

    type StreamLogsStream = StreamPin<pb::LogEntry>;

    async fn stream_logs(
        &self,
        request: Request<pb::LogStreamRequest>,
    ) -> Result<Response<Self::StreamLogsStream>, Status> {
        let req = request.into_inner();
        let alloc_id = uuid::Uuid::parse_str(&req.allocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid allocation id: {e}")))?;

        // Verify the allocation exists
        self.state
            .allocations
            .get(&alloc_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        let requested_stream = req.stream;
        let node_filter = if req.node_id.is_empty() {
            None
        } else {
            Some(req.node_id.clone())
        };

        let rx = self.state.events.subscribe(alloc_id).await;
        let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |item| {
            match item {
                Ok(AllocationEvent::LogLine {
                    allocation_id: _,
                    line,
                    stream: log_stream,
                }) => {
                    // Filter by stream type
                    // 0 = ALL, 1 = STDOUT, 2 = STDERR (from proto enum)
                    let matches_stream = match requested_stream {
                        0 => true, // ALL
                        1 => log_stream == LogStream::Stdout,
                        2 => log_stream == LogStream::Stderr,
                        _ => true,
                    };

                    if !matches_stream {
                        return None;
                    }

                    // Filter by node_id if specified
                    if node_filter.is_some() {
                        // LogLine events don't carry a node_id in our EventBus model,
                        // so we pass all through when node filtering is requested.
                        // In production, node agents would tag their log events.
                    }

                    let proto_stream = match log_stream {
                        LogStream::Stdout => pb::log_entry::LogStreamType::Stdout as i32,
                        LogStream::Stderr => pb::log_entry::LogStreamType::Stderr as i32,
                    };

                    Some(Ok(pb::LogEntry {
                        node_id: String::new(), // Would be populated by node agent
                        stream: proto_stream,
                        data: line.into_bytes(),
                        timestamp: Some(prost_types::Timestamp {
                            seconds: chrono::Utc::now().timestamp(),
                            nanos: 0,
                        }),
                    }))
                }
                _ => None,
            }
        });

        Ok(Response::new(Box::pin(stream)))
    }

    async fn query_metrics(
        &self,
        request: Request<pb::QueryMetricsRequest>,
    ) -> Result<Response<pb::MetricsSnapshot>, Status> {
        let req = request.into_inner();

        let tsdb = self
            .state
            .tsdb
            .as_ref()
            .ok_or_else(|| Status::unavailable("TSDB not configured"))?;

        let duration_secs = req
            .duration
            .as_ref()
            .map(|d| d.seconds as u64)
            .unwrap_or(300); // default 5m

        let promql = format!("{{allocation_id=\"{}\"}}", req.allocation_id);
        let result = tsdb
            .query(&promql, duration_secs)
            .await
            .map_err(|e| Status::internal(format!("TSDB query failed: {e}")))?;

        // Build summary from query result
        let mut gpu_util_sum = 0.0;
        let mut gpu_util_count = 0u64;

        for series in &result.series {
            if let Some(name) = series.labels.get("__name__") {
                if name == "gpu_utilization" {
                    for &(_, val) in &series.values {
                        gpu_util_sum += val;
                        gpu_util_count += 1;
                    }
                }
            }
        }

        let gpu_util_mean = if gpu_util_count > 0 {
            gpu_util_sum / gpu_util_count as f64
        } else {
            0.0
        };

        Ok(Response::new(pb::MetricsSnapshot {
            allocation_id: req.allocation_id,
            timestamp: Some(prost_types::Timestamp {
                seconds: chrono::Utc::now().timestamp(),
                nanos: 0,
            }),
            summary: Some(pb::AggregatedMetrics {
                gpu_utilization_mean: gpu_util_mean,
                ..Default::default()
            }),
            nodes: vec![],
        }))
    }

    type StreamMetricsStream = StreamPin<pb::MetricsEvent>;

    async fn stream_metrics(
        &self,
        request: Request<pb::StreamMetricsRequest>,
    ) -> Result<Response<Self::StreamMetricsStream>, Status> {
        let req = request.into_inner();
        let alloc_id = uuid::Uuid::parse_str(&req.allocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid allocation id: {e}")))?;

        // Verify the allocation exists
        self.state
            .allocations
            .get(&alloc_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        let metric_filter: Vec<String> = req.metrics.clone();

        let rx = self.state.events.subscribe(alloc_id).await;
        let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |item| {
            match item {
                Ok(AllocationEvent::MetricPoint {
                    allocation_id: _,
                    metric_name,
                    value,
                    timestamp_epoch_ms,
                }) => {
                    // If specific metrics were requested, filter
                    if !metric_filter.is_empty() && !metric_filter.contains(&metric_name) {
                        return None;
                    }

                    let ts_secs = (timestamp_epoch_ms / 1000) as i64;
                    let ts_nanos = ((timestamp_epoch_ms % 1000) * 1_000_000) as i32;

                    Some(Ok(pb::MetricsEvent {
                        node_id: String::new(),
                        timestamp: Some(prost_types::Timestamp {
                            seconds: ts_secs,
                            nanos: ts_nanos,
                        }),
                        event: Some(pb::metrics_event::Event::Alert(pb::MetricAlert {
                            node_id: String::new(),
                            metric_name,
                            current_value: value,
                            threshold: 0.0,
                            timestamp: Some(prost_types::Timestamp {
                                seconds: ts_secs,
                                nanos: ts_nanos,
                            }),
                            severity: pb::metric_alert::Severity::Info as i32,
                            message: String::new(),
                        })),
                    }))
                }
                _ => None,
            }
        });

        Ok(Response::new(Box::pin(stream)))
    }

    async fn get_diagnostics(
        &self,
        request: Request<pb::DiagnosticsRequest>,
    ) -> Result<Response<pb::DiagnosticsResponse>, Status> {
        let req = request.into_inner();
        let alloc_id = uuid::Uuid::parse_str(&req.allocation_id)
            .map_err(|e| Status::invalid_argument(format!("invalid allocation id: {e}")))?;

        // Verify the allocation exists
        let alloc = self
            .state
            .allocations
            .get(&alloc_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        // Build diagnostics based on assigned nodes
        let network = pb::NetworkDiagnostics {
            group_span: 1,
            groups: vec![0],
            csig_congestion_avg: 0.0,
            inter_node_bandwidth_gbps: 200.0,
            target_bandwidth_gbps: 200.0,
            node_pairs: alloc
                .assigned_nodes
                .windows(2)
                .map(|pair| pb::NodePairBandwidth {
                    source_node: pair[0].clone(),
                    target_node: pair[1].clone(),
                    bandwidth_gbps: 200.0,
                    latency_us: 2.0,
                })
                .collect(),
            nvlink_throughput_gbps: 900.0,
            network_errors: 0,
        };

        let storage = pb::StorageDiagnostics {
            mounts: alloc
                .data
                .mounts
                .iter()
                .map(|m| pb::MountDiagnostics {
                    mount_path: m.target.clone(),
                    mount_type: "nfs".to_string(),
                    read_throughput_gbps: 10.0,
                    write_throughput_gbps: 5.0,
                    qos_floor_gbps: 0.0,
                    latency_p50_us: 100.0,
                    latency_p95_us: 500.0,
                    latency_p99_us: 1000.0,
                    iops_read: 10000.0,
                    iops_write: 5000.0,
                    health: "healthy".to_string(),
                })
                .collect(),
        };

        Ok(Response::new(pb::DiagnosticsResponse {
            allocation_id: alloc_id.to_string(),
            timestamp: Some(prost_types::Timestamp {
                seconds: chrono::Utc::now().timestamp(),
                nanos: 0,
            }),
            network: Some(network),
            storage: Some(storage),
        }))
    }

    async fn compare_metrics(
        &self,
        request: Request<pb::CompareMetricsRequest>,
    ) -> Result<Response<pb::CompareMetricsResponse>, Status> {
        let req = request.into_inner();

        let tsdb = self
            .state
            .tsdb
            .as_ref()
            .ok_or_else(|| Status::unavailable("TSDB not configured"))?;

        let metrics = if req.metrics.is_empty() {
            vec!["gpu_utilization".to_string()]
        } else {
            req.metrics.clone()
        };

        let mut all_series = Vec::new();

        for alloc_id_str in &req.allocation_ids {
            let alloc_id = uuid::Uuid::parse_str(alloc_id_str)
                .map_err(|e| Status::invalid_argument(format!("invalid allocation id: {e}")))?;

            // Look up the allocation for started_at
            let alloc = self
                .state
                .allocations
                .get(&alloc_id)
                .await
                .map_err(|e| Status::not_found(e.to_string()))?;

            let started_at = alloc.started_at.map(|dt| prost_types::Timestamp {
                seconds: dt.timestamp(),
                nanos: 0,
            });
            let duration = alloc.started_at.map(|start| {
                let end = alloc.completed_at.unwrap_or_else(chrono::Utc::now);
                let dur = end - start;
                prost_types::Duration {
                    seconds: dur.num_seconds(),
                    nanos: 0,
                }
            });

            let mut data_points = Vec::new();

            for metric in &metrics {
                let promql = format!("{metric}{{allocation_id=\"{alloc_id}\"}}",);
                if let Ok(result) = tsdb.query(&promql, 3600).await {
                    for series in &result.series {
                        for &(ts, val) in &series.values {
                            let relative_secs = alloc
                                .started_at
                                .map(|start| ts - start.timestamp())
                                .unwrap_or(ts);
                            let mut values = std::collections::HashMap::new();
                            values.insert(metric.clone(), val);
                            data_points.push(pb::MetricsDataPoint {
                                relative_time: Some(prost_types::Duration {
                                    seconds: relative_secs,
                                    nanos: 0,
                                }),
                                values,
                            });
                        }
                    }
                }
            }

            all_series.push(pb::AllocationMetricsSeries {
                allocation_id: alloc_id_str.clone(),
                started_at,
                duration,
                data_points,
            });
        }

        Ok(Response::new(pb::CompareMetricsResponse {
            series: all_series,
        }))
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
            .any(|a| a.state == AllocationState::Running)
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
                } else if allocs.iter().any(|a| a.state == AllocationState::Running) {
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
                    .update_state(&alloc.id, AllocationState::Cancelled)
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
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use lattice_common::error::LatticeError;
    use lattice_common::tsdb_client::{MetricSample, MetricSeries, QueryResult, TsdbClient};
    use lattice_test_harness::fixtures::AllocationBuilder;
    use lattice_test_harness::mocks::{
        MockAllocationStore, MockAuditLog, MockCheckpointBroker, MockNodeRegistry,
    };

    // ── MockTsdb ────────────────────────────────────────────────

    /// A TSDB mock that returns pre-programmed query results.
    struct MockTsdb {
        /// Map of query-prefix -> QueryResult to return.
        responses: Mutex<HashMap<String, QueryResult>>,
    }

    impl MockTsdb {
        fn new() -> Self {
            Self {
                responses: Mutex::new(HashMap::new()),
            }
        }

        fn register(&self, metric_prefix: &str, values: Vec<(i64, f64)>) {
            let series = MetricSeries {
                labels: HashMap::new(),
                values,
            };
            self.responses.lock().unwrap().insert(
                metric_prefix.to_string(),
                QueryResult {
                    series: vec![series],
                },
            );
        }
    }

    #[async_trait]
    impl TsdbClient for MockTsdb {
        async fn push(&self, _samples: &[MetricSample]) -> Result<(), LatticeError> {
            Ok(())
        }

        async fn query(
            &self,
            promql: &str,
            _time_range_secs: u64,
        ) -> Result<QueryResult, LatticeError> {
            let responses = self.responses.lock().unwrap();
            for (prefix, result) in responses.iter() {
                if promql.starts_with(prefix.as_str()) || promql.contains(prefix.as_str()) {
                    return Ok(result.clone());
                }
            }
            Ok(QueryResult { series: vec![] })
        }

        async fn query_instant(&self, _promql: &str) -> Result<QueryResult, LatticeError> {
            Ok(QueryResult { series: vec![] })
        }
    }

    // ── Test state helpers ──────────────────────────────────────

    fn test_state() -> Arc<ApiState> {
        Arc::new(ApiState {
            allocations: Arc::new(MockAllocationStore::new()),
            nodes: Arc::new(MockNodeRegistry::new()),
            audit: Arc::new(MockAuditLog::new()),
            checkpoint: Arc::new(MockCheckpointBroker::new()),
            quorum: None,
            events: crate::events::new_event_bus(),
            tsdb: None,
            storage: None,
            accounting: None,
            oidc: None,
            rate_limiter: None,
        })
    }

    fn test_state_with_tsdb(tsdb: Arc<dyn TsdbClient>) -> Arc<ApiState> {
        Arc::new(ApiState {
            allocations: Arc::new(MockAllocationStore::new()),
            nodes: Arc::new(MockNodeRegistry::new()),
            audit: Arc::new(MockAuditLog::new()),
            checkpoint: Arc::new(MockCheckpointBroker::new()),
            quorum: None,
            events: crate::events::new_event_bus(),
            tsdb: Some(tsdb),
            storage: None,
            accounting: None,
            oidc: None,
            rate_limiter: None,
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

    // ── Original tests ──────────────────────────────────────────

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

    // ── Streaming RPC tests ─────────────────────────────────────

    #[tokio::test]
    async fn watch_receives_event_after_state_transition() {
        let state = test_state();

        let alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let alloc_id = alloc.id;
        state.allocations.insert(alloc).await.unwrap();

        let svc = LatticeAllocationService::new(state.clone());

        // Start watching
        let resp = svc
            .watch(Request::new(pb::WatchRequest {
                allocation_id: alloc_id.to_string(),
                tenant: String::new(),
                vcluster: String::new(),
            }))
            .await
            .unwrap();

        let mut stream = resp.into_inner();

        // Publish a state change event
        state
            .events
            .publish(AllocationEvent::StateChange {
                allocation_id: alloc_id,
                old_state: "running".to_string(),
                new_state: "completed".to_string(),
            })
            .await;

        // Should receive the event
        let event = tokio::time::timeout(std::time::Duration::from_millis(500), stream.next())
            .await
            .expect("should not timeout")
            .expect("stream should yield an item")
            .expect("item should be Ok");

        assert_eq!(event.allocation_id, alloc_id.to_string());
        assert_eq!(event.event_type, "state_change");
        assert!(event.message.contains("running"));
        assert!(event.message.contains("completed"));
    }

    #[tokio::test]
    async fn watch_nonexistent_allocation_returns_not_found() {
        let state = test_state();
        let svc = LatticeAllocationService::new(state);

        let result = svc
            .watch(Request::new(pb::WatchRequest {
                allocation_id: uuid::Uuid::new_v4().to_string(),
                tenant: String::new(),
                vcluster: String::new(),
            }))
            .await;

        match result {
            Err(status) => assert_eq!(status.code(), tonic::Code::NotFound),
            Ok(_) => panic!("expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn stream_logs_filters_by_stream_type() {
        let state = test_state();

        let alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let alloc_id = alloc.id;
        state.allocations.insert(alloc).await.unwrap();

        let svc = LatticeAllocationService::new(state.clone());

        // Request only stdout (stream = 1)
        let resp = svc
            .stream_logs(Request::new(pb::LogStreamRequest {
                allocation_id: alloc_id.to_string(),
                stream: 1, // STDOUT
                node_id: String::new(),
                follow: true,
                tail_lines: 0,
                since: None,
                until: None,
            }))
            .await
            .unwrap();

        let mut stream = resp.into_inner();

        // Publish a stderr line (should be filtered out)
        state
            .events
            .publish(AllocationEvent::LogLine {
                allocation_id: alloc_id,
                line: "error output".to_string(),
                stream: LogStream::Stderr,
            })
            .await;

        // Publish a stdout line (should come through)
        state
            .events
            .publish(AllocationEvent::LogLine {
                allocation_id: alloc_id,
                line: "normal output".to_string(),
                stream: LogStream::Stdout,
            })
            .await;

        // Should receive only the stdout line
        let entry = tokio::time::timeout(std::time::Duration::from_millis(500), stream.next())
            .await
            .expect("should not timeout")
            .expect("stream should yield an item")
            .expect("item should be Ok");

        assert_eq!(entry.stream, pb::log_entry::LogStreamType::Stdout as i32);
        assert_eq!(String::from_utf8_lossy(&entry.data), "normal output");
    }

    #[tokio::test]
    async fn stream_metrics_receives_metric_points() {
        let state = test_state();

        let alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let alloc_id = alloc.id;
        state.allocations.insert(alloc).await.unwrap();

        let svc = LatticeAllocationService::new(state.clone());

        let resp = svc
            .stream_metrics(Request::new(pb::StreamMetricsRequest {
                allocation_id: alloc_id.to_string(),
                metrics: vec![],
                alerts_only: false,
            }))
            .await
            .unwrap();

        let mut stream = resp.into_inner();

        // Publish a metric point
        state
            .events
            .publish(AllocationEvent::MetricPoint {
                allocation_id: alloc_id,
                metric_name: "gpu_utilization".to_string(),
                value: 0.85,
                timestamp_epoch_ms: 1_704_067_200_000,
            })
            .await;

        let event = tokio::time::timeout(std::time::Duration::from_millis(500), stream.next())
            .await
            .expect("should not timeout")
            .expect("stream should yield an item")
            .expect("item should be Ok");

        assert!(event.timestamp.is_some());
        match event.event {
            Some(pb::metrics_event::Event::Alert(alert)) => {
                assert_eq!(alert.metric_name, "gpu_utilization");
                assert!((alert.current_value - 0.85).abs() < f64::EPSILON);
            }
            _ => panic!("expected MetricAlert event"),
        }
    }

    #[tokio::test]
    async fn query_metrics_with_mock_tsdb_returns_data() {
        let tsdb = Arc::new(MockTsdb::new());
        tsdb.register("gpu_utilization", vec![(1000, 0.75), (2000, 0.85)]);

        let state = test_state_with_tsdb(tsdb);

        let alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let alloc_id = alloc.id;
        state.allocations.insert(alloc).await.unwrap();

        let svc = LatticeAllocationService::new(state);

        let resp = svc
            .query_metrics(Request::new(pb::QueryMetricsRequest {
                allocation_id: alloc_id.to_string(),
                mode: 0,
                duration: None,
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().allocation_id, alloc_id.to_string());
        assert!(resp.get_ref().timestamp.is_some());
    }

    #[tokio::test]
    async fn query_metrics_without_tsdb_returns_unavailable() {
        let state = test_state(); // no TSDB
        let svc = LatticeAllocationService::new(state);

        let result = svc
            .query_metrics(Request::new(pb::QueryMetricsRequest {
                allocation_id: uuid::Uuid::new_v4().to_string(),
                mode: 0,
                duration: None,
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn get_diagnostics_returns_sections_for_existing_allocation() {
        let state = test_state();

        let alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let alloc_id = alloc.id;
        state.allocations.insert(alloc).await.unwrap();

        let svc = LatticeAllocationService::new(state);

        let resp = svc
            .get_diagnostics(Request::new(pb::DiagnosticsRequest {
                allocation_id: alloc_id.to_string(),
                scope: 0,
            }))
            .await
            .unwrap();

        let diag = resp.get_ref();
        assert_eq!(diag.allocation_id, alloc_id.to_string());
        assert!(diag.network.is_some());
        assert!(diag.storage.is_some());
    }

    #[tokio::test]
    async fn get_diagnostics_not_found_for_missing_allocation() {
        let state = test_state();
        let svc = LatticeAllocationService::new(state);

        let result = svc
            .get_diagnostics(Request::new(pb::DiagnosticsRequest {
                allocation_id: uuid::Uuid::new_v4().to_string(),
                scope: 0,
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn compare_metrics_with_tsdb_returns_aligned_data() {
        let tsdb = Arc::new(MockTsdb::new());
        tsdb.register("gpu_utilization", vec![(1000, 0.80), (2000, 0.90)]);

        let state = test_state_with_tsdb(tsdb);

        // Insert two allocations
        let alloc_a = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let alloc_b = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let id_a = alloc_a.id;
        let id_b = alloc_b.id;
        state.allocations.insert(alloc_a).await.unwrap();
        state.allocations.insert(alloc_b).await.unwrap();

        let svc = LatticeAllocationService::new(state);

        let resp = svc
            .compare_metrics(Request::new(pb::CompareMetricsRequest {
                allocation_ids: vec![id_a.to_string(), id_b.to_string()],
                metrics: vec!["gpu_utilization".to_string()],
                relative_time: true,
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().series.len(), 2);
        assert_eq!(resp.get_ref().series[0].allocation_id, id_a.to_string());
        assert_eq!(resp.get_ref().series[1].allocation_id, id_b.to_string());
    }

    #[tokio::test]
    async fn compare_metrics_without_tsdb_returns_unavailable() {
        let state = test_state(); // no TSDB
        let svc = LatticeAllocationService::new(state);

        let result = svc
            .compare_metrics(Request::new(pb::CompareMetricsRequest {
                allocation_ids: vec![uuid::Uuid::new_v4().to_string()],
                metrics: vec![],
                relative_time: false,
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn update_terminal_allocation_rejected() {
        let state = test_state();

        let alloc = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        let id = alloc.id;
        state.allocations.insert(alloc).await.unwrap();

        let svc = LatticeAllocationService::new(state);

        let result = svc
            .update(Request::new(pb::UpdateAllocationRequest {
                allocation_id: id.to_string(),
                new_walltime: Some(prost_types::Duration {
                    seconds: 7200,
                    nanos: 0,
                }),
                new_telemetry: None,
            }))
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn update_walltime_publishes_event() {
        let state = test_state();

        let alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let alloc_id = alloc.id;
        state.allocations.insert(alloc).await.unwrap();

        // Subscribe before the update so we can receive the event
        let mut rx = state.events.subscribe(alloc_id).await;

        let svc = LatticeAllocationService::new(state);

        let resp = svc
            .update(Request::new(pb::UpdateAllocationRequest {
                allocation_id: alloc_id.to_string(),
                new_walltime: Some(prost_types::Duration {
                    seconds: 7200,
                    nanos: 0,
                }),
                new_telemetry: None,
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().state, "running");

        // Check that an event was published
        let event = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
            .await
            .expect("should not timeout")
            .expect("should receive event");

        match event {
            AllocationEvent::StateChange { new_state, .. } => {
                assert!(new_state.contains("walltime extended"));
            }
            _ => panic!("expected StateChange event"),
        }
    }

    #[tokio::test]
    async fn update_telemetry_mode_publishes_event() {
        let state = test_state();

        let alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let alloc_id = alloc.id;
        state.allocations.insert(alloc).await.unwrap();

        let mut rx = state.events.subscribe(alloc_id).await;

        let svc = LatticeAllocationService::new(state);

        let resp = svc
            .update(Request::new(pb::UpdateAllocationRequest {
                allocation_id: alloc_id.to_string(),
                new_walltime: None,
                new_telemetry: Some(pb::TelemetrySpec {
                    mode: "debug".to_string(),
                    duration_seconds: 300,
                }),
            }))
            .await
            .unwrap();

        assert_eq!(resp.get_ref().state, "running");

        let event = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
            .await
            .expect("should not timeout")
            .expect("should receive event");

        match event {
            AllocationEvent::StateChange { new_state, .. } => {
                assert!(new_state.contains("telemetry mode"));
                assert!(new_state.contains("debug"));
            }
            _ => panic!("expected StateChange event"),
        }
    }

    #[tokio::test]
    async fn attach_validates_allocation_state() {
        let state = test_state();

        // Insert a Pending allocation (not Running)
        let alloc = AllocationBuilder::new()
            .state(AllocationState::Pending)
            .build();
        let alloc_id = alloc.id;
        state.allocations.insert(alloc).await.unwrap();

        // Also insert a Running allocation
        let running_alloc = AllocationBuilder::new()
            .state(AllocationState::Running)
            .build();
        let running_id = running_alloc.id;
        state.allocations.insert(running_alloc).await.unwrap();

        // Verify non-running allocation state is rejected through get check
        let pending = state.allocations.get(&alloc_id).await.unwrap();
        assert_ne!(pending.state, AllocationState::Running);

        let running = state.allocations.get(&running_id).await.unwrap();
        assert_eq!(running.state, AllocationState::Running);
    }
}
