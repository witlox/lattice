use cucumber::{given, then, when};

use super::helpers::parse_duration_str;
use crate::LatticeWorld;
use lattice_scheduler::walltime::{ExpiryPhase, WalltimeEnforcer};

use chrono::{Duration, Utc};
use uuid::Uuid;

// ─── Given Steps ───────────────────────────────────────────

#[given("a walltime enforcer with default grace period")]
fn given_walltime_enforcer_default(world: &mut LatticeWorld) {
    world.walltime_enforcer = Some(WalltimeEnforcer::new());
    world.walltime_start = Some(Utc::now());
}

#[given(regex = r#"^a walltime enforcer with grace period (\d+) seconds$"#)]
fn given_walltime_enforcer_custom(world: &mut LatticeWorld, grace_secs: i64) {
    world.walltime_enforcer = Some(WalltimeEnforcer::with_grace_period(Duration::seconds(
        grace_secs,
    )));
    world.walltime_start = Some(Utc::now());
}

#[given(regex = r#"^a running allocation registered with walltime "(\w+)"$"#)]
fn given_alloc_with_walltime(world: &mut LatticeWorld, walltime_str: String) {
    let enforcer = world
        .walltime_enforcer
        .as_mut()
        .expect("walltime enforcer not initialized");
    let start = world.walltime_start.expect("walltime_start not set");
    let alloc_id = Uuid::new_v4();
    let walltime = parse_duration_str(&walltime_str);

    // Zero walltime means unbounded: do not register with the enforcer.
    if walltime > Duration::zero() {
        enforcer.register(alloc_id, walltime, start);
    }

    world.walltime_alloc_id = Some(alloc_id);
    world.walltime_alloc_ids.push(alloc_id);
}

#[given(regex = r#"^a running allocation "(\S+)" registered with walltime "(\w+)"$"#)]
fn given_named_alloc_with_walltime(world: &mut LatticeWorld, name: String, walltime_str: String) {
    let enforcer = world
        .walltime_enforcer
        .as_mut()
        .expect("walltime enforcer not initialized");
    let start = world.walltime_start.expect("walltime_start not set");
    let alloc_id = Uuid::new_v4();
    let walltime = parse_duration_str(&walltime_str);

    if walltime > Duration::zero() {
        enforcer.register(alloc_id, walltime, start);
    }

    world.named_alloc_ids.insert(name, alloc_id);
    world.walltime_alloc_ids.push(alloc_id);
    world.walltime_alloc_id = Some(alloc_id);
}

#[given("a checkpoint is in progress")]
fn given_checkpoint_in_progress(world: &mut LatticeWorld) {
    // Flag that a checkpoint is active; the grace period should allow it to complete.
    world.should_checkpoint = Some(true);
}

// ─── When Steps ────────────────────────────────────────────

#[when(regex = r#"^(\d+) hours? has elapsed$"#)]
fn when_hours_elapsed(world: &mut LatticeWorld, hours: i64) {
    let enforcer = world
        .walltime_enforcer
        .as_mut()
        .expect("walltime enforcer not initialized");
    let start = world.walltime_start.expect("walltime_start not set");
    let now = start + Duration::hours(hours);
    world.walltime_expired = enforcer.check_expired(now);
}

#[when(regex = r#"^(\d+) hours? and (\d+) seconds? have elapsed$"#)]
fn when_hours_and_secs_elapsed(world: &mut LatticeWorld, hours: i64, secs: i64) {
    let enforcer = world
        .walltime_enforcer
        .as_mut()
        .expect("walltime enforcer not initialized");
    let start = world.walltime_start.expect("walltime_start not set");
    let now = start + Duration::hours(hours) + Duration::seconds(secs);
    world.walltime_expired = enforcer.check_expired(now);
}

#[when(regex = r#"^(\d+) minutes? and (\d+) seconds? have elapsed$"#)]
fn when_mins_and_secs_elapsed(world: &mut LatticeWorld, mins: i64, secs: i64) {
    let enforcer = world
        .walltime_enforcer
        .as_mut()
        .expect("walltime enforcer not initialized");
    let start = world.walltime_start.expect("walltime_start not set");
    let now = start + Duration::minutes(mins) + Duration::seconds(secs);
    world.walltime_expired = enforcer.check_expired(now);
}

#[when(regex = r#"^(\d+) hours have elapsed$"#)]
fn when_multiple_hours_elapsed(world: &mut LatticeWorld, hours: i64) {
    let enforcer = world
        .walltime_enforcer
        .as_mut()
        .expect("walltime enforcer not initialized");
    let start = world.walltime_start.expect("walltime_start not set");
    let now = start + Duration::hours(hours);
    world.walltime_expired = enforcer.check_expired(now);
}

#[when(regex = r#"^(\d+) seconds? have elapsed$"#)]
fn when_secs_elapsed(world: &mut LatticeWorld, secs: i64) {
    let enforcer = world
        .walltime_enforcer
        .as_mut()
        .expect("walltime enforcer not initialized");
    let start = world.walltime_start.expect("walltime_start not set");
    let now = start + Duration::seconds(secs);
    world.walltime_expired = enforcer.check_expired(now);
}

#[when("the allocation is unregistered from walltime tracking")]
fn when_unregister_walltime(world: &mut LatticeWorld) {
    let enforcer = world
        .walltime_enforcer
        .as_mut()
        .expect("walltime enforcer not initialized");
    let alloc_id = world
        .walltime_alloc_id
        .expect("no walltime alloc_id to unregister");
    enforcer.unregister(&alloc_id);
}

// ─── Then Steps ────────────────────────────────────────────

#[then(regex = r#"^the allocation should be in "(\w+)" phase$"#)]
fn then_alloc_in_phase(world: &mut LatticeWorld, expected_phase: String) {
    let alloc_id = world.walltime_alloc_id.expect("no walltime alloc_id set");
    let expiry = world
        .walltime_expired
        .iter()
        .find(|e| e.allocation_id == alloc_id)
        .unwrap_or_else(|| {
            panic!(
                "Expected allocation {alloc_id} to be expired in {expected_phase} phase, \
                 but it was not found in expired list ({} entries)",
                world.walltime_expired.len()
            )
        });
    let actual_phase = match expiry.phase {
        ExpiryPhase::Terminate => "Terminate",
        ExpiryPhase::Kill => "Kill",
    };
    assert_eq!(
        actual_phase, expected_phase,
        "Expected {expected_phase} phase, got {actual_phase}"
    );
}

#[then("no allocations should be expired")]
fn then_no_expired(world: &mut LatticeWorld) {
    assert!(
        world.walltime_expired.is_empty(),
        "Expected no expired allocations, but found {}: {:?}",
        world.walltime_expired.len(),
        world.walltime_expired
    );
}

#[then(regex = r#"^allocation "(\S+)" should be in "(\w+)" phase$"#)]
fn then_named_alloc_in_phase(world: &mut LatticeWorld, name: String, expected_phase: String) {
    let alloc_id = world
        .named_alloc_ids
        .get(&name)
        .copied()
        .unwrap_or_else(|| panic!("No allocation named '{name}' found"));
    let expiry = world
        .walltime_expired
        .iter()
        .find(|e| e.allocation_id == alloc_id)
        .unwrap_or_else(|| {
            panic!(
                "Expected allocation '{name}' ({alloc_id}) to be in {expected_phase} phase, \
                 but it was not found in expired list"
            )
        });
    let actual_phase = match expiry.phase {
        ExpiryPhase::Terminate => "Terminate",
        ExpiryPhase::Kill => "Kill",
    };
    assert_eq!(
        actual_phase, expected_phase,
        "Expected allocation '{name}' in {expected_phase} phase, got {actual_phase}"
    );
}

#[then(regex = r#"^allocation "(\S+)" should not be expired$"#)]
fn then_named_alloc_not_expired(world: &mut LatticeWorld, name: String) {
    let alloc_id = world
        .named_alloc_ids
        .get(&name)
        .copied()
        .unwrap_or_else(|| panic!("No allocation named '{name}' found"));
    let found = world
        .walltime_expired
        .iter()
        .any(|e| e.allocation_id == alloc_id);
    assert!(
        !found,
        "Expected allocation '{name}' ({alloc_id}) to NOT be expired, but it was"
    );
}

#[then("the checkpoint has the grace period to complete")]
fn then_checkpoint_has_grace_period(world: &mut LatticeWorld) {
    // The allocation is in Terminate phase (SIGTERM sent), which means the
    // application has the grace period to finish its checkpoint before Kill.
    let alloc_id = world.walltime_alloc_id.expect("no walltime alloc_id set");
    let expiry = world
        .walltime_expired
        .iter()
        .find(|e| e.allocation_id == alloc_id)
        .expect("Expected allocation to be expired");
    assert_eq!(
        expiry.phase,
        ExpiryPhase::Terminate,
        "During grace period, allocation should be in Terminate (not Kill) phase"
    );
    // Verify a checkpoint was in progress.
    assert_eq!(
        world.should_checkpoint,
        Some(true),
        "A checkpoint should be in progress during grace period"
    );
}
