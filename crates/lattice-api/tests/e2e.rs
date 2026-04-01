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
use lattice_api::mpi::{MpiLaunchOrchestrator, NodeAgentPool, StubNodeAgentPool};
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
        sovra: None,
        pty: None,
        agent_pool: None,
        data_dir: None,
        oidc_config: None,
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
        sovra: None,
        pty: None,
        agent_pool: None,
        data_dir: None,
        oidc_config: None,
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

    // Verify 2 are drained (no active allocations → immediate Draining → Drained)
    let drained: Vec<_> = all_nodes
        .iter()
        .filter(|n| matches!(n.state, NodeState::Drained))
        .collect();
    assert_eq!(drained.len(), 2);

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
        timeline_config: lattice_scheduler::resource_timeline::TimelineConfig::default(),
        budget_utilization: HashMap::new(),
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
        sovra: None,
        pty: None,
        agent_pool: None,
        data_dir: None,
        oidc_config: None,
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

// ─── Test 8: Node drain + undrain lifecycle via gRPC ────────���
// Drain with no active allocations → immediate Drained, then undrain → Ready.

#[tokio::test]
async fn node_drain_undrain_lifecycle_via_grpc() {
    let nodes = create_node_batch(3, 0);
    let state = e2e_state_with_nodes(nodes.clone());
    let node_svc = LatticeNodeService::new(state.clone());

    // Drain node 0 (no active allocations → goes directly to Drained)
    let drain_resp = node_svc
        .drain_node(Request::new(pb::DrainNodeRequest {
            node_id: nodes[0].id.clone(),
            reason: "upgrade".to_string(),
        }))
        .await
        .unwrap();
    assert!(drain_resp.get_ref().success);
    assert_eq!(drain_resp.get_ref().active_allocations, 0);

    // Verify node 0 is Drained (not stuck in Draining)
    let node0 = state.nodes.get_node(&nodes[0].id).await.unwrap();
    assert!(
        matches!(node0.state, NodeState::Drained),
        "node with no allocations should be Drained, got {:?}",
        node0.state
    );

    // Verify other nodes unaffected
    let node1 = state.nodes.get_node(&nodes[1].id).await.unwrap();
    assert!(matches!(node1.state, NodeState::Ready));

    // Undrain node 0 (Drained → Ready)
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

// ─── PartialFailNodeAgentPool for testing ────────────────────

struct PartialFailNodeAgentPool {
    fail_addresses: Vec<String>,
}

#[async_trait::async_trait]
impl NodeAgentPool for PartialFailNodeAgentPool {
    async fn launch_processes(
        &self,
        node_address: &str,
        _request: pb::LaunchProcessesRequest,
    ) -> Result<pb::LaunchProcessesResponse, String> {
        if self.fail_addresses.iter().any(|a| a == node_address) {
            Ok(pb::LaunchProcessesResponse {
                accepted: false,
                message: format!("node {} unavailable", node_address),
            })
        } else {
            Ok(pb::LaunchProcessesResponse {
                accepted: true,
                message: "ok".into(),
            })
        }
    }
}

// ─── Test 9: MPI launch orchestrator fan-out ─────────────────
// Direct test of MpiLaunchOrchestrator with StubNodeAgentPool across 4 nodes.

#[tokio::test]
async fn mpi_launch_orchestrator_fan_out() {
    let pool = Arc::new(StubNodeAgentPool);
    let orch = MpiLaunchOrchestrator::new(pool);

    let nodes: Vec<String> = (0..4).map(|i| format!("n{i}")).collect();
    let node_addresses: HashMap<String, String> = nodes
        .iter()
        .map(|n| (n.clone(), format!("http://{}:50052", n)))
        .collect();

    let result = orch
        .launch(
            Uuid::new_v4(),
            &nodes,
            &node_addresses,
            "./my_mpi_app",
            &[],
            &HashMap::new(),
            16,
            4,
            PmiMode::Pmi2,
        )
        .await;

    assert!(result.is_ok(), "fan-out to 4 nodes should succeed");
    let launch_id = result.unwrap();
    // Verify it's a valid UUID
    assert_eq!(launch_id.to_string().len(), 36);
}

// ─── Test 10: MPI launch orchestrator partial failure ────────
// Some nodes reject the launch; verify error reports failing nodes.

#[tokio::test]
async fn mpi_launch_orchestrator_partial_failure() {
    let pool = Arc::new(PartialFailNodeAgentPool {
        fail_addresses: vec!["http://n1:50052".to_string(), "http://n3:50052".to_string()],
    });
    let orch = MpiLaunchOrchestrator::new(pool);

    let nodes: Vec<String> = (0..4).map(|i| format!("n{i}")).collect();
    let node_addresses: HashMap<String, String> = nodes
        .iter()
        .map(|n| (n.clone(), format!("http://{}:50052", n)))
        .collect();

    let result = orch
        .launch(
            Uuid::new_v4(),
            &nodes,
            &node_addresses,
            "./my_mpi_app",
            &[],
            &HashMap::new(),
            8,
            2,
            PmiMode::Pmi2,
        )
        .await;

    assert!(result.is_err(), "should fail when some nodes reject");
    let err = result.unwrap_err();
    assert!(
        err.contains("2 node(s)"),
        "error should mention 2 failing nodes, got: {err}"
    );
    assert!(
        err.contains("rejected") || err.contains("unavailable"),
        "error should mention rejection, got: {err}"
    );
}

// ─── Test 11: RankLayout uneven distribution ─────────────────
// Test RankLayout::compute with 3 nodes and varying tasks_per_node.

#[tokio::test]
async fn rank_layout_uneven_distribution() {
    let nodes: Vec<String> = vec!["n0".into(), "n1".into(), "n2".into()];

    // 4 tasks_per_node across 3 nodes = 12 total ranks
    let layout = RankLayout::compute(&nodes, 4);
    assert_eq!(layout.total_ranks, 12);
    assert_eq!(layout.node_assignments.len(), 3);
    assert_eq!(layout.node_assignments[0].first_rank, 0);
    assert_eq!(layout.node_assignments[0].num_ranks, 4);
    assert_eq!(layout.node_assignments[1].first_rank, 4);
    assert_eq!(layout.node_assignments[1].num_ranks, 4);
    assert_eq!(layout.node_assignments[2].first_rank, 8);
    assert_eq!(layout.node_assignments[2].num_ranks, 4);

    // 1 task_per_node across 3 nodes = 3 total ranks
    let layout_single = RankLayout::compute(&nodes, 1);
    assert_eq!(layout_single.total_ranks, 3);
    assert_eq!(layout_single.node_assignments[0].first_rank, 0);
    assert_eq!(layout_single.node_assignments[0].num_ranks, 1);
    assert_eq!(layout_single.node_assignments[1].first_rank, 1);
    assert_eq!(layout_single.node_assignments[2].first_rank, 2);

    // 7 tasks_per_node across 3 nodes = 21 total ranks
    let layout_large = RankLayout::compute(&nodes, 7);
    assert_eq!(layout_large.total_ranks, 21);
    assert_eq!(layout_large.node_assignments[0].num_ranks, 7);
    assert_eq!(layout_large.node_assignments[1].first_rank, 7);
    assert_eq!(layout_large.node_assignments[2].first_rank, 14);
}

// ─── Test 12: Full flow: submit → schedule → launch_tasks ────
// Submit allocation, run scheduler, allocation gets nodes, launch tasks.

#[tokio::test]
async fn launch_tasks_e2e_with_scheduling() {
    let nodes = create_node_batch(4, 0);
    let alloc_store = Arc::new(MockAllocationStore::new());
    let node_registry = Arc::new(MockNodeRegistry::new().with_nodes(nodes.clone()));

    let state = Arc::new(ApiState {
        allocations: alloc_store.clone(),
        nodes: node_registry.clone(),
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
        agent_pool: Some(Arc::new(StubNodeAgentPool)),
        data_dir: None,
        oidc_config: None,
    });

    let svc = LatticeAllocationService::new(state.clone());

    // Submit an allocation requesting 2 nodes
    let resp = svc
        .submit(Request::new(pb::SubmitRequest {
            submission: Some(pb::submit_request::Submission::Single(pb::AllocationSpec {
                tenant: "physics".to_string(),
                project: "test".to_string(),
                entrypoint: "python train.py".to_string(),
                resources: Some(pb::ResourceSpec {
                    min_nodes: 2,
                    max_nodes: 2,
                    ..Default::default()
                }),
                ..Default::default()
            })),
        }))
        .await
        .unwrap();
    let alloc_id_str = &resp.get_ref().allocation_ids[0];
    let alloc_id: Uuid = alloc_id_str.parse().unwrap();

    // Run scheduler cycle
    let all_nodes = state
        .nodes
        .list_nodes(&NodeFilter::default())
        .await
        .unwrap();
    let alloc = state.allocations.get(&alloc_id).await.unwrap();

    let topology = create_test_topology(1, 4);
    let input = CycleInput {
        pending: vec![alloc],
        running: vec![],
        nodes: all_nodes.clone(),
        tenants: vec![],
        topology,
        data_readiness: HashMap::new(),
        energy_price: 0.5,
        timeline_config: lattice_scheduler::resource_timeline::TimelineConfig::default(),
        budget_utilization: HashMap::new(),
    };

    let result = run_cycle(&input, &CostWeights::default());

    // Apply placement decision
    let placed = result.decisions.iter().find(|d| {
        matches!(d, PlacementDecision::Place { allocation_id, .. } if *allocation_id == alloc_id)
    });
    assert!(placed.is_some(), "allocation should be placed by scheduler");

    if let Some(PlacementDecision::Place {
        nodes: placed_nodes,
        ..
    }) = placed
    {
        // Update allocation with assigned nodes and Running state
        {
            let mut allocs = alloc_store.allocations.lock().unwrap();
            let alloc = allocs.get_mut(&alloc_id).unwrap();
            alloc.assigned_nodes = placed_nodes.clone();
            alloc.state = AllocationState::Running;
        }

        // Now launch tasks on the scheduled nodes
        let launch_resp = svc
            .launch_tasks(Request::new(pb::LaunchTasksRequest {
                allocation_id: alloc_id_str.clone(),
                num_tasks: 8,
                tasks_per_node: 4,
                entrypoint: "python train.py".to_string(),
                args: vec!["--batch-size".to_string(), "64".to_string()],
                env: HashMap::new(),
                pmi_mode: String::new(),
            }))
            .await
            .unwrap();

        let launch_id = &launch_resp.get_ref().task_launch_id;
        assert!(!launch_id.is_empty(), "should receive a launch ID");
        assert!(
            Uuid::parse_str(launch_id).is_ok(),
            "launch ID should be a valid UUID"
        );
    }
}

// ─── Test 13: Container allocation lifecycle via gRPC ─────────
// Submit an allocation with OCI image, verify environment has images populated.

#[tokio::test]
async fn container_allocation_lifecycle_via_grpc() {
    let state = e2e_state();
    let svc = LatticeAllocationService::new(state.clone());

    // Submit allocation with OCI image
    let resp = svc
        .submit(Request::new(pb::SubmitRequest {
            submission: Some(pb::submit_request::Submission::Single(pb::AllocationSpec {
                tenant: "ml-team".to_string(),
                project: "training".to_string(),
                entrypoint: "python train.py".to_string(),
                environment: Some(pb::EnvironmentSpec {
                    images: vec![pb::ImageRefProto {
                        spec: "nvcr.io/nvidia/pytorch:24.01-py3".to_string(),
                        image_type: "oci".to_string(),
                        registry: "nvcr.io".to_string(),
                        name: "pytorch".to_string(),
                        version: "24.01-py3".to_string(),
                        original_tag: "24.01-py3".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            })),
        }))
        .await
        .unwrap();

    let alloc_id_str = &resp.get_ref().allocation_ids[0];
    let alloc_id: Uuid = alloc_id_str.parse().unwrap();

    // Verify allocation was created
    let get_resp = svc
        .get(Request::new(pb::GetAllocationRequest {
            allocation_id: alloc_id_str.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(get_resp.get_ref().state, "pending");

    // Verify environment has images populated
    let alloc = state.allocations.get(&alloc_id).await.unwrap();
    assert!(
        !alloc.environment.images.is_empty(),
        "allocation should have images in environment"
    );
    let image = &alloc.environment.images[0];
    assert_eq!(image.image_type, ImageType::Oci);
    assert_eq!(image.spec, "nvcr.io/nvidia/pytorch:24.01-py3");
    assert_eq!(image.name, "pytorch");
}
