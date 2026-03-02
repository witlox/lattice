//! Attach manager — manages interactive terminal attach sessions.
//!
//! Users can attach to running allocations to get an interactive shell
//! or run commands within the allocation's environment. The `AttachManager`
//! validates ownership, manages session lifecycle, and delegates PTY
//! operations to a `PtyBackend`.
//!
//! Architecture notes from docs/architecture/system-architecture.md:
//! - Attach uses nsenter (not a new allocation)
//! - The API server forwards the Attach bidirectional stream to the
//!   node agent, which manages the actual PTY session

use std::collections::HashMap;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, warn};
use uuid::Uuid;

use lattice_common::types::{AllocId, UserId};

use crate::allocation_runner::{AllocationManager, LocalAllocationPhase};
use crate::pty::{PtyBackend, PtyError, PtySessionId, TerminalSize};

/// Errors from attach operations.
#[derive(Debug, Error)]
pub enum AttachError {
    #[error("allocation {0} not found on this node")]
    AllocationNotFound(AllocId),

    #[error("allocation {0} is not running (phase: {1})")]
    AllocationNotRunning(AllocId, String),

    #[error("user {user} is not allowed to attach to allocation {alloc_id} (owner: {owner})")]
    PermissionDenied {
        user: UserId,
        alloc_id: AllocId,
        owner: UserId,
    },

    #[error("session {0} not found")]
    SessionNotFound(AttachSessionId),

    #[error("PTY error: {0}")]
    Pty(#[from] PtyError),
}

/// Unique identifier for an attach session.
pub type AttachSessionId = Uuid;

/// Metadata tracked for each active attach session.
#[derive(Debug, Clone)]
pub struct AttachSession {
    /// Unique session identifier.
    pub id: AttachSessionId,
    /// The allocation this session is attached to.
    pub alloc_id: AllocId,
    /// The user who initiated the attach.
    pub user: UserId,
    /// The PTY session backing this attach.
    pub pty_session_id: PtySessionId,
    /// The command being run (or the user's shell).
    pub command: Option<String>,
}

/// Ownership lookup for allocations. The `AttachManager` needs to
/// verify that the requesting user owns the allocation. This trait
/// decouples the attach logic from how ownership is tracked.
#[async_trait::async_trait]
pub trait AllocationOwnerLookup: Send + Sync + std::fmt::Debug {
    /// Return the user who owns the given allocation, or None if
    /// the allocation is not found.
    async fn get_owner(&self, alloc_id: &AllocId) -> Option<UserId>;
}

/// Manages active attach sessions on a node.
///
/// Thread-safe: all mutable state is behind a `Mutex`.
pub struct AttachManager {
    /// Active attach sessions, keyed by session ID.
    sessions: Arc<Mutex<HashMap<AttachSessionId, AttachSession>>>,
    /// PTY backend for creating and managing terminal sessions.
    pty_backend: Arc<dyn PtyBackend>,
    /// Allocation manager for checking allocation phase.
    allocations: Arc<Mutex<AllocationManager>>,
    /// Ownership lookup for authorization.
    owner_lookup: Arc<dyn AllocationOwnerLookup>,
}

impl std::fmt::Debug for AttachManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sessions = self.sessions.try_lock();
        let count = sessions.map(|s| s.len()).unwrap_or(0);
        f.debug_struct("AttachManager")
            .field("active_sessions", &count)
            .finish_non_exhaustive()
    }
}

impl AttachManager {
    /// Create a new AttachManager.
    pub fn new(
        pty_backend: Arc<dyn PtyBackend>,
        allocations: Arc<Mutex<AllocationManager>>,
        owner_lookup: Arc<dyn AllocationOwnerLookup>,
    ) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            pty_backend,
            allocations,
            owner_lookup,
        }
    }

    /// Attach to a running allocation. Validates:
    /// 1. The allocation exists on this node and is in `Running` phase
    /// 2. The requesting user owns the allocation
    ///
    /// On success, opens a PTY session and returns the session handle.
    pub async fn attach(
        &self,
        alloc_id: AllocId,
        user: UserId,
        command: Option<String>,
        terminal_size: TerminalSize,
    ) -> Result<AttachSession, AttachError> {
        // Check allocation exists and is running
        {
            let alloc_mgr = self.allocations.lock().await;
            let local_alloc = alloc_mgr
                .get(&alloc_id)
                .ok_or(AttachError::AllocationNotFound(alloc_id))?;

            if local_alloc.phase != LocalAllocationPhase::Running {
                return Err(AttachError::AllocationNotRunning(
                    alloc_id,
                    format!("{:?}", local_alloc.phase),
                ));
            }
        }

        // Check ownership
        let owner = self
            .owner_lookup
            .get_owner(&alloc_id)
            .await
            .ok_or(AttachError::AllocationNotFound(alloc_id))?;

        if owner != user {
            return Err(AttachError::PermissionDenied {
                user,
                alloc_id,
                owner,
            });
        }

        // Open PTY session
        let pty_session_id = self
            .pty_backend
            .open(command.as_deref(), terminal_size)
            .await?;

        let session_id = Uuid::new_v4();
        let session = AttachSession {
            id: session_id,
            alloc_id,
            user: user.clone(),
            pty_session_id,
            command,
        };

        self.sessions
            .lock()
            .await
            .insert(session_id, session.clone());

        debug!(
            session_id = %session_id,
            alloc_id = %alloc_id,
            user = %user,
            "attach session created"
        );

        Ok(session)
    }

    /// Detach (close) an active session. Cleans up the PTY.
    pub async fn detach(&self, session_id: &AttachSessionId) -> Result<Option<i32>, AttachError> {
        let session = {
            let mut sessions = self.sessions.lock().await;
            sessions
                .remove(session_id)
                .ok_or(AttachError::SessionNotFound(*session_id))?
        };

        let exit_code = self.pty_backend.close(&session.pty_session_id).await?;

        debug!(
            session_id = %session_id,
            alloc_id = %session.alloc_id,
            exit_code = ?exit_code,
            "attach session detached"
        );

        Ok(exit_code)
    }

    /// Write data (user input) to an attach session's PTY.
    pub async fn write(
        &self,
        session_id: &AttachSessionId,
        data: &[u8],
    ) -> Result<(), AttachError> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(session_id)
            .ok_or(AttachError::SessionNotFound(*session_id))?;
        self.pty_backend
            .write(&session.pty_session_id, data)
            .await?;
        Ok(())
    }

    /// Read data (process output) from an attach session's PTY.
    pub async fn read(&self, session_id: &AttachSessionId) -> Result<Vec<u8>, AttachError> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(session_id)
            .ok_or(AttachError::SessionNotFound(*session_id))?;
        let data = self.pty_backend.read(&session.pty_session_id).await?;
        Ok(data)
    }

    /// Resize the terminal for an attach session.
    pub async fn resize(
        &self,
        session_id: &AttachSessionId,
        size: TerminalSize,
    ) -> Result<(), AttachError> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(session_id)
            .ok_or(AttachError::SessionNotFound(*session_id))?;
        self.pty_backend
            .resize(&session.pty_session_id, size)
            .await?;
        Ok(())
    }

    /// Forward a signal to the process in an attach session.
    pub async fn signal(
        &self,
        session_id: &AttachSessionId,
        signal: i32,
    ) -> Result<(), AttachError> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(session_id)
            .ok_or(AttachError::SessionNotFound(*session_id))?;
        self.pty_backend
            .signal(&session.pty_session_id, signal)
            .await?;
        Ok(())
    }

    /// Get the number of active attach sessions.
    pub async fn active_session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Get a session by ID (clone).
    pub async fn get_session(&self, session_id: &AttachSessionId) -> Option<AttachSession> {
        self.sessions.lock().await.get(session_id).cloned()
    }

    /// List all active sessions for a given allocation.
    pub async fn sessions_for_allocation(&self, alloc_id: &AllocId) -> Vec<AttachSession> {
        self.sessions
            .lock()
            .await
            .values()
            .filter(|s| s.alloc_id == *alloc_id)
            .cloned()
            .collect()
    }

    /// Detach all sessions for an allocation (e.g., when it completes).
    pub async fn detach_all_for_allocation(&self, alloc_id: &AllocId) -> Vec<AttachSessionId> {
        let session_ids: Vec<AttachSessionId> = self
            .sessions
            .lock()
            .await
            .values()
            .filter(|s| s.alloc_id == *alloc_id)
            .map(|s| s.id)
            .collect();

        let mut detached = Vec::new();
        for sid in session_ids {
            if self.detach(&sid).await.is_ok() {
                detached.push(sid);
            } else {
                warn!(session_id = %sid, "failed to detach session during bulk cleanup");
            }
        }
        detached
    }
}

/// Simple in-memory ownership lookup for testing.
#[derive(Debug)]
pub struct MockOwnerLookup {
    owners: Mutex<HashMap<AllocId, UserId>>,
}

impl MockOwnerLookup {
    pub fn new() -> Self {
        Self {
            owners: Mutex::new(HashMap::new()),
        }
    }

    pub async fn set_owner(&self, alloc_id: AllocId, user: UserId) {
        self.owners.lock().await.insert(alloc_id, user);
    }
}

impl Default for MockOwnerLookup {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AllocationOwnerLookup for MockOwnerLookup {
    async fn get_owner(&self, alloc_id: &AllocId) -> Option<UserId> {
        self.owners.lock().await.get(alloc_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocation_runner::AllocationManager;
    use crate::pty::MockPtyBackend;

    /// Helper to set up an AttachManager with a running allocation.
    async fn setup_with_running_alloc() -> (
        AttachManager,
        AllocId,
        UserId,
        Arc<MockOwnerLookup>,
        Arc<MockPtyBackend>,
    ) {
        let alloc_id = Uuid::new_v4();
        let user: UserId = "alice".to_string();

        // Set up allocation manager with a running allocation
        let alloc_mgr = {
            let mut mgr = AllocationManager::new();
            mgr.start(alloc_id, "python train.py".to_string()).unwrap();
            mgr.advance(&alloc_id).unwrap(); // Prologue -> Running
            Arc::new(Mutex::new(mgr))
        };

        // Set up ownership
        let owner_lookup = Arc::new(MockOwnerLookup::new());
        owner_lookup.set_owner(alloc_id, user.clone()).await;

        // Set up PTY backend
        let pty_backend = Arc::new(MockPtyBackend::new());

        let attach_mgr = AttachManager::new(pty_backend.clone(), alloc_mgr, owner_lookup.clone());

        (attach_mgr, alloc_id, user, owner_lookup, pty_backend)
    }

    #[tokio::test]
    async fn attach_to_running_allocation() {
        let (mgr, alloc_id, user, _, _) = setup_with_running_alloc().await;

        let session = mgr
            .attach(alloc_id, user.clone(), None, TerminalSize::default())
            .await
            .unwrap();

        assert_eq!(session.alloc_id, alloc_id);
        assert_eq!(session.user, user);
        assert_eq!(mgr.active_session_count().await, 1);
    }

    #[tokio::test]
    async fn attach_denied_for_wrong_user() {
        let (mgr, alloc_id, _, _, _) = setup_with_running_alloc().await;

        let result = mgr
            .attach(
                alloc_id,
                "eve".to_string(), // not the owner
                None,
                TerminalSize::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            AttachError::PermissionDenied {
                user,
                alloc_id: err_alloc_id,
                owner,
            } => {
                assert_eq!(user, "eve");
                assert_eq!(err_alloc_id, alloc_id);
                assert_eq!(owner, "alice");
            }
            other => panic!("expected PermissionDenied, got: {other:?}"),
        }
        assert_eq!(mgr.active_session_count().await, 0);
    }

    #[tokio::test]
    async fn attach_fails_for_non_running_allocation() {
        let alloc_id = Uuid::new_v4();
        let user: UserId = "alice".to_string();

        // Allocation in Prologue phase (not yet Running)
        let alloc_mgr = {
            let mut mgr = AllocationManager::new();
            mgr.start(alloc_id, "train.py".to_string()).unwrap();
            Arc::new(Mutex::new(mgr))
        };

        let owner_lookup = Arc::new(MockOwnerLookup::new());
        owner_lookup.set_owner(alloc_id, user.clone()).await;

        let pty_backend = Arc::new(MockPtyBackend::new());
        let attach_mgr = AttachManager::new(pty_backend, alloc_mgr, owner_lookup);

        let result = attach_mgr
            .attach(alloc_id, user, None, TerminalSize::default())
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            AttachError::AllocationNotRunning(_, _)
        ));
    }

    #[tokio::test]
    async fn attach_fails_for_unknown_allocation() {
        let alloc_mgr = Arc::new(Mutex::new(AllocationManager::new()));
        let owner_lookup = Arc::new(MockOwnerLookup::new());
        let pty_backend = Arc::new(MockPtyBackend::new());

        let attach_mgr = AttachManager::new(pty_backend, alloc_mgr, owner_lookup);

        let result = attach_mgr
            .attach(
                Uuid::new_v4(),
                "alice".to_string(),
                None,
                TerminalSize::default(),
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            AttachError::AllocationNotFound(_)
        ));
    }

    #[tokio::test]
    async fn session_lifecycle_attach_and_detach() {
        let (mgr, alloc_id, user, _, _) = setup_with_running_alloc().await;

        // Attach
        let session = mgr
            .attach(alloc_id, user, None, TerminalSize::default())
            .await
            .unwrap();
        let session_id = session.id;

        assert_eq!(mgr.active_session_count().await, 1);
        assert!(mgr.get_session(&session_id).await.is_some());

        // Detach
        let exit_code = mgr.detach(&session_id).await.unwrap();
        assert_eq!(exit_code, None);
        assert_eq!(mgr.active_session_count().await, 0);
        assert!(mgr.get_session(&session_id).await.is_none());
    }

    #[tokio::test]
    async fn detach_unknown_session_fails() {
        let (mgr, _, _, _, _) = setup_with_running_alloc().await;

        let result = mgr.detach(&Uuid::new_v4()).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            AttachError::SessionNotFound(_)
        ));
    }

    #[tokio::test]
    async fn write_and_read_through_attach() {
        let (mgr, alloc_id, user, _, pty_backend) = setup_with_running_alloc().await;

        let session = mgr
            .attach(alloc_id, user, None, TerminalSize::default())
            .await
            .unwrap();

        // Write to the session (user input)
        mgr.write(&session.id, b"ls -la\n").await.unwrap();

        // Verify the PTY received it
        let written = pty_backend.written_data(&session.pty_session_id).await;
        assert_eq!(written, b"ls -la\n");

        // Enqueue output data and read it
        pty_backend
            .enqueue_read_data(&session.pty_session_id, b"total 42\n".to_vec())
            .await;

        let output = mgr.read(&session.id).await.unwrap();
        assert_eq!(output, b"total 42\n");
    }

    #[tokio::test]
    async fn resize_session() {
        let (mgr, alloc_id, user, _, pty_backend) = setup_with_running_alloc().await;

        let session = mgr
            .attach(alloc_id, user, None, TerminalSize::default())
            .await
            .unwrap();

        let new_size = TerminalSize {
            rows: 50,
            cols: 200,
        };
        mgr.resize(&session.id, new_size).await.unwrap();

        let actual = pty_backend
            .current_size(&session.pty_session_id)
            .await
            .unwrap();
        assert_eq!(actual, new_size);
    }

    #[tokio::test]
    async fn signal_forwarding() {
        let (mgr, alloc_id, user, _, pty_backend) = setup_with_running_alloc().await;

        let session = mgr
            .attach(alloc_id, user, None, TerminalSize::default())
            .await
            .unwrap();

        // Send SIGINT (2)
        mgr.signal(&session.id, 2).await.unwrap();

        let sig = pty_backend
            .last_signal(&session.pty_session_id)
            .await
            .unwrap();
        assert_eq!(sig, 2);
    }

    #[tokio::test]
    async fn multiple_sessions_on_same_allocation() {
        let (mgr, alloc_id, _user, owner_lookup, _) = setup_with_running_alloc().await;

        // A second user (bob) also owns the allocation for this test
        let user2: UserId = "bob".to_string();
        owner_lookup.set_owner(alloc_id, user2.clone()).await;

        // Alice attaches (will fail because ownership was overwritten to bob)
        // Instead, let bob attach twice
        let _session1 = mgr
            .attach(alloc_id, user2.clone(), None, TerminalSize::default())
            .await
            .unwrap();
        let _session2 = mgr
            .attach(
                alloc_id,
                user2.clone(),
                Some("/bin/zsh".to_string()),
                TerminalSize::default(),
            )
            .await
            .unwrap();

        assert_eq!(mgr.active_session_count().await, 2);

        let alloc_sessions = mgr.sessions_for_allocation(&alloc_id).await;
        assert_eq!(alloc_sessions.len(), 2);

        // Detach all
        let detached = mgr.detach_all_for_allocation(&alloc_id).await;
        assert_eq!(detached.len(), 2);
        assert_eq!(mgr.active_session_count().await, 0);
    }

    #[tokio::test]
    async fn detach_all_for_allocation_with_no_sessions() {
        let (mgr, alloc_id, _, _, _) = setup_with_running_alloc().await;
        let detached = mgr.detach_all_for_allocation(&alloc_id).await;
        assert!(detached.is_empty());
    }

    #[tokio::test]
    async fn attach_with_custom_command() {
        let (mgr, alloc_id, user, _, _) = setup_with_running_alloc().await;

        let session = mgr
            .attach(
                alloc_id,
                user,
                Some("python3".to_string()),
                TerminalSize {
                    rows: 30,
                    cols: 100,
                },
            )
            .await
            .unwrap();

        assert_eq!(session.command, Some("python3".to_string()));
    }

    #[tokio::test]
    async fn exit_code_propagated_on_detach() {
        let (mgr, alloc_id, user, _, pty_backend) = setup_with_running_alloc().await;

        let session = mgr
            .attach(alloc_id, user, None, TerminalSize::default())
            .await
            .unwrap();

        // Set exit code on the mock PTY
        pty_backend
            .set_exit_code(&session.pty_session_id, 137)
            .await;

        let exit_code = mgr.detach(&session.id).await.unwrap();
        assert_eq!(exit_code, Some(137));
    }
}
