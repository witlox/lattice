Feature: Network Domains
  Allocations sharing a network domain get L3 reachability.

  Scenario: DAG allocations share network domain
    Given a tenant "ml-team" with max nodes 100
    And a vCluster "train-vc" for tenant "ml-team" with scheduler "hpc_backfill"
    And 8 nodes in group 0
    When a DAG is submitted with 2 allocations requiring shared networking
    Then both allocations should be assigned the same network domain

  Scenario: Independent allocations get separate domains
    Given a tenant "multi" with max nodes 100
    And a vCluster "batch-vc" for tenant "multi" with scheduler "hpc_backfill"
    And 8 nodes in group 0
    When two independent allocations are submitted
    Then each allocation should have its own network domain

  Scenario: Network domain released after allocation completes
    Given a tenant "physics" with max nodes 100
    And a vCluster "sim-vc" for tenant "physics" with scheduler "hpc_backfill"
    And 4 nodes in group 0
    When an allocation with a network domain completes
    Then the network domain should transition to Released

  Scenario: VNI uniqueness enforced across active domains
    Given 2 active network domains
    Then each domain should have a unique VNI
    And no two active domains share a VNI

  Scenario: VNI reused after domain teardown
    Given a network domain with VNI 1001
    When the domain is released and torn down
    Then VNI 1001 should return to the pool
    When a new domain is created
    Then VNI 1001 may be assigned to the new domain

  Scenario: Cross-tenant network domain prevented
    Given a tenant "physics" with max nodes 100
    And a tenant "biology" with max nodes 100
    When an allocation from tenant "physics" requests to join a domain owned by tenant "biology"
    Then the request should be rejected with reason "cross_tenant_domain"

  Scenario: VNI pool exhaustion blocks new domain creation
    Given the VNI pool is fully allocated (3095/3095 in use)
    When a new allocation requests a network domain
    Then domain creation should be blocked with reason "vni_pool_exhausted"
    And the allocation enters Pending

  Scenario: Domain with CXI credentials for Slingshot
    Given a tenant "physics" with max nodes 100
    And 4 nodes with Slingshot interconnect in group 0
    When an allocation with a network domain is submitted
    Then the domain should have CXI credentials assigned
    And the credentials should be available during prologue
