Feature: Streaming Telemetry
  EventBus delivers allocation events to subscribers.

  Scenario: Watch state changes
    Given an event bus
    And a subscriber for allocation "alloc-alpha"
    When a state change event is published for "alloc-alpha" from "Pending" to "Running"
    Then the subscriber for "alloc-alpha" should receive 1 event
    And the received event should be a state change to "Running"

  Scenario: Stream log lines
    Given an event bus
    And a subscriber for allocation "alloc-beta"
    When 3 log lines are published for "alloc-beta"
    Then the subscriber for "alloc-beta" should receive 3 events
    And all received events should be log lines

  Scenario: Subscriber isolation between allocations
    Given an event bus
    And a subscriber for allocation "alloc-one"
    And a subscriber for allocation "alloc-two"
    When a state change event is published for "alloc-one" from "Pending" to "Running"
    Then the subscriber for "alloc-one" should receive 1 event
    And the subscriber for "alloc-two" should receive 0 events
