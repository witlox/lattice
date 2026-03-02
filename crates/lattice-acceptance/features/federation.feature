Feature: Federation
  Federated scheduling across sites with trust and capacity constraints.

  Scenario: Accept offer from trusted site within capacity
    Given a federation broker for site "site-a"
    And trusted sites "site-b, site-c"
    And 100 total nodes with 20 idle
    When site "site-b" offers 5 nodes for a non-medical workload
    Then the offer should be accepted
    And the accepted nodes should be prefixed with "site-a"

  Scenario: Reject offer from untrusted site
    Given a federation broker for site "site-a"
    And trusted sites "site-b"
    And 100 total nodes with 20 idle
    When site "evil-site" offers 2 nodes for a non-medical workload
    Then the offer should be rejected with reason "not trusted"

  Scenario: Reject medical workload from federation by default
    Given a federation broker for site "site-a"
    And trusted sites "site-b"
    And 100 total nodes with 20 idle
    When site "site-b" offers 2 nodes for a medical workload
    Then the offer should be rejected with reason "medical"

  Scenario: Reject offer exceeding capacity limit
    Given a federation broker for site "site-a"
    And trusted sites "site-b"
    And 100 total nodes with 20 idle
    When site "site-b" offers 25 nodes for a non-medical workload
    Then the offer should be rejected with reason "exceeds"
