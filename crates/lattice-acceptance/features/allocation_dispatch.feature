Feature: Allocation Dispatch
  The bridge from a scheduler node-assignment to an actual Workload Process on
  the target node. Exercises the real wire (RunAllocation RPC, Completion
  Reports on Heartbeat) rather than the abstract state machine.
  Maps to invariants INV-D1 through INV-D10 in specs/invariants.md.

  Background:
    Given a quorum with one leader and two followers
    And a tenant "ov" with a quota of 10 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"

  # ─── Node registration with Agent Address (INV-D1, INV-D2) ─────────

  Scenario: Register node with valid Agent Address is accepted
    Given an agent for node "x1000c0s0b0n0" started on "10.1.0.5:50052"
    When the agent calls RegisterNode with address "10.1.0.5:50052"
    Then the quorum commits the node record with agent_address "10.1.0.5:50052"
    And the node state transitions to "Ready"
    And the scheduler's available_nodes() includes "x1000c0s0b0n0"

  Scenario: Register node without Agent Address is rejected
    Given an agent for node "x1000c0s0b0n0"
    When the agent calls RegisterNode with address ""
    Then the quorum rejects the proposal with reason "agent_address required"
    And the node record is not created

  Scenario: Register node with syntactically invalid address is rejected
    Given an agent for node "x1000c0s0b0n0"
    When the agent calls RegisterNode with address "0.0.0.0:0"
    Then the quorum rejects the proposal with reason "agent_address invalid"
    And the node record is not created

  Scenario: Addressless Ready node is scheduler-invisible
    Given a node record "x1000c0s0b0n0" with state "Ready" and agent_address absent
    When the scheduler calls available_nodes()
    Then the result does not contain "x1000c0s0b0n0"
    And no allocation is ever placed on "x1000c0s0b0n0"

  # ─── Happy path per runtime (INV-D5 positive case) ─────────────────

  Scenario: Bare-process allocation completes end-to-end
    Given a Ready node "x1000c0s0b0n0" with agent_address "10.1.0.5:50052"
    And a pending bounded allocation with entrypoint "/bin/echo hello"
    And the allocation has no uenv and no image
    When the scheduler assigns "x1000c0s0b0n0" to the allocation
    And the dispatcher calls RunAllocation on "10.1.0.5:50052"
    Then the agent selects the Bare-Process Runtime
    And the agent spawns a Workload Process under workload.slice
    And the next heartbeat carries a Completion Report phase "Running" with pid set
    And eventually the heartbeat carries a Completion Report phase "Completed" with exit_code 0
    And the allocation state in GlobalState is "Completed"

  Scenario: Uenv allocation completes end-to-end
    Given a Ready node "x1000c0s0b0n0" with agent_address "10.1.0.5:50052"
    And a pending bounded allocation with uenv "prgenv-gnu/24.11:v1"
    And the uenv image is cached on the node
    When the scheduler assigns "x1000c0s0b0n0" to the allocation
    And the dispatcher calls RunAllocation on "10.1.0.5:50052"
    Then the agent selects the Uenv Runtime
    And the agent mounts the squashfs image and spawns via nsenter
    And eventually the Completion Report phase is "Completed" with exit_code 0

  Scenario: Podman allocation completes end-to-end
    Given a Ready node "x1000c0s0b0n0" with agent_address "10.1.0.5:50052"
    And a pending bounded allocation with image "registry/app:latest"
    When the scheduler assigns "x1000c0s0b0n0" to the allocation
    And the dispatcher calls RunAllocation on "10.1.0.5:50052"
    Then the agent selects the Podman Runtime
    And the agent pulls the image and spawns via podman run + nsenter
    And eventually the Completion Report phase is "Completed" with exit_code 0

  # ─── Dispatch idempotency (INV-D3, INV-D4) ─────────────────────────

  Scenario: Duplicate RunAllocation spawns at most one Workload Process (INV-D3)
    Given a Ready node "x1000c0s0b0n0" with a running allocation
    When the dispatcher calls RunAllocation for the same allocation a second time
    Then the agent responds accepted with reason "already_running"
    And the AllocationManager still has exactly one Workload Process
    And no second cgroup scope is created

  Scenario: Duplicate Completion Report produces one state transition (INV-D4)
    Given an allocation whose state is "Completed" from a prior Completion Report
    When a heartbeat arrives carrying a Completion Report for the same (allocation_id, "Completed")
    Then the quorum acknowledges the heartbeat
    And no second state transition is applied
    And no duplicate accounting event is emitted
    And no duplicate audit event is emitted

  # ─── Dispatch failure and bounded retry (Q3, INV-D6) ───────────────

  Scenario: Agent unreachable on all attempts triggers Requeue On Dispatch Failure
    Given a Ready node "x1000c0s0b0n0" with agent_address "10.1.0.5:50052"
    And the agent process is not actually listening on that port
    And a pending allocation assigned to "x1000c0s0b0n0"
    When the dispatcher attempts RunAllocation
    Then 3 attempts are made with backoff 1s, 2s, 5s
    And each attempt fails with "connection refused" or timeout
    And a single Raft proposal atomically transitions state to "Pending", increments dispatch_retry_count, and releases node ownership
    And on the next scheduler cycle, the allocation is eligible to be placed on a different node

  Scenario: Agent accepts on third attempt after transient failures
    Given a Ready node "x1000c0s0b0n0" whose agent is recovering
    And the first two RunAllocation attempts time out
    When the third RunAllocation attempt reaches the agent
    Then the agent accepts and begins prologue
    And the allocation proceeds to Running normally
    And dispatch_retry_count remains 0 (successful dispatch, no rollback counted)

  Scenario: Agent address changes between attempts with commit-visible before retry (INV-D10)
    Given a Ready node whose agent restarted on port "50062" after Attempt 1
    And the agent successfully called UpdateNodeAddress("10.1.0.5:50062")
    And that command has committed in Raft before Attempt 2 begins
    When the dispatcher performs Attempt 2
    Then the dispatcher re-reads agent_address from GlobalState
    And Attempt 2 targets "10.1.0.5:50062" not the stale "10.1.0.5:50052"

  Scenario: Agent address changes but commit lags behind retry (INV-D10 best-effort)
    Given a Ready node whose agent restarted on port "50062" after Attempt 1
    And the UpdateNodeAddress command has not yet committed when Attempt 2 begins
    When the dispatcher performs Attempt 2
    Then the dispatcher re-reads agent_address from GlobalState
    And Attempt 2 targets the still-stale "10.1.0.5:50052"
    And Attempt 2 fails
    And either a subsequent attempt sees the new address or the retry budget exhausts and rollback fires

  # ─── Post-accept failures (INV-D5, INV-D8) ─────────────────────────

  Scenario: Agent accepts then crashes before spawn triggers node_silent failure
    Given a Ready node "x1000c0s0b0n0" with agent_address "10.1.0.5:50052"
    And the agent accepted RunAllocation but crashed before selecting a Runtime
    When the scheduler's silent-node sweep runs after heartbeat_interval + grace_period
    Then the sweep detects no heartbeat or Completion Report for the allocation
    And the allocation state transitions to "Failed" with reason "node_silent"
    And the allocation's RequeuePolicy is applied (requeue if "always" or "on_node_failure")

  Scenario: Workload Process exits and Completion Report arrives within one heartbeat
    Given a Running allocation with a live Workload Process on "x1000c0s0b0n0"
    When the Workload Process exits with code 0
    Then the agent appends a Completion Report phase "Completed" to its heartbeat buffer
    And the next heartbeat (within heartbeat_interval) delivers the report
    And the quorum applies state "Completed" within 2 × heartbeat_interval of the exit

  # ─── Reattach after agent restart (INV-D5(b), INV-D9) ──────────────

  Scenario: Agent restart with live Workload Process continues the allocation
    Given a Running allocation with pid 12345 persisted in AgentState
    And the pid 12345 is still alive after agent restart
    When the new agent starts up
    Then reattach recognizes pid 12345 via is_process_alive
    And the allocation is restored to AllocationManager with phase "Running"
    And the allocation state in GlobalState remains "Running"
    And heartbeats resume carrying the allocation in the running set

  Scenario: Agent restart with dead Workload Process reports Failed
    Given a Running allocation with pid 12345 persisted in AgentState
    And the pid 12345 is no longer alive after agent restart
    When the new agent starts up
    Then reattach detects the process is dead
    And the agent enqueues a Completion Report phase "Failed" with reason "pid_vanished_across_restart"
    And the next heartbeat carries the report
    And the allocation state transitions to "Failed" in GlobalState

  Scenario: Orphan cgroup scope is cleaned up on agent boot (INV-D9)
    Given a cgroup scope at "workload.slice/alloc-9999.scope" exists on disk
    And no allocation with id "9999" is in AgentState
    And no reattach record references "9999"
    When the agent boots
    Then the orphan scope is terminated and removed within heartbeat_interval
    And a lost_workload audit event is emitted with allocation_id "9999"

  # ─── Race conditions (INV-D6, INV-D7) ──────────────────────────────

  Scenario: Late Completion Report wins the race against rollback proposal (INV-D6)
    Given the dispatcher observed allocation state_version 5
    And the dispatcher is about to submit a RollbackDispatch proposal based on version 5
    When a heartbeat arrives first carrying a Completion Report "Completed" which advances state_version to 6
    And the RollbackDispatch proposal is then submitted
    Then the quorum rejects the RollbackDispatch due to state_version mismatch
    And the allocation remains in state "Completed"
    And node ownership is released by the normal completion path, not by rollback

  Scenario: Phase regression Completion Report is rejected (INV-D7)
    Given an allocation whose current state is "Running"
    When a heartbeat arrives carrying a Completion Report phase "Staging" for that allocation
    Then the quorum logs an anomaly with (node_id, allocation_id, "Running", "Staging")
    And the Completion Report is acknowledged so the agent does not retransmit
    And the allocation state remains "Running"

  # ─── Multi-node (a single allocation, many agents) ─────────────────

  Scenario: Multi-node allocation dispatches to every assigned node
    Given a pending bounded allocation requesting 4 nodes
    And 4 Ready nodes each with distinct agent_address
    When the scheduler assigns all 4 nodes to the allocation
    Then the dispatcher performs RunAllocation on each of the 4 agent_addresses
    And each agent spawns a Workload Process
    And the allocation state becomes "Running" only after at least one Completion Report phase "Running" arrives from each node
    And when all 4 nodes report Completion Reports phase "Completed" with exit_code 0, the allocation state is "Completed"

  Scenario: Multi-node allocation fails if any node reports Failed
    Given a multi-node allocation with 3 assigned nodes all Running
    When one node's Completion Report arrives with phase "Failed" and exit_code 137
    Then the allocation state transitions to "Failed"
    And the remaining two nodes receive StopAllocation RPCs
    And node ownership is released for all 3 nodes

  Scenario: Multi-node allocation remains Staging until all nodes report Running (DEC-DISP-11)
    Given a pending bounded allocation requesting 4 nodes
    And 4 Ready nodes each with distinct agent_address
    When the scheduler assigns all 4 nodes to the allocation
    And Nodes 1, 2, 3 report Completion Report phase "Running"
    And Node 4 is still in phase "Prologue"
    Then the allocation state in GlobalState is "Staging"
    When Node 4 reports Completion Report phase "Running"
    Then the allocation state in GlobalState transitions to "Running"

  Scenario: Rollback on multi-node allocation releases all assigned nodes (DEC-DISP-07)
    Given a pending allocation assigned to Nodes 1, 2, 3, 4
    And Nodes 1, 2, 3 accepted RunAllocation and began prologue
    And Node 4's RunAllocation failed after 3 attempts
    When the Dispatcher submits RollbackDispatch
    Then the Raft proposal atomically releases Nodes 1, 2, 3, and 4
    And the allocation state returns to "Pending" with dispatch_retry_count incremented
    And Node 4's consecutive_dispatch_failures is incremented
    And Nodes 1, 2, 3 each receive a fire-and-forget StopAllocation RPC
    And counter lattice_dispatch_rollback_stop_sent_total is incremented 3 times
