Feature: Data Staging
  Lattice pre-stages data during queue wait time.

  Scenario: Data is staged before allocation starts
    Given a tenant "physics" with max nodes 100
    And a vCluster "sim-vc" for tenant "physics" with scheduler "hpc_backfill"
    And 4 nodes in group 0
    And storage with data readiness 0.5 for "s3://input/dataset"
    When an allocation is submitted with data mount "s3://input/dataset" to "/data"
    Then a staging plan should include the data mount
    And the staging plan priority should match the allocation priority

  Scenario: Ready data skips staging
    Given a tenant "physics" with max nodes 100
    And a vCluster "sim-vc" for tenant "physics" with scheduler "hpc_backfill"
    And 4 nodes in group 0
    And storage with data readiness 1.0 for "s3://cached/dataset"
    When an allocation is submitted with data mount "s3://cached/dataset" to "/data"
    Then no staging should be required

  Scenario: Multiple data mounts planned together
    Given a tenant "bio" with max nodes 100
    And a vCluster "bio-vc" for tenant "bio" with scheduler "hpc_backfill"
    And 4 nodes in group 0
    When an allocation is submitted with multiple data mounts
    Then the staging plan should include all mounts sorted by priority

  Scenario: Staging failure is non-fatal
    Given a tenant "physics" with max nodes 100
    And a vCluster "sim-vc" for tenant "physics" with scheduler "hpc_backfill"
    And 4 nodes in group 0
    And storage staging fails with a VAST API error
    When the allocation reaches the front of the scheduling queue
    Then the allocation is scheduled despite incomplete staging
    And a warning is attached to the allocation status

  Scenario: QoS set on staged data mounts
    Given a tenant "physics" with max nodes 100
    And 4 nodes in group 0
    When an allocation is submitted with data mount "s3://input/dataset" to "/data"
    And data staging executes
    Then the QoS policy should be set to "ReadWrite" on the staged data

  Scenario: Scratch directory created per node during prologue
    Given a tenant "physics" with max nodes 100
    And a node agent for node "x1000c0s0b0n0" with 4 GPUs
    When the prologue executes for an allocation with scratch_per_node enabled
    Then a scratch directory should be created on the node
    And the scratch path should be available to the allocation

  Scenario: Epilogue cleanup of scratch directories
    Given a node agent for node "x1000c0s0b0n0" with 4 GPUs
    And an allocation with scratch directories on the node
    When the epilogue executes
    Then the scratch directories should be cleaned up
    And cleanup failure should not fail the epilogue

  Scenario: Sensitive data staging uses encrypted storage pool
    Given a tenant "hospital" with strict isolation
    And 4 nodes in group 0
    When a sensitive allocation is submitted with data mount "s3://sensitive/records" to "/data"
    Then the staging plan should target the encrypted storage pool
    And staging should use access-logged paths

  Scenario: Data readiness threshold determines staging completeness
    Given a tenant "physics" with max nodes 100
    And 4 nodes in group 0
    And storage with data readiness 0.90 for "s3://input/large-dataset"
    When an allocation is submitted with data mount "s3://input/large-dataset" to "/data"
    Then the data should not be considered fully staged
    When readiness reaches 0.95
    Then the data should be considered staged and ready
