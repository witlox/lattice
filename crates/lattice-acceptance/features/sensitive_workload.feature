Feature: Sensitive Workload Isolation
  Sensitive allocations require strict node claiming, audit logging, and no sharing.

  Scenario: Sensitive node claim records audit entry
    Given a tenant "hospital-a" with strict isolation
    And 4 ready nodes with conformance fingerprint "sensitive-baseline-v1"
    When user "dr-x" claims node 0 for a sensitive allocation
    Then node 0 should be owned by user "dr-x"
    And an audit entry should record action "NodeClaim"

  Scenario: Concurrent sensitive claims conflict
    Given a tenant "hospital-a" with strict isolation
    And 4 ready nodes with conformance fingerprint "sensitive-baseline-v1"
    When user "dr-x" claims node 0 for a sensitive allocation
    And user "dr-y" attempts to claim node 0
    Then user "dr-y" receives an OwnershipConflict error

  Scenario: Sensitive allocation requires signed images
    Given a tenant "hospital-a" with strict isolation
    When I submit a sensitive allocation
    Then the allocation environment should require signed images
    And the allocation environment should require vulnerability scanning

  Scenario: Sensitive epilogue triggers wipe on release
    Given a tenant "hospital-a" with strict isolation
    And a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When I run the sensitive epilogue for an allocation
    Then the sensitive wipe should have been triggered
    And the epilogue cleanup should have completed

  Scenario: Non-sensitive epilogue skips wipe
    Given a tenant "physics" with a quota of 100 nodes
    And a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When I run the standard epilogue for an allocation
    Then the sensitive wipe should not have been triggered
    And the epilogue cleanup should have completed
