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

  Scenario: GPU budget over-usage applies soft penalty not hard rejection
    Given a tenant "ai-team" with gpu_hours_budget 100
    And 120 gpu_hours already consumed by tenant "ai-team"
    When I submit an allocation requesting 8 GPUs with walltime "2h" for tenant "ai-team"
    Then the allocation should not be rejected
    And the allocation should receive a severely reduced scheduling score

  Scenario: Soft quota overshoot scores lower but not rejected
    Given a tenant "physics" with fair_share_target 0.3
    And tenant "physics" currently using 0.5 of cluster resources
    When I submit an allocation for tenant "physics"
    Then the allocation should not be rejected
    And the allocation should receive a lower scheduling score

  Scenario: Quota reduction below current usage blocks new proposals
    Given a tenant "physics" with max_nodes 100
    And 80 nodes already allocated to tenant "physics"
    When the admin reduces tenant "physics" max_nodes to 60
    Then running allocations continue unaffected
    And new allocations for tenant "physics" should be rejected until usage drops below 60

  Scenario: Sensitive pool size limit
    Given a sensitive node pool of size 20
    And 20 nodes already claimed for sensitive allocations
    When a new sensitive node claim is submitted
    Then the claim should be rejected with "QuotaExceeded"

  Scenario: Quota updated via admin API takes effect immediately
    Given a tenant "physics" with max_nodes 10
    When the admin updates tenant "physics" max_nodes to 50 via API
    Then the quota change is Raft-committed
    And subsequent allocations can use up to 50 nodes

  Scenario: Fair share deficit increases scheduling priority
    Given a tenant "starved" with fair_share_target 0.5 and current usage 0.1
    And a tenant "heavy" with fair_share_target 0.5 and current usage 0.9
    When both tenants have pending allocations
    Then tenant "starved" allocations should have higher fair share score
