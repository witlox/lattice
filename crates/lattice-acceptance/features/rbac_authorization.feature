Feature: RBAC Authorization
  Role-based access control enforces who can do what.

  Scenario: User can submit and list allocations
    Given a user "alice" with role "User"
    When the user attempts operation "SubmitAllocation"
    Then the operation should be allowed
    When the user attempts operation "ListAllocations"
    Then the operation should be allowed

  Scenario: User cannot manage tenants or drain nodes
    Given a user "alice" with role "User"
    When the user attempts operation "CreateTenant"
    Then the operation should be denied
    When the user attempts operation "DrainNode"
    Then the operation should be denied

  Scenario: TenantAdmin same-tenant vs cross-tenant
    Given a user "bob" with role "TenantAdmin" in tenant "physics"
    When the user attempts operation "CreateVCluster" on tenant "physics"
    Then the operation should be allowed
    When the user attempts operation "CreateVCluster" on tenant "biology"
    Then the operation should be denied

  Scenario: ClaimingUser can claim and release but not create tenants
    Given a user "dr-smith" with role "ClaimingUser"
    When the user attempts operation "ClaimNode"
    Then the operation should be allowed
    When the user attempts operation "ReleaseNode"
    Then the operation should be allowed
    When the user attempts operation "CreateTenant"
    Then the operation should be denied

  Scenario: SystemAdmin can perform all operations
    Given a user "admin" with role "SystemAdmin"
    When the user attempts operation "CreateTenant"
    Then the operation should be allowed
    When the user attempts operation "DrainNode"
    Then the operation should be allowed
    When the user attempts operation "BackupExport"
    Then the operation should be allowed

  Scenario: Operator can drain and undrain but not create tenants
    Given a user "ops" with role "Operator"
    When the user attempts operation "DrainNode"
    Then the operation should be allowed
    When the user attempts operation "UndrainNode"
    Then the operation should be allowed
    When the user attempts operation "CreateTenant"
    Then the operation should be denied

  Scenario: ReadOnly user can list but not submit
    Given a user "viewer" with role "ReadOnly"
    When the user attempts operation "ListAllocations"
    Then the operation should be allowed
    When the user attempts operation "GetAllocation"
    Then the operation should be allowed
    When the user attempts operation "SubmitAllocation"
    Then the operation should be denied
    When the user attempts operation "CancelAllocation"
    Then the operation should be denied

  Scenario: Sensitive claim requires ClaimingUser role
    Given a user "alice" with role "User"
    When the user attempts operation "ClaimNode"
    Then the operation should be denied
    Given a user "dr-smith" with role "ClaimingUser"
    When the user attempts operation "ClaimNode"
    Then the operation should be allowed

  Scenario: Federation operations require SystemAdmin
    Given a user "ops" with role "Operator"
    When the user attempts operation "FederationManage"
    Then the operation should be denied
    Given a user "admin" with role "SystemAdmin"
    When the user attempts operation "FederationManage"
    Then the operation should be allowed
