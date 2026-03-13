use std::collections::HashMap;

use cucumber::{given, then, when};
use uuid::Uuid;

use crate::LatticeWorld;
use super::helpers::parse_allocation_state;
use lattice_common::types::*;
use lattice_scheduler::cycle::{run_cycle, CycleInput};
use lattice_scheduler::dag::{validate_dag, resolve_dependencies, DEFAULT_MAX_DAG_SIZE, DagError};
use lattice_scheduler::placement::PlacementDecision;
use lattice_scheduler::resource_timeline::TimelineConfig;
use lattice_test_harness::fixtures::*;

// ─── Helpers ───────────────────────────────────────────────

/// Parse a dependency condition string from the feature file.
fn parse_dep_condition(s: &str) -> DependencyCondition {
    match s.to_lowercase().as_str() {
        "success" => DependencyCondition::Success,
        "any" => DependencyCondition::Any,
        "notok" | "failure" => DependencyCondition::Failure,
        "corresponding" => DependencyCondition::Corresponding,
        "mutex" => DependencyCondition::Mutex,
        other => panic!("Unknown dependency condition: {other}"),
    }
}

/// Parse a depends_on cell like "preprocess:success" or "shard-a:success,shard-b:success".
/// Returns a list of (name, condition) pairs.
fn parse_depends_on_cell(cell: &str) -> Vec<(String, DependencyCondition)> {
    if cell.trim().is_empty() {
        return Vec::new();
    }
    cell.split(',')
        .map(|dep| {
            let dep = dep.trim();
            let parts: Vec<&str> = dep.splitn(2, ':').collect();
            assert_eq!(
                parts.len(),
                2,
                "depends_on entry must be 'name:condition', got '{dep}'"
            );
            (parts[0].to_string(), parse_dep_condition(parts[1]))
        })
        .collect()
}

// ─── When Steps ────────────────────────────────────────────

#[when("I submit a DAG with stages:")]
fn submit_dag(world: &mut LatticeWorld, step: &cucumber::gherkin::Step) {
    let table = step.table.as_ref().expect("Expected a data table");
    let dag_id = Uuid::new_v4().to_string();
    world.dag_id = Some(dag_id.clone());

    let tenant = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());

    // First pass: create allocations with placeholder names stored as tags.
    // We need to know all names before wiring up dependencies (which use UUIDs).
    let mut name_to_id: HashMap<String, Uuid> = HashMap::new();
    let mut rows_parsed: Vec<(String, u32, Vec<(String, DependencyCondition)>)> = Vec::new();

    for row in table.rows.iter().skip(1) {
        // columns: id | nodes | depends_on
        let name = row[0].trim().to_string();
        let nodes: u32 = row[1].trim().parse().expect("nodes must be a number");
        let deps = parse_depends_on_cell(row[2].trim());

        // Pre-generate a UUID for this allocation
        let id = Uuid::new_v4();
        name_to_id.insert(name.clone(), id);
        rows_parsed.push((name, nodes, deps));
    }

    // Second pass: build allocations with real UUID-based dependencies
    for (name, nodes, deps) in &rows_parsed {
        let id = name_to_id[name];
        let mut builder = AllocationBuilder::new()
            .tenant(&tenant)
            .nodes(*nodes)
            .dag_id(&dag_id)
            .tag("dag_stage_name", name);

        for (dep_name, condition) in deps {
            let dep_id = name_to_id
                .get(dep_name)
                .unwrap_or_else(|| panic!("Unknown dependency stage: {dep_name}"));
            builder = builder.depends_on(&dep_id.to_string(), condition.clone());
        }

        let mut alloc = builder.build();
        // Override the randomly generated ID with our pre-generated one
        alloc.id = id;

        world.named_allocations.insert(name.clone(), alloc.clone());
        world.allocations.push(alloc);
    }
}

#[given(regex = r#"^I submit a DAG with (\d+) stages$"#)]
#[when(regex = r#"^I submit a DAG with (\d+) stages$"#)]
fn submit_large_dag(world: &mut LatticeWorld, count: usize) {
    let dag_id = Uuid::new_v4().to_string();
    world.dag_id = Some(dag_id.clone());

    let tenant = world
        .tenants
        .last()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| "test-tenant".into());

    for i in 0..count {
        let name = format!("stage-{i}");
        let alloc = AllocationBuilder::new()
            .tenant(&tenant)
            .nodes(1)
            .dag_id(&dag_id)
            .tag("dag_stage_name", &name)
            .build();
        world.named_allocations.insert(name, alloc.clone());
        world.allocations.push(alloc);
    }
}

#[when("the scheduler runs a DAG cycle")]
fn scheduler_runs_dag_cycle(world: &mut LatticeWorld) {
    let topology = create_test_topology(1, world.nodes.len());
    let weights = CostWeights::default();

    // Separate DAG allocations into pending (with unsatisfied deps) and schedulable
    let dag_id = world.dag_id.as_ref().expect("No DAG submitted");
    let dag_allocs: Vec<&Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.dag_id.as_deref() == Some(dag_id))
        .collect();

    // Terminal states for dependency resolution
    let terminal_states: HashMap<AllocId, AllocationState> = dag_allocs
        .iter()
        .filter(|a| a.state.is_terminal())
        .map(|a| (a.id, a.state.clone()))
        .collect();

    // Root allocations (no deps) that are still Pending are schedulable
    let mut pending = Vec::new();
    let running: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.state == AllocationState::Running)
        .cloned()
        .collect();

    for alloc in &dag_allocs {
        if alloc.state != AllocationState::Pending {
            continue;
        }
        if alloc.depends_on.is_empty() {
            // Root — always eligible
            pending.push((*alloc).clone());
        } else {
            // Check if all dependencies are satisfied
            let all_satisfied = alloc.depends_on.iter().all(|dep| {
                // Find the dep's allocation and check its terminal state
                dag_allocs
                    .iter()
                    .find(|a| a.id.to_string() == dep.ref_id)
                    .and_then(|dep_alloc| terminal_states.get(&dep_alloc.id))
                    .is_some_and(|state| {
                        lattice_scheduler::dag::is_condition_satisfied(&dep.condition, state)
                    })
            });
            if all_satisfied {
                pending.push((*alloc).clone());
            }
        }
    }

    if pending.is_empty() {
        return;
    }

    let tenants = if world.tenants.is_empty() {
        vec![TenantBuilder::new("test-tenant").build()]
    } else {
        world.tenants.clone()
    };

    let input = CycleInput {
        pending: pending.clone(),
        running,
        nodes: world.nodes.clone(),
        tenants,
        topology,
        data_readiness: HashMap::new(),
        energy_price: 0.5,
        timeline_config: TimelineConfig::default(),
    };

    let result = run_cycle(&input, &weights);

    // Apply placement decisions to world state
    for decision in &result.decisions {
        match decision {
            PlacementDecision::Place {
                allocation_id,
                nodes,
            }
            | PlacementDecision::Backfill {
                allocation_id,
                nodes,
                ..
            } => {
                // Update the allocation in world.allocations
                if let Some(alloc) = world.allocations.iter_mut().find(|a| a.id == *allocation_id) {
                    alloc.state = AllocationState::Running;
                    alloc.assigned_nodes = nodes.clone();
                    alloc.started_at = Some(chrono::Utc::now());
                }
                // Also update named_allocations
                for (_, named) in world.named_allocations.iter_mut() {
                    if named.id == *allocation_id {
                        named.state = AllocationState::Running;
                        named.assigned_nodes = nodes.clone();
                        named.started_at = Some(chrono::Utc::now());
                    }
                }
            }
            _ => {}
        }
    }
}

#[when(regex = r#"^"(\w[\w-]*)" transitions to "(\w+)"$"#)]
fn named_alloc_transitions(world: &mut LatticeWorld, name: String, target_str: String) {
    let target = parse_allocation_state(&target_str);

    // Update in named_allocations
    let alloc = world
        .named_allocations
        .get_mut(&name)
        .unwrap_or_else(|| panic!("No named allocation '{name}'"));
    alloc.state = target.clone();
    if target == AllocationState::Running && alloc.started_at.is_none() {
        alloc.started_at = Some(chrono::Utc::now());
    }
    if target == AllocationState::Completed || target == AllocationState::Failed {
        alloc.completed_at = Some(chrono::Utc::now());
    }

    let alloc_id = alloc.id;

    // Sync back to world.allocations
    if let Some(wa) = world.allocations.iter_mut().find(|a| a.id == alloc_id) {
        wa.state = target;
        wa.started_at = alloc.started_at;
        wa.completed_at = alloc.completed_at;
    }
}

#[when("I cancel the DAG")]
fn cancel_dag(world: &mut LatticeWorld) {
    let dag_id = world.dag_id.as_ref().expect("No DAG submitted").clone();

    // Cancel all non-terminal DAG allocations
    for alloc in world.allocations.iter_mut() {
        if alloc.dag_id.as_deref() == Some(&dag_id) && !alloc.state.is_terminal() {
            // Running allocations get cancelled too
            alloc.state = AllocationState::Cancelled;
            alloc.completed_at = Some(chrono::Utc::now());
            alloc.assigned_nodes.clear();
        }
    }

    // Sync to named_allocations
    for (_, named) in world.named_allocations.iter_mut() {
        if named.dag_id.as_deref() == Some(&dag_id) && !named.state.is_terminal() {
            named.state = AllocationState::Cancelled;
            named.completed_at = Some(chrono::Utc::now());
            named.assigned_nodes.clear();
        }
    }
}

// ─── Then Steps ────────────────────────────────────────────

#[then(regex = r#"^the DAG should have (\d+) allocations$"#)]
fn dag_has_allocations(world: &mut LatticeWorld, expected: usize) {
    let dag_id = world.dag_id.as_ref().expect("No DAG submitted");
    let count = world
        .allocations
        .iter()
        .filter(|a| a.dag_id.as_deref() == Some(dag_id))
        .count();
    assert_eq!(
        count, expected,
        "Expected {expected} DAG allocations, got {count}"
    );
}

#[then(regex = r#"^"(\w[\w-]*)" should have no dependencies$"#)]
fn no_dependencies(world: &mut LatticeWorld, name: String) {
    let alloc = world
        .named_allocations
        .get(&name)
        .unwrap_or_else(|| panic!("No named allocation '{name}'"));
    assert!(
        alloc.depends_on.is_empty(),
        "Expected '{name}' to have no dependencies, but found {:?}",
        alloc.depends_on
    );
}

#[then(regex = r#"^"(\w[\w-]*)" should depend on "(\w[\w-]*)" with condition "(\w+)"$"#)]
fn has_dependency(world: &mut LatticeWorld, name: String, dep_name: String, condition: String) {
    let dep_alloc = world
        .named_allocations
        .get(&dep_name)
        .unwrap_or_else(|| panic!("No named allocation '{dep_name}'"));
    let dep_id = dep_alloc.id.to_string();

    let alloc = world
        .named_allocations
        .get(&name)
        .unwrap_or_else(|| panic!("No named allocation '{name}'"));

    let expected_condition = parse_dep_condition(&condition);

    let found = alloc.depends_on.iter().any(|d| {
        d.ref_id == dep_id && std::mem::discriminant(&d.condition) == std::mem::discriminant(&expected_condition)
    });

    assert!(
        found,
        "Expected '{name}' to depend on '{dep_name}' (id={dep_id}) with condition {condition:?}, \
         but depends_on = {:?}",
        alloc.depends_on
    );
}

#[then("all DAG allocations should share the same dag_id")]
fn shared_dag_id(world: &mut LatticeWorld) {
    let dag_id = world.dag_id.as_ref().expect("No DAG submitted");
    let dag_allocs: Vec<&Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.dag_id.is_some())
        .collect();

    assert!(
        !dag_allocs.is_empty(),
        "No allocations with dag_id found"
    );

    for alloc in &dag_allocs {
        assert_eq!(
            alloc.dag_id.as_deref(),
            Some(dag_id.as_str()),
            "All DAG allocations should share dag_id '{dag_id}'"
        );
    }
}

#[then("the DAG should fail validation with a cycle error")]
fn dag_cycle_error(world: &mut LatticeWorld) {
    // Rebuild the allocations with UUID-based ref_ids and validate
    let dag_allocs: Vec<Allocation> = world
        .named_allocations
        .values()
        .cloned()
        .collect();

    let result = validate_dag(&dag_allocs, DEFAULT_MAX_DAG_SIZE);
    assert_eq!(
        result,
        Err(DagError::CycleDetected),
        "Expected DAG validation to fail with CycleDetected, got {:?}",
        result
    );
}

#[then("the DAG should fail validation with a size limit error")]
fn dag_size_limit_error(world: &mut LatticeWorld) {
    let dag_allocs: Vec<Allocation> = world
        .allocations
        .iter()
        .filter(|a| a.dag_id == world.dag_id)
        .cloned()
        .collect();

    let result = validate_dag(&dag_allocs, DEFAULT_MAX_DAG_SIZE);
    assert!(
        matches!(result, Err(DagError::TooLarge { .. })),
        "Expected DAG validation to fail with TooLarge, got {:?}",
        result
    );
}

#[then(regex = r#"^"(\w[\w-]*)" should be "(\w+)"$"#)]
fn named_alloc_state(world: &mut LatticeWorld, name: String, expected_str: String) {
    let expected = parse_allocation_state(&expected_str);

    // Check named_allocations first, fall back to world.allocations
    let state = if let Some(alloc) = world.named_allocations.get(&name) {
        alloc.state.clone()
    } else {
        // Try finding by tag
        world
            .allocations
            .iter()
            .find(|a| a.tags.get("dag_stage_name").map(|s| s.as_str()) == Some(&name))
            .map(|a| a.state.clone())
            .unwrap_or_else(|| panic!("No allocation named '{name}' found"))
    };

    assert_eq!(
        state, expected,
        "Expected '{name}' to be in state {expected_str}, got {state:?}"
    );
}

#[then(regex = r#"^"(\w[\w-]*)" should become eligible for scheduling$"#)]
fn should_become_eligible(world: &mut LatticeWorld, name: String) {
    let dag_allocs: Vec<Allocation> = world.named_allocations.values().cloned().collect();

    // Build terminal states from current world state
    let terminal_states: HashMap<AllocId, AllocationState> = dag_allocs
        .iter()
        .filter(|a| a.state.is_terminal())
        .map(|a| (a.id, a.state.clone()))
        .collect();

    let eligible = resolve_dependencies(&dag_allocs, &terminal_states);

    let alloc = world
        .named_allocations
        .get(&name)
        .unwrap_or_else(|| panic!("No named allocation '{name}'"));

    assert!(
        eligible.contains(&alloc.id),
        "Expected '{name}' to become eligible for scheduling, but it did not. \
         Terminal states: {:?}, eligible: {:?}",
        terminal_states,
        eligible
    );
}

#[then(regex = r#"^"(\w[\w-]*)" should remain "(\w+)" with unsatisfied dependencies$"#)]
fn should_remain_with_unsatisfied_deps(world: &mut LatticeWorld, name: String, expected_str: String) {
    let expected = parse_allocation_state(&expected_str);
    let alloc = world
        .named_allocations
        .get(&name)
        .unwrap_or_else(|| panic!("No named allocation '{name}'"));

    assert_eq!(
        alloc.state, expected,
        "Expected '{name}' to remain in state {expected_str}, got {:?}",
        alloc.state
    );

    // Verify dependencies are NOT satisfied
    let dag_allocs: Vec<Allocation> = world.named_allocations.values().cloned().collect();
    let terminal_states: HashMap<AllocId, AllocationState> = dag_allocs
        .iter()
        .filter(|a| a.state.is_terminal())
        .map(|a| (a.id, a.state.clone()))
        .collect();

    let eligible = resolve_dependencies(&dag_allocs, &terminal_states);
    assert!(
        !eligible.contains(&alloc.id),
        "Expected '{name}' to have unsatisfied dependencies, but it is eligible"
    );
}

#[then(regex = r#"^"([\w-]+)", "([\w-]+)", "([\w-]+)" should all depend on "(\w[\w-]*)"$"#)]
fn all_depend_on(
    world: &mut LatticeWorld,
    name_a: String,
    name_b: String,
    name_c: String,
    parent_name: String,
) {
    let parent = world
        .named_allocations
        .get(&parent_name)
        .unwrap_or_else(|| panic!("No named allocation '{parent_name}'"));
    let parent_id = parent.id.to_string();

    for child_name in [&name_a, &name_b, &name_c] {
        let child = world
            .named_allocations
            .get(child_name.as_str())
            .unwrap_or_else(|| panic!("No named allocation '{child_name}'"));

        let depends = child.depends_on.iter().any(|d| d.ref_id == parent_id);
        assert!(
            depends,
            "Expected '{child_name}' to depend on '{parent_name}' (id={parent_id}), \
             but depends_on = {:?}",
            child.depends_on
        );
    }
}

#[then(regex = r#"^"([\w-]+)", "([\w-]+)", "([\w-]+)" can run in parallel$"#)]
fn can_run_in_parallel(
    world: &mut LatticeWorld,
    name_a: String,
    name_b: String,
    name_c: String,
) {
    // Verify none of the three depend on each other
    let names = [&name_a, &name_b, &name_c];
    let ids: Vec<String> = names
        .iter()
        .map(|n| {
            world
                .named_allocations
                .get(n.as_str())
                .unwrap_or_else(|| panic!("No named allocation '{n}'"))
                .id
                .to_string()
        })
        .collect();

    for (i, name) in names.iter().enumerate() {
        let alloc = world.named_allocations.get(name.as_str()).unwrap();
        for (j, other_id) in ids.iter().enumerate() {
            if i == j {
                continue;
            }
            let depends_on_sibling = alloc.depends_on.iter().any(|d| d.ref_id == *other_id);
            assert!(
                !depends_on_sibling,
                "'{name}' should not depend on a sibling for parallel execution, \
                 but it depends on id={other_id}"
            );
        }
    }
}

#[then(regex = r#"^"(\w[\w-]*)" should depend on both "(\w[\w-]*)" and "(\w[\w-]*)"$"#)]
fn depends_on_both(
    world: &mut LatticeWorld,
    name: String,
    dep_a: String,
    dep_b: String,
) {
    let alloc = world
        .named_allocations
        .get(&name)
        .unwrap_or_else(|| panic!("No named allocation '{name}'"));

    for dep_name in [&dep_a, &dep_b] {
        let dep_alloc = world
            .named_allocations
            .get(dep_name.as_str())
            .unwrap_or_else(|| panic!("No named allocation '{dep_name}'"));
        let dep_id = dep_alloc.id.to_string();

        let found = alloc.depends_on.iter().any(|d| d.ref_id == dep_id);
        assert!(
            found,
            "Expected '{name}' to depend on '{dep_name}' (id={dep_id}), \
             but depends_on = {:?}",
            alloc.depends_on
        );
    }
}

#[then(regex = r#"^"(\w[\w-]*)" only becomes eligible when both predecessors complete$"#)]
fn eligible_when_both_complete(world: &mut LatticeWorld, name: String) {
    let alloc = world
        .named_allocations
        .get(&name)
        .unwrap_or_else(|| panic!("No named allocation '{name}'"));

    // It should have at least 2 dependencies
    assert!(
        alloc.depends_on.len() >= 2,
        "Expected '{name}' to have at least 2 dependencies, got {}",
        alloc.depends_on.len()
    );

    let dag_allocs: Vec<Allocation> = world.named_allocations.values().cloned().collect();

    // When no predecessors are terminal, should NOT be eligible
    let empty_terminal: HashMap<AllocId, AllocationState> = HashMap::new();
    let eligible = resolve_dependencies(&dag_allocs, &empty_terminal);
    assert!(
        !eligible.contains(&alloc.id),
        "'{name}' should not be eligible when no predecessors have completed"
    );

    // When only the first predecessor is completed, still not eligible
    let first_dep_ref = &alloc.depends_on[0].ref_id;
    let first_dep_id: AllocId = dag_allocs
        .iter()
        .find(|a| a.id.to_string() == *first_dep_ref)
        .map(|a| a.id)
        .expect("First dependency not found");

    let mut partial_terminal: HashMap<AllocId, AllocationState> = HashMap::new();
    partial_terminal.insert(first_dep_id, AllocationState::Completed);

    let eligible = resolve_dependencies(&dag_allocs, &partial_terminal);
    assert!(
        !eligible.contains(&alloc.id),
        "'{name}' should not be eligible when only one predecessor has completed"
    );

    // When all predecessors are completed, should be eligible
    let mut full_terminal: HashMap<AllocId, AllocationState> = HashMap::new();
    for dep in &alloc.depends_on {
        let dep_id = dag_allocs
            .iter()
            .find(|a| a.id.to_string() == dep.ref_id)
            .map(|a| a.id)
            .expect("Dependency not found");
        full_terminal.insert(dep_id, AllocationState::Completed);
    }

    let eligible = resolve_dependencies(&dag_allocs, &full_terminal);
    assert!(
        eligible.contains(&alloc.id),
        "'{name}' should be eligible when all predecessors have completed"
    );
}
