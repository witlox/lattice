Feature: Memory Topology
  Lattice discovers and uses NUMA/CXL/unified memory topology for scheduling.

  Scenario: Unified memory nodes preferred for GPU workloads
    Given a tenant "ai-team" with max nodes 100
    And a vCluster "gpu-train" for tenant "ai-team" with scheduler "hpc_backfill"
    And 4 nodes with unified memory in group 0
    And 4 nodes with NUMA memory in group 1
    When an allocation is submitted requiring unified memory
    Then the allocation should be placed on unified memory nodes

  Scenario: CXL memory nodes rejected when not allowed
    Given a tenant "hpc-team" with max nodes 100
    And a vCluster "hpc-batch" for tenant "hpc-team" with scheduler "hpc_backfill"
    And 4 nodes with CXL memory domains in group 0
    And 4 nodes with standard DRAM in group 1
    When an allocation is submitted with allow_cxl_memory false
    Then the allocation should be placed on DRAM-only nodes

  Scenario: Memory locality score influences placement
    Given a tenant "ml-team" with max nodes 100
    And a vCluster "ml-vc" for tenant "ml-team" with scheduler "hpc_backfill"
    And 4 nodes with single NUMA domain in group 0
    And 4 nodes with 4 NUMA domains in group 1
    When an allocation is submitted preferring same NUMA
    Then nodes with fewer NUMA domains should score higher
