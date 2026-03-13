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

  Scenario: Local memory policy generates numactl bind arguments
    Given a node with 4 NUMA domains
    And an allocation with memory policy "Local"
    When the prologue generates numactl arguments
    Then the arguments should include "--membind" for the assigned NUMA domain

  Scenario: Interleave policy distributes memory across domains
    Given a node with 4 NUMA domains
    And an allocation with memory policy "Interleave"
    When the prologue generates numactl arguments
    Then the arguments should include "--interleave=all"

  Scenario: GH200 superchip detected as unified memory
    Given a node with GH200 superchip architecture
    When memory topology is discovered
    Then the node should report unified memory
    And no NUMA pinning should be required

  Scenario: CXL memory tier scored lower than local DRAM
    Given a tenant "hpc-team" with max nodes 100
    And a vCluster "hpc-vc" for tenant "hpc-team" with scheduler "hpc_backfill"
    And 4 nodes with CXL-attached memory in group 0
    And 4 nodes with local DRAM only in group 1
    When an allocation is submitted without CXL preference
    Then local DRAM nodes should score higher for memory locality

  Scenario: Memory topology reported via node heartbeat
    Given a node agent for node "x1000c0s0b0n0" with 4 NUMA domains
    When the agent sends a heartbeat
    Then the heartbeat should include memory topology information
    And the topology should report domain count and interconnect types
