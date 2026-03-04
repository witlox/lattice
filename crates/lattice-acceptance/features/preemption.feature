Feature: Preemption
  Higher priority allocations can preempt lower priority ones.

  Scenario: High priority preempts low priority
    Given a tenant "urgent" with max nodes 100
    And a vCluster "urgent-vc" for tenant "urgent" with scheduler "hpc_backfill"
    And 4 nodes in group 0 all running low-priority allocations
    When a high-priority allocation is submitted requiring 2 nodes
    Then preemption should be evaluated
    And the preemption result should identify victims

  Scenario: Sensitive allocations are never preempted
    Given a tenant "med-team" with max nodes 100
    And a vCluster "med-vc" for tenant "med-team" with scheduler "sensitive_reservation"
    And 4 nodes running sensitive allocations
    When a high-priority non-sensitive allocation needs nodes
    Then no sensitive allocations should be selected as victims

  Scenario: Preemption respects checkpoint cost
    Given a tenant "ml-team" with max nodes 100
    And a vCluster "ml-vc" for tenant "ml-team" with scheduler "hpc_backfill"
    And 4 nodes running allocations with different checkpoint costs
    When preemption is evaluated
    Then allocations with lower checkpoint cost should be preferred as victims
