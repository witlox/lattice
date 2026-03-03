//! Integration tests for the lattice-checkpoint crate.
//!
//! These tests exercise the checkpoint broker, cost model, loop runner,
//! and protocol resolution together as integrated components.

use std::sync::Arc;

use chrono::Utc;

use lattice_checkpoint::protocol::resolve_protocol;
use lattice_checkpoint::CheckpointLoop;
use lattice_checkpoint::{
    CheckpointDestination, CheckpointParams, CheckpointProtocol, LatticeCheckpointBroker,
};
use lattice_common::types::*;
use lattice_test_harness::fixtures::AllocationBuilder;
use lattice_test_harness::mocks::MockAllocationStore;

fn running_alloc(nodes: usize) -> Allocation {
    let mut alloc = AllocationBuilder::new()
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = (0..nodes).map(|i| format!("n{i}")).collect();
    alloc.started_at = Some(Utc::now() - chrono::Duration::hours(2));
    alloc
}

fn high_pressure_params() -> CheckpointParams {
    CheckpointParams {
        backlog_pressure: 0.9,
        waiting_higher_priority_jobs: 5,
        failure_probability: 0.1,
        ..Default::default()
    }
}

fn low_pressure_params() -> CheckpointParams {
    CheckpointParams {
        backlog_pressure: 0.0,
        waiting_higher_priority_jobs: 0,
        failure_probability: 0.001,
        ..Default::default()
    }
}

// ─── Test 1: evaluate_batch with mixed states ────────────────
// Only Running + checkpointable allocations should produce requests.

#[test]
fn evaluate_batch_mixed_states() {
    let broker = LatticeCheckpointBroker::new(high_pressure_params());

    let running_ok = running_alloc(4);
    let running_ok_id = running_ok.id;

    let pending = AllocationBuilder::new()
        .state(AllocationState::Pending)
        .build();

    let mut running_none = running_alloc(2);
    running_none.checkpoint = CheckpointStrategy::None;

    let completed = AllocationBuilder::new()
        .state(AllocationState::Completed)
        .build();

    let allocations = vec![running_ok, pending, running_none, completed];
    let requests = broker.evaluate_batch(&allocations);

    // Only the first allocation (Running + Auto checkpoint) should produce a request
    assert_eq!(requests.len(), 1, "only one checkpointable running alloc");
    assert_eq!(requests[0].allocation_id, running_ok_id);
    assert_eq!(requests[0].protocol, CheckpointProtocol::Signal);
}

// ─── Test 2: Cost model sensitivity ──────────────────────────
// Same allocation: low pressure → no checkpoint, high → yes.

#[test]
fn cost_model_sensitivity() {
    let alloc = running_alloc(4);

    let low = LatticeCheckpointBroker::new(low_pressure_params());
    let low_results = low.evaluate_batch(std::slice::from_ref(&alloc));
    assert!(
        low_results.is_empty(),
        "low pressure should not trigger checkpoint"
    );

    let high = LatticeCheckpointBroker::new(high_pressure_params());
    let high_results = high.evaluate_batch(&[alloc]);
    assert!(
        !high_results.is_empty(),
        "high pressure should trigger checkpoint"
    );
}

// ─── Test 3: Loop runner full cycle ──────────────────────────
// CheckpointLoop.run_once() with N running allocations → N IDs returned.

#[tokio::test]
async fn loop_runner_full_cycle() {
    let alloc1 = running_alloc(4);
    let alloc2 = running_alloc(2);
    let alloc3 = running_alloc(1);
    let id1 = alloc1.id;
    let id2 = alloc2.id;
    let id3 = alloc3.id;

    let store = Arc::new(MockAllocationStore::new().with_allocations(vec![alloc1, alloc2, alloc3]));
    let broker = Arc::new(LatticeCheckpointBroker::new(high_pressure_params()));

    let loop_runner = CheckpointLoop::new(broker, store);
    let ids = loop_runner.run_once().await;

    assert_eq!(ids.len(), 3, "all 3 running allocations should be flagged");
    assert!(ids.contains(&id1));
    assert!(ids.contains(&id2));
    assert!(ids.contains(&id3));
}

// ─── Test 4: Loop runner cancellation ────────────────────────
// Start run(), send cancel, verify it exits cleanly.

#[tokio::test]
async fn loop_runner_cancellation() {
    let store = Arc::new(MockAllocationStore::new());
    let broker = Arc::new(LatticeCheckpointBroker::new(CheckpointParams::default()));

    let loop_runner =
        CheckpointLoop::new(broker, store).with_interval(std::time::Duration::from_millis(50));

    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    let handle = tokio::spawn(async move {
        loop_runner.run(cancel_rx).await;
    });

    // Let it run briefly
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    cancel_tx.send(true).expect("cancel send should succeed");

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    assert!(
        result.is_ok(),
        "loop should exit within timeout after cancellation"
    );
}

// ─── Test 5: Checkpoint destinations S3 vs NFS ──────────────
// Default → S3, with_nfs() → NFS.

#[test]
fn checkpoint_destinations_s3_vs_nfs() {
    let alloc = running_alloc(4);

    let s3_broker = LatticeCheckpointBroker::new(high_pressure_params());
    let s3_requests = s3_broker.evaluate_batch(std::slice::from_ref(&alloc));
    assert!(!s3_requests.is_empty());
    assert!(
        matches!(&s3_requests[0].destination, CheckpointDestination::S3 { path } if path.starts_with("s3://"))
    );

    let nfs_broker = LatticeCheckpointBroker::new(high_pressure_params()).with_nfs();
    let nfs_requests = nfs_broker.evaluate_batch(&[alloc]);
    assert!(!nfs_requests.is_empty());
    assert!(
        matches!(&nfs_requests[0].destination, CheckpointDestination::Nfs { path } if path.starts_with("/scratch/"))
    );
}

// ─── Test 6: Manual checkpoint strategy resolves ─────────────
// CheckpointStrategy::Manual → Some(Signal).

#[test]
fn manual_checkpoint_strategy_resolves() {
    let mut alloc = AllocationBuilder::new().build();
    alloc.checkpoint = CheckpointStrategy::Manual;
    let protocol = resolve_protocol(&alloc);
    assert_eq!(
        protocol,
        Some(CheckpointProtocol::Signal),
        "Manual defaults to Signal protocol"
    );

    // Verify None → None
    alloc.checkpoint = CheckpointStrategy::None;
    let protocol = resolve_protocol(&alloc);
    assert!(protocol.is_none(), "None strategy should return None");

    // Verify Auto → Signal
    alloc.checkpoint = CheckpointStrategy::Auto;
    let protocol = resolve_protocol(&alloc);
    assert_eq!(protocol, Some(CheckpointProtocol::Signal));
}

// ─── Test 7: Concurrent evaluations produce unique IDs ───────
// Evaluate same allocations twice; all checkpoint_ids must be unique.

#[test]
fn concurrent_evaluations_unique_ids() {
    let broker = LatticeCheckpointBroker::new(high_pressure_params());

    let alloc1 = running_alloc(4);
    let alloc2 = running_alloc(2);
    let allocations = vec![alloc1, alloc2];

    let batch1 = broker.evaluate_batch(&allocations);
    let batch2 = broker.evaluate_batch(&allocations);

    assert_eq!(batch1.len(), 2);
    assert_eq!(batch2.len(), 2);

    let mut all_ids: Vec<String> = batch1
        .iter()
        .chain(batch2.iter())
        .map(|r| r.checkpoint_id.clone())
        .collect();

    let total = all_ids.len();
    all_ids.sort();
    all_ids.dedup();
    assert_eq!(
        all_ids.len(),
        total,
        "all checkpoint IDs across batches must be unique"
    );
}
