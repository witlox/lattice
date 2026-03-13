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

  Scenario: Scale up respects tenant max_nodes quota
    Given a tenant "inference" with max nodes 10
    And a vCluster "serve-vc" for tenant "inference" with scheduler "service_bin_pack"
    And a running reactive allocation using 9 nodes
    When queue pressure exceeds the scale-up threshold
    Then the autoscaler should recommend scale up to at most 10 nodes
    And the quota should not be exceeded

  Scenario: Scale down respects allocation min_nodes
    Given a tenant "inference" with max nodes 100
    And a vCluster "serve-vc" for tenant "inference" with scheduler "service_bin_pack"
    And a running reactive allocation using 6 nodes with min_nodes 4
    When utilization drops below the scale-down threshold
    Then the autoscaler should recommend scale down to no fewer than 4 nodes

  Scenario: Scale to zero blocked by min_nodes constraint
    Given a tenant "inference" with max nodes 100
    And a running reactive allocation using 2 nodes with min_nodes 1
    When utilization drops to zero
    Then the autoscaler should recommend scale down to 1 node
    And the allocation should not be terminated

  Scenario: Metric unavailable pauses autoscaling
    Given a tenant "inference" with max nodes 100
    And a running reactive allocation using 4 nodes
    And the TSDB is unreachable
    When an autoscaling evaluation occurs
    Then the autoscaler should take no action
    And the allocation should remain at 4 nodes

  Scenario: Multiple reactive allocations evaluated independently
    Given a tenant "inference" with max nodes 100
    And a vCluster "serve-vc" for tenant "inference" with scheduler "service_bin_pack"
    And reactive allocation "service-a" using 4 nodes
    And reactive allocation "service-b" using 2 nodes
    When autoscaling is evaluated
    Then "service-a" and "service-b" should be evaluated independently
    And scaling decisions should not interfere

  Scenario: Independent cooldown timers for scale up and scale down
    Given a tenant "inference" with max nodes 100
    And a running reactive allocation using 4 nodes
    And a recent scale-up event within cooldown period
    When a scale-down is evaluated
    Then the scale-down cooldown should be checked independently
    And scale down may proceed if its own cooldown has expired
