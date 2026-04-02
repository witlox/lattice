Feature: Operational Validation (Level 3.5)
  ReFrame-inspired workload tests against a deployed cluster.
  Validates fidelity gaps unreachable by unit/BDD tests.

  # ── Suite 1: Allocation Lifecycle (parallel-safe) ──────────

  @parallel @docker @gcp
  Scenario: Bare process completes successfully
    Given a running cluster with at least 1 compute node
    When I submit a bounded allocation running "/bin/echo hello-lattice"
    Then the allocation reaches "completed" within 30s
    And the allocation has exactly 1 assigned node

  @parallel @docker @gcp
  Scenario: Container workload completes successfully
    Given the test registry has "lattice-test/numpy-bench"
    When I submit a bounded container allocation running numpy-bench
    Then the allocation reaches "completed" within 60s

  @parallel @docker @gcp
  Scenario: ML training workload completes
    Given the test registry has "lattice-test/pytorch-mnist"
    When I submit a bounded container allocation running pytorch-mnist
    Then the allocation reaches "completed" within 120s

  @parallel @docker @gcp
  Scenario: Walltime exceeded triggers termination
    Given a running cluster
    When I submit a bounded allocation with walltime 10s running "sleep 300"
    Then the allocation reaches "failed" within 20s

  # ── Suite 2: Service Workloads (parallel-safe) ─────────────
  # Targets: service_workloads LOW confidence area

  @parallel @docker @gcp
  Scenario: Unbounded service stays running
    Given the test registry has "lattice-test/service-http"
    When I submit an unbounded allocation running service-http
    Then the allocation reaches "running" within 30s
    And the allocation remains "running" for 30s
    When I cancel the allocation
    Then the allocation reaches "completed" within 10s

  @parallel @docker @gcp
  Scenario: Service failure triggers requeue
    Given the test registry has "lattice-test/crasher"
    When I submit an unbounded allocation with requeue_policy "always" and max_requeue 3
    And the container exits with code 1 after 5s
    Then the allocation is requeued within 15s
    And a new instance reaches "running" within 30s

  @parallel @docker @gcp
  Scenario: Max requeue exhaustion stops retries
    Given the test registry has "lattice-test/crasher"
    When I submit an unbounded allocation with requeue_policy "always" and max_requeue 1
    And the container crashes twice
    Then the allocation reaches "failed" after exhausting requeues

  # ── Suite 3: Quota and Fairness (sequential) ───────────────
  # Targets: G69/G70 quota race conditions

  @sequential @docker @gcp
  Scenario: Fair share across tenants under contention
    Given 2 tenants with equal quota sharing the cluster
    When each tenant submits 5 bounded allocations running "sleep 2"
    Then each tenant gets at least 1 allocation scheduled within 30s
    And no tenant gets more than 4 of the first 5 scheduled

  @sequential @docker @gcp
  Scenario: Hard quota rejection
    Given a tenant "quota-test" with quota of 2 nodes
    When the tenant has 2 running allocations
    And submits a 3rd allocation
    Then the 3rd allocation stays "pending" and is not scheduled

  # ── Suite 4: Drain Under Load (sequential) ─────────────────

  @sequential @docker @gcp
  Scenario: Drain node with running allocation
    Given 2 running allocations spread across nodes
    When I drain a node that has a running allocation
    Then the node transitions to "draining"
    And when the allocation completes the node becomes "drained"
    When I undrain the node
    Then the node returns to "ready"

  @sequential @docker @gcp
  Scenario: Drain empty node is immediate
    Given all allocations are on node-0 only
    When I drain node-1
    Then node-1 becomes "drained" within 5s

  # ── Suite 5: Preemption (sequential) ────────────────────────

  @sequential @docker @gcp
  Scenario: High-priority preempts low-priority
    Given the cluster is full with "low" priority allocations
    When I submit a "high" priority allocation
    Then one low-priority allocation is preempted
    And the high-priority allocation reaches "running" within 60s

  @sequential @docker @gcp
  Scenario: Checkpoint signal before preemption
    Given the test registry has "lattice-test/checkpoint"
    And a running allocation with checkpoint support at "low" priority
    When a "high" priority allocation triggers preemption
    Then the checkpointing allocation receives SIGUSR1 before termination

  # ── Suite 6: DAG Workflows (parallel-safe) ──────────────────

  @parallel @docker @gcp
  Scenario: Linear DAG executes in order
    Given a 3-stage DAG: preprocess then train then postprocess
    When I submit the DAG
    Then each stage runs only after its predecessor completes
    And the DAG reaches "completed" within 120s

  @parallel @docker @gcp
  Scenario: DAG with failure stops dependents
    Given a 2-stage DAG where stage-1 exits with code 1
    When I submit the DAG
    Then stage-2 never reaches "running"
    And the DAG reaches "failed"

  @parallel @docker @gcp
  Scenario: DAG with afternotok runs error handler
    Given a 2-stage DAG with afternotok dependency
    And stage-1 exits with code 1
    When I submit the DAG
    Then stage-2 runs because stage-1 failed
    And the DAG reaches "completed"

  # ── Suite 7: Agent Recovery (sequential, GCP only) ──────────
  # Targets: G12 reconnect replay

  @sequential @gcp
  Scenario: Agent restart preserves running workloads
    Given a running container allocation on compute-1
    When the lattice-agent on compute-1 is restarted via systemctl
    Then the agent re-registers within 15s
    And the allocation returns to "running" within 30s

  @sequential @gcp
  Scenario: Agent crash triggers requeue after heartbeat timeout
    Given a running allocation on compute-1
    When the lattice-agent on compute-1 is killed with SIGKILL
    And 30s passes without heartbeat
    Then the allocation is requeued to a healthy node

  # ── Suite 8: Performance Baselines (parallel-safe) ──────────

  @parallel @docker @gcp
  Scenario: numpy-bench reports GFLOPS
    Given the test registry has "lattice-test/numpy-bench"
    When I submit numpy-bench as a bounded container allocation
    Then the allocation completes within 60s
    And stdout contains a GFLOPS measurement

  @parallel @docker @gcp
  Scenario: pytorch-mnist trains to convergence
    Given the test registry has "lattice-test/pytorch-mnist"
    When I submit pytorch-mnist as a bounded container allocation
    Then the allocation completes within 120s
    And stdout contains "accuracy"

  @parallel @docker @gcp
  Scenario: Concurrent submission throughput
    Given a running cluster with at least 2 compute nodes
    When I submit 20 bounded allocations within 5s
    Then all 20 allocations are acknowledged within 5s
    And at least 2 allocations reach "running" within 30s
