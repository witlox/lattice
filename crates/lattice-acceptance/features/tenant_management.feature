Feature: Tenant Management
  Tenants have quotas that control resource allocation.

  Scenario: Create tenant with quota and isolation
    Given a tenant "radiology" with max_nodes 50
    And the tenant "radiology" has strict isolation
    Then tenant "radiology" should have max_nodes 50
    And tenant "radiology" should have strict isolation

  Scenario: Quota limits concurrency
    Given a tenant "limited" with max_concurrent_allocations 2
    And 2 running allocations for tenant "limited"
    When I submit a new allocation for tenant "limited"
    Then the allocation should be rejected with "QuotaExceeded"
