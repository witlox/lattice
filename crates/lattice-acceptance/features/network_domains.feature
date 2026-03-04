Feature: Network Domains
  Allocations sharing a network domain get L3 reachability.

  Scenario: DAG allocations share network domain
    Given a tenant "ml-team" with max nodes 100
    And a vCluster "train-vc" for tenant "ml-team" with scheduler "hpc_backfill"
    And 8 nodes in group 0
    When a DAG is submitted with 2 allocations requiring shared networking
    Then both allocations should be assigned the same network domain

  Scenario: Independent allocations get separate domains
    Given a tenant "multi" with max nodes 100
    And a vCluster "batch-vc" for tenant "multi" with scheduler "hpc_backfill"
    And 8 nodes in group 0
    When two independent allocations are submitted
    Then each allocation should have its own network domain

  Scenario: Network domain released after allocation completes
    Given a tenant "physics" with max nodes 100
    And a vCluster "sim-vc" for tenant "physics" with scheduler "hpc_backfill"
    And 4 nodes in group 0
    When an allocation with a network domain completes
    Then the network domain should transition to Released
