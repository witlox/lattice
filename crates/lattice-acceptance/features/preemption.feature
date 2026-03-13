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

  Scenario: Preemption class ordering enforced
    Given a tenant "research" with max nodes 100
    And a vCluster "hpc-vc" for tenant "research" with scheduler "hpc_backfill"
    And 4 nodes running class-5 allocations
    When a class-4 allocation needs those nodes
    Then no preemption should occur
    And the class-4 allocation remains pending

  Scenario: Higher class preempts lower class
    Given a tenant "research" with max nodes 100
    And a vCluster "hpc-vc" for tenant "research" with scheduler "hpc_backfill"
    And 4 nodes running class-3 allocations
    When a class-6 allocation needs those nodes
    Then preemption should be evaluated
    And the class-3 allocations should be selected as victims

  Scenario: Gang preemption frees all nodes of victim together
    Given a tenant "ml-team" with max nodes 100
    And a vCluster "ml-vc" for tenant "ml-team" with scheduler "hpc_backfill"
    And a running allocation "victim-1" spanning 4 nodes
    When preemption selects "victim-1" as a victim
    Then all 4 nodes of "victim-1" are freed together
    And "victim-1" is not partially preempted

  Scenario: Preemption cascade triggers new evaluation cycle
    Given 8 nodes all running low-priority allocations
    And a high-priority allocation needs 6 nodes
    When preemption frees 4 nodes from one victim
    Then the scheduler re-evaluates with newly freed nodes
    And additional victims may be selected to satisfy the requirement

  Scenario: Preemption with successful checkpoint transitions to Suspended
    Given a running allocation with checkpoint enabled
    When preemption is initiated and checkpoint completes
    Then the allocation transitions to "Suspended"
    And the allocation re-enters the queue with original submission time

  Scenario: Preemption without checkpoint transitions to Failed
    Given a running allocation with checkpoint protocol "none"
    When preemption is initiated
    Then the allocation transitions to "Failed"
    And the allocation is not requeued

  Scenario: Equal preemption class favors newer allocation as victim
    Given a tenant "research" with max nodes 100
    And a vCluster "hpc-vc" for tenant "research" with scheduler "hpc_backfill"
    And 2 class-3 allocations running with different submission times
    When a class-8 allocation needs nodes
    Then the more recently submitted class-3 allocation is preempted first

  Scenario: Non-preemptible flag blocks preemption entirely
    Given a tenant "critical" with max nodes 100
    And a vCluster "crit-vc" for tenant "critical" with scheduler "hpc_backfill"
    And 4 nodes running non-preemptible allocations
    When a high-priority allocation needs those nodes
    Then no non-preemptible allocations should be selected as victims

  Scenario: Multiple victims selected to free enough nodes
    Given a tenant "research" with max nodes 100
    And a vCluster "hpc-vc" for tenant "research" with scheduler "hpc_backfill"
    And 8 nodes running 4 allocations of 2 nodes each at class-2
    When a class-7 allocation needs 6 nodes
    Then 3 allocations should be selected as victims
    And exactly 6 nodes should be freed
