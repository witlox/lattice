Feature: Scheduling Cycle
  End-to-end test: submit → schedule → running → completed.

  Scenario: Single allocation scheduled and completed
    Given a tenant "physics" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 10 ready nodes in group 0
    When I submit a bounded allocation requesting 2 nodes with walltime "1h"
    And the scheduler runs a cycle
    Then the allocation should be placed on 2 nodes
    And the allocation state should be "Running"

  Scenario: Higher priority allocation scheduled first
    Given a tenant "ml" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 4 ready nodes in group 0
    When I submit a low-priority allocation requesting 2 nodes
    And I submit a high-priority allocation requesting 2 nodes
    And the scheduler runs a cycle
    Then the high-priority allocation should be "Running"

  Scenario: Allocation deferred when no resources available
    Given a tenant "physics" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 2 ready nodes in group 0
    When I submit a bounded allocation requesting 4 nodes with walltime "1h"
    And the scheduler runs a cycle
    Then the allocation state should be "Pending"
