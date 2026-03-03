//! Role-Based Access Control (RBAC) middleware.
//!
//! Maps OIDC token claims to [`Role`]s and evaluates whether a given role
//! is allowed to perform an [`Operation`] within an [`RbacContext`].
//!
//! The [`RbacPolicy`] encodes the authorization matrix:
//! - `SystemAdmin` -- unrestricted access.
//! - `TenantAdmin` -- tenant-scoped management plus all `User` permissions.
//! - `ClaimingUser` -- medical node claim/release plus all `User` permissions.
//! - `User` -- workload lifecycle, read-only cluster views, own-resource mutations.

use super::oidc::TokenClaims;
use serde::{Deserialize, Serialize};

// ---- Role ----------------------------------------------------------------

/// Authorization role derived from OIDC token scopes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// Regular user -- can manage own workloads.
    #[default]
    User,
    /// Tenant administrator -- can manage resources within their tenant.
    TenantAdmin,
    /// System administrator -- full, unrestricted access.
    SystemAdmin,
    /// Medical claiming user -- can claim and release medical nodes.
    ClaimingUser,
}

// ---- Operation -----------------------------------------------------------

/// An action that can be performed against the Lattice API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    // Allocation lifecycle
    SubmitAllocation,
    GetAllocation,
    ListAllocations,
    CancelAllocation,
    UpdateAllocation,
    WatchAllocation,
    AttachAllocation,
    StreamLogs,
    QueryMetrics,
    StreamMetrics,
    GetDiagnostics,
    CompareMetrics,
    CheckpointAllocation,

    // DAG operations
    GetDag,
    ListDags,
    CancelDag,
    LaunchTasks,

    // Node operations
    ListNodes,
    GetNode,
    DrainNode,
    UndrainNode,
    DisableNode,

    // Tenant / vCluster administration
    CreateTenant,
    UpdateTenant,
    CreateVCluster,
    UpdateVCluster,

    // Cluster administration
    GetRaftStatus,
    BackupVerify,
    QueryAudit,

    // Medical node claiming
    ClaimNode,
    ReleaseNode,
}

// ---- RbacContext ----------------------------------------------------------

/// Contextual information used when evaluating an authorization decision.
#[derive(Debug, Clone)]
pub struct RbacContext {
    /// Identity of the requesting user (OIDC `sub`).
    pub requesting_user: String,
    /// Tenant the requesting user belongs to, if known.
    pub requesting_tenant: Option<String>,
    /// Tenant that owns the target resource.
    pub target_tenant: Option<String>,
    /// Owner (user) of the target allocation, if applicable.
    pub target_owner: Option<String>,
}

// ---- RbacPolicy ----------------------------------------------------------

/// Stateless policy evaluator for RBAC decisions.
pub struct RbacPolicy;

impl RbacPolicy {
    /// Returns `true` if `role` is permitted to perform `op` in the given
    /// `context`.
    pub fn is_allowed(role: &Role, op: &Operation, context: &RbacContext) -> bool {
        match role {
            Role::SystemAdmin => true,

            Role::ClaimingUser => {
                matches!(op, Operation::ClaimNode | Operation::ReleaseNode)
                    || Self::is_allowed(&Role::User, op, context)
            }

            Role::TenantAdmin => {
                match op {
                    // Tenant-scoped management: allowed only when operating on
                    // own tenant.
                    Operation::CreateVCluster | Operation::UpdateTenant => {
                        Self::same_tenant(context)
                    }

                    // Additional admin-level node/audit operations.
                    Operation::DrainNode | Operation::QueryAudit => true,

                    // Ownership checks are relaxed for the tenant admin: they
                    // can cancel/update allocations within their own tenant.
                    Operation::CancelAllocation | Operation::UpdateAllocation => {
                        Self::owner_or_same_tenant(context)
                    }

                    // Everything the base User role can do.
                    _ => Self::is_allowed(&Role::User, op, context),
                }
            }

            Role::User => match op {
                // Workload lifecycle -- broad access.
                Operation::SubmitAllocation
                | Operation::GetAllocation
                | Operation::ListAllocations
                | Operation::WatchAllocation
                | Operation::AttachAllocation
                | Operation::StreamLogs
                | Operation::QueryMetrics
                | Operation::StreamMetrics
                | Operation::GetDiagnostics
                | Operation::CompareMetrics
                | Operation::CheckpointAllocation
                | Operation::GetDag
                | Operation::ListDags
                | Operation::CancelDag
                | Operation::LaunchTasks
                | Operation::ListNodes
                | Operation::GetNode => true,

                // Mutations on own allocations only.
                Operation::CancelAllocation | Operation::UpdateAllocation => {
                    Self::is_owner(context)
                }

                // Everything else is denied for a regular user.
                Operation::DrainNode
                | Operation::UndrainNode
                | Operation::DisableNode
                | Operation::CreateTenant
                | Operation::UpdateTenant
                | Operation::CreateVCluster
                | Operation::UpdateVCluster
                | Operation::GetRaftStatus
                | Operation::BackupVerify
                | Operation::QueryAudit
                | Operation::ClaimNode
                | Operation::ReleaseNode => false,
            },
        }
    }

    // ---- helpers ---------------------------------------------------------

    /// `true` when the target owner matches the requesting user, or when no
    /// owner is specified (permissive by default for list-level operations).
    fn is_owner(ctx: &RbacContext) -> bool {
        match &ctx.target_owner {
            Some(owner) => owner == &ctx.requesting_user,
            None => true,
        }
    }

    /// `true` when the requesting and target tenants match.
    fn same_tenant(ctx: &RbacContext) -> bool {
        match (&ctx.requesting_tenant, &ctx.target_tenant) {
            (Some(req), Some(tgt)) => req == tgt,
            // If either tenant is unknown we cannot confirm a match.
            _ => false,
        }
    }

    /// `true` when the user owns the resource OR the tenants match (for
    /// `TenantAdmin` cancel/update).
    fn owner_or_same_tenant(ctx: &RbacContext) -> bool {
        Self::is_owner(ctx) || Self::same_tenant(ctx)
    }
}

// ---- Role derivation from OIDC claims ------------------------------------

/// Determine the [`Role`] from the scopes embedded in validated OIDC
/// [`TokenClaims`].
///
/// Precedence (first match wins):
/// 1. `"admin"` or `"system:admin"` --> `SystemAdmin`
/// 2. `"tenant:admin"` --> `TenantAdmin`
/// 3. `"medical:claim"` --> `ClaimingUser`
/// 4. Anything else --> `User`
pub fn derive_role(claims: &TokenClaims) -> Role {
    for scope in &claims.scopes {
        if scope == "admin" || scope == "system:admin" {
            return Role::SystemAdmin;
        }
    }
    for scope in &claims.scopes {
        if scope == "tenant:admin" {
            return Role::TenantAdmin;
        }
    }
    for scope in &claims.scopes {
        if scope == "medical:claim" {
            return Role::ClaimingUser;
        }
    }
    Role::User
}

// ---- gRPC method mapping -------------------------------------------------

/// Map a fully-qualified gRPC method path to the corresponding [`Operation`].
///
/// Returns `None` for unrecognized paths.
pub fn operation_from_grpc_method(method: &str) -> Option<Operation> {
    match method {
        // AllocationService
        "/lattice.v1.AllocationService/Submit" => Some(Operation::SubmitAllocation),
        "/lattice.v1.AllocationService/Get" => Some(Operation::GetAllocation),
        "/lattice.v1.AllocationService/List" => Some(Operation::ListAllocations),
        "/lattice.v1.AllocationService/Cancel" => Some(Operation::CancelAllocation),
        "/lattice.v1.AllocationService/Update" => Some(Operation::UpdateAllocation),
        "/lattice.v1.AllocationService/Watch" => Some(Operation::WatchAllocation),
        "/lattice.v1.AllocationService/Attach" => Some(Operation::AttachAllocation),
        "/lattice.v1.AllocationService/StreamLogs" => Some(Operation::StreamLogs),
        "/lattice.v1.AllocationService/QueryMetrics" => Some(Operation::QueryMetrics),
        "/lattice.v1.AllocationService/StreamMetrics" => Some(Operation::StreamMetrics),
        "/lattice.v1.AllocationService/GetDiagnostics" => Some(Operation::GetDiagnostics),
        "/lattice.v1.AllocationService/CompareMetrics" => Some(Operation::CompareMetrics),
        "/lattice.v1.AllocationService/Checkpoint" => Some(Operation::CheckpointAllocation),

        // DagService
        "/lattice.v1.DagService/Get" => Some(Operation::GetDag),
        "/lattice.v1.DagService/List" => Some(Operation::ListDags),
        "/lattice.v1.DagService/Cancel" => Some(Operation::CancelDag),
        "/lattice.v1.DagService/LaunchTasks" => Some(Operation::LaunchTasks),

        // NodeService
        "/lattice.v1.NodeService/List" => Some(Operation::ListNodes),
        "/lattice.v1.NodeService/Get" => Some(Operation::GetNode),
        "/lattice.v1.NodeService/DrainNode" => Some(Operation::DrainNode),
        "/lattice.v1.NodeService/UndrainNode" => Some(Operation::UndrainNode),
        "/lattice.v1.NodeService/DisableNode" => Some(Operation::DisableNode),
        "/lattice.v1.NodeService/ClaimNode" => Some(Operation::ClaimNode),
        "/lattice.v1.NodeService/ReleaseNode" => Some(Operation::ReleaseNode),

        // AdminService
        "/lattice.v1.AdminService/CreateTenant" => Some(Operation::CreateTenant),
        "/lattice.v1.AdminService/UpdateTenant" => Some(Operation::UpdateTenant),
        "/lattice.v1.AdminService/CreateVCluster" => Some(Operation::CreateVCluster),
        "/lattice.v1.AdminService/UpdateVCluster" => Some(Operation::UpdateVCluster),
        "/lattice.v1.AdminService/GetRaftStatus" => Some(Operation::GetRaftStatus),
        "/lattice.v1.AdminService/BackupVerify" => Some(Operation::BackupVerify),
        "/lattice.v1.AdminService/QueryAudit" => Some(Operation::QueryAudit),

        _ => None,
    }
}

// ---- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build an `RbacContext` with sensible defaults.
    fn ctx(user: &str) -> RbacContext {
        RbacContext {
            requesting_user: user.to_string(),
            requesting_tenant: None,
            target_tenant: None,
            target_owner: None,
        }
    }

    fn ctx_with_tenants(user: &str, req_tenant: &str, tgt_tenant: &str) -> RbacContext {
        RbacContext {
            requesting_user: user.to_string(),
            requesting_tenant: Some(req_tenant.to_string()),
            target_tenant: Some(tgt_tenant.to_string()),
            target_owner: None,
        }
    }

    fn ctx_with_owner(user: &str, owner: &str) -> RbacContext {
        RbacContext {
            requesting_user: user.to_string(),
            requesting_tenant: None,
            target_tenant: None,
            target_owner: Some(owner.to_string()),
        }
    }

    fn claims_with_scopes(scopes: &[&str]) -> TokenClaims {
        TokenClaims {
            sub: "testuser".to_string(),
            exp: i64::MAX,
            iss: "https://auth.example.com".to_string(),
            aud: "lattice-api".to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        }
    }

    // 1. SystemAdmin can do everything.
    #[test]
    fn admin_can_do_everything() {
        let c = ctx("admin-user");
        assert!(RbacPolicy::is_allowed(
            &Role::SystemAdmin,
            &Operation::DrainNode,
            &c
        ));
        assert!(RbacPolicy::is_allowed(
            &Role::SystemAdmin,
            &Operation::CreateTenant,
            &c
        ));
        assert!(RbacPolicy::is_allowed(
            &Role::SystemAdmin,
            &Operation::BackupVerify,
            &c
        ));
        assert!(RbacPolicy::is_allowed(
            &Role::SystemAdmin,
            &Operation::ClaimNode,
            &c
        ));
        assert!(RbacPolicy::is_allowed(
            &Role::SystemAdmin,
            &Operation::SubmitAllocation,
            &c
        ));
    }

    // 2. User can submit and list.
    #[test]
    fn user_can_submit_and_list() {
        let c = ctx("alice");
        assert!(RbacPolicy::is_allowed(
            &Role::User,
            &Operation::SubmitAllocation,
            &c
        ));
        assert!(RbacPolicy::is_allowed(
            &Role::User,
            &Operation::ListAllocations,
            &c
        ));
        assert!(RbacPolicy::is_allowed(
            &Role::User,
            &Operation::GetAllocation,
            &c
        ));
    }

    // 3. User cannot drain nodes or create tenants.
    #[test]
    fn user_cannot_drain_nodes() {
        let c = ctx("alice");
        assert!(!RbacPolicy::is_allowed(
            &Role::User,
            &Operation::DrainNode,
            &c
        ));
        assert!(!RbacPolicy::is_allowed(
            &Role::User,
            &Operation::DisableNode,
            &c
        ));
        assert!(!RbacPolicy::is_allowed(
            &Role::User,
            &Operation::CreateTenant,
            &c
        ));
    }

    // 4. TenantAdmin can manage own tenant.
    #[test]
    fn tenant_admin_can_manage_own_tenant() {
        let c = ctx_with_tenants("bob", "physics", "physics");
        assert!(RbacPolicy::is_allowed(
            &Role::TenantAdmin,
            &Operation::CreateVCluster,
            &c
        ));
        assert!(RbacPolicy::is_allowed(
            &Role::TenantAdmin,
            &Operation::UpdateTenant,
            &c
        ));
    }

    // 5. TenantAdmin cannot manage a different tenant.
    #[test]
    fn tenant_admin_cannot_manage_other_tenant() {
        let c = ctx_with_tenants("bob", "physics", "biology");
        assert!(!RbacPolicy::is_allowed(
            &Role::TenantAdmin,
            &Operation::UpdateTenant,
            &c
        ));
        assert!(!RbacPolicy::is_allowed(
            &Role::TenantAdmin,
            &Operation::CreateVCluster,
            &c
        ));
    }

    // 6. ClaimingUser can claim and release nodes.
    #[test]
    fn claiming_user_can_claim_nodes() {
        let c = ctx("dr-smith");
        assert!(RbacPolicy::is_allowed(
            &Role::ClaimingUser,
            &Operation::ClaimNode,
            &c
        ));
        assert!(RbacPolicy::is_allowed(
            &Role::ClaimingUser,
            &Operation::ReleaseNode,
            &c
        ));
        // Also has regular user capabilities.
        assert!(RbacPolicy::is_allowed(
            &Role::ClaimingUser,
            &Operation::SubmitAllocation,
            &c
        ));
    }

    // 7. User can cancel own allocation.
    #[test]
    fn user_can_cancel_own_allocation() {
        let c = ctx_with_owner("alice", "alice");
        assert!(RbacPolicy::is_allowed(
            &Role::User,
            &Operation::CancelAllocation,
            &c
        ));
    }

    // 8. User cannot cancel another user's allocation.
    #[test]
    fn user_cannot_cancel_others_allocation() {
        let c = ctx_with_owner("alice", "bob");
        assert!(!RbacPolicy::is_allowed(
            &Role::User,
            &Operation::CancelAllocation,
            &c
        ));
        assert!(!RbacPolicy::is_allowed(
            &Role::User,
            &Operation::UpdateAllocation,
            &c
        ));
    }

    // 9. derive_role maps scopes correctly.
    #[test]
    fn derive_role_from_scopes() {
        assert_eq!(
            derive_role(&claims_with_scopes(&["admin"])),
            Role::SystemAdmin
        );
        assert_eq!(
            derive_role(&claims_with_scopes(&["system:admin"])),
            Role::SystemAdmin
        );
        assert_eq!(
            derive_role(&claims_with_scopes(&["tenant:admin"])),
            Role::TenantAdmin
        );
        assert_eq!(
            derive_role(&claims_with_scopes(&["medical:claim"])),
            Role::ClaimingUser
        );
        assert_eq!(
            derive_role(&claims_with_scopes(&["jobs:read", "jobs:write"])),
            Role::User
        );
    }

    // 10. operation_from_grpc_method maps all known methods.
    #[test]
    fn operation_from_grpc_method_mapping() {
        assert_eq!(
            operation_from_grpc_method("/lattice.v1.AllocationService/Submit"),
            Some(Operation::SubmitAllocation)
        );
        assert_eq!(
            operation_from_grpc_method("/lattice.v1.AllocationService/Cancel"),
            Some(Operation::CancelAllocation)
        );
        assert_eq!(
            operation_from_grpc_method("/lattice.v1.NodeService/DrainNode"),
            Some(Operation::DrainNode)
        );
        assert_eq!(
            operation_from_grpc_method("/lattice.v1.AdminService/CreateTenant"),
            Some(Operation::CreateTenant)
        );
        assert_eq!(
            operation_from_grpc_method("/lattice.v1.AdminService/QueryAudit"),
            Some(Operation::QueryAudit)
        );
        assert_eq!(
            operation_from_grpc_method("/lattice.v1.NodeService/ClaimNode"),
            Some(Operation::ClaimNode)
        );
        assert_eq!(
            operation_from_grpc_method("/lattice.v1.DagService/LaunchTasks"),
            Some(Operation::LaunchTasks)
        );
        assert_eq!(operation_from_grpc_method("/unknown.Service/Method"), None);
    }

    // 11. Default role is User.
    #[test]
    fn default_role_is_user() {
        assert_eq!(Role::default(), Role::User);
    }

    // 12. derive_role with no matching scopes defaults to User.
    #[test]
    fn missing_role_defaults_to_user() {
        assert_eq!(derive_role(&claims_with_scopes(&[])), Role::User);
        assert_eq!(
            derive_role(&claims_with_scopes(&["openid", "profile"])),
            Role::User
        );
    }
}
