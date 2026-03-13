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

  Scenario: Multi-GPU NVLink group awareness
    Given a tenant "ai-team" with max nodes 100
    And a vCluster "train-vc" for tenant "ai-team" with scheduler "hpc_backfill"
    And 4 nodes with NVLink-connected GPU pairs in group 0
    And 4 nodes with PCIe-only GPUs in group 1
    When an allocation requiring multi-GPU communication is submitted
    Then the allocation should prefer NVLink-connected nodes

  Scenario: PCIe fallback when NVLink topology unavailable
    Given a tenant "ai-team" with max nodes 100
    And a vCluster "train-vc" for tenant "ai-team" with scheduler "hpc_backfill"
    And 4 nodes with PCIe-only GPUs in group 0
    When an allocation requesting GPUs is submitted
    Then the allocation should be placed using PCIe topology
    And no NVLink preference should be applied

  Scenario: No GPU preference gets any available node
    Given a tenant "ai-team" with max nodes 100
    And a vCluster "train-vc" for tenant "ai-team" with scheduler "hpc_backfill"
    And 4 nodes with GPU type "GH200" in group 0
    And 4 nodes with GPU type "MI300X" in group 1
    When an allocation without GPU type preference is submitted
    Then the allocation may be placed on either GPU type

  Scenario: GPU memory constraint filters nodes
    Given a tenant "ai-team" with max nodes 100
    And a vCluster "train-vc" for tenant "ai-team" with scheduler "hpc_backfill"
    And 4 nodes with 40GB GPU memory in group 0
    And 4 nodes with 80GB GPU memory in group 1
    When an allocation requiring 80GB GPU memory is submitted
    Then the allocation should be placed on 80GB GPU nodes only

  Scenario: Topology common ancestor packing for multi-node
    Given a tenant "ai-team" with max nodes 100
    And a vCluster "train-vc" for tenant "ai-team" with scheduler "hpc_backfill"
    And 4 nodes sharing a PCIe switch in group 0
    And 4 nodes in a different switch group in group 1
    When an allocation requiring 3 nodes is submitted
    Then the allocation should prefer nodes sharing the common PCIe ancestor

  Scenario: No-GPU allocation ignores GPU topology
    Given a tenant "hpc-team" with max nodes 100
    And a vCluster "cpu-vc" for tenant "hpc-team" with scheduler "hpc_backfill"
    And 4 nodes with GPU type "GH200" in group 0
    And 4 nodes without GPUs in group 1
    When an allocation requiring 0 GPUs is submitted
    Then GPU topology should not influence placement
