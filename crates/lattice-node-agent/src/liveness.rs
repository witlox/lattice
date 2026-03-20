//! Liveness probe executor for service allocations.
//!
//! Runs periodic health checks against a workload's exposed endpoint.
//! After `failure_threshold` consecutive failures, the probe reports
//! the allocation as unhealthy so the agent can mark it Failed (and
//! the scheduler reconciler can requeue it).

use std::time::Duration;

use lattice_common::types::{AllocId, LivenessProbe, ProbeType};
use tokio::net::TcpStream;
use tracing::{debug, warn};

/// Result of a single probe execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeResult {
    Success,
    Failure(String),
}

/// Tracks consecutive failures for a single allocation's liveness probe.
pub struct ProbeState {
    pub allocation_id: AllocId,
    pub probe: LivenessProbe,
    pub consecutive_failures: u32,
    /// Whether the initial delay has elapsed.
    pub started: bool,
}

impl ProbeState {
    pub fn new(allocation_id: AllocId, probe: LivenessProbe) -> Self {
        Self {
            allocation_id,
            probe,
            consecutive_failures: 0,
            started: false,
        }
    }

    /// Record a probe result. Returns true if the allocation should be marked Failed.
    pub fn record(&mut self, result: ProbeResult) -> bool {
        match result {
            ProbeResult::Success => {
                if self.consecutive_failures > 0 {
                    debug!(
                        alloc_id = %self.allocation_id,
                        "Liveness probe recovered after {} failures",
                        self.consecutive_failures
                    );
                }
                self.consecutive_failures = 0;
                false
            }
            ProbeResult::Failure(reason) => {
                self.consecutive_failures += 1;
                warn!(
                    alloc_id = %self.allocation_id,
                    failures = self.consecutive_failures,
                    threshold = self.probe.failure_threshold,
                    reason = %reason,
                    "Liveness probe failed"
                );
                self.consecutive_failures >= self.probe.failure_threshold
            }
        }
    }

    /// Whether the failure threshold has been reached.
    pub fn is_failed(&self) -> bool {
        self.consecutive_failures >= self.probe.failure_threshold
    }
}

/// Execute a single probe against the target.
pub async fn run_probe(probe: &LivenessProbe, target_addr: &str) -> ProbeResult {
    let timeout = Duration::from_secs(probe.timeout_secs as u64);

    match &probe.probe_type {
        ProbeType::Tcp { port } => {
            let addr = format!("{target_addr}:{port}");
            match tokio::time::timeout(timeout, TcpStream::connect(&addr)).await {
                Ok(Ok(_)) => ProbeResult::Success,
                Ok(Err(e)) => ProbeResult::Failure(format!("TCP connect to {addr}: {e}")),
                Err(_) => ProbeResult::Failure(format!("TCP connect to {addr}: timeout")),
            }
        }
        ProbeType::Http { port, path } => {
            let url = format!("http://{target_addr}:{port}{path}");
            match tokio::time::timeout(timeout, http_get(&url)).await {
                Ok(Ok(status)) if (200..300).contains(&status) => ProbeResult::Success,
                Ok(Ok(status)) => ProbeResult::Failure(format!("HTTP GET {url}: status {status}")),
                Ok(Err(e)) => ProbeResult::Failure(format!("HTTP GET {url}: {e}")),
                Err(_) => ProbeResult::Failure(format!("HTTP GET {url}: timeout")),
            }
        }
    }
}

/// Minimal HTTP GET that returns the status code. Uses raw TCP to avoid
/// pulling in a full HTTP client dependency in the node agent.
async fn http_get(url: &str) -> Result<u16, String> {
    // Parse host:port from URL
    let without_scheme = url
        .strip_prefix("http://")
        .ok_or("invalid URL: missing http://")?;
    let (host_port, path) = without_scheme
        .split_once('/')
        .unwrap_or((without_scheme, ""));
    let path = format!("/{path}");

    let mut stream = TcpStream::connect(host_port)
        .await
        .map_err(|e| e.to_string())?;

    let request = format!("GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;

    let mut buf = vec![0u8; 1024];
    let n = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
    let response = String::from_utf8_lossy(&buf[..n]);

    // Parse status code from "HTTP/1.1 200 OK"
    let status_line = response.lines().next().ok_or("empty response")?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or("no status code")?
        .parse::<u16>()
        .map_err(|e| e.to_string())?;

    Ok(status_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_probe(failure_threshold: u32) -> LivenessProbe {
        LivenessProbe {
            probe_type: ProbeType::Tcp { port: 8080 },
            period_secs: 10,
            initial_delay_secs: 0,
            failure_threshold,
            timeout_secs: 5,
        }
    }

    #[test]
    fn probe_state_tracks_consecutive_failures() {
        let mut state = ProbeState::new(Uuid::new_v4(), test_probe(3));

        assert!(!state.record(ProbeResult::Failure("err".into())));
        assert_eq!(state.consecutive_failures, 1);

        assert!(!state.record(ProbeResult::Failure("err".into())));
        assert_eq!(state.consecutive_failures, 2);

        // Third failure triggers
        assert!(state.record(ProbeResult::Failure("err".into())));
        assert_eq!(state.consecutive_failures, 3);
        assert!(state.is_failed());
    }

    #[test]
    fn probe_state_resets_on_success() {
        let mut state = ProbeState::new(Uuid::new_v4(), test_probe(3));

        state.record(ProbeResult::Failure("err".into()));
        state.record(ProbeResult::Failure("err".into()));
        assert_eq!(state.consecutive_failures, 2);

        // Success resets
        state.record(ProbeResult::Success);
        assert_eq!(state.consecutive_failures, 0);
        assert!(!state.is_failed());
    }

    #[test]
    fn threshold_one_fails_immediately() {
        let mut state = ProbeState::new(Uuid::new_v4(), test_probe(1));
        assert!(state.record(ProbeResult::Failure("err".into())));
    }

    #[test]
    fn success_never_triggers_failure() {
        let mut state = ProbeState::new(Uuid::new_v4(), test_probe(1));
        for _ in 0..100 {
            assert!(!state.record(ProbeResult::Success));
        }
    }

    #[tokio::test]
    async fn tcp_probe_to_closed_port() {
        let probe = LivenessProbe {
            probe_type: ProbeType::Tcp { port: 1 }, // unlikely to be open
            period_secs: 10,
            initial_delay_secs: 0,
            failure_threshold: 1,
            timeout_secs: 1,
        };
        let result = run_probe(&probe, "127.0.0.1").await;
        assert!(matches!(result, ProbeResult::Failure(_)));
    }

    #[tokio::test]
    async fn http_probe_to_closed_port() {
        let probe = LivenessProbe {
            probe_type: ProbeType::Http {
                port: 1,
                path: "/health".into(),
            },
            period_secs: 10,
            initial_delay_secs: 0,
            failure_threshold: 1,
            timeout_secs: 1,
        };
        let result = run_probe(&probe, "127.0.0.1").await;
        assert!(matches!(result, ProbeResult::Failure(_)));
    }
}
