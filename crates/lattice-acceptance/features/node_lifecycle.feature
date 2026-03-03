Feature: Node Lifecycle
  Nodes can be drained, undrained, and claimed for tenants.

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
