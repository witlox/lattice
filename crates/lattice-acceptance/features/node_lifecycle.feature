Feature: Node Lifecycle
  Nodes can be drained, undrained, disabled, enabled, and claimed for tenants.

  Scenario: Drain transitions node to Draining
    Given 5 ready nodes in group 0
    When I drain node 0
    Then node 0 should be in state "Draining"
    And nodes 1 through 4 should be in state "Ready"

  Scenario: Undrain returns node to Ready
    Given 5 ready nodes in group 0
    When I drain node 0
    And I undrain node 0
    Then node 0 should be in state "Ready"

  Scenario: Claiming records ownership and rejects double-claim
    Given 5 ready nodes in group 0
    And a tenant "med-team" with a quota of 100 nodes
    When user "dr-smith" claims node 0 for tenant "med-team"
    Then node 0 should be owned by user "dr-smith"
    When user "dr-jones" attempts to claim node 0
    Then user "dr-jones" receives an OwnershipConflict error

  Scenario: Node registration on first heartbeat
    Given a new node "x1000c0s0b0n5" not yet registered
    When the node agent sends its first heartbeat
    Then node "x1000c0s0b0n5" should be registered
    And node "x1000c0s0b0n5" should be in state "Ready"

  Scenario: Degraded to Down after grace period
    Given 4 ready nodes in group 0
    When node "x1000c0s0b0n0" misses heartbeats
    Then node "x1000c0s0b0n0" should transition to "Degraded"
    When the grace period expires without recovery
    Then node "x1000c0s0b0n0" should transition to "Down"

  Scenario: Down node recovery via agent restart
    Given a node "x1000c0s0b0n0" in "Down" state
    When the node agent restarts and sends a heartbeat
    Then node "x1000c0s0b0n0" should transition to "Ready"
    And the node should be available for scheduling

  Scenario: Sensitive wipe failure quarantines node
    Given a node "x1000c0s0b0n0" undergoing sensitive wipe
    When the wipe operation fails
    Then node "x1000c0s0b0n0" should remain in "Down" state
    And node "x1000c0s0b0n0" should not return to the scheduling pool
    And an operator alert should be raised

  Scenario: Heartbeat sequence rejects stale messages
    Given a node "x1000c0s0b0n0" with heartbeat sequence at 10
    When a heartbeat with sequence 8 is received
    Then the stale heartbeat should be rejected
    And the node state should not change

  Scenario: Heartbeat timeout triggers degraded transition
    Given 4 ready nodes in group 0
    And a heartbeat timeout of 30 seconds
    When node "x1000c0s0b0n0" sends no heartbeat for 30 seconds
    Then node "x1000c0s0b0n0" should transition to "Degraded"

  Scenario: Enable transitions disabled node from Down to Ready
    Given 5 ready nodes in group 0
    When I disable node 0 with reason "maintenance"
    Then node 0 should be in state "Down"
    When I enable node 0
    Then node 0 should be in state "Ready"
    And node 0 should be available for scheduling

  Scenario: Enable on non-disabled node fails
    Given 5 ready nodes in group 0
    When I attempt to enable node 0
    Then the operation should fail with an invalid transition error

  Scenario: Multiple nodes drained simultaneously
    Given 5 ready nodes in group 0
    When I drain node 0 and node 1 simultaneously
    Then node 0 should be in state "Draining"
    And node 1 should be in state "Draining"
    And nodes 2 through 4 should be in state "Ready"
