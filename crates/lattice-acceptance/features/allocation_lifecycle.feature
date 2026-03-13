Feature: Allocation Lifecycle
  As a user, I submit allocations and they progress through states correctly.

  Scenario: Submit a bounded allocation
    Given a tenant "physics" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 10 ready nodes in group 0
    When I submit a bounded allocation requesting 4 nodes with walltime "2h"
    Then the allocation state should be "Pending"
    And the allocation should have a valid ID

  Scenario: Allocation transitions from Pending to Running
    Given a pending allocation requesting 2 nodes
    And 4 ready nodes in group 0
    When the allocation transitions to "Running"
    Then the allocation state should be "Running"

  Scenario: Running allocation completes
    Given a running allocation
    When the allocation transitions to "Completed"
    Then the allocation state should be "Completed"

  Scenario: Terminal states block further transitions
    Given a completed allocation
    Then the allocation cannot transition to "Running"
    And the allocation cannot transition to "Pending"
    And the allocation cannot transition to "Staging"

  Scenario: Suspended allocation can be requeued
    Given a suspended allocation
    When the allocation transitions to "Pending"
    Then the allocation state should be "Pending"

  Scenario: Cancel from Pending state
    Given a tenant "physics" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And a pending allocation requesting 4 nodes
    When the user cancels the allocation
    Then the allocation state should be "Cancelled"
    And no nodes should be assigned

  Scenario: Cancel from Running state
    Given a running allocation
    When the user cancels the allocation
    Then the allocation state should be "Cancelled"
    And assigned nodes should be released

  Scenario: Failed state from application crash
    Given a running allocation
    When the application process exits with non-zero status
    Then the allocation state should be "Failed"
    And assigned nodes should be released

  Scenario: Staging state during prologue
    Given a pending allocation requesting 2 nodes
    And 4 ready nodes in group 0
    When the allocation begins prologue
    Then the allocation state should be "Staging"
    When the prologue completes
    Then the allocation state should be "Running"

  Scenario: Checkpointing state during preemption
    Given a running allocation with checkpoint enabled
    When a preemption checkpoint is initiated
    Then the allocation state should be "Checkpointing"
    When the checkpoint completes
    Then the allocation state should be "Suspended"

  Scenario: Unbounded lifecycle has no walltime
    Given a tenant "services" with a quota of 100 nodes
    And a vCluster "service-vc" with scheduler "ServiceBinPack"
    When I submit an unbounded allocation requesting 2 nodes
    Then the allocation should have an unbounded lifecycle
    And the allocation walltime should be zero
    And the allocation does not expire automatically

  Scenario: Reactive lifecycle has min and max nodes
    Given a tenant "inference" with a quota of 100 nodes
    When I submit a reactive allocation with min_nodes 2 and max_nodes 8
    Then the allocation should have a reactive lifecycle
    And the allocation min_nodes should be 2
    And the allocation max_nodes should be 8

  Scenario: Task group creates multiple allocations
    Given a tenant "physics" with a quota of 100 nodes
    When I submit a task group with 5 tasks requesting 1 node each
    Then 5 allocations should be created
    And all allocations should share the same task_group_id

  Scenario: Requeue after node failure with requeue policy
    Given a running allocation with requeue policy "on_node_failure"
    When the assigned node transitions to down
    Then the allocation state should be "Pending"
    And the requeue count should be 1

  Scenario: Requeue limit exceeded becomes permanent failure
    Given a running allocation with requeue policy "always" and max_requeue 3
    And the allocation has been requeued 3 times
    When the application crashes again
    Then the allocation state should be "Failed"
    And the failure reason should include "max_requeue_exceeded"

  Scenario: Invalid transition Pending to Completed rejected
    Given a pending allocation requesting 2 nodes
    Then the allocation cannot transition to "Completed"
    And the allocation cannot transition to "Suspended"
