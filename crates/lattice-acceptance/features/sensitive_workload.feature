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

  Scenario: Sensitive attach limited to one session at a time
    Given a tenant "hospital-a" with strict isolation
    And a running sensitive allocation owned by user "dr-x"
    And user "dr-x" has an active attach session
    When user "dr-x" attempts a second concurrent attach
    Then the second attach should be denied with "max_sessions_exceeded"

  Scenario: Signed image verification failure rejects allocation
    Given a tenant "hospital-a" with strict isolation
    When I submit a sensitive allocation with an unsigned image
    Then the allocation should be rejected with "unsigned_image"
    And no nodes should be assigned

  Scenario: Encrypted storage pool assigned to sensitive allocation
    Given a tenant "hospital-a" with strict isolation
    And a running sensitive allocation
    Then the allocation should use the encrypted storage pool
    And all data operations should be access-logged

  Scenario: Access logging for all sensitive data operations
    Given a tenant "hospital-a" with strict isolation
    And a running sensitive allocation
    When the allocation reads data from the mounted path
    Then an audit entry should record the data access
    And the audit entry should include the user identity and timestamp

  Scenario: Sensitive node not eligible for elastic borrowing
    Given a tenant "hospital-a" with strict isolation
    And 4 nodes claimed for sensitive allocations
    When vCluster "service" requests to borrow idle sensitive nodes
    Then the borrowing request should be denied
    And sensitive nodes should remain exclusively assigned

  Scenario: Wipe failure quarantines node indefinitely
    Given a tenant "hospital-a" with strict isolation
    And a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When the sensitive wipe operation fails on node "x1000c0s0b0n0"
    Then node "x1000c0s0b0n0" should enter quarantine
    And node "x1000c0s0b0n0" should not be scheduled until operator intervention
    And a critical audit entry should be committed
