Feature: Cgroup Isolation
  Per-allocation cgroup v2 isolation via the CgroupManager trait.
  The lattice-node-agent creates a scope under workload.slice/ for each
  allocation, applies resource limits, and destroys the scope on epilogue.

  Tests run against StubCgroupManager (no real cgroups on macOS).

  # --- Hierarchy ---

  Scenario: Create hierarchy creates workload.slice
    Given a stub cgroup manager
    When the hierarchy is created
    Then hierarchy creation should succeed

  Scenario: Hierarchy creation is idempotent
    Given a stub cgroup manager
    When the hierarchy is created
    And the hierarchy is created again
    Then hierarchy creation should succeed

  # --- Scope Creation ---

  Scenario: Create scope under workload.slice
    Given a stub cgroup manager
    When a scope "alloc-42" is created under workload.slice
    Then the scope path should contain "workload.slice/alloc-42.scope"

  Scenario: Scope creation with memory limits
    Given a stub cgroup manager
    When a scope "alloc-mem" is created with memory limit 1073741824
    Then the scope should be created successfully

  Scenario: Scope creation with CPU weight
    Given a stub cgroup manager
    When a scope "alloc-cpu" is created with CPU weight 200
    Then the scope should be created successfully

  Scenario: Scope with no resource limits creates without constraints
    Given a stub cgroup manager
    When a scope "alloc-default" is created with no resource limits
    Then the scope should be created successfully
    And the scope path should contain "alloc-default.scope"

  # --- Scope Destruction ---

  Scenario: Destroy scope cleans up
    Given a stub cgroup manager
    And a scope "alloc-cleanup" exists under workload.slice
    When the scope is destroyed
    Then scope destruction should succeed

  # --- Metrics ---

  Scenario: Read metrics returns valid data
    Given a stub cgroup manager
    When metrics are read from a scope path
    Then the metrics should have zero memory usage
    And the metrics should have zero CPU usage
    And the metrics should have zero processes

  # --- Empty Check ---

  Scenario: is_scope_empty detects empty scope
    Given a stub cgroup manager
    And a scope "alloc-empty" exists under workload.slice
    When the scope emptiness is checked
    Then the scope should be empty
