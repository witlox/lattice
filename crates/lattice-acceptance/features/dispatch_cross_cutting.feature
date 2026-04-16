Feature: Dispatch × adjacent features (integration review — 2026-04-16)
  # Verifies that the dispatch implementation (Impl 1..12 + deferral + gate-2
  # fixes) composes correctly with the adjacent features that touch the same
  # seams. Corresponds to specs/integration/dispatch-cross-cutting.md.

  Scenario: INT-1 — cancel tears down the workload, not just the state
    Given allocation "int1-alloc" with assigned_nodes ["int1-node-a", "int1-node-b"]
    When the user cancels "int1-alloc"
    Then the allocation state becomes "Cancelled"
    And a StopAllocation RPC is scheduled for "int1-node-a"
    And a StopAllocation RPC is scheduled for "int1-node-b"

  Scenario: INT-2 — requeue clears all dispatch fields
    Given allocation "int2-alloc" has previously dispatched, retry_count 2, per_node_phase populated
    When RequeueAllocation is applied for "int2-alloc"
    Then allocation "int2-alloc" per_node_phase is empty
    And allocation "int2-alloc" assigned_at is None
    And allocation "int2-alloc" dispatch_retry_count is 0
    And allocation "int2-alloc" last_completion_report_at is None

  Scenario: INT-3 — Dispatcher skips assigned nodes that are not Ready
    Given allocation "int3-alloc" is Running with assigned_nodes ["int3-node"]
    And node "int3-node" state is "Draining"
    When the Dispatcher observes pending dispatches
    Then the allocation "int3-alloc" is not included in pending_dispatches

  Scenario: INT-4 — workload pid is visible to the AllocationManager
    Given the agent has started tracking allocation "int4-alloc"
    When the runtime monitor records pid 12345 for "int4-alloc"
    Then AllocationManager::get("int4-alloc").pid is Some(12345)
