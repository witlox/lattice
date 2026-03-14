Feature: Cross-Context Interactions
  Tests that exercise boundaries between bounded contexts.
  These test integration surfaces, race conditions, and failure propagation
  across Scheduling, Consensus, Node Management, Observability, Tenant & Access,
  and Federation contexts.

  # -- Concurrent vCluster Proposals ------------------------------

  Scenario: Two vCluster schedulers propose conflicting node assignments
    Given a quorum with 3 healthy members
    And node "x1000c0s0b0n0" is unowned
    And the HPC vCluster scheduler proposes allocation "hpc-1" on node "x1000c0s0b0n0"
    And the Service vCluster scheduler simultaneously proposes allocation "svc-1" on node "x1000c0s0b0n0"
    When the quorum processes both proposals
    Then exactly one proposal is committed
    And the other proposal is rejected with reason "ownership_conflict"
    And the rejected scheduler retries on the next scheduling cycle

  Scenario: Rejected proposal does not leave stale state
    Given node "x1000c0s0b0n0" is assigned to allocation "hpc-1" via committed proposal
    And the Service scheduler's proposal for the same node was rejected
    When the Service scheduler runs its next scheduling cycle
    Then it observes node "x1000c0s0b0n0" as owned by "hpc-1"
    And it does not re-propose the same node for "svc-1"

  # -- Preemption vs Natural Completion ---------------------------

  Scenario: Allocation completes naturally while checkpoint hint is in flight
    Given allocation "train-1" is running on nodes "x1000c0s0b0n0,x1000c0s0b0n1"
    And allocation "train-1" has checkpoint strategy "auto"
    And the checkpoint broker sends a CHECKPOINT_HINT for "train-1"
    When allocation "train-1" completes successfully before the checkpoint begins
    Then "train-1" transitions to "Completed"
    And nodes are released normally
    And the checkpoint hint is discarded as a no-op
    And no checkpoint file is written

  Scenario: Preemption completes before natural completion
    Given allocation "train-1" is running with 50% walltime remaining
    And a class-8 allocation "urgent-1" is pending and needs "train-1"'s nodes
    And "train-1" has preemption class 3
    When the scheduler initiates preemption of "train-1"
    Then a CHECKPOINT_HINT is sent to "train-1"'s node agents
    And "train-1" transitions to "Checkpointing"
    When the checkpoint completes within the timeout
    Then "train-1" transitions to "Suspended"
    And "train-1"'s nodes are released
    And "urgent-1" is proposed for the freed nodes
    And "train-1" re-enters the queue with its original submission time preserved

  # -- Quota Reduction vs In-Flight Proposal ----------------------

  Scenario: Quota reduced while proposal is in flight
    Given tenant "physics" has max_nodes quota of 200
    And tenant "physics" currently uses 190 nodes
    And the scheduler proposes allocation "sim-1" requiring 8 nodes for tenant "physics"
    When Waldur reduces tenant "physics" max_nodes to 195 via API
    And the quota change is Raft-committed before the proposal
    Then the proposal for "sim-1" is rejected with reason "quota_exceeded"
    And the scheduler observes the new quota on its next cycle
    And no nodes are assigned

  Scenario: Proposal committed before quota reduction
    Given tenant "physics" has max_nodes quota of 200
    And tenant "physics" currently uses 190 nodes
    And the scheduler proposes allocation "sim-1" requiring 8 nodes for tenant "physics"
    When the proposal for "sim-1" is Raft-committed before the quota reduction
    Then "sim-1" is committed successfully with 198 total nodes
    When Waldur reduces tenant "physics" max_nodes to 195
    Then "sim-1" continues running (no retroactive preemption)
    And new proposals for tenant "physics" are rejected until usage drops below 195

  # -- Walltime vs In-Progress Checkpoint -------------------------

  Scenario: Walltime expires during active checkpoint - checkpoint completes in grace period
    Given allocation "long-job" has 30 seconds of walltime remaining
    And a checkpoint is in progress for "long-job"
    When the walltime expires
    Then SIGTERM is sent to "long-job"
    And the checkpoint has 30 seconds (the SIGTERM grace period) to complete
    When the checkpoint completes within the grace period
    Then the checkpoint file is usable for restart
    And "long-job" transitions to "Suspended" (not Failed)

  Scenario: Walltime expires during active checkpoint - checkpoint does not complete
    Given allocation "long-job" has 30 seconds of walltime remaining
    And a checkpoint is in progress for "long-job"
    When the walltime expires
    Then SIGTERM is sent to "long-job"
    When the checkpoint does not complete within the 30-second grace period
    Then SIGKILL is sent
    And the incomplete checkpoint is discarded
    And "long-job" transitions to "Failed" with reason "walltime_exceeded"
    And the metric "lattice_checkpoint_walltime_conflict_total" is incremented

  # -- VNI Exhaustion During DAG ----------------------------------

  Scenario: VNI pool exhaustion stalls DAG at stage requiring new domain
    Given a DAG with stages "preprocess" -> "train" -> "evaluate"
    And "preprocess" uses network domain "ml-workspace" (VNI already allocated)
    And "train" requires a new network domain "train-domain"
    And the VNI pool is exhausted (3095/3095 in use)
    When "preprocess" completes successfully
    Then "train" enters "Pending" with reason "vni_pool_exhausted"
    And "evaluate" remains blocked (dependency on "train" unsatisfied)
    And "preprocess"'s domain "ml-workspace" is in grace period (VNI not yet freed)
    When another allocation's domain releases a VNI
    Then "train" is re-evaluated on the next scheduling cycle
    And a VNI is allocated for "train-domain"
    And "train" proceeds to scheduling

  Scenario: DAG stages sharing a domain are unaffected by VNI exhaustion
    Given a DAG with stages "stage-a" -> "stage-b" sharing network domain "shared-dom"
    And the VNI pool is exhausted
    When "stage-a" completes
    Then "stage-b" joins the existing domain "shared-dom" (same VNI, no new allocation needed)
    And "stage-b" proceeds to scheduling normally

  # -- Sensitive Audit Ordering -----------------------------------

  Scenario: Sensitive node claim audit entry committed before workload starts
    Given user "dr-x" requests 4 sensitive nodes
    When the quorum processes the claim
    Then an audit entry recording "dr-x claimed nodes N1-N4" is Raft-committed
    And only after the audit commit does the quorum notify node agents
    And node agents begin prologue only after receiving the committed assignment

  Scenario: Sensitive attach session audit entry committed before terminal opens
    Given user "dr-x" has a running sensitive allocation on node "N1"
    When "dr-x" requests to attach to the allocation
    Then an audit entry recording "dr-x attached to allocation X on N1" is Raft-committed
    And only after the audit commit does the node agent spawn the PTY
    And the terminal stream opens to the client

  Scenario: Non-claiming user denied sensitive attach
    Given user "dr-x" has claimed sensitive nodes for allocation "sens-1"
    When user "dr-y" attempts to attach to "sens-1"
    Then the attach is denied with reason "not_claiming_user"
    And an audit entry recording the denied attempt is Raft-committed

  # -- Node Failure During Cross-Context Operations ---------------

  Scenario: Node crashes during checkpoint initiated by preemption
    Given allocation "victim" is being preempted
    And a CHECKPOINT_HINT has been sent to "victim"'s node agent on "x1000c0s0b0n0"
    When the node agent on "x1000c0s0b0n0" crashes
    Then the quorum detects missed heartbeats
    And "x1000c0s0b0n0" transitions to Degraded, then Down after grace period
    And allocation "victim" transitions to Failed (checkpoint could not complete)
    And "victim" is requeued per its requeue policy (without checkpoint)
    And the pending higher-priority allocation can now be proposed for other nodes

  Scenario: Node crashes during sensitive wipe
    Given sensitive allocation "sens-1" has completed on node "N1"
    And node "N1" is undergoing secure wipe via OpenCHAMI
    When the node agent on "N1" crashes during wipe
    Then "N1" enters quarantine (treated as Down)
    And "N1" does NOT return to the general scheduling pool
    And a critical audit entry is Raft-committed recording the wipe failure
    And an alert is raised for operator intervention

  # -- Accounting Failure Isolation -------------------------------

  Scenario: Waldur unavailable does not affect scheduling
    Given Waldur is unreachable
    And an allocation "batch-1" is pending in the scheduler queue
    When the scheduler runs a scheduling cycle
    Then "batch-1" is scheduled normally
    And accounting events are buffered in memory
    And the metric "lattice_accounting_buffer_size" increases
    And no error is returned to the user

  Scenario: Accounting buffer overflow drops events without blocking
    Given Waldur has been unreachable for an extended period
    And the in-memory buffer (10,000 events) is full
    And the disk buffer (100,000 events) is full
    When a new allocation completes
    Then the accounting event is dropped
    And the metric "lattice_accounting_events_dropped_total" is incremented
    And scheduling continues normally
    And the allocation completion is recorded in the quorum (recoverable)

  # -- Federation Cross-Context -----------------------------------

  Scenario: Federated request arrives during local leader election
    Given federation is enabled
    And the local quorum is undergoing leader election
    When a signed allocation request arrives from a federated peer
    Then the federation broker returns 503 with "Retry-After: 5" header
    And the remote site retries after the specified interval
    And no request is queued locally during election

  Scenario: Federated request for sensitive data violates sovereignty
    Given federation is enabled
    And user at Site A submits an allocation targeting Site B
    And the allocation references data in Site A's sensitive storage pool
    When the federation broker at Site A evaluates the request
    Then the request is rejected with reason "sensitive_data_sovereignty"
    And no data transfer is initiated
    And no request is forwarded to Site B

  # -- Observability Isolation for Sensitive ----------------------

  Scenario: Cross-tenant metric comparison rejected for sensitive allocations
    Given tenant "hospital-a" has sensitive allocation "sens-1"
    And tenant "research-b" has allocation "research-1"
    When a user requests CompareMetrics between "sens-1" and "research-1"
    Then the request is rejected with reason "cross_tenant_sensitive_comparison"
    And no metrics are returned

  Scenario: Same-tenant sensitive metric comparison allowed
    Given user "dr-x" of tenant "hospital-a" has sensitive allocation "sens-1"
    And user "dr-x" of tenant "hospital-a" has sensitive allocation "sens-2"
    When "dr-x" requests CompareMetrics between "sens-1" and "sens-2"
    Then the comparison is performed
    And an audit entry is committed for both allocations
    And metrics are returned

  # -- Data Staging Cross-Context ---------------------------------

  Scenario: Data staging completes during queue wait improving scheduling score
    Given allocation "train-1" is pending with data mounts requiring hot-tier staging
    And the data mover begins staging during queue wait
    When staging reaches 95% readiness
    Then allocation "train-1"'s f5 (data_readiness) score increases
    And "train-1" is scored higher in the next scheduling cycle
    And "train-1" is scheduled ahead of allocations with lower data readiness

  Scenario: Data staging failure is non-fatal
    Given allocation "train-1" is pending with data mounts
    And data staging fails (VAST API error)
    When "train-1" reaches the front of the scheduling queue
    Then "train-1" is scheduled despite incomplete staging
    And a warning is attached to the allocation status
    And the entrypoint starts (may encounter I/O latency)

  # -- Elastic Borrowing Cross-Context ----------------------------

  Scenario: Borrowed node reclaimed by home vCluster
    Given vCluster "hpc" has lent 20 idle nodes to vCluster "service"
    And vCluster "service" is running allocation "inference-1" on borrowed nodes
    When a pending allocation in vCluster "hpc" needs those nodes
    Then the scheduler triggers checkpoint of "inference-1" on borrowed nodes
    When the checkpoint completes
    Then the borrowed nodes are returned to vCluster "hpc"
    And "inference-1" scales down (if reactive) or is suspended (if bounded)
    And the pending HPC allocation is proposed for the reclaimed nodes

  Scenario: Reactive allocation scales below min_nodes during reclamation
    Given reactive allocation "autoscale-1" has min_nodes=4 and is running on 6 nodes
    And 2 of those nodes are borrowed
    When the home vCluster reclaims both borrowed nodes
    Then "autoscale-1" drops to 4 nodes (at min_nodes)
    And no alert is raised (within bounds)
    When a third node would be reclaimed
    Then "autoscale-1" would drop below min_nodes
    And the scheduler attempts to acquire a replacement from the home vCluster
    And no replacement is available
    Then "autoscale-1" operates below min_nodes temporarily
    And an alert is raised
