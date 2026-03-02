use std::future::Future;
use std::time::Duration;

use lattice_common::types::{AllocationState, NetworkDomainState, NodeState};

/// Retry an async predicate with backoff until it returns true or timeout elapses.
pub async fn assert_eventually<F, Fut>(f: F, timeout: Duration, description: &str)
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = std::time::Instant::now();
    let mut interval = Duration::from_millis(10);
    loop {
        if f().await {
            return;
        }
        if start.elapsed() > timeout {
            panic!(
                "assert_eventually timed out after {:?}: {description}",
                timeout
            );
        }
        tokio::time::sleep(interval).await;
        // Exponential backoff capped at 500ms
        interval = std::cmp::min(interval * 2, Duration::from_millis(500));
    }
}

/// Assert that a state transition is valid according to the AllocationState machine.
pub fn assert_valid_allocation_transition(from: &AllocationState, to: &AllocationState) {
    assert!(
        from.can_transition_to(to),
        "Invalid allocation state transition: {from:?} -> {to:?}"
    );
}

/// Assert that a state transition is valid according to the NodeState machine.
pub fn assert_valid_node_transition(from: &NodeState, to: &NodeState) {
    assert!(
        from.can_transition_to(to),
        "Invalid node state transition: {from:?} -> {to:?}"
    );
}

/// Assert that a state transition is valid according to the NetworkDomainState machine.
pub fn assert_valid_network_domain_transition(from: &NetworkDomainState, to: &NetworkDomainState) {
    assert!(
        from.can_transition_to(to),
        "Invalid network domain state transition: {from:?} -> {to:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_allocation_transition_passes() {
        assert_valid_allocation_transition(&AllocationState::Pending, &AllocationState::Running);
    }

    #[test]
    #[should_panic(expected = "Invalid allocation state transition")]
    fn invalid_allocation_transition_panics() {
        assert_valid_allocation_transition(&AllocationState::Completed, &AllocationState::Running);
    }

    #[test]
    fn valid_node_transition_passes() {
        assert_valid_node_transition(&NodeState::Ready, &NodeState::Draining);
    }

    #[test]
    #[should_panic(expected = "Invalid node state transition")]
    fn invalid_node_transition_panics() {
        assert_valid_node_transition(&NodeState::Draining, &NodeState::Ready);
    }

    #[tokio::test]
    async fn assert_eventually_succeeds_immediately() {
        assert_eventually(
            || async { true },
            Duration::from_secs(1),
            "should pass immediately",
        )
        .await;
    }

    #[tokio::test]
    async fn assert_eventually_succeeds_after_delay() {
        let start = std::time::Instant::now();
        assert_eventually(
            move || {
                let elapsed = start.elapsed();
                async move { elapsed > Duration::from_millis(50) }
            },
            Duration::from_secs(1),
            "should pass after ~50ms",
        )
        .await;
    }
}
