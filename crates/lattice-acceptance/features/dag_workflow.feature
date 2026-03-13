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

  Scenario: afternotok dependency runs on predecessor failure
    Given a tenant "ml-team" with a quota of 20 nodes
    When I submit a DAG with stages:
      | id        | nodes | depends_on         |
      | main      | 2     |                    |
      | cleanup   | 1     | main:notok         |
    Then "cleanup" should depend on "main" with condition "NotOk"
    When "main" transitions to "Failed"
    Then "cleanup" should become eligible for scheduling

  Scenario: afterany dependency runs regardless of outcome
    Given a tenant "ml-team" with a quota of 20 nodes
    When I submit a DAG with stages:
      | id       | nodes | depends_on    |
      | compute  | 4     |               |
      | report   | 1     | compute:any   |
    When "compute" transitions to "Failed"
    Then "report" should become eligible for scheduling

  Scenario: DAG partial failure triggers afternotok dependents
    Given a tenant "ml-team" with a quota of 20 nodes
    And 10 ready nodes in group 0
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    When I submit a DAG with stages:
      | id         | nodes | depends_on           |
      | train      | 4     |                      |
      | evaluate   | 2     | train:success        |
      | alert      | 1     | train:notok          |
    And "train" transitions to "Failed"
    Then "evaluate" should remain "Pending" with unsatisfied dependencies
    And "alert" should become eligible for scheduling

  Scenario: DAG cancellation cancels pending stages
    Given a tenant "ml-team" with a quota of 20 nodes
    And 10 ready nodes in group 0
    And a vCluster "hpc-batch" with scheduler "HpcBackfill"
    When I submit a DAG with stages:
      | id         | nodes | depends_on         |
      | preprocess | 2     |                    |
      | train      | 4     | preprocess:success |
      | evaluate   | 2     | train:success      |
    And the scheduler runs a DAG cycle
    And I cancel the DAG
    Then "train" should be "Cancelled"
    And "evaluate" should be "Cancelled"

  Scenario: DAG size limit exceeded
    Given a tenant "ml-team" with a quota of 20 nodes
    When I submit a DAG with 1001 stages
    Then the DAG should fail validation with a size limit error

  Scenario: Fan-out pattern with parallel stages
    Given a tenant "ml-team" with a quota of 20 nodes
    When I submit a DAG with stages:
      | id         | nodes | depends_on         |
      | preprocess | 2     |                    |
      | train-a    | 2     | preprocess:success |
      | train-b    | 2     | preprocess:success |
      | train-c    | 2     | preprocess:success |
    Then the DAG should have 4 allocations
    And "train-a", "train-b", "train-c" should all depend on "preprocess"
    And "train-a", "train-b", "train-c" can run in parallel

  Scenario: Fan-in pattern converges to single stage
    Given a tenant "ml-team" with a quota of 20 nodes
    When I submit a DAG with stages:
      | id        | nodes | depends_on                           |
      | shard-a   | 2     |                                      |
      | shard-b   | 2     |                                      |
      | aggregate | 1     | shard-a:success,shard-b:success      |
    Then "aggregate" should depend on both "shard-a" and "shard-b"
    And "aggregate" only becomes eligible when both predecessors complete
