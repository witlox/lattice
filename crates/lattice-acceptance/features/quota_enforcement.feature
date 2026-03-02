Feature: Quota Enforcement
  Tenants cannot exceed their resource quotas.

  Scenario: Hard quota rejection on node count
    Given a tenant "physics" with max_nodes 10
    And 8 nodes already allocated to tenant "physics"
    When I submit an allocation requesting 4 nodes for tenant "physics"
    Then the allocation should be rejected with "QuotaExceeded"

  Scenario: Allocation within quota succeeds
    Given a tenant "physics" with max_nodes 10
    And 4 nodes already allocated to tenant "physics"
    When I submit an allocation requesting 4 nodes for tenant "physics"
    Then the allocation should not be rejected

  Scenario: Concurrent allocation limit
    Given a tenant "ml-team" with max_concurrent_allocations 5
    And 5 running allocations for tenant "ml-team"
    When I submit a new allocation for tenant "ml-team"
    Then the allocation should be rejected with "QuotaExceeded"
