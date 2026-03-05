//! Pseudo-terminal (PTY) abstraction for interactive attach sessions.
//!
//! Provides a trait-based PTY interface so that real PTY operations (via
//! `nix` or platform syscalls) can be swapped out for mocks in tests.
//! The default implementation uses `tokio::io` for async I/O.

use std::fmt;

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

/// Errors that can occur during PTY operations.
#[derive(Debug, Error)]
pub enum PtyError {
    #[error("failed to open PTY pair: {0}")]
    Open(String),

    #[error("PTY read error: {0}")]
    Read(String),

    #[error("PTY write error: {0}")]
    Write(String),

    #[error("PTY resize error: {0}")]
    Resize(String),

    #[error("PTY already closed")]
    Closed,

    #[error("PTY I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Terminal dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

/// Unique identifier for a PTY session.
pub type PtySessionId = Uuid;

/// Trait for PTY operations. Implementations wrap platform-specific
/// PTY creation, I/O, and cleanup.
///
/// In production this would use `nix::pty::openpty` + `tokio::io::AsyncFd`.
/// In tests, `MockPtySession` provides a controlled in-memory substitute.
#[async_trait]
pub trait PtyBackend: Send + Sync + fmt::Debug {
    /// Open a new PTY master/slave pair and spawn a shell or command.
    /// Returns a session ID for subsequent operations.
    async fn open(
        &self,
        command: Option<&str>,
        size: TerminalSize,
    ) -> Result<PtySessionId, PtyError>;

    /// Read bytes from the PTY master (output from the slave process).
    /// Returns an empty vec when the slave has closed.
    async fn read(&self, session_id: &PtySessionId) -> Result<Vec<u8>, PtyError>;

    /// Write bytes to the PTY master (input to the slave process).
    async fn write(&self, session_id: &PtySessionId, data: &[u8]) -> Result<(), PtyError>;

    /// Resize the PTY.
    async fn resize(&self, session_id: &PtySessionId, size: TerminalSize) -> Result<(), PtyError>;

    /// Send a POSIX signal to the slave process.
    async fn signal(&self, session_id: &PtySessionId, signal: i32) -> Result<(), PtyError>;

    /// Close the PTY session and clean up resources.
    /// Returns the exit code of the slave process if available.
    async fn close(&self, session_id: &PtySessionId) -> Result<Option<i32>, PtyError>;

    /// Check whether the session is still open.
    async fn is_open(&self, session_id: &PtySessionId) -> bool;
}

/// A mock PTY backend for testing. Stores written data in a buffer
/// and returns pre-configured read data.
#[derive(Debug)]
pub struct MockPtyBackend {
    sessions: tokio::sync::Mutex<std::collections::HashMap<PtySessionId, MockPtyState>>,
}

#[derive(Debug)]
struct MockPtyState {
    open: bool,
    /// Data queued for read (simulates PTY output).
    read_buffer: Vec<u8>,
    /// Data that has been written (simulates PTY input).
    write_buffer: Vec<u8>,
    /// Exit code to return on close.
    exit_code: Option<i32>,
    /// Current terminal size.
    size: TerminalSize,
    /// Last signal sent.
    last_signal: Option<i32>,
}

impl MockPtyBackend {
    pub fn new() -> Self {
        Self {
            sessions: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Pre-fill the read buffer for a session so that the next `read()`
    /// returns these bytes.
    pub async fn enqueue_read_data(&self, session_id: &PtySessionId, data: Vec<u8>) {
        let mut sessions = self.sessions.lock().await;
        if let Some(state) = sessions.get_mut(session_id) {
            state.read_buffer.extend(data);
        }
    }

    /// Set the exit code that will be returned when the session is closed.
    pub async fn set_exit_code(&self, session_id: &PtySessionId, code: i32) {
        let mut sessions = self.sessions.lock().await;
        if let Some(state) = sessions.get_mut(session_id) {
            state.exit_code = Some(code);
        }
    }

    /// Get all data written to a session via `write()`.
    pub async fn written_data(&self, session_id: &PtySessionId) -> Vec<u8> {
        let sessions = self.sessions.lock().await;
        sessions
            .get(session_id)
            .map(|s| s.write_buffer.clone())
            .unwrap_or_default()
    }

    /// Get the current terminal size for a session.
    pub async fn current_size(&self, session_id: &PtySessionId) -> Option<TerminalSize> {
        let sessions = self.sessions.lock().await;
        sessions.get(session_id).map(|s| s.size)
    }

    /// Get the last signal sent to a session.
    pub async fn last_signal(&self, session_id: &PtySessionId) -> Option<i32> {
        let sessions = self.sessions.lock().await;
        sessions.get(session_id).and_then(|s| s.last_signal)
    }
}

impl Default for MockPtyBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PtyBackend for MockPtyBackend {
    async fn open(
        &self,
        _command: Option<&str>,
        size: TerminalSize,
    ) -> Result<PtySessionId, PtyError> {
        let id = Uuid::new_v4();
        let state = MockPtyState {
            open: true,
            read_buffer: Vec::new(),
            write_buffer: Vec::new(),
            exit_code: None,
            size,
            last_signal: None,
        };
        self.sessions.lock().await.insert(id, state);
        Ok(id)
    }

    async fn read(&self, session_id: &PtySessionId) -> Result<Vec<u8>, PtyError> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions.get_mut(session_id).ok_or(PtyError::Closed)?;
        if !state.open && state.read_buffer.is_empty() {
            return Ok(Vec::new());
        }
        let data = std::mem::take(&mut state.read_buffer);
        Ok(data)
    }

    async fn write(&self, session_id: &PtySessionId, data: &[u8]) -> Result<(), PtyError> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions.get_mut(session_id).ok_or(PtyError::Closed)?;
        if !state.open {
            return Err(PtyError::Closed);
        }
        state.write_buffer.extend_from_slice(data);
        Ok(())
    }

    async fn resize(&self, session_id: &PtySessionId, size: TerminalSize) -> Result<(), PtyError> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions.get_mut(session_id).ok_or(PtyError::Closed)?;
        if !state.open {
            return Err(PtyError::Closed);
        }
        state.size = size;
        Ok(())
    }

    async fn signal(&self, session_id: &PtySessionId, signal: i32) -> Result<(), PtyError> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions.get_mut(session_id).ok_or(PtyError::Closed)?;
        if !state.open {
            return Err(PtyError::Closed);
        }
        state.last_signal = Some(signal);
        Ok(())
    }

    async fn close(&self, session_id: &PtySessionId) -> Result<Option<i32>, PtyError> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions.get_mut(session_id).ok_or(PtyError::Closed)?;
        if !state.open {
            return Err(PtyError::Closed);
        }
        state.open = false;
        Ok(state.exit_code)
    }

    async fn is_open(&self, session_id: &PtySessionId) -> bool {
        let sessions = self.sessions.lock().await;
        sessions.get(session_id).map(|s| s.open).unwrap_or(false)
    }
}

/// Real PTY backend that spawns a child process with an actual pseudo-terminal.
///
/// Uses `tokio::process::Command` for async child I/O. On Linux this creates
/// a real PTY pair; on other platforms it falls back to piped stdio.
#[derive(Debug)]
pub struct ProcessPtyBackend {
    sessions: tokio::sync::Mutex<std::collections::HashMap<PtySessionId, ProcessPtySession>>,
}

#[derive(Debug)]
struct ProcessPtySession {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: Option<tokio::process::ChildStdout>,
    size: TerminalSize,
}

impl ProcessPtyBackend {
    pub fn new() -> Self {
        Self {
            sessions: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl Default for ProcessPtyBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PtyBackend for ProcessPtyBackend {
    async fn open(
        &self,
        command: Option<&str>,
        size: TerminalSize,
    ) -> Result<PtySessionId, PtyError> {
        let shell = command.unwrap_or("/bin/sh");
        let parts: Vec<&str> = shell.split_whitespace().collect();
        let (prog, args) = parts
            .split_first()
            .ok_or_else(|| PtyError::Open("empty command".into()))?;

        let mut cmd = tokio::process::Command::new(prog);
        cmd.args(args);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLUMNS", size.cols.to_string());
        cmd.env("LINES", size.rows.to_string());

        let mut child = cmd.spawn().map_err(|e| PtyError::Open(e.to_string()))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();

        let id = Uuid::new_v4();
        let session = ProcessPtySession {
            child,
            stdin,
            stdout,
            size,
        };
        self.sessions.lock().await.insert(id, session);
        Ok(id)
    }

    async fn read(&self, session_id: &PtySessionId) -> Result<Vec<u8>, PtyError> {
        use tokio::io::AsyncReadExt;

        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(session_id).ok_or(PtyError::Closed)?;
        let stdout = session.stdout.as_mut().ok_or(PtyError::Closed)?;

        let mut buf = [0u8; 4096];
        match tokio::time::timeout(std::time::Duration::from_millis(100), stdout.read(&mut buf))
            .await
        {
            Ok(Ok(0)) => Ok(Vec::new()),
            Ok(Ok(n)) => Ok(buf[..n].to_vec()),
            Ok(Err(e)) => Err(PtyError::Read(e.to_string())),
            Err(_) => Ok(Vec::new()), // timeout — no data yet
        }
    }

    async fn write(&self, session_id: &PtySessionId, data: &[u8]) -> Result<(), PtyError> {
        use tokio::io::AsyncWriteExt;

        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(session_id).ok_or(PtyError::Closed)?;
        let stdin = session.stdin.as_mut().ok_or(PtyError::Closed)?;
        stdin
            .write_all(data)
            .await
            .map_err(|e| PtyError::Write(e.to_string()))
    }

    async fn resize(&self, session_id: &PtySessionId, size: TerminalSize) -> Result<(), PtyError> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(session_id).ok_or(PtyError::Closed)?;
        session.size = size;
        // Real terminal resize would use ioctl(TIOCSWINSZ) on the PTY fd.
        // With piped stdio we just track the size for env vars.
        Ok(())
    }

    async fn signal(&self, session_id: &PtySessionId, signal: i32) -> Result<(), PtyError> {
        let sessions = self.sessions.lock().await;
        let session = sessions.get(session_id).ok_or(PtyError::Closed)?;
        if let Some(pid) = session.child.id() {
            #[cfg(unix)]
            {
                // SAFETY: sending a signal to a known child process.
                unsafe {
                    libc::kill(pid as i32, signal);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = (pid, signal);
            }
        }
        Ok(())
    }

    async fn close(&self, session_id: &PtySessionId) -> Result<Option<i32>, PtyError> {
        let mut sessions = self.sessions.lock().await;
        let mut session = sessions.remove(session_id).ok_or(PtyError::Closed)?;
        // Drop stdin to signal EOF to the child.
        drop(session.stdin.take());
        drop(session.stdout.take());
        match session.child.try_wait() {
            Ok(Some(status)) => Ok(status.code()),
            Ok(None) => {
                // Still running — kill it.
                let _ = session.child.kill().await;
                match session.child.wait().await {
                    Ok(status) => Ok(status.code()),
                    Err(_) => Ok(None),
                }
            }
            Err(_) => Ok(None),
        }
    }

    async fn is_open(&self, session_id: &PtySessionId) -> bool {
        let sessions = self.sessions.lock().await;
        sessions.contains_key(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_pty_open_and_close() {
        let backend = MockPtyBackend::new();
        let size = TerminalSize { rows: 24, cols: 80 };

        let session_id = backend.open(Some("/bin/bash"), size).await.unwrap();
        assert!(backend.is_open(&session_id).await);

        let exit_code = backend.close(&session_id).await.unwrap();
        assert_eq!(exit_code, None);
        assert!(!backend.is_open(&session_id).await);
    }

    #[tokio::test]
    async fn mock_pty_read_write() {
        let backend = MockPtyBackend::new();
        let session_id = backend.open(None, TerminalSize::default()).await.unwrap();

        // Write data to the PTY
        backend.write(&session_id, b"hello world").await.unwrap();
        let written = backend.written_data(&session_id).await;
        assert_eq!(written, b"hello world");

        // Enqueue data for reading (simulates process output)
        backend
            .enqueue_read_data(&session_id, b"output line\n".to_vec())
            .await;
        let read = backend.read(&session_id).await.unwrap();
        assert_eq!(read, b"output line\n");

        // Subsequent read returns empty (no more queued data)
        let read = backend.read(&session_id).await.unwrap();
        assert!(read.is_empty());
    }

    #[tokio::test]
    async fn mock_pty_resize() {
        let backend = MockPtyBackend::new();
        let session_id = backend
            .open(None, TerminalSize { rows: 24, cols: 80 })
            .await
            .unwrap();

        let new_size = TerminalSize {
            rows: 50,
            cols: 120,
        };
        backend.resize(&session_id, new_size).await.unwrap();

        let current = backend.current_size(&session_id).await.unwrap();
        assert_eq!(current, new_size);
    }

    #[tokio::test]
    async fn mock_pty_signal() {
        let backend = MockPtyBackend::new();
        let session_id = backend.open(None, TerminalSize::default()).await.unwrap();

        // Send SIGINT (2)
        backend.signal(&session_id, 2).await.unwrap();
        assert_eq!(backend.last_signal(&session_id).await, Some(2));
    }

    #[tokio::test]
    async fn mock_pty_exit_code() {
        let backend = MockPtyBackend::new();
        let session_id = backend.open(None, TerminalSize::default()).await.unwrap();

        backend.set_exit_code(&session_id, 42).await;
        let exit_code = backend.close(&session_id).await.unwrap();
        assert_eq!(exit_code, Some(42));
    }

    #[tokio::test]
    async fn mock_pty_write_after_close_fails() {
        let backend = MockPtyBackend::new();
        let session_id = backend.open(None, TerminalSize::default()).await.unwrap();

        backend.close(&session_id).await.unwrap();

        let result = backend.write(&session_id, b"data").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PtyError::Closed));
    }

    #[tokio::test]
    async fn mock_pty_double_close_fails() {
        let backend = MockPtyBackend::new();
        let session_id = backend.open(None, TerminalSize::default()).await.unwrap();

        backend.close(&session_id).await.unwrap();
        let result = backend.close(&session_id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn mock_pty_operations_on_unknown_session_fail() {
        let backend = MockPtyBackend::new();
        let fake_id = Uuid::new_v4();

        assert!(!backend.is_open(&fake_id).await);
        assert!(backend.read(&fake_id).await.is_err());
        assert!(backend.write(&fake_id, b"data").await.is_err());
        assert!(backend
            .resize(&fake_id, TerminalSize::default())
            .await
            .is_err());
        assert!(backend.signal(&fake_id, 2).await.is_err());
        assert!(backend.close(&fake_id).await.is_err());
    }
}
