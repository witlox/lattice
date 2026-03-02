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
