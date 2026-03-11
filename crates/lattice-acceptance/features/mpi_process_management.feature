Feature: MPI Process Management
  The system orchestrates MPI launches across nodes using native PMI-2.
  The API server computes rank layout, fans out LaunchProcesses to node agents,
  and each node agent runs a PMI-2 server for its local ranks.

  Scenario: Single-node single-rank PMI-2 lifecycle
    Given a PMI-2 server for 1 rank on 1 node with world size 1
    When a rank connects and performs fullinit
    Then the rank should receive rank 0 and world size 1
    When the rank performs kvsfence
    Then the fence should complete successfully
    When the rank finalizes
    Then the PMI-2 server should shut down cleanly

  Scenario: Two ranks on one node exchange KVS data via fence
    Given a PMI-2 server for 2 ranks on 1 node with world size 2
    When rank 0 connects and puts key "addr0" with value "10.0.0.1:5000"
    And rank 1 connects and puts key "addr1" with value "10.0.0.2:5000"
    And both ranks perform kvsfence
    Then rank 0 should be able to get key "addr1" with value "10.0.0.2:5000"
    And rank 1 should be able to get key "addr0" with value "10.0.0.1:5000"

  Scenario: Rank layout computation distributes ranks evenly
    Given 4 nodes with 4 tasks per node
    When the rank layout is computed
    Then the total rank count should be 16
    And node 0 should have ranks 0 through 3
    And node 1 should have ranks 4 through 7
    And node 2 should have ranks 8 through 11
    And node 3 should have ranks 12 through 15

  Scenario: Rank layout handles uneven distribution
    Given 3 nodes with 10 total tasks
    When the rank layout is auto-computed
    Then each node should have 4 tasks per node
    And the total rank count should be 12

  Scenario: MPI orchestrator fans out to multiple node agents
    Given an MPI orchestrator with a stub agent pool
    And 4 assigned nodes with addresses
    When I launch an MPI job with 16 tasks and 4 tasks per node
    Then the launch should succeed with a valid launch ID

  Scenario: MPI orchestrator reports partial node failure
    Given an MPI orchestrator with a failing agent pool
    And 2 assigned nodes with addresses
    When I launch an MPI job with 4 tasks and 2 tasks per node
    Then the launch should fail mentioning node rejection

  Scenario: PMI-2 wire protocol parses all command types
    Given a PMI-2 protocol parser
    When I parse "cmd=fullinit;pmi_version=2;pmi_subversion=0;"
    Then the command should be FullInit with version 2
    When I parse "cmd=kvsput;key=mykey;value=myval;"
    Then the command should be KvsPut with key "mykey" and value "myval"
    When I parse "cmd=abort;message=rank failed;"
    Then the command should be Abort with message "rank failed"

  Scenario: Process launcher sets correct environment variables
    Given a launch config for 4 ranks starting at rank 8 with world size 16
    When I compute the environment for rank 10 local rank 2
    Then the environment should contain "PMI_RANK" = "10"
    And the environment should contain "PMI_SIZE" = "16"
    And the environment should contain "LATTICE_LOCAL_RANK" = "2"
    And the environment should contain "LATTICE_LOCAL_SIZE" = "4"

  Scenario: Process launcher sets CXI credentials when provided
    Given a launch config with CXI credentials VNI 1234 and auth key "deadbeef"
    When I compute the environment for rank 0 local rank 0
    Then the environment should contain "FI_CXI_DEFAULT_VNI" = "1234"
    And the environment should contain "FI_CXI_AUTH_KEY" = "deadbeef"
    And the environment should contain "FI_PROVIDER" = "cxi"

  Scenario: Cross-node fence exchange via star topology
    Given a fence coordinator for 3 nodes with node 0 as head
    When node 0 contributes key "n0_addr" with value "10.0.0.1"
    And node 1 contributes key "n1_addr" with value "10.0.0.2"
    And node 2 contributes key "n2_addr" with value "10.0.0.3"
    And the head node executes the fence
    Then the merged KVS should contain all 3 entries
