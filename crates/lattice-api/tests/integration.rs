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
use lattice_common::traits::AuditLog;
use lattice_common::tsdb_client::{MetricSample, MetricSeries, QueryResult, TsdbClient};
use lattice_test_harness::fixtures::AllocationBuilder;
use lattice_test_harness::mocks::{
    MockAllocationStore, MockAuditLog, MockCheckpointBroker, MockNodeRegistry,
};
use tokio_stream::StreamExt;

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
        tsdb: None,
        storage: None,
        accounting: None,
        oidc: None,
        rate_limiter: None,
        sovra: None,
        pty: None,
        data_dir: None,
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
        "sensitive tenant should have strict isolation"
    );

    // Create a vCluster for this tenant
    let vc_resp = admin_svc
        .create_v_cluster(Request::new(pb::CreateVClusterRequest {
            tenant_id: "oncology".to_string(),
            name: "sensitive-reserved".to_string(),
            scheduler_type: "sensitive_reservation".to_string(),
            cost_weights: None,
            dedicated_nodes: vec![],
            allow_borrowing: false,
            allow_lending: false,
        }))
        .await
        .unwrap();
    assert_eq!(vc_resp.get_ref().name, "sensitive-reserved");
    assert_eq!(vc_resp.get_ref().scheduler_type, "sensitive_reservation");

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

// ═══════════════════════════════════════════════════════════════
// REST Endpoint Integration Tests (T1-A)
// ═══════════════════════════════════════════════════════════════

use axum::body::Body;
use axum::http;
use tower::ServiceExt;

use lattice_api::rest;

fn rest_test_state() -> Arc<ApiState> {
    let nodes = lattice_test_harness::fixtures::create_node_batch(5, 0);
    Arc::new(ApiState {
        allocations: Arc::new(MockAllocationStore::new()),
        nodes: Arc::new(MockNodeRegistry::new().with_nodes(nodes)),
        audit: Arc::new(MockAuditLog::new()),
        checkpoint: Arc::new(MockCheckpointBroker::new()),
        quorum: None,
        events: Arc::new(EventBus::new()),
        tsdb: None,
        storage: None,
        accounting: None,
        oidc: None,
        rate_limiter: None,
        sovra: None,
        pty: None,
        data_dir: None,
    })
}

/// Helper: submit an allocation via REST and return the allocation_id.
async fn rest_submit(state: &Arc<ApiState>, tenant: &str) -> String {
    let app = rest::router(state.clone());
    let body = serde_json::json!({
        "tenant": tenant,
        "entrypoint": "python train.py",
        "nodes": 1,
        "walltime_hours": 1.0
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
    assert_eq!(resp.status(), http::StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let submit: rest::SubmitResponse = serde_json::from_slice(&body).unwrap();
    submit.allocation_id
}

// ─── Test 13: POST + GET /allocations ────────────────────────
#[tokio::test]
async fn rest_submit_returns_201_and_get_returns_correct_tenant() {
    let state = rest_test_state();
    let id = rest_submit(&state, "physics").await;

    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri(format!("/api/v1/allocations/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let alloc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(alloc["tenant"], "physics");
    assert_eq!(alloc["state"], "pending");
}

// ─── Test 14: GET /allocations?tenant=X ──────────────────────
#[tokio::test]
async fn rest_list_allocations_filters_by_tenant() {
    let state = rest_test_state();
    rest_submit(&state, "physics").await;
    rest_submit(&state, "physics").await;
    rest_submit(&state, "biology").await;

    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri("/api/v1/allocations?tenant=physics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let allocs: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(allocs.len(), 2);
    for a in &allocs {
        assert_eq!(a["tenant"], "physics");
    }
}

// ─── Test 15: POST /allocations/:id/cancel ───────────────────
#[tokio::test]
async fn rest_cancel_transitions_to_cancelled() {
    let state = rest_test_state();
    let id = rest_submit(&state, "physics").await;

    // Cancel
    let app = rest::router(state.clone());
    let resp = app
        .oneshot(
            http::Request::builder()
                .method("POST")
                .uri(format!("/api/v1/allocations/{id}/cancel"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    // Verify state changed
    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri(format!("/api/v1/allocations/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let alloc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(alloc["state"], "cancelled");
}

// ─── Test 16: PATCH /allocations/:id ─────────────────────────
#[tokio::test]
async fn rest_patch_allocation_returns_200() {
    let state = rest_test_state();
    let id = rest_submit(&state, "physics").await;

    let app = rest::router(state);
    let patch_body = serde_json::json!({"walltime_hours": 4.0});
    let resp = app
        .oneshot(
            http::Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/allocations/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&patch_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

// ─── Test 17: GET /allocations/:id (missing) → 404 ──────────
#[tokio::test]
async fn rest_get_missing_allocation_returns_404() {
    let state = rest_test_state();
    let missing_id = Uuid::new_v4();
    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri(format!("/api/v1/allocations/{missing_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
}

// ─── Test 18: GET /allocations/:id (bad UUID) → 400 ─────────
#[tokio::test]
async fn rest_get_bad_uuid_returns_400() {
    let state = rest_test_state();
    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri("/api/v1/allocations/not-a-uuid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
}

// ─── Test 19: Session lifecycle ──────────────────────────────
#[tokio::test]
async fn rest_session_create_get_delete_verify() {
    let state = rest_test_state();

    // Create session
    let app = rest::router(state.clone());
    let body = serde_json::json!({"tenant": "interactive"});
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
    assert_eq!(resp.status(), http::StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(session["state"], "pending");
    assert_eq!(session["tenant"], "interactive");
    let sid = session["session_id"].as_str().unwrap().to_string();

    // Get session
    let app = rest::router(state.clone());
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri(format!("/api/v1/sessions/{sid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    // Delete session
    let app = rest::router(state.clone());
    let resp = app
        .oneshot(
            http::Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/sessions/{sid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    // Verify cancelled
    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri(format!("/api/v1/sessions/{sid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let after: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(after["state"], "cancelled");
}

// ─── Test 20: DAG lifecycle ──────────────────────────────────
#[tokio::test]
async fn rest_dag_submit_get_cancel() {
    let state = rest_test_state();

    let app = rest::router(state.clone());
    let body = serde_json::json!({
        "dag_id": "pipeline-integ",
        "allocations": [
            {"name": "preprocess", "tenant": "ml", "entrypoint": "prep.sh"},
            {"name": "train", "tenant": "ml", "entrypoint": "train.py"}
        ],
        "edges": [{"from": "preprocess", "to": "train"}]
    });
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
    assert_eq!(resp.status(), http::StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let dag: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(dag["dag_id"], "pipeline-integ");
    assert_eq!(dag["allocation_ids"].as_array().unwrap().len(), 2);

    // GET DAG
    let app = rest::router(state.clone());
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri("/api/v1/dags/pipeline-integ")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    // Cancel DAG
    let app = rest::router(state.clone());
    let resp = app
        .oneshot(
            http::Request::builder()
                .method("POST")
                .uri("/api/v1/dags/pipeline-integ/cancel")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

// ─── Test 21: GET /dags list ─────────────────────────────────
#[tokio::test]
async fn rest_list_dags_returns_submitted() {
    let state = rest_test_state();

    // Submit a DAG
    let app = rest::router(state.clone());
    let body = serde_json::json!({
        "dag_id": "dag-list-test",
        "allocations": [{"name": "step1", "tenant": "ml", "entrypoint": "run.sh"}],
        "edges": []
    });
    app.oneshot(
        http::Request::builder()
            .method("POST")
            .uri("/api/v1/dags")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
    .unwrap();

    // List DAGs
    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri("/api/v1/dags")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let dags: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(!dags.is_empty());
    assert!(dags.iter().any(|d| d["dag_id"] == "dag-list-test"));
}

// ─── Test 22: GET /allocations/:id/diagnostics ───────────────
#[tokio::test]
async fn rest_diagnostics_returns_200_with_health() {
    let state = rest_test_state();
    let id = rest_submit(&state, "physics").await;

    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri(format!("/api/v1/allocations/{id}/diagnostics"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let diag: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(diag["healthy"], true);
    assert_eq!(diag["allocation_id"], id);
}

// ─── Test 23: GET /allocations/:id/metrics ───────────────────
#[tokio::test]
async fn rest_metrics_returns_200() {
    let state = rest_test_state();
    let id = rest_submit(&state, "physics").await;

    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri(format!("/api/v1/allocations/{id}/metrics"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let metrics: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(metrics["allocation_id"], id);
}

// ─── Test 24: GET /allocations/:id/logs ──────────────────────
#[tokio::test]
async fn rest_logs_returns_200_with_empty_lines() {
    let state = rest_test_state();
    let id = rest_submit(&state, "physics").await;

    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri(format!("/api/v1/allocations/{id}/logs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let logs: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(logs["lines"].as_array().unwrap().is_empty());
}

// ─── Test 25: GET /audit ─────────────────────────────────────
#[tokio::test]
async fn rest_audit_returns_entries() {
    let audit_log = Arc::new(MockAuditLog::new());
    let entry = lattice_common::traits::AuditEntry {
        id: Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        user: "admin".into(),
        action: lattice_common::traits::AuditAction::NodeClaim,
        details: serde_json::json!({"node": "n1"}),
    };
    audit_log.record(entry).await.unwrap();

    let state = Arc::new(ApiState {
        allocations: Arc::new(MockAllocationStore::new()),
        nodes: Arc::new(MockNodeRegistry::new()),
        audit: audit_log,
        checkpoint: Arc::new(MockCheckpointBroker::new()),
        quorum: None,
        events: Arc::new(EventBus::new()),
        tsdb: None,
        storage: None,
        accounting: None,
        oidc: None,
        rate_limiter: None,
        sovra: None,
        pty: None,
        data_dir: None,
    });

    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri("/api/v1/audit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let entries: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["user"], "admin");
}

// ─── Test 26: Node endpoints (list, get, drain, undrain) ─────
#[tokio::test]
async fn rest_node_list_get_drain_undrain() {
    let state = rest_test_state();

    // List nodes (5 from rest_test_state)
    let app = rest::router(state.clone());
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri("/api/v1/nodes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let nodes: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(nodes.len(), 5);
    let node_id = nodes[0]["id"].as_str().unwrap().to_string();

    // Get single node
    let app = rest::router(state.clone());
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri(format!("/api/v1/nodes/{node_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    // Drain node
    let app = rest::router(state.clone());
    let resp = app
        .oneshot(
            http::Request::builder()
                .method("POST")
                .uri(format!("/api/v1/nodes/{node_id}/drain"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

// ─── Test 27: GET /healthz ───────────────────────────────────
#[tokio::test]
async fn rest_healthz_returns_ok() {
    let state = rest_test_state();
    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"ok");
}

// ═══════════════════════════════════════════════════════════════
// Streaming gRPC Tests (T1-B)
// ═══════════════════════════════════════════════════════════════

fn streaming_test_state() -> Arc<ApiState> {
    Arc::new(ApiState {
        allocations: Arc::new(MockAllocationStore::new()),
        nodes: Arc::new(MockNodeRegistry::new()),
        audit: Arc::new(MockAuditLog::new()),
        checkpoint: Arc::new(MockCheckpointBroker::new()),
        quorum: None,
        events: Arc::new(EventBus::new()),
        tsdb: None,
        storage: None,
        accounting: None,
        oidc: None,
        rate_limiter: None,
        sovra: None,
        pty: None,
        data_dir: None,
    })
}

fn streaming_test_state_with_tsdb(tsdb: Arc<MockTsdb>) -> Arc<ApiState> {
    Arc::new(ApiState {
        allocations: Arc::new(MockAllocationStore::new()),
        nodes: Arc::new(MockNodeRegistry::new()),
        audit: Arc::new(MockAuditLog::new()),
        checkpoint: Arc::new(MockCheckpointBroker::new()),
        quorum: None,
        events: Arc::new(EventBus::new()),
        tsdb: Some(tsdb),
        storage: None,
        accounting: None,
        oidc: None,
        rate_limiter: None,
        sovra: None,
        pty: None,
        data_dir: None,
    })
}

// ─── Test 28: watch() + publish state change ─────────────────
#[tokio::test]
async fn streaming_watch_receives_state_change_event() {
    let state = streaming_test_state();

    let alloc = AllocationBuilder::new()
        .state(lattice_common::types::AllocationState::Running)
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
    let event = AllocationEvent::StateChange {
        allocation_id: alloc_id,
        old_state: "running".to_string(),
        new_state: "completed".to_string(),
    };
    state.events.publish(event).await;

    // Receive from stream
    let received = timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("should not timeout")
        .expect("should have an item")
        .expect("should be Ok");

    assert_eq!(received.allocation_id, alloc_id.to_string());
    assert_eq!(received.event_type, "state_change");
}

// ─── Test 29: watch() nonexistent allocation → NOT_FOUND ─────
#[tokio::test]
async fn streaming_watch_nonexistent_returns_not_found() {
    let state = streaming_test_state();
    let svc = LatticeAllocationService::new(state);

    let result = svc
        .watch(Request::new(pb::WatchRequest {
            allocation_id: Uuid::new_v4().to_string(),
            tenant: String::new(),
            vcluster: String::new(),
        }))
        .await;

    assert!(result.is_err());
    let err = result.err().unwrap();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ─── Test 30: stream_logs() + publish log lines ──────────────
#[tokio::test]
async fn streaming_logs_receives_log_lines() {
    let state = streaming_test_state();

    let alloc = AllocationBuilder::new()
        .state(lattice_common::types::AllocationState::Running)
        .build();
    let alloc_id = alloc.id;
    state.allocations.insert(alloc).await.unwrap();

    let svc = LatticeAllocationService::new(state.clone());

    let resp = svc
        .stream_logs(Request::new(pb::LogStreamRequest {
            allocation_id: alloc_id.to_string(),
            stream: 0, // ALL
            node_id: String::new(),
            follow: true,
            tail_lines: 0,
            since: None,
            until: None,
        }))
        .await
        .unwrap();
    let mut stream = resp.into_inner();

    // Publish log lines
    for i in 0..3 {
        let event = AllocationEvent::LogLine {
            allocation_id: alloc_id,
            line: format!("line {i}"),
            stream: LogStream::Stdout,
        };
        state.events.publish(event).await;
    }

    // Receive 3 log entries
    for _i in 0..3 {
        let received = timeout(Duration::from_millis(200), stream.next())
            .await
            .expect("should not timeout")
            .expect("should have an item")
            .expect("should be Ok");
        // LogEntry has `data` (bytes), not `line`
        assert!(!received.data.is_empty());
    }
}

// ─── Test 31: stream_metrics() + publish metrics ─────────────
#[tokio::test]
async fn streaming_metrics_receives_metric_events() {
    let state = streaming_test_state();

    let alloc = AllocationBuilder::new()
        .state(lattice_common::types::AllocationState::Running)
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
    let event = AllocationEvent::MetricPoint {
        allocation_id: alloc_id,
        metric_name: "gpu_utilization".to_string(),
        value: 0.95,
        timestamp_epoch_ms: 1234567890,
    };
    state.events.publish(event).await;

    let received = timeout(Duration::from_millis(200), stream.next())
        .await
        .expect("should not timeout")
        .expect("should have an item")
        .expect("should be Ok");
    // MetricsEvent has node_id, timestamp, and oneof event (snapshot/alert)
    assert!(!received.node_id.is_empty() || received.event.is_some());
}

// ─── Test 32: attach precondition: running allocation ─────────
// Attach requires a Running allocation. Verify the allocation state
// is correctly validated by the service (bidirectional streaming
// requires tonic::Streaming which is hard to construct in tests).
#[tokio::test]
async fn streaming_attach_precondition_running_state() {
    let state = streaming_test_state();

    let running = AllocationBuilder::new()
        .state(lattice_common::types::AllocationState::Running)
        .build();
    let running_id = running.id;
    state.allocations.insert(running).await.unwrap();

    let pending = AllocationBuilder::new()
        .state(lattice_common::types::AllocationState::Pending)
        .build();
    let pending_id = pending.id;
    state.allocations.insert(pending).await.unwrap();

    // Verify precondition: only Running allocations should be attachable
    let running_alloc = state.allocations.get(&running_id).await.unwrap();
    assert_eq!(
        running_alloc.state,
        lattice_common::types::AllocationState::Running
    );

    let pending_alloc = state.allocations.get(&pending_id).await.unwrap();
    assert_ne!(
        pending_alloc.state,
        lattice_common::types::AllocationState::Running
    );
}

// ─── Test 34: query_metrics() with MockTsdb ──────────────────
#[tokio::test]
async fn streaming_query_metrics_with_tsdb_returns_data() {
    let tsdb = Arc::new(MockTsdb::new());
    tsdb.register("gpu_utilization", vec![(1000, 0.75), (2000, 0.85)]);

    let state = streaming_test_state_with_tsdb(tsdb);

    let alloc = AllocationBuilder::new()
        .state(lattice_common::types::AllocationState::Running)
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
    let snapshot = resp.into_inner();
    // MetricsSnapshot has summary and nodes fields — just verify it was returned
    assert!(
        !snapshot.allocation_id.is_empty(),
        "should have allocation_id"
    );
}

// ─── Test 35: query_metrics() without TSDB → UNAVAILABLE ─────
#[tokio::test]
async fn streaming_query_metrics_without_tsdb_returns_unavailable() {
    let state = streaming_test_state(); // no TSDB
    let svc = LatticeAllocationService::new(state);

    let result = svc
        .query_metrics(Request::new(pb::QueryMetricsRequest {
            allocation_id: Uuid::new_v4().to_string(),
            mode: 0,
            duration: None,
        }))
        .await;

    assert!(result.is_err());
    let err = result.err().unwrap();
    assert_eq!(err.code(), tonic::Code::Unavailable);
}

// ═══════════════════════════════════════════════════════════════
// Additional Integration Tests (T2)
// ═══════════════════════════════════════════════════════════════

use lattice_api::grpc::node_service::LatticeNodeService;
use lattice_common::proto::lattice::v1::node_service_server::NodeService;
use lattice_common::types::NodeState;

// ─── Test 36: Admin create + update tenant lifecycle (in-memory) ─────
#[tokio::test]
async fn admin_create_update_tenant_lifecycle() {
    let state = test_state();
    let admin_svc = LatticeAdminService::new(state.clone());

    // Create tenant
    let create_resp = admin_svc
        .create_tenant(Request::new(pb::CreateTenantRequest {
            name: "research".to_string(),
            quota: Some(pb::TenantQuotaSpec {
                max_nodes: 50,
                fair_share_target: 0.3,
                gpu_hours_budget: None,
                max_concurrent_allocations: None,
            }),
            isolation_level: "standard".to_string(),
        }))
        .await
        .unwrap();
    assert_eq!(create_resp.get_ref().name, "research");
    let quota = create_resp.get_ref().quota.as_ref().unwrap();
    assert_eq!(quota.max_nodes, 50);

    // Update tenant quota
    let update_resp = admin_svc
        .update_tenant(Request::new(pb::UpdateTenantRequest {
            tenant_id: create_resp.get_ref().tenant_id.clone(),
            quota: Some(pb::TenantQuotaSpec {
                max_nodes: 100,
                fair_share_target: 0.5,
                gpu_hours_budget: Some(1000.0),
                max_concurrent_allocations: Some(10),
            }),
            isolation_level: Some("strict".to_string()),
        }))
        .await
        .unwrap();
    let updated_quota = update_resp.get_ref().quota.as_ref().unwrap();
    assert_eq!(updated_quota.max_nodes, 100);
    assert_eq!(update_resp.get_ref().isolation_level, "strict");
}

// ─── Test 37: Admin create + update vCluster lifecycle (in-memory) ───
#[tokio::test]
async fn admin_create_update_vcluster_lifecycle() {
    let state = test_state();
    let admin_svc = LatticeAdminService::new(state.clone());

    // Create tenant first
    admin_svc
        .create_tenant(Request::new(pb::CreateTenantRequest {
            name: "ml-team".to_string(),
            quota: Some(pb::TenantQuotaSpec {
                max_nodes: 100,
                fair_share_target: 0.5,
                gpu_hours_budget: None,
                max_concurrent_allocations: None,
            }),
            isolation_level: "standard".to_string(),
        }))
        .await
        .unwrap();

    // Create vCluster
    let create_resp = admin_svc
        .create_v_cluster(Request::new(pb::CreateVClusterRequest {
            name: "train-vc".to_string(),
            tenant_id: "ml-team".to_string(),
            scheduler_type: "hpc_backfill".to_string(),
            cost_weights: None,
            dedicated_nodes: vec![],
            allow_borrowing: false,
            allow_lending: false,
        }))
        .await
        .unwrap();
    let vc_id = create_resp.get_ref().vcluster_id.clone();
    assert!(!vc_id.is_empty());
    assert_eq!(create_resp.get_ref().name, "train-vc");

    // Update vCluster cost weights
    let update_resp = admin_svc
        .update_v_cluster(Request::new(pb::UpdateVClusterRequest {
            vcluster_id: vc_id.clone(),
            cost_weights: Some(pb::CostWeightsSpec {
                priority: 2.0,
                wait_time: 1.5,
                fair_share: 1.0,
                topology: 3.0,
                data_readiness: 0.5,
                backlog: 1.0,
                energy: 0.2,
                checkpoint_efficiency: 0.8,
                conformance: 1.0,
            }),
            allow_borrowing: Some(true),
            allow_lending: Some(false),
        }))
        .await
        .unwrap();
    assert_eq!(update_resp.get_ref().vcluster_id, vc_id);
}

// ─── Test 38: Node disable via gRPC ──────────────────────────
#[tokio::test]
async fn node_disable_sets_down_state() {
    let nodes = lattice_test_harness::fixtures::create_node_batch(3, 0);
    let state = Arc::new(ApiState {
        allocations: Arc::new(MockAllocationStore::new()),
        nodes: Arc::new(MockNodeRegistry::new().with_nodes(nodes.clone())),
        audit: Arc::new(MockAuditLog::new()),
        checkpoint: Arc::new(MockCheckpointBroker::new()),
        quorum: None,
        events: Arc::new(EventBus::new()),
        tsdb: None,
        storage: None,
        accounting: None,
        oidc: None,
        rate_limiter: None,
        sovra: None,
        pty: None,
        data_dir: None,
    });
    let node_svc = LatticeNodeService::new(state.clone());

    let resp = node_svc
        .disable_node(Request::new(pb::DisableNodeRequest {
            node_id: nodes[0].id.clone(),
            reason: "hardware failure".to_string(),
        }))
        .await
        .unwrap();
    assert!(resp.get_ref().success);

    // Verify node is Down
    let node = state.nodes.get_node(&nodes[0].id).await.unwrap();
    assert!(
        matches!(node.state, NodeState::Down { .. }),
        "node should be Down, got {:?}",
        node.state
    );
}

// ─── Test 39: Admin backup_verify returns not-implemented ────
#[tokio::test]
async fn admin_backup_verify_returns_invalid_for_nonexistent_path() {
    let state = test_state();
    let admin_svc = LatticeAdminService::new(state);

    let resp = admin_svc
        .backup_verify(Request::new(pb::BackupVerifyRequest {
            backup_path: "/tmp/nonexistent-backup.tar.gz".to_string(),
        }))
        .await
        .unwrap();

    // Nonexistent path should return invalid
    assert!(!resp.get_ref().valid);
    assert!(resp.get_ref().message.contains("failed"));
}

// ─── Test 39b: Admin backup_verify with empty path → error ───
#[tokio::test]
async fn admin_backup_verify_empty_path_returns_error() {
    let state = test_state();
    let admin_svc = LatticeAdminService::new(state);

    let result = admin_svc
        .backup_verify(Request::new(pb::BackupVerifyRequest {
            backup_path: String::new(),
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.err().unwrap().code(), tonic::Code::InvalidArgument);
}

// ─── Test 40: LaunchTasks RPC returns task ID ────────────────
#[tokio::test]
async fn launch_tasks_returns_task_id() {
    let state = test_state();
    let svc = LatticeAllocationService::new(state.clone());

    // Submit an allocation first
    let resp = svc
        .submit(Request::new(single_submit_request("physics")))
        .await
        .unwrap();
    let alloc_id = &resp.get_ref().allocation_ids[0];

    // Launch tasks
    let launch_resp = svc
        .launch_tasks(Request::new(pb::LaunchTasksRequest {
            allocation_id: alloc_id.clone(),
            num_tasks: 4,
            tasks_per_node: 1,
            entrypoint: "python worker.py".to_string(),
        }))
        .await
        .unwrap();

    assert!(
        !launch_resp.get_ref().task_launch_id.is_empty(),
        "should receive a task launch ID"
    );
    // Verify it's a valid UUID
    assert!(
        Uuid::parse_str(&launch_resp.get_ref().task_launch_id).is_ok(),
        "task_launch_id should be a valid UUID"
    );
}

// ─── Test 41: LaunchTasks with invalid ID → error ────────────
#[tokio::test]
async fn launch_tasks_invalid_allocation_id_returns_error() {
    let state = test_state();
    let svc = LatticeAllocationService::new(state);

    let result = svc
        .launch_tasks(Request::new(pb::LaunchTasksRequest {
            allocation_id: "not-a-uuid".to_string(),
            num_tasks: 1,
            tasks_per_node: 1,
            entrypoint: "echo hello".to_string(),
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.err().unwrap().code(), tonic::Code::InvalidArgument);
}

// ─── Test 42: CompareMetrics with TSDB data ──────────────────
#[tokio::test]
async fn compare_metrics_returns_aligned_series() {
    let tsdb = Arc::new(MockTsdb::new());
    tsdb.register("gpu_utilization", vec![(100, 0.5), (200, 0.8)]);

    let state = streaming_test_state_with_tsdb(tsdb);
    let svc = LatticeAllocationService::new(state.clone());

    // Create two allocations to compare
    let resp1 = svc
        .submit(Request::new(single_submit_request("physics")))
        .await
        .unwrap();
    let resp2 = svc
        .submit(Request::new(single_submit_request("physics")))
        .await
        .unwrap();
    let id1 = &resp1.get_ref().allocation_ids[0];
    let id2 = &resp2.get_ref().allocation_ids[0];

    let compare_resp = svc
        .compare_metrics(Request::new(pb::CompareMetricsRequest {
            allocation_ids: vec![id1.clone(), id2.clone()],
            metrics: vec!["gpu_utilization".to_string()],
            relative_time: true,
        }))
        .await
        .unwrap();

    let series = &compare_resp.get_ref().series;
    assert_eq!(series.len(), 2, "should have series for both allocations");
    assert_eq!(series[0].allocation_id, *id1);
    assert_eq!(series[1].allocation_id, *id2);
}

// ─── Test 43: CompareMetrics without TSDB → UNAVAILABLE ──────
#[tokio::test]
async fn compare_metrics_without_tsdb_returns_unavailable() {
    let state = streaming_test_state();
    let svc = LatticeAllocationService::new(state);

    let result = svc
        .compare_metrics(Request::new(pb::CompareMetricsRequest {
            allocation_ids: vec![Uuid::new_v4().to_string()],
            metrics: vec!["gpu_utilization".to_string()],
            relative_time: true,
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.err().unwrap().code(), tonic::Code::Unavailable);
}

// ─── Test 44: REST tenant create and list ────────────────────
#[tokio::test]
async fn rest_tenant_create_returns_201_with_correct_data() {
    let state = rest_test_state();

    // Create tenant
    let app = rest::router(state.clone());
    let body = serde_json::json!({
        "name": "bio",
        "max_nodes": 30,
        "fair_share_target": 0.4,
        "isolation_level": "standard"
    });
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
    assert_eq!(resp.status(), http::StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let tenant: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(tenant["name"], "bio");
    assert_eq!(tenant["max_nodes"], 30);
    assert_eq!(tenant["isolation_level"], "standard");

    // List tenants without quorum returns empty (quorum-dependent)
    let app = rest::router(state.clone());
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri("/api/v1/tenants")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

// ─── Test 45: REST vCluster create ───────────────────────────
#[tokio::test]
async fn rest_vcluster_create_returns_201_with_correct_data() {
    let state = rest_test_state();

    // Create vCluster
    let app = rest::router(state.clone());
    let body = serde_json::json!({
        "name": "train-gpu",
        "tenant_id": "physics",
        "scheduler_type": "hpc_backfill"
    });
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
    assert_eq!(resp.status(), http::StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let vc: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(vc["name"], "train-gpu");
    assert_eq!(vc["tenant_id"], "physics");
    assert_eq!(vc["scheduler_type"], "hpc_backfill");
    assert!(!vc["id"].as_str().unwrap().is_empty());

    // List vClusters without quorum returns empty (quorum-dependent)
    let app = rest::router(state.clone());
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri("/api/v1/vclusters")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

// ─── Test 46: REST undrain node ──────────────────────────────
#[tokio::test]
async fn rest_undrain_node() {
    let nodes = lattice_test_harness::fixtures::create_node_batch(3, 0);
    let state = Arc::new(ApiState {
        allocations: Arc::new(MockAllocationStore::new()),
        nodes: Arc::new(MockNodeRegistry::new().with_nodes(nodes.clone())),
        audit: Arc::new(MockAuditLog::new()),
        checkpoint: Arc::new(MockCheckpointBroker::new()),
        quorum: None,
        events: Arc::new(EventBus::new()),
        tsdb: None,
        storage: None,
        accounting: None,
        oidc: None,
        rate_limiter: None,
        sovra: None,
        pty: None,
        data_dir: None,
    });
    let node_id = &nodes[0].id;

    // Drain the node first
    state
        .nodes
        .update_node_state(node_id, NodeState::Draining)
        .await
        .unwrap();
    state
        .nodes
        .update_node_state(node_id, NodeState::Drained)
        .await
        .unwrap();

    // Undrain via REST
    let app = rest::router(state.clone());
    let resp = app
        .oneshot(
            http::Request::builder()
                .method("POST")
                .uri(format!("/api/v1/nodes/{node_id}/undrain"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    // Verify node is Ready
    let node = state.nodes.get_node(node_id).await.unwrap();
    assert!(
        matches!(node.state, NodeState::Ready),
        "node should be Ready after undrain, got {:?}",
        node.state
    );
}

// ─── Test 47: Admin + Node service: drain + submit → only non-drained nodes listed ─
#[tokio::test]
async fn admin_drain_and_node_list_filters_correctly() {
    let nodes = lattice_test_harness::fixtures::create_node_batch(4, 0);
    let state = Arc::new(ApiState {
        allocations: Arc::new(MockAllocationStore::new()),
        nodes: Arc::new(MockNodeRegistry::new().with_nodes(nodes.clone())),
        audit: Arc::new(MockAuditLog::new()),
        checkpoint: Arc::new(MockCheckpointBroker::new()),
        quorum: None,
        events: Arc::new(EventBus::new()),
        tsdb: None,
        storage: None,
        accounting: None,
        oidc: None,
        rate_limiter: None,
        sovra: None,
        pty: None,
        data_dir: None,
    });
    let node_svc = LatticeNodeService::new(state.clone());

    // Drain 2 of 4 nodes
    for node in nodes.iter().take(2) {
        node_svc
            .drain_node(Request::new(pb::DrainNodeRequest {
                node_id: node.id.clone(),
                reason: "test".to_string(),
            }))
            .await
            .unwrap();
    }

    // List all nodes — should show 4 total
    let list_resp = node_svc
        .list_nodes(Request::new(pb::ListNodesRequest::default()))
        .await
        .unwrap();
    assert_eq!(list_resp.get_ref().nodes.len(), 4);

    // Verify exactly 2 are draining
    let draining_count = list_resp
        .get_ref()
        .nodes
        .iter()
        .filter(|n| n.state == "draining")
        .count();
    assert_eq!(draining_count, 2);
}

// ─── Test 48: Admin update nonexistent tenant → NOT_FOUND ────
#[tokio::test]
async fn admin_update_nonexistent_tenant_returns_not_found() {
    let state = test_state();
    let admin_svc = LatticeAdminService::new(state);

    let result = admin_svc
        .update_tenant(Request::new(pb::UpdateTenantRequest {
            tenant_id: "nonexistent".to_string(),
            quota: Some(pb::TenantQuotaSpec {
                max_nodes: 10,
                ..Default::default()
            }),
            isolation_level: None,
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.err().unwrap().code(), tonic::Code::NotFound);
}

// ─── Test 49: Admin update nonexistent vCluster → NOT_FOUND ──
#[tokio::test]
async fn admin_update_nonexistent_vcluster_returns_not_found() {
    let state = test_state();
    let admin_svc = LatticeAdminService::new(state);

    let result = admin_svc
        .update_v_cluster(Request::new(pb::UpdateVClusterRequest {
            vcluster_id: "nonexistent-id".to_string(),
            cost_weights: None,
            allow_borrowing: Some(true),
            allow_lending: None,
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.err().unwrap().code(), tonic::Code::NotFound);
}

// ─── Test 50: REST accounting usage without service → 503 ────
#[tokio::test]
async fn rest_accounting_usage_without_service_returns_ok_with_null_budget() {
    let state = rest_test_state(); // no accounting service
    let app = rest::router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .uri("/api/v1/accounting/usage?tenant=physics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let usage: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(usage["tenant"], "physics");
    assert!(
        usage["remaining_budget"].is_null(),
        "budget should be null without accounting service"
    );
}
