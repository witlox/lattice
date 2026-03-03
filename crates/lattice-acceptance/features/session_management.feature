Feature: Session Management
  Interactive sessions are unbounded allocations with session metadata.

  Scenario: Create and retrieve interactive session
    Given a tenant "hpc-users" with a quota of 100 nodes
    And 10 ready nodes in group 0
    When I create an interactive session for tenant "hpc-users"
    Then the session allocation should be "Pending"
    And the session should have an unbounded lifecycle
    And the session should have a session tag

  Scenario: Delete session cancels allocation
    Given a tenant "hpc-users" with a quota of 100 nodes
    And 10 ready nodes in group 0
    When I create an interactive session for tenant "hpc-users"
    And the session transitions to "Running"
    And I delete the session
    Then the session allocation should be "Cancelled"

  Scenario: Session allocation is unbounded
    Given a tenant "hpc-users" with a quota of 100 nodes
    When I create an interactive session for tenant "hpc-users"
    Then the session should have an unbounded lifecycle
    And the session walltime should be zero
