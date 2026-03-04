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
