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

  Scenario: Update tenant quota
    Given a tenant "physics" with max_nodes 50
    When I update tenant "physics" max_nodes to 100
    Then tenant "physics" should have max_nodes 100
    And the quota change should take effect immediately

  Scenario: Delete tenant with no active allocations
    Given a tenant "temp-project" with max_nodes 10
    And no active allocations for tenant "temp-project"
    When I delete tenant "temp-project"
    Then tenant "temp-project" should no longer exist

  Scenario: Delete tenant with active allocations rejected
    Given a tenant "active-project" with max_nodes 50
    And 2 running allocations for tenant "active-project"
    When I attempt to delete tenant "active-project"
    Then the deletion should be rejected with "active_allocations_exist"

  Scenario: Tenant with GPU quota
    Given a tenant "ai-team" with max_nodes 50 and gpu_hours_budget 1000
    Then tenant "ai-team" should have gpu_hours_budget 1000
    And GPU usage should be tracked against the budget

  Scenario: Tenant with sensitive isolation mode
    Given a tenant "hospital" with max_nodes 50
    And the tenant "hospital" has strict isolation
    Then tenant "hospital" should require signed images
    And tenant "hospital" should require audit logging
    And tenant "hospital" should require encrypted storage

  Scenario: Multiple vClusters per tenant
    Given a tenant "research" with max_nodes 100
    When I create vCluster "batch" for tenant "research" with scheduler "HpcBackfill"
    And I create vCluster "interactive" for tenant "research" with scheduler "InteractiveFifo"
    Then tenant "research" should have 2 vClusters
    And both vClusters share the tenant quota
