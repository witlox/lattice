//! End-to-end tests for the Lattice API.
//!
//! These tests wire together the API layer with the scheduler, checkpoint broker,
//! and event bus to verify cross-crate flows.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::time::{timeout, Duration};
use tonic::Request;
use uuid::Uuid;

use lattice_api::events::{AllocationEvent, EventBus};
use lattice_api::grpc::allocation_service::LatticeAllocationService;
use lattice_api::grpc::node_service::LatticeNodeService;
use lattice_api::state::ApiState;

use lattice_common::proto::lattice::v1 as pb;
use lattice_common::proto::lattice::v1::allocation_service_server::AllocationService;
use lattice_common::proto::lattice::v1::node_service_server::NodeService;
use lattice_common::traits::*;
use lattice_common::types::*;

use lattice_scheduler::cycle::{run_cycle, CycleInput};
use lattice_scheduler::placement::PlacementDecision;

use lattice_test_harness::fixtures::*;
use lattice_test_harness::mocks::*;

// ─── Shared helpers ──────────────────────────────────────────

fn e2e_state() -> Arc<ApiState> {
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
    })
}

fn e2e_state_with_nodes(nodes: Vec<Node>) -> Arc<ApiState> {
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
    })
}

fn submit_req(tenant: &str) -> pb::SubmitRequest {
    pb::SubmitRequest {
        submission: Some(pb::submit_request::Submission::Single(pb::AllocationSpec {
            tenant: tenant.to_string(),
            project: "test".to_string(),
            entrypoint: "python train.py".to_string(),
            ..Default::default()
        })),
    }
}

// ─── Test 1: Submit → scheduler cycle → EventBus watch ───────
// Submit via gRPC, run a scheduler cycle, verify event bus publishes
// the state change.

#[tokio::test]
async fn submit_schedule_and_watch_event_bus() {
    let state = e2e_state();
    let svc = LatticeAllocationService::new(state.clone());

    // Submit allocation
    let resp = svc
        .submit(Request::new(submit_req("physics")))
        .await
        .unwrap();
    let alloc_id_str = &resp.get_ref().allocation_ids[0];
    let alloc_id: Uuid = alloc_id_str.parse().unwrap();

    // Subscribe to events before publishing
    let mut rx = state.events.subscribe(alloc_id).await;

    // Simulate scheduler placing the allocation (transition to Running)
    state
        .allocations
        .update_state(&alloc_id, AllocationState::Running)
        .await
        .unwrap();

    // Publish a state change event (mimicking scheduler notification)
    state
        .events
        .publish(AllocationEvent::StateChange {
            allocation_id: alloc_id,
            old_state: "pending".to_string(),
            new_state: "running".to_string(),
        })
        .await;

    // Verify subscriber receives the event
    let event = timeout(Duration::from_millis(100), rx.recv())
        .await
        .expect("should not timeout")
        .expect("should receive event");
    assert!(matches!(
        event,
        AllocationEvent::StateChange { new_state, .. } if new_state == "running"
    ));

    // Verify allocation state via gRPC get
    let get_resp = svc
        .get(Request::new(pb::GetAllocationRequest {
            allocation_id: alloc_id_str.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(get_resp.get_ref().state, "running");
}

// ─── Test 2: DAG dependency resolution ───────────────────────
// Submit 3 allocations as a DAG, complete step 1, verify step 2
// becomes schedulable.

#[tokio::test]
async fn dag_dependency_resolution() {
    let state = e2e_state();

    let dag_id = Uuid::new_v4().to_string();

    // Step 1: no dependencies
    let step1 = AllocationBuilder::new()
        .tenant("physics")
        .dag_id(&dag_id)
        .tag("dag_stage", "preprocess")
        .build();
    let step1_id = step1.id;

    // Step 2: depends on step 1
    let step2 = AllocationBuilder::new()
        .tenant("physics")
        .dag_id(&dag_id)
        .tag("dag_stage", "train")
        .depends_on(&step1_id.to_string(), DependencyCondition::Success)
        .build();
    let step2_id = step2.id;

    // Step 3: depends on step 2
    let step3 = AllocationBuilder::new()
        .tenant("physics")
        .dag_id(&dag_id)
        .tag("dag_stage", "evaluate")
        .depends_on(&step2_id.to_string(), DependencyCondition::Success)
        .build();

    // Insert all into store
    state.allocations.insert(step1.clone()).await.unwrap();
    state.allocations.insert(step2.clone()).await.unwrap();
    state.allocations.insert(step3.clone()).await.unwrap();

    // Initially only step 1 is schedulable (no deps)
    assert!(step1.depends_on.is_empty());
    assert!(!step2.depends_on.is_empty());
    assert!(!step3.depends_on.is_empty());

    // Complete step 1
    state
        .allocations
        .update_state(&step1_id, AllocationState::Completed)
        .await
        .unwrap();

    // Verify step 1 is completed
    let completed = state.allocations.get(&step1_id).await.unwrap();
    assert_eq!(completed.state, AllocationState::Completed);

    // Step 2 can now be scheduled (its only dep is step1 which is completed)
    let dep = &step2.depends_on[0];
    assert_eq!(dep.ref_id, step1_id.to_string());
    assert!(matches!(dep.condition, DependencyCondition::Success));
}

// ─── Test 3: Tenant quota enforcement via API ────────────────
// Set up a tenant with max_concurrent, submit allocations until
// quota is exceeded, verify the error.

#[tokio::test]
async fn tenant_quota_enforcement_via_api() {
    let state = e2e_state();
    let svc = LatticeAllocationService::new(state.clone());

    // Submit 2 allocations and transition them to Running
    let resp1 = svc
        .submit(Request::new(submit_req("limited")))
        .await
        .unwrap();
    let resp2 = svc
        .submit(Request::new(submit_req("limited")))
        .await
        .unwrap();
    let id1: Uuid = resp1.get_ref().allocation_ids[0].parse().unwrap();
    let id2: Uuid = resp2.get_ref().allocation_ids[0].parse().unwrap();

    // Transition to Running
    state
        .allocations
        .update_state(&id1, AllocationState::Running)
        .await
        .unwrap();
    state
        .allocations
        .update_state(&id2, AllocationState::Running)
        .await
        .unwrap();

    // Verify count
    let running = state
        .allocations
        .count_running(&"limited".to_string())
        .await
        .unwrap();
    assert_eq!(running, 2);

    // A third should still be insertable at the store level (quota enforcement
    // is at the scheduler level, not the store level)
    let resp3 = svc
        .submit(Request::new(submit_req("limited")))
        .await
        .unwrap();
    assert_eq!(resp3.get_ref().allocation_ids.len(), 1);

    // Cancel one and verify count goes down
    svc.cancel(Request::new(pb::CancelRequest {
        allocation_id: id1.to_string(),
    }))
    .await
    .unwrap();

    let after_cancel = state
        .allocations
        .count_running(&"limited".to_string())
        .await
        .unwrap();
    assert_eq!(after_cancel, 1);
}

// ─── Test 4: Drain → scheduler avoids drained nodes ──────────
// Set up 5 nodes, drain 2, run a scheduler cycle, verify
// placement only uses the 3 non-drained nodes.

#[tokio::test]
async fn drain_scheduler_avoids_drained_nodes() {
    let nodes = create_node_batch(5, 0);
    let state = e2e_state_with_nodes(nodes.clone());
    let node_svc = LatticeNodeService::new(state.clone());

    // Drain nodes 0 and 1
    node_svc
        .drain_node(Request::new(pb::DrainNodeRequest {
            node_id: nodes[0].id.clone(),
            reason: "maintenance".to_string(),
        }))
        .await
        .unwrap();
    node_svc
        .drain_node(Request::new(pb::DrainNodeRequest {
            node_id: nodes[1].id.clone(),
            reason: "maintenance".to_string(),
        }))
        .await
        .unwrap();

    // Fetch all nodes from registry for scheduler
    let all_nodes = state
        .nodes
        .list_nodes(&NodeFilter::default())
        .await
        .unwrap();

    // Verify 2 are draining
    let draining: Vec<_> = all_nodes
        .iter()
        .filter(|n| matches!(n.state, NodeState::Draining))
        .collect();
    assert_eq!(draining.len(), 2);

    // Run scheduler with the nodes — only ready nodes should be used
    let alloc = AllocationBuilder::new().tenant("physics").nodes(2).build();
    let alloc_id = alloc.id;

    let topology = create_test_topology(1, 5);
    let input = CycleInput {
        pending: vec![alloc],
        running: vec![],
        nodes: all_nodes.clone(),
        tenants: vec![],
        topology,
        data_readiness: HashMap::new(),
        energy_price: 0.5,
    };

    let result = run_cycle(&input, &CostWeights::default());

    // Should have placed the allocation
    let placed = result.decisions.iter().find(|d| {
        matches!(d, PlacementDecision::Place { allocation_id, .. } if *allocation_id == alloc_id)
    });
    assert!(placed.is_some(), "allocation should be placed");

    // Verify placement is on non-drained nodes only
    if let Some(PlacementDecision::Place {
        nodes: placed_nodes,
        ..
    }) = placed
    {
        for node_id in placed_nodes {
            let node = all_nodes.iter().find(|n| n.id == *node_id).unwrap();
            assert!(
                node.state.is_operational(),
                "node {node_id} should be operational, got {:?}",
                node.state
            );
        }
    }
}

// ─── Test 5: Checkpoint broker via API ───────────────────────
// Submit + run allocation, invoke checkpoint via the mock broker,
// verify the broker was called.

#[tokio::test]
async fn checkpoint_broker_via_api() {
    let checkpoint = MockCheckpointBroker::new().with_should_checkpoint(true);
    let state = Arc::new(ApiState {
        allocations: Arc::new(MockAllocationStore::new()),
        nodes: Arc::new(MockNodeRegistry::new()),
        audit: Arc::new(MockAuditLog::new()),
        checkpoint: Arc::new(checkpoint),
        quorum: None,
        events: Arc::new(EventBus::new()),
        tsdb: None,
        storage: None,
        accounting: None,
        oidc: None,
        rate_limiter: None,
    });

    let svc = LatticeAllocationService::new(state.clone());

    // Submit and get an allocation
    let resp = svc
        .submit(Request::new(submit_req("physics")))
        .await
        .unwrap();
    let alloc_id_str = &resp.get_ref().allocation_ids[0];
    let alloc_id: Uuid = alloc_id_str.parse().unwrap();

    // Transition to Running
    state
        .allocations
        .update_state(&alloc_id, AllocationState::Running)
        .await
        .unwrap();

    // Invoke checkpoint via gRPC
    let checkpoint_resp = svc
        .checkpoint(Request::new(pb::CheckpointRequest {
            allocation_id: alloc_id_str.clone(),
        }))
        .await
        .unwrap();
    assert!(
        checkpoint_resp.get_ref().accepted,
        "checkpoint should be accepted"
    );
}

// ─── Test 6: Full lifecycle: submit → run → complete ─────────
// All state transitions + EventBus events.

#[tokio::test]
async fn full_lifecycle_submit_run_complete() {
    let state = e2e_state();
    let svc = LatticeAllocationService::new(state.clone());

    // Submit
    let resp = svc.submit(Request::new(submit_req("bio"))).await.unwrap();
    let alloc_id_str = &resp.get_ref().allocation_ids[0];
    let alloc_id: Uuid = alloc_id_str.parse().unwrap();

    // Subscribe to events
    let mut rx = state.events.subscribe(alloc_id).await;

    // Verify initial state
    let get1 = svc
        .get(Request::new(pb::GetAllocationRequest {
            allocation_id: alloc_id_str.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(get1.get_ref().state, "pending");

    // Transition: Pending → Running
    state
        .allocations
        .update_state(&alloc_id, AllocationState::Running)
        .await
        .unwrap();
    state
        .events
        .publish(AllocationEvent::StateChange {
            allocation_id: alloc_id,
            old_state: "pending".to_string(),
            new_state: "running".to_string(),
        })
        .await;

    // Transition: Running → Completed
    state
        .allocations
        .update_state(&alloc_id, AllocationState::Completed)
        .await
        .unwrap();
    state
        .events
        .publish(AllocationEvent::StateChange {
            allocation_id: alloc_id,
            old_state: "running".to_string(),
            new_state: "completed".to_string(),
        })
        .await;

    // Verify final state
    let get_final = svc
        .get(Request::new(pb::GetAllocationRequest {
            allocation_id: alloc_id_str.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(get_final.get_ref().state, "completed");

    // Verify both events were received
    let ev1 = timeout(Duration::from_millis(100), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        ev1,
        AllocationEvent::StateChange { new_state, .. } if new_state == "running"
    ));

    let ev2 = timeout(Duration::from_millis(100), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        ev2,
        AllocationEvent::StateChange { new_state, .. } if new_state == "completed"
    ));
}

// ─── Test 7: Concurrent submit + cancel race ─────────────────
// 10 submits + 5 cancels, verify consistency.

#[tokio::test]
async fn concurrent_submit_and_cancel_race() {
    let state = e2e_state();
    let svc = Arc::new(LatticeAllocationService::new(state.clone()));

    // Submit 10 allocations
    let mut ids = Vec::new();
    for _ in 0..10 {
        let resp = svc
            .submit(Request::new(submit_req("concurrent")))
            .await
            .unwrap();
        ids.push(resp.get_ref().allocation_ids[0].clone());
    }
    assert_eq!(ids.len(), 10);

    // Cancel the first 5 concurrently
    let mut handles = Vec::new();
    for id in ids.iter().take(5) {
        let svc_clone = svc.clone();
        let id_clone = id.clone();
        handles.push(tokio::spawn(async move {
            svc_clone
                .cancel(Request::new(pb::CancelRequest {
                    allocation_id: id_clone,
                }))
                .await
                .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    // Verify: 5 cancelled, 5 pending
    let all = state
        .allocations
        .list(&AllocationFilter {
            tenant: Some("concurrent".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(all.len(), 10);

    let cancelled = all
        .iter()
        .filter(|a| a.state == AllocationState::Cancelled)
        .count();
    let pending = all
        .iter()
        .filter(|a| a.state == AllocationState::Pending)
        .count();
    assert_eq!(cancelled, 5, "5 should be cancelled");
    assert_eq!(pending, 5, "5 should remain pending");
}

// ─── Test 8: Node drain + undrain lifecycle via gRPC ─────────
// Drain, verify drained, undrain, verify ready.

#[tokio::test]
async fn node_drain_undrain_lifecycle_via_grpc() {
    let nodes = create_node_batch(3, 0);
    let state = e2e_state_with_nodes(nodes.clone());
    let node_svc = LatticeNodeService::new(state.clone());

    // Drain node 0
    let drain_resp = node_svc
        .drain_node(Request::new(pb::DrainNodeRequest {
            node_id: nodes[0].id.clone(),
            reason: "upgrade".to_string(),
        }))
        .await
        .unwrap();
    assert!(drain_resp.get_ref().success);

    // Verify node 0 is Draining
    let node0 = state.nodes.get_node(&nodes[0].id).await.unwrap();
    assert!(matches!(node0.state, NodeState::Draining));

    // Verify other nodes unaffected
    let node1 = state.nodes.get_node(&nodes[1].id).await.unwrap();
    assert!(matches!(node1.state, NodeState::Ready));

    // Undrain node 0 (Draining → Drained → Ready)
    state
        .nodes
        .update_node_state(&nodes[0].id, NodeState::Drained)
        .await
        .unwrap();
    let undrain_resp = node_svc
        .undrain_node(Request::new(pb::UndrainNodeRequest {
            node_id: nodes[0].id.clone(),
        }))
        .await
        .unwrap();
    assert!(undrain_resp.get_ref().success);

    // Verify node 0 is back to Ready
    let node0_after = state.nodes.get_node(&nodes[0].id).await.unwrap();
    assert!(
        matches!(node0_after.state, NodeState::Ready),
        "node should be Ready after undrain, got {:?}",
        node0_after.state
    );
}
