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

  Scenario: Session list filtered by tenant
    Given a tenant "team-a" with a quota of 100 nodes
    And a tenant "team-b" with a quota of 100 nodes
    And 10 ready nodes in group 0
    When I create an interactive session for tenant "team-a"
    And I create an interactive session for tenant "team-b"
    Then listing sessions for tenant "team-a" should return 1 session
    And listing sessions for tenant "team-b" should return 1 session

  Scenario: Max concurrent sessions per user enforced
    Given a tenant "hpc-users" with a quota of 100 nodes
    And a max of 3 concurrent sessions per user
    And user "alice" has 3 active sessions
    When user "alice" attempts to create another session
    Then the session creation should be rejected with "max_sessions_exceeded"

  Scenario: Session reattach after disconnect
    Given a tenant "hpc-users" with a quota of 100 nodes
    And 10 ready nodes in group 0
    When I create an interactive session for tenant "hpc-users"
    And the session transitions to "Running"
    And the client disconnects
    Then the session should remain running
    When the client reattaches to the session
    Then the attach should succeed

  Scenario: Multiple sessions on different nodes
    Given a tenant "hpc-users" with a quota of 100 nodes
    And 10 ready nodes in group 0
    When I create 2 interactive sessions for tenant "hpc-users"
    Then both sessions should have separate allocations
    And each session should be on a different node
