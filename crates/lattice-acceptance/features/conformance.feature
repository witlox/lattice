Feature: Conformance
  Nodes are grouped by conformance fingerprint for homogeneous allocation placement.

  Scenario: Allocations placed on conformant nodes
    Given a tenant "hpc" with max nodes 100
    And a vCluster "hpc-vc" for tenant "hpc" with scheduler "hpc_backfill"
    And 4 nodes with conformance fingerprint "abc123" in group 0
    And 4 nodes with conformance fingerprint "def456" in group 1
    When an allocation requiring 3 nodes is submitted
    Then all assigned nodes should share the same conformance fingerprint

  Scenario: Mixed conformance falls back to topology
    Given a tenant "hpc" with max nodes 100
    And a vCluster "hpc-vc" for tenant "hpc" with scheduler "hpc_backfill"
    And 2 nodes with conformance "a" in group 0
    And 2 nodes with conformance "b" in group 0
    And 2 nodes with conformance "c" in group 1
    When an allocation requiring 4 nodes is submitted
    Then the allocation should still be placed using topology-aware selection
