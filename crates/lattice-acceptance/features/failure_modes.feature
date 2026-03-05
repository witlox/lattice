Feature: Failure Modes
  As a system, I handle failures gracefully without losing state.

  Scenario: Node failure marks allocations as failed
    Given a tenant "research" with a quota of 100 nodes
    And 4 ready nodes in group 0
    And a running allocation on node "x1000c0s0b0n0"
    When node "x1000c0s0b0n0" transitions to down with reason "hardware fault"
    Then node "x1000c0s0b0n0" should not be operational
    And the allocation on the failed node should be marked "Failed"

  Scenario: Draining node completes running allocations before draining
    Given a tenant "research" with a quota of 100 nodes
    And 4 ready nodes in group 0
    And a running allocation on node "x1000c0s0b0n1"
    When node "x1000c0s0b0n1" begins draining
    Then node "x1000c0s0b0n1" should be in "Draining" state
    And new allocations should not be placed on "x1000c0s0b0n1"

  Scenario: Degraded node remains operational
    Given 4 ready nodes in group 0
    When node "x1000c0s0b0n2" becomes degraded with reason "slow disk"
    Then node "x1000c0s0b0n2" should still be operational
    And the degradation reason should be "slow disk"
