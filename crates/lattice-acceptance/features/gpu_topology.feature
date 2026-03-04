Feature: GPU Topology
  Scheduler considers GPU topology for placement decisions.

  Scenario: GPU type constraint filters nodes
    Given a tenant "ai-team" with max nodes 100
    And a vCluster "train-vc" for tenant "ai-team" with scheduler "hpc_backfill"
    And 4 nodes with GPU type "GH200" in group 0
    And 4 nodes with GPU type "MI300X" in group 1
    When an allocation requiring GPU type "GH200" is submitted
    Then the allocation should be placed on GH200 nodes only

  Scenario: GPU count constraint respected
    Given a tenant "ai-team" with max nodes 100
    And a vCluster "train-vc" for tenant "ai-team" with scheduler "hpc_backfill"
    And 4 nodes with 4 GPUs each in group 0
    And 4 nodes with 8 GPUs each in group 1
    When an allocation requiring 8 GPUs per node is submitted
    Then the allocation should be placed on 8-GPU nodes
