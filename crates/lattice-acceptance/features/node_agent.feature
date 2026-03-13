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

  Scenario: Prologue failure triggers retry on different nodes
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When I run the prologue and it fails
    Then the prologue result should indicate failure
    And the allocation should be eligible for retry on different nodes

  Scenario: Epilogue cleans up scratch and sends accounting event
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    And a completed allocation with scratch directories
    When I run the epilogue for the allocation
    Then scratch directories should be cleaned up
    And an accounting event should be emitted

  Scenario: Runtime selection uses uenv when specified
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When I run the prologue for an allocation with uenv "prgenv-gnu/24.11:v1"
    Then the runtime should use uenv mount namespace
    And the runtime should have been prepared

  Scenario: Runtime selection uses sarus for container images
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When I run the prologue for an allocation with container image "registry/app:latest"
    Then the runtime should use sarus container
    And the runtime should have been prepared

  Scenario: Data staging executes during prologue
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    And an allocation with data mount "s3://input/data" to "/data"
    When I run the prologue for the allocation
    Then data staging should execute before the entrypoint
    And the data mount should be available at "/data"

  Scenario: Checkpoint signal forwarded to application
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    And a running allocation with checkpoint protocol "signal"
    When a CHECKPOINT_HINT is received
    Then the agent forwards the signal to the application process

  Scenario: Health check includes memory and disk metrics
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When the agent runs a health check with all systems healthy
    Then the heartbeat should include memory utilization
    And the heartbeat should include disk health

  Scenario: Agent restart re-registers with quorum
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When the agent restarts after a crash
    Then the agent should re-register with the quorum
    And the heartbeat sequence should reset
