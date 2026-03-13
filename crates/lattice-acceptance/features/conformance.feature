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

  Scenario: Conformance fingerprint includes OS and driver versions
    Given a node with OS "cos-2.0", GPU driver "535.104", and libraries "prgenv-gnu/24.11"
    When the conformance fingerprint is computed
    Then the fingerprint should encode OS, driver, and library versions
    And two nodes with identical software stacks should have the same fingerprint

  Scenario: Conformance drift detected via heartbeat
    Given a node "x1000c0s0b0n0" with conformance fingerprint "abc123"
    When the node reports a new fingerprint "abc456" via heartbeat
    Then a conformance drift alert should be raised
    And the node's fingerprint should be updated to "abc456"

  Scenario: Sensitive allocation requires exact conformance match
    Given a tenant "hospital" with strict isolation
    And a vCluster "sens-vc" for tenant "hospital" with scheduler "sensitive_reservation"
    And 4 nodes with conformance fingerprint "sensitive-baseline-v1"
    And 4 nodes with conformance fingerprint "standard-v1"
    When a sensitive allocation requiring 2 nodes is submitted
    Then all assigned nodes must have fingerprint "sensitive-baseline-v1"

  Scenario: Large allocation spans conformance groups when necessary
    Given a tenant "hpc" with max nodes 100
    And a vCluster "hpc-vc" for tenant "hpc" with scheduler "hpc_backfill"
    And 3 nodes with conformance "a" in group 0
    And 3 nodes with conformance "b" in group 1
    When an allocation requiring 5 nodes is submitted
    Then the allocation spans both conformance groups
    And the conformance fitness score is penalized

  Scenario: Conformance score f9 influences placement cost
    Given a tenant "hpc" with max nodes 100
    And a vCluster "hpc-vc" for tenant "hpc" with scheduler "hpc_backfill"
    And 4 nodes with conformance "a" in group 0
    And 4 nodes with conformance "b" in group 1
    When an allocation requiring 2 nodes is submitted
    Then the placement on homogeneous conformance nodes should score higher
    And the conformance fitness factor f9 should contribute to the total cost

  Scenario: Node reimage restores conformance fingerprint
    Given a node "x1000c0s0b0n0" with drifted conformance fingerprint "abc456"
    When the node is reimaged via OpenCHAMI
    And the node reports its new fingerprint via heartbeat
    Then the fingerprint should match the baseline "abc123"
    And no conformance drift alert should be raised
