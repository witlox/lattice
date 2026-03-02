//! DAG validation and dependency resolution.
//!
//! Implements the DAG lifecycle from docs/architecture/dag-scheduling.md.

use std::collections::{HashMap, HashSet, VecDeque};

use lattice_common::types::*;

/// Maximum number of allocations in a single DAG (configurable).
pub const DEFAULT_MAX_DAG_SIZE: usize = 1000;

/// A DAG of allocations with dependency edges.
#[derive(Debug, Clone)]
pub struct Dag {
    pub id: String,
    /// Allocations indexed by their ID
    pub allocations: HashMap<AllocId, Allocation>,
    /// Adjacency list: allocation → its dependencies
    pub edges: HashMap<AllocId, Vec<DagEdge>>,
}

/// A directed edge in the DAG.
#[derive(Debug, Clone)]
pub struct DagEdge {
    pub from: AllocId,
    pub to: AllocId,
    pub condition: DependencyCondition,
}

/// Errors during DAG validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DagError {
    CycleDetected,
    UnknownDependency { ref_id: String },
    TooLarge { count: usize, limit: usize },
    Empty,
}

impl std::fmt::Display for DagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DagError::CycleDetected => write!(f, "DAG contains a cycle"),
            DagError::UnknownDependency { ref_id } => {
                write!(f, "unknown dependency reference: {ref_id}")
            }
            DagError::TooLarge { count, limit } => {
                write!(
                    f,
                    "DAG exceeds maximum size ({count} allocations, limit: {limit})"
                )
            }
            DagError::Empty => write!(f, "DAG has no allocations"),
        }
    }
}

impl std::error::Error for DagError {}

/// Validate a DAG: check for cycles, unknown dependencies, and size limits.
pub fn validate_dag(allocations: &[Allocation], max_size: usize) -> Result<Vec<AllocId>, DagError> {
    if allocations.is_empty() {
        return Err(DagError::Empty);
    }

    if allocations.len() > max_size {
        return Err(DagError::TooLarge {
            count: allocations.len(),
            limit: max_size,
        });
    }

    // Build ID lookup (use string id within the dag context)
    let id_set: HashSet<String> = allocations.iter().map(|a| a.id.to_string()).collect();

    // Check all dependencies reference valid IDs
    for alloc in allocations {
        for dep in &alloc.depends_on {
            if !id_set.contains(&dep.ref_id) {
                return Err(DagError::UnknownDependency {
                    ref_id: dep.ref_id.clone(),
                });
            }
        }
    }

    // Topological sort (Kahn's algorithm) to detect cycles
    topological_sort(allocations)
}

/// Perform a topological sort on the allocations.
///
/// Returns the sorted order, or an error if a cycle is detected.
pub fn topological_sort(allocations: &[Allocation]) -> Result<Vec<AllocId>, DagError> {
    let mut in_degree: HashMap<AllocId, usize> = HashMap::new();
    let mut adjacency: HashMap<AllocId, Vec<AllocId>> = HashMap::new();
    let id_lookup: HashMap<String, AllocId> = allocations
        .iter()
        .map(|a| (a.id.to_string(), a.id))
        .collect();

    // Initialize
    for alloc in allocations {
        in_degree.entry(alloc.id).or_insert(0);
        adjacency.entry(alloc.id).or_default();
    }

    // Build edges
    for alloc in allocations {
        for dep in &alloc.depends_on {
            if let Some(&dep_id) = id_lookup.get(&dep.ref_id) {
                adjacency.entry(dep_id).or_default().push(alloc.id);
                *in_degree.entry(alloc.id).or_insert(0) += 1;
            }
        }
    }

    // Kahn's algorithm
    let mut queue: VecDeque<AllocId> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();

    let mut sorted = Vec::new();

    while let Some(node) = queue.pop_front() {
        sorted.push(node);
        if let Some(neighbors) = adjacency.get(&node) {
            for &neighbor in neighbors {
                if let Some(deg) = in_degree.get_mut(&neighbor) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(neighbor);
                    }
                }
            }
        }
    }

    if sorted.len() == allocations.len() {
        Ok(sorted)
    } else {
        Err(DagError::CycleDetected)
    }
}

/// Get root allocations (no incoming dependencies) from a DAG.
pub fn root_allocations(allocations: &[Allocation]) -> Vec<AllocId> {
    allocations
        .iter()
        .filter(|a| a.depends_on.is_empty())
        .map(|a| a.id)
        .collect()
}

/// Evaluate which downstream allocations are unblocked given terminal states.
///
/// Returns IDs of allocations whose all dependencies are now satisfied.
pub fn resolve_dependencies(
    allocations: &[Allocation],
    terminal_states: &HashMap<AllocId, AllocationState>,
) -> Vec<AllocId> {
    allocations
        .iter()
        .filter(|alloc| {
            // Only consider pending (not yet scheduled) allocations
            alloc.state == AllocationState::Pending
                && !alloc.depends_on.is_empty()
                && alloc.depends_on.iter().all(|dep| {
                    let dep_id = &dep.ref_id;
                    // Find the dependency's terminal state
                    allocations
                        .iter()
                        .find(|a| a.id.to_string() == *dep_id)
                        .and_then(|dep_alloc| terminal_states.get(&dep_alloc.id))
                        .is_some_and(|state| is_condition_satisfied(&dep.condition, state))
                })
        })
        .map(|a| a.id)
        .collect()
}

/// Check if a dependency condition is satisfied by the given terminal state.
pub fn is_condition_satisfied(condition: &DependencyCondition, state: &AllocationState) -> bool {
    match condition {
        DependencyCondition::Success => *state == AllocationState::Completed,
        DependencyCondition::Failure => *state == AllocationState::Failed,
        DependencyCondition::Any => state.is_terminal(),
        DependencyCondition::Corresponding => *state == AllocationState::Completed,
        DependencyCondition::Mutex => state.is_terminal(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_test_harness::fixtures::AllocationBuilder;

    #[test]
    fn validate_empty_dag_fails() {
        let result = validate_dag(&[], DEFAULT_MAX_DAG_SIZE);
        assert_eq!(result, Err(DagError::Empty));
    }

    #[test]
    fn validate_too_large_dag_fails() {
        let allocs: Vec<Allocation> = (0..5).map(|_| AllocationBuilder::new().build()).collect();
        let result = validate_dag(&allocs, 3);
        assert_eq!(result, Err(DagError::TooLarge { count: 5, limit: 3 }));
    }

    #[test]
    fn validate_simple_dag() {
        let a = AllocationBuilder::new().build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();

        let result = validate_dag(&[a, b], DEFAULT_MAX_DAG_SIZE);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_unknown_dependency_fails() {
        let a = AllocationBuilder::new()
            .depends_on("nonexistent", DependencyCondition::Success)
            .build();

        let result = validate_dag(&[a], DEFAULT_MAX_DAG_SIZE);
        assert!(matches!(result, Err(DagError::UnknownDependency { .. })));
    }

    #[test]
    fn validate_cycle_detected() {
        let mut a = AllocationBuilder::new().build();
        let mut b = AllocationBuilder::new().build();
        a.depends_on = vec![Dependency {
            ref_id: b.id.to_string(),
            condition: DependencyCondition::Success,
        }];
        b.depends_on = vec![Dependency {
            ref_id: a.id.to_string(),
            condition: DependencyCondition::Success,
        }];

        let result = validate_dag(&[a, b], DEFAULT_MAX_DAG_SIZE);
        assert_eq!(result, Err(DagError::CycleDetected));
    }

    #[test]
    fn topological_sort_linear_chain() {
        let a = AllocationBuilder::new().build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();
        let c = AllocationBuilder::new()
            .depends_on(&b.id.to_string(), DependencyCondition::Success)
            .build();

        let order = topological_sort(&[a.clone(), b.clone(), c.clone()]).unwrap();
        assert_eq!(order.len(), 3);
        // a must come before b, b before c
        let pos_a = order.iter().position(|&id| id == a.id).unwrap();
        let pos_b = order.iter().position(|&id| id == b.id).unwrap();
        let pos_c = order.iter().position(|&id| id == c.id).unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn root_allocations_found() {
        let a = AllocationBuilder::new().build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();
        let c = AllocationBuilder::new().build();

        let roots = root_allocations(&[a.clone(), b, c.clone()]);
        assert_eq!(roots.len(), 2);
        assert!(roots.contains(&a.id));
        assert!(roots.contains(&c.id));
    }

    #[test]
    fn resolve_success_dependency() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Completed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();

        let mut terminal = HashMap::new();
        terminal.insert(a.id, AllocationState::Completed);

        let unblocked = resolve_dependencies(&[a, b.clone()], &terminal);
        assert_eq!(unblocked.len(), 1);
        assert_eq!(unblocked[0], b.id);
    }

    #[test]
    fn success_dependency_not_satisfied_on_failure() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Failed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Success)
            .build();

        let mut terminal = HashMap::new();
        terminal.insert(a.id, AllocationState::Failed);

        let unblocked = resolve_dependencies(&[a, b], &terminal);
        assert!(unblocked.is_empty());
    }

    #[test]
    fn failure_dependency_satisfied_on_failure() {
        let a = AllocationBuilder::new()
            .state(AllocationState::Failed)
            .build();
        let b = AllocationBuilder::new()
            .depends_on(&a.id.to_string(), DependencyCondition::Failure)
            .build();

        let mut terminal = HashMap::new();
        terminal.insert(a.id, AllocationState::Failed);

        let unblocked = resolve_dependencies(&[a, b.clone()], &terminal);
        assert_eq!(unblocked.len(), 1);
        assert_eq!(unblocked[0], b.id);
    }

    #[test]
    fn any_dependency_satisfied_on_any_terminal() {
        for state in [
            AllocationState::Completed,
            AllocationState::Failed,
            AllocationState::Cancelled,
        ] {
            let a = AllocationBuilder::new().state(state.clone()).build();
            let b = AllocationBuilder::new()
                .depends_on(&a.id.to_string(), DependencyCondition::Any)
                .build();

            let mut terminal = HashMap::new();
            terminal.insert(a.id, state);

            let unblocked = resolve_dependencies(&[a, b.clone()], &terminal);
            assert_eq!(unblocked.len(), 1);
        }
    }

    #[test]
    fn condition_satisfied_logic() {
        assert!(is_condition_satisfied(
            &DependencyCondition::Success,
            &AllocationState::Completed
        ));
        assert!(!is_condition_satisfied(
            &DependencyCondition::Success,
            &AllocationState::Failed
        ));

        assert!(is_condition_satisfied(
            &DependencyCondition::Failure,
            &AllocationState::Failed
        ));
        assert!(!is_condition_satisfied(
            &DependencyCondition::Failure,
            &AllocationState::Completed
        ));

        assert!(is_condition_satisfied(
            &DependencyCondition::Any,
            &AllocationState::Completed
        ));
        assert!(is_condition_satisfied(
            &DependencyCondition::Any,
            &AllocationState::Failed
        ));
        assert!(is_condition_satisfied(
            &DependencyCondition::Any,
            &AllocationState::Cancelled
        ));
        assert!(!is_condition_satisfied(
            &DependencyCondition::Any,
            &AllocationState::Running
        ));
    }
}
