Feature: Observability
  Users can observe running allocations via attach, logs, and metrics.

  Scenario: Attach to running allocation
    Given a running allocation owned by user "alice"
    When user "alice" attaches to the allocation
    Then an attach session should be created
    And the session should support write and read

  Scenario: Attach denied for non-owner
    Given a running allocation owned by user "alice"
    When user "bob" attempts to attach
    Then the attach should be denied with permission error

  Scenario: Log buffer captures output
    Given a running allocation producing log output
    When the log buffer receives data
    Then reading the buffer should return the data in order
    And flushing to S3 should upload the contents

  Scenario: Metrics streamed per allocation
    Given a running allocation with telemetry enabled
    And a metrics subscriber for the allocation
    When the node agent pushes metric samples
    Then the subscriber should receive metric events
    And metrics should be scoped to the allocation

  Scenario: Diagnostics query returns allocation health
    Given a running allocation on 4 nodes
    When a diagnostics query is issued for the allocation
    Then the response should include per-node health status
    And the response should include resource utilization

  Scenario: Cross-tenant metric comparison denied
    Given a running allocation owned by tenant "physics"
    And a running allocation owned by tenant "biology"
    When a user requests to compare metrics across both allocations
    Then the comparison should be denied with "cross_tenant_comparison"

  Scenario: Same-tenant metric comparison allowed
    Given 2 running allocations owned by tenant "physics"
    When a user of tenant "physics" requests to compare metrics
    Then the comparison should be performed
    And metrics from both allocations should be returned

  Scenario: Sensitive allocation attach creates audit entry
    Given a running sensitive allocation owned by user "dr-x"
    When user "dr-x" attaches to the allocation
    Then an audit entry should be committed before the terminal opens

  Scenario: Log buffer wraps around evicting oldest entries
    Given a running allocation producing log output
    And the log buffer is at capacity
    When new log lines are produced
    Then the oldest entries should be evicted
    And the newest entries should be retained

  Scenario: Metric resolution switches between production and debug
    Given a running allocation with telemetry at 30-second resolution
    When the resolution is switched to 1-second debug mode
    Then metrics should be collected at 1-second intervals
    When switched back to production mode
    Then metrics should return to 30-second intervals
