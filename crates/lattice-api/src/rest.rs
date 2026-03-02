//! REST gateway — axum routes wrapping the same backing stores.
//!
//! Provides JSON endpoints as an alternative to gRPC for CLI and
//! browser clients.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Json;
use serde::{Deserialize, Serialize};

use lattice_common::types::AllocationState;
use lattice_common::types::NodeState;

use crate::convert;
use crate::state::ApiState;

/// Build the axum REST router.
pub fn router(state: Arc<ApiState>) -> axum::Router {
    axum::Router::new()
        // Allocation endpoints
        .route("/api/v1/allocations", post(submit_allocation))
        .route("/api/v1/allocations", get(list_allocations))
        .route("/api/v1/allocations/:id", get(get_allocation))
        .route("/api/v1/allocations/:id/cancel", post(cancel_allocation))
        // Node endpoints
        .route("/api/v1/nodes", get(list_nodes))
        .route("/api/v1/nodes/:id", get(get_node))
        .route("/api/v1/nodes/:id/drain", post(drain_node))
        .route("/api/v1/nodes/:id/undrain", post(undrain_node))
        // Health
        .route("/healthz", get(healthz))
        .with_state(state)
}

// ─── Request/Response types ──────────────────────────────────

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

#[derive(Serialize)]
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

// ─── Handlers ─────────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http;
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

        let body = serde_json::json!({
            "tenant": "physics",
            "entrypoint": "python train.py",
            "nodes": 2,
            "walltime_hours": 2.0
        });

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

        // Now get it
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
}
