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
