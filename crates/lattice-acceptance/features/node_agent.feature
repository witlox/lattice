Feature: Node Agent
  End-to-end test: agent heartbeat, health, and allocation lifecycle.

  Scenario: Healthy agent generates valid heartbeat
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When the agent runs a health check with all systems healthy
    Then the heartbeat should report healthy
    And the heartbeat sequence should be 1

  Scenario: Unhealthy GPU triggers degraded heartbeat
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When the agent runs a health check with only 3 GPUs detected
    Then the heartbeat should report unhealthy
    And the heartbeat issues should mention "GPU"

  Scenario: Agent tracks allocation lifecycle
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When the agent starts allocation "train.py"
    Then the agent should have 1 active allocation
    When the agent completes the allocation
    Then the agent should have 0 active allocations

  Scenario: Image cache hit skips pull during prologue
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    And a pre-cached image "prgenv-gnu/24.11:v1"
    When I run the prologue for an allocation
    Then the prologue should report a cache hit
    And the runtime should have been prepared

  Scenario: Image cache miss triggers cache insert during prologue
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When I run the prologue for an allocation
    Then the prologue should report a cache miss
    And the runtime should have been prepared
