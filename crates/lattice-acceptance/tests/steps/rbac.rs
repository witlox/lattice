use cucumber::{given, then, when};

use super::helpers::{parse_operation, parse_role};
use crate::LatticeWorld;
use lattice_api::middleware::rbac::{RbacContext, RbacPolicy};

// ─── Given Steps ───────────────────────────────────────────

#[given(regex = r#"^a user "(\w[\w-]*)" with role "(\w+)"$"#)]
fn given_user_with_role(world: &mut LatticeWorld, user: String, role_str: String) {
    let role = parse_role(&role_str);
    world.current_user = Some(user);
    world.current_role = Some(role);
    world.requesting_tenant = None;
}

#[given(regex = r#"^a user "(\w[\w-]*)" with role "(\w+)" in tenant "(\w[\w-]*)"$"#)]
fn given_user_with_role_in_tenant(
    world: &mut LatticeWorld,
    user: String,
    role_str: String,
    tenant: String,
) {
    let role = parse_role(&role_str);
    world.current_user = Some(user);
    world.current_role = Some(role);
    world.requesting_tenant = Some(tenant);
}

// ─── When Steps ────────────────────────────────────────────

#[when(regex = r#"^the user attempts operation "(\w+)"$"#)]
fn attempt_operation(world: &mut LatticeWorld, op_str: String) {
    let role = world.current_role.as_ref().expect("no role set").clone();
    let user = world
        .current_user
        .as_deref()
        .unwrap_or("unknown")
        .to_string();
    let op = parse_operation(&op_str);

    let context = RbacContext {
        requesting_user: user,
        requesting_tenant: world.requesting_tenant.clone(),
        target_tenant: None,
        target_owner: None,
    };

    let allowed = RbacPolicy::is_allowed(&role, &op, &context);
    world.last_rbac_result = Some(allowed);
}

#[when(regex = r#"^the user attempts operation "(\w+)" on tenant "(\w[\w-]*)"$"#)]
fn attempt_operation_on_tenant(world: &mut LatticeWorld, op_str: String, target_tenant: String) {
    let role = world.current_role.as_ref().expect("no role set").clone();
    let user = world
        .current_user
        .as_deref()
        .unwrap_or("unknown")
        .to_string();
    let op = parse_operation(&op_str);

    let context = RbacContext {
        requesting_user: user,
        requesting_tenant: world.requesting_tenant.clone(),
        target_tenant: Some(target_tenant),
        target_owner: None,
    };

    let allowed = RbacPolicy::is_allowed(&role, &op, &context);
    world.last_rbac_result = Some(allowed);
}

// ─── Then Steps ────────────────────────────────────────────

#[then("the operation should be allowed")]
fn operation_allowed(world: &mut LatticeWorld) {
    let result = world.last_rbac_result.expect("no RBAC result recorded");
    assert!(
        result,
        "Expected operation to be allowed, but it was denied"
    );
}

#[then("the operation should be denied")]
fn operation_denied(world: &mut LatticeWorld) {
    let result = world.last_rbac_result.expect("no RBAC result recorded");
    assert!(
        !result,
        "Expected operation to be denied, but it was allowed"
    );
}
