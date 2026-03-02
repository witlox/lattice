Feature: DAG Workflow
  Allocations can be submitted as a DAG with dependency edges.

  Scenario: Sequential DAG with dependency edges
    Given a tenant "ml-team" with a quota of 20 nodes
    When I submit a DAG with stages:
      | id         | nodes | depends_on         |
      | preprocess | 2     |                    |
      | train      | 8     | preprocess:success |
      | evaluate   | 2     | train:any          |
    Then the DAG should have 3 allocations
    And "preprocess" should have no dependencies
    And "train" should depend on "preprocess" with condition "Success"
    And "evaluate" should depend on "train" with condition "Any"

  Scenario: DAG allocations share a dag_id
    Given a tenant "ml-team" with a quota of 20 nodes
    When I submit a DAG with stages:
      | id    | nodes | depends_on    |
      | step1 | 1     |               |
      | step2 | 1     | step1:success |
    Then all DAG allocations should share the same dag_id

  Scenario: DAG cycle is rejected
    Given a tenant "ml-team" with a quota of 20 nodes
    When I submit a DAG with stages:
      | id    | nodes | depends_on  |
      | stepA | 1     | stepC:any   |
      | stepB | 1     | stepA:any   |
      | stepC | 1     | stepB:any   |
    Then the DAG should fail validation with a cycle error

  Scenario: DAG stage execution respects dependencies
    Given a tenant "ml-team" with a quota of 20 nodes
    And 10 ready nodes in group 0
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    When I submit a DAG with stages:
      | id         | nodes | depends_on         |
      | preprocess | 2     |                    |
      | train      | 4     | preprocess:success |
    And the scheduler runs a DAG cycle
    Then "preprocess" should be "Running"
    And "train" should be "Pending"
