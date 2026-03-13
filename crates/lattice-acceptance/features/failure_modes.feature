Feature: Failure Modes
  As a system, I handle failures gracefully without losing state.

  Scenario: Node failure marks allocations as failed
    Given a tenant "research" with a quota of 100 nodes
    And 4 ready nodes in group 0
    And a running allocation on node "x1000c0s0b0n0"
    When node "x1000c0s0b0n0" transitions to down with reason "hardware fault"
    Then node "x1000c0s0b0n0" should not be operational
    And the allocation on the failed node should be marked "Failed"

  Scenario: Draining node completes running allocations before draining
    Given a tenant "research" with a quota of 100 nodes
    And 4 ready nodes in group 0
    And a running allocation on node "x1000c0s0b0n1"
    When node "x1000c0s0b0n1" begins draining
    Then node "x1000c0s0b0n1" should be in "Draining" state
    And new allocations should not be placed on "x1000c0s0b0n1"

  Scenario: Degraded node remains operational
    Given 4 ready nodes in group 0
    When node "x1000c0s0b0n2" becomes degraded with reason "slow disk"
    Then node "x1000c0s0b0n2" should still be operational
    And the degradation reason should be "slow disk"

  Scenario: Quorum minority loss continues scheduling
    Given a quorum with 3 healthy members
    When 1 quorum member becomes unreachable
    Then the remaining 2 members form a majority
    And proposals continue to be committed
    And running allocations are unaffected

  Scenario: Quorum leader loss triggers election
    Given a quorum with 3 healthy members and an elected leader
    When the leader becomes unreachable
    Then a new leader is elected within 3 seconds
    And in-flight proposals are retried by schedulers
    And running allocations are unaffected

  Scenario: Complete quorum loss preserves running allocations
    Given a quorum with 3 members
    And 5 running allocations across 20 nodes
    When all quorum members become unreachable
    Then no new allocations can be committed
    And running allocations continue on their nodes
    And node agents operate autonomously

  Scenario: vCluster scheduler crash isolates impact
    Given vCluster "hpc-batch" and vCluster "service" are both active
    When the scheduler for "hpc-batch" crashes
    Then scheduling for "hpc-batch" pauses
    And scheduling for "service" continues normally
    And running allocations in "hpc-batch" are unaffected

  Scenario: API server crash allows client retry
    Given 2 API server replicas behind a load balancer
    When one API server crashes
    Then client requests are routed to the surviving replica
    And no submissions are lost

  Scenario: Checkpoint broker crash makes allocations non-preemptible
    Given a running checkpoint broker
    And 3 running allocations with checkpoint enabled
    When the checkpoint broker crashes
    Then no checkpoint hints are sent
    And running allocations are effectively non-preemptible
    And scheduling continues without preemption

  Scenario: Network partition heals within grace period
    Given a tenant "research" with a quota of 100 nodes
    And a running allocation on node "x1000c0s0b0n0"
    When node "x1000c0s0b0n0" is network-partitioned from the quorum
    Then node "x1000c0s0b0n0" transitions to "Degraded" after heartbeat timeout
    When the partition heals within the grace period
    Then node "x1000c0s0b0n0" returns to "Ready"
    And the running allocation continues uninterrupted

  Scenario: VAST storage unavailable pauses staging
    Given a pending allocation with data mounts requiring staging
    When VAST storage becomes unavailable
    Then data staging pauses with retry backoff
    And running allocations with already-mounted data continue
    And checkpoint writes are suppressed

  Scenario: OpenCHAMI unavailable quarantines wipe-pending nodes
    Given a sensitive allocation completing on node "x1000c0s0b0n0"
    When OpenCHAMI becomes unavailable during secure wipe
    Then node "x1000c0s0b0n0" remains quarantined
    And node "x1000c0s0b0n0" does not return to the scheduling pool
    And scheduling of other nodes continues normally

  Scenario: TSDB unavailable uses stale cost scores
    Given the TSDB is unreachable
    And 3 pending allocations awaiting scheduling
    When the scheduler runs a cycle
    Then stale cost function values from last successful query are used
    And allocations are scheduled with suboptimal but valid scores
    And autoscaling is paused

  Scenario: Prologue failure retried on different nodes
    Given a tenant "physics" with a quota of 100 nodes
    And 8 ready nodes in group 0
    And a pending allocation requesting 2 nodes
    When the prologue fails on the initially assigned nodes
    Then the allocation is retried on different nodes
    And the retry count increments

  Scenario: Prologue failure exhausts max retries
    Given a pending allocation with max retries of 3
    When the prologue fails 3 times on different nodes
    Then the allocation transitions to "Failed"
    And the failure reason includes "prologue_failure_max_retries"

  Scenario: Application crash with requeue policy never
    Given a running allocation with requeue policy "never"
    When the application process exits with non-zero status
    Then the allocation transitions to "Failed"
    And the allocation is not requeued

  Scenario: Application crash with requeue policy on_node_failure
    Given a running allocation with requeue policy "on_node_failure"
    When the application process exits with non-zero status due to node hardware fault
    Then the allocation is requeued
    And the requeue count increments

  Scenario: Application crash with requeue policy always
    Given a running allocation with requeue policy "always"
    When the application process exits with non-zero status
    Then the allocation is requeued
    And the requeue count increments
    When the allocation has been requeued 3 times
    Then the allocation transitions to "Failed" with reason "max_requeue_exceeded"
