Feature: Scheduling Cycle
  End-to-end test: submit → schedule → running → completed.

  Scenario: Single allocation scheduled and completed
    Given a tenant "physics" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 10 ready nodes in group 0
    When I submit a bounded allocation requesting 2 nodes with walltime "1h"
    And the scheduler runs a cycle
    Then the allocation should be placed on 2 nodes
    And the allocation state should be "Running"

  Scenario: Higher priority allocation scheduled first
    Given a tenant "ml" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 4 ready nodes in group 0
    When I submit a low-priority allocation requesting 2 nodes
    And I submit a high-priority allocation requesting 2 nodes
    And the scheduler runs a cycle
    Then the high-priority allocation should be "Running"

  Scenario: Allocation deferred when no resources available
    Given a tenant "physics" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 2 ready nodes in group 0
    When I submit a bounded allocation requesting 4 nodes with walltime "1h"
    And the scheduler runs a cycle
    Then the allocation state should be "Pending"

  Scenario: Backfill small job fits in gap before reserved job
    Given a tenant "physics" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 10 ready nodes in group 0
    And a large deferred allocation reserving 8 nodes starting in 2 hours
    When I submit a small allocation requesting 2 nodes with walltime "1h"
    And the scheduler runs a cycle
    Then the small allocation should be "Running" via backfill
    And the reservation for the large allocation is preserved

  Scenario: Reservation created for highest-priority deferred job
    Given a tenant "physics" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 4 ready nodes in group 0 all running allocations
    When I submit a high-priority allocation requesting 4 nodes with walltime "2h"
    And the scheduler runs a cycle
    Then a reservation should be created for the high-priority allocation
    And the reservation should target the earliest available nodes

  Scenario: Backfill-safe check prevents delaying reserved job
    Given a tenant "physics" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 8 ready nodes in group 0
    And a reservation for a high-priority allocation needing 6 nodes in 1 hour
    When I submit a backfill candidate requiring 4 nodes with walltime "2h"
    And the scheduler runs a cycle
    Then the backfill candidate should remain "Pending"
    And the reservation should not be delayed

  Scenario: Multi-vCluster scheduling each gets its turn
    Given a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And a vCluster "service" with scheduler "ServiceBinPack"
    And 10 ready nodes in group 0
    When both vClusters have pending allocations
    And the scheduler runs a cycle
    Then allocations from both vClusters should be evaluated

  Scenario: Cost function topology weight favors same dragonfly group
    Given a tenant "physics" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 4 ready nodes in group 0
    And 4 ready nodes in group 1
    When I submit a bounded allocation requesting 3 nodes with walltime "1h"
    And the scheduler runs a cycle
    Then all placed nodes should be in the same dragonfly group

  Scenario: Fair share deficit improves starved tenant score
    Given a tenant "starved" with fair_share_target 0.5 and current usage 0.1
    And a tenant "heavy" with fair_share_target 0.5 and current usage 0.9
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 4 ready nodes in group 0
    When both tenants submit allocations requesting 2 nodes
    And the scheduler runs a cycle
    Then the "starved" tenant's allocation should be scheduled first

  Scenario: Wait time aging increases score over time
    Given a tenant "physics" with a quota of 100 nodes
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    And 2 ready nodes in group 0
    And a pending allocation submitted 6 hours ago requesting 2 nodes
    And a pending allocation submitted just now requesting 2 nodes
    When the scheduler runs a cycle
    Then the older allocation should score higher due to wait time aging

  Scenario: Service bin-pack scheduler fills nodes densely
    Given a tenant "services" with a quota of 100 nodes
    And a vCluster "service" with scheduler "ServiceBinPack"
    And 4 ready nodes in group 0
    When I submit 3 unbounded allocations each requesting 1 node
    And the scheduler runs a cycle
    Then allocations should be packed onto fewer nodes when possible

  Scenario: Interactive FIFO scheduler orders by submission time
    Given a tenant "users" with a quota of 100 nodes
    And a vCluster "interactive" with scheduler "InteractiveFifo"
    And 2 ready nodes in group 0
    When I submit 3 interactive allocations in sequence
    And the scheduler runs a cycle
    Then the first submitted allocation should be scheduled first
