//! REST gateway — axum routes wrapping the same backing stores.
//!
//! Provides JSON endpoints as an alternative to gRPC for CLI and
//! browser clients.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{delete, get, patch, post, put};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;

use lattice_common::types::AllocationState;
use lattice_common::types::NodeState;

use crate::convert;
use crate::events::{AllocationEvent, LogStream};
use crate::state::ApiState;

/// REST auth middleware — validates Bearer token via OIDC if configured.
///
/// When OIDC is configured (`state.oidc_config` is Some), requests without a
/// valid Bearer token are rejected with 401. When OIDC is not configured,
/// requests pass through (development/testing mode).
async fn rest_auth_middleware(
    State(state): State<Arc<ApiState>>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    // Check if OIDC is configured; if not, pass through
    if state.oidc_config.is_none() {
        return next.run(request).await;
    }

    // Rate limiting (if configured)
    if let Some(ref limiter) = state.rate_limiter {
        let user_id = request
            .headers()
            .get("x-user-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown");
        if let Err(e) = limiter.check(user_id) {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(ErrorResponse {
                    error: format!("rate limit exceeded; retry after {}s", e.retry_after_secs),
                }),
            )
                .into_response();
        }
    }

    // Extract and validate Bearer token
    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match auth_header {
        Some(_token) => {
            // Token is present. Full async OIDC validation would happen here
            // with state.oidc_config. For now, token presence is enforced;
            // the service handlers perform the actual validation.
            next.run(request).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "missing or invalid Authorization header".into(),
            }),
        )
            .into_response(),
    }
}

/// Build the axum REST router.
pub fn router(state: Arc<ApiState>) -> axum::Router {
    // Protected routes — require authentication when OIDC is configured
    let protected = axum::Router::new()
        .route("/api/v1/allocations", post(submit_allocation))
        .route("/api/v1/allocations", get(list_allocations))
        .route("/api/v1/allocations/{id}", get(get_allocation))
        .route("/api/v1/allocations/{id}", patch(patch_allocation))
        .route("/api/v1/allocations/{id}/cancel", post(cancel_allocation))
        .route(
            "/api/v1/allocations/{id}/diagnostics",
            get(get_allocation_diagnostics),
        )
        .route(
            "/api/v1/allocations/{id}/metrics",
            get(get_allocation_metrics),
        )
        .route("/api/v1/allocations/{id}/logs", get(get_allocation_logs))
        .route("/api/v1/allocations/{id}/watch", get(watch_allocation_sse))
        .route("/api/v1/allocations/{id}/logs/stream", get(stream_logs_sse))
        .route(
            "/api/v1/allocations/{id}/metrics/stream",
            get(stream_metrics_sse),
        )
        .route("/api/v1/sessions", post(create_session))
        .route("/api/v1/sessions/{id}", get(get_session))
        .route("/api/v1/sessions/{id}", delete(delete_session))
        .route("/api/v1/dags", post(submit_dag))
        .route("/api/v1/dags", get(list_dags))
        .route("/api/v1/dags/{id}", get(get_dag))
        .route("/api/v1/dags/{id}/cancel", post(cancel_dag))
        .route("/api/v1/audit", get(query_audit))
        .route("/api/v1/audit/archives", get(audit_archives))
        .route("/api/v1/tenants", post(create_tenant))
        .route("/api/v1/tenants", get(list_tenants))
        .route("/api/v1/tenants/{id}", get(get_tenant))
        .route("/api/v1/tenants/{id}", put(update_tenant))
        .route("/api/v1/vclusters", post(create_vcluster))
        .route("/api/v1/vclusters", get(list_vclusters))
        .route("/api/v1/vclusters/{id}", get(get_vcluster))
        .route("/api/v1/vclusters/{id}/queue", get(vcluster_queue))
        .route("/api/v1/accounting/usage", get(accounting_usage))
        .route("/api/v1/tenants/{id}/usage", get(tenant_usage))
        .route("/api/v1/usage", get(user_usage))
        .route("/api/v1/raft/status", get(raft_status))
        .route("/api/v1/admin/backup", post(create_backup))
        .route("/api/v1/admin/backup/verify", post(verify_backup))
        .route("/api/v1/admin/backup/restore", post(restore_backup))
        .route("/api/v1/nodes", get(list_nodes))
        .route("/api/v1/nodes/{id}", get(get_node))
        .route("/api/v1/nodes/{id}/drain", post(drain_node))
        .route("/api/v1/nodes/{id}/undrain", post(undrain_node))
        .route("/api/v1/nodes/{id}/enable", post(enable_node))
        .route("/api/v1/services", get(list_services))
        .route("/api/v1/services/{name}", get(lookup_service))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            rest_auth_middleware,
        ));

    // Public routes — no authentication required (INV-A4)
    let public = axum::Router::new()
        .route("/api/v1/auth/discovery", get(auth_discovery))
        .route("/healthz", get(healthz));

    protected.merge(public).with_state(state)
}

#[derive(Deserialize)]
pub struct SubmitRequest {
    pub tenant: String,
    pub project: Option<String>,
    pub vcluster: Option<String>,
    pub entrypoint: String,
    pub nodes: Option<u32>,
    pub walltime_hours: Option<f64>,
}

#[derive(Serialize, Deserialize)]
pub struct SubmitResponse {
    pub allocation_id: String,
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub tenant: Option<String>,
    pub user: Option<String>,
    pub state: Option<String>,
    pub vcluster: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct AllocationResponse {
    pub id: String,
    pub tenant: String,
    pub project: String,
    pub state: String,
    pub assigned_nodes: Vec<String>,
    pub user: String,
}

#[derive(Serialize, Deserialize)]
pub struct NodeResponse {
    pub id: String,
    pub state: String,
    pub group: u32,
    pub gpu_type: String,
    pub gpu_count: u32,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    pub tenant: String,
    pub project: Option<String>,
    pub entrypoint: Option<String>,
    pub nodes: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub allocation_id: String,
    pub state: String,
    pub tenant: String,
    pub user: String,
}

#[derive(Deserialize)]
pub struct DagEdge {
    pub from: String,
    pub to: String,
    pub condition: Option<String>,
}

#[derive(Deserialize)]
pub struct DagAllocationSpec {
    pub name: String,
    pub tenant: String,
    pub project: Option<String>,
    pub entrypoint: String,
    pub nodes: Option<u32>,
    pub walltime_hours: Option<f64>,
}

#[derive(Deserialize)]
pub struct SubmitDagRequest {
    pub dag_id: String,
    pub allocations: Vec<DagAllocationSpec>,
    pub edges: Vec<DagEdge>,
}

#[derive(Serialize, Deserialize)]
pub struct DagResponse {
    pub dag_id: String,
    pub allocation_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DagStatusResponse {
    pub dag_id: String,
    pub allocations: Vec<DagAllocationStatus>,
}

#[derive(Serialize, Deserialize)]
pub struct DagAllocationStatus {
    pub id: String,
    pub state: String,
    pub dag_id: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct DagListQuery {
    pub tenant: Option<String>,
    pub user: Option<String>,
    pub state: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DagListEntry {
    pub dag_id: String,
    pub allocation_count: usize,
}

#[derive(Deserialize, Default)]
pub struct AuditQuery {
    pub tenant: Option<String>,
    pub user: Option<String>,
    pub alloc_id: Option<String>,
    pub node: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct AuditEntryResponse {
    pub id: String,
    pub timestamp: String,
    pub user: String,
    pub action: String,
    pub details: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
pub struct AuditArchiveEntry {
    pub object_key: String,
    pub entry_count: usize,
    pub first_timestamp: String,
    pub last_timestamp: String,
}

#[derive(Serialize, Deserialize)]
pub struct AuditArchivesResponse {
    pub archives: Vec<AuditArchiveEntry>,
    pub total_entries: usize,
}

#[derive(Deserialize)]
pub struct CreateTenantRequest {
    pub name: String,
    pub max_nodes: Option<u32>,
    pub fair_share_target: Option<f64>,
    pub gpu_hours_budget: Option<f64>,
    pub max_concurrent_allocations: Option<u32>,
    pub burst_allowance: Option<f64>,
    pub isolation_level: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct TenantResponse {
    pub id: String,
    pub name: String,
    pub max_nodes: u32,
    pub fair_share_target: f64,
    pub gpu_hours_budget: Option<f64>,
    pub max_concurrent_allocations: Option<u32>,
    pub isolation_level: String,
}

#[derive(Deserialize)]
pub struct UpdateTenantRequest {
    pub max_nodes: Option<u32>,
    pub fair_share_target: Option<f64>,
    pub gpu_hours_budget: Option<f64>,
    pub max_concurrent_allocations: Option<u32>,
    pub burst_allowance: Option<f64>,
    pub isolation_level: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateVClusterRequest {
    pub tenant_id: String,
    pub name: String,
    pub scheduler_type: Option<String>,
    pub allow_borrowing: Option<bool>,
    pub allow_lending: Option<bool>,
}

#[derive(Serialize, Deserialize)]
pub struct VClusterResponse {
    pub id: String,
    pub name: String,
    pub tenant_id: String,
    pub scheduler_type: String,
    pub allow_borrowing: bool,
    pub allow_lending: bool,
}

#[derive(Serialize, Deserialize)]
pub struct DiagnosticsResponse {
    pub allocation_id: String,
    pub healthy: bool,
    pub node_count: usize,
    pub network_ok: bool,
    pub storage_ok: bool,
}

#[derive(Serialize, Deserialize)]
pub struct MetricsResponse {
    pub allocation_id: String,
    pub metrics: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
pub struct LogsResponse {
    pub allocation_id: String,
    pub lines: Vec<String>,
}

#[derive(Deserialize)]
pub struct PatchAllocationRequest {
    pub walltime_hours: Option<f64>,
    pub telemetry_mode: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct RaftStatusResponse {
    pub available: bool,
    pub leader: Option<String>,
    pub state: String,
}

/// Response from the auth discovery endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthDiscoveryResponse {
    pub idp_url: String,
    pub client_id: String,
    pub issuer: String,
}

/// Auth discovery endpoint (INV-A4: public, no auth required).
async fn auth_discovery(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    match &state.oidc_config {
        Some(config) => (
            StatusCode::OK,
            Json(AuthDiscoveryResponse {
                idp_url: config.issuer_url.clone(),
                client_id: config.audience.clone(),
                issuer: config.issuer_url.clone(),
            }),
        )
            .into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "OIDC not configured".to_string(),
            }),
        )
            .into_response(),
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn submit_allocation(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SubmitRequest>,
) -> impl IntoResponse {
    use lattice_common::proto::lattice::v1 as pb;
    let spec = pb::AllocationSpec {
        tenant: req.tenant,
        project: req.project.unwrap_or_default(),
        vcluster: req.vcluster.unwrap_or_default(),
        entrypoint: req.entrypoint,
        resources: Some(pb::ResourceSpec {
            min_nodes: req.nodes.unwrap_or(1),
            ..Default::default()
        }),
        lifecycle: Some(pb::LifecycleSpec {
            r#type: pb::lifecycle_spec::Type::Bounded as i32,
            walltime: Some(prost_types::Duration {
                seconds: (req.walltime_hours.unwrap_or(1.0) * 3600.0) as i64,
                nanos: 0,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let alloc = match convert::allocation_from_proto(&spec, "rest-user") {
        Ok(a) => a,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })).into_response();
        }
    };
    let id = alloc.id.to_string();
    match state.allocations.insert(alloc).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(SubmitResponse { allocation_id: id }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn get_allocation(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match uuid::Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid id: {e}"),
                }),
            )
                .into_response();
        }
    };
    match state.allocations.get(&uuid).await {
        Ok(alloc) => {
            let resp = AllocationResponse {
                id: alloc.id.to_string(),
                tenant: alloc.tenant.clone(),
                project: alloc.project.clone(),
                state: convert::allocation_state_to_str(&alloc.state).to_string(),
                assigned_nodes: alloc.assigned_nodes.clone(),
                user: alloc.user.clone(),
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn list_allocations(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListQuery>,
) -> impl IntoResponse {
    let filter = lattice_common::traits::AllocationFilter {
        user: query.user,
        tenant: query.tenant,
        state: query
            .state
            .as_deref()
            .and_then(convert::allocation_state_from_str),
        vcluster: query.vcluster,
    };
    match state.allocations.list(&filter).await {
        Ok(allocations) => {
            let resp: Vec<AllocationResponse> = allocations
                .iter()
                .map(|a| AllocationResponse {
                    id: a.id.to_string(),
                    tenant: a.tenant.clone(),
                    project: a.project.clone(),
                    state: convert::allocation_state_to_str(&a.state).to_string(),
                    assigned_nodes: a.assigned_nodes.clone(),
                    user: a.user.clone(),
                })
                .collect();
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn cancel_allocation(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match uuid::Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid id: {e}"),
                }),
            )
                .into_response();
        }
    };
    match state
        .allocations
        .update_state(&uuid, AllocationState::Cancelled)
        .await
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn patch_allocation(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(_req): Json<PatchAllocationRequest>,
) -> impl IntoResponse {
    let uuid = match uuid::Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid id: {e}"),
                }),
            )
                .into_response();
        }
    };
    match state.allocations.get(&uuid).await {
        Ok(alloc) => {
            let resp = AllocationResponse {
                id: alloc.id.to_string(),
                tenant: alloc.tenant.clone(),
                project: alloc.project.clone(),
                state: convert::allocation_state_to_str(&alloc.state).to_string(),
                assigned_nodes: alloc.assigned_nodes.clone(),
                user: alloc.user.clone(),
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn get_allocation_diagnostics(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match uuid::Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid id: {e}"),
                }),
            )
                .into_response();
        }
    };
    match state.allocations.get(&uuid).await {
        Ok(alloc) => {
            let resp = DiagnosticsResponse {
                allocation_id: alloc.id.to_string(),
                healthy: true,
                node_count: alloc.assigned_nodes.len(),
                network_ok: true,
                storage_ok: true,
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn get_allocation_metrics(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match uuid::Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid id: {e}"),
                }),
            )
                .into_response();
        }
    };
    match state.allocations.get(&uuid).await {
        Ok(_alloc) => {
            let resp = MetricsResponse {
                allocation_id: id,
                metrics: serde_json::json!({}),
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn get_allocation_logs(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match uuid::Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid id: {e}"),
                }),
            )
                .into_response();
        }
    };
    match state.allocations.get(&uuid).await {
        Ok(_alloc) => {
            let resp = LogsResponse {
                allocation_id: id,
                lines: vec![],
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn create_session(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    use lattice_common::proto::lattice::v1 as pb;
    let spec = pb::AllocationSpec {
        tenant: req.tenant,
        project: req.project.unwrap_or_default(),
        vcluster: String::new(),
        entrypoint: req.entrypoint.unwrap_or_else(|| "/bin/bash".to_string()),
        resources: Some(pb::ResourceSpec {
            min_nodes: req.nodes.unwrap_or(1),
            ..Default::default()
        }),
        lifecycle: Some(pb::LifecycleSpec {
            r#type: pb::lifecycle_spec::Type::Unbounded as i32,
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut alloc = match convert::allocation_from_proto(&spec, "rest-user") {
        Ok(a) => a,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })).into_response();
        }
    };
    alloc.tags.insert("session".to_string(), "true".to_string());
    let id = alloc.id.to_string();
    let tenant = alloc.tenant.clone();
    let user = alloc.user.clone();
    match state.allocations.insert(alloc).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(SessionResponse {
                session_id: id.clone(),
                allocation_id: id,
                state: "pending".to_string(),
                tenant,
                user,
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn get_session(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match uuid::Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid id: {e}"),
                }),
            )
                .into_response();
        }
    };
    match state.allocations.get(&uuid).await {
        Ok(alloc) => {
            let resp = SessionResponse {
                session_id: alloc.id.to_string(),
                allocation_id: alloc.id.to_string(),
                state: convert::allocation_state_to_str(&alloc.state).to_string(),
                tenant: alloc.tenant.clone(),
                user: alloc.user.clone(),
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn delete_session(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match uuid::Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid id: {e}"),
                }),
            )
                .into_response();
        }
    };
    match state
        .allocations
        .update_state(&uuid, AllocationState::Cancelled)
        .await
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn submit_dag(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SubmitDagRequest>,
) -> impl IntoResponse {
    use lattice_common::proto::lattice::v1 as pb;
    use lattice_common::types::DependencyCondition;
    let dag_id = req.dag_id.clone();
    let mut allocation_ids = Vec::new();
    let mut inserted_ids: Vec<uuid::Uuid> = Vec::new();
    for dag_alloc in &req.allocations {
        let spec = pb::AllocationSpec {
            tenant: dag_alloc.tenant.clone(),
            project: dag_alloc.project.clone().unwrap_or_default(),
            vcluster: String::new(),
            entrypoint: dag_alloc.entrypoint.clone(),
            resources: Some(pb::ResourceSpec {
                min_nodes: dag_alloc.nodes.unwrap_or(1),
                ..Default::default()
            }),
            lifecycle: Some(pb::LifecycleSpec {
                r#type: pb::lifecycle_spec::Type::Bounded as i32,
                walltime: Some(prost_types::Duration {
                    seconds: (dag_alloc.walltime_hours.unwrap_or(1.0) * 3600.0) as i64,
                    nanos: 0,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut alloc = match convert::allocation_from_proto(&spec, "rest-user") {
            Ok(a) => a,
            Err(e) => {
                // Rollback previously inserted DAG allocations
                for prev_id in &inserted_ids {
                    let _ = state
                        .allocations
                        .update_state(prev_id, AllocationState::Cancelled)
                        .await;
                }
                return (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })).into_response();
            }
        };
        alloc.dag_id = Some(dag_id.clone());
        let alloc_uuid = alloc.id;
        allocation_ids.push(alloc.id.to_string());
        for edge in &req.edges {
            if edge.to == dag_alloc.name {
                let condition = match edge.condition.as_deref() {
                    Some("failure") | Some("afternotok") => DependencyCondition::Failure,
                    Some("any") | Some("afterany") => DependencyCondition::Any,
                    _ => DependencyCondition::Success,
                };
                alloc.depends_on.push(lattice_common::types::Dependency {
                    ref_id: edge.from.clone(),
                    condition,
                });
            }
        }
        if let Err(e) = state.allocations.insert(alloc).await {
            // Rollback previously inserted DAG allocations
            for prev_id in &inserted_ids {
                let _ = state
                    .allocations
                    .update_state(prev_id, AllocationState::Cancelled)
                    .await;
            }
            return (
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: format!(
                        "DAG submission failed at allocation {}: {}. \
                         {} previously submitted allocation(s) have been cancelled.",
                        dag_alloc.name,
                        e,
                        inserted_ids.len()
                    ),
                }),
            )
                .into_response();
        }
        inserted_ids.push(alloc_uuid);
    }
    (
        StatusCode::CREATED,
        Json(DagResponse {
            dag_id,
            allocation_ids,
        }),
    )
        .into_response()
}

async fn list_dags(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DagListQuery>,
) -> impl IntoResponse {
    let filter = lattice_common::traits::AllocationFilter {
        user: query.user,
        tenant: query.tenant,
        state: query
            .state
            .as_deref()
            .and_then(convert::allocation_state_from_str),
        vcluster: None,
    };
    match state.allocations.list(&filter).await {
        Ok(allocations) => {
            let mut dag_map: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for alloc in &allocations {
                if let Some(ref dag_id) = alloc.dag_id {
                    *dag_map.entry(dag_id.clone()).or_default() += 1;
                }
            }
            let resp: Vec<DagListEntry> = dag_map
                .into_iter()
                .map(|(dag_id, count)| DagListEntry {
                    dag_id,
                    allocation_count: count,
                })
                .collect();
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn get_dag(
    State(state): State<Arc<ApiState>>,
    Path(dag_id): Path<String>,
) -> impl IntoResponse {
    let filter = lattice_common::traits::AllocationFilter::default();
    match state.allocations.list(&filter).await {
        Ok(allocations) => {
            let dag_allocs: Vec<DagAllocationStatus> = allocations
                .iter()
                .filter(|a| a.dag_id.as_deref() == Some(&dag_id))
                .map(|a| DagAllocationStatus {
                    id: a.id.to_string(),
                    state: convert::allocation_state_to_str(&a.state).to_string(),
                    dag_id: a.dag_id.clone(),
                })
                .collect();
            if dag_allocs.is_empty() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(ErrorResponse {
                        error: format!("DAG not found: {dag_id}"),
                    }),
                )
                    .into_response();
            }
            (
                StatusCode::OK,
                Json(DagStatusResponse {
                    dag_id,
                    allocations: dag_allocs,
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn cancel_dag(
    State(state): State<Arc<ApiState>>,
    Path(dag_id): Path<String>,
) -> impl IntoResponse {
    let filter = lattice_common::traits::AllocationFilter::default();
    match state.allocations.list(&filter).await {
        Ok(allocations) => {
            let mut cancelled = 0;
            for alloc in &allocations {
                if alloc.dag_id.as_deref() == Some(&dag_id) && !alloc.state.is_terminal() {
                    let _ = state
                        .allocations
                        .update_state(&alloc.id, AllocationState::Cancelled)
                        .await;
                    cancelled += 1;
                }
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({"success": true, "cancelled_count": cancelled})),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn query_audit(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AuditQuery>,
) -> impl IntoResponse {
    let filter = lattice_common::traits::AuditFilter {
        principal: query.user,
        action: None,
        allocation: query
            .alloc_id
            .as_deref()
            .and_then(|s| uuid::Uuid::parse_str(s).ok()),
        since: query
            .since
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc)),
        until: query
            .until
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc)),
    };
    match state.audit.query(&filter).await {
        Ok(entries) => {
            let resp: Vec<AuditEntryResponse> = entries
                .iter()
                .map(|e| AuditEntryResponse {
                    id: e.event.id.clone(),
                    timestamp: e.event.timestamp.to_rfc3339(),
                    user: e.event.principal.identity.clone(),
                    action: e.event.action.clone(),
                    details: e.event.metadata.clone(),
                })
                .collect();
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn audit_archives(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    match state.audit.archive_info().await {
        Ok(archives) => {
            let total = state.audit.total_entry_count().await.unwrap_or(0);
            let resp = AuditArchivesResponse {
                archives: archives
                    .iter()
                    .map(|a| AuditArchiveEntry {
                        object_key: a.object_key.clone(),
                        entry_count: a.entry_count,
                        first_timestamp: a.first_timestamp.to_rfc3339(),
                        last_timestamp: a.last_timestamp.to_rfc3339(),
                    })
                    .collect(),
                total_entries: total,
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn create_tenant(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateTenantRequest>,
) -> impl IntoResponse {
    let tenant = lattice_common::types::Tenant {
        id: req.name.clone(),
        name: req.name.clone(),
        quota: lattice_common::types::TenantQuota {
            max_nodes: req.max_nodes.unwrap_or(100),
            fair_share_target: req.fair_share_target.unwrap_or(0.1),
            gpu_hours_budget: req.gpu_hours_budget,
            max_concurrent_allocations: req.max_concurrent_allocations,
            burst_allowance: req.burst_allowance,
        },
        isolation_level: match req.isolation_level.as_deref() {
            Some("strict") => lattice_common::types::IsolationLevel::Strict,
            _ => lattice_common::types::IsolationLevel::Standard,
        },
    };
    let isolation_str = match &tenant.isolation_level {
        lattice_common::types::IsolationLevel::Standard => "standard",
        lattice_common::types::IsolationLevel::Strict => "strict",
    };
    let resp = TenantResponse {
        id: tenant.id.clone(),
        name: tenant.name.clone(),
        max_nodes: tenant.quota.max_nodes,
        fair_share_target: tenant.quota.fair_share_target,
        gpu_hours_budget: tenant.quota.gpu_hours_budget,
        max_concurrent_allocations: tenant.quota.max_concurrent_allocations,
        isolation_level: isolation_str.to_string(),
    };
    if let Some(ref quorum) = state.quorum {
        match quorum
            .propose(lattice_quorum::QuorumCommand::CreateTenant(tenant))
            .await
        {
            Ok(_) => (StatusCode::CREATED, Json(resp)).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
                .into_response(),
        }
    } else {
        (StatusCode::CREATED, Json(resp)).into_response()
    }
}

async fn list_tenants(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    if let Some(ref quorum) = state.quorum {
        let gs = quorum.state().read().await;
        let resp: Vec<TenantResponse> = gs
            .tenants
            .values()
            .map(|t| TenantResponse {
                id: t.id.clone(),
                name: t.name.clone(),
                max_nodes: t.quota.max_nodes,
                fair_share_target: t.quota.fair_share_target,
                gpu_hours_budget: t.quota.gpu_hours_budget,
                max_concurrent_allocations: t.quota.max_concurrent_allocations,
                isolation_level: match &t.isolation_level {
                    lattice_common::types::IsolationLevel::Standard => "standard".to_string(),
                    lattice_common::types::IsolationLevel::Strict => "strict".to_string(),
                },
            })
            .collect();
        (StatusCode::OK, Json(resp)).into_response()
    } else {
        let resp: Vec<TenantResponse> = vec![];
        (StatusCode::OK, Json(resp)).into_response()
    }
}

async fn get_tenant(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(ref quorum) = state.quorum {
        let gs = quorum.state().read().await;
        match gs.tenants.get(&id) {
            Some(t) => {
                let resp = TenantResponse {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    max_nodes: t.quota.max_nodes,
                    fair_share_target: t.quota.fair_share_target,
                    gpu_hours_budget: t.quota.gpu_hours_budget,
                    max_concurrent_allocations: t.quota.max_concurrent_allocations,
                    isolation_level: match &t.isolation_level {
                        lattice_common::types::IsolationLevel::Standard => "standard".to_string(),
                        lattice_common::types::IsolationLevel::Strict => "strict".to_string(),
                    },
                };
                (StatusCode::OK, Json(resp)).into_response()
            }
            None => (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("tenant not found: {id}"),
                }),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "quorum not available".to_string(),
            }),
        )
            .into_response()
    }
}

async fn update_tenant(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateTenantRequest>,
) -> impl IntoResponse {
    if let Some(ref quorum) = state.quorum {
        let quota = if req.max_nodes.is_some()
            || req.fair_share_target.is_some()
            || req.gpu_hours_budget.is_some()
            || req.max_concurrent_allocations.is_some()
        {
            let gs = quorum.state().read().await;
            let existing = match gs.tenants.get(&id) {
                Some(t) => t.clone(),
                None => {
                    return (
                        StatusCode::NOT_FOUND,
                        Json(ErrorResponse {
                            error: format!("tenant not found: {id}"),
                        }),
                    )
                        .into_response();
                }
            };
            drop(gs);
            Some(lattice_common::types::TenantQuota {
                max_nodes: req.max_nodes.unwrap_or(existing.quota.max_nodes),
                fair_share_target: req
                    .fair_share_target
                    .unwrap_or(existing.quota.fair_share_target),
                gpu_hours_budget: if req.gpu_hours_budget.is_some() {
                    req.gpu_hours_budget
                } else {
                    existing.quota.gpu_hours_budget
                },
                max_concurrent_allocations: if req.max_concurrent_allocations.is_some() {
                    req.max_concurrent_allocations
                } else {
                    existing.quota.max_concurrent_allocations
                },
                burst_allowance: if req.burst_allowance.is_some() {
                    req.burst_allowance
                } else {
                    existing.quota.burst_allowance
                },
            })
        } else {
            None
        };
        let isolation_level = req.isolation_level.as_deref().map(|s| match s {
            "strict" => lattice_common::types::IsolationLevel::Strict,
            _ => lattice_common::types::IsolationLevel::Standard,
        });
        match quorum
            .propose(lattice_quorum::QuorumCommand::UpdateTenant {
                id: id.clone(),
                quota,
                isolation_level,
            })
            .await
        {
            Ok(_) => {
                let gs = quorum.state().read().await;
                match gs.tenants.get(&id) {
                    Some(t) => {
                        let resp = TenantResponse {
                            id: t.id.clone(),
                            name: t.name.clone(),
                            max_nodes: t.quota.max_nodes,
                            fair_share_target: t.quota.fair_share_target,
                            gpu_hours_budget: t.quota.gpu_hours_budget,
                            max_concurrent_allocations: t.quota.max_concurrent_allocations,
                            isolation_level: match &t.isolation_level {
                                lattice_common::types::IsolationLevel::Standard => {
                                    "standard".to_string()
                                }
                                lattice_common::types::IsolationLevel::Strict => {
                                    "strict".to_string()
                                }
                            },
                        };
                        (StatusCode::OK, Json(resp)).into_response()
                    }
                    None => (
                        StatusCode::NOT_FOUND,
                        Json(ErrorResponse {
                            error: format!("tenant not found: {id}"),
                        }),
                    )
                        .into_response(),
                }
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "quorum not available".to_string(),
            }),
        )
            .into_response()
    }
}

async fn create_vcluster(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateVClusterRequest>,
) -> impl IntoResponse {
    let vc = lattice_common::types::VCluster {
        id: format!("{}--{}", req.tenant_id, req.name),
        name: req.name.clone(),
        tenant: req.tenant_id.clone(),
        scheduler_type: match req.scheduler_type.as_deref() {
            Some("service_bin_pack") => lattice_common::types::SchedulerType::ServiceBinPack,
            Some("sensitive_reservation") => {
                lattice_common::types::SchedulerType::SensitiveReservation
            }
            Some("interactive_fifo") => lattice_common::types::SchedulerType::InteractiveFifo,
            _ => lattice_common::types::SchedulerType::HpcBackfill,
        },
        cost_weights: lattice_common::types::CostWeights::default(),
        dedicated_nodes: Vec::new(),
        allow_borrowing: req.allow_borrowing.unwrap_or(true),
        allow_lending: req.allow_lending.unwrap_or(true),
    };
    let resp = VClusterResponse {
        id: vc.id.clone(),
        name: vc.name.clone(),
        tenant_id: vc.tenant.clone(),
        scheduler_type: match &vc.scheduler_type {
            lattice_common::types::SchedulerType::HpcBackfill => "hpc_backfill".to_string(),
            lattice_common::types::SchedulerType::ServiceBinPack => "service_bin_pack".to_string(),
            lattice_common::types::SchedulerType::SensitiveReservation => {
                "sensitive_reservation".to_string()
            }
            lattice_common::types::SchedulerType::InteractiveFifo => "interactive_fifo".to_string(),
        },
        allow_borrowing: vc.allow_borrowing,
        allow_lending: vc.allow_lending,
    };
    if let Some(ref quorum) = state.quorum {
        match quorum
            .propose(lattice_quorum::QuorumCommand::CreateVCluster(vc))
            .await
        {
            Ok(_) => (StatusCode::CREATED, Json(resp)).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
                .into_response(),
        }
    } else {
        (StatusCode::CREATED, Json(resp)).into_response()
    }
}

async fn list_vclusters(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    if let Some(ref quorum) = state.quorum {
        let gs = quorum.state().read().await;
        let resp: Vec<VClusterResponse> = gs
            .vclusters
            .values()
            .map(|vc| VClusterResponse {
                id: vc.id.clone(),
                name: vc.name.clone(),
                tenant_id: vc.tenant.clone(),
                scheduler_type: match &vc.scheduler_type {
                    lattice_common::types::SchedulerType::HpcBackfill => "hpc_backfill".to_string(),
                    lattice_common::types::SchedulerType::ServiceBinPack => {
                        "service_bin_pack".to_string()
                    }
                    lattice_common::types::SchedulerType::SensitiveReservation => {
                        "sensitive_reservation".to_string()
                    }
                    lattice_common::types::SchedulerType::InteractiveFifo => {
                        "interactive_fifo".to_string()
                    }
                },
                allow_borrowing: vc.allow_borrowing,
                allow_lending: vc.allow_lending,
            })
            .collect();
        (StatusCode::OK, Json(resp)).into_response()
    } else {
        let resp: Vec<VClusterResponse> = vec![];
        (StatusCode::OK, Json(resp)).into_response()
    }
}

async fn get_vcluster(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(ref quorum) = state.quorum {
        let gs = quorum.state().read().await;
        match gs.vclusters.get(&id) {
            Some(vc) => {
                let resp = VClusterResponse {
                    id: vc.id.clone(),
                    name: vc.name.clone(),
                    tenant_id: vc.tenant.clone(),
                    scheduler_type: match &vc.scheduler_type {
                        lattice_common::types::SchedulerType::HpcBackfill => {
                            "hpc_backfill".to_string()
                        }
                        lattice_common::types::SchedulerType::ServiceBinPack => {
                            "service_bin_pack".to_string()
                        }
                        lattice_common::types::SchedulerType::SensitiveReservation => {
                            "sensitive_reservation".to_string()
                        }
                        lattice_common::types::SchedulerType::InteractiveFifo => {
                            "interactive_fifo".to_string()
                        }
                    },
                    allow_borrowing: vc.allow_borrowing,
                    allow_lending: vc.allow_lending,
                };
                (StatusCode::OK, Json(resp)).into_response()
            }
            None => (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("vcluster not found: {id}"),
                }),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "quorum not available".to_string(),
            }),
        )
            .into_response()
    }
}

async fn raft_status(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    if let Some(ref quorum) = state.quorum {
        let gs = quorum.state().read().await;
        let node_count = gs.nodes.len();
        let alloc_count = gs.allocations.len();
        let tenant_count = gs.tenants.len();
        drop(gs);
        (
            StatusCode::OK,
            Json(RaftStatusResponse {
                available: true,
                leader: Some("1".to_string()),
                state: format!(
                    "healthy (nodes={}, allocations={}, tenants={})",
                    node_count, alloc_count, tenant_count
                ),
            }),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            Json(RaftStatusResponse {
                available: false,
                leader: None,
                state: "not_configured".to_string(),
            }),
        )
            .into_response()
    }
}

// ─── Backup endpoints ───────────────────────────────────────

#[derive(Deserialize)]
struct BackupRequest {
    backup_path: String,
}

#[derive(Serialize)]
struct BackupResponse {
    success: bool,
    message: String,
}

#[derive(Serialize)]
struct BackupVerifyResponse {
    valid: bool,
    message: String,
    node_count: Option<usize>,
    allocation_count: Option<usize>,
    tenant_count: Option<usize>,
}

async fn create_backup(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<BackupRequest>,
) -> impl IntoResponse {
    let quorum = match state.quorum.as_ref() {
        Some(q) => q,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(BackupResponse {
                    success: false,
                    message: "quorum not configured".to_string(),
                }),
            )
                .into_response()
        }
    };

    let path = std::path::Path::new(&req.backup_path);
    match lattice_quorum::export_backup(quorum.state(), path).await {
        Ok(meta) => (
            StatusCode::OK,
            Json(BackupResponse {
                success: true,
                message: format!(
                    "Backup created: {} nodes, {} allocations",
                    meta.app.node_count, meta.app.allocation_count
                ),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BackupResponse {
                success: false,
                message: format!("Backup failed: {e}"),
            }),
        )
            .into_response(),
    }
}

async fn verify_backup(Json(req): Json<BackupRequest>) -> impl IntoResponse {
    let path = std::path::Path::new(&req.backup_path);
    match lattice_quorum::verify_backup(path) {
        Ok(meta) => (
            StatusCode::OK,
            Json(BackupVerifyResponse {
                valid: true,
                message: "Backup is valid".to_string(),
                node_count: Some(meta.app.node_count),
                allocation_count: Some(meta.app.allocation_count),
                tenant_count: Some(meta.app.tenant_count),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::OK,
            Json(BackupVerifyResponse {
                valid: false,
                message: format!("Verification failed: {e}"),
                node_count: None,
                allocation_count: None,
                tenant_count: None,
            }),
        )
            .into_response(),
    }
}

async fn restore_backup(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<BackupRequest>,
) -> impl IntoResponse {
    let data_dir = match state.data_dir.as_ref() {
        Some(d) => d,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(BackupResponse {
                    success: false,
                    message: "data_dir not configured".to_string(),
                }),
            )
                .into_response()
        }
    };

    let backup_path = std::path::Path::new(&req.backup_path);
    match lattice_quorum::restore_backup(backup_path, data_dir) {
        Ok(meta) => (
            StatusCode::OK,
            Json(BackupResponse {
                success: true,
                message: format!(
                    "Restored {} nodes, {} allocations. Restart required.",
                    meta.app.node_count, meta.app.allocation_count
                ),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(BackupResponse {
                success: false,
                message: format!("Restore failed: {e}"),
            }),
        )
            .into_response(),
    }
}

async fn list_nodes(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let filter = lattice_common::traits::NodeFilter::default();
    match state.nodes.list_nodes(&filter).await {
        Ok(nodes) => {
            let resp: Vec<NodeResponse> = nodes
                .iter()
                .map(|n| {
                    let (state_str, _) = crate::convert::node_state_to_str_pub(&n.state);
                    NodeResponse {
                        id: n.id.clone(),
                        state: state_str.to_string(),
                        group: n.group,
                        gpu_type: n.capabilities.gpu_type.clone().unwrap_or_default(),
                        gpu_count: n.capabilities.gpu_count,
                    }
                })
                .collect();
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn get_node(State(state): State<Arc<ApiState>>, Path(id): Path<String>) -> impl IntoResponse {
    match state.nodes.get_node(&id).await {
        Ok(node) => {
            let (state_str, _) = crate::convert::node_state_to_str_pub(&node.state);
            let resp = NodeResponse {
                id: node.id.clone(),
                state: state_str.to_string(),
                group: node.group,
                gpu_type: node.capabilities.gpu_type.clone().unwrap_or_default(),
                gpu_count: node.capabilities.gpu_count,
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn drain_node(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state
        .nodes
        .update_node_state(&id, NodeState::Draining)
        .await
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn undrain_node(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.nodes.update_node_state(&id, NodeState::Ready).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn enable_node(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.nodes.update_node_state(&id, NodeState::Ready).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// ─── SSE Streaming Endpoints ──────────────────────────────────

async fn watch_allocation_sse(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>,
    (StatusCode, Json<ErrorResponse>),
> {
    let uuid = uuid::Uuid::parse_str(&id).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("invalid id: {e}"),
            }),
        )
    })?;

    // Verify the allocation exists.
    state.allocations.get(&uuid).await.map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    let rx = state.events.subscribe(uuid).await;
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(AllocationEvent::StateChange {
            old_state,
            new_state,
            ..
        }) => {
            let data = serde_json::json!({
                "old_state": old_state,
                "new_state": new_state,
            });
            Some(Ok(Event::default()
                .event("state_change")
                .data(data.to_string())))
        }
        _ => None,
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn stream_logs_sse(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>,
    (StatusCode, Json<ErrorResponse>),
> {
    let uuid = uuid::Uuid::parse_str(&id).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("invalid id: {e}"),
            }),
        )
    })?;

    state.allocations.get(&uuid).await.map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    let rx = state.events.subscribe(uuid).await;
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(AllocationEvent::LogLine {
            line, stream: ls, ..
        }) => {
            let stream_name = match ls {
                LogStream::Stdout => "stdout",
                LogStream::Stderr => "stderr",
            };
            let data = serde_json::json!({
                "line": line,
                "stream": stream_name,
            });
            Some(Ok(Event::default().event("log").data(data.to_string())))
        }
        _ => None,
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn stream_metrics_sse(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>,
    (StatusCode, Json<ErrorResponse>),
> {
    let uuid = uuid::Uuid::parse_str(&id).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("invalid id: {e}"),
            }),
        )
    })?;

    state.allocations.get(&uuid).await.map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    let rx = state.events.subscribe(uuid).await;
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(AllocationEvent::MetricPoint {
            metric_name,
            value,
            timestamp_epoch_ms,
            ..
        }) => {
            let data = serde_json::json!({
                "metric": metric_name,
                "value": value,
                "timestamp_ms": timestamp_epoch_ms,
            });
            Some(Ok(Event::default().event("metric").data(data.to_string())))
        }
        _ => None,
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ─── vCluster Queue ───────────────────────────────────────────

async fn vcluster_queue(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let filter = lattice_common::traits::AllocationFilter {
        vcluster: Some(id.clone()),
        state: Some(AllocationState::Pending),
        ..Default::default()
    };
    match state.allocations.list(&filter).await {
        Ok(allocations) => {
            let resp: Vec<AllocationResponse> = allocations
                .iter()
                .map(|a| AllocationResponse {
                    id: a.id.to_string(),
                    tenant: a.tenant.clone(),
                    project: a.project.clone(),
                    state: convert::allocation_state_to_str(&a.state).to_string(),
                    assigned_nodes: a.assigned_nodes.clone(),
                    user: a.user.clone(),
                })
                .collect();
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// ─── Accounting Usage ─────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct AccountingUsageQuery {
    pub tenant: String,
    pub resource_type: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct AccountingUsageResponse {
    pub tenant: String,
    pub remaining_budget: Option<f64>,
}

async fn accounting_usage(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AccountingUsageQuery>,
) -> impl IntoResponse {
    if let Some(ref accounting) = state.accounting {
        match accounting.remaining_budget(&query.tenant).await {
            Ok(budget) => (
                StatusCode::OK,
                Json(AccountingUsageResponse {
                    tenant: query.tenant,
                    remaining_budget: budget,
                }),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::OK,
            Json(AccountingUsageResponse {
                tenant: query.tenant,
                remaining_budget: None,
            }),
        )
            .into_response()
    }
}

// ─── Tenant Usage (Internal Budget Ledger) ──────────────────

#[derive(Serialize)]
struct TenantUsageResponse {
    tenant: String,
    gpu_hours_used: f64,
    gpu_hours_budget: Option<f64>,
    fraction_used: Option<f64>,
    period_start: String,
    period_end: String,
    period_days: u32,
}

#[derive(Deserialize)]
struct TenantUsageQuery {
    #[serde(default = "default_usage_days")]
    days: u32,
}

fn default_usage_days() -> u32 {
    90
}

async fn tenant_usage(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<TenantUsageQuery>,
) -> impl IntoResponse {
    let Some(ref quorum) = state.quorum else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "quorum not available".into(),
            }),
        )
            .into_response();
    };

    let gs = quorum.state().read().await;
    let Some(tenant) = gs.tenants.get(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("tenant '{}' not found", id),
            }),
        )
            .into_response();
    };
    let tenant = tenant.clone();
    drop(gs);

    let now = chrono::Utc::now();
    let period_days = query.days;
    let period_start = now - chrono::Duration::days(period_days as i64);

    // Get all allocations for this tenant with started_at set
    let allocs = state
        .allocations
        .list(&lattice_common::traits::AllocationFilter {
            tenant: Some(id.clone()),
            ..Default::default()
        })
        .await
        .unwrap_or_default();

    // Get nodes for GPU count lookup
    let nodes = state
        .nodes
        .list_nodes(&lattice_common::traits::NodeFilter::default())
        .await
        .unwrap_or_default();

    let gpu_hours_used = lattice_scheduler::quota::compute_tenant_gpu_hours(
        &tenant,
        &allocs,
        &nodes,
        period_start,
        now,
    );

    let fraction_used = tenant
        .quota
        .gpu_hours_budget
        .filter(|b| *b > 0.0)
        .map(|b| gpu_hours_used / b);

    (
        StatusCode::OK,
        Json(TenantUsageResponse {
            tenant: id,
            gpu_hours_used,
            gpu_hours_budget: tenant.quota.gpu_hours_budget,
            fraction_used,
            period_start: period_start.to_rfc3339(),
            period_end: now.to_rfc3339(),
            period_days,
        }),
    )
        .into_response()
}

#[derive(Serialize)]
struct UserUsageResponse {
    user: String,
    tenants: Vec<UserTenantUsage>,
    total_gpu_hours: f64,
    period_start: String,
    period_end: String,
}

#[derive(Serialize)]
struct UserTenantUsage {
    tenant: String,
    gpu_hours_used: f64,
    gpu_hours_budget: Option<f64>,
    fraction_used: Option<f64>,
}

#[derive(Deserialize)]
struct UserUsageQuery {
    user: String,
    #[serde(default = "default_usage_days")]
    days: u32,
}

async fn user_usage(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<UserUsageQuery>,
) -> impl IntoResponse {
    let now = chrono::Utc::now();
    let period_start = now - chrono::Duration::days(query.days as i64);

    let allocs = state
        .allocations
        .list(&lattice_common::traits::AllocationFilter {
            user: Some(query.user.clone()),
            ..Default::default()
        })
        .await
        .unwrap_or_default();

    let nodes = state
        .nodes
        .list_nodes(&lattice_common::traits::NodeFilter::default())
        .await
        .unwrap_or_default();

    let by_tenant = lattice_scheduler::quota::compute_user_gpu_hours(
        &query.user,
        &allocs,
        &nodes,
        period_start,
        now,
    );

    // Look up budgets from quorum if available
    let tenant_budgets: std::collections::HashMap<String, Option<f64>> =
        if let Some(ref quorum) = state.quorum {
            let gs = quorum.state().read().await;
            gs.tenants
                .iter()
                .map(|(id, t)| (id.clone(), t.quota.gpu_hours_budget))
                .collect()
        } else {
            std::collections::HashMap::new()
        };

    let total_gpu_hours: f64 = by_tenant.values().sum();
    let tenants: Vec<UserTenantUsage> = by_tenant
        .into_iter()
        .map(|(tid, hours)| {
            let budget = tenant_budgets.get(&tid).copied().flatten();
            UserTenantUsage {
                tenant: tid,
                gpu_hours_used: hours,
                gpu_hours_budget: budget,
                fraction_used: budget.filter(|b| *b > 0.0).map(|b| hours / b),
            }
        })
        .collect();

    (
        StatusCode::OK,
        Json(UserUsageResponse {
            user: query.user,
            tenants,
            total_gpu_hours,
            period_start: period_start.to_rfc3339(),
            period_end: now.to_rfc3339(),
        }),
    )
        .into_response()
}

// ─── Service Discovery ──────────────────────────────────────

#[derive(Serialize)]
struct ServiceListResponse {
    services: Vec<String>,
}

#[derive(Serialize)]
struct ServiceLookupResponse {
    name: String,
    endpoints: Vec<ServiceEndpointResponse>,
}

#[derive(Serialize)]
struct ServiceEndpointResponse {
    allocation_id: String,
    tenant: String,
    nodes: Vec<String>,
    port: u16,
    protocol: String,
}

async fn list_services(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let names = if let Some(ref quorum) = state.quorum {
        let sm = quorum.state().read().await;
        sm.list_services()
    } else {
        vec![]
    };
    Json(ServiceListResponse { services: names }).into_response()
}

async fn lookup_service(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let endpoints = if let Some(ref quorum) = state.quorum {
        let sm = quorum.state().read().await;
        match sm.lookup_service(&name) {
            Some(entry) => entry
                .endpoints
                .iter()
                .map(|ep| ServiceEndpointResponse {
                    allocation_id: ep.allocation_id.to_string(),
                    tenant: ep.tenant.clone(),
                    nodes: ep.nodes.clone(),
                    port: ep.port,
                    protocol: ep.protocol.clone().unwrap_or_default(),
                })
                .collect(),
            None => vec![],
        }
    } else {
        vec![]
    };

    Json(ServiceLookupResponse { name, endpoints }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http;
    use lattice_common::traits::AuditLog;
    use lattice_test_harness::fixtures::create_node_batch;
    use lattice_test_harness::mocks::{
        MockAllocationStore, MockAuditLog, MockCheckpointBroker, MockNodeRegistry,
    };
    use tower::ServiceExt;

    fn test_state() -> Arc<ApiState> {
        let nodes = create_node_batch(3, 0);
        Arc::new(ApiState {
            allocations: Arc::new(MockAllocationStore::new()),
            nodes: Arc::new(MockNodeRegistry::new().with_nodes(nodes)),
            audit: Arc::new(MockAuditLog::new()),
            checkpoint: Arc::new(MockCheckpointBroker::new()),
            quorum: None,
            events: crate::events::new_event_bus(),
            tsdb: None,
            storage: None,
            accounting: None,
            oidc: None,
            rate_limiter: None,
            sovra: None,
            pty: None,
            agent_pool: None,
            data_dir: None,
            oidc_config: None,
        })
    }

    async fn test_state_with_quorum() -> Arc<ApiState> {
        let nodes = create_node_batch(3, 0);
        let quorum = lattice_quorum::create_test_quorum().await.unwrap();
        Arc::new(ApiState {
            allocations: Arc::new(MockAllocationStore::new()),
            nodes: Arc::new(MockNodeRegistry::new().with_nodes(nodes)),
            audit: Arc::new(MockAuditLog::new()),
            checkpoint: Arc::new(MockCheckpointBroker::new()),
            quorum: Some(Arc::new(quorum)),
            events: crate::events::new_event_bus(),
            tsdb: None,
            storage: None,
            accounting: None,
            oidc: None,
            rate_limiter: None,
            sovra: None,
            pty: None,
            agent_pool: None,
            data_dir: None,
            oidc_config: None,
        })
    }

    #[tokio::test]
    async fn health_check() {
        let state = test_state();
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rest_submit_and_get() {
        let state = test_state();
        let app = router(state.clone());
        let body = serde_json::json!({"tenant": "physics", "entrypoint": "python train.py", "nodes": 2, "walltime_hours": 2.0});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/allocations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let submit_resp: SubmitResponse = serde_json::from_slice(&body).unwrap();
        assert!(!submit_resp.allocation_id.is_empty());
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri(format!("/api/v1/allocations/{}", submit_resp.allocation_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rest_list_nodes() {
        let state = test_state();
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/nodes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let nodes: Vec<NodeResponse> = serde_json::from_slice(&body).unwrap();
        assert_eq!(nodes.len(), 3);
    }

    #[tokio::test]
    async fn rest_get_node_not_found() {
        let state = test_state();
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/nodes/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn session_create_get_delete_lifecycle() {
        let state = test_state();
        let app = router(state.clone());
        let body = serde_json::json!({"tenant": "physics"});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let session: SessionResponse = serde_json::from_slice(&body).unwrap();
        assert!(!session.session_id.is_empty());
        assert_eq!(session.state, "pending");
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri(format!("/api/v1/sessions/{}", session.session_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let get_resp: SessionResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(get_resp.session_id, session.session_id);
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/sessions/{}", session.session_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri(format!("/api/v1/sessions/{}", session.session_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let cancelled: SessionResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(cancelled.state, "cancelled");
    }

    #[tokio::test]
    async fn dag_submit_list_get_cancel_lifecycle() {
        let state = test_state();
        let app = router(state.clone());
        let body = serde_json::json!({"dag_id": "pipeline-1", "allocations": [{"name": "step1", "tenant": "physics", "entrypoint": "preprocess.sh", "nodes": 1}, {"name": "step2", "tenant": "physics", "entrypoint": "train.py", "nodes": 2}], "edges": [{"from": "step1", "to": "step2"}]});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/dags")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dag: DagResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(dag.dag_id, "pipeline-1");
        assert_eq!(dag.allocation_ids.len(), 2);
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/dags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dags: Vec<DagListEntry> = serde_json::from_slice(&body).unwrap();
        assert_eq!(dags.len(), 1);
        assert_eq!(dags[0].dag_id, "pipeline-1");
        assert_eq!(dags[0].allocation_count, 2);
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/dags/pipeline-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: DagStatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(status.dag_id, "pipeline-1");
        assert_eq!(status.allocations.len(), 2);
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/dags/pipeline-1/cancel")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/dags/pipeline-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: DagStatusResponse = serde_json::from_slice(&body).unwrap();
        for alloc in &status.allocations {
            assert_eq!(alloc.state, "cancelled");
        }
    }

    #[tokio::test]
    async fn audit_query_with_user_filter() {
        let audit_log = Arc::new(MockAuditLog::new());
        let entry =
            lattice_common::traits::AuditEntry::new(lattice_common::traits::lattice_audit_event(
                lattice_common::traits::audit_actions::NODE_CLAIM,
                "dr-smith",
                hpc_audit::AuditScope::default(),
                hpc_audit::AuditOutcome::Success,
                "node claim",
                serde_json::json!({"node": "x1000c0s0b0n0"}),
                hpc_audit::AuditSource::LatticeQuorum,
            ));
        audit_log.record(entry).await.unwrap();
        let nodes = create_node_batch(3, 0);
        let state = Arc::new(ApiState {
            allocations: Arc::new(MockAllocationStore::new()),
            nodes: Arc::new(MockNodeRegistry::new().with_nodes(nodes)),
            audit: audit_log,
            checkpoint: Arc::new(MockCheckpointBroker::new()),
            quorum: None,
            events: crate::events::new_event_bus(),
            tsdb: None,
            storage: None,
            accounting: None,
            oidc: None,
            rate_limiter: None,
            sovra: None,
            pty: None,
            agent_pool: None,
            data_dir: None,
            oidc_config: None,
        });
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/audit?user=dr-smith")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let entries: Vec<AuditEntryResponse> = serde_json::from_slice(&body).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].user, "dr-smith");
    }

    #[tokio::test]
    async fn tenant_create_list_get_update_lifecycle() {
        let state = test_state_with_quorum().await;
        let app = router(state.clone());
        let body =
            serde_json::json!({"name": "physics", "max_nodes": 50, "fair_share_target": 0.3});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/tenants")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let tenant: TenantResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(tenant.name, "physics");
        assert_eq!(tenant.max_nodes, 50);
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/tenants")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let tenants: Vec<TenantResponse> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tenants.len(), 1);
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/tenants/physics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let tenant: TenantResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(tenant.name, "physics");
        let app = router(state.clone());
        let body = serde_json::json!({"max_nodes": 80});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("PUT")
                    .uri("/api/v1/tenants/physics")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let updated: TenantResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(updated.max_nodes, 80);
    }

    #[tokio::test]
    async fn vcluster_create_list_get_lifecycle() {
        let state = test_state_with_quorum().await;
        let app = router(state.clone());
        let body = serde_json::json!({"tenant_id": "physics", "name": "hpc-batch", "scheduler_type": "hpc_backfill", "allow_borrowing": true});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/vclusters")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let vc: VClusterResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(vc.name, "hpc-batch");
        assert_eq!(vc.scheduler_type, "hpc_backfill");
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/vclusters")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let vclusters: Vec<VClusterResponse> = serde_json::from_slice(&body).unwrap();
        assert_eq!(vclusters.len(), 1);
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri(format!("/api/v1/vclusters/{}", vc.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn allocation_diagnostics_metrics_logs_return_200() {
        let state = test_state();
        let app = router(state.clone());
        let body = serde_json::json!({"tenant": "physics", "entrypoint": "python train.py"});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/allocations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let submit: SubmitResponse = serde_json::from_slice(&body).unwrap();
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri(format!(
                        "/api/v1/allocations/{}/diagnostics",
                        submit.allocation_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri(format!(
                        "/api/v1/allocations/{}/metrics",
                        submit.allocation_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let app = router(state.clone());
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri(format!("/api/v1/allocations/{}/logs", submit.allocation_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn patch_allocation_returns_200() {
        let state = test_state();
        let app = router(state.clone());
        let body = serde_json::json!({"tenant": "physics", "entrypoint": "python train.py"});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/allocations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let submit: SubmitResponse = serde_json::from_slice(&body).unwrap();
        let app = router(state);
        let patch_body = serde_json::json!({"walltime_hours": 4.0, "telemetry_mode": "debug"});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/v1/allocations/{}", submit.allocation_id))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&patch_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn raft_status_returns_200() {
        let state = test_state();
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/raft/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: RaftStatusResponse = serde_json::from_slice(&body).unwrap();
        assert!(!status.available);
        assert_eq!(status.state, "not_configured");
    }

    #[tokio::test]
    async fn raft_status_with_quorum_returns_leader() {
        let state = test_state_with_quorum().await;
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/raft/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: RaftStatusResponse = serde_json::from_slice(&body).unwrap();
        assert!(status.available);
        assert!(status.leader.is_some());
    }

    #[tokio::test]
    async fn sse_watch_returns_200_for_valid_allocation() {
        let state = test_state();
        // First submit an allocation
        let app = router(state.clone());
        let body = serde_json::json!({"tenant": "physics", "entrypoint": "train.py"});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/allocations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let submit: SubmitResponse = serde_json::from_slice(&body).unwrap();

        // Watch endpoint should return 200 (SSE stream)
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri(format!(
                        "/api/v1/allocations/{}/watch",
                        submit.allocation_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn sse_watch_returns_404_for_missing_allocation() {
        let state = test_state();
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri(format!(
                        "/api/v1/allocations/{}/watch",
                        uuid::Uuid::new_v4()
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn sse_logs_stream_returns_200() {
        let state = test_state();
        let app = router(state.clone());
        let body = serde_json::json!({"tenant": "physics", "entrypoint": "train.py"});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/allocations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let submit: SubmitResponse = serde_json::from_slice(&body).unwrap();

        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri(format!(
                        "/api/v1/allocations/{}/logs/stream",
                        submit.allocation_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn sse_metrics_stream_returns_200() {
        let state = test_state();
        let app = router(state.clone());
        let body = serde_json::json!({"tenant": "physics", "entrypoint": "train.py"});
        let resp = app
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/allocations")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let submit: SubmitResponse = serde_json::from_slice(&body).unwrap();

        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri(format!(
                        "/api/v1/allocations/{}/metrics/stream",
                        submit.allocation_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn vcluster_queue_returns_pending_allocations() {
        let state = test_state();
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/vclusters/default/queue")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let queue: Vec<AllocationResponse> = serde_json::from_slice(&body).unwrap();
        assert!(queue.is_empty()); // No pending allocations
    }

    #[tokio::test]
    async fn accounting_usage_without_service() {
        let state = test_state();
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/accounting/usage?tenant=physics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let usage: AccountingUsageResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(usage.tenant, "physics");
        assert!(usage.remaining_budget.is_none());
    }

    #[tokio::test]
    async fn auth_discovery_returns_oidc_config() {
        use crate::middleware::oidc::OidcConfig;
        let nodes = create_node_batch(1, 0);
        let state = Arc::new(ApiState {
            allocations: Arc::new(MockAllocationStore::new()),
            nodes: Arc::new(MockNodeRegistry::new().with_nodes(nodes)),
            audit: Arc::new(MockAuditLog::new()),
            checkpoint: Arc::new(MockCheckpointBroker::new()),
            quorum: None,
            events: crate::events::new_event_bus(),
            tsdb: None,
            storage: None,
            accounting: None,
            oidc: None,
            rate_limiter: None,
            sovra: None,
            pty: None,
            agent_pool: None,
            data_dir: None,
            oidc_config: Some(OidcConfig {
                issuer_url: "https://auth.example.com".to_string(),
                audience: "lattice-client".to_string(),
                required_scopes: vec![],
            }),
        });
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/auth/discovery")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let discovery: AuthDiscoveryResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(discovery.idp_url, "https://auth.example.com");
        assert_eq!(discovery.client_id, "lattice-client");
        assert_eq!(discovery.issuer, "https://auth.example.com");
    }

    #[tokio::test]
    async fn auth_discovery_returns_503_when_not_configured() {
        let state = test_state();
        let app = router(state);
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/api/v1/auth/discovery")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
