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

  Scenario: Metric events streamed per allocation
    Given an event bus
    And a subscriber for allocation "alloc-metrics"
    When 5 metric samples are published for "alloc-metrics"
    Then the subscriber for "alloc-metrics" should receive 5 events
    And all received events should be metric samples

  Scenario: Slow subscriber events dropped without blocking
    Given an event bus
    And a slow subscriber for allocation "alloc-slow"
    When 1000 events are published rapidly for "alloc-slow"
    Then the event bus should not block
    And the slow subscriber may receive fewer than 1000 events
    And dropped events should be counted

  Scenario: Multiple subscribers per allocation receive same events
    Given an event bus
    And subscriber "sub-1" for allocation "alloc-multi"
    And subscriber "sub-2" for allocation "alloc-multi"
    When a state change event is published for "alloc-multi" from "Pending" to "Running"
    Then subscriber "sub-1" should receive 1 event
    And subscriber "sub-2" should receive 1 event

  Scenario: Subscriber cleanup on disconnect
    Given an event bus
    And a subscriber for allocation "alloc-cleanup"
    When the subscriber disconnects
    And a state change event is published for "alloc-cleanup" from "Pending" to "Running"
    Then the disconnected subscriber should not receive the event
    And no error should be raised

  Scenario: Event ordering preserved within allocation stream
    Given an event bus
    And a subscriber for allocation "alloc-ordered"
    When events are published in order: "Pending" to "Staging" to "Running"
    Then the subscriber should receive events in the same order
