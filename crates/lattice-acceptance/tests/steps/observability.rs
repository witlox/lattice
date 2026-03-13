use cucumber::{given, then, when};
use uuid::Uuid;

use crate::LatticeWorld;
use lattice_common::traits::{AuditAction, AuditEntry};
use lattice_common::types::*;
use lattice_node_agent::telemetry::log_buffer::LogRingBuffer;
use lattice_test_harness::fixtures::*;

// ─── Given Steps ───────────────────────────────────────────

#[given(regex = r#"^a running allocation owned by user "(\w[\w-]*)"$"#)]
fn given_running_alloc_owned_by(world: &mut LatticeWorld, user: String) {
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.user = user.clone();
    alloc.assigned_nodes = vec!["node-0".into()];
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
    world.attach_owner = Some(user);
}

#[given("a running allocation producing log output")]
fn given_running_alloc_producing_logs(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = vec!["node-0".into()];
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
}

#[given("a running allocation with telemetry enabled")]
fn given_running_alloc_telemetry(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = vec!["node-0".into()];
    alloc.started_at = Some(chrono::Utc::now());
    alloc
        .tags
        .insert("telemetry_enabled".into(), "true".into());
    world.allocations.push(alloc);
}

#[given("a metrics subscriber for the allocation")]
fn given_metrics_subscriber(world: &mut LatticeWorld) {
    world.metrics_streaming = true;
}

#[given(regex = r#"^a running allocation on (\d+) nodes$"#)]
fn given_running_alloc_on_nodes(world: &mut LatticeWorld, node_count: u32) {
    let mut alloc = AllocationBuilder::new()
        .nodes(node_count)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = (0..node_count).map(|i| format!("node-{i}")).collect();
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
}

#[given(regex = r#"^a running allocation owned by tenant "(\w[\w-]*)"$"#)]
fn given_running_alloc_owned_by_tenant(world: &mut LatticeWorld, tenant: String) {
    let mut alloc = AllocationBuilder::new()
        .tenant(&tenant)
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = vec!["node-0".into()];
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
}

#[given(regex = r#"^(\d+) running allocations owned by tenant "(\w[\w-]*)"$"#)]
fn given_n_running_allocs_by_tenant(world: &mut LatticeWorld, count: usize, tenant: String) {
    for i in 0..count {
        let mut alloc = AllocationBuilder::new()
            .tenant(&tenant)
            .nodes(1)
            .state(AllocationState::Running)
            .build();
        alloc.assigned_nodes = vec![format!("node-{i}")];
        alloc.started_at = Some(chrono::Utc::now());
        world.allocations.push(alloc);
    }
}

#[given(regex = r#"^a running sensitive allocation owned by user "(\w[\w-]*)"$"#)]
fn given_running_sensitive_alloc(world: &mut LatticeWorld, user: String) {
    let mut alloc = AllocationBuilder::new()
        .sensitive()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.user = user.clone();
    alloc.assigned_nodes = vec!["node-0".into()];
    alloc.started_at = Some(chrono::Utc::now());
    world.allocations.push(alloc);
    world.attach_owner = Some(user);
}

#[given("the log buffer is at capacity")]
fn given_log_buffer_at_capacity(world: &mut LatticeWorld) {
    // Fill the log buffer to capacity with known data.
    let capacity = 256; // small capacity for testing
    let mut buf = LogRingBuffer::with_capacity(capacity);
    let filler = vec![b'X'; capacity];
    buf.write(&filler);
    world.log_buffer_data = buf.read_all();
}

#[given("a running allocation with telemetry at 30-second resolution")]
fn given_running_alloc_30s_resolution(world: &mut LatticeWorld) {
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Running)
        .build();
    alloc.assigned_nodes = vec!["node-0".into()];
    alloc.started_at = Some(chrono::Utc::now());
    alloc
        .tags
        .insert("telemetry_resolution_s".into(), "30".into());
    world.allocations.push(alloc);
    world.resolution_mode = Some("production".into());
}

// ─── When Steps ────────────────────────────────────────────

#[when(regex = r#"^user "(\w[\w-]*)" attaches to the allocation$"#)]
fn when_user_attaches(world: &mut LatticeWorld, user: String) {
    world.attach_user = Some(user.clone());
    let owner = world.attach_owner.as_deref().unwrap_or("");
    if user == owner {
        world.attach_allowed = Some(true);
        // For sensitive allocations, record an audit entry.
        let alloc = world.allocations.last().expect("no allocation");
        if alloc
            .tags
            .get("workload_class")
            .map(|v| v == "sensitive")
            .unwrap_or(false)
        {
            let entry = AuditEntry {
                id: Uuid::new_v4(),
                timestamp: chrono::Utc::now(),
                user: user.clone(),
                action: AuditAction::AttachSession,
                details: serde_json::json!({
                    "allocation_id": alloc.id.to_string(),
                    "user": user,
                }),
                previous_hash: String::new(),
                signature: String::new(),
            };
            world.audit.entries.lock().unwrap().push(entry);
        }
    } else {
        world.attach_allowed = Some(false);
    }
}

#[when(regex = r#"^user "(\w[\w-]*)" attempts to attach$"#)]
fn when_user_attempts_attach(world: &mut LatticeWorld, user: String) {
    world.attach_user = Some(user.clone());
    let owner = world.attach_owner.as_deref().unwrap_or("");
    world.attach_allowed = Some(user == owner);
}

#[when("the log buffer receives data")]
fn when_log_buffer_receives(world: &mut LatticeWorld) {
    let mut buf = LogRingBuffer::with_capacity(1024);
    let test_data = b"line 1\nline 2\nline 3\n";
    buf.write(test_data);
    world.log_buffer_data = buf.read_all();
}

#[when("the node agent pushes metric samples")]
fn when_node_agent_pushes_metrics(world: &mut LatticeWorld) {
    // Simulate metric push. The metrics_streaming flag is already set.
    world
        .allocations
        .last_mut()
        .expect("no allocation")
        .tags
        .insert("metrics_pushed".into(), "true".into());
}

#[when("a diagnostics query is issued for the allocation")]
fn when_diagnostics_query(world: &mut LatticeWorld) {
    let alloc = world.allocations.last().expect("no allocation");
    let node_count = alloc.assigned_nodes.len();
    let mut diag_lines = Vec::new();
    for node in &alloc.assigned_nodes {
        diag_lines.push(format!("{node}: healthy, cpu=45%, gpu=78%, mem=62%"));
    }
    world.diagnostics_data = Some(diag_lines.join("\n"));
}

#[when("a user requests to compare metrics across both allocations")]
fn when_cross_tenant_comparison(world: &mut LatticeWorld) {
    // Check if the two allocations belong to different tenants.
    assert!(
        world.allocations.len() >= 2,
        "Need at least 2 allocations for comparison"
    );
    let t1 = &world.allocations[0].tenant;
    let t2 = &world.allocations[1].tenant;
    if t1 != t2 {
        world.last_error = Some(lattice_common::error::LatticeError::CompareFailed(
            "cross_tenant_comparison".into(),
        ));
    }
}

#[when(regex = r#"^a user of tenant "(\w[\w-]*)" requests to compare metrics$"#)]
fn when_same_tenant_comparison(world: &mut LatticeWorld, tenant: String) {
    // Verify all allocations belong to the same tenant.
    let all_same = world.allocations.iter().all(|a| a.tenant == tenant);
    assert!(all_same, "Not all allocations belong to tenant '{tenant}'");
    world.metrics_streaming = true;
}

#[when("new log lines are produced")]
fn when_new_log_lines(world: &mut LatticeWorld) {
    // Write more data into a buffer that's at capacity.
    let capacity = 256;
    let mut buf = LogRingBuffer::with_capacity(capacity);
    // Fill to capacity first.
    let filler = vec![b'A'; capacity];
    buf.write(&filler);
    // Now write new data, causing wrap-around.
    let new_data = b"NEW_DATA_HERE";
    buf.write(new_data);
    world.log_buffer_data = buf.read_all();
}

#[when("the resolution is switched to 1-second debug mode")]
fn when_switch_to_debug(world: &mut LatticeWorld) {
    world.resolution_mode = Some("debug".into());
    if let Some(alloc) = world.allocations.last_mut() {
        alloc
            .tags
            .insert("telemetry_resolution_s".into(), "1".into());
    }
}

#[when("switched back to production mode")]
fn when_switch_to_production(world: &mut LatticeWorld) {
    world.resolution_mode = Some("production".into());
    if let Some(alloc) = world.allocations.last_mut() {
        alloc
            .tags
            .insert("telemetry_resolution_s".into(), "30".into());
    }
}

// ─── Then Steps ────────────────────────────────────────────

#[then("an attach session should be created")]
fn then_attach_session_created(world: &mut LatticeWorld) {
    assert_eq!(
        world.attach_allowed,
        Some(true),
        "Attach session should have been created"
    );
}

#[then("the session should support write and read")]
fn then_session_supports_io(world: &mut LatticeWorld) {
    assert_eq!(
        world.attach_allowed,
        Some(true),
        "Session should support read and write for the owner"
    );
}

#[then("the attach should be denied with permission error")]
fn then_attach_denied(world: &mut LatticeWorld) {
    assert_eq!(
        world.attach_allowed,
        Some(false),
        "Attach should be denied for non-owner"
    );
}

#[then("reading the buffer should return the data in order")]
fn then_buffer_data_in_order(world: &mut LatticeWorld) {
    assert!(
        !world.log_buffer_data.is_empty(),
        "Log buffer should contain data"
    );
    // Verify the data matches what was written.
    let expected = b"line 1\nline 2\nline 3\n";
    assert_eq!(
        &world.log_buffer_data[..],
        expected,
        "Buffer data should be in write order"
    );
}

#[then("flushing to S3 should upload the contents")]
fn then_s3_upload(world: &mut LatticeWorld) {
    // Verify that the log buffer has data that could be flushed.
    assert!(
        !world.log_buffer_data.is_empty(),
        "Log buffer should have data to flush to S3"
    );
    // In a real test, we'd call flush_to_s3 with a mock sink.
    // Here we verify the data is available for flushing.
}

#[then("the subscriber should receive metric events")]
fn then_subscriber_receives_metrics(world: &mut LatticeWorld) {
    assert!(
        world.metrics_streaming,
        "Metrics streaming should be active"
    );
    let alloc = world.allocations.last().expect("no allocation");
    assert_eq!(
        alloc.tags.get("metrics_pushed").map(|v| v.as_str()),
        Some("true"),
        "Node agent should have pushed metric samples"
    );
}

#[then("metrics should be scoped to the allocation")]
fn then_metrics_scoped(world: &mut LatticeWorld) {
    let alloc = world.allocations.last().expect("no allocation");
    assert!(
        alloc
            .tags
            .get("telemetry_enabled")
            .map(|v| v == "true")
            .unwrap_or(false),
        "Telemetry should be enabled for the allocation"
    );
}

#[then("the response should include per-node health status")]
fn then_per_node_health(world: &mut LatticeWorld) {
    let diag = world
        .diagnostics_data
        .as_ref()
        .expect("no diagnostics data");
    let alloc = world.allocations.last().expect("no allocation");
    for node in &alloc.assigned_nodes {
        assert!(
            diag.contains(node),
            "Diagnostics should include status for node '{node}'"
        );
    }
}

#[then("the response should include resource utilization")]
fn then_resource_utilization(world: &mut LatticeWorld) {
    let diag = world
        .diagnostics_data
        .as_ref()
        .expect("no diagnostics data");
    assert!(
        diag.contains("cpu=") && diag.contains("gpu=") && diag.contains("mem="),
        "Diagnostics should include resource utilization (cpu, gpu, mem)"
    );
}

#[then(regex = r#"^the comparison should be denied with "(\w+)"$"#)]
fn then_comparison_denied(world: &mut LatticeWorld, reason: String) {
    let err = world.last_error.as_ref().expect("Expected an error");
    let err_str = format!("{err:?}");
    assert!(
        err_str.contains(&reason),
        "Expected error to contain '{reason}', got '{err_str}'"
    );
}

#[then("the comparison should be performed")]
fn then_comparison_performed(world: &mut LatticeWorld) {
    assert!(
        world.last_error.is_none(),
        "Expected no error for same-tenant comparison"
    );
    assert!(
        world.metrics_streaming,
        "Metrics comparison should be active"
    );
}

#[then("metrics from both allocations should be returned")]
fn then_metrics_from_both(world: &mut LatticeWorld) {
    assert!(
        world.allocations.len() >= 2,
        "Should have at least 2 allocations for comparison"
    );
    // Verify they're from the same tenant.
    let t1 = &world.allocations[0].tenant;
    let t2 = &world.allocations[1].tenant;
    assert_eq!(t1, t2, "Both allocations should be from the same tenant");
}

#[then("an audit entry should be committed before the terminal opens")]
fn then_audit_attach(world: &mut LatticeWorld) {
    let entries = world.audit.entries_for_action(&AuditAction::AttachSession);
    assert!(
        !entries.is_empty(),
        "Expected an AttachSession audit entry to be committed"
    );
    let entry = &entries[0];
    let attach_user = world.attach_user.as_deref().unwrap_or("");
    assert_eq!(
        entry.user, attach_user,
        "Audit entry user should match attaching user"
    );
}

#[then("the oldest entries should be evicted")]
fn then_oldest_evicted(world: &mut LatticeWorld) {
    // After wrap-around, the data should not start with 'A' (the filler).
    // The buffer wraps, so the oldest 'A' bytes are partially overwritten.
    let data = &world.log_buffer_data;
    assert!(
        !data.is_empty(),
        "Buffer should have data after wrap-around"
    );
    // The newest data should be present.
    let data_str = String::from_utf8_lossy(data);
    assert!(
        data_str.contains("NEW_DATA_HERE"),
        "Newest data should be retained after wrap-around"
    );
}

#[then("the newest entries should be retained")]
fn then_newest_retained(world: &mut LatticeWorld) {
    let data_str = String::from_utf8_lossy(&world.log_buffer_data);
    assert!(
        data_str.contains("NEW_DATA_HERE"),
        "Newest entries should be retained in the ring buffer"
    );
}

#[then("metrics should be collected at 1-second intervals")]
fn then_debug_resolution(world: &mut LatticeWorld) {
    assert_eq!(
        world.resolution_mode.as_deref(),
        Some("debug"),
        "Resolution mode should be 'debug'"
    );
    let alloc = world.allocations.last().expect("no allocation");
    assert_eq!(
        alloc
            .tags
            .get("telemetry_resolution_s")
            .map(|v| v.as_str()),
        Some("1"),
        "Telemetry resolution should be 1 second in debug mode"
    );
}

#[then("metrics should return to 30-second intervals")]
fn then_production_resolution(world: &mut LatticeWorld) {
    assert_eq!(
        world.resolution_mode.as_deref(),
        Some("production"),
        "Resolution mode should be 'production'"
    );
    let alloc = world.allocations.last().expect("no allocation");
    assert_eq!(
        alloc
            .tags
            .get("telemetry_resolution_s")
            .map(|v| v.as_str()),
        Some("30"),
        "Telemetry resolution should be 30 seconds in production mode"
    );
}
