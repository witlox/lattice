//! Integration tests for the lattice-api crate.
//!
//! These tests exercise multi-component interactions: API state wiring,
//! event bus pub/sub, diagnostics collection, OIDC validation, rate limiting,
//! and gRPC service handlers working together through shared state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::time::{timeout, Duration};
use tonic::Request;
use uuid::Uuid;

use lattice_api::diagnostics::{
    AllocationDiagnostics, DiagnosticsCollector, NetworkDiagnostics, NodeDiagState,
    StorageDiagnostics,
};
use lattice_api::events::{AllocationEvent, EventBus, LogStream};
use lattice_api::grpc::admin_service::LatticeAdminService;
use lattice_api::grpc::allocation_service::LatticeAllocationService;
use lattice_api::middleware::oidc::{OidcConfig, OidcError, OidcValidator, StubOidcValidator};
use lattice_api::middleware::rate_limit::{RateLimitConfig, RateLimiter};
use lattice_api::state::ApiState;

use lattice_common::error::LatticeError;
use lattice_common::proto::lattice::v1 as pb;
use lattice_common::proto::lattice::v1::admin_service_server::AdminService;
use lattice_common::proto::lattice::v1::allocation_service_server::AllocationService;
use lattice_common::tsdb_client::{MetricSample, MetricSeries, QueryResult, TsdbClient};
use lattice_test_harness::mocks::{
    MockAllocationStore, MockAuditLog, MockCheckpointBroker, MockNodeRegistry,
};

// ─── Mock TSDB for integration tests ─────────────────────────

/// A TSDB mock that returns pre-programmed query results keyed by metric prefix.
struct MockTsdb {
    responses: Mutex<HashMap<String, QueryResult>>,
    always_error: bool,
}

impl MockTsdb {
    fn new() -> Self {
        Self {
            responses: Mutex::new(HashMap::new()),
            always_error: false,
        }
    }

    fn with_error() -> Self {
        Self {
            responses: Mutex::new(HashMap::new()),
            always_error: true,
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
        if self.always_error {
            return Err(LatticeError::Internal("mock tsdb error".into()));
        }
        let responses = self.responses.lock().unwrap();
        for (prefix, result) in responses.iter() {
            if promql.starts_with(prefix.as_str()) {
                return Ok(result.clone());
            }
        }
        Ok(QueryResult { series: vec![] })
    }

    async fn query_instant(&self, _promql: &str) -> Result<QueryResult, LatticeError> {
        if self.always_error {
            return Err(LatticeError::Internal("mock tsdb error".into()));
        }
        Ok(QueryResult { series: vec![] })
    }
}

// ─── Shared helpers ──────────────────────────────────────────

fn test_state() -> Arc<ApiState> {
    Arc::new(ApiState {
        allocations: Arc::new(MockAllocationStore::new()),
        nodes: Arc::new(MockNodeRegistry::new()),
        audit: Arc::new(MockAuditLog::new()),
        checkpoint: Arc::new(MockCheckpointBroker::new()),
        quorum: None,
        events: Arc::new(EventBus::new()),
    })
}

fn stub_oidc_config() -> OidcConfig {
    OidcConfig {
        issuer_url: "https://auth.example.com".to_string(),
        audience: "lattice-api".to_string(),
        required_scopes: vec!["jobs:read".to_string(), "jobs:write".to_string()],
    }
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

// ─── Test 1: API state wiring ────────────────────────────────
// Create an ApiState with all components and verify multi-component
// operations flow through the shared state correctly.

#[tokio::test]
async fn api_state_wiring_submit_and_retrieve_via_shared_state() {
    let state = test_state();
    let alloc_svc = LatticeAllocationService::new(state.clone());

    // Submit an allocation through the gRPC service
    let submit_resp = alloc_svc
        .submit(Request::new(single_submit_request("physics")))
        .await
        .unwrap();
    let ids = &submit_resp.get_ref().allocation_ids;
    assert_eq!(ids.len(), 1);

    // Retrieve through the same shared state via get
    let get_resp = alloc_svc
        .get(Request::new(pb::GetAllocationRequest {
            allocation_id: ids[0].clone(),
        }))
        .await
        .unwrap();
    assert_eq!(get_resp.get_ref().state, "pending");
    assert_eq!(get_resp.get_ref().allocation_id, ids[0]);

    // Verify it also shows up in list
    let list_resp = alloc_svc
        .list(Request::new(pb::ListAllocationsRequest {
            tenant: "physics".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(list_resp.get_ref().allocations.len(), 1);

    // Cancel and verify state change propagates
    alloc_svc
        .cancel(Request::new(pb::CancelRequest {
            allocation_id: ids[0].clone(),
        }))
        .await
        .unwrap();

    let get_after_cancel = alloc_svc
        .get(Request::new(pb::GetAllocationRequest {
            allocation_id: ids[0].clone(),
        }))
        .await
        .unwrap();
    assert_eq!(get_after_cancel.get_ref().state, "cancelled");
}

// ─── Test 2: EventBus pub/sub across components ──────────────
// Subscribe to events, publish allocation state changes via the event bus,
// and verify subscribers on different allocations receive only their events.

#[tokio::test]
async fn event_bus_pubsub_across_allocation_boundaries() {
    let state = test_state();
    let bus = &state.events;

    let alloc_a = Uuid::new_v4();
    let alloc_b = Uuid::new_v4();

    // Subscribe to both allocations
    let mut rx_a = bus.subscribe(alloc_a).await;
    let mut rx_b = bus.subscribe(alloc_b).await;

    // Publish a state change for allocation A
    let event_a = AllocationEvent::StateChange {
        allocation_id: alloc_a,
        old_state: "pending".to_string(),
        new_state: "running".to_string(),
    };
    let count_a = bus.publish(event_a.clone()).await;
    assert_eq!(count_a, 1, "exactly one subscriber for alloc_a");

    // Publish a log line for allocation B
    let event_b = AllocationEvent::LogLine {
        allocation_id: alloc_b,
        line: "epoch 1 complete".to_string(),
        stream: LogStream::Stdout,
    };
    let count_b = bus.publish(event_b.clone()).await;
    assert_eq!(count_b, 1, "exactly one subscriber for alloc_b");

    // Publish a metric point for allocation A
    let metric_event = AllocationEvent::MetricPoint {
        allocation_id: alloc_a,
        metric_name: "gpu_utilization".to_string(),
        value: 0.95,
        timestamp_epoch_ms: 1000,
    };
    bus.publish(metric_event.clone()).await;

    // Verify rx_a receives events for alloc_a only
    let received_a1 = timeout(Duration::from_millis(100), rx_a.recv())
        .await
        .expect("should not timeout")
        .expect("should receive state change");
    assert_eq!(received_a1, event_a);

    let received_a2 = timeout(Duration::from_millis(100), rx_a.recv())
        .await
        .expect("should not timeout")
        .expect("should receive metric event");
    assert_eq!(received_a2, metric_event);

    // Verify rx_b receives only alloc_b events
    let received_b1 = timeout(Duration::from_millis(100), rx_b.recv())
        .await
        .expect("should not timeout")
        .expect("should receive log line");
    assert_eq!(received_b1, event_b);

    // rx_b should have no more events
    assert!(rx_b.try_recv().is_err());

    // Clean up channel for alloc_a and verify no new events arrive
    bus.remove(&alloc_a).await;
    let post_remove_count = bus
        .publish(AllocationEvent::StateChange {
            allocation_id: alloc_a,
            old_state: "running".to_string(),
            new_state: "completed".to_string(),
        })
        .await;
    assert_eq!(post_remove_count, 0, "no subscribers after channel removed");
}

// ─── Test 3: Diagnostics collector with TSDB integration ─────
// Create AllocationDiagnostics through the DiagnosticsCollector,
// verify it aggregates node states, network, storage, and TSDB metrics.

#[tokio::test]
async fn diagnostics_collector_aggregates_multi_source_data() {
    let tsdb = Arc::new(MockTsdb::new());
    let alloc_id = Uuid::new_v4();

    // Register two metrics in the mock TSDB
    tsdb.register(
        "gpu_utilization",
        vec![(1_000_000, 0.75), (1_000_030, 0.80), (1_000_060, 0.85)],
    );
    tsdb.register("memory_used_bytes", vec![(1_000_000, 4096.0)]);

    let collector = DiagnosticsCollector::new(tsdb);

    let node_states = vec![
        NodeDiagState {
            node_id: "x1000c0s0b0n0".to_string(),
            agent_connected: true,
            phase: "Running".to_string(),
            last_heartbeat_age_secs: 5,
        },
        NodeDiagState {
            node_id: "x1000c0s0b0n1".to_string(),
            agent_connected: true,
            phase: "Running".to_string(),
            last_heartbeat_age_secs: 3,
        },
    ];

    let network = NetworkDiagnostics {
        connectivity_ok: true,
        vni: Some(2001),
        avg_latency_us: 1.5,
        avg_bandwidth_gbps: 200.0,
    };

    let storage = StorageDiagnostics {
        mounts_healthy: true,
        io_throughput_gbps: 10.0,
        cache_hit_rate: 0.95,
    };

    let result = collector
        .collect(
            alloc_id,
            node_states,
            network,
            storage,
            vec!["training step 42".to_string(), "loss=0.31".to_string()],
            &["gpu_utilization", "memory_used_bytes"],
        )
        .await
        .unwrap();

    // Verify aggregated diagnostics
    assert_eq!(result.allocation_id, alloc_id);
    assert_eq!(result.node_states.len(), 2);
    assert!(result.is_healthy());
    assert_eq!(result.network_health.vni, Some(2001));
    assert!((result.storage_health.cache_hit_rate - 0.95).abs() < f64::EPSILON);
    assert_eq!(result.log_tail.len(), 2);

    // Verify TSDB metrics were fetched (latest value for each metric)
    assert_eq!(
        result.recent_metrics.get("gpu_utilization").copied(),
        Some(0.85),
        "should pick the latest timestamp value"
    );
    assert_eq!(
        result.recent_metrics.get("memory_used_bytes").copied(),
        Some(4096.0)
    );

    // Verify unhealthy detection when a node is disconnected
    let unhealthy_diag = AllocationDiagnostics {
        allocation_id: alloc_id,
        node_states: vec![
            NodeDiagState {
                node_id: "n0".to_string(),
                agent_connected: true,
                phase: "Running".to_string(),
                last_heartbeat_age_secs: 2,
            },
            NodeDiagState {
                node_id: "n1".to_string(),
                agent_connected: false,
                phase: "Unknown".to_string(),
                last_heartbeat_age_secs: 120,
            },
        ],
        network_health: NetworkDiagnostics::default(),
        storage_health: StorageDiagnostics::default(),
        log_tail: vec![],
        recent_metrics: HashMap::new(),
    };
    assert!(
        !unhealthy_diag.is_healthy(),
        "should be unhealthy when a node agent is disconnected"
    );
}

// ─── Test 4: OIDC validator with various token types ─────────
// Test the StubOidcValidator with valid, expired, wrong-issuer,
// missing-scope, and invalid tokens, verifying cross-module claim propagation.

#[tokio::test]
async fn oidc_validator_token_types_and_claim_propagation() {
    let config = stub_oidc_config();
    let validator = StubOidcValidator::new(config.clone());

    // Valid token: claims should propagate config values
    let claims = validator.validate_token("valid-alice").await.unwrap();
    assert_eq!(claims.sub, "alice");
    assert_eq!(claims.iss, config.issuer_url);
    assert_eq!(claims.aud, config.audience);
    assert_eq!(claims.scopes, config.required_scopes);
    assert_eq!(claims.exp, i64::MAX);

    // Expired token
    let err = validator
        .validate_token("expired-session")
        .await
        .unwrap_err();
    assert_eq!(err, OidcError::Expired);

    // Wrong issuer
    let err = validator
        .validate_token("wrong-issuer-external")
        .await
        .unwrap_err();
    assert_eq!(err, OidcError::WrongIssuer);

    // Missing scope: the remainder after "no-scope-" becomes the scope name
    let err = validator
        .validate_token("no-scope-jobs:admin")
        .await
        .unwrap_err();
    assert_eq!(err, OidcError::MissingScope("jobs:admin".to_string()));

    // Invalid token (no recognized prefix)
    let err = validator.validate_token("garbage").await.unwrap_err();
    assert_eq!(err, OidcError::InvalidToken);

    // Empty token
    let err = validator.validate_token("").await.unwrap_err();
    assert_eq!(err, OidcError::InvalidToken);

    // Validate that two different valid users get different subjects
    // but the same issuer/audience/scopes (config-driven)
    let alice = validator.validate_token("valid-alice").await.unwrap();
    let bob = validator.validate_token("valid-bob").await.unwrap();
    assert_ne!(alice.sub, bob.sub);
    assert_eq!(alice.iss, bob.iss);
    assert_eq!(alice.aud, bob.aud);
    assert_eq!(alice.scopes, bob.scopes);
}

// ─── Test 5: Rate limiter per-user isolation ─────────────────
// Test per-user rate limiting: one user hitting their limit does not
// affect another user's bucket.

#[tokio::test]
async fn rate_limiter_per_user_isolation() {
    // 4 req/min + burst of 2 = capacity of 6 tokens
    let limiter = RateLimiter::new(RateLimitConfig {
        max_requests_per_minute: 4,
        burst_size: 2,
    });

    // Alice exhausts her 6 tokens
    for i in 0..6 {
        assert!(
            limiter.check("alice").is_ok(),
            "alice request {i} should succeed"
        );
    }

    // Alice should now be rate-limited
    let err = limiter.check("alice").unwrap_err();
    assert!(
        err.retry_after_secs > 0,
        "alice should have a positive retry-after"
    );

    // Bob is completely unaffected, has a full bucket
    for i in 0..6 {
        assert!(
            limiter.check("bob").is_ok(),
            "bob request {i} should succeed even though alice is limited"
        );
    }

    // Bob is also now limited
    assert!(
        limiter.check("bob").is_err(),
        "bob should be limited after exhausting tokens"
    );

    // Carol (never seen before) still has full capacity
    assert!(
        limiter.check("carol").is_ok(),
        "carol should have full bucket on first request"
    );
}

// ─── Test 6: gRPC submit → list → get → cancel lifecycle ────
// Full allocation lifecycle through the gRPC service layer,
// verifying state transitions and data consistency.

#[tokio::test]
async fn grpc_allocation_full_lifecycle() {
    let state = test_state();
    let svc = LatticeAllocationService::new(state.clone());

    // Submit two allocations for different tenants
    let resp_physics = svc
        .submit(Request::new(single_submit_request("physics")))
        .await
        .unwrap();
    let resp_bio = svc
        .submit(Request::new(single_submit_request("biology")))
        .await
        .unwrap();

    let physics_id = &resp_physics.get_ref().allocation_ids[0];
    let bio_id = &resp_bio.get_ref().allocation_ids[0];
    assert_ne!(physics_id, bio_id, "allocation IDs should be unique");

    // List for physics tenant should return only the physics allocation
    let list_physics = svc
        .list(Request::new(pb::ListAllocationsRequest {
            tenant: "physics".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(list_physics.get_ref().allocations.len(), 1);
    assert_eq!(
        list_physics.get_ref().allocations[0].allocation_id,
        *physics_id
    );

    // Get each allocation individually
    let get_physics = svc
        .get(Request::new(pb::GetAllocationRequest {
            allocation_id: physics_id.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(get_physics.get_ref().state, "pending");

    let get_bio = svc
        .get(Request::new(pb::GetAllocationRequest {
            allocation_id: bio_id.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(get_bio.get_ref().state, "pending");

    // Cancel the physics allocation
    let cancel_resp = svc
        .cancel(Request::new(pb::CancelRequest {
            allocation_id: physics_id.clone(),
        }))
        .await
        .unwrap();
    assert!(cancel_resp.get_ref().success);

    // Verify physics is cancelled but biology is still pending
    let after_cancel_physics = svc
        .get(Request::new(pb::GetAllocationRequest {
            allocation_id: physics_id.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(after_cancel_physics.get_ref().state, "cancelled");

    let after_cancel_bio = svc
        .get(Request::new(pb::GetAllocationRequest {
            allocation_id: bio_id.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(after_cancel_bio.get_ref().state, "pending");
}

// ─── Test 7: gRPC DAG submit and cancel ─────────────────────
// Submit a multi-step DAG through the allocation service, then cancel
// the entire DAG and verify all steps are cancelled.

#[tokio::test]
async fn grpc_dag_submit_and_cancel_propagation() {
    let state = test_state();
    let svc = LatticeAllocationService::new(state.clone());

    // Submit a 3-step DAG: preprocess → train → postprocess
    let dag_req = pb::SubmitRequest {
        submission: Some(pb::submit_request::Submission::Dag(pb::DagSpec {
            allocations: vec![
                pb::AllocationSpec {
                    id: "preprocess".to_string(),
                    tenant: "ml".to_string(),
                    entrypoint: "preprocess.py".to_string(),
                    ..Default::default()
                },
                pb::AllocationSpec {
                    id: "train".to_string(),
                    tenant: "ml".to_string(),
                    entrypoint: "train.py".to_string(),
                    depends_on: vec![pb::DependencySpec {
                        ref_id: "preprocess".to_string(),
                        condition: "success".to_string(),
                    }],
                    ..Default::default()
                },
                pb::AllocationSpec {
                    id: "postprocess".to_string(),
                    tenant: "ml".to_string(),
                    entrypoint: "postprocess.py".to_string(),
                    depends_on: vec![pb::DependencySpec {
                        ref_id: "train".to_string(),
                        condition: "success".to_string(),
                    }],
                    ..Default::default()
                },
            ],
        })),
    };

    let dag_resp = svc.submit(Request::new(dag_req)).await.unwrap();
    let dag_id = &dag_resp.get_ref().dag_id;
    assert!(!dag_id.is_empty(), "DAG should have an ID");
    assert_eq!(
        dag_resp.get_ref().allocation_ids.len(),
        3,
        "DAG should have 3 allocations"
    );

    // Verify the DAG status shows all allocations
    let dag_status = svc
        .get_dag(Request::new(pb::GetDagRequest {
            dag_id: dag_id.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(dag_status.get_ref().allocations.len(), 3);
    assert_eq!(dag_status.get_ref().state, "pending");

    // Cancel the entire DAG
    let cancel_resp = svc
        .cancel_dag(Request::new(pb::CancelDagRequest {
            dag_id: dag_id.clone(),
        }))
        .await
        .unwrap();
    assert!(cancel_resp.get_ref().success);
    assert_eq!(cancel_resp.get_ref().allocations_cancelled, 3);

    // Verify all allocations in the DAG are cancelled
    for alloc_id in &dag_resp.get_ref().allocation_ids {
        let status = svc
            .get(Request::new(pb::GetAllocationRequest {
                allocation_id: alloc_id.clone(),
            }))
            .await
            .unwrap();
        assert_eq!(
            status.get_ref().state,
            "cancelled",
            "allocation {alloc_id} should be cancelled"
        );
    }
}

// ─── Test 8: Admin service + Allocation service cross-wiring ─
// Create a tenant via AdminService (in-memory fallback), then submit
// allocations via AllocationService, verifying both services share state.

#[tokio::test]
async fn admin_and_allocation_services_share_api_state() {
    let state = test_state();
    let admin_svc = LatticeAdminService::new(state.clone());
    let alloc_svc = LatticeAllocationService::new(state.clone());

    // Create a tenant through the admin service
    let tenant_resp = admin_svc
        .create_tenant(Request::new(pb::CreateTenantRequest {
            name: "oncology".to_string(),
            quota: Some(pb::TenantQuotaSpec {
                max_nodes: 20,
                fair_share_target: 0.25,
                gpu_hours_budget: Some(500.0),
                max_concurrent_allocations: Some(5),
            }),
            isolation_level: "strict".to_string(),
        }))
        .await
        .unwrap();
    assert_eq!(tenant_resp.get_ref().name, "oncology");
    assert_eq!(
        tenant_resp.get_ref().isolation_level,
        "strict",
        "medical tenant should have strict isolation"
    );

    // Create a vCluster for this tenant
    let vc_resp = admin_svc
        .create_v_cluster(Request::new(pb::CreateVClusterRequest {
            tenant_id: "oncology".to_string(),
            name: "medical-reserved".to_string(),
            scheduler_type: "medical_reservation".to_string(),
            cost_weights: None,
            dedicated_nodes: vec![],
            allow_borrowing: false,
            allow_lending: false,
        }))
        .await
        .unwrap();
    assert_eq!(vc_resp.get_ref().name, "medical-reserved");
    assert_eq!(vc_resp.get_ref().scheduler_type, "medical_reservation");

    // Submit allocations for this tenant through the allocation service
    let submit_resp = alloc_svc
        .submit(Request::new(pb::SubmitRequest {
            submission: Some(pb::submit_request::Submission::Single(pb::AllocationSpec {
                tenant: "oncology".to_string(),
                project: "tumor-detection".to_string(),
                entrypoint: "inference.py".to_string(),
                ..Default::default()
            })),
        }))
        .await
        .unwrap();
    assert_eq!(submit_resp.get_ref().allocation_ids.len(), 1);

    // List allocations for this tenant confirms the allocation is there
    let list_resp = alloc_svc
        .list(Request::new(pb::ListAllocationsRequest {
            tenant: "oncology".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(list_resp.get_ref().allocations.len(), 1);

    // Checkpoint the allocation (exercises the checkpoint broker mock)
    let alloc_id = &submit_resp.get_ref().allocation_ids[0];
    let ckpt_resp = alloc_svc
        .checkpoint(Request::new(pb::CheckpointRequest {
            allocation_id: alloc_id.clone(),
        }))
        .await
        .unwrap();
    assert!(
        ckpt_resp.get_ref().accepted,
        "checkpoint should be accepted by mock broker"
    );
}

// ─── Test 9: Event bus + allocation store consistency ────────
// Submit allocations, publish events through the shared event bus,
// and verify that the event bus and allocation store remain consistent.

#[tokio::test]
async fn event_bus_and_allocation_store_consistency() {
    let state = test_state();
    let svc = LatticeAllocationService::new(state.clone());

    // Submit an allocation
    let resp = svc
        .submit(Request::new(single_submit_request("physics")))
        .await
        .unwrap();
    let alloc_id_str = &resp.get_ref().allocation_ids[0];
    let alloc_id = Uuid::parse_str(alloc_id_str).unwrap();

    // Subscribe to events for this allocation
    let mut rx = state.events.subscribe(alloc_id).await;

    // Simulate state change events that would come from the scheduling loop
    let events = vec![
        AllocationEvent::StateChange {
            allocation_id: alloc_id,
            old_state: "pending".to_string(),
            new_state: "staging".to_string(),
        },
        AllocationEvent::StateChange {
            allocation_id: alloc_id,
            old_state: "staging".to_string(),
            new_state: "running".to_string(),
        },
        AllocationEvent::LogLine {
            allocation_id: alloc_id,
            line: "Training started".to_string(),
            stream: LogStream::Stdout,
        },
    ];

    for event in &events {
        state.events.publish(event.clone()).await;
    }

    // Verify all events arrive in order
    for expected in &events {
        let received = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("should not timeout")
            .expect("should receive event");
        assert_eq!(&received, expected);
    }

    // The allocation store still has the allocation (events don't modify store)
    let stored = svc
        .get(Request::new(pb::GetAllocationRequest {
            allocation_id: alloc_id_str.clone(),
        }))
        .await
        .unwrap();
    // State in store is still "pending" because events don't drive store updates
    // in this test (that would be the scheduling loop's job)
    assert_eq!(stored.get_ref().state, "pending");
}

// ─── Test 10: Diagnostics collector with TSDB error fallback ─
// Verify that TSDB errors during diagnostics collection degrade
// gracefully: node/network/storage data is still collected,
// only metrics are absent.

#[tokio::test]
async fn diagnostics_graceful_degradation_on_tsdb_failure() {
    let tsdb = Arc::new(MockTsdb::with_error());
    let alloc_id = Uuid::new_v4();

    let collector = DiagnosticsCollector::new(tsdb);

    let node_states = vec![NodeDiagState {
        node_id: "x1000c0s0b0n0".to_string(),
        agent_connected: true,
        phase: "Running".to_string(),
        last_heartbeat_age_secs: 2,
    }];

    let result = collector
        .collect(
            alloc_id,
            node_states,
            NetworkDiagnostics {
                connectivity_ok: true,
                vni: Some(3001),
                avg_latency_us: 0.5,
                avg_bandwidth_gbps: 400.0,
            },
            StorageDiagnostics {
                mounts_healthy: true,
                io_throughput_gbps: 20.0,
                cache_hit_rate: 0.99,
            },
            vec!["checkpoint saved".to_string()],
            &["gpu_utilization", "memory_used_bytes", "io_throughput"],
        )
        .await
        .unwrap();

    // Node, network, storage, and logs should all be present
    assert_eq!(result.node_states.len(), 1);
    assert!(result.is_healthy());
    assert_eq!(result.network_health.vni, Some(3001));
    assert!(result.storage_health.mounts_healthy);
    assert_eq!(result.log_tail, vec!["checkpoint saved"]);

    // Metrics should be empty due to TSDB errors (graceful degradation)
    assert!(
        result.recent_metrics.is_empty(),
        "metrics should be absent when TSDB fails, got: {:?}",
        result.recent_metrics
    );
}

// ─── Test 11: Rate limiter with default config ───────────────
// Verify that the default RateLimitConfig provides reasonable
// behavior for multiple users.

#[tokio::test]
async fn rate_limiter_default_config_behavior() {
    let limiter = RateLimiter::new(RateLimitConfig::default());
    // Default: 60 req/min + 10 burst = 70 tokens

    // A new user should be able to make many requests
    for _ in 0..70 {
        assert!(limiter.check("high-volume-user").is_ok());
    }

    // 71st request should fail
    let err = limiter.check("high-volume-user").unwrap_err();
    assert!(err.retry_after_secs > 0);

    // A different user should still have full capacity
    for _ in 0..70 {
        assert!(limiter.check("another-user").is_ok());
    }
}

// ─── Test 12: OIDC validator with different configs ──────────
// Test that different OidcConfig instances produce different
// claim values, verifying config propagation through the validator.

#[tokio::test]
async fn oidc_validator_config_propagation_across_instances() {
    let config_a = OidcConfig {
        issuer_url: "https://idp-a.example.com".to_string(),
        audience: "service-a".to_string(),
        required_scopes: vec!["read".to_string()],
    };
    let config_b = OidcConfig {
        issuer_url: "https://idp-b.example.com".to_string(),
        audience: "service-b".to_string(),
        required_scopes: vec!["write".to_string(), "admin".to_string()],
    };

    let validator_a = StubOidcValidator::new(config_a);
    let validator_b = StubOidcValidator::new(config_b);

    // Same token, different validators -> different claims
    let claims_a = validator_a.validate_token("valid-user").await.unwrap();
    let claims_b = validator_b.validate_token("valid-user").await.unwrap();

    assert_eq!(claims_a.sub, "user");
    assert_eq!(claims_b.sub, "user");
    assert_ne!(claims_a.iss, claims_b.iss);
    assert_ne!(claims_a.aud, claims_b.aud);
    assert_ne!(claims_a.scopes, claims_b.scopes);

    assert_eq!(claims_a.iss, "https://idp-a.example.com");
    assert_eq!(claims_b.iss, "https://idp-b.example.com");
    assert_eq!(claims_a.scopes, vec!["read"]);
    assert_eq!(claims_b.scopes, vec!["write", "admin"]);
}
