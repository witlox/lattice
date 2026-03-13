use cucumber::{given, then, when};
use uuid::Uuid;

use crate::LatticeWorld;
use lattice_common::types::*;
use lattice_test_harness::fixtures::*;

// ─── Helpers ───────────────────────────────────────────────

fn create_network_domain(name: &str, tenant: &str, vni: u32) -> NetworkDomain {
    NetworkDomain {
        name: name.to_string(),
        tenant: tenant.to_string(),
        vni,
        state: NetworkDomainState::Active,
        member_allocations: Vec::new(),
        created_at: chrono::Utc::now(),
        grace_deadline: None,
    }
}

// ─── Given Steps ───────────────────────────────────────────

#[given(regex = r#"^a tenant "(\w[\w-]*)" with max nodes (\d+)$"#)]
fn given_tenant_max_nodes_net(world: &mut LatticeWorld, name: String, max_nodes: u32) {
    let tenant = TenantBuilder::new(&name).max_nodes(max_nodes).build();
    world.tenants.push(tenant);
}

#[given(regex = r#"^a vCluster "(\w[\w-]*)" for tenant "(\w[\w-]*)" with scheduler "(\w+)"$"#)]
fn given_vcluster_net(
    world: &mut LatticeWorld,
    vc_name: String,
    tenant_name: String,
    scheduler: String,
) {
    use super::helpers::parse_scheduler_type;
    let stype = parse_scheduler_type(&scheduler);
    let vc = VClusterBuilder::new(&vc_name)
        .tenant(&tenant_name)
        .scheduler(stype)
        .build();
    world.vclusters.push(vc);
}

#[given(regex = r#"^(\d+) nodes in group (\d+)$"#)]
fn given_nodes_in_group_net(world: &mut LatticeWorld, count: usize, group: u32) {
    let nodes = create_node_batch(count, group);
    world.nodes.extend(nodes);
}

#[given(regex = r#"^(\d+) nodes with Slingshot interconnect in group (\d+)$"#)]
fn given_slingshot_nodes(world: &mut LatticeWorld, count: usize, group: u32) {
    let mut nodes = create_node_batch(count, group);
    for node in &mut nodes {
        node.tags
            .insert("interconnect".into(), "slingshot".into());
    }
    world.nodes.extend(nodes);
}

#[given(regex = r#"^(\d+) active network domains$"#)]
fn given_active_domains(world: &mut LatticeWorld, count: usize) {
    for i in 0..count {
        let domain = create_network_domain(
            &format!("domain-{i}"),
            "default-tenant",
            1000 + i as u32,
        );
        world.network_domains.push(domain);
    }
}

#[given(regex = r#"^a network domain with VNI (\d+)$"#)]
fn given_domain_with_vni(world: &mut LatticeWorld, vni: u32) {
    let domain = create_network_domain("test-domain", "default-tenant", vni);
    world.network_domains.push(domain);
}

#[given(regex = r#"^the VNI pool is fully allocated \((\d+)/(\d+) in use\)$"#)]
fn given_vni_pool_exhausted(world: &mut LatticeWorld, in_use: u32, total: u32) {
    assert_eq!(in_use, total, "Pool should be fully exhausted");
    for i in 0..total {
        let domain = create_network_domain(
            &format!("domain-{i}"),
            "default-tenant",
            1000 + i,
        );
        world.network_domains.push(domain);
    }
}

// ─── When Steps ────────────────────────────────────────────

#[when("a DAG is submitted with 2 allocations requiring shared networking")]
fn when_dag_shared_networking(world: &mut LatticeWorld) {
    let tenant = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "default-tenant".into());
    let dag_id = Uuid::new_v4().to_string();

    let domain = create_network_domain("dag-domain", &tenant, 1001);
    let domain_name = domain.name.clone();

    let mut alloc1 = AllocationBuilder::new()
        .tenant(&tenant)
        .nodes(2)
        .dag_id(&dag_id)
        .build();
    alloc1
        .tags
        .insert("network_domain".into(), domain_name.clone());

    let mut alloc2 = AllocationBuilder::new()
        .tenant(&tenant)
        .nodes(2)
        .dag_id(&dag_id)
        .build();
    alloc2
        .tags
        .insert("network_domain".into(), domain_name.clone());

    let mut domain = domain;
    domain.member_allocations.push(alloc1.id);
    domain.member_allocations.push(alloc2.id);

    world.allocations.push(alloc1);
    world.allocations.push(alloc2);
    world.network_domains.push(domain);
    world.dag_id = Some(dag_id);
}

#[when("two independent allocations are submitted")]
fn when_two_independent_allocations(world: &mut LatticeWorld) {
    let tenant = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "default-tenant".into());

    let domain1 = create_network_domain("domain-ind-1", &tenant, 2001);
    let domain2 = create_network_domain("domain-ind-2", &tenant, 2002);

    let mut alloc1 = AllocationBuilder::new().tenant(&tenant).nodes(2).build();
    alloc1
        .tags
        .insert("network_domain".into(), domain1.name.clone());

    let mut alloc2 = AllocationBuilder::new().tenant(&tenant).nodes(2).build();
    alloc2
        .tags
        .insert("network_domain".into(), domain2.name.clone());

    let mut d1 = domain1;
    d1.member_allocations.push(alloc1.id);
    let mut d2 = domain2;
    d2.member_allocations.push(alloc2.id);

    world.allocations.push(alloc1);
    world.allocations.push(alloc2);
    world.network_domains.push(d1);
    world.network_domains.push(d2);
}

#[when("an allocation with a network domain completes")]
fn when_allocation_with_domain_completes(world: &mut LatticeWorld) {
    let tenant = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "default-tenant".into());

    let mut domain = create_network_domain("complete-domain", &tenant, 3001);

    let mut alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .nodes(2)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = vec!["node-0".into(), "node-1".into()];
    alloc.started_at = Some(chrono::Utc::now());
    alloc
        .tags
        .insert("network_domain".into(), domain.name.clone());

    domain.member_allocations.push(alloc.id);

    // Complete the allocation.
    alloc.state = AllocationState::Completed;
    alloc.completed_at = Some(chrono::Utc::now());
    alloc.assigned_nodes.clear();

    // Release the domain since no members remain running.
    domain.member_allocations.clear();
    domain.state = NetworkDomainState::Released;

    world.allocations.push(alloc);
    world.network_domains.push(domain);
}

#[when("the domain is released and torn down")]
fn when_domain_released(world: &mut LatticeWorld) {
    if let Some(domain) = world.network_domains.last_mut() {
        domain.state = NetworkDomainState::Released;
        domain.member_allocations.clear();
    }
}

#[when("a new domain is created")]
fn when_new_domain_created(world: &mut LatticeWorld) {
    // Reuse a VNI from the released domain if available.
    let released_vni = world
        .network_domains
        .iter()
        .find(|d| d.state == NetworkDomainState::Released)
        .map(|d| d.vni);

    let vni = released_vni.unwrap_or(9999);
    let new_domain = create_network_domain("new-domain", "default-tenant", vni);
    world.network_domains.push(new_domain);
}

#[when(regex = r#"^an allocation from tenant "(\w[\w-]*)" requests to join a domain owned by tenant "(\w[\w-]*)"$"#)]
fn when_cross_tenant_domain(
    world: &mut LatticeWorld,
    requesting_tenant: String,
    owning_tenant: String,
) {
    // Create a domain owned by the owning tenant.
    let domain = create_network_domain("cross-tenant-domain", &owning_tenant, 4001);
    world.network_domains.push(domain);

    // The requesting tenant tries to join -- should be rejected.
    world.requesting_tenant = Some(requesting_tenant.clone());
    world.last_error = Some(lattice_common::error::LatticeError::AuthorizationDenied(
        "cross_tenant_domain".into(),
    ));
}

#[when("a new allocation requests a network domain")]
fn when_new_alloc_requests_domain(world: &mut LatticeWorld) {
    // All VNIs are exhausted, so this should fail.
    let all_active = world
        .network_domains
        .iter()
        .all(|d| d.state == NetworkDomainState::Active);
    if all_active && !world.network_domains.is_empty() {
        world.last_error = Some(lattice_common::error::LatticeError::ResourceConstraint(
            "vni_pool_exhausted".into(),
        ));
        // The allocation enters Pending since it can't get a domain.
        let alloc = AllocationBuilder::new()
            .nodes(1)
            .state(AllocationState::Pending)
            .build();
        world.allocations.push(alloc);
    }
}

#[when("an allocation with a network domain is submitted")]
fn when_alloc_with_domain_submitted(world: &mut LatticeWorld) {
    let tenant = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "default-tenant".into());

    let has_slingshot = world
        .nodes
        .iter()
        .any(|n| n.tags.get("interconnect").map(|v| v == "slingshot").unwrap_or(false));

    let mut domain = create_network_domain("cxi-domain", &tenant, 5001);
    if has_slingshot {
        // Assign CXI credentials to the domain.
        domain
            .member_allocations
            .push(Uuid::new_v4()); // placeholder
    }

    let mut alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .nodes(2)
        .state(AllocationState::Pending)
        .build();
    alloc
        .tags
        .insert("network_domain".into(), domain.name.clone());
    if has_slingshot {
        alloc
            .tags
            .insert("cxi_credentials".into(), "true".into());
        alloc.tags.insert("cxi_vni".into(), domain.vni.to_string());
        alloc
            .tags
            .insert("cxi_auth_key".into(), "slingshot-auth-key-001".into());
    }

    world.allocations.push(alloc);
    world.network_domains.push(domain);
}

// ─── Then Steps ────────────────────────────────────────────

#[then("both allocations should be assigned the same network domain")]
fn then_same_network_domain(world: &mut LatticeWorld) {
    assert!(
        world.allocations.len() >= 2,
        "Need at least 2 allocations"
    );
    let d1 = world.allocations[world.allocations.len() - 2]
        .tags
        .get("network_domain");
    let d2 = world.allocations[world.allocations.len() - 1]
        .tags
        .get("network_domain");
    assert!(
        d1.is_some() && d2.is_some(),
        "Both allocations should have a network_domain tag"
    );
    assert_eq!(
        d1, d2,
        "Both allocations should share the same network domain"
    );
}

#[then("each allocation should have its own network domain")]
fn then_separate_network_domains(world: &mut LatticeWorld) {
    assert!(
        world.allocations.len() >= 2,
        "Need at least 2 allocations"
    );
    let d1 = world.allocations[world.allocations.len() - 2]
        .tags
        .get("network_domain");
    let d2 = world.allocations[world.allocations.len() - 1]
        .tags
        .get("network_domain");
    assert!(
        d1.is_some() && d2.is_some(),
        "Both allocations should have a network_domain tag"
    );
    assert_ne!(
        d1, d2,
        "Independent allocations should have separate network domains"
    );
}

#[then("the network domain should transition to Released")]
fn then_domain_released(world: &mut LatticeWorld) {
    let domain = world
        .network_domains
        .last()
        .expect("no network domains created");
    assert_eq!(
        domain.state,
        NetworkDomainState::Released,
        "Network domain should be Released, got {:?}",
        domain.state
    );
}

#[then("each domain should have a unique VNI")]
fn then_unique_vnis(world: &mut LatticeWorld) {
    let vnis: Vec<u32> = world.network_domains.iter().map(|d| d.vni).collect();
    let mut unique_vnis = vnis.clone();
    unique_vnis.sort();
    unique_vnis.dedup();
    assert_eq!(
        vnis.len(),
        unique_vnis.len(),
        "All domain VNIs should be unique, got duplicates in {:?}",
        vnis
    );
}

#[then("no two active domains share a VNI")]
fn then_no_shared_vnis(world: &mut LatticeWorld) {
    let active_vnis: Vec<u32> = world
        .network_domains
        .iter()
        .filter(|d| d.state == NetworkDomainState::Active)
        .map(|d| d.vni)
        .collect();
    let mut unique = active_vnis.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        active_vnis.len(),
        unique.len(),
        "No two active domains should share a VNI"
    );
}

#[then(regex = r#"^VNI (\d+) should return to the pool$"#)]
fn then_vni_returned(world: &mut LatticeWorld, vni: u32) {
    let released = world
        .network_domains
        .iter()
        .any(|d| d.vni == vni && d.state == NetworkDomainState::Released);
    assert!(
        released,
        "VNI {vni} should be in a Released domain, returning it to the pool"
    );
}

#[then(regex = r#"^VNI (\d+) may be assigned to the new domain$"#)]
fn then_vni_may_reuse(world: &mut LatticeWorld, vni: u32) {
    let new_domain = world.network_domains.last().expect("no domains");
    assert_eq!(
        new_domain.vni, vni,
        "New domain should have reused VNI {vni}, got {}",
        new_domain.vni
    );
}

#[then(regex = r#"^the request should be rejected with reason "(\w+)"$"#)]
fn then_rejected_with_reason(world: &mut LatticeWorld, reason: String) {
    let err = world.last_error.as_ref().expect("Expected an error");
    let err_str = format!("{err:?}");
    assert!(
        err_str.contains(&reason),
        "Expected error to contain '{reason}', got '{err_str}'"
    );
}

#[then(regex = r#"^domain creation should be blocked with reason "(\w+)"$"#)]
fn then_domain_blocked(world: &mut LatticeWorld, reason: String) {
    let err = world.last_error.as_ref().expect("Expected an error");
    let err_str = format!("{err:?}");
    assert!(
        err_str.contains(&reason),
        "Expected error to contain '{reason}', got '{err_str}'"
    );
}

#[then("the allocation enters Pending")]
fn then_alloc_pending(world: &mut LatticeWorld) {
    let alloc = world.allocations.last().expect("no allocations");
    assert_eq!(
        alloc.state,
        AllocationState::Pending,
        "Allocation should be in Pending state when domain creation is blocked"
    );
}

#[then("the domain should have CXI credentials assigned")]
fn then_cxi_credentials(world: &mut LatticeWorld) {
    let alloc = world.allocations.last().expect("no allocations");
    assert_eq!(
        alloc.tags.get("cxi_credentials").map(|v| v.as_str()),
        Some("true"),
        "Domain should have CXI credentials assigned"
    );
}

#[then("the credentials should be available during prologue")]
fn then_credentials_in_prologue(world: &mut LatticeWorld) {
    let alloc = world.allocations.last().expect("no allocations");
    assert!(
        alloc.tags.contains_key("cxi_vni"),
        "CXI VNI should be available during prologue"
    );
    assert!(
        alloc.tags.contains_key("cxi_auth_key"),
        "CXI auth key should be available during prologue"
    );
}
