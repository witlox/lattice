Feature: Federation
  Federated scheduling across sites with trust and capacity constraints.

  Scenario: Accept offer from trusted site within capacity
    Given a federation broker for site "site-a"
    And trusted sites "site-b, site-c"
    And 100 total nodes with 20 idle
    When site "site-b" offers 5 nodes for a non-sensitive workload
    Then the offer should be accepted
    And the accepted nodes should be prefixed with "site-a"

  Scenario: Reject offer from untrusted site
    Given a federation broker for site "site-a"
    And trusted sites "site-b"
    And 100 total nodes with 20 idle
    When site "evil-site" offers 2 nodes for a non-sensitive workload
    Then the offer should be rejected with reason "not trusted"

  Scenario: Reject sensitive workload from federation by default
    Given a federation broker for site "site-a"
    And trusted sites "site-b"
    And 100 total nodes with 20 idle
    When site "site-b" offers 2 nodes for a sensitive workload
    Then the offer should be rejected with reason "sensitive"

  Scenario: Reject offer exceeding capacity limit
    Given a federation broker for site "site-a"
    And trusted sites "site-b"
    And 100 total nodes with 20 idle
    When site "site-b" offers 25 nodes for a non-sensitive workload
    Then the offer should be rejected with reason "exceeds"

  Scenario: Federation disabled by feature flag
    Given federation is disabled via feature flag
    When site "site-b" sends an allocation request
    Then the request should be rejected with reason "federation_disabled"

  Scenario: Federation broker unavailable does not affect local scheduling
    Given a federation broker for site "site-a" that is unavailable
    And 10 ready nodes in group 0
    And a pending local allocation
    When the scheduler runs a cycle
    Then the local allocation should be scheduled normally

  Scenario: Request deduplication via request ID
    Given a federation broker for site "site-a"
    And trusted sites "site-b"
    And 100 total nodes with 20 idle
    When site "site-b" sends the same request ID twice
    Then the second request should return the existing result
    And no duplicate allocation should be created

  Scenario: Sovra token validation failure
    Given a federation broker for site "site-a"
    And trusted sites "site-b"
    When site "site-b" sends a request with an invalid Sovra token
    Then the request should be rejected with status 403

  Scenario: Remote site unreachable with retry
    Given a federation broker for site "site-a"
    And trusted sites "site-b"
    When site "site-a" attempts to forward a request to unreachable site "site-b"
    Then the broker retries 3 times with backoff
    And after all retries fail the request is rejected with reason "remote_unreachable"

  Scenario: Federation request during leader election returns retry-after
    Given a federation broker for site "site-a"
    And the local quorum is undergoing leader election
    When site "site-b" sends an allocation request
    Then the broker returns status 503 with "Retry-After" header
