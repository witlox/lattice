Feature: Checkpoint Coordination
  End-to-end test: checkpoint broker evaluates and signals.

  Scenario: Running allocation checkpointed under backlog pressure
    Given a running allocation with 4 nodes and 2 hours elapsed
    And a system backlog of 0.8
    When the checkpoint broker evaluates the allocation
    Then the allocation should be checkpointed

  Scenario: Idle system skips checkpoint
    Given a running allocation with 4 nodes and 30 minutes elapsed
    And a system backlog of 0.1
    When the checkpoint broker evaluates the allocation
    Then the allocation should not be checkpointed

  Scenario: Checkpoint cost must exceed value threshold
    Given a running allocation with 2 nodes and 10 minutes elapsed
    And a system backlog of 0.5
    And the checkpoint write cost exceeds the recompute savings
    When the checkpoint broker evaluates the allocation
    Then the allocation should not be checkpointed

  Scenario: Checkpoint via signal protocol
    Given a running allocation with checkpoint protocol "signal"
    And a system backlog of 0.8
    When the checkpoint broker initiates a checkpoint
    Then SIGUSR1 should be sent to the application process
    And the allocation should transition to "Checkpointing"

  Scenario: Checkpoint via gRPC callback protocol
    Given a running allocation with checkpoint protocol "grpc"
    And a system backlog of 0.8
    When the checkpoint broker initiates a checkpoint
    Then the gRPC checkpoint callback should be invoked
    And the allocation should transition to "Checkpointing"

  Scenario: DMTCP transparent checkpoint fallback
    Given a running allocation with checkpoint protocol "dmtcp"
    And a system backlog of 0.7
    When the checkpoint broker initiates a checkpoint
    Then DMTCP coordinator should receive the checkpoint command
    And the allocation should transition to "Checkpointing"

  Scenario: Checkpoint timeout with extension for slow application
    Given a running allocation in "Checkpointing" state
    And the checkpoint timeout is 10 minutes
    When the checkpoint does not complete within 10 minutes
    And the application is still responsive
    Then the timeout is extended by 50% to 15 minutes
    And the allocation remains in "Checkpointing"

  Scenario: Checkpoint timeout without extension forces kill
    Given a running allocation in "Checkpointing" state
    And the checkpoint timeout is 10 minutes
    When the checkpoint does not complete within 10 minutes
    And the application is unresponsive
    Then SIGTERM is sent followed by SIGKILL
    And the allocation transitions to "Failed"

  Scenario: Partial checkpoint failure across nodes
    Given a running allocation with 4 nodes in "Checkpointing" state
    When 3 nodes complete checkpoint successfully
    And 1 node fails to checkpoint
    Then the checkpoint is marked as partial failure
    And the allocation transitions to "Failed" not "Suspended"

  Scenario: Non-preemptible allocation skips checkpoint entirely
    Given a running allocation with non-preemptible flag set
    And a system backlog of 0.9
    When the checkpoint broker evaluates the allocation
    Then no checkpoint hint is sent
    And the allocation continues running

  Scenario: Broker evaluates all running allocations each cycle
    Given 3 running allocations with different elapsed times
    And a system backlog of 0.6
    When the checkpoint broker runs an evaluation cycle
    Then all 3 allocations are evaluated against the cost model
    And only allocations where value exceeds cost are checkpointed

  Scenario: High backlog increases checkpoint aggressiveness
    Given a running allocation with 4 nodes and 30 minutes elapsed
    And a system backlog of 0.95
    When the checkpoint broker evaluates the allocation
    Then the allocation should be checkpointed
    And the checkpoint threshold should be lower than at normal backlog
