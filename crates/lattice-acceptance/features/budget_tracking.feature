Feature: Internal Budget Tracking
  GPU-hours budget enforcement works without Waldur by computing usage
  from allocation history stored in the quorum. The budget penalty in
  the cost function uses this data to deprioritize over-budget tenants.

  Background:
    Given the system is running without Waldur integration
    And budget tracking uses a configurable period (default 90 days)

  # ── Internal usage computation ──

  Scenario: GPU-hours computed from completed allocations
    Given a tenant "physics" with gpu_hours_budget 1000
    And a completed allocation for tenant "physics" that ran for 10 hours on 4 nodes with 4 GPUs each
    Then tenant "physics" should have 160 gpu_hours_used in the current period

  Scenario: GPU-hours includes running allocations
    Given a tenant "physics" with gpu_hours_budget 1000
    And a running allocation for tenant "physics" started 5 hours ago on 2 nodes with 4 GPUs each
    Then tenant "physics" should have at least 40 gpu_hours_used in the current period

  Scenario: GPU-hours only counts current budget period
    Given a tenant "physics" with gpu_hours_budget 1000 and budget_period_days 90
    And a completed allocation for tenant "physics" that finished 100 days ago using 500 gpu_hours
    And a completed allocation for tenant "physics" that finished 10 days ago using 200 gpu_hours
    Then tenant "physics" should have 200 gpu_hours_used in the current period

  Scenario: GPU-hours unlimited when no budget set
    Given a tenant "physics" with no gpu_hours_budget
    Then tenant "physics" should have budget_utilization fraction_used 0.0

  # ── Budget penalty wiring ──

  Scenario: Budget utilization fed to cost function
    Given a tenant "physics" with gpu_hours_budget 1000
    And 850 gpu_hours consumed in the current period
    When a scheduling cycle runs
    Then the budget_utilization for tenant "physics" should have fraction_used 0.85
    And the budget penalty should reduce the tenant's scheduling score

  Scenario: Under 80% budget has no penalty
    Given a tenant "physics" with gpu_hours_budget 1000
    And 500 gpu_hours consumed in the current period
    When a scheduling cycle runs
    Then the budget penalty multiplier for tenant "physics" should be 1.0

  Scenario: Over 100% budget has severe penalty
    Given a tenant "physics" with gpu_hours_budget 1000
    And 1200 gpu_hours consumed in the current period
    When a scheduling cycle runs
    Then the budget penalty multiplier for tenant "physics" should be less than 0.1
    And allocations for tenant "physics" should not be hard-rejected

  # ── Waldur override ──

  Scenario: Waldur remaining_budget takes precedence when available
    Given the system is running with Waldur integration enabled
    And a tenant "physics" with gpu_hours_budget 1000
    And Waldur reports remaining_budget 200 for tenant "physics"
    When a scheduling cycle runs
    Then the budget_utilization should use Waldur's value, not the internal ledger

  Scenario: Waldur unavailable falls back to internal ledger
    Given the system is running with Waldur integration enabled
    And Waldur is unreachable
    And a tenant "physics" with gpu_hours_budget 1000
    And 800 gpu_hours consumed in the current period (per internal ledger)
    When a scheduling cycle runs
    Then the budget_utilization should use the internal ledger value

  # ── REST API ──

  Scenario: Query tenant usage via REST
    Given a tenant "physics" with gpu_hours_budget 1000
    And 450 gpu_hours consumed in the current period
    When I GET /v1/tenants/physics/usage
    Then the response should contain gpu_hours_used 450
    And the response should contain gpu_hours_budget 1000
    And the response should contain fraction_used 0.45
    And the response should contain period_start and period_end timestamps

  Scenario: Query usage for tenant with no budget
    Given a tenant "ml-team" with no gpu_hours_budget
    When I GET /v1/tenants/ml-team/usage
    Then the response should contain gpu_hours_used with the actual value
    And the response should contain gpu_hours_budget null
    And the response should contain fraction_used null

  Scenario: User queries their own usage across tenants
    Given user "alice" has allocations in tenant "physics" and tenant "climate"
    And tenant "physics" has 200 gpu_hours_used by user "alice"
    And tenant "climate" has 100 gpu_hours_used by user "alice"
    When I GET /v1/usage?user=alice
    Then the response should list usage per tenant for user "alice"

  # ── CLI convenience ──

  Scenario: CLI shows tenant budget status
    Given a tenant "physics" with gpu_hours_budget 1000
    And 450 gpu_hours consumed in the current period
    When I run "lattice usage --tenant physics"
    Then the output should show gpu_hours_used, gpu_hours_budget, and percentage

  Scenario: CLI shows user usage across tenants
    When I run "lattice usage"
    Then the output should show my usage across all tenants I have submitted to

  Scenario: CLI shows usage with period filter
    When I run "lattice usage --tenant physics --days 30"
    Then the output should show usage for the last 30 days only
