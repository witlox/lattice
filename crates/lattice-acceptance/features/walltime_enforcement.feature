Feature: Walltime Enforcement
  As a scheduler, I enforce walltime limits on running allocations.

  Scenario: Allocation expires after walltime
    Given a walltime enforcer with default grace period
    And a running allocation registered with walltime "1h"
    When 1 hour has elapsed
    Then the allocation should be in "Terminate" phase

  Scenario: Grace period before kill
    Given a walltime enforcer with default grace period
    And a running allocation registered with walltime "1h"
    When 1 hour has elapsed
    Then the allocation should be in "Terminate" phase
    When 1 hour and 31 seconds have elapsed
    Then the allocation should be in "Kill" phase

  Scenario: Custom grace period is respected
    Given a walltime enforcer with grace period 120 seconds
    And a running allocation registered with walltime "10m"
    When 10 minutes and 1 seconds have elapsed
    Then the allocation should be in "Terminate" phase
    When 10 minutes and 122 seconds have elapsed
    Then the allocation should be in "Kill" phase

  Scenario: Unregistered allocation is not enforced
    Given a walltime enforcer with default grace period
    And a running allocation registered with walltime "1h"
    When the allocation is unregistered from walltime tracking
    And 2 hours have elapsed
    Then no allocations should be expired
