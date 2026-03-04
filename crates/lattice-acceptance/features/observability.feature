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
