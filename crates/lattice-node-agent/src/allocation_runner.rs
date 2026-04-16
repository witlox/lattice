//! Allocation runner — manages the lifecycle of allocations on a node.
//!
//! Lifecycle phases:
//! 1. Prologue: pull uenv image, mount, create directories, pre-stage data
//! 2. Run: execute entrypoint in mount namespace
//! 3. Epilogue: flush logs, unmount, clean temporary files

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use lattice_common::types::{AllocId, AllocationState};

/// State of a locally-running allocation on this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalAllocationPhase {
    Prologue,
    Running,
    Epilogue,
    Completed,
    Failed { reason: String },
}

/// Tracks a single allocation's state on this node.
#[derive(Debug, Clone)]
pub struct LocalAllocation {
    pub id: AllocId,
    pub phase: LocalAllocationPhase,
    pub entrypoint: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Workload PID once the runtime has spawned. INT-4: attach and
    /// state-persistence both need the workload's pid, but the runtime
    /// owns the ProcessHandle. The runtime monitor task calls
    /// `AllocationManager::set_pid` once it has the pid so attach's
    /// nsenter path and reattach persistence can both find it.
    pub pid: Option<u32>,
}

impl LocalAllocation {
    pub fn new(id: AllocId, entrypoint: String) -> Self {
        Self {
            id,
            phase: LocalAllocationPhase::Prologue,
            entrypoint,
            started_at: Utc::now(),
            completed_at: None,
            pid: None,
        }
    }

    /// Transition to the next phase.
    pub fn advance(&mut self) -> Result<(), String> {
        self.phase = match &self.phase {
            LocalAllocationPhase::Prologue => LocalAllocationPhase::Running,
            LocalAllocationPhase::Running => LocalAllocationPhase::Epilogue,
            LocalAllocationPhase::Epilogue => {
                self.completed_at = Some(Utc::now());
                LocalAllocationPhase::Completed
            }
            LocalAllocationPhase::Completed => {
                return Err("allocation already completed".to_string());
            }
            LocalAllocationPhase::Failed { .. } => {
                return Err("allocation has failed".to_string());
            }
        };
        Ok(())
    }

    /// Mark the allocation as failed.
    pub fn fail(&mut self, reason: String) {
        self.phase = LocalAllocationPhase::Failed { reason };
        self.completed_at = Some(Utc::now());
    }

    /// Whether the allocation is still active (not completed or failed).
    pub fn is_active(&self) -> bool {
        !matches!(
            self.phase,
            LocalAllocationPhase::Completed | LocalAllocationPhase::Failed { .. }
        )
    }

    /// Convert local phase to the global allocation state.
    pub fn to_allocation_state(&self) -> AllocationState {
        match &self.phase {
            LocalAllocationPhase::Prologue => AllocationState::Staging,
            LocalAllocationPhase::Running => AllocationState::Running,
            LocalAllocationPhase::Epilogue => AllocationState::Running,
            LocalAllocationPhase::Completed => AllocationState::Completed,
            LocalAllocationPhase::Failed { .. } => AllocationState::Failed,
        }
    }
}

/// INV-D13 latest-wins-per-allocation Completion Report buffer. The
/// struct is cheaply clonable (internal Arc<Mutex<_>>) so multiple
/// concurrent producers (runtime monitor tasks, reattach worker, gRPC
/// handler) can write while the heartbeat loop drains, without explicit
/// locking at each call site and without the full actor-channel pattern
/// originally envisioned in DEC-DISP-09. Concurrency correctness still
/// follows from the invariant (keyed map: multiple upserts for the same
/// key collapse to the most recent phase).
///
/// Architecture deviation note (2026-04-16): DEC-DISP-09 recommended an
/// actor loop via mpsc::Sender<AllocationManagerCmd>. This implementation
/// uses `Arc<Mutex<HashMap<..>>>` instead. Both patterns satisfy INV-D13
/// (latest-wins), INV-D3 (idempotency serialization), and D-ADV-ARCH-03
/// thread-safety. Rationale: attach.rs already uses the same Arc<Mutex<>>
/// idiom for AllocationManager, so this choice keeps the agent codebase
/// uniform; the actor rewrite is deferred until a measurable lock-
/// contention issue arises.
#[derive(Debug, Clone, Default)]
pub struct CompletionBuffer {
    inner:
        std::sync::Arc<std::sync::Mutex<HashMap<AllocId, lattice_common::types::CompletionReport>>>,
}

impl CompletionBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Upsert a Completion Report. INV-D13 semantics — replaces any prior
    /// entry for this allocation. Never blocks beyond the mutex hold.
    pub fn push(&self, report: lattice_common::types::CompletionReport) {
        let mut guard = self.inner.lock().expect("CompletionBuffer mutex poisoned");
        guard.insert(report.allocation_id, report);
    }

    /// Drain all buffered reports. Called by the heartbeat loop at each
    /// heartbeat interval.
    pub fn drain(&self) -> Vec<lattice_common::types::CompletionReport> {
        let mut guard = self.inner.lock().expect("CompletionBuffer mutex poisoned");
        guard.drain().map(|(_, v)| v).collect()
    }

    /// Current buffered count. Useful for observability + cardinality-pressure
    /// detection per FM-D8.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("CompletionBuffer mutex poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Manages all allocations running on a single node.
#[derive(Debug)]
pub struct AllocationManager {
    allocations: HashMap<AllocId, LocalAllocation>,
    /// INV-D13 latest-wins-per-allocation Completion Report buffer.
    /// Extracted as a shared handle so the gRPC handler (running in
    /// parallel tokio tasks) can push reports while the heartbeat loop
    /// (holding `&mut self`) drains — see `completion_buffer()`.
    completion_buffer: CompletionBuffer,
}

impl AllocationManager {
    pub fn new() -> Self {
        Self {
            allocations: HashMap::new(),
            completion_buffer: CompletionBuffer::new(),
        }
    }

    /// Get a cheaply-clonable handle to the completion buffer. Used by the
    /// gRPC server to hand runtime monitor tasks a writable reference.
    pub fn completion_buffer(&self) -> CompletionBuffer {
        self.completion_buffer.clone()
    }

    /// INV-D13: upsert a Completion Report (delegates to the buffer).
    pub fn push_report(&mut self, report: lattice_common::types::CompletionReport) {
        self.completion_buffer.push(report);
    }

    /// Drain all buffered Completion Reports for the next heartbeat.
    pub fn drain_reports(&mut self) -> Vec<lattice_common::types::CompletionReport> {
        self.completion_buffer.drain()
    }

    /// INV-D3 idempotency check: returns true if the allocation is tracked
    /// and has not reached a terminal phase.
    pub fn contains_active(&self, id: &AllocId) -> bool {
        self.allocations
            .get(id)
            .map(|a| a.is_active())
            .unwrap_or(false)
    }

    /// Start tracking a new allocation.
    pub fn start(&mut self, id: AllocId, entrypoint: String) -> Result<(), String> {
        if self.allocations.contains_key(&id) {
            return Err(format!("allocation {id} already tracked"));
        }
        self.allocations
            .insert(id, LocalAllocation::new(id, entrypoint));
        Ok(())
    }

    /// Advance an allocation to the next phase.
    pub fn advance(&mut self, id: &AllocId) -> Result<(), String> {
        let alloc = self
            .allocations
            .get_mut(id)
            .ok_or_else(|| format!("allocation {id} not found"))?;
        alloc.advance()
    }

    /// INT-4: record the pid of the spawned workload so attach (nsenter)
    /// and state-persistence (reattach after agent restart) can find it.
    /// Called by the runtime monitor task once `runtime.spawn()` returns
    /// a handle with a concrete pid. No-op if the pid is already set.
    pub fn set_pid(&mut self, id: &AllocId, pid: u32) -> Result<(), String> {
        let alloc = self
            .allocations
            .get_mut(id)
            .ok_or_else(|| format!("allocation {id} not found"))?;
        alloc.pid = Some(pid);
        Ok(())
    }

    /// Mark an allocation as failed.
    pub fn fail(&mut self, id: &AllocId, reason: String) -> Result<(), String> {
        let alloc = self
            .allocations
            .get_mut(id)
            .ok_or_else(|| format!("allocation {id} not found"))?;
        alloc.fail(reason);
        Ok(())
    }

    /// Get allocation state.
    pub fn get(&self, id: &AllocId) -> Option<&LocalAllocation> {
        self.allocations.get(id)
    }

    /// Count active (non-terminal) allocations.
    pub fn active_count(&self) -> u32 {
        self.allocations.values().filter(|a| a.is_active()).count() as u32
    }

    /// Remove completed/failed allocations from tracking.
    pub fn cleanup(&mut self) -> Vec<AllocId> {
        let finished: Vec<AllocId> = self
            .allocations
            .iter()
            .filter(|(_, a)| !a.is_active())
            .map(|(id, _)| *id)
            .collect();
        for id in &finished {
            self.allocations.remove(id);
        }
        finished
    }

    /// List all tracked allocation IDs.
    pub fn list_ids(&self) -> Vec<AllocId> {
        self.allocations.keys().copied().collect()
    }
}

impl Default for AllocationManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_lifecycle_phases() {
        let id = uuid::Uuid::new_v4();
        let mut alloc = LocalAllocation::new(id, "python train.py".to_string());

        assert_eq!(alloc.phase, LocalAllocationPhase::Prologue);
        assert!(alloc.is_active());
        assert!(matches!(
            alloc.to_allocation_state(),
            AllocationState::Staging
        ));

        alloc.advance().unwrap();
        assert_eq!(alloc.phase, LocalAllocationPhase::Running);
        assert!(matches!(
            alloc.to_allocation_state(),
            AllocationState::Running
        ));

        alloc.advance().unwrap();
        assert_eq!(alloc.phase, LocalAllocationPhase::Epilogue);

        alloc.advance().unwrap();
        assert_eq!(alloc.phase, LocalAllocationPhase::Completed);
        assert!(!alloc.is_active());
        assert!(alloc.completed_at.is_some());
    }

    #[test]
    fn advance_past_completed_fails() {
        let id = uuid::Uuid::new_v4();
        let mut alloc = LocalAllocation::new(id, "test".to_string());
        alloc.advance().unwrap(); // → Running
        alloc.advance().unwrap(); // → Epilogue
        alloc.advance().unwrap(); // → Completed

        assert!(alloc.advance().is_err());
    }

    #[test]
    fn failed_allocation_cannot_advance() {
        let id = uuid::Uuid::new_v4();
        let mut alloc = LocalAllocation::new(id, "test".to_string());
        alloc.fail("GPU error".to_string());

        assert!(!alloc.is_active());
        assert!(alloc.advance().is_err());
        assert!(matches!(
            alloc.to_allocation_state(),
            AllocationState::Failed
        ));
    }

    #[test]
    fn manager_tracks_allocations() {
        let mut mgr = AllocationManager::new();
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();

        mgr.start(id1, "train.py".to_string()).unwrap();
        mgr.start(id2, "eval.py".to_string()).unwrap();
        assert_eq!(mgr.active_count(), 2);
    }

    #[test]
    fn manager_rejects_duplicate() {
        let mut mgr = AllocationManager::new();
        let id = uuid::Uuid::new_v4();
        mgr.start(id, "test".to_string()).unwrap();
        assert!(mgr.start(id, "test".to_string()).is_err());
    }

    #[test]
    fn manager_advance_and_fail() {
        let mut mgr = AllocationManager::new();
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();

        mgr.start(id1, "train.py".to_string()).unwrap();
        mgr.start(id2, "eval.py".to_string()).unwrap();

        // Advance id1 through full lifecycle
        mgr.advance(&id1).unwrap(); // Running
        mgr.advance(&id1).unwrap(); // Epilogue
        mgr.advance(&id1).unwrap(); // Completed

        // Fail id2
        mgr.fail(&id2, "OOM".to_string()).unwrap();

        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn manager_cleanup_removes_finished() {
        let mut mgr = AllocationManager::new();
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        let id3 = uuid::Uuid::new_v4();

        mgr.start(id1, "a".to_string()).unwrap();
        mgr.start(id2, "b".to_string()).unwrap();
        mgr.start(id3, "c".to_string()).unwrap();

        // Complete id1, fail id2, leave id3 active
        mgr.advance(&id1).unwrap();
        mgr.advance(&id1).unwrap();
        mgr.advance(&id1).unwrap();
        mgr.fail(&id2, "error".to_string()).unwrap();

        let removed = mgr.cleanup();
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&id1));
        assert!(removed.contains(&id2));
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn manager_get_returns_state() {
        let mut mgr = AllocationManager::new();
        let id = uuid::Uuid::new_v4();
        mgr.start(id, "test".to_string()).unwrap();
        mgr.advance(&id).unwrap();

        let alloc = mgr.get(&id).unwrap();
        assert_eq!(alloc.phase, LocalAllocationPhase::Running);
    }

    #[test]
    fn manager_advance_unknown_fails() {
        let mut mgr = AllocationManager::new();
        assert!(mgr.advance(&uuid::Uuid::new_v4()).is_err());
    }
}
