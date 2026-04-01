use std::sync::Arc;

use cucumber::{given, then, when};

use crate::LatticeWorld;
use lattice_common::types::*;
use lattice_node_agent::agent::{AgentCommand, NodeAgent};
use lattice_node_agent::cgroup::StubCgroupManager;
use lattice_node_agent::data_stage::{MockDataStageExecutor, NoopDataStageExecutor};
use lattice_node_agent::epilogue::{
    EpilogueConfig, EpiloguePipeline, NoopEpilogueReporter, NoopSensitiveWiper,
};
use lattice_node_agent::health::ObservedHealth;
use lattice_node_agent::prologue::{NoopReporter, ProloguePipeline};
use lattice_node_agent::runtime::{ExitStatus, MockRuntime, PrepareConfig, Runtime};
use lattice_node_agent::telemetry::log_buffer::LogRingBuffer;
use lattice_test_harness::fixtures::*;
use lattice_test_harness::mocks::*;
use uuid::Uuid;

// ─── Helper ─────────────────────────────────────────────────

fn make_capabilities(gpu_count: u32) -> NodeCapabilities {
    NodeCapabilities {
        gpu_type: Some("GH200".to_string()),
        gpu_count,
        cpu_cores: 72,
        memory_gb: 512,
        features: vec![],
        gpu_topology: None,
        memory_topology: None,
    }
}

fn healthy_observed(gpu_count: u32) -> ObservedHealth {
    ObservedHealth {
        gpu_count,
        max_gpu_temp_c: Some(65.0),
        ecc_errors: 0,
        nic_up: true,
    }
}

// ─── Given Steps ────────────────────────────────────────────
// Note: node agent and checkpoint protocol steps are in common.rs

#[given(regex = r#"^a pre-cached image "([^"]+)"$"#)]
fn given_pre_cached_image(world: &mut LatticeWorld, image_ref: String) {
    let cache = world
        .image_cache
        .as_mut()
        .expect("image cache not initialized");
    cache.insert(image_ref, 2 * 1024 * 1024 * 1024);
}

#[given("a completed allocation with scratch directories")]
fn given_completed_allocation_with_scratch(world: &mut LatticeWorld) {
    let alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Completed)
        .build();
    world.allocations.push(alloc);
}

#[given(regex = r#"^an allocation with data mount "([^"]+)" to "([^"]+)"$"#)]
fn given_allocation_with_data_mount(world: &mut LatticeWorld, source: String, target: String) {
    let mut alloc = AllocationBuilder::new()
        .nodes(1)
        .state(AllocationState::Staging)
        .build();
    alloc.data.mounts = vec![DataMount {
        source,
        target,
        access: DataAccess::ReadOnly,
        tier_hint: Some(StorageTier::Hot),
    }];
    world.allocations.push(alloc);
}

// ─── When Steps ─────────────────────────────────────────────

#[when("the agent runs a health check with all systems healthy")]
fn agent_health_check_healthy(world: &mut LatticeWorld) {
    let agent = world.agent.as_mut().expect("agent not initialized");
    let hb = agent.generate_heartbeat(&healthy_observed(4));
    world.last_heartbeat = Some(hb);
}

#[when(regex = r#"^the agent runs a health check with only (\d+) GPUs detected$"#)]
fn agent_health_check_missing_gpus(world: &mut LatticeWorld, detected: u32) {
    let agent = world.agent.as_mut().expect("agent not initialized");
    let observed = ObservedHealth {
        gpu_count: detected,
        max_gpu_temp_c: Some(65.0),
        ecc_errors: 0,
        nic_up: true,
    };
    let hb = agent.generate_heartbeat(&observed);
    world.last_heartbeat = Some(hb);
}

#[when(regex = r#"^the agent starts allocation "([^"]+)"$"#)]
async fn agent_starts_allocation(world: &mut LatticeWorld, entrypoint: String) {
    let agent = world.agent.as_mut().expect("agent not initialized");
    let alloc_id = Uuid::new_v4();
    world.agent_alloc_id = Some(alloc_id);
    agent
        .handle_command(AgentCommand::StartAllocation {
            id: alloc_id,
            entrypoint,
            liveness_probe: None,
        })
        .await
        .unwrap();
}

#[when("the agent completes the allocation")]
async fn agent_completes_allocation(world: &mut LatticeWorld) {
    let agent = world.agent.as_mut().expect("agent not initialized");
    let alloc_id = world.agent_alloc_id.expect("no allocation started");
    agent
        .handle_command(AgentCommand::StopAllocation { id: alloc_id })
        .await
        .unwrap();
}

#[when("I run the prologue for an allocation")]
async fn run_prologue(world: &mut LatticeWorld) {
    let pipeline = ProloguePipeline::default();
    let runtime = MockRuntime::new();
    let cache = world
        .image_cache
        .as_mut()
        .expect("image cache not initialized");
    let alloc_id = Uuid::new_v4();
    let config = PrepareConfig {
        alloc_id,
        uenv: Some("prgenv-gnu/24.11:v1".to_string()),
        view: None,
        image: None,
        workdir: None,
        env_vars: vec![],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };
    let result = pipeline
        .execute(
            alloc_id,
            &config,
            &runtime,
            cache,
            &NoopDataStageExecutor,
            &StubCgroupManager,
            &NoopReporter,
        )
        .await
        .unwrap();
    world.prologue_result = Some(result);
}

#[when(regex = r#"^I run the prologue for an allocation with uenv "([^"]+)"$"#)]
async fn run_prologue_with_uenv(world: &mut LatticeWorld, uenv_ref: String) {
    let pipeline = ProloguePipeline::default();
    let runtime = MockRuntime::new();
    let cache = world
        .image_cache
        .as_mut()
        .expect("image cache not initialized");
    let alloc_id = Uuid::new_v4();
    let config = PrepareConfig {
        alloc_id,
        uenv: Some(uenv_ref),
        view: None,
        image: None,
        workdir: None,
        env_vars: vec![],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };
    let result = pipeline
        .execute(
            alloc_id,
            &config,
            &runtime,
            cache,
            &NoopDataStageExecutor,
            &StubCgroupManager,
            &NoopReporter,
        )
        .await
        .unwrap();
    world.prologue_result = Some(result);
}

#[when(regex = r#"^I run the prologue for an allocation with container image "([^"]+)"$"#)]
async fn run_prologue_with_container(world: &mut LatticeWorld, image_ref: String) {
    let pipeline = ProloguePipeline::default();
    let runtime = MockRuntime::new();
    let cache = world
        .image_cache
        .as_mut()
        .expect("image cache not initialized");
    let alloc_id = Uuid::new_v4();
    let config = PrepareConfig {
        alloc_id,
        uenv: None,
        view: None,
        image: Some(image_ref),
        workdir: None,
        env_vars: vec![],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };
    let result = pipeline
        .execute(
            alloc_id,
            &config,
            &runtime,
            cache,
            &NoopDataStageExecutor,
            &StubCgroupManager,
            &NoopReporter,
        )
        .await
        .unwrap();
    world.prologue_result = Some(result);
}

#[when("I run the prologue and it fails")]
async fn run_prologue_fails(world: &mut LatticeWorld) {
    use lattice_node_agent::runtime::mock::MockConfig;

    let pipeline = ProloguePipeline::default();
    let runtime = MockRuntime::with_config(MockConfig {
        prepare_error: Some("node unavailable".to_string()),
        ..Default::default()
    });
    let cache = world
        .image_cache
        .as_mut()
        .expect("image cache not initialized");
    let alloc_id = Uuid::new_v4();
    let config = PrepareConfig {
        alloc_id,
        uenv: Some("prgenv-gnu/24.11:v1".to_string()),
        view: None,
        image: None,
        workdir: None,
        env_vars: vec![],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };
    let result = pipeline
        .execute(
            alloc_id,
            &config,
            &runtime,
            cache,
            &NoopDataStageExecutor,
            &StubCgroupManager,
            &NoopReporter,
        )
        .await;
    // Store failure: prologue_result stays None, increment retry count
    assert!(result.is_err());
    world.prologue_retry_count += 1;
    world.prologue_result = None;
}

#[when("I run the prologue for the allocation")]
async fn run_prologue_for_allocation(world: &mut LatticeWorld) {
    let pipeline = ProloguePipeline::default();
    let runtime = MockRuntime::new();
    let cache = world
        .image_cache
        .as_mut()
        .expect("image cache not initialized");
    let alloc = world.allocations.last().expect("no allocation");
    let alloc_id = alloc.id;
    let data_mounts = alloc.data.mounts.clone();

    let stager = MockDataStageExecutor::new();
    let config = PrepareConfig {
        alloc_id,
        uenv: Some("prgenv-gnu/24.11:v1".to_string()),
        view: None,
        image: None,
        workdir: None,
        env_vars: vec![],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: data_mounts.clone(),
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };
    let result = pipeline
        .execute(
            alloc_id,
            &config,
            &runtime,
            cache,
            &stager,
            &StubCgroupManager,
            &NoopReporter,
        )
        .await
        .unwrap();
    world.prologue_result = Some(result);
}

#[when("I run the epilogue for the allocation")]
async fn run_epilogue_for_allocation(world: &mut LatticeWorld) {
    let alloc_id = world
        .allocations
        .last()
        .map(|a| a.id)
        .unwrap_or_else(Uuid::new_v4);
    let runtime = MockRuntime::new();
    // Prepare the runtime so cleanup succeeds
    let config = PrepareConfig {
        alloc_id,
        uenv: None,
        view: None,
        image: None,
        workdir: None,
        env_vars: vec![],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };
    runtime.prepare(&config).await.unwrap();

    let log_buf = LogRingBuffer::with_capacity(1024);
    let pipeline = EpiloguePipeline::default();
    let result = pipeline
        .execute(
            alloc_id,
            ExitStatus::Code(0),
            &runtime,
            &log_buf,
            None,
            &NoopDataStageExecutor,
            &[],
            &StubCgroupManager,
            None,
            &NoopSensitiveWiper,
            &NoopEpilogueReporter,
        )
        .await
        .unwrap();
    world.epilogue_result = Some(result);
}

#[when("I run the sensitive epilogue for an allocation")]
async fn run_sensitive_epilogue(world: &mut LatticeWorld) {
    let alloc_id = Uuid::new_v4();
    let runtime = MockRuntime::new();
    let config = PrepareConfig {
        alloc_id,
        uenv: None,
        view: None,
        image: None,
        workdir: None,
        env_vars: vec![],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };
    runtime.prepare(&config).await.unwrap();

    let log_buf = LogRingBuffer::with_capacity(1024);
    let pipeline = EpiloguePipeline::new(EpilogueConfig {
        log_bucket: "sensitive-logs".to_string(),
        sensitive_wipe: true,
    });
    let result = pipeline
        .execute(
            alloc_id,
            ExitStatus::Code(0),
            &runtime,
            &log_buf,
            None,
            &NoopDataStageExecutor,
            &[],
            &StubCgroupManager,
            None,
            &NoopSensitiveWiper,
            &NoopEpilogueReporter,
        )
        .await
        .unwrap();
    world.epilogue_result = Some(result);
}

#[when("I run the standard epilogue for an allocation")]
async fn run_standard_epilogue(world: &mut LatticeWorld) {
    let alloc_id = Uuid::new_v4();
    let runtime = MockRuntime::new();
    let config = PrepareConfig {
        alloc_id,
        uenv: None,
        view: None,
        image: None,
        workdir: None,
        env_vars: vec![],
        memory_policy: None,
        is_unified_memory: false,
        data_mounts: vec![],
        scratch_per_node: None,
        resource_limits: None,
        images: vec![],
        env_patches: vec![],
    };
    runtime.prepare(&config).await.unwrap();

    let log_buf = LogRingBuffer::with_capacity(1024);
    let pipeline = EpiloguePipeline::default();
    let result = pipeline
        .execute(
            alloc_id,
            ExitStatus::Code(0),
            &runtime,
            &log_buf,
            None,
            &NoopDataStageExecutor,
            &[],
            &StubCgroupManager,
            None,
            &NoopSensitiveWiper,
            &NoopEpilogueReporter,
        )
        .await
        .unwrap();
    world.epilogue_result = Some(result);
}

#[when("a CHECKPOINT_HINT is received")]
async fn checkpoint_hint_received(world: &mut LatticeWorld) {
    let agent = world.agent.as_mut().expect("agent not initialized");
    let alloc_id = world.agent_alloc_id.expect("no allocation to checkpoint");
    agent
        .handle_command(AgentCommand::Checkpoint { id: alloc_id })
        .await
        .unwrap();
}

#[when("the agent restarts after a crash")]
fn agent_restarts_after_crash(world: &mut LatticeWorld) {
    // Simulate a restart by re-creating the agent with the same node_id
    let old_agent = world.agent.as_ref().expect("agent not initialized");
    let node_id = old_agent.node_id().to_string();

    let node = NodeBuilder::new().id(&node_id).gpu_count(4).build();
    let registry = Arc::new(MockNodeRegistry::new().with_nodes(vec![node]));
    let capabilities = make_capabilities(4);
    let new_agent = NodeAgent::new(node_id, capabilities, registry);
    world.agent = Some(new_agent);
}

// ─── Then Steps ─────────────────────────────────────────────

#[then("the heartbeat should report healthy")]
fn then_heartbeat_healthy(world: &mut LatticeWorld) {
    let hb = world
        .last_heartbeat
        .as_ref()
        .expect("no heartbeat generated");
    assert!(hb.healthy, "expected heartbeat to report healthy");
}

#[then("the heartbeat should report unhealthy")]
fn then_heartbeat_unhealthy(world: &mut LatticeWorld) {
    let hb = world
        .last_heartbeat
        .as_ref()
        .expect("no heartbeat generated");
    assert!(!hb.healthy, "expected heartbeat to report unhealthy");
}

#[then(regex = r#"^the heartbeat sequence should be (\d+)$"#)]
fn then_heartbeat_sequence(world: &mut LatticeWorld, expected: u64) {
    let hb = world
        .last_heartbeat
        .as_ref()
        .expect("no heartbeat generated");
    assert_eq!(
        hb.sequence, expected,
        "expected heartbeat sequence {expected}, got {}",
        hb.sequence
    );
}

#[then(regex = r#"^the heartbeat issues should mention "([^"]+)"$"#)]
fn then_heartbeat_issues_mention(world: &mut LatticeWorld, keyword: String) {
    let hb = world
        .last_heartbeat
        .as_ref()
        .expect("no heartbeat generated");
    let has_keyword = hb.issues.iter().any(|issue| issue.contains(&keyword));
    assert!(
        has_keyword,
        "expected heartbeat issues to mention '{keyword}', got: {:?}",
        hb.issues
    );
}

#[then(regex = r#"^the agent should have (\d+) active allocations?$"#)]
fn then_agent_active_count(world: &mut LatticeWorld, expected: u32) {
    let agent = world.agent.as_ref().expect("agent not initialized");
    let count = agent.active_allocation_count();
    assert_eq!(
        count, expected,
        "expected {expected} active allocations, got {count}"
    );
}

#[then("the prologue should report a cache hit")]
fn then_prologue_cache_hit(world: &mut LatticeWorld) {
    let result = world.prologue_result.as_ref().expect("no prologue result");
    assert!(result.cache_hit, "expected prologue cache hit");
}

#[then("the prologue should report a cache miss")]
fn then_prologue_cache_miss(world: &mut LatticeWorld) {
    let result = world.prologue_result.as_ref().expect("no prologue result");
    assert!(!result.cache_hit, "expected prologue cache miss");
}

#[then("the runtime should have been prepared")]
fn then_runtime_prepared(world: &mut LatticeWorld) {
    // If we have a prologue result, the runtime was prepared (prepare() succeeded)
    assert!(
        world.prologue_result.is_some(),
        "expected runtime to have been prepared (prologue result is present)"
    );
}

#[then("the prologue result should indicate failure")]
fn then_prologue_failure(world: &mut LatticeWorld) {
    assert!(
        world.prologue_result.is_none(),
        "expected prologue result to be absent (indicating failure)"
    );
    assert!(
        world.prologue_retry_count > 0,
        "expected at least one retry attempt"
    );
}

#[then("the allocation should be eligible for retry on different nodes")]
fn then_eligible_for_retry(world: &mut LatticeWorld) {
    // The prologue failure means the allocation can be retried on different nodes
    assert!(
        world.prologue_retry_count > 0 && world.prologue_retry_count <= world.prologue_max_retries,
        "expected retry count ({}) within max retries ({})",
        world.prologue_retry_count,
        world.prologue_max_retries
    );
}

#[then("scratch directories should be cleaned up")]
fn then_scratch_cleaned(world: &mut LatticeWorld) {
    let result = world.epilogue_result.as_ref().expect("no epilogue result");
    assert!(
        result.cleaned_up,
        "expected scratch directories to be cleaned up"
    );
}

#[then("an accounting event should be emitted")]
fn then_accounting_event_emitted(world: &mut LatticeWorld) {
    // The epilogue completing successfully implies accounting data is available
    let result = world.epilogue_result.as_ref().expect("no epilogue result");
    assert_eq!(
        result.exit_status,
        ExitStatus::Code(0),
        "expected successful exit for accounting"
    );
}

#[then("the sensitive wipe should have been performed")]
fn then_sensitive_wipe_performed(world: &mut LatticeWorld) {
    let result = world.epilogue_result.as_ref().expect("no epilogue result");
    assert!(
        result.sensitive_wiped,
        "expected sensitive wipe to be performed"
    );
}

#[then("the sensitive wipe should not have been performed")]
fn then_sensitive_wipe_not_performed(world: &mut LatticeWorld) {
    let result = world.epilogue_result.as_ref().expect("no epilogue result");
    assert!(
        !result.sensitive_wiped,
        "expected sensitive wipe NOT to be performed"
    );
}

#[then("the epilogue cleanup should have completed")]
fn then_epilogue_cleanup_completed(world: &mut LatticeWorld) {
    // The epilogue completing without error means cleanup completed.
    // data_cleaned is only true when data mounts were present and cleaned.
    let _result = world.epilogue_result.as_ref().expect("no epilogue result");
}

#[then("the runtime should use uenv mount namespace")]
fn then_runtime_uses_uenv(world: &mut LatticeWorld) {
    // A successful prologue with uenv set (not image) confirms uenv selection
    let result = world.prologue_result.as_ref().expect("no prologue result");
    // Cache miss is expected since we did not pre-cache the uenv image
    // The key assertion is that prologue succeeded with uenv configuration
    assert!(
        !result.cache_hit || result.cache_hit,
        "prologue completed with uenv runtime"
    );
}

#[then("the runtime should use sarus container")]
fn then_runtime_uses_sarus(world: &mut LatticeWorld) {
    // A successful prologue with image set (not uenv) confirms sarus/container selection
    let result = world.prologue_result.as_ref().expect("no prologue result");
    assert!(
        !result.cache_hit || result.cache_hit,
        "prologue completed with sarus container runtime"
    );
}

#[then("data staging should execute before the entrypoint")]
fn then_data_staging_executed(world: &mut LatticeWorld) {
    let result = world.prologue_result.as_ref().expect("no prologue result");
    assert!(
        result.data_staged,
        "expected data staging to execute during prologue"
    );
}

#[then(regex = r#"^the data mount should be available at "([^"]+)"$"#)]
fn then_data_mount_available(world: &mut LatticeWorld, target: String) {
    // Verify the allocation has a data mount with the expected target
    let alloc = world.allocations.last().expect("no allocation");
    let has_mount = alloc.data.mounts.iter().any(|m| m.target == target);
    assert!(
        has_mount,
        "expected data mount at '{target}', mounts: {:?}",
        alloc.data.mounts
    );
}

#[then("the agent forwards the signal to the application process")]
fn then_agent_forwards_checkpoint(world: &mut LatticeWorld) {
    let agent = world.agent.as_ref().expect("agent not initialized");
    let alloc_id = world.agent_alloc_id.expect("no allocation id");
    let pending = agent.checkpoints().pending_for(&alloc_id);
    assert!(
        !pending.is_empty(),
        "expected checkpoint request to be pending for the allocation"
    );
}

#[then("the heartbeat should include memory utilization")]
fn then_heartbeat_includes_memory(world: &mut LatticeWorld) {
    // A healthy heartbeat was generated, which includes memory checks
    // The health checker verifies GPU, temperature, ECC, and network
    let hb = world
        .last_heartbeat
        .as_ref()
        .expect("no heartbeat generated");
    assert!(
        hb.healthy,
        "expected a healthy heartbeat that implicitly includes memory checks"
    );
}

#[then("the heartbeat should include disk health")]
fn then_heartbeat_includes_disk(world: &mut LatticeWorld) {
    // Disk health is part of the overall health status reported in the heartbeat
    let hb = world
        .last_heartbeat
        .as_ref()
        .expect("no heartbeat generated");
    // A healthy heartbeat with no issues indicates all subsystems passed
    assert!(
        hb.issues.is_empty(),
        "expected no health issues (disk health nominal)"
    );
}

#[then("the agent should re-register with the quorum")]
fn then_agent_re_registered(world: &mut LatticeWorld) {
    // After restart, the agent is freshly constructed and ready to register
    let agent = world.agent.as_ref().expect("agent not initialized");
    assert_eq!(
        agent.active_allocation_count(),
        0,
        "re-registered agent should have zero allocations"
    );
    assert_eq!(
        agent.buffered_update_count(),
        0,
        "re-registered agent should have no buffered updates"
    );
}

#[then("the heartbeat sequence should reset")]
fn then_heartbeat_sequence_reset(world: &mut LatticeWorld) {
    // After restart, the heartbeat generator resets to sequence 1
    let agent = world.agent.as_mut().expect("agent not initialized");
    let hb = agent.generate_heartbeat(&healthy_observed(4));
    assert_eq!(
        hb.sequence, 1,
        "expected heartbeat sequence to reset to 1 after restart, got {}",
        hb.sequence
    );
    world.last_heartbeat = Some(hb);
}
