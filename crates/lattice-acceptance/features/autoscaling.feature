Feature: Autoscaling
  Reactive lifecycle allocations scale based on metrics.

  Scenario: Scale up on high queue pressure
    Given a tenant "inference" with max nodes 100
    And a vCluster "serve-vc" for tenant "inference" with scheduler "service_bin_pack"
    And 4 nodes in group 0
    And a running reactive allocation using 2 nodes
    When queue pressure exceeds the scale-up threshold
    Then the autoscaler should recommend scale up

  Scenario: Scale down on low utilization
    Given a tenant "inference" with max nodes 100
    And a vCluster "serve-vc" for tenant "inference" with scheduler "service_bin_pack"
    And 8 nodes in group 0
    And a running reactive allocation using 6 nodes
    When utilization drops below the scale-down threshold
    Then the autoscaler should recommend scale down

  Scenario: Cooldown prevents rapid oscillation
    Given a tenant "inference" with max nodes 100
    And a vCluster "serve-vc" for tenant "inference" with scheduler "service_bin_pack"
    And 4 nodes in group 0
    And a recent scale-up event within cooldown period
    When another scale-up is evaluated
    Then the autoscaler should hold due to cooldown
